use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub mqtt: Option<serde_yaml::Value>,
    #[serde(default)]
    pub detect: DetectConfig,
    #[serde(default)]
    pub openvino: OpenvinoConfig,
    #[serde(default)]
    pub cameras: HashMap<String, CameraConfig>,
    #[serde(default)]
    pub record: RecordConfig,
    #[serde(default)]
    pub events: EventsConfig,
    #[serde(default)]
    pub web: WebConfig,
    #[serde(default)]
    pub paths: PathsConfig,
    #[serde(default)]
    pub webhook: Option<WebhookConfig>,
    #[serde(default)]
    pub ffmpeg: FfmpegConfig,
    #[serde(default)]
    pub infer: InferConfig,
    /// HTTP-FLV playback for WeChat mini-program (go2rtc + signed URLs).
    #[serde(default)]
    pub stream: StreamConfig,
    /// Local go2rtc process. Started at daemon boot when its API port is closed.
    #[serde(default)]
    pub go2rtc: Go2rtcConfig,
    /// WeChat mini-program login / boarding / device APIs.
    #[serde(default)]
    pub miniprogram: MiniprogramConfig,
    /// Local USB webcam for testing (replaces RTSP for one camera).
    #[serde(default)]
    pub usb: UsbConfig,
    /// Feeder serial: last step of /api/feeding writes one Modbus RTU frame.
    #[serde(default)]
    pub feed: FeedConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectConfig {
    #[serde(default = "default_detect_fps")]
    pub fps: f32,
    /// Detection pipe resolution (raw RGB24 from FFmpeg).
    #[serde(default = "default_detect_width")]
    pub width: u32,
    #[serde(default = "default_detect_height")]
    pub height: u32,
    #[serde(default = "default_labels")]
    pub labels: Vec<String>,
    #[serde(default = "default_score_threshold")]
    pub score_threshold: f32,
    /// Skip inference when the frame is almost unchanged and nothing is currently tracked.
    #[serde(default = "default_motion_enabled")]
    pub motion: bool,
    /// Fraction of the coarse grid that must change before a frame is sent to the model.
    #[serde(default = "default_motion_threshold")]
    pub motion_threshold: f32,
}

fn default_detect_fps() -> f32 {
    5.0
}
fn default_detect_width() -> u32 {
    640
}
fn default_detect_height() -> u32 {
    360
}
fn default_labels() -> Vec<String> {
    vec![
        "person".into(),
        "cat".into(),
        "dog".into(),
    ]
}
fn default_score_threshold() -> f32 {
    0.5
}
fn default_motion_enabled() -> bool {
    true
}
fn default_motion_threshold() -> f32 {
    0.02
}

impl Default for DetectConfig {
    fn default() -> Self {
        Self {
            fps: default_detect_fps(),
            width: default_detect_width(),
            height: default_detect_height(),
            labels: default_labels(),
            score_threshold: default_score_threshold(),
            motion: default_motion_enabled(),
            motion_threshold: default_motion_threshold(),
        }
    }
}

