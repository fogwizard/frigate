//! One long-lived FFmpeg per camera: segment recording + continuous raw RGB24 on stdout.

use crate::db::Db;
use crate::detect::DetectJob;
use crate::proc_guard;
use crate::record;
use anyhow::{Context, Result};
use chrono::Local;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info, warn};
use vigil_common::shm::{FrameRing, FORMAT_RGB8};
use vigil_common::{Config, UsbConfig};

enum SourceKind {
    Rtsp,
    Usb,
}

/// No raw frames, or a restream with nobody publishing, for this long means the
/// camera socket died (sleep, Wi-Fi drop) while FFmpeg is still running.
const STALL_MS: u64 = 20_000;

fn mono_ms() -> u64 {
    static BOOT: OnceLock<Instant> = OnceLock::new();
    BOOT.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Why the camera FFmpeg should be killed and started again.
/// `publisher_missing_ms` is set only when go2rtc answered and this camera has no producer.
fn restart_reason(stale_ms: u64, publisher_missing_ms: Option<u64>) -> Option<&'static str> {
    if stale_ms > STALL_MS {
        return Some("no frames");
    }
    if publisher_missing_ms.is_some_and(|ms| ms > STALL_MS) {
        return Some("go2rtc publisher missing");
    }
    None
}

pub async fn run_all_cameras(
    cfg: Arc<Config>,
    db: Arc<Db>,
    ring: Arc<FrameRing>,
    detect_tx: mpsc::Sender<DetectJob>,
) {
    if cfg.usb.enabled && cfg.usb.device.trim().is_empty() {
        warn!("usb.enabled but device is empty — USB mode skipped");
    }

    for (name, cam) in &cfg.cameras {
        if !cfg.record.enabled {
            warn!(camera = %name, "record disabled; combined mode skipped");
            continue;
        }

        let use_usb = cfg.usb.is_active_for(name);
        if cfg.usb.enabled && cfg.usb.only && !use_usb {
            debug!(camera = %name, "skipped (usb.only)");
            continue;
        }

        let source = if use_usb {
            cfg.usb.device.clone()
        } else {
            let record = cam.input_for_role("record");
            let detect = cam.input_for_role("detect");
            match (record, detect) {
                (Some(r), Some(d)) if r.path == d.path => r.path.clone(),
                (Some(_), Some(_)) => {
                    warn!(
                        camera = %name,
                        "record/detect RTSP differ; only combined (same URL) is supported, skip"
                    );
                    continue;
                }
                _ => {
                    warn!(
                        camera = %name,
                        "combined mode needs both record and detect roles on one input, skip"
                    );
                    continue;
                }
            }
        };

        let kind = if use_usb {
            SourceKind::Usb
        } else {
            SourceKind::Rtsp
        };
        let cfg = cfg.clone();
        let db = db.clone();
        let ring = ring.clone();
        let camera = name.clone();
        let tx = detect_tx.clone();
        tokio::spawn(async move {
            run_combined(cfg, db, ring, camera, source, kind, tx).await;
        });
    }
}

async fn run_combined(
    cfg: Arc<Config>,
    db: Arc<Db>,
    ring: Arc<FrameRing>,
    camera: String,
    source: String,
    kind: SourceKind,
    tx: mpsc::Sender<DetectJob>,
) {
    let mut backoff = 1u64;
    let live_dir = cfg.live_snapshots_dir();
    let _ = std::fs::create_dir_all(&live_dir);

    loop {
        match spawn_combined_ffmpeg(&cfg, &db, &camera, &source, &kind).await {
            Ok(mut child) => {
                let do_restream = restream_enabled(&cfg, &kind);
                info!(
                    camera = %camera,
                    source = %source,
                    usb = matches!(kind, SourceKind::Usb),
                    restream = do_restream,
                    "ffmpeg started (record+raw detect{})",
                    if do_restream { "+restream" } else { "" }
                );
                backoff = 1;
                let Some(stdout) = child.stdout.take() else {
                    warn!(camera = %camera, "no stdout pipe");
                    let _ = child.kill().await;
                    continue;
                };
                if let Some(stderr) = child.stderr.take() {
                    spawn_ffmpeg_stderr_logger(camera.clone(), stderr);
                }

                let clock = Arc::new(AtomicU64::new(mono_ms()));
                let raw = read_raw_loop(
                    cfg.clone(),
                    ring.clone(),
                    camera.clone(),
                    stdout,
                    tx.clone(),
                    clock.clone(),
                );
                let tracker =
                    record::segment_tracker_loop(cfg.clone(), db.clone(), camera.clone());
                let recover = watch_stream(
                    clock,
                    camera.clone(),
                    do_restream,
                    cfg.go2rtc.api.clone(),
                );

                let child_pid = child.id();
                tokio::select! {
                    r = raw => {
                        if let Err(e) = r {
                            warn!(camera = %camera, error = %e, "raw pipe ended");
                        }
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                    }
                    status = child.wait() => {
                        warn!(camera = %camera, ?status, "ffmpeg exited, reconnecting");
                    }
                    _ = tracker => {
                        warn!(camera = %camera, "segment tracker ended unexpectedly");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                    }
                    reason = recover => {
                        warn!(camera = %camera, reason, "restarting camera stream");
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                    }
                }
                if let Some(pid) = child_pid {
                    proc_guard::untrack_pid(pid);
                }
            }
            Err(e) => error!(camera = %camera, error = %e, "spawn ffmpeg"),
        }
        sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(60);
    }
}

