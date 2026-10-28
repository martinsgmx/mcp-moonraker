//! Structured, redacted audit logging.
//!
//! Audit records are JSON lines. Configure `logging.audit_log` (or
//! `MCP_MOONRAKER_AUDIT_LOG`) to append to a file; otherwise records go to the
//! `mcp_moonraker::audit` tracing target (stderr by default). Secrets are
//! redacted before serialization, and long payloads are truncated.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use fs2::FileExt;
use serde::Serialize;

use crate::{
    error::{Error, Result},
    safety::{
        policy::Risk,
        redact::{redact_value, truncate_for_audit},
    },
};

/// Audit event outcome.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Prepared,
    Allowed,
    Denied,
    ApprovalRequired,
    Executed,
    Failed,
    Confirmed,
}

/// One audit record.
#[derive(Debug, Serialize)]
pub struct AuditEvent<'a> {
    pub timestamp: u64,
    pub event: &'static str,
    pub tool: &'a str,
    pub risk: Risk,
    pub capability: &'a str,
    pub approval_id: Option<&'a str>,
    pub args: String,
    pub outcome: Outcome,
    pub duration_ms: u128,
    pub error: Option<String>,
}

/// Audit sink.
pub struct AuditLogger {
    path: Option<PathBuf>,
    lock: Mutex<()>,
}

impl std::fmt::Debug for AuditLogger {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuditLogger")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl AuditLogger {
    /// Create a logger. Ensures the parent directory exists.
    pub fn new(path: Option<&Path>) -> Result<Self> {
        if let Some(path) = path {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
        }
        Ok(Self {
            path: path.map(Path::to_path_buf),
            lock: Mutex::new(()),
        })
    }

    /// Write an event.
    ///
    /// Kept as a flat set of arguments so every call site records the same
    /// fields in the same order; grouping them into a struct would add
    /// allocation noise without improving the audit schema.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        tool: &str,
        risk: Risk,
        capability: &str,
        approval_id: Option<&str>,
        args: &serde_json::Value,
        outcome: Outcome,
        duration: Duration,
        error: Option<&str>,
    ) {
        let redacted = redact_value(args);
        let args_text = truncate_for_audit(&redacted.to_string(), 4000);
        let event = AuditEvent {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            event: "tool_call",
            tool,
            risk,
            capability,
            approval_id,
            args: args_text,
            outcome,
            duration_ms: duration.as_millis(),
            error: error.map(|message| truncate_for_audit(message, 2000)),
        };

        if let Some(path) = &self.path {
            if let Err(err) = self.append(path, &event) {
                // Never lose the audit trail silently, but never leak secrets
                // through a panic either.
                tracing::error!(target: "mcp_moonraker::audit", error = %err, "failed to write audit log");
            }
        } else {
            tracing::info!(
                target: "mcp_moonraker::audit",
                tool = %event.tool,
                risk = %event.risk.as_str(),
                capability = %event.capability,
                approval_id = ?event.approval_id,
                outcome = ?event.outcome,
                duration_ms = event.duration_ms,
                args = %event.args,
                error = ?event.error,
                "tool call"
            );
        }
    }

    fn append(&self, path: &Path, event: &AuditEvent<'_>) -> Result<()> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| Error::Config("audit logger lock poisoned".into()))?;
        let line = serde_json::to_string(event)?;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = file.metadata()?.permissions();
            permissions.set_mode(0o600);
            file.set_permissions(permissions)?;
        }
        FileExt::lock_exclusive(&file)?;
        writeln!(file, "{line}")?;
        file.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn audit_redacts_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let logger = AuditLogger::new(Some(&path)).unwrap();
        logger.record(
            "test_tool",
            Risk::High,
            "mutation",
            None,
            &json!({"api_key": "hunter2", "filename": "part.gcode"}),
            Outcome::Executed,
            Duration::from_millis(12),
            None,
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("hunter2"));
        assert!(text.contains("[REDACTED]"));
        assert!(text.contains("part.gcode"));
    }
}
