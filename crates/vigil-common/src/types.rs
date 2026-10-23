use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    /// Operator note, used by feed events (`成功` or a `失败：…` reason).
    #[serde(default)]
    pub note: Option<String>,
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
    /// Latest boxes for this camera, normalized xyxy. Empty when nothing is in frame.
    #[serde(default)]
    pub objects: Vec<Detection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub health: HealthStatus,
    pub cameras: Vec<CameraStatus>,
}
