//! Configuration loading and validation.
//!
//! Priority (lowest to highest):
//! 1. Built-in safe defaults
//! 2. Optional TOML file (`--config` / `MCP_MOONRAKER_CONFIG`)
//! 3. Environment variables (`MOONRAKER_*`, `MCP_MOONRAKER_*`)
//! 4. Command line flags (handled by `main.rs`)
//!
//! Secrets are held in [`SecretString`] and are never serialized by
//! [`Config::public_view`]. Prefer environment variables or a secret manager
//! over putting keys in the TOML file.

use std::{
    env,
    net::IpAddr,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{Error, Result};

/// MCP transport to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// Newline-delimited JSON-RPC over stdin/stdout. Best for local agents.
    #[default]
    Stdio,
    /// Streamable HTTP (with optional SSE compatibility).
    Http,
}

impl FromStr for TransportKind {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stdio" | "stdin" | "stdout" => Ok(Self::Stdio),
            "http" | "streamable-http" | "streamable_http" => Ok(Self::Http),
            other => Err(format!(
                "unknown transport '{other}' (expected 'stdio' or 'http')"
            )),
        }
    }
}

/// How destructive actions are approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// The prepare call returns a single-use phrase; the agent supplies it to
    /// the confirm tool. Convenient, but an autonomous agent can self-approve.
    #[default]
    InBand,
    /// The phrase is printed to the server console and written to the approval
    /// file. A human must run `mcp-moonraker approve <id>` before the agent can
    /// confirm. Recommended for remote/public deployments.
    OutOfBand,
    /// Both an in-band phrase and a human approval are required.
    Both,
}

impl FromStr for ApprovalMode {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "in_band" | "in-band" | "inband" => Ok(Self::InBand),
            "out_of_band" | "out-of-band" | "outofband" => Ok(Self::OutOfBand),
            "both" => Ok(Self::Both),
            other => Err(format!(
                "unknown approval mode '{other}' (expected 'in_band', 'out_of_band' or 'both')"
            )),
        }
    }
}

/// Moonraker connection settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MoonrakerConfig {
    /// Base URL, e.g. `http://127.0.0.1:7125`.
    pub url: String,
    /// Optional Moonraker API key (sent as `X-Api-Key`).
    #[serde(skip_serializing)]
    pub api_key: Option<SecretString>,
    /// Request timeout for normal calls.
    pub timeout_secs: u64,
    /// Timeout for `printer.gcode.script` calls. Klipper only replies once the
    /// script has finished executing, so motion-heavy scripts (e.g. `G28`
    /// homing) and blocking macros (e.g. `TEMPERATURE_WAIT`) need a larger
    /// budget than normal RPCs.
    pub script_timeout_secs: u64,
    /// Verify TLS certificates (disable only for a trusted self-signed host).
    pub tls_verify: bool,
    /// Maximum JSON body size accepted from Moonraker.
    pub max_response_bytes: usize,
}

impl Default for MoonrakerConfig {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:7125".to_string(),
            api_key: None,
            timeout_secs: 15,
            script_timeout_secs: 120,
            tls_verify: true,
            max_response_bytes: 8 * 1024 * 1024,
        }
    }
}

/// HTTP transport settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    /// Bind host. Defaults to loopback.
    pub host: String,
    /// Bind port.
    pub port: u16,
    /// Static bearer token. Required when binding to a non-loopback address.
    #[serde(skip_serializing)]
    pub token: Option<SecretString>,
    /// Extra `Host` header values accepted by DNS-rebinding protection.
    pub allowed_hosts: Vec<String>,
    /// Extra browser origins allowed by CORS.
    pub allowed_origins: Vec<String>,
    /// Explicitly allow unauthenticated non-loopback HTTP. Never use this on a
    /// network you do not fully control.
    pub allow_unauthenticated: bool,
    /// Streamable HTTP path.
    pub path: String,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8765,
            token: None,
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
            allow_unauthenticated: false,
            path: "/mcp".to_string(),
        }
    }
}

