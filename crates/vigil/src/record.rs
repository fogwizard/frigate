use crate::db::Db;
use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone, Utc};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};
use vigil_common::Config;

/// Poll recording directory and update SQLite until cancelled.
pub async fn segment_tracker_loop(cfg: Arc<Config>, db: Arc<Db>, camera: String) {
    let interval = Duration::from_secs(5);
    let camera_root = cfg.recordings_dir().join(&camera);
    let mut last_day: Option<NaiveDate> = None;
    // Existing backlog is trimmed as soon as this camera's tracker starts.
    // A later local day trims again.
    loop {
        let today = Local::now().date_naive();
        if last_day != Some(today) {
            prune_camera(&cfg, &db, &camera);
            last_day = Some(today);
        }
        if let Err(e) = index_recordings(&db, &camera, &camera_root) {
            warn!(camera = %camera, error = %e, "index recordings");
        }
        sleep(interval).await;
    }
}

/// Recording files whose filename time overlaps `[start, end]`.
#[cfg(test)]
fn recordings_covering(camera_root: &Path, start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<PathBuf> {
    let mut files = recording_files(camera_root, start, end);
    files.sort_by_key(|(t, _)| *t);
    let mut out = Vec::new();
    for i in 0..files.len() {
        let file_start = files[i].0;
        let file_end = files.get(i + 1).map(|(t, _)| *t).unwrap_or_else(Utc::now);
        if file_start <= end && file_end >= start {
            out.push(files[i].1.clone());
        }
    }
    out
}

fn index_recordings(db: &Db, camera: &str, camera_root: &Path) -> anyhow::Result<()> {
    let today = Local::now().date_naive();
    let mut days = vec![today];
    if let Some(yesterday) = today.pred_opt() {
        days.push(yesterday);
    }
    let known = db.segment_paths(camera)?;
    let mut found = Vec::new();
    for day in days {
        let dir = camera_root.join(day.format("%Y-%m-%d").to_string());
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("mp4") || !usable_recording(&path) {
                continue;
            }
            let Some(start) = recording_time(&path) else {
                continue;
            };
            let rel = path.to_string_lossy().to_string();
            if !known.contains(&rel.replace('\\', "/")) {
                db.insert_segment(camera, &rel, start)?;
            }
            found.push((start, rel));
        }
    }
    found.sort_by_key(|(start, _)| *start);
    for pair in found.windows(2) {
        db.finish_segment_at_path(&pair[0].1, pair[1].0)?;
    }
    Ok(())
}

#[cfg(test)]
fn recording_files(camera_root: &Path, start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<(DateTime<Utc>, PathBuf)> {
    let mut out = Vec::new();
    for day in day_span(start, end) {
        let dir = camera_root.join(day.format("%Y-%m-%d").to_string());
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("mp4") || !usable_recording(&path) {
                continue;
            }
            let Some(t) = recording_time(&path) else {
                continue;
            };
            out.push((t, path));
        }
    }
    out
}

#[cfg(test)]
fn day_span(start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<NaiveDate> {
    let mut day = start.with_timezone(&Local).date_naive();
    let last = end.with_timezone(&Local).date_naive();
    let mut out = Vec::new();
    loop {
        out.push(day);
        if day >= last {
            break;
        }
        let Some(next) = day.succ_opt() else {
            break;
        };
        day = next;
    }
    out
}

/// `.../YYYY-MM-DD/HH-MM-SS.mp4` is the machine-local time FFmpeg wrote.
fn recording_time(path: &Path) -> Option<DateTime<Utc>> {
    let stem = path.file_stem()?.to_str()?;
    let day = path.parent()?.file_name()?.to_str()?;
    let date = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    let time = NaiveTime::parse_from_str(stem, "%H-%M-%S").ok()?;
    Local
        .from_local_datetime(&date.and_time(time))
        .earliest()
        .map(|t| t.with_timezone(&Utc))
}

fn usable_recording(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.len() >= 1024)
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

    #[test]
    fn a_middle_recording_covers_the_event() {
        let root = std::env::temp_dir().join(format!("vigil-cover-{}", uuid::Uuid::new_v4()));
        let day = root.join("cam1").join("2026-11-03");
        std::fs::create_dir_all(&day).unwrap();
        for name in ["20-50-04.mp4", "20-53-22.mp4", "20-57-54.mp4"] {
            std::fs::write(day.join(name), vec![0u8; 2048]).unwrap();
        }
        let start = Local
            .with_ymd_and_hms(2026, 11, 3, 20, 53, 48)
            .unwrap()
            .with_timezone(&Utc);
        let end = Local
            .with_ymd_and_hms(2026, 11, 3, 20, 53, 59)
            .unwrap()
            .with_timezone(&Utc);
        let paths = recordings_covering(&root.join("cam1"), start, end);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with("20-53-22.mp4"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
