//! One long-lived FFmpeg per camera: segment recording + continuous raw RGB24 on stdout.

use crate::db::Db;
use crate::detect::DetectJob;
use crate::proc_guard;
use crate::record;
use anyhow::{Context, Result};
use chrono::Utc;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info, warn};
use vigil_common::shm::{FrameRing, FORMAT_RGB8};
use vigil_common::Config;

pub async fn run_all_cameras(
    cfg: Arc<Config>,
    db: Arc<Db>,
    ring: Arc<FrameRing>,
    detect_tx: mpsc::Sender<DetectJob>,
) {
    for (name, cam) in &cfg.cameras {
        if !cfg.record.enabled {
            warn!(camera = %name, "record disabled; combined mode skipped");
            continue;
        }

        let record = cam.input_for_role("record");
        let detect = cam.input_for_role("detect");
        let rtsp = match (record, detect) {
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
        };

        let cfg = cfg.clone();
        let db = db.clone();
        let ring = ring.clone();
        let camera = name.clone();
        let tx = detect_tx.clone();
        tokio::spawn(async move {
            run_combined(cfg, db, ring, camera, rtsp, tx).await;
        });
    }
}

async fn run_combined(
    cfg: Arc<Config>,
    db: Arc<Db>,
    ring: Arc<FrameRing>,
    camera: String,
    rtsp: String,
    tx: mpsc::Sender<DetectJob>,
) {
    let mut backoff = 1u64;
    let live_dir = cfg.live_snapshots_dir();
    let _ = std::fs::create_dir_all(&live_dir);

    loop {
        match spawn_combined_ffmpeg(&cfg, &camera, &rtsp).await {
            Ok(mut child) => {
                info!(
                    camera = %camera,
                    restream = cfg.stream.restream,
                    "ffmpeg started (record+raw detect{})",
                    if cfg.stream.restream { "+restream" } else { "" }
                );
                backoff = 1;
                let Some(stdout) = child.stdout.take() else {
                    warn!(camera = %camera, "no stdout pipe");
                    let _ = child.kill().await;
                    continue;
                };

                let raw =
                    read_raw_loop(cfg.clone(), ring.clone(), camera.clone(), stdout, tx.clone());
                let tracker =
                    record::segment_tracker_loop(cfg.clone(), db.clone(), camera.clone());

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
                        // Should not finish; if it does, never leave ffmpeg orphaned.
                        warn!(camera = %camera, "segment tracker ended unexpectedly");
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

fn detect_vf(cfg: &Config) -> String {
    format!(
        "fps={},scale={}:{}:flags=fast_bilinear",
        cfg.detect.fps.max(0.1),
        cfg.detect.width,
        cfg.detect.height
    )
}

async fn spawn_combined_ffmpeg(cfg: &Config, camera: &str, rtsp: &str) -> Result<Child> {
    let out_dir = cfg.recordings_dir().join(camera);
    std::fs::create_dir_all(&out_dir)?;
    let today = Utc::now().format("%Y-%m-%d").to_string();
    std::fs::create_dir_all(out_dir.join(&today))?;
    let pattern = out_dir
        .join("%Y-%m-%d")
        .join("%H-%M-%S.mp4")
        .to_string_lossy()
        .to_string();
    let segment = cfg.record.segment_secs.to_string();
    let vf = detect_vf(cfg);

    // One FFmpeg: record + detect pipe [+ optional RTSP publish to MediaMTX].
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        "-nostdin".into(),
        "-rtsp_transport".into(),
        "tcp".into(),
        "-i".into(),
        rtsp.to_string(),
        // Output 0: recording (bitstream copy)
        "-map".into(),
        "0:v:0".into(),
        "-an".into(),
        "-c".into(),
        "copy".into(),
        "-f".into(),
        "segment".into(),
        "-segment_time".into(),
        segment,
        "-segment_atclocktime".into(),
        "1".into(),
        "-reset_timestamps".into(),
        "1".into(),
        "-strftime".into(),
        "1".into(),
        pattern,
        // Output 1: raw RGB24 detection frames
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
    ];

    // Output 2: restream to MediaMTX (publisher). Camera is not pulled again by MTX.
    if cfg.stream.restream {
        let publish = cfg.stream.publish_url(camera);
        info!(camera = %camera, %publish, "ffmpeg restream to MediaMTX");
        args.extend([
            "-map".into(),
            "0:v:0".into(),
            "-an".into(),
            "-c".into(),
            "copy".into(),
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
        // Never leave stderr piped unread — full pipe can stall ffmpeg forever.
        .stderr(Stdio::null())
        .kill_on_drop(true);

    proc_guard::apply_spawn_hooks(&mut cmd);
    let child = cmd.spawn().context("spawn combined ffmpeg")?;
    proc_guard::track_child(&child);
    Ok(child)
}

async fn read_raw_loop<R: AsyncReadExt + Unpin>(
    cfg: Arc<Config>,
    ring: Arc<FrameRing>,
    camera: String,
    reader: R,
    tx: mpsc::Sender<DetectJob>,
) -> Result<()> {
    let live_dir = cfg.live_snapshots_dir();
    let width = cfg.detect.width;
    let height = cfg.detect.height;
    let frame_len = cfg.detect.frame_bytes();
    let mut reader = BufReader::with_capacity(frame_len * 2, reader);

    // UI preview: write JPEG at most ~2 Hz to avoid encode cost on every frame
    let mut last_preview = std::time::Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(std::time::Instant::now);

    loop {
        // Read FFmpeg rawvideo straight into a claimed shm slot (no staging Vec).
        let mut slot_w = match ring.begin_write(&camera, width, height, FORMAT_RGB8, frame_len) {
            Ok(w) => w,
            Err(e) => {
                warn!(camera = %camera, error = %e, "shm begin_write failed");
                continue;
            }
        };

        if let Err(e) = reader.read_exact(slot_w.payload_mut()).await {
            // Drop aborts WRITING slot.
            return Err(e.into());
        }

        if last_preview.elapsed() >= Duration::from_millis(500) {
            if let Some(jpeg) = rgb_to_jpeg(slot_w.payload(), width, height) {
                let live_path = live_dir.join(format!("{camera}.jpg"));
                let _ = std::fs::write(&live_path, jpeg);
            }
            last_preview = std::time::Instant::now();
        }

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
            width,
            height,
        };
        if tx.try_send(job).is_err() {
            debug!(camera = %camera, slot, seq, "detect queue full, dropping notify");
        }
    }
}

fn rgb_to_jpeg(rgb: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    use image::{codecs::jpeg::JpegEncoder, ExtendedColorType, ImageEncoder};
    let mut out = Vec::new();
    let mut enc = JpegEncoder::new_with_quality(&mut out, 70);
    enc.write_image(rgb, width, height, ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}