/// Safety policy settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyConfig {
    /// Disable every mutation, including safe ones. Emergency stop remains
    /// available unless `disable_emergency_stop` is set.
    pub read_only: bool,
    /// Master switch for non-destructive mutations (heaters, fans, motion,
    /// speed/flow, pause/resume, job queue additions).
    pub allow_mutations: bool,
    /// Allow the raw G-code tool. Strongly discouraged.
    pub allow_raw_gcode: bool,
    /// Allow host administration: service restarts, reboot, shutdown, updates.
    pub allow_host_admin: bool,
    /// Allow configuration-file writes. Not implemented in v0.1; reserved.
    pub allow_config_write: bool,
    /// Hard limit for hotend targets.
    pub max_hotend_temp: f64,
    /// Hard limit for bed targets.
    pub max_bed_temp: f64,
    /// Emergency stop bypasses `read_only`; set false only in exceptional cases.
    pub disable_emergency_stop: bool,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            read_only: false,
            allow_mutations: true,
            allow_raw_gcode: false,
            allow_host_admin: false,
            allow_config_write: false,
            max_hotend_temp: 300.0,
            max_bed_temp: 120.0,
            disable_emergency_stop: false,
        }
    }
}

/// Double-confirmation settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalConfig {
    /// Approval model (see [`ApprovalMode`]).
    pub mode: ApprovalMode,
    /// How long a prepared action remains valid.
    pub ttl_secs: u64,
    /// Directory used to persist pending approvals.
    pub state_dir: String,
}

impl Default for ApprovalConfig {
    fn default() -> Self {
        Self {
            mode: ApprovalMode::InBand,
            ttl_secs: 120,
            state_dir: default_state_dir_string(),
        }
    }
}

/// Camera snapshot settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CameraConfig {
    /// Hard cap for a single snapshot.
    pub max_bytes: usize,
    /// Snapshot HTTP timeout.
    pub timeout_secs: u64,
    /// Allow camera hosts on private/LAN addresses. Almost every Moonraker
    /// webcam lives on the LAN, so this defaults to true.
    pub allow_private_hosts: bool,
    /// Optional allow-list of camera hostnames. Empty means "any host returned
    /// by Moonraker".
    pub allowed_hosts: Vec<String>,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            timeout_secs: 10,
            allow_private_hosts: true,
            allowed_hosts: Vec::new(),
        }
    }
}

/// Logging settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    /// `tracing` filter, e.g. `info` or `mcp_moonraker=debug`.
    pub level: String,
    /// Optional JSON-lines audit log. When unset, audit events go to stderr.
    pub audit_log: Option<String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            audit_log: None,
        }
    }
}

/// Fully resolved runtime configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub moonraker: MoonrakerConfig,
    pub server: ServerConfig,
    pub policy: PolicyConfig,
    pub approval: ApprovalConfig,
    pub camera: CameraConfig,
    pub logging: LoggingConfig,
}

/// Server/transport settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub transport: TransportKind,
    pub http: HttpConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            transport: TransportKind::Stdio,
            http: HttpConfig::default(),
        }
    }
}

impl Config {
    /// Load defaults, then an optional TOML file, then environment overrides.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let mut config = Self::default();

        let config_path = path
            .map(Path::to_path_buf)
            .or_else(|| env::var("MCP_MOONRAKER_CONFIG").ok().map(PathBuf::from));

        if let Some(config_path) = config_path {
            if config_path.exists() {
                let text = std::fs::read_to_string(&config_path).map_err(|err| {
                    Error::Config(format!(
                        "cannot read config file {}: {err}",
                        config_path.display()
                    ))
                })?;
                config = toml::from_str(&text)?;
            } else if path.is_some() {
                return Err(Error::Config(format!(
                    "config file not found: {}",
                    config_path.display()
                )));
            }
        }

