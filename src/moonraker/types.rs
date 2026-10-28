//! Small, tolerant data models for the Moonraker responses we normalize.
//!
//! Moonraker evolves; unknown fields are ignored and most fields have defaults
//! so a newer Moonraker cannot break the MCP server. Anything we do not model
//! is returned as `serde_json::Value` (redacted where appropriate).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// `server.info` response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerInfo {
    pub klippy_connected: bool,
    pub klippy_state: String,
    pub components: Vec<String>,
    pub failed_components: Vec<String>,
    pub registered_directories: Vec<String>,
    pub warnings: Vec<String>,
    pub websocket_count: u64,
    pub moonraker_version: String,
    pub api_version: Vec<u64>,
    pub api_version_string: String,
}

/// `printer.info` response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PrinterInfo {
    pub state: String,
    pub state_message: String,
    pub hostname: String,
    pub klipper_path: String,
    pub python_path: String,
    pub process_id: u64,
    pub user_id: u64,
    pub group_id: u64,
    pub log_file: String,
    pub config_file: String,
    pub software_version: String,
    pub cpu_info: String,
}

/// One webcam from `server.webcams.list`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Webcam {
    pub name: String,
    pub enabled: bool,
    pub icon: Option<String>,
    pub aspect_ratio: Option<String>,
    pub target_fps: Option<f64>,
    pub target_fps_idle: Option<f64>,
    pub location: Option<String>,
    pub service: Option<String>,
    pub stream_url: Option<String>,
    pub snapshot_url: Option<String>,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    pub rotation: Option<i64>,
    pub source: Option<String>,
    pub uid: Option<String>,
    pub extra_data: Option<Value>,
}

/// `server.webcams.list` response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebcamList {
    pub webcams: Vec<Webcam>,
}

/// `printer.objects.list` response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ObjectList {
    pub objects: Vec<String>,
}

/// `printer.objects.query` response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ObjectQuery {
    pub eventtime: f64,
    pub status: Map<String, Value>,
}

/// `printer.print_stats` subset, extracted from an object query.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PrintStats {
    pub filename: String,
    pub state: String,
    pub message: String,
    pub print_duration: f64,
    pub total_duration: f64,
    pub filament_used: f64,
}

/// `server.files.list` entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FileEntry {
    pub path: String,
    pub modified: f64,
    pub size: u64,
    pub permissions: Option<String>,
}

/// `printer.query_endstops.status` response is a map of name -> state.
pub type EndstopStatus = Map<String, Value>;

/// `server.history.list` response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryList {
    pub count: u64,
    pub jobs: Vec<Value>,
}

/// `server.job_queue.status` response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JobQueueStatus {
    pub queued_jobs: Vec<Value>,
    pub queue_state: String,
}

/// `server.temperature_store` response.
pub type TemperatureStore = Map<String, Value>;

/// `server.gcode_store` response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GcodeStore {
    pub gcode_store: Vec<Value>,
}

/// `machine.proc_stats` response.
pub type ProcStats = Value;

/// `machine.system_info` response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemInfoResponse {
    pub system_info: Value,
}

/// `server.config` response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub config: Value,
    pub orig: Value,
    pub files: Vec<Value>,
}
