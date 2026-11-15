use crate::db::Db;
use chrono::{Duration as ChronoDuration, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;
use vigil_common::{Config, Detection, Event};

struct ActiveEvent {
    event: Event,
    last_seen: chrono::DateTime<Utc>,
    max_score: f32,
    box_xyxy: [f32; 4],
}

/// Intersection over union of two normalized xyxy boxes.
fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let x1 = a[0].max(b[0]);
    let y1 = a[1].max(b[1]);
    let x2 = a[2].min(b[2]);
    let y2 = a[3].min(b[3]);
    let iw = (x2 - x1).max(0.0);
    let ih = (y2 - y1).max(0.0);
    let inter = iw * ih;
    let area_a = (a[2] - a[0]).max(0.0) * (a[3] - a[1]).max(0.0);
    let area_b = (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    let union = area_a + area_b - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

pub struct EventEngine {
    cfg: Arc<Config>,
    db: Arc<Db>,
    active: Mutex<HashMap<String, ActiveEvent>>,
    clip_tries: Mutex<HashMap<Uuid, u8>>,
}

impl EventEngine {
    pub fn new(cfg: Arc<Config>, db: Arc<Db>) -> Self {
        Self {
            cfg,
            db,
            active: Mutex::new(HashMap::new()),
            clip_tries: Mutex::new(HashMap::new()),
        }
    }

    pub async fn on_detections(&self, camera: &str, dets: &[Detection], jpeg: &[u8]) {
        let now = Utc::now();
        let mut started: Vec<Event> = Vec::new();
        let mut seen_labels = std::collections::HashSet::new();

        {
            let mut active = self.active.lock().await;
            let mut claimed = std::collections::HashSet::new();
            for d in dets {
                let mut best: Option<(String, f32)> = None;
                for (key, ae) in active.iter() {
                    if claimed.contains(key) {
                        continue;
                    }
                    if ae.event.camera != camera || ae.event.label != d.label {
                        continue;
                    }
                    let overlap = iou(ae.box_xyxy, d.xyxy);
                    if overlap < 0.3 {
                        continue;
                    }
                    if best.as_ref().map(|(_, score)| overlap > *score).unwrap_or(true) {
                        best = Some((key.clone(), overlap));
                    }
                }
                if let Some((key, _)) = best {
                    claimed.insert(key.clone());
                    seen_labels.insert(key.clone());
                    if let Some(ae) = active.get_mut(&key) {
                        ae.last_seen = now;
                        ae.max_score = ae.max_score.max(d.score);
                        ae.event.score = ae.max_score;
                        ae.box_xyxy = d.xyxy;
                    }
                    continue;
                }
                if jpeg.is_empty() {
                    continue;
                }
                let id = Uuid::new_v4();
                let snap_path = self.cfg.snapshots_dir().join(format!("{id}.jpg"));
                let _ = std::fs::create_dir_all(self.cfg.snapshots_dir());
                let _ = std::fs::write(&snap_path, jpeg);
                let event = Event {
                    id,
                    camera: camera.to_string(),
                    label: d.label.clone(),
                    score: d.score,
                    start_time: now,
                    end_time: None,
                    snapshot_path: Some(snap_path.to_string_lossy().to_string()),
                    clip_path: None,
                    note: None,
                };
                if let Err(e) = self.db.insert_event(&event) {
                    warn!(error = %e, "insert event");
                    continue;
                }
                info!(camera = %camera, label = %d.label, id = %id, "event started");
                started.push(event.clone());
                let key = id.to_string();
                seen_labels.insert(key.clone());
                active.insert(
                    key,
                    ActiveEvent {
                        event,
                        last_seen: now,
                        max_score: d.score,
                        box_xyxy: d.xyxy,
                    },
                );
            }
        }

        for ev in &started {
            self.maybe_webhook(ev).await;
        }
        self.finish_timed_out(Some(camera), &seen_labels).await;
    }

    async fn finish_timed_out(
        &self,
        camera_filter: Option<&str>,
        keep: &std::collections::HashSet<String>,
    ) {
        let now = Utc::now();
        let timeout = ChronoDuration::seconds(self.cfg.events.post_capture_secs as i64);
        let mut active = self.active.lock().await;
        let mut finished = Vec::new();
        active.retain(|key, ae| {
            if let Some(cam) = camera_filter {
                if ae.event.camera != cam {
                    return true;
                }
            }
            if keep.contains(key) {
                return true;
            }
            if now - ae.last_seen > timeout {
                finished.push(ae.event.clone());
                false
            } else {
                true
            }
        });
        drop(active);

        for mut ev in finished {
            ev.end_time = Some(now);
            let clip = self.build_clip(&ev).await;
            ev.clip_path = clip.clone();
            let _ = self
                .db
                .update_event_end(ev.id, now, clip.as_deref(), ev.score);
            info!(id = %ev.id, camera = %ev.camera, "event ended");
            self.maybe_webhook(&ev).await;
        }
    }

    /// `start_time` is written when the event opens and `end_time` when it closes.
    /// The clip is the recording between those two clocks, plus the pre-roll.
    /// Cutting waits until the segment that contains `end_time` has been closed,
    /// because the file ffmpeg is still writing cannot be read reliably.
    async fn build_clip(&self, ev: &Event) -> Option<String> {
        let pre = ChronoDuration::seconds(self.cfg.events.pre_capture_secs as i64);
        let start = ev.start_time - pre;
        let end = ev.end_time.unwrap_or_else(Utc::now);
        match crate::feed::cut_detection_clip(&self.cfg, &ev.camera, ev.id, start, end).await {
            crate::feed::ClipCut::Done(path) => Some(path),
            crate::feed::ClipCut::Wait | crate::feed::ClipCut::Failed => None,
        }
    }

    async fn maybe_webhook(&self, event: &Event) {
        let Some(ref wh) = self.cfg.webhook else {
            return;
        };
        let client = reqwest::Client::new();
        if let Err(e) = client.post(&wh.url).json(event).send().await {
            warn!(error = %e, "webhook failed");
        }
    }

    pub async fn sweep_timeouts(self: Arc<Self>) {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            tick.tick().await;
            let empty = std::collections::HashSet::new();
            self.finish_timed_out(None, &empty).await;
            self.fill_missing_clips().await;
        }
    }

    async fn fill_missing_clips(&self) {
        let since = Utc::now() - ChronoDuration::hours(12);
        let pending = match self.db.events_missing_clips(since) {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "list events missing clips");
                return;
            }
        };
        let mut cut = false;
        for ev in pending {
            if self.clip_tries.lock().await.get(&ev.id).copied().unwrap_or(0) >= 5 {
                continue;
            }
            let pre = ChronoDuration::seconds(self.cfg.events.pre_capture_secs as i64);
            let start = ev.start_time - pre;
            let Some(end) = ev.end_time else {
                continue;
            };
            match crate::feed::cut_detection_clip(&self.cfg, &ev.camera, ev.id, start, end).await {
                crate::feed::ClipCut::Wait => {}
                crate::feed::ClipCut::Done(path) => {
                    self.clip_tries.lock().await.remove(&ev.id);
                    if let Err(e) = self.db.set_event_clip(ev.id, &path) {
                        warn!(error = %e, id = %ev.id, "save event clip");
                    } else {
                        info!(id = %ev.id, camera = %ev.camera, %path, "event clip saved");
                    }
                    cut = true;
                }
                crate::feed::ClipCut::Failed => {
                    let mut tries = self.clip_tries.lock().await;
                    let n = tries.entry(ev.id).or_insert(0);
                    *n += 1;
                    if *n == 5 {
                        warn!(id = %ev.id, camera = %ev.camera, "event clip failed");
                    }
                    cut = true;
                }
            }
            if cut {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::iou;

    #[test]
    fn separate_boxes_are_different_tracks() {
        let left = [0.0, 0.0, 0.2, 0.4];
        let right = [0.7, 0.1, 0.95, 0.5];
        assert!(iou(left, right) < 0.3);
        let shifted = [0.02, 0.02, 0.22, 0.42];
        assert!(iou(left, shifted) >= 0.3);
    }
}