impl DetectConfig {
    pub fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 3
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenvinoConfig {
    #[serde(default = "default_device")]
    pub device: String,
    #[serde(default = "default_model")]
    pub model: String,
}

fn default_device() -> String {
    "GPU".into()
}
fn default_model() -> String {
    "models/yolo11n.onnx".into()
}

impl Default for OpenvinoConfig {
    fn default() -> Self {
        Self {
            device: default_device(),
            model: default_model(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraConfig {
    pub ffmpeg: CameraFfmpeg,
    /// Modbus RTU slave of the feeder for this camera. Required when `feed.port` is set.
    #[serde(default)]
    pub feed_slave: Option<u8>,
    /// How long this camera's feeder stays open, in seconds.
    #[serde(default = "default_feed_time")]
    pub feed_time: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraFfmpeg {
    pub inputs: Vec<CameraInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraInput {
    pub path: String,
    #[serde(default)]
    pub roles: Vec<String>,
}

impl CameraConfig {
    pub fn input_for_role(&self, role: &str) -> Option<&CameraInput> {
        self.ffmpeg
            .inputs
            .iter()
            .find(|i| i.roles.iter().any(|r| r == role))
            .or_else(|| self.ffmpeg.inputs.first())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Delete the oldest recordings once `paths.data_dir` grows past this many gibibytes.
    #[serde(default = "default_max_gb")]
    pub max_gb: u64,
    #[serde(default = "default_segment_secs")]
    pub segment_secs: u32,
}

fn default_true() -> bool {
    true
}
fn default_max_gb() -> u64 {
    100
}
fn default_segment_secs() -> u32 {
    60
}

impl Default for RecordConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_gb: default_max_gb(),
            segment_secs: default_segment_secs(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventsConfig {
    #[serde(default = "default_pre_capture")]
    pub pre_capture_secs: u32,
    #[serde(default = "default_post_capture")]
    pub post_capture_secs: u32,
}

fn default_pre_capture() -> u32 {
    5
}
fn default_post_capture() -> u32 {
    5
}

impl Default for EventsConfig {
    fn default() -> Self {
        Self {
            pre_capture_secs: default_pre_capture(),
            post_capture_secs: default_post_capture(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
}

fn default_bind() -> String {
    "0.0.0.0:8970".into()
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathsConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("data")
}
fn default_db_path() -> PathBuf {
    PathBuf::from("data/vigil.db")
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            db_path: default_db_path(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub url: String,
}

/// Use a local USB webcam instead of RTSP (dev / demo).
///
/// Windows: DirectShow (`ffmpeg -list_devices true -f dshow -i dummy`).
/// macOS: AVFoundation (`ffmpeg -f avfoundation -list_devices true -i ""`).
/// Linux: V4L2 (`/dev/video0`, or `v4l2-ctl --list-devices`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsbConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Camera name (Windows), index or name (macOS, e.g. `0`), or device path (Linux, e.g. `/dev/video0`).
    #[serde(default)]
    pub device: String,
    /// Which configured camera id receives the USB feed.
    #[serde(default = "default_usb_camera")]
    pub camera: String,
    /// Capture size (0 = let driver choose).
    #[serde(default = "default_usb_width")]
    pub width: u32,
    #[serde(default = "default_usb_height")]
    pub height: u32,
    #[serde(default = "default_usb_fps")]
    pub fps: f32,
    /// Force backend: `dshow` | `avfoundation` | `v4l2` | empty = auto by OS / device path.
    #[serde(default)]
    pub backend: String,
    /// Publish USB cam to go2rtc (needs :8554 up). Default off — a dead restream
    /// target kills the whole FFmpeg (no detect / live snapshot either).
    #[serde(default)]
    pub restream: bool,
    /// When true, only start the USB-bound camera (skip other RTSP placeholders).
    #[serde(default = "default_true")]
    pub only: bool,
}

fn default_usb_camera() -> String {
    "cam1".into()
}
fn default_usb_width() -> u32 {
    640
}
fn default_usb_height() -> u32 {
    480
}
fn default_usb_fps() -> f32 {
    15.0
}

impl Default for UsbConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            device: String::new(),
            camera: default_usb_camera(),
            width: default_usb_width(),
            height: default_usb_height(),
            fps: default_usb_fps(),
            backend: String::new(),
            restream: false,
            only: true,
        }
    }
}

impl UsbConfig {
    pub fn is_active_for(&self, camera: &str) -> bool {
        self.enabled && !self.device.trim().is_empty() && self.camera == camera
    }

    /// Resolved capture backend: `dshow`, `avfoundation`, or `v4l2`.
    pub fn resolved_backend(&self) -> &'static str {
        match self.backend.trim().to_ascii_lowercase().as_str() {
            "dshow" | "directshow" => "dshow",
            "avfoundation" | "avf" => "avfoundation",
            "v4l2" | "v4l" => "v4l2",
            _ if self.device.starts_with("/dev/") => "v4l2",
            _ if cfg!(windows) => "dshow",
            _ if cfg!(target_os = "macos") => "avfoundation",
            _ => "v4l2",
        }
    }
}

/// Modbus RTU write used by the feed command.
///
/// Empty `port` skips the serial write (auth still succeeds).
/// Slave address is per camera (`cameras.*.feed_slave`), not global.
/// Default frame is function 0x06 (write single holding register), 8N1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedConfig {
    /// Windows `COM3`, Linux `/dev/ttyUSB0`. Empty = do not open a port.
    #[serde(default)]
    pub port: String,
    #[serde(default = "default_feed_baud")]
    pub baud: u32,
    /// 0x05 write coil, 0x06 write single register.
    #[serde(default = "default_feed_function")]
    pub function: u8,
    #[serde(default)]
    pub address: u16,
    /// Register value, or non-zero to energize a coil (0x05).
    #[serde(default = "default_feed_value")]
    pub value: u16,
    /// Kept so older configs still load. The feed write waits 500 character times derived from `baud`.
    #[serde(default = "default_feed_timeout_ms")]
    pub timeout_ms: u64,
    /// How long the feeder stays open. The clip of this interval is sent to the VLM.
    #[serde(default = "default_feed_open_secs")]
    pub open_secs: u64,
    /// Value written to close the feeder after `open_secs`.
    #[serde(default)]
    pub close_value: u16,
    /// Kept so older configs still load. Feeding success comes from the VLM, not this threshold.
    #[serde(default = "default_feed_motion_threshold")]
    pub motion_threshold: f32,
    #[serde(default)]
    pub vlm: FeedVlmConfig,
}

/// Qwen vision model that watches the feed clip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedVlmConfig {
    /// OpenAI-compatible root, without `/chat/completions`.
    #[serde(default = "default_vlm_base")]
    pub base_url: String,
    #[serde(default = "default_vlm_model")]
    pub model: String,
    /// Optional. A local server does not need one. A remote server falls back to `DASHSCOPE_API_KEY`.
    #[serde(default)]
    pub api_key: String,
}

fn default_feed_baud() -> u32 {
    9600
}
fn default_feed_function() -> u8 {
    0x06
}
fn default_feed_value() -> u16 {
    1
}
fn default_feed_timeout_ms() -> u64 {
    500
}
fn default_feed_open_secs() -> u64 {
    8
}
fn default_feed_time() -> u64 {
    8
}
fn default_feed_motion_threshold() -> f32 {
    0.05
}
fn default_vlm_base() -> String {
    "http://127.0.0.1:8000/v1".into()
}
fn default_vlm_model() -> String {
    "qwen-vl-max-latest".into()
}

impl Default for FeedVlmConfig {
    fn default() -> Self {
        Self {
            base_url: default_vlm_base(),
            model: default_vlm_model(),
            api_key: String::new(),
        }
    }
}

impl Default for FeedConfig {
    fn default() -> Self {
        Self {
            port: String::new(),
            baud: default_feed_baud(),
            function: default_feed_function(),
            address: 0,
            value: default_feed_value(),
            timeout_ms: default_feed_timeout_ms(),
            open_secs: default_feed_open_secs(),
            close_value: 0,
            motion_threshold: default_feed_motion_threshold(),
            vlm: FeedVlmConfig::default(),
        }
    }
}

impl FeedConfig {
    pub fn serial_enabled(&self) -> bool {
        !self.port.trim().is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FfmpegConfig {
    #[serde(default = "default_ffmpeg")]
    pub binary: String,
}

fn default_ffmpeg() -> String {
    "ffmpeg".into()
}

impl Default for FfmpegConfig {
    fn default() -> Self {
        Self {
            binary: default_ffmpeg(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferConfig {
    /// Unix socket path (Linux) or TCP host:port (e.g. 127.0.0.1:9870)
    #[serde(default = "default_infer_endpoint")]
    pub endpoint: String,
    #[serde(default = "default_infer_bin")]
    pub binary: String,
    #[serde(default = "default_queue_size")]
    pub queue_size: usize,
    /// Shared frame ring (mmap file). Created by vigil, opened by vigil-infer.
    #[serde(default = "default_shm_path")]
    pub shm_path: String,
    #[serde(default = "default_shm_slots")]
    pub shm_slots: u32,
    /// Max bytes per frame slot (RGB24).
    #[serde(default = "default_shm_slot_bytes")]
    pub shm_slot_bytes: u32,
}

fn default_infer_endpoint() -> String {
    if cfg!(windows) {
        "127.0.0.1:9870".into()
    } else {
        "/run/vigil/infer.sock".into()
    }
}
fn default_infer_bin() -> String {
    "vigil-infer".into()
}
fn default_queue_size() -> usize {
    // Pending (slot, seq) notifies; keep below shm_slots headroom.
    32
}
fn default_shm_path() -> String {
    "data/frames.shm".into()
}
fn default_shm_slots() -> u32 {
    // Multi-camera frame ring: enough slots that ingest can publish ahead of
    // detect/infer without immediately overwriting in-flight frames.
    64
}
fn default_shm_slot_bytes() -> u32 {
    // Fits 1280x720 RGB24; raise if detect.width/height larger.
    2_764_800
}

impl Default for InferConfig {
    fn default() -> Self {
        Self {
            endpoint: default_infer_endpoint(),
            binary: default_infer_bin(),
            queue_size: default_queue_size(),
            shm_path: default_shm_path(),
            shm_slots: default_shm_slots(),
            shm_slot_bytes: default_shm_slot_bytes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamConfig {
    #[serde(default)]
    pub enabled: bool,
    /// When true, combined FFmpeg also publishes a copy bitstream to go2rtc
    /// (camera is only opened once by Vigil; go2rtc does not pull the camera).
    #[serde(default = "default_true")]
    pub restream: bool,
    /// FFmpeg publish URL template, e.g. `rtsp://127.0.0.1:8554/{camera}`.
    #[serde(default = "default_stream_publish_url")]
    pub publish_url_template: String,
    /// Public HTTPS FLV origin (Nginx → go2rtc HTTP-FLV), no trailing slash.
    #[serde(default = "default_stream_public_base")]
    pub public_base: String,
    /// Path template; `{camera}` replaced with camera name (e.g. `/{camera}` → `/cam1`).
    #[serde(default = "default_stream_path_template")]
    pub path_template: String,
    /// URL handed to the mini-program `live-player`. `{camera}` is the stream name.
    /// Default is go2rtc HTTP-FLV, the same link ffplay uses.
    #[serde(default = "default_stream_play_url")]
    pub play_url_template: String,
    #[serde(default = "default_stream_sign_secret")]
    pub sign_secret: String,
    #[serde(default = "default_stream_sign_ttl")]
    pub sign_ttl_secs: u64,
}

fn default_stream_public_base() -> String {
    "https://stream.example.com".into()
}
fn default_stream_path_template() -> String {
    "/{camera}".into()
}
fn default_stream_play_url() -> String {
    "http://127.0.0.1:1984/api/stream.flv?src={camera}".into()
}
fn default_stream_sign_secret() -> String {
    "change-me".into()
}
fn default_stream_sign_ttl() -> u64 {
    120
}
fn default_stream_publish_url() -> String {
    "rtsp://127.0.0.1:8554/{camera}".into()
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            restream: true,
            publish_url_template: default_stream_publish_url(),
            public_base: default_stream_public_base(),
            path_template: default_stream_path_template(),
            play_url_template: default_stream_play_url(),
            sign_secret: default_stream_sign_secret(),
            sign_ttl_secs: default_stream_sign_ttl(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Go2rtcConfig {
    /// Executable. Absolute path, or a name on PATH.
    #[serde(default = "default_go2rtc_binary")]
    pub binary: String,
    /// Config file, relative to the process working directory (the project root).
    #[serde(default = "default_go2rtc_config")]
    pub config: String,
    /// API address used to decide whether a process is already up.
    #[serde(default = "default_go2rtc_api")]
    pub api: String,
}

fn default_go2rtc_binary() -> String {
    "go2rtc".into()
}
fn default_go2rtc_config() -> String {
    "packaging/go2rtc.yaml".into()
}
fn default_go2rtc_api() -> String {
    "127.0.0.1:1984".into()
}

impl Default for Go2rtcConfig {
    fn default() -> Self {
        Self {
            binary: default_go2rtc_binary(),
            config: default_go2rtc_config(),
            api: default_go2rtc_api(),
        }
    }
}

impl StreamConfig {
    pub fn flv_path(&self, camera: &str) -> String {
        self.path_template.replace("{camera}", camera)
    }

    /// Absolute FLV URL without query string, e.g. `https://host/cam1.flv`.
    pub fn flv_url(&self, camera: &str) -> String {
        let base = self.public_base.trim_end_matches('/');
        let path = self.flv_path(camera);
        let path = if path.ends_with(".flv") {
            path
        } else {
            format!("{path}.flv")
        };
        if path.starts_with('/') {
            format!("{base}{path}")
        } else {
            format!("{base}/{path}")
        }
    }

    /// Local go2rtc RTSP publisher URL for this camera.
    pub fn publish_url(&self, camera: &str) -> String {
        self.publish_url_template.replace("{camera}", camera)
    }

    /// HTTP-FLV URL for the mini-program player.
    pub fn play_url(&self, camera: &str) -> String {
        self.play_url_template.replace("{camera}", camera)
    }

    /// Browser `<video>` URL. go2rtc's fMP4 endpoint, derived from the FLV template.
    pub fn browser_play_url(&self, camera: &str) -> String {
        self.play_url(camera).replace("stream.flv", "stream.mp4")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiniprogramConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Skip WeChat code2session / pay; issue stable demo openid and auto-activate boarding.
    #[serde(default = "default_true")]
    pub demo_mode: bool,
    #[serde(default = "default_demo_openid")]
    pub demo_openid: String,
    #[serde(default)]
    pub wechat_app_id: String,
    #[serde(default)]
    pub wechat_app_secret: String,
    #[serde(default = "default_mp_session_ttl")]
    pub session_ttl_secs: i64,
    /// OpenIDs that receive role=admin (full camera list).
    #[serde(default)]
    pub admin_openids: Vec<String>,
    /// Cameras where admin gets canControl=true (feed/water).
    #[serde(default)]
    pub control_cameras: Vec<String>,
}

fn default_demo_openid() -> String {
    "demo-admin".into()
}
fn default_mp_session_ttl() -> i64 {
    86_400
}

impl Default for MiniprogramConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            demo_mode: true,
            demo_openid: default_demo_openid(),
            wechat_app_id: String::new(),
            wechat_app_secret: String::new(),
            session_ttl_secs: default_mp_session_ttl(),
            admin_openids: vec!["demo-admin".into()],
            control_cameras: Vec::new(),
        }
    }
}

impl MiniprogramConfig {
    pub fn is_admin(&self, openid: &str) -> bool {
        self.admin_openids.iter().any(|o| o == openid)
    }

    pub fn can_control(&self, camera: &str) -> bool {
        self.control_cameras.is_empty() || self.control_cameras.iter().any(|c| c == camera)
    }
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let mut cfg: Config = serde_yaml::from_str(&text)
            .with_context(|| format!("parsing config {}", path.display()))?;
        cfg.ffmpeg.binary = crate::paths::resolve_tool(&cfg.ffmpeg.binary);
        cfg.go2rtc.binary = crate::paths::resolve_tool(&cfg.go2rtc.binary);
        cfg.infer.binary = crate::paths::resolve_tool(&cfg.infer.binary);
        Ok(cfg)
    }

    pub fn recordings_dir(&self) -> PathBuf {
        self.paths.data_dir.join("recordings")
    }

    pub fn clips_dir(&self) -> PathBuf {
        self.paths.data_dir.join("clips")
    }

    pub fn snapshots_dir(&self) -> PathBuf {
        self.paths.data_dir.join("snapshots")
    }

    pub fn live_snapshots_dir(&self) -> PathBuf {
        self.paths.data_dir.join("live")
    }
}
