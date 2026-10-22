use crate::events::EventEngine;
use crate::infer_client::InferClient;
use crate::ingest;
use chrono::Utc;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::warn;
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
    while let Some(job) = rx.recv().await {
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
