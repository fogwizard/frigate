use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CameraId(pub String);

impl CameraId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for CameraId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for CameraId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detection {
    pub label: String,
    pub score: f32,
    /// Normalized xyxy in [0, 1]: [x1, y1, x2, y2]
    pub xyxy: [f32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,
    pub camera: String,
    pub label: String,
    pub score: f32,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub snapshot_path: Option<String>,
    pub clip_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub id: i64,
    pub camera: String,
    pub path: String,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub ok: bool,
    pub version: String,
    pub cameras: usize,
    pub infer_connected: bool,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraStatus {
    pub name: String,
    pub recording: bool,
    pub detecting: bool,
    pub last_detection: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub health: HealthStatus,
    pub cameras: Vec<CameraStatus>,
}
