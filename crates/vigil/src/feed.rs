//! Daily feed for each camera.
//!
//! Local 07:00–08:00 opens the feeder once for that camera's `feed_time`, then
//! closes it. The clip of that interval is sent to a Qwen vision model, which
//! decides whether food came out. After 08:00 local time that day does not try again.
//! Closing is retried until the write succeeds, including after a restart.

use crate::db::Db;
use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use tokio::process::Command;
use tracing::{info, warn};
use uuid::Uuid;
use vigil_common::{Config, Event};

const WINDOW_HOUR: u32 = 7;

static OPEN_FAILURES: AtomicU32 = AtomicU32::new(0);
static FAILURE_DAY: Mutex<i32> = Mutex::new(0);

/// Hour of the machine's local clock. The feed window is 07:00–08:00 there.
pub fn local_hour(now: DateTime<Utc>) -> u32 {
    now.with_timezone(&Local).hour()
}

/// During 07:00–08:00 the header shows the retry count until every feeder's note
/// starts with 成功. Before 07:00, after 08:00, and once they have all succeeded,
/// the countdown is shown again. An empty list means no feeder is configured.
pub fn show_feeding_message(hour: u32, succeeded: &[bool]) -> bool {
    hour == WINDOW_HOUR && succeeded.iter().any(|ok| !*ok)
}

/// Seconds until the next local 07:00. At 07:00 the next one is tomorrow.
pub fn secs_until_next_feed(now: DateTime<Utc>) -> u64 {
    let local = now.with_timezone(&Local);
    let today = local.date_naive();
    let opening = today
        .and_hms_opt(WINDOW_HOUR, 0, 0)
        .expect("07:00");
    let target_naive = if local.naive_local() < opening {
        opening
    } else {
        today
            .succ_opt()
            .expect("next day")
            .and_hms_opt(WINDOW_HOUR, 0, 0)
            .expect("07:00")
    };
    let target = Local
        .from_local_datetime(&target_naive)
        .earliest()
        .unwrap_or_else(|| Utc.from_utc_datetime(&target_naive).with_timezone(&Local));
    (target.with_timezone(&Utc) - now).num_seconds().max(0) as u64
}

/// Failed open writes today. The count resets when the local date changes.
pub fn record_open_failure(now: DateTime<Utc>) {
    let day = local_date(now).num_days_from_ce();
    let mut slot = FAILURE_DAY.lock().unwrap_or_else(|err| err.into_inner());
    if *slot != day {
        *slot = day;
        OPEN_FAILURES.store(1, Ordering::SeqCst);
    } else {
        OPEN_FAILURES.fetch_add(1, Ordering::SeqCst);
    }
}

pub fn open_failure_count(now: DateTime<Utc>) -> u32 {
    let day = local_date(now).num_days_from_ce();
    let slot = FAILURE_DAY.lock().unwrap_or_else(|err| err.into_inner());
    if *slot != day {
        0
    } else {
        OPEN_FAILURES.load(Ordering::SeqCst)
    }
}

/// True while the 07:00–08:00 attempt is still short of a 成功 note on any feeder.
pub fn feeding_in_progress(cfg: &Config, db: &Db, now: DateTime<Utc>) -> bool {
    let day = local_date(now);
    let succeeded: Vec<bool> = cfg
        .cameras
        .iter()
        .filter(|(_, cam)| cam.feed_time > 0)
        .map(|(camera, _)| {
            db.latest_feed_event(camera, day)
                .ok()
                .flatten()
                .and_then(|ev| ev.note)
                .is_some_and(|note| note.starts_with("成功"))
        })
        .collect();
    show_feeding_message(local_hour(now), &succeeded)
}

/// Calendar date of the machine's local clock. The UTC date changes at 08:00
/// in UTC+8, which is still the same local morning.
pub fn local_date(now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&Local).date_naive()
}
pub const NOTE_FEEDING: &str = "喂食中";
pub const NOTE_REVIEWING: &str = "分析中";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    None,
    Open,
    /// Close the feeder. `write` is false once a close has already succeeded.
    Close {
        start: DateTime<Utc>,
        write: bool,
        event_id: Option<Uuid>,
    },
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
    event_id: Option<Uuid>,
    close_ok: bool,
    reviewed: bool,
    recorded: bool,
}

