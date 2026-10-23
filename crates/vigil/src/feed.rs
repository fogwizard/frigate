//! Daily feed decided inside each camera's inference loop.
//!
//! UTC 07:00–08:00 opens the feeder once for `open_secs`, then closes it.
//! The clip of that interval is sent to a Qwen vision model, which decides
//! whether food came out. After 08:00 this UTC day does not try again.

use crate::db::Db;
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::Command;
use tracing::{info, warn};
use uuid::Uuid;
use vigil_common::{Config, Event};

const WINDOW_HOUR: u32 = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    None,
    Open,
    /// Feeder was opened; close it and record the result.
    Close { start: DateTime<Utc> },
    /// The window ended before an open succeeded.
    GiveUp,
}

#[derive(Clone, Copy)]
enum Phase {
    Wait,
    Open { start: DateTime<Utc> },
    Done,
}

struct Cam {
    day: NaiveDate,
    phase: Phase,
    tried_open: bool,
    retry_at: DateTime<Utc>,
}

#[derive(Default)]
pub struct CameraFeeds {
    cameras: HashMap<String, Cam>,
}

impl CameraFeeds {
    /// `already_fed` is true when this camera already has a feed event today.
    /// `open_secs` is how long the feeder stays on. The picture is not inspected here.
    pub fn step(&mut self, camera: &str, now: DateTime<Utc>, already_fed: bool, open_secs: u64) -> Step {
        if open_secs == 0 {
            return Step::None;
        }
        let day = now.date_naive();
        let cam = self.cameras.entry(camera.to_string()).or_insert(Cam {
            day,
            phase: Phase::Wait,
            tried_open: false,
            retry_at: DateTime::UNIX_EPOCH,
        });
        if cam.day != day {
            let prev = std::mem::replace(&mut cam.phase, Phase::Wait);
            cam.day = day;
            cam.tried_open = false;
            cam.retry_at = DateTime::UNIX_EPOCH;
            if let Phase::Open { start } = prev {
                return Step::Close { start };
            }
        }

        match cam.phase {
            Phase::Done => Step::None,
            Phase::Wait => {
                if already_fed || now.hour() > WINDOW_HOUR {
                    cam.phase = Phase::Done;
                    return if cam.tried_open { Step::GiveUp } else { Step::None };
                }
                if now.hour() != WINDOW_HOUR || now < cam.retry_at {
                    return Step::None;
                }
                cam.phase = Phase::Open { start: now };
                Step::Open
            }
            Phase::Open { start } => {
                let elapsed = now.signed_duration_since(start).num_seconds();
                if now.hour() != WINDOW_HOUR || elapsed >= open_secs as i64 {
                    cam.phase = Phase::Done;
                    Step::Close { start }
                } else {
                    Step::None
                }
            }
        }
    }

    /// Open write failed. Try again later inside the same hour.
    pub fn fail_open(&mut self, camera: &str, now: DateTime<Utc>) {
        let Some(cam) = self.cameras.get_mut(camera) else {
            return;
        };
        if matches!(cam.phase, Phase::Open { .. }) {
            cam.phase = Phase::Wait;
            cam.tried_open = true;
            cam.retry_at = now + chrono::Duration::seconds(30);
        }
    }
}

pub fn note_from_vlm(text: &str) -> (String, bool) {
    let text = text.trim();
    let first = text.lines().next().unwrap_or("").trim();
    if first.starts_with("成功") {
        (text.to_string(), true)
    } else if first.starts_with("失败") {
        (text.to_string(), false)
    } else {
        (format!("失败：{text}"), false)
    }
}

pub fn save_event(
    db: &Db,
    cfg: &Config,
    camera: &str,
    start: DateTime<Utc>,
    note: &str,
    ok: bool,
) -> Option<(Uuid, DateTime<Utc>, f32)> {
    let id = Uuid::new_v4();
    let end = Utc::now();
    let score = if ok { 1.0 } else { 0.0 };
    let event = Event {
        id,
        camera: camera.to_string(),
        label: "feed".into(),
        score,
        start_time: start,
        end_time: Some(end),
        snapshot_path: snapshot(cfg, camera, id),
        clip_path: None,
        note: Some(note.to_string()),
    };
    if let Err(e) = db.insert_event(&event) {
        warn!(error = %e, "insert feed event");
        return None;
    }
    Some((id, end, score))
}