fn restream_enabled(cfg: &Config, kind: &SourceKind) -> bool {
    match kind {
        // USB restream is opt-in: a missing go2rtc kills the whole process.
        SourceKind::Usb => cfg.usb.restream,
        SourceKind::Rtsp => cfg.stream.restream,
    }
}

fn detect_vf(cfg: &Config) -> String {
    format!(
        "fps={},scale={}:{}:flags=fast_bilinear",
        cfg.detect.fps.max(0.1),
        cfg.detect.width,
        cfg.detect.height
    )
}

/// Dahua IPC/NVR RTSP. The path stays one argv so `channel` and `subtype` are not split on `&`.
///
/// `subtype=0` is the main stream (often H.265). `subtype=1` is the sub stream, usually H.264,
/// which `-c copy` can publish to go2rtc for the mini-program.
fn push_dahua_rtsp_input_args(args: &mut Vec<String>, source: &str) {
    args.extend([
        "-rtsp_transport".into(),
        "tcp".into(),
        "-rtsp_flags".into(),
        "prefer_tcp".into(),
        // FFmpeg 5.x RTSP socket timeout, in microseconds. A silent Dahua peer must not hang forever.
        "-timeout".into(),
        "5000000".into(),
        "-user_agent".into(),
        "Dahua Rtsp Client".into(),
        // Skip the G.711 audio track. This pipeline maps video only.
        "-allowed_media_types".into(),
        "video".into(),
        "-fflags".into(),
        "+genpts+discardcorrupt".into(),
        "-avoid_negative_ts".into(),
        "make_zero".into(),
        "-use_wallclock_as_timestamps".into(),
        "1".into(),
        "-probesize".into(),
        "32768".into(),
        "-analyzeduration".into(),
        "1000000".into(),
        "-i".into(),
        source.to_string(),
    ]);
}

fn push_usb_input_args(args: &mut Vec<String>, usb: &UsbConfig) {
    let backend = usb.resolved_backend();
    if backend == "dshow" {
        // A 100MB queue held ~10s of frames, so the live picture lagged behind the room.
        // ~1s is enough to ride out a short encode stall without showing an old hand.
        args.extend([
            "-fflags".into(),
            "nobuffer".into(),
            "-flags".into(),
            "low_delay".into(),
            "-rtbufsize".into(),
            "8M".into(),
        ]);
    }
    args.push("-f".into());
    args.push(backend.into());
    // nv12 is cheaper than yuyv422 on most Windows webcams.
    if backend == "dshow" {
        args.extend(["-pixel_format".into(), "nv12".into()]);
    }
    if usb.width > 0 && usb.height > 0 {
        args.push("-video_size".into());
        args.push(format!("{}x{}", usb.width, usb.height));
    }
    if usb.fps > 0.0 {
        args.push("-framerate".into());
        args.push(format!("{}", usb.fps));
    }
    args.push("-i".into());
    match backend {
        "dshow" => args.push(format!("video={}", usb.device.trim())),
        _ => args.push(usb.device.trim().to_string()),
    }
}