impl Cam {
    fn fresh(day: NaiveDate) -> Self {
        Self {
            day,
            phase: Phase::Wait,
            tried_open: false,
            retry_at: DateTime::UNIX_EPOCH,
            event_id: None,
            close_ok: false,
            reviewed: false,
            recorded: false,
        }
    }
}

#[derive(Default)]
pub struct CameraFeeds {
    cameras: HashMap<String, Cam>,
}

impl CameraFeeds {
    pub fn is_open(&self, camera: &str) -> bool {
        self.cameras
            .get(camera)
            .is_some_and(|cam| matches!(cam.phase, Phase::Open { .. }))
    }

    pub fn event_id(&self, camera: &str) -> Option<Uuid> {
        self.cameras.get(camera).and_then(|cam| cam.event_id)
    }

    pub fn set_event(&mut self, camera: &str, id: Uuid) {
        if let Some(cam) = self.cameras.get_mut(camera) {
            cam.event_id = Some(id);
        }
    }

    pub fn ack_close(&mut self, camera: &str) {
        let Some(cam) = self.cameras.get_mut(camera) else {
            return;
        };
        cam.close_ok = true;
        if cam.reviewed {
            cam.phase = Phase::Done;
        }
    }

    /// First call after a successful close schedules the clip review once.
    pub fn mark_reviewed(&mut self, camera: &str) -> bool {
        let Some(cam) = self.cameras.get_mut(camera) else {
            return false;
        };
        if cam.reviewed {
            return false;
        }
        cam.reviewed = true;
        if cam.close_ok {
            cam.phase = Phase::Done;
        }
        true
    }

    pub fn mark_give_up(&mut self, camera: &str) {
        if let Some(cam) = self.cameras.get_mut(camera) {
            cam.recorded = true;
        }
    }

    /// A restart found an unfinished feed event. Keep that open interval and close it
    /// when `feed_time` has elapsed instead of opening the feeder a second time.
    pub fn adopt(&mut self, camera: &str, start: DateTime<Utc>, id: Uuid) {
        let day = local_date(start);
        let cam = self
            .cameras
            .entry(camera.to_string())
            .or_insert_with(|| Cam::fresh(day));
        if !matches!(cam.phase, Phase::Wait) {
            return;
        }
        cam.day = day;
        cam.phase = Phase::Open { start };
        cam.event_id = Some(id);
        cam.close_ok = false;
        cam.reviewed = false;
        cam.recorded = true;
        cam.retry_at = DateTime::UNIX_EPOCH;
        cam.tried_open = true;
    }

