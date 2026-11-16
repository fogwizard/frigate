use crate::db::Db;
use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone, Utc};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};
use vigil_common::Config;

/// Poll recording directory and update SQLite until cancelled.
pub async fn segment_tracker_loop(cfg: Arc<Config>, db: Arc<Db>, camera: String) {
    let interval = Duration::from_secs(5);
    let camera_root = cfg.recordings_dir().join(&camera);
    loop {
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

/// Once a minute, delete the oldest timestamp-named recordings while `data/` is over budget.
pub fn enforce_data_budget(cfg: &Config, db: &Db) -> anyhow::Result<()> {
    let limit = cfg.record.max_gb.saturating_mul(1024 * 1024 * 1024);
    prune_videos_to(&cfg.paths.data_dir, &cfg.recordings_dir(), limit, db);
    Ok(())
}

/// `recordings` holds `camera/YYYY-MM-DD/HH-MM-SS.mp4`. The newest file of each
/// camera is the segment ffmpeg is writing, so it stays even when the directory
/// is still over `limit`. Events older than a camera's oldest kept recording go
/// with the files that were removed.
fn prune_videos_to(data_dir: &Path, recordings: &Path, limit: u64, db: &Db) {
    let mut used = dir_size(data_dir);
    if used <= limit {
        return;
    }
    let videos = recording_videos(recordings);
    let keep = newest_per_camera(recordings, &videos);
    let mut deleted = 0usize;
    let mut removed: HashSet<PathBuf> = HashSet::new();
    let mut touched: HashSet<String> = HashSet::new();
    for (when, path) in &videos {
        if used <= limit || keep.contains(path) {
            continue;
        }
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        match std::fs::remove_file(path) {
            Ok(()) => {
                if let Err(e) = db.delete_segment_at(path) {
                    warn!(path = %path.display(), error = %e, "delete segment row");
                }
                if let Some(parent) = path.parent() {
                    let _ = std::fs::remove_dir(parent);
                }
                if let Some(camera) = camera_name(recordings, path) {
                    touched.insert(camera);
                }
                removed.insert(path.clone());
                deleted += 1;
                used = used.saturating_sub(len);
                info!(path = %path.display(), bytes = len, %when, "deleted oldest recording");
            }
            Err(e) => warn!(path = %path.display(), error = %e, "delete oldest recording"),
        }
    }
    let mut events = 0usize;
    for camera in &touched {
        let Some(cutoff) = videos
            .iter()
            .filter(|(_, path)| {
                camera_name(recordings, path).as_deref() == Some(camera.as_str()) && !removed.contains(path)
            })
            .map(|(when, _)| *when)
            .min()
        else {
            continue;
        };
        let (n, bytes) = remove_events_before(db, camera, cutoff);
        events += n;
        used = used.saturating_sub(bytes);
    }
    if deleted > 0 {
        info!(deleted, events, after = used, limit, "data directory trimmed to budget");
    } else if used > limit {
        warn!(used, limit, "data directory over budget and every remaining recording is still being written");
    }
}

fn remove_events_before(db: &Db, camera: &str, before: DateTime<Utc>) -> (usize, u64) {
    let removed = match db.delete_old_events(Some(camera), before) {
        Ok(rows) => rows,
        Err(e) => {
            warn!(%camera, error = %e, "delete events older than kept recordings");
            return (0, 0);
        }
    };
    let n = removed.len();
    let mut bytes = 0u64;
    for (snap, clip) in &removed {
        if let Some(path) = snap {
            bytes = bytes.saturating_add(file_len(Path::new(path)));
            let _ = std::fs::remove_file(path);
        }
        if let Some(path) = clip {
            let clip_path = Path::new(path);
            bytes = bytes.saturating_add(file_len(clip_path));
            let _ = std::fs::remove_file(clip_path);
            let note = clip_path.with_extension("txt");
            bytes = bytes.saturating_add(file_len(&note));
            let _ = std::fs::remove_file(note);
        }
    }
    (n, bytes)
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn dir_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

fn recording_videos(recordings: &Path) -> Vec<(DateTime<Utc>, PathBuf)> {
    let mut out = Vec::new();
    let Ok(cams) = std::fs::read_dir(recordings) else {
        return out;
    };
    for cam in cams.flatten() {
        if !cam.path().is_dir() {
            continue;
        }
        let Ok(days) = std::fs::read_dir(cam.path()) else {
            continue;
        };
        for day in days.flatten() {
            if !day.path().is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(day.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(|e| e.to_str()) != Some("mp4") {
                    continue;
                }
                let Some(when) = recording_time(&path) else {
                    continue;
                };
                out.push((when, path));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    out
}

fn newest_per_camera(recordings: &Path, videos: &[(DateTime<Utc>, PathBuf)]) -> HashSet<PathBuf> {
    let mut keep: HashMap<String, (DateTime<Utc>, PathBuf)> = HashMap::new();
    for (when, path) in videos {
        let Some(camera) = camera_name(recordings, path) else {
            continue;
        };
        match keep.get(&camera) {
            Some((kept_at, kept_path)) if (*when, path) <= (*kept_at, kept_path) => {}
            _ => {
                keep.insert(camera, (*when, path.clone()));
            }
        }
    }
    keep.into_values().map(|(_, path)| path).collect()
}

fn camera_name(recordings: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(recordings).ok()?;
    rel.components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn video(dir: &Path, bytes: usize) {
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::fs::write(dir, vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn over_budget_deletes_the_oldest_named_recording() {
        use vigil_common::Event;

        let root = std::env::temp_dir().join(format!("vigil-budget-{}", uuid::Uuid::new_v4()));
        let data = root.join("data");
        let recordings = data.join("recordings");
        let oldest = recordings.join("cam1").join("2026-10-18").join("12-00-00.mp4");
        let newest = recordings.join("cam1").join("2026-10-19").join("12-00-00.mp4");
        let only = recordings.join("cam2").join("2026-10-17").join("08-00-00.mp4");
        video(&oldest, 4000);
        video(&newest, 4000);
        video(&only, 4000);
        let db = Db::open(data.join("vigil.db")).unwrap();
        db.insert_segment("cam1", &oldest.to_string_lossy(), Utc::now())
            .unwrap();

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
        let old_clip = data.join("clips").join(format!("{old_id}.mp4"));
        let old_txt = data.join("clips").join(format!("{old_id}.txt"));
        let keep_clip = data.join("clips").join(format!("{keep_id}.mp4"));
        video(&old_clip, 100);
        video(&old_txt, 10);
        video(&keep_clip, 100);
        let event = |id, start: DateTime<Utc>, clip: &Path| Event {
            id,
            camera: "cam1".into(),
            label: "dog".into(),
            score: 0.8,
            start_time: start,
            end_time: Some(start),
            snapshot_path: None,
            clip_path: Some(clip.to_string_lossy().to_string()),
            note: None,
        };
        db.insert_event(&event(old_id, old_at, &old_clip)).unwrap();
        db.insert_event(&event(keep_id, keep_at, &keep_clip)).unwrap();

        prune_videos_to(&data, &recordings, 1, &db);

        assert!(!oldest.exists());
        assert!(!recordings.join("cam1").join("2026-10-18").exists());
        assert!(newest.exists());
        assert!(only.exists(), "the only file of a camera is the one being written");
        assert!(db.list_segments(Some("cam1"), None, None, 10, 0).unwrap().is_empty());
        assert!(!old_clip.exists());
        assert!(!old_txt.exists());
        assert!(keep_clip.exists());
        assert!(db.get_event(&old_id.to_string()).unwrap().is_none());
        assert!(db.get_event(&keep_id.to_string()).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn under_budget_keeps_every_recording() {
        let root = std::env::temp_dir().join(format!("vigil-budget-{}", uuid::Uuid::new_v4()));
        let data = root.join("data");
        let recordings = data.join("recordings");
        let file = recordings.join("cam1").join("2026-10-18").join("12-00-00.mp4");
        video(&file, 4000);
        let db = Db::open(data.join("vigil.db")).unwrap();

        prune_videos_to(&data, &recordings, 100 * 1024 * 1024 * 1024, &db);

        assert!(file.exists());
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
