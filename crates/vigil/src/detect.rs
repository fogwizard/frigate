use crate::events::EventEngine;
use crate::infer_client::InferClient;
use crate::ingest;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
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
    let mut motion = MotionGate::default();
    while let Some(job) = rx.recv().await {
        if cfg.detect.motion && !camera_has_objects(&statuses, &job.camera).await {
            match infer.ring().peek(job.slot, job.seq) {
                Ok(frame) if frame.format == FORMAT_RGB8 => {
                    if !motion.scene_changed(
                        &job.camera,
                        &frame.data,
                        frame.width,
                        frame.height,
                        cfg.detect.motion_threshold,
                    ) {
                        debug!(camera = %job.camera, "skip inference, scene unchanged");
                        continue;
                    }
                }
                _ => {}
            }
        }
        match infer.infer_slot(job.slot, job.seq).await {
            Ok(resp) => {
                if let Some(err) = &resp.error {
                    warn!(error = %err, "infer error");
                }
                let filtered: Vec<Detection> = resp
                    .detections
                    .into_iter()
                    .filter(|d| {
                        cfg.detect.labels.iter().any(|l| l == &d.label)
                            && d.score >= cfg.detect.score_threshold
                    })
                    .collect();
                let mut st = statuses.write().await;
                if let Some(s) = st.get_mut(&job.camera) {
                    if !filtered.is_empty() {
                        s.last_detection = Some(Utc::now());
                    }
                    s.objects = filtered.clone();
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

#[derive(Default)]
struct MotionGate {
    prev: HashMap<String, Vec<u8>>,
}

impl MotionGate {
    fn scene_changed(&mut self, camera: &str, rgb: &[u8], width: u32, height: u32, threshold: f32) -> bool {
        let grid = luma_grid(rgb, width, height);
        let changed = match self.prev.get(camera) {
            Some(prev) => changed_ratio(prev, &grid, 24),
            None => 1.0,
        };
        self.prev.insert(camera.to_string(), grid);
        changed >= threshold
    }
}

fn luma_grid(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0u8; (GRID_W * GRID_H) as usize];
    if width == 0 || height == 0 || rgb.len() < (width * height * 3) as usize {
        return out;
    }
    for gy in 0..GRID_H {
        for gx in 0..GRID_W {
            let x = (gx * width / GRID_W).min(width - 1);
            let y = (gy * height / GRID_H).min(height - 1);
            let i = ((y * width + x) * 3) as usize;
            let luma = (u16::from(rgb[i]) * 77
                + u16::from(rgb[i + 1]) * 150
                + u16::from(rgb[i + 2]) * 29)
                >> 8;
            out[(gy * GRID_W + gx) as usize] = luma as u8;
        }
    }
    out
}

fn changed_ratio(prev: &[u8], next: &[u8], delta: u8) -> f32 {
    if prev.len() != next.len() || prev.is_empty() {
        return 1.0;
    }
    let changed = prev
        .iter()
        .zip(next)
        .filter(|(a, b)| a.abs_diff(**b) > delta)
        .count();
    changed as f32 / prev.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn still_scene_skips_and_a_real_change_does_not() {
        let mut gate = MotionGate::default();
        let still = vec![40u8; 16 * 16 * 3];
        assert!(gate.scene_changed("cam1", &still, 16, 16, 0.02));
        assert!(!gate.scene_changed("cam1", &still, 16, 16, 0.02));
        let mut moved = still.clone();
        for px in moved.iter_mut().step_by(2) {
            *px = 220;
        }
        assert!(gate.scene_changed("cam1", &moved, 16, 16, 0.02));
    }
}
