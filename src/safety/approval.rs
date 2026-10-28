//! File-backed, single-use approval store for destructive actions.
//!
//! Flow:
//! 1. A destructive tool calls [`ApprovalStore::create`] and returns an
//!    approval prompt instead of executing.
//! 2. The agent calls `moonraker_confirm_action` with the approval id and,
//!    depending on the mode, the confirmation phrase.
//! 3. [`ApprovalStore::consume`] atomically verifies and marks the record
//!    consumed. The server then executes the *stored* tool + arguments, not
//!    anything supplied in the confirm call. This prevents argument swapping
//!    between approval and execution.
//!
//! Records live in `<state_dir>/approvals/<id>.json` with `0600` permissions
//! (Unix). A per-record lock file makes consume/approve race-free across
//! processes, which is what makes the `mcp-moonraker approve` CLI work while
//! the server is running.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    config::ApprovalMode,
    error::{Error, Result},
    safety::policy::Risk,
};

/// Lifecycle state of an approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    /// Waiting for the agent/human.
    Pending,
    /// A human approved it via the CLI (out-of-band modes).
    Approved,
    /// The action was executed (or execution started).
    Consumed,
    /// Cancelled by the agent or a human.
    Denied,
    /// The TTL elapsed before confirmation.
    Expired,
}

/// A persisted prepared action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub id: String,
    pub tool: String,
    pub args: Value,
    pub fingerprint: String,
    pub risk: Risk,
    pub summary: String,
    pub phrase: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub human_approved_at: Option<u64>,
    pub consumed_at: Option<u64>,
    pub status: ApprovalStatus,
    pub result: Option<String>,
}

impl ApprovalRecord {
    /// Whether the record can still be confirmed.
    #[must_use]
    pub fn is_open(&self) -> bool {
        matches!(
            self.status,
            ApprovalStatus::Pending | ApprovalStatus::Approved
        )
    }

    /// Seconds remaining before expiry (saturating at zero).
    #[must_use]
    pub fn remaining_secs(&self) -> u64 {
        self.expires_at.saturating_sub(unix_now())
    }
}

/// Information returned to the agent when approval is required.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalPrompt {
    pub status: &'static str,
    pub approval_id: String,
    pub tool: String,
    pub summary: String,
    pub risk: &'static str,
    pub expires_at: u64,
    pub expires_in_secs: u64,
    pub confirmation_phrase: Option<String>,
    pub human_approval_required: bool,
    pub instructions: String,
}

/// File-backed approval store.
#[derive(Debug, Clone)]
pub struct ApprovalStore {
    root: PathBuf,
    ttl: Duration,
    mode: ApprovalMode,
}

impl ApprovalStore {
    /// Create a store, ensuring the state directory exists with restrictive
    /// permissions.
    pub fn new(state_dir: impl AsRef<Path>, ttl: Duration, mode: ApprovalMode) -> Result<Self> {
        let root = state_dir.as_ref().join("approvals");
        create_private_dir(state_dir.as_ref())?;
        create_private_dir(&root)?;
        Ok(Self { root, ttl, mode })
    }

    /// Approval mode.
    #[must_use]
    pub fn mode(&self) -> ApprovalMode {
        self.mode
    }

    /// Directory holding approval records.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the confirmation phrase is returned to the agent.
    #[must_use]
    pub fn phrase_in_response(&self) -> bool {
        matches!(self.mode, ApprovalMode::InBand | ApprovalMode::Both)
    }

    /// Whether a human approval is required.
    #[must_use]
    pub fn requires_human(&self) -> bool {
        matches!(self.mode, ApprovalMode::OutOfBand | ApprovalMode::Both)
    }

    /// Whether the agent must echo the phrase.
    #[must_use]
    pub fn requires_phrase(&self) -> bool {
        matches!(self.mode, ApprovalMode::InBand | ApprovalMode::Both)
    }

    /// Create a new pending approval for a tool call.
    pub fn create(
        &self,
        tool: &str,
        args: Value,
        risk: Risk,
        summary: String,
    ) -> Result<ApprovalRecord> {
        let now = unix_now();
        let fingerprint = fingerprint(tool, &args);
        let id = format!("apr_{}", Uuid::new_v4().simple());
        let phrase = phrase_for(&fingerprint);
        let record = ApprovalRecord {
            id: id.clone(),
            tool: tool.to_string(),
            args,
            fingerprint,
            risk,
            summary,
            phrase,
            created_at: now,
            expires_at: now + self.ttl.as_secs(),
            human_approved_at: None,
            consumed_at: None,
            status: ApprovalStatus::Pending,
            result: None,
        };
        self.write_record(&record)?;
        Ok(record)
    }