/// Cut the feed clip, ask Qwen whether food came out, and write that note on the event.
/// The detect loop must not wait on this.
pub async fn review_feed(
    db: Arc<Db>,
    cfg: Arc<Config>,
    camera: String,
    id: Uuid,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    close_error: Option<String>,
) {
    let clip = wait_for_clip(&cfg, &camera, id, start, end).await;
    if let Some(path) = &clip {
        if let Err(e) = db.update_event_end(id, end, Some(path), 0.0) {
            warn!(error = %e, "save feed clip path");
        } else {
            info!(camera = %camera, %path, "feed clip saved");
        }
    }
    let (mut note, ok) = match &clip {
        None => ("失败：没有喂食视频".to_string(), false),
        Some(path) => match judge_clip(&cfg, path).await {
            Ok(text) => note_from_vlm(&text),
            Err(e) => (format!("失败：视觉分析失败：{e}"), false),
        },
    };
    if let Some(err) = close_error {
        note.push_str(&format!("；关闭喂食器失败：{err}"));
    }
    let score = if ok { 1.0 } else { 0.0 };
    if let Err(e) = db.update_event_note(id, &note, score) {
        warn!(error = %e, "save feed note");
    } else {
        info!(camera = %camera, %note, "feed review recorded");
    }
}

async fn wait_for_clip(cfg: &Config, camera: &str, id: Uuid, start: DateTime<Utc>, end: DateTime<Utc>) -> Option<String> {
    let wait = std::time::Duration::from_secs(u64::from(cfg.record.segment_secs) * 2 + 15);
    let deadline = tokio::time::Instant::now() + wait;
    let root = cfg.recordings_dir().join(camera);
    loop {
        let files = recording_files(&root);
        if let ClipFiles::Ready(needed) = clip_files(&files, start, end, cfg.record.segment_secs as i64) {
            if let Some(path) = cut_clip(cfg, id, &needed, start, end).await {
                return Some(path);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            warn!(camera = %camera, "feed clip could not be cut from the recording");
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

const VLM_PROMPT: &str = "这是自动喂食器打开期间的监控录像。请只根据画面判断有没有粮食从喂食器里出来。第一行只能写“成功”或“失败”。第二行用一句中文说明看到了什么。";

fn vlm_api_key(cfg: &Config) -> String {
    if !cfg.feed.vlm.api_key.trim().is_empty() {
        return cfg.feed.vlm.api_key.clone();
    }
    let base = cfg.feed.vlm.base_url.to_ascii_lowercase();
    if base.contains("127.0.0.1") || base.contains("localhost") {
        return String::new();
    }
    std::env::var("DASHSCOPE_API_KEY").unwrap_or_default()
}

async fn judge_clip(cfg: &Config, path: &str) -> Result<String, String> {
    let key = vlm_api_key(cfg);
    let local = {
        let base = cfg.feed.vlm.base_url.to_ascii_lowercase();
        base.contains("127.0.0.1") || base.contains("localhost")
    };
    if key.trim().is_empty() && !local {
        return Err("未配置 Qwen API Key".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("喂食视频过大".into());
    }
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let video = format!("data:video/mp4;base64,{b64}");
    let root = cfg.feed.vlm.base_url.trim_end_matches('/');
    let endpoint = format!("{root}/chat/completions");
    let body = serde_json::json!({
        "model": cfg.feed.vlm.model,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "video_url", "video_url": {"url": video}},
                {"type": "text", "text": VLM_PROMPT}
            ]
        }]
    });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.post(endpoint);
    if !key.trim().is_empty() {
        req = req.bearer_auth(key);
    }
    let resp = req.json(&body).send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("Qwen 返回 {status}：{}", brief(&text)));
    }
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    message_text(&value).ok_or_else(|| format!("Qwen 没有返回文字：{}", brief(&text)))
}