        config.apply_env()?;
        config.normalize()?;
        config.validate()?;
        Ok(config)
    }

    fn apply_env(&mut self) -> Result<()> {
        if let Some(value) = env_str("MOONRAKER_URL") {
            self.moonraker.url = value;
        }
        if let Some(value) = env_str("MOONRAKER_API_KEY") {
            self.moonraker.api_key = Some(SecretString::from(value));
        }
        if let Some(value) = env_u64("MOONRAKER_TIMEOUT_SECS")? {
            self.moonraker.timeout_secs = value;
        }
        if let Some(value) = env_u64("MOONRAKER_SCRIPT_TIMEOUT_SECS")? {
            self.moonraker.script_timeout_secs = value;
        }
        if let Some(value) = env_bool("MOONRAKER_TLS_VERIFY")? {
            self.moonraker.tls_verify = value;
        }
        if let Some(value) = env_usize("MOONRAKER_MAX_RESPONSE_BYTES")? {
            self.moonraker.max_response_bytes = value;
        }

        if let Some(value) = env_str("MCP_MOONRAKER_TRANSPORT") {
            self.server.transport = value.parse().map_err(Error::Config)?;
        }
        if let Some(value) = env_str("MCP_MOONRAKER_HTTP_HOST") {
            self.server.http.host = value;
        }
        if let Some(value) = env_u64("MCP_MOONRAKER_HTTP_PORT")? {
            self.server.http.port = u16::try_from(value)
                .map_err(|_| Error::Config("HTTP port must be between 1 and 65535".into()))?;
        }
        if let Some(value) = env_str("MCP_MOONRAKER_HTTP_TOKEN") {
            self.server.http.token = Some(SecretString::from(value));
        }
        if let Some(value) = env_list("MCP_MOONRAKER_HTTP_ALLOWED_HOSTS") {
            self.server.http.allowed_hosts = value;
        }
        if let Some(value) = env_list("MCP_MOONRAKER_HTTP_ALLOWED_ORIGINS") {
            self.server.http.allowed_origins = value;
        }
        if let Some(value) = env_bool("MCP_MOONRAKER_HTTP_ALLOW_UNAUTHENTICATED")? {
            self.server.http.allow_unauthenticated = value;
        }
        if let Some(value) = env_str("MCP_MOONRAKER_HTTP_PATH") {
            self.server.http.path = value;
        }

        if let Some(value) = env_bool("MCP_MOONRAKER_READ_ONLY")? {
            self.policy.read_only = value;
        }
        if let Some(value) = env_bool("MCP_MOONRAKER_ALLOW_MUTATIONS")? {
            self.policy.allow_mutations = value;
        }
        if let Some(value) = env_bool("MCP_MOONRAKER_ALLOW_RAW_GCODE")? {
            self.policy.allow_raw_gcode = value;
        }
        if let Some(value) = env_bool("MCP_MOONRAKER_ALLOW_HOST_ADMIN")? {
            self.policy.allow_host_admin = value;
        }
        if let Some(value) = env_bool("MCP_MOONRAKER_ALLOW_CONFIG_WRITE")? {
            self.policy.allow_config_write = value;
        }
        if let Some(value) = env_f64("MCP_MOONRAKER_MAX_HOTEND_TEMP")? {
            self.policy.max_hotend_temp = value;
        }
        if let Some(value) = env_f64("MCP_MOONRAKER_MAX_BED_TEMP")? {
            self.policy.max_bed_temp = value;
        }
        if let Some(value) = env_bool("MCP_MOONRAKER_DISABLE_EMERGENCY_STOP")? {
            self.policy.disable_emergency_stop = value;
        }

        if let Some(value) = env_str("MCP_MOONRAKER_APPROVAL_MODE") {
            self.approval.mode = value.parse().map_err(Error::Config)?;
        }
        if let Some(value) = env_u64("MCP_MOONRAKER_APPROVAL_TTL_SECS")? {
            self.approval.ttl_secs = value;
        }
        if let Some(value) = env_str("MCP_MOONRAKER_STATE_DIR") {
            self.approval.state_dir = value;
        }

        if let Some(value) = env_usize("MCP_MOONRAKER_CAMERA_MAX_BYTES")? {
            self.camera.max_bytes = value;
        }
        if let Some(value) = env_u64("MCP_MOONRAKER_CAMERA_TIMEOUT_SECS")? {
            self.camera.timeout_secs = value;
        }
        if let Some(value) = env_bool("MCP_MOONRAKER_CAMERA_ALLOW_PRIVATE_HOSTS")? {
            self.camera.allow_private_hosts = value;
        }
        if let Some(value) = env_list("MCP_MOONRAKER_CAMERA_ALLOWED_HOSTS") {
            self.camera.allowed_hosts = value;
        }

        if let Some(value) = env_str("MCP_MOONRAKER_LOG_LEVEL") {
            self.logging.level = value;
        }
        if let Some(value) = env_str("MCP_MOONRAKER_AUDIT_LOG") {
            self.logging.audit_log = Some(value);
        }

        Ok(())
    }

    fn normalize(&mut self) -> Result<()> {
        self.moonraker.url = self.moonraker.url.trim().trim_end_matches('/').to_string();
        if self.moonraker.url.is_empty() {
            return Err(Error::Config("Moonraker URL must not be empty".into()));
        }
        let url = Url::parse(&self.moonraker.url)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(Error::Config(
                "Moonraker URL must use http:// or https://".into(),
            ));
        }
        if url.host_str().is_none() {
            return Err(Error::Config("Moonraker URL must include a host".into()));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Error::Config(
                "Moonraker URL must not embed credentials; use MOONRAKER_API_KEY".into(),
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(Error::Config(
                "Moonraker URL must not contain a query string or fragment".into(),
            ));
        }

        self.server.http.path = normalize_path(&self.server.http.path)?;
        self.approval.state_dir = expand_tilde(&self.approval.state_dir);
        if let Some(path) = self.logging.audit_log.as_mut() {
            *path = expand_tilde(path);
        }
        self.server.http.allowed_hosts = normalize_list(&mut self.server.http.allowed_hosts);
        self.server.http.allowed_origins = normalize_list(&mut self.server.http.allowed_origins);
        self.camera.allowed_hosts = normalize_list(&mut self.camera.allowed_hosts);
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.moonraker.timeout_secs == 0 || self.moonraker.timeout_secs > 300 {
            return Err(Error::Config(
                "Moonraker timeout must be between 1 and 300 seconds".into(),
            ));
        }
        if self.moonraker.script_timeout_secs == 0 || self.moonraker.script_timeout_secs > 600 {
            return Err(Error::Config(
                "Moonraker script timeout must be between 1 and 600 seconds".into(),
            ));
        }
        if !(1024..=64 * 1024 * 1024).contains(&self.moonraker.max_response_bytes) {
            return Err(Error::Config(
                "max_response_bytes must be between 1 KiB and 64 MiB".into(),
            ));
        }
        if self.approval.ttl_secs < 5 || self.approval.ttl_secs > 3600 {
            return Err(Error::Config(
                "approval TTL must be between 5 and 3600 seconds".into(),
            ));
        }
        if self.server.http.port == 0 {
            return Err(Error::Config("HTTP port must not be 0".into()));
        }
        if !(1024..=64 * 1024 * 1024).contains(&self.camera.max_bytes) {
            return Err(Error::Config(
                "camera max_bytes must be between 1 KiB and 64 MiB".into(),
            ));
        }
        if self.camera.timeout_secs == 0 || self.camera.timeout_secs > 120 {
            return Err(Error::Config(
                "camera timeout must be between 1 and 120 seconds".into(),
            ));
        }
        if !(0.0..=500.0).contains(&self.policy.max_hotend_temp) {
            return Err(Error::Config(
                "max_hotend_temp must be between 0 and 500 °C".into(),
            ));
        }
        if !(0.0..=200.0).contains(&self.policy.max_bed_temp) {
            return Err(Error::Config(
                "max_bed_temp must be between 0 and 200 °C".into(),
            ));
        }

        if self.server.transport == TransportKind::Http {
            let host = self.server.http.host.trim();
            let is_loopback = host.eq_ignore_ascii_case("localhost")
                || host.parse::<IpAddr>().is_ok_and(|addr| addr.is_loopback());
            if !is_loopback
                && self.server.http.token.is_none()
                && !self.server.http.allow_unauthenticated
            {
                return Err(Error::Config(
                    "refusing to bind HTTP to a non-loopback address without \
                     MCP_MOONRAKER_HTTP_TOKEN; set a bearer token or explicitly set \
                     MCP_MOONRAKER_HTTP_ALLOW_UNAUTHENTICATED=true"
                        .into(),
                ));
            }
        }

        if self.policy.read_only && self.policy.allow_raw_gcode {
            return Err(Error::Config(
                "read_only=true conflicts with allow_raw_gcode=true".into(),
            ));
        }

        Ok(())
    }

    /// Timeout as a `Duration`.
    #[must_use]
    pub fn moonraker_timeout(&self) -> Duration {
        Duration::from_secs(self.moonraker.timeout_secs)
    }

    /// G-code script timeout as a `Duration`.
    #[must_use]
    pub fn moonraker_script_timeout(&self) -> Duration {
        Duration::from_secs(self.moonraker.script_timeout_secs)
    }

    /// Re-run normalization and validation after programmatic overrides
    /// (for example command-line flags applied by `main.rs`).
    pub fn revalidate(&mut self) -> Result<()> {
        self.normalize()?;
        self.validate()
    }

    /// Camera timeout as a `Duration`.
    #[must_use]
    pub fn camera_timeout(&self) -> Duration {
        Duration::from_secs(self.camera.timeout_secs)
    }

    /// Approval TTL as a `Duration`.
    #[must_use]
    pub fn approval_ttl(&self) -> Duration {
        Duration::from_secs(self.approval.ttl_secs)
    }

    /// State directory as a `PathBuf`.
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        PathBuf::from(&self.approval.state_dir)
    }

    /// Audit log path, if configured.
    #[must_use]
    pub fn audit_log_path(&self) -> Option<PathBuf> {
        self.logging.audit_log.as_ref().map(PathBuf::from)
    }

    /// Secret-free view for `doctor`, logs, and docs.
    #[must_use]
    pub fn public_view(&self) -> serde_json::Value {
        serde_json::json!({
            "moonraker": {
                "url": crate::safety::redact::redact_url_userinfo(&self.moonraker.url),
                "api_key_configured": self.moonraker.api_key.is_some(),
                "timeout_secs": self.moonraker.timeout_secs,
                "script_timeout_secs": self.moonraker.script_timeout_secs,
                "tls_verify": self.moonraker.tls_verify,
                "max_response_bytes": self.moonraker.max_response_bytes,
            },
            "server": {
                "transport": self.server.transport,
                "http": {
                    "host": self.server.http.host,
                    "port": self.server.http.port,
                    "path": self.server.http.path,
                    "token_configured": self.server.http.token.is_some(),
                    "allowed_hosts": self.server.http.allowed_hosts,
                    "allowed_origins": self.server.http.allowed_origins,
                    "allow_unauthenticated": self.server.http.allow_unauthenticated,
                }
            },
            "policy": self.policy,
            "approval": {
                "mode": self.approval.mode,
                "ttl_secs": self.approval.ttl_secs,
                "state_dir": self.approval.state_dir,
            },
            "camera": self.camera,
            "logging": self.logging,
        })
    }
}