fn spawn_ffmpeg_stderr_logger(camera: String, stderr: impl tokio::io::AsyncRead + Unpin + Send + 'static) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let lower = line.to_ascii_lowercase();
            if lower.contains("error")
                || lower.contains("failed")
                || lower.contains("could not")
                || lower.contains("not found")
            {
                warn!(camera = %camera, %line, "ffmpeg");
            } else {
                debug!(camera = %camera, %line, "ffmpeg");
            }
        }
    });
}

async fn spawn_combined_ffmpeg(
    cfg: &Config,
    db: &Db,
    camera: &str,
    source: &str,
    kind: &SourceKind,
) -> Result<Child> {
    let out_dir = cfg.recordings_dir().join(camera);
    std::fs::create_dir_all(&out_dir)?;
    // FFmpeg strftime uses the machine's local clock, not UTC.
    let today = Local::now().format("%Y-%m-%d").to_string();
    let day_dir = out_dir.join(&today);
    let new_day = !day_dir.exists();
    std::fs::create_dir_all(&day_dir)?;
    if new_day {
        record::prune_camera(cfg, db, camera);
    }
    let pattern = out_dir
        .join("%Y-%m-%d")
        .join("%H-%M-%S.mp4")
        .to_string_lossy()
        .to_string();
    let segment = cfg.record.segment_secs.to_string();
    let vf = detect_vf(cfg);
    let usb_mode = matches!(kind, SourceKind::Usb);
    let do_restream = restream_enabled(cfg, kind);

    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        "-nostdin".into(),
    ];

    match kind {
        SourceKind::Rtsp => {
            push_dahua_rtsp_input_args(&mut args, source);
        }
        SourceKind::Usb => {
            push_usb_input_args(&mut args, &cfg.usb);
            info!(
                camera = %camera,
                device = %cfg.usb.device,
                backend = cfg.usb.resolved_backend(),
                "using USB webcam input"
            );
        }
    }

    // Output 0: recording — copy for RTSP; re-encode for USB.
    args.extend(["-map".into(), "0:v:0".into(), "-an".into()]);
    if usb_mode {
        args.extend([
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "ultrafast".into(),
            "-tune".into(),
            "zerolatency".into(),
            "-g".into(),
            "15".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
        ]);
    } else {
        args.extend(["-c".into(), "copy".into()]);
    }
    args.extend([
        "-f".into(),
        "segment".into(),
        "-segment_time".into(),
        segment,
        "-reset_timestamps".into(),
        "1".into(),
        "-strftime".into(),
        "1".into(),
        // Header is written up front, so the file being recorded can be cut
        // before the next segment starts.
        "-segment_format_options".into(),
        "movflags=+frag_keyframe+empty_moov+default_base_moof".into(),
        pattern,
        // Output 1: raw RGB24 detection / live-preview frames
        "-map".into(),
        "0:v:0".into(),
        "-an".into(),
        "-vf".into(),
        vf,
        "-pix_fmt".into(),
        "rgb24".into(),
        "-f".into(),
        "rawvideo".into(),
        "pipe:1".into(),
    ]);

    // Output 2: restream to go2rtc (optional).
    if do_restream {
        let publish = cfg.stream.publish_url(camera);
        info!(camera = %camera, %publish, "ffmpeg restream to go2rtc");
        args.extend(["-map".into(), "0:v:0".into(), "-an".into()]);
        if usb_mode {
            args.extend([
                "-c:v".into(),
                "libx264".into(),
                "-preset".into(),
                "ultrafast".into(),
                "-tune".into(),
                "zerolatency".into(),
                "-bf".into(),
                "0".into(),
                "-g".into(),
                "15".into(),
                "-pix_fmt".into(),
                "yuv420p".into(),
            ]);
        } else {
            args.extend(["-c".into(), "copy".into()]);
        }
        args.extend([
            "-muxdelay".into(),
            "0".into(),
            "-muxpreload".into(),
            "0".into(),
            "-f".into(),
            "rtsp".into(),
            "-rtsp_transport".into(),
            "tcp".into(),
            publish,
        ]);
    }

    let mut cmd = Command::new(&cfg.ffmpeg.binary);
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    proc_guard::apply_spawn_hooks(&mut cmd);
    let child = cmd.spawn().context("spawn combined ffmpeg")?;
    proc_guard::track_child(&child);
    Ok(child)
}

