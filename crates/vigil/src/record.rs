use crate::db::Db;
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::warn;
use vigil_common::Config;

/// Poll recording directory and update SQLite until cancelled.
pub async fn segment_tracker_loop(cfg: Arc<Config>, db: Arc<Db>, camera: String) {
    let interval = Duration::from_secs(cfg.record.segment_secs.max(5) as u64);
    let mut last_seen: Option<PathBuf> = None;
    let mut open_id: Option<i64> = None;

    loop {
        sleep(interval).await;
        if let Some(latest) = newest_mp4(&cfg.recordings_dir().join(&camera)) {
            if last_seen.as_ref() != Some(&latest) {
                if let Some(id) = open_id.take() {
                    let _ = db.finish_segment(id, Utc::now());
                }
                let rel = latest.to_string_lossy().to_string();
                match db.insert_segment(&camera, &rel, Utc::now()) {
                    Ok(id) => open_id = Some(id),
                    Err(e) => warn!(error = %e, "insert segment"),
                }
                last_seen = Some(latest);
            }
        }
    }
}

fn newest_mp4(root: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for p in walkdir(root) {
        if p.extension().and_then(|e| e.to_str()) != Some("mp4") {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&p) else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
            best = Some((modified, p));
        }
    }
    best.map(|(_, p)| p)
}

fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            out.extend(walkdir(&p));
        } else {
            out.push(p);
        }
    }
    out
}
