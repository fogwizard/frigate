use crate::events::EventEngine;
use crate::infer_client::InferClient;
use crate::ingest;
use chrono::Utc;
use std::collections::{HashMap, HashSet};
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

    let feed_cfg = cfg.clone();
    let feed_db = db.clone();
    tokio::spawn(async move {
        feed_loop(feed_cfg, feed_db).await;
    });

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

enum FeedSeen {
    Busy(vigil_common::Event),
    Done,
    None,
}

fn feed_seen(db: &crate::db::Db, camera: &str, now: chrono::DateTime<Utc>) -> anyhow::Result<FeedSeen> {
    let today = crate::feed::local_date(now);
    if let Some(ev) = db.latest_feed_event(camera, today)? {
        return Ok(if crate::feed::unfinished(&ev.note) {
            FeedSeen::Busy(ev)
        } else {
            FeedSeen::Done
        });
    }
    if let Some(yesterday) = today.pred_opt() {
        if let Some(ev) = db.latest_feed_event(camera, yesterday)? {
            if crate::feed::unfinished(&ev.note) {
                return Ok(FeedSeen::Busy(ev));
            }
        }
    }
    Ok(FeedSeen::None)
}

/// Feed timing does not follow detect frames. A stalled camera or a slow
/// inference call must still close the feeder.
async fn feed_loop(cfg: Arc<Config>, db: Arc<crate::db::Db>) {
    let mut feeds = crate::feed::CameraFeeds::default();
    let mut fed_today: HashMap<(chrono::NaiveDate, String), bool> = HashMap::new();
    let mut missing_slave = HashSet::new();
    release_stuck_feeders(&cfg, &db).await;
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let now = Utc::now();
        let day = crate::feed::local_date(now);
        fed_today.retain(|(seen, _), _| *seen == day);
        let cameras: Vec<String> = cfg.cameras.keys().cloned().collect();
        for camera in cameras {
            tick_feed(
                cfg.clone(),
                db.clone(),
                &mut feeds,
                &mut fed_today,
                &mut missing_slave,
                &camera,
            )
            .await;
        }
    }
}

/// Close feeders a dead process may have left on. An automatic feed that is
/// still inside its open interval is left alone so the loop can finish it.
async fn release_stuck_feeders(cfg: &Config, db: &crate::db::Db) {
    if !cfg.feed.serial_enabled() {
        return;
    }
    let now = Utc::now();
    for (camera, cam) in &cfg.cameras {
        if cam.feed_time == 0 {
            continue;
        }
        let Some(slave) = cam.feed_slave else {
            warn!(%camera, "feeder slave not configured");
            continue;
        };
        let still_open = match feed_seen(db, camera, now) {
            Ok(FeedSeen::Busy(ev)) => {
                let elapsed = now.signed_duration_since(ev.start_time).num_seconds();
                crate::feed::local_hour(now) == 7
                    && elapsed >= 0
                    && (elapsed as u64) < cam.feed_time
            }
            Ok(_) => false,
            Err(e) => {
                warn!(%camera, error = %e, "read feed event before startup close");
                false
            }
        };
        if still_open {
            continue;
        }
        match write_feed(cfg, slave, cfg.feed.close_value).await {
            Ok(()) => tracing::info!(%camera, "feeder closed at startup"),
            Err(e) => warn!(%camera, error = %e, "feeder close at startup failed"),
        }
    }
}