async fn watch_stream(
    clock: Arc<AtomicU64>,
    camera: String,
    restream: bool,
    go2rtc_api: String,
) -> &'static str {
    let mut missing_since: Option<Instant> = None;
    loop {
        sleep(Duration::from_secs(5)).await;
        let stale_ms = mono_ms().saturating_sub(clock.load(Ordering::Relaxed));
        let publisher_missing_ms = if !restream {
            None
        } else {
            match publisher_alive(&go2rtc_api, &camera).await {
                Some(true) => {
                    missing_since = None;
                    None
                }
                Some(false) => {
                    let since = missing_since.get_or_insert_with(Instant::now);
                    Some(since.elapsed().as_millis() as u64)
                }
                None => {
                    missing_since = None;
                    None
                }
            }
        };
        if let Some(reason) = restart_reason(stale_ms, publisher_missing_ms) {
            return reason;
        }
    }
}

/// `Some` when go2rtc answered. `None` when it is down; the go2rtc supervisor restarts that process.
async fn publisher_alive(api: &str, camera: &str) -> Option<bool> {
    let base = api.trim().trim_end_matches('/');
    let base = base.strip_prefix("http://").unwrap_or(base);
    let base = base.strip_prefix("https://").unwrap_or(base);
    let url = format!("http://{base}/api/streams");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let body: serde_json::Value = client.get(url).send().await.ok()?.json().await.ok()?;
    Some(publishers_present(&body, camera))
}

fn publishers_present(body: &serde_json::Value, camera: &str) -> bool {
    body.get(camera)
        .and_then(|cam| cam.get("producers"))
        .and_then(|producers| producers.as_array())
        .is_some_and(|list| !list.is_empty())
}

async fn read_raw_loop<R: AsyncReadExt + Unpin>(
    cfg: Arc<Config>,
    ring: Arc<FrameRing>,
    camera: String,
    reader: R,
    tx: mpsc::Sender<DetectJob>,
    clock: Arc<AtomicU64>,
) -> Result<()> {
    let live_dir = cfg.live_snapshots_dir();
    let width = cfg.detect.width;
    let height = cfg.detect.height;
    let frame_len = cfg.detect.frame_bytes();
    let mut reader = BufReader::with_capacity(frame_len * 2, reader);
    let mut motion = crate::detect::MotionSampler::default();
    let jpeg_busy = Arc::new(AtomicBool::new(false));
    let mut last_jpeg = Instant::now() - Duration::from_secs(1);

    loop {
        let mut slot_w = match ring.begin_write(&camera, width, height, FORMAT_RGB8, frame_len) {
            Ok(w) => w,
            Err(e) => {
                warn!(camera = %camera, error = %e, "shm begin_write failed");
                continue;
            }
        };

        if let Err(e) = reader.read_exact(slot_w.payload_mut()).await {
            return Err(e.into());
        }
        clock.store(mono_ms(), Ordering::Relaxed);

        // JPEG is only the fallback preview. Encoding it inline stalled the raw pipe,
        // which stalled the go2rtc restream on the same FFmpeg process.
        if last_jpeg.elapsed() >= Duration::from_millis(200) && !jpeg_busy.load(Ordering::Relaxed) {
            last_jpeg = Instant::now();
            let rgb = slot_w.payload().to_vec();
            let path = live_dir.join(format!("{camera}.jpg"));
            let busy = jpeg_busy.clone();
            busy.store(true, Ordering::Relaxed);
            tokio::task::spawn_blocking(move || {
                if let Some(jpeg) = rgb_to_jpeg(&rgb, width, height, 70) {
                    // Write aside, then replace. Truncating cam.jpg in place lets the
                    // browser read a partial file and treat the preview as failed.
                    let tmp = path.with_extension("jpg.tmp");
                    if std::fs::write(&tmp, jpeg).is_ok() {
                        let _ = std::fs::rename(&tmp, &path);
                    }
                }
                busy.store(false, Ordering::Relaxed);
            });
        }

        let moved = if cfg.detect.motion {
            motion.moved(&camera, slot_w.payload(), width, height, cfg.detect.motion_threshold)
        } else {
            true
        };

        let (slot, seq) = match slot_w.commit() {
            Ok(v) => v,
            Err(e) => {
                warn!(camera = %camera, error = %e, "shm commit failed");
                continue;
            }
        };

        let job = DetectJob {
            camera: camera.clone(),
            slot,
            seq,
            moved,
        };
        if tx.try_send(job).is_err() {
            debug!(camera = %camera, slot, seq, "detect queue full, dropping notify");
        }
    }
}