fn normalize_path(path: &str) -> Result<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(Error::Config("HTTP path must not be empty".into()));
    }
    let with_slash = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    };
    if with_slash.len() > 128 {
        return Err(Error::Config("HTTP path is too long".into()));
    }
    Ok(with_slash)
}

fn normalize_list(values: &mut Vec<String>) -> Vec<String> {
    values
        .drain(..)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    path.to_string()
}

fn default_state_dir_string() -> String {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map_or_else(
            || PathBuf::from(".mcp-moonraker"),
            |dir| dir.join("mcp-moonraker"),
        )
        .to_string_lossy()
        .to_string()
}

fn env_str(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

fn env_list(key: &str) -> Option<Vec<String>> {
    env_str(key).map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToString::to_string)
            .collect()
    })
}

fn env_bool(key: &str) -> Result<Option<bool>> {
    let Some(value) = env_str(key) else {
        return Ok(None);
    };
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "yes" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "off" => Ok(Some(false)),
        _ => Err(Error::Config(format!(
            "{key} must be a boolean (true/false), got '{value}'"
        ))),
    }
}

fn env_u64(key: &str) -> Result<Option<u64>> {
    let Some(value) = env_str(key) else {
        return Ok(None);
    };
    value
        .parse()
        .map(Some)
        .map_err(|_| Error::Config(format!("{key} must be an unsigned integer, got '{value}'")))
}

