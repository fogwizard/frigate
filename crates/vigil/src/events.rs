use crate::db::Db;
use anyhow::Result;
use chrono::{Duration as ChronoDuration, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;
use vigil_common::{Config, Detection, Event};

struct ActiveEvent {
    event: Event,
    last_seen: chrono::DateTime<Utc>,
    max_score: f32,
}

pub struct EventEngine {
    cfg: Arc<Config>,
    db: Arc<Db>,
    active: Mutex<HashMap<String, ActiveEvent>>,
}

impl EventEngine {
    pub fn new(cfg: Arc<Config>, db: Arc<Db>) -> Self {
        Self {
            cfg,
            db,
            active: Mutex::new(HashMap::new()),
        }
    }

    pub async fn on_detections(&self, camera: &str, dets: &[Detection], jpeg: &[u8]) {
        let now = Utc::now();
        let mut started: Vec<Event> = Vec::new();
        let mut seen_labels = std::collections::HashSet::new();

        {
            let mut active = self.active.lock().await;
            for d in dets {
                let key = format!("{camera}:{}", d.label);
                seen_labels.insert(key.clone());
                if let Some(ae) = active.get_mut(&key) {
                    ae.last_seen = now;
                    ae.max_score = ae.max_score.max(d.score);
                    ae.event.score = ae.max_score;
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
                };
                if let Err(e) = self.db.insert_event(&event) {
                    warn!(error = %e, "insert event");
                    continue;
                }
                info!(camera = %camera, label = %d.label, id = %id, "event started");
                started.push(event.clone());
                active.insert(
                    key,
                    ActiveEvent {
                        event,
                        last_seen: now,
                        max_score: d.score,
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
                if !key.starts_with(&format!("{cam}:")) {
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

    async fn build_clip(&self, ev: &Event) -> Option<String> {
        let _ = std::fs::create_dir_all(self.cfg.clips_dir());
        let out = self.cfg.clips_dir().join(format!("{}.mp4", ev.id));
        let pre = ChronoDuration::seconds(self.cfg.events.pre_capture_secs as i64);
        let start = ev.start_time - pre;
        let end = ev.end_time.unwrap_or(Utc::now());

        let segs = self
            .db
            .list_segments(
                Some(&ev.camera),
                Some(start - ChronoDuration::seconds(120)),
                Some(end),
                50,
            )
            .ok()?;
        let paths: Vec<PathBuf> = segs
            .into_iter()
            .filter(|s| {
                let seg_end = s.end_time.unwrap_or(Utc::now());
                s.start_time <= end && seg_end >= start
            })
            .map(|s| PathBuf::from(s.path))
            .filter(|p| p.exists())
            .collect();

        if paths.is_empty() {
            return None;
        }

        if paths.len() == 1 {
            if copy_segment(&self.cfg.ffmpeg.binary, &paths[0], &out)
                .await
                .is_ok()
            {
                return Some(out.to_string_lossy().to_string());
            }
            return None;
        }

        let list_path = self.cfg.clips_dir().join(format!("{}.txt", ev.id));
        let list_body: String = paths
            .iter()
            .map(|p| format!("file '{}'", p.to_string_lossy().replace('\\', "/")))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(&list_path, list_body);
        let ok = Command::new(&self.cfg.ffmpeg.binary)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "concat",
                "-safe",
                "0",
                "-i",
                &list_path.to_string_lossy(),
                "-c",
                "copy",
                "-y",
                &out.to_string_lossy(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);
        let _ = std::fs::remove_file(&list_path);
        if ok {
            Some(out.to_string_lossy().to_string())
        } else {
            None
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
        }
    }
}

async fn copy_segment(ffmpeg: &str, input: &PathBuf, output: &PathBuf) -> Result<()> {
    let status = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            &input.to_string_lossy(),
            "-c",
            "copy",
            "-y",
            &output.to_string_lossy(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("ffmpeg clip failed")
    }
}