/// Encode RGB8 → JPEG. Shared by live preview and event snapshots.
pub(crate) fn rgb_to_jpeg(rgb: &[u8], width: u32, height: u32, quality: u8) -> Option<Vec<u8>> {
    use image::{codecs::jpeg::JpegEncoder, ExtendedColorType, ImageEncoder};
    let mut out = Vec::new();
    let enc = JpegEncoder::new_with_quality(&mut out, quality);
    enc.write_image(rgb, width, height, ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vigil_common::UsbConfig;

    #[test]
    fn usb_backend_auto_windows_style_name() {
        let u = UsbConfig {
            device: "Integrated Camera".into(),
            backend: String::new(),
            ..Default::default()
        };
        #[cfg(windows)]
        assert_eq!(u.resolved_backend(), "dshow");
        #[cfg(not(windows))]
        assert_eq!(u.resolved_backend(), "v4l2");
    }

    #[test]
    fn usb_backend_dev_path() {
        let u = UsbConfig {
            device: "/dev/video0".into(),
            backend: String::new(),
            ..Default::default()
        };
        assert_eq!(u.resolved_backend(), "v4l2");
    }

    #[test]
    fn usb_input_args_dshow() {
        let u = UsbConfig {
            enabled: true,
            device: "HD Webcam".into(),
            width: 640,
            height: 480,
            fps: 30.0,
            backend: "dshow".into(),
            ..Default::default()
        };
        let mut args = Vec::new();
        push_usb_input_args(&mut args, &u);
        assert_eq!(
            args,
            vec![
                "-fflags",
                "nobuffer",
                "-flags",
                "low_delay",
                "-rtbufsize",
                "8M",
                "-f",
                "dshow",
                "-pixel_format",
                "nv12",
                "-video_size",
                "640x480",
                "-framerate",
                "30",
                "-i",
                "video=HD Webcam",
            ]
        );
    }

    #[test]
    fn dahua_rtsp_args_keep_query_in_one_argv() {
        let mut args = Vec::new();
        let url = "rtsp://admin:pass@192.168.1.108:554/cam/realmonitor?channel=1&subtype=1";
        push_dahua_rtsp_input_args(&mut args, url);
        assert_eq!(args.last().map(String::as_str), Some(url));
        assert!(args.windows(2).any(|w| w == ["-rtsp_transport", "tcp"]));
        assert!(args.windows(2).any(|w| w == ["-allowed_media_types", "video"]));
        assert!(args.windows(2).any(|w| w == ["-user_agent", "Dahua Rtsp Client"]));
        assert!(args.windows(2).any(|w| w == ["-timeout", "5000000"]));
    }

    #[test]
    fn a_quiet_stream_restarts_and_a_live_one_does_not() {
        assert_eq!(restart_reason(0, None), None);
        assert_eq!(restart_reason(STALL_MS, None), None);
        assert_eq!(restart_reason(STALL_MS + 1, None), Some("no frames"));
        assert_eq!(restart_reason(1_000, Some(STALL_MS)), None);
        assert_eq!(
            restart_reason(1_000, Some(STALL_MS + 1)),
            Some("go2rtc publisher missing")
        );
    }

    #[test]
    fn go2rtc_publishers_count_only_a_non_empty_list() {
        let body = serde_json::json!({
            "cam1": { "producers": [{ "id": 1 }] },
            "cam2": { "producers": null },
            "cam3": { "producers": [] }
        });
        assert!(publishers_present(&body, "cam1"));
        assert!(!publishers_present(&body, "cam2"));
        assert!(!publishers_present(&body, "cam3"));
        assert!(!publishers_present(&body, "cam4"));
    }

    #[test]
    fn usb_active_for_named_camera() {
        let u = UsbConfig {
            enabled: true,
            device: "cam".into(),
            camera: "cam1".into(),
            ..Default::default()
        };
        assert!(u.is_active_for("cam1"));
        assert!(!u.is_active_for("cam2"));
    }
}
