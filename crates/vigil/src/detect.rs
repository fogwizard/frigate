use crate::events::EventEngine;
use crate::infer_client::InferClient;
use crate::ingest;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, warn};
use vigil_common::shm::{FrameRing, FORMAT_RGB8};
use vigil_common::{CameraStatus, Config, Detection};

pub type CameraStatusMap = Arc<RwLock<std::collections::HashMap<String, CameraStatus>>>;

pub fn new_status_map(cameras: &[String]) -> CameraStatusMap {
    let mut m = std::collections::HashMap::new();
    for c in cameras {
        m.insert(
            c.clone(),
            CameraStatus {
                name: c.clone(),
                recording: true,
                detecting: true,
                last_detection: None,
                objects: Vec::new(),
            },
        );
    }
    Arc::new(RwLock::new(m))
}

/// Lightweight notify: pixels already live in shm at (slot, seq).
pub struct DetectJob {
    pub camera: String,
    pub slot: u32,
    pub seq: u64,
    /// True when this frame differs from the previous one, or motion gating is off.
    pub moved: bool,
}

pub async fn run_detect_pipeline(
    cfg: Arc<Config>,
    db: Arc<crate::db::Db>,
    infer: Arc<InferClient>,
    events: Arc<EventEngine>,
    statuses: CameraStatusMap,
) {
    let ring = infer.ring();
    let (tx, rx) = mpsc::channel::<DetectJob>(cfg.infer.queue_size);

    let worker_cfg = cfg.clone();
    let worker_infer = infer.clone();
    let worker_events = events.clone();
    let worker_statuses = statuses.clone();
    tokio::spawn(async move {
        detect_worker(worker_cfg, worker_infer, worker_events, worker_statuses, rx).await;
    });

    ingest::run_all_cameras(cfg, db, ring, tx).await;
}

async fn detect_worker(
    cfg: Arc<Config>,
    infer: Arc<InferClient>,
    events: Arc<EventEngine>,
    statuses: CameraStatusMap,
    mut rx: mpsc::Receiver<DetectJob>,
) {
    let mut idle = IdleProbe::default();
    let mut empty_streak: HashMap<String, u32> = HashMap::new();
    while let Some(mut job) = rx.recv().await {
        let mut skipped = 0u32;
        while let Ok(newer) = rx.try_recv() {
            job = newer;
            skipped += 1;
        }
        if skipped > 0 {
            debug!(camera = %job.camera, skipped, "dropped stale detect frames");
        }
        let tracking = camera_has_objects(&statuses, &job.camera).await;
        if cfg.detect.motion && !job.moved && !tracking && !idle.due(&job.camera, Duration::from_secs(5)) {
            debug!(camera = %job.camera, "skip inference, scene still");
            continue;
        }
        match infer.infer_slot(job.slot, job.seq).await {
            Ok(resp) => {
                if let Some(err) = &resp.error {
                    warn!(error = %err, "infer error");
                    continue;
                }
                let filtered: Vec<Detection> = resp
                    .detections
                    .into_iter()
                    .filter(|d| {
                        cfg.detect.labels.iter().any(|l| l == &d.label)
                            && d.score >= cfg.detect.score_threshold
                    })
                    .collect();
                let clear_boxes = if filtered.is_empty() {
                    let n = empty_streak.entry(job.camera.clone()).or_insert(0);
                    *n += 1;
                    *n >= 3
                } else {
                    empty_streak.insert(job.camera.clone(), 0);
                    true
                };
                if clear_boxes {
                    let mut st = statuses.write().await;
                    if let Some(s) = st.get_mut(&job.camera) {
                        if !filtered.is_empty() {
                            s.last_detection = Some(Utc::now());
                        }
                        s.objects = filtered.clone();
                    }
                }
                // Snapshot from shm if the slot was not overwritten yet.
                let snap = if filtered.is_empty() {
                    Vec::new()
                } else {
                    frame_to_jpeg_snapshot(infer.ring().as_ref(), &job)
                };
                events
                    .on_detections(&job.camera, &filtered, &snap)
                    .await;
            }
            Err(e) => warn!(error = %e, "infer call failed"),
        }
    }
}