    /// `already_fed` is true when this camera already has a finished feed event today.
    /// `open_secs` is how long the feeder stays on. Zero never starts a feed, but an
    /// interval that is already open still closes.
    pub fn step(&mut self, camera: &str, now: DateTime<Utc>, already_fed: bool, open_secs: u64) -> Step {
        if open_secs == 0 && !self.is_open(camera) {
            return Step::None;
        }
        let day = local_date(now);
        let cam = self
            .cameras
            .entry(camera.to_string())
            .or_insert_with(|| Cam::fresh(day));
        if cam.day != day && !matches!(cam.phase, Phase::Open { .. }) {
            *cam = Cam::fresh(day);
        }

        match cam.phase {
            Phase::Done => Step::None,
            Phase::Wait => {
                if open_secs == 0 || already_fed {
                    cam.phase = Phase::Done;
                    return Step::None;
                }
                let hour = local_hour(now);
                if hour > WINDOW_HOUR {
                    if cam.tried_open && !cam.recorded {
                        if now < cam.retry_at {
                            return Step::None;
                        }
                        cam.retry_at = now + chrono::Duration::seconds(30);
                        return Step::GiveUp;
                    }
                    cam.phase = Phase::Done;
                    return Step::None;
                }
                if hour != WINDOW_HOUR || now < cam.retry_at {
                    return Step::None;
                }
                cam.phase = Phase::Open { start: now };
                cam.retry_at = DateTime::UNIX_EPOCH;
                cam.close_ok = false;
                cam.reviewed = false;
                Step::Open
            }
            Phase::Open { start } => {
                let elapsed = now.signed_duration_since(start).num_seconds();
                let due = open_secs == 0 || local_hour(now) != WINDOW_HOUR || elapsed >= open_secs as i64;
                if !due {
                    return Step::None;
                }
                if cam.close_ok && cam.reviewed {
                    cam.phase = Phase::Done;
                    return Step::None;
                }
                if !cam.close_ok && now < cam.retry_at {
                    return Step::None;
                }
                if !cam.close_ok {
                    cam.retry_at = now + chrono::Duration::seconds(5);
                }
                Step::Close {
                    start,
                    write: !cam.close_ok,
                    event_id: cam.event_id,
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
            cam.event_id = None;
            cam.close_ok = false;
            cam.reviewed = false;
        }
    }
}

pub fn unfinished(note: &Option<String>) -> bool {
    matches!(note.as_deref(), Some(NOTE_FEEDING) | Some(NOTE_REVIEWING))
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

/// Claim today's feed before the feeder is energized, so a crash cannot open it twice.
pub fn claim_feed(db: &Db, cfg: &Config, camera: &str, start: DateTime<Utc>) -> Option<Uuid> {
    let id = Uuid::new_v4();
    let event = Event {
        id,
        camera: camera.to_string(),
        label: "feed".into(),
        score: 0.0,
        start_time: start,
        end_time: None,
        snapshot_path: snapshot(cfg, camera, id),
        clip_path: None,
        note: Some(NOTE_FEEDING.to_string()),
    };
    if let Err(e) = db.insert_event(&event) {
        warn!(error = %e, "claim feed event");
        return None;
    }
    Some(id)
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

pub enum ClipCut {
    /// The recording that contains `end` is still open.
    Wait,
    Done(String),
    Failed,
}

/// Cut `[start, end]` once every overlapping segment file has been closed.
/// `start` and `end` are the event timestamps, including any pre-roll the caller added.
pub async fn cut_detection_clip(
    cfg: &Config,
    camera: &str,
    id: Uuid,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> ClipCut {
    let root = cfg.recordings_dir().join(camera);
    let files: Vec<_> = recording_files(&root)
        .into_iter()
        .filter(|(_, path)| {
            std::fs::metadata(path).is_ok_and(|meta| meta.len() >= 1024)
        })
        .collect();
    let newest_closed = match files.last() {
        Some((_, path)) => !file_recently_written(path),
        None => false,
    };
    let Some(needed) = closed_segments_covering(
        &files,
        start,
        end,
        cfg.record.segment_secs as i64,
        newest_closed,
    ) else {
        return ClipCut::Wait;
    };
    match cut_clip(cfg, id, &needed, start, end).await {
        Some(path) => ClipCut::Done(path),
        None => ClipCut::Failed,
    }
}

/// Concat demuxer resolves a relative path from the list file, which lives in
/// `data/clips`, not from the process working directory.
fn ffmpeg_concat_path(path: &Path) -> String {
    let abs = std::env::current_dir()
        .map(|dir| dir.join(path))
        .unwrap_or_else(|_| path.to_path_buf());
    abs.to_string_lossy().replace('\\', "/")
}

fn file_recently_written(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    Utc::now() - DateTime::<Utc>::from(modified) < chrono::Duration::seconds(3)
}

/// Files that overlap `[start, end]`, but only after the file containing `end` is closed.
/// A missing later file means that segment is still being written, unless the newest
/// file has stopped growing.
fn closed_segments_covering(
    files: &[(DateTime<Utc>, PathBuf)],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    segment_secs: i64,
    newest_closed: bool,
) -> Option<Vec<(DateTime<Utc>, PathBuf)>> {
    if files.is_empty() {
        return None;
    }
    let container = files.iter().rposition(|(t, _)| *t <= end)?;
    if container + 1 == files.len() && !newest_closed {
        return None;
    }
    let mut needed = Vec::new();
    for i in 0..=container {
        let t = files[i].0;
        let next = if let Some(following) = files.get(i + 1) {
            following.0
        } else {
            t + chrono::Duration::seconds(segment_secs.max(1))
        };
        if t < end && next > start {
            needed.push(files[i].clone());
        }
    }
    if needed.is_empty() {
        None
    } else {
        Some(needed)
    }
}

async fn wait_for_clip(cfg: &Config, camera: &str, id: Uuid, start: DateTime<Utc>, end: DateTime<Utc>) -> Option<String> {
    let wait = std::time::Duration::from_secs(u64::from(cfg.record.segment_secs) * 2 + 15);
    let deadline = tokio::time::Instant::now() + wait;
    let root = cfg.recordings_dir().join(camera);
    loop {
        let files = recording_files(&root);
        let growing = files
            .last()
            .is_some_and(|(_, path)| newest_still_growing(path, start));
        if let ClipFiles::Ready(needed) =
            clip_files(&files, start, end, cfg.record.segment_secs as i64, growing)
        {
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
    newest_is_growing: bool,
) -> ClipFiles {
    if files.is_empty() {
        return ClipFiles::Wait;
    }
    let span = chrono::Duration::seconds(segment_secs.max(1));
    let mut needed = Vec::new();
    for i in 0..files.len() {
        let t = files[i].0;
        let next = if let Some(following) = files.get(i + 1) {
            following.0
        } else if newest_is_growing {
            // The file ffmpeg is still writing covers the feed past one segment.
            end + span
        } else {
            t + span
        };
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

fn newest_still_growing(path: &Path, start: DateTime<Utc>) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    DateTime::<Utc>::from(modified) + chrono::Duration::seconds(2) >= start
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
            .map(|(_, p)| format!("file '{}'", ffmpeg_concat_path(p)))
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
            warn!(stderr = %brief(&String::from_utf8_lossy(&out.stderr)), "clip ffmpeg failed");
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

#[cfg(test)]
fn at(hour: u32, min: u32) -> DateTime<Utc> {
    at_hms(hour, min, 0)
}

#[cfg(test)]
fn at_hms(hour: u32, min: u32, sec: u32) -> DateTime<Utc> {
    Local
        .with_ymd_and_hms(2026, 10, 23, hour, min, sec)
        .unwrap()
        .with_timezone(&Utc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_says_feeding_until_every_feeder_succeeds() {
        assert!(!show_feeding_message(6, &[false]));
        assert!(show_feeding_message(7, &[false, false]));
        assert!(show_feeding_message(7, &[true, false]));
        assert!(!show_feeding_message(7, &[true, true]));
        assert!(!show_feeding_message(7, &[]));
        assert!(!show_feeding_message(8, &[false]));
    }

    #[test]
    fn open_failures_restart_on_the_next_local_day() {
        let today = at(7, 10);
        let before = open_failure_count(today);
        record_open_failure(today);
        record_open_failure(today);
        assert_eq!(open_failure_count(today), before + 2);
        let tomorrow = Local
            .with_ymd_and_hms(2026, 10, 24, 7, 10, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(open_failure_count(tomorrow), 0);
        record_open_failure(tomorrow);
        assert_eq!(open_failure_count(tomorrow), 1);
        assert_eq!(open_failure_count(today), 0);
    }

    #[test]
    fn next_feed_is_today_before_seven_and_tomorrow_after() {
        assert_eq!(secs_until_next_feed(at_hms(6, 59, 0)), 60);
        assert_eq!(secs_until_next_feed(at(7, 0)), 24 * 3600);
        assert_eq!(secs_until_next_feed(at(8, 0)), 23 * 3600);
    }

    #[test]
    fn opens_once_during_the_utc_hour_and_stops_after_it() {
        let mut feeds = CameraFeeds::default();
        assert_eq!(feeds.step("cam1", at(6, 59), false, 3600), Step::None);
        assert_eq!(feeds.step("cam1", at(7, 0), false, 3600), Step::Open);
        assert_eq!(feeds.step("cam1", at(7, 10), false, 3600), Step::None);
        assert_eq!(
            feeds.step("cam1", at(8, 0), false, 3600),
            Step::Close {
                start: at(7, 0),
                write: true,
                event_id: None
            }
        );
        feeds.ack_close("cam1");
        assert!(feeds.mark_reviewed("cam1"));
        assert_eq!(feeds.step("cam1", at(8, 10), false, 3600), Step::None);
    }

    #[test]
    fn feeder_closes_after_open_secs_without_looking_at_the_picture() {
        let mut feeds = CameraFeeds::default();
        let start = at_hms(7, 0, 0);
        assert_eq!(feeds.step("cam1", start, false, 8), Step::Open);
        let early = at_hms(7, 0, 7);
        assert_eq!(feeds.step("cam1", early, false, 8), Step::None);
        let due = at_hms(7, 0, 8);
        assert_eq!(
            feeds.step("cam1", due, false, 8),
            Step::Close {
                start,
                write: true,
                event_id: None
            }
        );
        feeds.ack_close("cam1");
        assert!(feeds.mark_reviewed("cam1"));
        let later = at_hms(7, 1, 0);
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
        match clip_files(&open, start, end, 60, true) {
            ClipFiles::Ready(got) => assert_eq!(got.len(), 1),
            ClipFiles::Wait => panic!("the open segment should be usable"),
        }
        match clip_files(&open, t(7, 45, 10), t(7, 45, 30), 60, true) {
            ClipFiles::Ready(got) => assert_eq!(got.len(), 1),
            ClipFiles::Wait => panic!("a long open segment should still cover the feed"),
        }
        let closed = vec![
            (t(7, 44, 0), PathBuf::from("a.mp4")),
            (t(7, 45, 0), PathBuf::from("b.mp4")),
        ];
        match clip_files(&closed, start, end, 60, true) {
            ClipFiles::Ready(got) => assert_eq!(got.len(), 1),
            ClipFiles::Wait => panic!("closed segment should be ready"),
        }
    }

    #[test]
    fn concat_paths_are_absolute_so_ffmpeg_does_not_look_next_to_the_list() {
        let listed = ffmpeg_concat_path(Path::new("data/recordings/cam1/2026-11-04/20-00-00.mp4"));
        assert!(listed.contains("data/recordings/cam1/2026-11-04/20-00-00.mp4"));
        assert!(Path::new(&listed).is_absolute());
    }

    #[test]
    fn an_event_waits_until_the_segment_containing_its_end_is_closed() {
        let t = |h, m, s| Utc.with_ymd_and_hms(2026, 11, 4, h, m, s).unwrap();
        let open = vec![
            (t(13, 40, 48), PathBuf::from("21-40-48.mp4")),
            (t(13, 41, 48), PathBuf::from("21-41-48.mp4")),
        ];
        let start = t(13, 41, 41);
        let end = t(13, 41, 53);
        assert!(closed_segments_covering(&open, start, end, 60, false).is_none());
        let closed = vec![
            (t(13, 40, 48), PathBuf::from("21-40-48.mp4")),
            (t(13, 41, 48), PathBuf::from("21-41-48.mp4")),
            (t(13, 42, 48), PathBuf::from("21-42-48.mp4")),
        ];
        let got = closed_segments_covering(&closed, start, end, 60, false).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got[0].1.ends_with("21-40-48.mp4"));
        assert!(got[1].1.ends_with("21-41-48.mp4"));
    }

    #[test]
    fn the_next_utc_day_can_feed_again() {
        let mut feeds = CameraFeeds::default();
        assert_eq!(feeds.step("cam1", at(7, 0), false, 3600), Step::Open);
        let _ = feeds.step("cam1", at(8, 0), false, 3600);
        feeds.ack_close("cam1");
        assert!(feeds.mark_reviewed("cam1"));
        let next = Local
            .with_ymd_and_hms(2026, 10, 24, 7, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(feeds.step("cam1", next, false, 3600), Step::Open);
    }

    #[test]
    fn a_failed_close_is_retried_until_it_is_acknowledged() {
        let mut feeds = CameraFeeds::default();
        let start = at(7, 0);
        assert_eq!(feeds.step("cam1", start, false, 8), Step::Open);
        let due = at_hms(7, 0, 8);
        assert_eq!(
            feeds.step("cam1", due, false, 8),
            Step::Close {
                start,
                write: true,
                event_id: None
            }
        );
        let too_soon = at_hms(7, 0, 10);
        assert_eq!(feeds.step("cam1", too_soon, false, 8), Step::None);
        let retry = at_hms(7, 0, 13);
        assert_eq!(
            feeds.step("cam1", retry, false, 8),
            Step::Close {
                start,
                write: true,
                event_id: None
            }
        );
        feeds.ack_close("cam1");
        assert!(feeds.mark_reviewed("cam1"));
        assert_eq!(feeds.step("cam1", retry, false, 8), Step::None);
    }

    #[test]
    fn zero_feed_time_still_closes_a_feeder_that_is_already_open() {
        let mut feeds = CameraFeeds::default();
        let start = at(7, 0);
        assert_eq!(feeds.step("cam1", start, false, 8), Step::Open);
        assert_eq!(
            feeds.step("cam1", start, false, 0),
            Step::Close {
                start,
                write: true,
                event_id: None
            }
        );
    }

    #[test]
    fn the_next_day_does_not_open_while_yesterday_is_still_on() {
        let mut feeds = CameraFeeds::default();
        let start = at(7, 30);
        assert_eq!(feeds.step("cam1", start, false, 8), Step::Open);
        let next = Local
            .with_ymd_and_hms(2026, 10, 24, 7, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            feeds.step("cam1", next, false, 8),
            Step::Close {
                start,
                write: true,
                event_id: None
            }
        );
        feeds.ack_close("cam1");
        assert!(feeds.mark_reviewed("cam1"));
        assert_eq!(feeds.step("cam1", next, false, 8), Step::Open);
    }

    #[test]
    fn give_up_is_recorded_once() {
        let mut feeds = CameraFeeds::default();
        assert_eq!(feeds.step("cam1", at(7, 0), false, 8), Step::Open);
        feeds.fail_open("cam1", at(7, 0));
        assert_eq!(feeds.step("cam1", at(7, 0), false, 8), Step::None);
        assert_eq!(feeds.step("cam1", at(8, 0), false, 8), Step::GiveUp);
        assert_eq!(feeds.step("cam1", at(8, 0), false, 8), Step::None);
        feeds.mark_give_up("cam1");
        assert_eq!(feeds.step("cam1", at(8, 1), false, 8), Step::None);
    }

    #[test]
    fn an_old_recording_is_not_used_as_the_feed_clip() {
        let t = |h, m, s| Utc.with_ymd_and_hms(2026, 10, 23, h, m, s).unwrap();
        let stale = vec![(t(6, 0, 0), PathBuf::from("old.mp4"))];
        match clip_files(&stale, t(7, 44, 30), t(7, 44, 41), 60, false) {
            ClipFiles::Wait => {}
            ClipFiles::Ready(_) => panic!("a finished file from before the feed must not be cut"),
        }
    }

    #[test]
    fn a_restart_adopts_the_unfinished_event_instead_of_opening_again() {
        let mut feeds = CameraFeeds::default();
        let start = at(7, 10);
        let id = Uuid::new_v4();
        feeds.adopt("cam1", start, id);
        let during = at_hms(7, 10, 3);
        assert_eq!(feeds.step("cam1", during, false, 8), Step::None);
        let due = at_hms(7, 10, 8);
        assert_eq!(
            feeds.step("cam1", due, false, 8),
            Step::Close {
                start,
                write: true,
                event_id: Some(id)
            }
        );
    }
}
