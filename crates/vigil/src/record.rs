use crate::db::Db;
use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
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
    prune_camera(&cfg, &db, &camera);

    loop {
        sleep(interval).await;
        if let Some(latest) = newest_mp4(&camera_root) {
            if let Some(day) = latest.parent() {
                if last_day.as_deref() != Some(day) {
                    prune_camera(&cfg, &db, &camera);
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

/// Drop local day directories once they fall outside the retain window.
/// `retain_days` includes today, so 3 keeps three folders and the fourth local
/// day deletes the oldest one. Folder names come from FFmpeg's local clock.
/// The same cutoff removes that day's clips and event rows, so the list has
/// nothing left to open.
pub fn prune_camera(cfg: &Config, db: &Db, camera: &str) {
    let today = Local::now().date_naive();
    let root = cfg.recordings_dir().join(camera);
    prune_expired_days_on(&root, cfg.record.retain_days, db, camera, today);
    prune_expired_clips_and_events(&cfg.clips_dir(), cfg.record.retain_days, db, today);
}

/// Delete clips and the events that point at them once their local day is
/// outside the same window as recordings.
pub fn prune_expired_clips_and_events(clips_dir: &Path, retain_days: u32, db: &Db, today: NaiveDate) {
    let cutoff = today - chrono::Duration::days(retain_days.saturating_sub(1) as i64);
    let before = local_midnight_utc(cutoff);
    let removed = match db.delete_old_events(before) {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "delete events for expired days");
            Vec::new()
        }
    };
    let mut files = 0usize;
    for (snap, clip) in &removed {
        if remove_file(snap) {
            files += 1;
        }
        if remove_file(clip) {
            files += 1;
        }
        if let Some(path) = clip {
            let _ = std::fs::remove_file(Path::new(path).with_extension("txt"));
        }
    }
    files += sweep_old_clip_files(clips_dir, before);
    if !removed.is_empty() || files > 0 {
        info!(events = removed.len(), files, %cutoff, "deleted clips and events older than retain window");
    }
}

fn remove_file(path: &Option<String>) -> bool {
    let Some(path) = path else {
        return false;
    };
    std::fs::remove_file(path).is_ok()
}

fn sweep_old_clip_files(clips_dir: &Path, before: DateTime<Utc>) -> usize {
    let Ok(rd) = std::fs::read_dir(clips_dir) else {
        return 0;
    };
    let mut n = 0;
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(meta) = path.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if DateTime::<Utc>::from(modified) < before && std::fs::remove_file(&path).is_ok() {
            n += 1;
        }
    }
    n
}

fn local_midnight_utc(day: NaiveDate) -> DateTime<Utc> {
    let naive = day.and_hms_opt(0, 0, 0).expect("midnight");
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&naive))
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
        assert!(db.list_segments(Some("cam1"), None, None, 10, 0).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_fourth_day_deletes_the_oldest_of_three() {
        let root = std::env::temp_dir().join(format!("vigil-rec-{}", uuid::Uuid::new_v4()));
        let cam = root.join("cam1");
        for day in ["2026-10-18", "2026-10-19", "2026-10-20", "2026-10-21"] {
            std::fs::create_dir_all(cam.join(day)).unwrap();
        }
        let db = Db::open(root.join("t.db")).unwrap();

        prune_expired_days_on(
            &cam,
            3,
            &db,
            "cam1",
            NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(),
        );

        assert!(!cam.join("2026-10-18").exists());
        assert!(cam.join("2026-10-19").exists());
        assert!(cam.join("2026-10-21").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_fourth_day_deletes_that_days_clips_and_events() {
        use std::time::SystemTime;
        use vigil_common::Event;

        let root = std::env::temp_dir().join(format!("vigil-clips-{}", uuid::Uuid::new_v4()));
        let clips = root.join("clips");
        std::fs::create_dir_all(&clips).unwrap();
        let db = Db::open(root.join("t.db")).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        let old_at = Local
            .with_ymd_and_hms(2026, 10, 18, 12, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let keep_at = Local
            .with_ymd_and_hms(2026, 10, 19, 12, 0, 0)
            .unwrap()
            .with_timezone(&Utc);

        let old_id = uuid::Uuid::new_v4();
        let keep_id = uuid::Uuid::new_v4();
        let old_clip = clips.join(format!("{old_id}.mp4"));
        let old_txt = clips.join(format!("{old_id}.txt"));
        let keep_clip = clips.join(format!("{keep_id}.mp4"));
        let old_snap = root.join("old.jpg");
        std::fs::write(&old_clip, b"old").unwrap();
        std::fs::write(&old_txt, b"list").unwrap();
        std::fs::write(&keep_clip, b"keep").unwrap();
        std::fs::write(&old_snap, b"jpg").unwrap();
        let orphan = clips.join("orphan.mp4");
        std::fs::write(&orphan, b"orphan").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&orphan)
            .unwrap()
            .set_modified(SystemTime::from(old_at))
            .unwrap();

        let event = |id, start: DateTime<Utc>, clip: &Path, snap: Option<&Path>| Event {
            id,
            camera: "cam1".into(),
            label: "dog".into(),
            score: 0.8,
            start_time: start,
            end_time: Some(start),
            snapshot_path: snap.map(|p| p.to_string_lossy().to_string()),
            clip_path: Some(clip.to_string_lossy().to_string()),
            note: None,
        };
        db.insert_event(&event(old_id, old_at, &old_clip, Some(&old_snap)))
            .unwrap();
        db.insert_event(&event(keep_id, keep_at, &keep_clip, None))
            .unwrap();

        prune_expired_clips_and_events(&clips, 3, &db, today);

        assert!(!old_clip.exists());
        assert!(!old_txt.exists());
        assert!(!old_snap.exists());
        assert!(!orphan.exists());
        assert!(keep_clip.exists());
        assert!(db.get_event(&old_id.to_string()).unwrap().is_none());
        assert!(db.get_event(&keep_id.to_string()).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&root);
    }
}