fn message_text(value: &serde_json::Value) -> Option<String> {
    let content = &value["choices"][0]["message"]["content"];
    if let Some(text) = content.as_str() {
        let text = text.trim();
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }
    let parts = content.as_array()?;
    let text = parts
        .iter()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn brief(text: &str) -> String {
    let mut out: String = text.chars().take(300).collect();
    if text.chars().count() > 300 {
        out.push('…');
    }
    out
}

enum ClipFiles {
    Wait,
    Ready(Vec<(DateTime<Utc>, PathBuf)>),
}

fn clip_files(
    files: &[(DateTime<Utc>, PathBuf)],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    segment_secs: i64,
) -> ClipFiles {
    if files.is_empty() {
        return ClipFiles::Wait;
    }
    let span = chrono::Duration::seconds(segment_secs.max(1));
    let mut needed = Vec::new();
    for i in 0..files.len() {
        let t = files[i].0;
        // The newest file is still growing, so it covers the feed even when
        // that feed runs longer than one configured segment.
        let next = files.get(i + 1).map(|f| f.0).unwrap_or(end + span);
        if t < end && next > start {
            needed.push(files[i].clone());
        }
    }
    if needed.is_empty() {
        ClipFiles::Wait
    } else {
        ClipFiles::Ready(needed)
    }
}

fn recording_files(root: &Path) -> Vec<(DateTime<Utc>, PathBuf)> {
    let mut out = Vec::new();
    let Ok(days) = std::fs::read_dir(root) else {
        return out;
    };
    for day in days.flatten() {
        let day_path = day.path();
        if !day_path.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&day_path) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("mp4") {
                continue;
            }
            if let Some(start) = segment_start(&path) {
                out.push((start, path));
            }
        }
    }
    out.sort_by_key(|(t, _)| *t);
    out
}

fn segment_start(path: &Path) -> Option<DateTime<Utc>> {
    let day = path.parent()?.file_name()?.to_str()?;
    let clock = path.file_stem()?.to_str()?;
    let naive = NaiveDateTime::parse_from_str(&format!("{day} {clock}"), "%Y-%m-%d %H-%M-%S").ok()?;
    // ffmpeg -strftime writes the machine's local clock, while events are UTC.
    chrono::Local
        .from_local_datetime(&naive)
        .single()
        .map(|t| t.with_timezone(&Utc))
}

async fn cut_clip(
    cfg: &Config,
    id: Uuid,
    files: &[(DateTime<Utc>, PathBuf)],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Option<String> {
    let first = files.first()?.0;
    let dir = cfg.clips_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let out = dir.join(format!("{id}.mp4"));
    let offset = (start - first).num_milliseconds().max(0) as f64 / 1000.0;
    let duration = (end - start).num_milliseconds().max(1000) as f64 / 1000.0;
    let source = if files.len() == 1 {
        files[0].1.clone()
    } else {
        let list = dir.join(format!("{id}.txt"));
        let body = files
            .iter()
            .map(|(_, p)| format!("file '{}'", p.to_string_lossy().replace('\\', "/")))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&list, body).ok()?;
        let joined = dir.join(format!("{id}.src.mp4"));
        let ok = ffmpeg(
            &cfg.ffmpeg.binary,
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "concat",
                "-safe",
                "0",
                "-i",
                &list.to_string_lossy(),
                "-c",
                "copy",
                "-y",
                &joined.to_string_lossy(),
            ],
        )
        .await;
        let _ = std::fs::remove_file(&list);
        if !ok {
            let _ = std::fs::remove_file(&joined);
            return None;
        }
        joined
    };
    let offset_s = format!("{offset:.3}");
    let duration_s = format!("{duration:.3}");
    let ok = ffmpeg(
        &cfg.ffmpeg.binary,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            &offset_s,
            "-i",
            &source.to_string_lossy(),
            "-t",
            &duration_s,
            "-c",
            "copy",
            "-y",
            &out.to_string_lossy(),
        ],
    )
    .await;
    if files.len() > 1 {
        let _ = std::fs::remove_file(&source);
    }
    let long_enough = out
        .metadata()
        .map(|m| m.len() > 4096)
        .unwrap_or(false);
    if ok && long_enough {
        Some(out.to_string_lossy().to_string())
    } else {
        let _ = std::fs::remove_file(&out);
        None
    }
}

async fn ffmpeg(bin: &str, args: &[&str]) -> bool {
    match Command::new(bin)
        .args(args)
        .output()
        .await
    {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            tracing::debug!(stderr = %String::from_utf8_lossy(&out.stderr), "feed clip ffmpeg failed");
            false
        }
        Err(e) => {
            warn!(error = %e, "feed clip ffmpeg failed");
            false
        }
    }
}

fn snapshot(cfg: &Config, camera: &str, id: Uuid) -> Option<String> {
    let src = cfg.live_snapshots_dir().join(format!("{camera}.jpg"));
    let bytes = std::fs::read(src).ok()?;
    let dir = cfg.snapshots_dir();
    let _ = std::fs::create_dir_all(&dir);
    let dest = dir.join(format!("{id}.jpg"));
    std::fs::write(&dest, bytes).ok()?;
    Some(dest.to_string_lossy().to_string())
}