async fn tick_feed(
    cfg: Arc<Config>,
    db: Arc<crate::db::Db>,
    feeds: &mut crate::feed::CameraFeeds,
    fed_today: &mut HashMap<(chrono::NaiveDate, String), bool>,
    missing_slave: &mut HashSet<String>,
    camera: &str,
) {
    use crate::feed::{claim_feed, save_event, Step, NOTE_REVIEWING};

    let open_secs = cfg.cameras.get(camera).map(|c| c.feed_time).unwrap_or(8);
    if open_secs == 0 && !feeds.is_open(camera) {
        return;
    }
    let slave = match cfg.cameras.get(camera).and_then(|c| c.feed_slave) {
        Some(slave) => slave,
        None if !cfg.feed.serial_enabled() => 0,
        None => {
            if missing_slave.insert(camera.to_string()) {
                warn!(%camera, "feeder slave not configured");
            }
            return;
        }
    };

    let now = Utc::now();
    let key = (crate::feed::local_date(now), camera.to_string());
    let already = if fed_today.get(&key) == Some(&true) {
        true
    } else {
        match feed_seen(&db, camera, now) {
            Ok(FeedSeen::Done) => {
                fed_today.insert(key.clone(), true);
                true
            }
            Ok(FeedSeen::Busy(ev)) => {
                feeds.adopt(camera, ev.start_time, ev.id);
                false
            }
            Ok(FeedSeen::None) => false,
            Err(e) => {
                warn!(%camera, error = %e, "read feed event");
                if !feeds.is_open(camera) {
                    return;
                }
                false
            }
        }
    };

    match feeds.step(camera, now, already, open_secs) {
        Step::None => {}
        Step::Open => {
            let id = if let Some(id) = feeds.event_id(camera) {
                id
            } else {
                match claim_feed(&db, &cfg, camera, now) {
                    Some(id) => {
                        feeds.set_event(camera, id);
                        id
                    }
                    None => {
                        feeds.fail_open(camera, now);
                        return;
                    }
                }
            };
            match write_feed(&cfg, slave, cfg.feed.value).await {
                Ok(()) => tracing::info!(%camera, slave, "feeder opened"),
                Err(e) => {
                    warn!(%camera, error = %e, "feeder open failed");
                    crate::feed::record_open_failure(now);
                    if db.delete_event(id).is_err() {
                        let _ = db.update_event_note(id, "失败：开启喂食器失败", 0.0);
                    }
                    feeds.fail_open(camera, now);
                }
            }
        }
        Step::Close { start, write, event_id } => {
            if write {
                match write_feed(&cfg, slave, cfg.feed.close_value).await {
                    Ok(()) => feeds.ack_close(camera),
                    Err(e) => {
                        warn!(%camera, error = %e, "feeder close failed");
                        return;
                    }
                }
            }
            let id = if let Some(id) = event_id.or_else(|| feeds.event_id(camera)) {
                id
            } else {
                match claim_feed(&db, &cfg, camera, start) {
                    Some(id) => {
                        feeds.set_event(camera, id);
                        id
                    }
                    None => return,
                }
            };
            if !feeds.mark_reviewed(camera) {
                return;
            }
            let end = Utc::now();
            let _ = db.update_event_note(id, NOTE_REVIEWING, 0.0);
            let _ = db.update_event_end(id, end, None, 0.0);
            fed_today.insert((crate::feed::local_date(start), camera.to_string()), true);
            tracing::info!(%camera, "feed event recorded");
            let db = db.clone();
            let cfg = cfg.clone();
            let camera = camera.to_string();
            tokio::spawn(async move {
                crate::feed::review_feed(db, cfg, camera, id, start, end, None).await;
            });
        }
        Step::GiveUp => {
            let note = "失败：开启喂食器失败";
            if save_event(&db, &cfg, camera, now, note, false).is_some() {
                feeds.mark_give_up(camera);
                fed_today.insert(key, true);
                tracing::info!(%camera, %note, "feed event recorded");
            }
        }
    }
}

async fn write_feed(cfg: &Config, slave: u8, value: u16) -> anyhow::Result<()> {
    let feed = cfg.feed.clone();
    tokio::task::spawn_blocking(move || {
        vigil_modbus::write_feed(
            &feed.port,
            feed.baud,
            slave,
            feed.function,
            feed.address,
            value,
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("feed task: {e}"))?
    .map_err(|e| anyhow::anyhow!(e))
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

pub(crate) fn luma_grid(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
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

pub(crate) fn changed_ratio(prev: &[u8], next: &[u8], delta: u8) -> f32 {
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
    fn yesterdays_finished_feed_does_not_count_as_this_morning() {
        use chrono::{Local, TimeZone};
        use uuid::Uuid;

        let path = std::env::temp_dir().join(format!("vigil-feed-seen-{}.db", Uuid::new_v4()));
        let db = crate::db::Db::open(&path).unwrap();
        let yesterday = Local
            .with_ymd_and_hms(2026, 11, 2, 8, 0, 23)
            .unwrap()
            .with_timezone(&Utc);
        let this_morning = Local
            .with_ymd_and_hms(2026, 11, 3, 7, 12, 0)
            .unwrap()
            .with_timezone(&Utc);
        db.insert_event(&vigil_common::Event {
            id: Uuid::new_v4(),
            camera: "cam1".into(),
            label: "feed".into(),
            score: 0.0,
            start_time: yesterday,
            end_time: Some(yesterday),
            snapshot_path: None,
            clip_path: None,
            note: Some("失败：开启喂食器失败".into()),
        })
        .unwrap();
        assert!(matches!(
            feed_seen(&db, "cam1", this_morning).unwrap(),
            FeedSeen::None
        ));

        let started = Local
            .with_ymd_and_hms(2026, 11, 3, 7, 10, 0)
            .unwrap()
            .with_timezone(&Utc);
        db.insert_event(&vigil_common::Event {
            id: Uuid::new_v4(),
            camera: "cam1".into(),
            label: "feed".into(),
            score: 0.0,
            start_time: started,
            end_time: Some(started),
            snapshot_path: None,
            clip_path: None,
            note: Some("失败：开启喂食器失败".into()),
        })
        .unwrap();
        assert!(matches!(
            feed_seen(&db, "cam1", this_morning).unwrap(),
            FeedSeen::Done
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn idle_probe_waits_out_its_interval() {
        let mut probe = IdleProbe::default();
        assert!(probe.due("cam1", Duration::from_secs(60)));
        assert!(!probe.due("cam1", Duration::from_secs(60)));
        assert!(probe.due("cam1", Duration::ZERO));
    }
}