    /// Load a record by id.
    pub fn load(&self, id: &str) -> Result<ApprovalRecord> {
        let path = self.record_path(id)?;
        let text = fs::read_to_string(&path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                Error::Approval(format!("approval '{id}' was not found or has expired"))
            } else {
                Error::Io(err)
            }
        })?;
        serde_json::from_str(&text).map_err(Error::Json)
    }

    /// List all records, newest first.
    pub fn list(&self) -> Result<Vec<ApprovalRecord>> {
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&path) {
                if let Ok(record) = serde_json::from_str::<ApprovalRecord>(&text) {
                    records.push(record);
                }
            }
        }
        records.sort_by_key(|record| std::cmp::Reverse(record.created_at));
        Ok(records)
    }

    /// List only open (pending/approved, unexpired) records.
    pub fn list_open(&self) -> Result<Vec<ApprovalRecord>> {
        let now = unix_now();
        Ok(self
            .list()?
            .into_iter()
            .filter(|record| record.is_open() && record.expires_at > now)
            .collect())
    }

    /// Mark a record as human-approved. Used by the `approve` CLI.
    pub fn human_approve(&self, id: &str) -> Result<ApprovalRecord> {
        let lock = self.lock(id)?;
        let mut record = self.load(id)?;
        if !record.is_open() {
            return Err(Error::Approval(format!(
                "approval '{id}' is not open (status: {:?})",
                record.status
            )));
        }
        if record.expires_at <= unix_now() {
            record.status = ApprovalStatus::Expired;
            self.write_record(&record)?;
            return Err(Error::Approval(format!("approval '{id}' has expired")));
        }
        record.human_approved_at = Some(unix_now());
        record.status = ApprovalStatus::Approved;
        self.write_record(&record)?;
        drop(lock);
        Ok(record)
    }

    /// Deny/cancel an approval. Works for both agents and humans.
    pub fn deny(&self, id: &str) -> Result<ApprovalRecord> {
        let lock = self.lock(id)?;
        let mut record = self.load(id)?;
        if !record.is_open() {
            return Err(Error::Approval(format!(
                "approval '{id}' is not open (status: {:?})",
                record.status
            )));
        }
        record.status = ApprovalStatus::Denied;
        self.write_record(&record)?;
        drop(lock);
        Ok(record)
    }

    /// Atomically verify and consume an approval.
    ///
    /// The caller must then execute `record.tool` with `record.args`.
    pub fn consume(
        &self,
        id: &str,
        phrase: Option<&str>,
        acknowledge: bool,
    ) -> Result<ApprovalRecord> {
        if !acknowledge {
            return Err(Error::Approval(
                "explicit acknowledgement is required: set acknowledge=true after reading the \
                 action summary"
                    .to_string(),
            ));
        }

        let lock = self.lock(id)?;
        let mut record = self.load(id)?;

        if !record.is_open() {
            return Err(Error::Approval(match record.status {
                ApprovalStatus::Consumed => {
                    format!("approval '{id}' has already been used; prepare the action again")
                }
                ApprovalStatus::Denied => format!("approval '{id}' was denied"),
                ApprovalStatus::Expired => format!("approval '{id}' has expired"),
                ApprovalStatus::Pending | ApprovalStatus::Approved => {
                    format!("approval '{id}' is not open")
                }
            }));
        }

        if record.expires_at <= unix_now() {
            record.status = ApprovalStatus::Expired;
            self.write_record(&record)?;
            return Err(Error::Approval(format!(
                "approval '{id}' has expired; prepare the action again"
            )));
        }

        if self.requires_human() && record.human_approved_at.is_none() {
            return Err(Error::Approval(format!(
                "approval '{id}' still requires a human. Run `mcp-moonraker approve {}` on the \
                 machine running the MCP server, then confirm.",
                record.id
            )));
        }

        if self.requires_phrase() {
            let supplied = phrase.unwrap_or_default();
            if !constant_time_eq(supplied.trim(), &record.phrase) {
                return Err(Error::Approval(
                    "confirmation phrase does not match the prepared action".to_string(),
                ));
            }
        }

        record.consumed_at = Some(unix_now());
        record.status = ApprovalStatus::Consumed;
        self.write_record(&record)?;
        drop(lock);
        Ok(record)
    }

    /// Record the outcome of an executed approval.
    pub fn finish(&self, id: &str, result: impl Into<String>) -> Result<()> {
        let lock = self.lock(id)?;
        let mut record = self.load(id)?;
        record.result = Some(result.into());
        self.write_record(&record)?;
        drop(lock);
        Ok(())
    }

    /// Build the prompt shown to the agent/human.
    #[must_use]
    pub fn prompt(&self, record: &ApprovalRecord) -> ApprovalPrompt {
        let human = self.requires_human();
        let instructions = if human && self.requires_phrase() {
            format!(
                "This action is destructive. A human must run `mcp-moonraker approve {}` on the \
                 MCP host, then call moonraker_confirm_action with the approval id, the \
                 confirmation phrase, and acknowledge=true.",
                record.id
            )
        } else if human {
            format!(
                "This action is destructive. A human must run `mcp-moonraker approve {}` on the \
                 MCP host, then call moonraker_confirm_action with the approval id and \
                 acknowledge=true.",
                record.id
            )
        } else {
            format!(
                "This action is destructive. Call moonraker_confirm_action with the approval id, \
                 confirmation_phrase=\"{}\", and acknowledge=true to execute it. The approval \
                 expires in {} seconds.",
                record.phrase,
                record.remaining_secs()
            )
        };

        ApprovalPrompt {
            status: "approval_required",
            approval_id: record.id.clone(),
            tool: record.tool.clone(),
            summary: record.summary.clone(),
            risk: record.risk.as_str(),
            expires_at: record.expires_at,
            expires_in_secs: record.remaining_secs(),
            confirmation_phrase: self.phrase_in_response().then(|| record.phrase.clone()),
            human_approval_required: human,
            instructions,
        }
    }

    /// Print an out-of-band prompt to stderr. Called by the server when a
    /// destructive action is prepared in `out_of_band` or `both` mode.
    pub fn announce(&self, record: &ApprovalRecord) {
        if !self.requires_human() {
            return;
        }
        eprintln!();
        eprintln!("┌──────────────── DESTRUCTIVE ACTION PENDING ────────────────");
        eprintln!("│ ID:      {}", record.id);
        eprintln!("│ Tool:    {}", record.tool);
        eprintln!("│ Summary: {}", record.summary);
        eprintln!("│ Risk:    {}", record.risk.as_str());
        eprintln!("│ Expires: {} seconds", record.remaining_secs());
        eprintln!("│ Phrase:  {}", record.phrase);
        eprintln!("│");
        eprintln!("│ Approve: mcp-moonraker approve {}", record.id);
        eprintln!("│ Deny:    mcp-moonraker deny {}", record.id);
        eprintln!("└────────────────────────────────────────────────────────────");
        eprintln!();
    }

    /// Mark expired records. Returns the number updated.
    pub fn expire_stale(&self) -> Result<usize> {
        let now = unix_now();
        let mut count = 0;
        for mut record in self.list()? {
            if record.is_open() && record.expires_at <= now {
                record.status = ApprovalStatus::Expired;
                self.write_record(&record)?;
                count += 1;
            }
        }
        Ok(count)
    }

    /// Remove consumed/expired records older than `max_age`.
    pub fn prune_older_than(&self, max_age: Duration) -> Result<usize> {
        let cutoff = unix_now().saturating_sub(max_age.as_secs());
        let mut count = 0;
        for record in self.list()? {
            if !record.is_open() && record.created_at < cutoff {
                let path = self.record_path(&record.id)?;
                if fs::remove_file(path).is_ok() {
                    let lock = self.root.join(format!("{}.lock", record.id));
                    let _ = fs::remove_file(lock);
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    fn record_path(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self.root.join(format!("{id}.json")))
    }

    fn lock(&self, id: &str) -> Result<File> {
        validate_id(id)?;
        let path = self.root.join(format!("{id}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        restrict_permissions(&file)?;
        file.lock_exclusive()?;
        Ok(file)
    }

    fn write_record(&self, record: &ApprovalRecord) -> Result<()> {
        let path = self.record_path(&record.id)?;
        let temp = self
            .root
            .join(format!(".{}.{}.tmp", record.id, Uuid::new_v4().simple()));
        let text = serde_json::to_vec_pretty(record)?;
        {
            let mut file = create_private_file(&temp)?;
            file.write_all(&text)?;
            file.sync_all()?;
        }
        fs::rename(&temp, &path)?;
        Ok(())
    }
}

/// Canonical fingerprint of a tool call.
#[must_use]
pub fn fingerprint(tool: &str, args: &Value) -> String {
    let canonical = canonical_json(args);
    let mut hasher = Sha256::new();
    hasher.update(tool.as_bytes());
    hasher.update(b"\0");
    hasher.update(canonical.as_bytes());
    hex::encode(hasher.finalize())
}

fn phrase_for(fingerprint: &str) -> String {
    let short = fingerprint
        .get(..8)
        .unwrap_or(fingerprint)
        .to_ascii_uppercase();
    format!("CONFIRM-{short}")
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let body = entries
                .into_iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into()),
                        canonical_json(value)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".into()),
    }
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(Error::Approval("invalid approval id".to_string()));
    }
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn create_private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn restrict_permissions(file: &File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = file.metadata()?.permissions();
        permissions.set_mode(0o600);
        file.set_permissions(permissions)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> (tempfile::TempDir, ApprovalStore) {
        let dir = tempfile::tempdir().unwrap();
        let store =
            ApprovalStore::new(dir.path(), Duration::from_secs(60), ApprovalMode::InBand).unwrap();
        (dir, store)
    }

    #[test]
    fn create_and_consume_single_use() {
        let (_dir, store) = store();
        let record = store
            .create(
                "moonraker_print_cancel",
                json!({"reason": "test"}),
                Risk::High,
                "Cancel the print".into(),
            )
            .unwrap();
        assert_eq!(record.status, ApprovalStatus::Pending);
        assert!(record.phrase.starts_with("CONFIRM-"));

        let consumed = store
            .consume(&record.id, Some(&record.phrase), true)
            .unwrap();
        assert_eq!(consumed.status, ApprovalStatus::Consumed);

        let replay = store.consume(&record.id, Some(&record.phrase), true);
        assert!(replay.is_err());
    }

    #[test]
    fn wrong_phrase_is_rejected() {
        let (_dir, store) = store();
        let record = store
            .create("x", json!({}), Risk::High, "x".into())
            .unwrap();
        assert!(store
            .consume(&record.id, Some("CONFIRM-DEADBEEF"), true)
            .is_err());
        assert!(store.load(&record.id).unwrap().is_open());
    }

    #[test]
    fn acknowledge_is_required() {
        let (_dir, store) = store();
        let record = store
            .create("x", json!({}), Risk::High, "x".into())
            .unwrap();
        assert!(store
            .consume(&record.id, Some(&record.phrase), false)
            .is_err());
    }

    #[test]
    fn out_of_band_requires_human() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            ApprovalStore::new(dir.path(), Duration::from_secs(60), ApprovalMode::OutOfBand)
                .unwrap();
        let record = store
            .create("x", json!({}), Risk::High, "x".into())
            .unwrap();
        assert!(store.consume(&record.id, None, true).is_err());
        store.human_approve(&record.id).unwrap();
        assert!(store.consume(&record.id, None, true).is_ok());
    }

    #[test]
    fn expiry_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            ApprovalStore::new(dir.path(), Duration::from_secs(60), ApprovalMode::InBand).unwrap();
        let mut record = store
            .create("x", json!({}), Risk::High, "x".into())
            .unwrap();
        record.expires_at = unix_now() - 1;
        store.write_record(&record).unwrap();
        assert!(store
            .consume(&record.id, Some(&record.phrase), true)
            .is_err());
    }

    #[test]
    fn fingerprint_is_order_independent() {
        let a = fingerprint("tool", &json!({"b": 2, "a": [1, {"z": 3, "y": 4}]}));
        let b = fingerprint("tool", &json!({"a": [1, {"y": 4, "z": 3}], "b": 2}));
        assert_eq!(a, b);
        assert_ne!(a, fingerprint("tool", &json!({"a": 3})));
    }

    #[test]
    fn rejects_path_traversal() {
        let (_dir, store) = store();
        assert!(store.load("../etc/passwd").is_err());
        assert!(store.load("apr_../../x").is_err());
    }
}
