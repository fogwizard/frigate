use crate::db::Db;
use chrono::{NaiveDate, Utc};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};
use vigil_common::Config;

/// Poll recording directory and update SQLite until cancelled.
pub async fn segment_tracker_loop(cfg: Arc<Config>, db: Arc<Db>, camera: String) {
    let interval = Duration::from_secs(cfg.record.segment_secs.max(5) as u64);
    let mut last_seen: Option<PathBuf> = None;
    let mut last_day: Option<PathBuf> = None;
    let mut open_id: Option<i64> = None;
    let camera_root = cfg.recordings_dir().join(&camera);
    // Existing backlog is trimmed as soon as this camera's tracker starts.
    // A later day-folder change (ffmpeg strftime at midnight) trims again.
    prune_expired_days(&camera_root, cfg.record.retain_days, &db, &camera);

    loop {
        sleep(interval).await;
        if let Some(latest) = newest_mp4(&camera_root) {
            if let Some(day) = latest.parent() {
                if last_day.as_deref() != Some(day) {
                    prune_expired_days(&camera_root, cfg.record.retain_days, &db, &camera);
                    last_day = Some(day.to_path_buf());
                }
            }
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

/// When a new `YYYY-MM-DD` directory shows up, drop day directories older than `retain_days`.
pub fn prune_expired_days(camera_root: &Path, retain_days: u32, db: &Db, camera: &str) {
    prune_expired_days_on(camera_root, retain_days, db, camera, Utc::now().date_naive());
}

fn prune_expired_days_on(
    camera_root: &Path,
    retain_days: u32,
    db: &Db,
    camera: &str,
    today: NaiveDate,
) {
    // Keep `retain_days` calendar folders, including today. Older day dirs are removed.
    let cutoff = today - chrono::Duration::days(retain_days.saturating_sub(1) as i64);
    let mut old: Vec<(NaiveDate, PathBuf)> = day_dirs(camera_root)
        .into_iter()
        .filter(|(date, _)| *date < cutoff)
        .collect();
    old.sort_by_key(|(date, _)| *date);
    for (date, dir) in old {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {
                let n = db.delete_segments_under(&dir).unwrap_or(0);
                info!(%camera, %date, segments = n, dir = %dir.display(), "deleted recording day older than retain window");
            }
            Err(e) => warn!(%camera, dir = %dir.display(), error = %e, "delete recording day"),
        }
    }
}

fn day_dirs(camera_root: &Path) -> Vec<(NaiveDate, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(camera_root) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let name = path.file_name()?.to_str()?;
            let date = NaiveDate::parse_from_str(name, "%Y-%m-%d").ok()?;
            Some((date, path))
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn new_day_deletes_dirs_older_than_a_week() {
        let root = std::env::temp_dir().join(format!("vigil-rec-{}", uuid::Uuid::new_v4()));
        let cam = root.join("cam1");
        std::fs::create_dir_all(cam.join("2026-10-01")).unwrap();
        std::fs::create_dir_all(cam.join("2026-10-13")).unwrap();
        std::fs::create_dir_all(cam.join("2026-10-14")).unwrap();
        std::fs::create_dir_all(cam.join("2026-10-15")).unwrap();
        std::fs::create_dir_all(cam.join("2026-10-21")).unwrap();
        std::fs::write(cam.join("2026-10-01").join("00-00-00.mp4"), b"old").unwrap();
        let db_path = root.join("t.db");
        let db = Db::open(&db_path).unwrap();
        let old_start = chrono::DateTime::parse_from_rfc3339("2026-10-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.insert_segment(
            "cam1",
            &cam.join("2026-10-01").join("00-00-00.mp4").to_string_lossy(),
            old_start,
        )
        .unwrap();

        prune_expired_days_on(
            &cam,
            7,
            &db,
            "cam1",
            NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(),
        );

        assert!(!cam.join("2026-10-01").exists());
        assert!(!cam.join("2026-10-13").exists());
        assert!(!cam.join("2026-10-14").exists());
        assert!(cam.join("2026-10-15").exists());
        assert!(cam.join("2026-10-21").exists());
        assert!(db.list_segments(Some("cam1"), None, None, 10).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