fn env_usize(key: &str) -> Result<Option<usize>> {
    let Some(value) = env_str(key) else {
        return Ok(None);
    };
    value
        .parse()
        .map(Some)
        .map_err(|_| Error::Config(format!("{key} must be an unsigned integer, got '{value}'")))
}

fn env_f64(key: &str) -> Result<Option<f64>> {
    let Some(value) = env_str(key) else {
        return Ok(None);
    };
    value
        .parse()
        .map(Some)
        .map_err(|_| Error::Config(format!("{key} must be a number, got '{value}'")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_safe() {
        let config = Config::default();
        assert_eq!(config.moonraker.url, "http://127.0.0.1:7125");
        assert!(!config.policy.read_only);
        assert!(config.policy.allow_mutations);
        assert!(!config.policy.allow_raw_gcode);
        assert!(!config.policy.allow_host_admin);
        assert_eq!(config.approval.mode, ApprovalMode::InBand);
        assert!(config.server.http.token.is_none());
        assert!(!config.public_view().to_string().contains("hunter2"));
    }

    #[test]
    fn rejects_non_loopback_http_without_token() {
        let mut config = Config::default();
        config.server.transport = TransportKind::Http;
        config.server.http.host = "0.0.0.0".into();
        assert!(config.validate().is_err());
        config.server.http.token = Some(SecretString::from("secret"));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn rejects_credentials_in_moonraker_url() {
        let mut config = Config::default();
        config.moonraker.url = "http://user:pass@printer.local:7125".into();
        assert!(config.normalize().is_err());
    }

    #[test]
    fn normalizes_paths_and_lists() {
        let mut config = Config::default();
        config.server.http.path = "mcp".into();
        config.server.http.allowed_hosts = vec![" a ".into(), String::new()];
        config.normalize().unwrap();
        assert_eq!(config.server.http.path, "/mcp");
        assert_eq!(config.server.http.allowed_hosts, vec!["a"]);
    }

    #[test]
    fn parses_approval_modes() {
        assert_eq!(
            "in_band".parse::<ApprovalMode>().unwrap(),
            ApprovalMode::InBand
        );
        assert_eq!(
            "out-of-band".parse::<ApprovalMode>().unwrap(),
            ApprovalMode::OutOfBand
        );
        assert_eq!("both".parse::<ApprovalMode>().unwrap(), ApprovalMode::Both);
        assert!("nope".parse::<ApprovalMode>().is_err());
    }
}