fn frame_to_jpeg_snapshot(ring: &FrameRing, job: &DetectJob) -> Vec<u8> {
    let Ok(frame) = ring.peek(job.slot, job.seq) else {
        return Vec::new();
    };
    if frame.format != FORMAT_RGB8 {
        return Vec::new();
    }
    ingest::rgb_to_jpeg(&frame.data, frame.width, frame.height, 80).unwrap_or_default()
}

async fn camera_has_objects(statuses: &CameraStatusMap, camera: &str) -> bool {
    let st = statuses.read().await;
    st.get(camera).is_some_and(|s| !s.objects.is_empty())
}

const GRID_W: u32 = 32;
const GRID_H: u32 = 18;

/// Compares a coarse luma grid of frames the ingest loop already holds.
/// This stays off the shared-memory read path so a still scene does not race the detector.
#[derive(Default)]
pub(crate) struct MotionSampler {
    prev: HashMap<String, Vec<u8>>,
}

impl MotionSampler {
    pub(crate) fn moved(&mut self, camera: &str, rgb: &[u8], width: u32, height: u32, threshold: f32) -> bool {
        let grid = luma_grid(rgb, width, height);
        let ratio = match self.prev.get(camera) {
            Some(prev) => changed_ratio(prev, &grid, 24),
            None => 1.0,
        };
        self.prev.insert(camera.to_string(), grid);
        ratio >= threshold
    }
}

fn luma_grid(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0u8; (GRID_W * GRID_H) as usize];
    if width == 0 || height == 0 || rgb.len() < (width as usize) * (height as usize) * 3 {
        return out;
    }
    for gy in 0..GRID_H {
        for gx in 0..GRID_W {
            let x = (gx * width / GRID_W).min(width - 1);
            let y = (gy * height / GRID_H).min(height - 1);
            let i = ((y * width + x) * 3) as usize;
            let luma = (u16::from(rgb[i]) * 77 + u16::from(rgb[i + 1]) * 150 + u16::from(rgb[i + 2]) * 29) >> 8;
            out[(gy * GRID_W + gx) as usize] = luma as u8;
        }
    }
    out
}

fn changed_ratio(prev: &[u8], next: &[u8], delta: u8) -> f32 {
    if prev.len() != next.len() || prev.is_empty() {
        return 1.0;
    }
    let changed = prev.iter().zip(next).filter(|(a, b)| a.abs_diff(**b) > delta).count();
    changed as f32 / prev.len() as f32
}

/// Still, empty scenes are rechecked on this interval so a missed stationary object is found again.
#[derive(Default)]
struct IdleProbe {
    last: HashMap<String, Instant>,
}

impl IdleProbe {
    fn due(&mut self, camera: &str, interval: Duration) -> bool {
        let now = Instant::now();
        if self.last.get(camera).is_some_and(|t| now.duration_since(*t) < interval) {
            return false;
        }
        self.last.insert(camera.to_string(), now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn still_frame_does_not_move_until_the_picture_changes() {
        let mut sampler = MotionSampler::default();
        let still = vec![40u8; 16 * 16 * 3];
        assert!(sampler.moved("cam1", &still, 16, 16, 0.02));
        assert!(!sampler.moved("cam1", &still, 16, 16, 0.02));
        let mut moved = still.clone();
        for px in moved.iter_mut().step_by(2) {
            *px = 220;
        }
        assert!(sampler.moved("cam1", &moved, 16, 16, 0.02));
    }

    #[test]
    fn idle_probe_waits_out_its_interval() {
        let mut probe = IdleProbe::default();
        assert!(probe.due("cam1", Duration::from_secs(60)));
        assert!(!probe.due("cam1", Duration::from_secs(60)));
        assert!(probe.due("cam1", Duration::ZERO));
    }
}