fn at(hour: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 23, hour, min, 0).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_once_during_the_utc_hour_and_stops_after_it() {
        let mut feeds = CameraFeeds::default();
        assert_eq!(feeds.step("cam1", at(6, 59), false, 3600), Step::None);
        assert_eq!(feeds.step("cam1", at(7, 0), false, 3600), Step::Open);
        assert_eq!(feeds.step("cam1", at(7, 10), false, 3600), Step::None);
        assert_eq!(
            feeds.step("cam1", at(8, 0), false, 3600),
            Step::Close { start: at(7, 0) }
        );
        assert_eq!(feeds.step("cam1", at(8, 10), false, 3600), Step::None);
    }

    #[test]
    fn feeder_closes_after_open_secs_without_looking_at_the_picture() {
        let mut feeds = CameraFeeds::default();
        let start = Utc.with_ymd_and_hms(2026, 10, 23, 7, 0, 0).unwrap();
        assert_eq!(feeds.step("cam1", start, false, 8), Step::Open);
        let early = Utc.with_ymd_and_hms(2026, 10, 23, 7, 0, 7).unwrap();
        assert_eq!(feeds.step("cam1", early, false, 8), Step::None);
        let due = Utc.with_ymd_and_hms(2026, 10, 23, 7, 0, 8).unwrap();
        assert_eq!(feeds.step("cam1", due, false, 8), Step::Close { start });
        let later = Utc.with_ymd_and_hms(2026, 10, 23, 7, 1, 0).unwrap();
        assert_eq!(feeds.step("cam1", later, false, 8), Step::None);
    }

    #[test]
    fn zero_feed_time_does_not_open_or_close() {
        let mut feeds = CameraFeeds::default();
        assert_eq!(feeds.step("cam1", at(7, 0), false, 0), Step::None);
        assert_eq!(feeds.step("cam1", at(7, 30), false, 0), Step::None);
        assert_eq!(feeds.step("cam1", at(8, 0), false, 0), Step::None);
    }

    #[test]
    fn vlm_reply_is_success_only_when_the_first_line_says_so() {
        assert_eq!(note_from_vlm("成功\n粮食从出料口落下").1, true);
        assert_eq!(note_from_vlm("失败\n食盆是空的").1, false);
        assert!(note_from_vlm("看不清").0.starts_with("失败"));
    }

    #[test]
    fn an_existing_feed_event_blocks_another_attempt() {
        let mut feeds = CameraFeeds::default();
        assert_eq!(feeds.step("cam1", at(7, 30), true, 8), Step::None);
        assert_eq!(feeds.step("cam1", at(7, 40), true, 8), Step::None);
    }

    #[test]
    fn starting_after_the_hour_does_not_try() {
        let mut feeds = CameraFeeds::default();
        assert_eq!(feeds.step("cam1", at(9, 0), false, 8), Step::None);
        assert_eq!(feeds.step("cam1", at(7, 0), false, 8), Step::None);
    }

    #[test]
    fn the_segment_being_written_can_be_cut_without_a_later_file() {
        let t = |h, m, s| Utc.with_ymd_and_hms(2026, 10, 23, h, m, s).unwrap();
        let open = vec![(t(7, 44, 0), PathBuf::from("a.mp4"))];
        let start = t(7, 44, 30);
        let end = t(7, 44, 41);
        match clip_files(&open, start, end, 60) {
            ClipFiles::Ready(got) => assert_eq!(got.len(), 1),
            ClipFiles::Wait => panic!("the open segment should be usable"),
        }
        match clip_files(&open, t(7, 45, 10), t(7, 45, 30), 60) {
            ClipFiles::Ready(got) => assert_eq!(got.len(), 1),
            ClipFiles::Wait => panic!("a long open segment should still cover the feed"),
        }
        let closed = vec![
            (t(7, 44, 0), PathBuf::from("a.mp4")),
            (t(7, 45, 0), PathBuf::from("b.mp4")),
        ];
        match clip_files(&closed, start, end, 60) {
            ClipFiles::Ready(got) => assert_eq!(got.len(), 1),
            ClipFiles::Wait => panic!("closed segment should be ready"),
        }
    }

    #[test]
    fn the_next_utc_day_can_feed_again() {
        let mut feeds = CameraFeeds::default();
        assert_eq!(feeds.step("cam1", at(7, 0), false, 3600), Step::Open);
        let _ = feeds.step("cam1", at(8, 0), false, 3600);
        let next = Utc.with_ymd_and_hms(2026, 10, 24, 7, 0, 0).unwrap();
        assert_eq!(feeds.step("cam1", next, false, 3600), Step::Open);
    }
}
