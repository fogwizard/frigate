use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use uuid::Uuid;
use vigil_common::{Event, Segment};

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path.as_ref()).context("open sqlite")?;
        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            CREATE TABLE IF NOT EXISTS segments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                camera TEXT NOT NULL,
                path TEXT NOT NULL,
                start_time TEXT NOT NULL,
                end_time TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_segments_cam_time ON segments(camera, start_time);
            CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                camera TEXT NOT NULL,
                label TEXT NOT NULL,
                score REAL NOT NULL,
                start_time TEXT NOT NULL,
                end_time TEXT,
                snapshot_path TEXT,
                clip_path TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_events_cam_time ON events(camera, start_time);
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            ",
        )?;
        let _ = conn.execute("ALTER TABLE events ADD COLUMN note TEXT", []);
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow::anyhow!("db lock poisoned"))
    }

    pub fn segment_paths(&self, camera: &str) -> Result<std::collections::HashSet<String>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT path FROM segments WHERE camera = ?1")?;
        let rows = stmt.query_map(params![camera], |row| row.get::<_, String>(0))?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            out.insert(row?.replace('\\', "/"));
        }
        Ok(out)
    }

    pub fn insert_segment(
        &self,
        camera: &str,
        path: &str,
        start: DateTime<Utc>,
    ) -> Result<i64> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO segments (camera, path, start_time) VALUES (?1, ?2, ?3)",
            params![camera, path, start.to_rfc3339()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn finish_segment_at_path(&self, path: &str, end: DateTime<Utc>) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE segments SET end_time = ?1 WHERE path = ?2 AND end_time IS NULL",
            params![end.to_rfc3339(), path],
        )?;
        Ok(())
    }

    pub fn get_segment(&self, id: i64) -> Result<Option<Segment>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, camera, path, start_time, end_time FROM segments WHERE id = ?1",
        )?;
        let row = stmt
            .query_row(params![id], |row| {
                Ok(Segment {
                    id: row.get(0)?,
                    camera: row.get(1)?,
                    path: row.get(2)?,
                    start_time: parse_dt(&row.get::<_, String>(3)?),
                    end_time: row
                        .get::<_, Option<String>>(4)?
                        .as_deref()
                        .map(parse_dt),
                })
            })
            .optional()?;
        Ok(row)
    }

    pub fn count_segments(
        &self,
        camera: Option<&str>,
        after: Option<DateTime<Utc>>,
        before: Option<DateTime<Utc>>,
    ) -> Result<i64> {
        let conn = self.lock()?;
        let after_s = after.map(|t| t.to_rfc3339());
        let before_s = before.map(|t| t.to_rfc3339());
        let n = conn.query_row(
            "SELECT COUNT(*) FROM segments
             WHERE (?1 IS NULL OR camera = ?1)
               AND (?2 IS NULL OR start_time >= ?2)
               AND (?3 IS NULL OR start_time <= ?3)",
            params![camera, after_s, before_s],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    pub fn list_segments(
        &self,
        camera: Option<&str>,
        after: Option<DateTime<Utc>>,
        before: Option<DateTime<Utc>>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Segment>> {
        let conn = self.lock()?;
        let after_s = after.map(|t| t.to_rfc3339());
        let before_s = before.map(|t| t.to_rfc3339());

        let mut stmt = conn.prepare(
            "SELECT id, camera, path, start_time, end_time FROM segments
             WHERE (?1 IS NULL OR camera = ?1)
               AND (?2 IS NULL OR start_time >= ?2)
               AND (?3 IS NULL OR start_time <= ?3)
             ORDER BY start_time DESC, id DESC
             LIMIT ?4 OFFSET ?5",
        )?;
        let rows = stmt.query_map(
            params![camera, after_s, before_s, limit, offset],
            |row| {
                Ok(Segment {
                    id: row.get(0)?,
                    camera: row.get(1)?,
                    path: row.get(2)?,
                    start_time: parse_dt(&row.get::<_, String>(3)?),
                    end_time: row
                        .get::<_, Option<String>>(4)?
                        .as_deref()
                        .map(parse_dt),
                })
            },
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn insert_event(&self, event: &Event) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO events (id, camera, label, score, start_time, end_time, snapshot_path, clip_path, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                event.id.to_string(),
                event.camera,
                event.label,
                event.score,
                event.start_time.to_rfc3339(),
                event.end_time.map(|t| t.to_rfc3339()),
                event.snapshot_path,
                event.clip_path,
                event.note,
            ],
        )?;
        Ok(())
    }

    pub fn update_event_note(&self, id: Uuid, note: &str, score: f32) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE events SET note = ?1, score = ?2 WHERE id = ?3",
            params![note, score, id.to_string()],
        )?;
        Ok(())
    }

    pub fn update_event_end(
        &self,
        id: Uuid,
        end: DateTime<Utc>,
        clip_path: Option<&str>,
        score: f32,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE events SET end_time = ?1, clip_path = COALESCE(?2, clip_path),
             score = CASE WHEN score > ?3 THEN score ELSE ?3 END WHERE id = ?4",
            params![end.to_rfc3339(), clip_path, score, id.to_string()],
        )?;
        Ok(())
    }

    /// `day` is the machine-local calendar date. A 07:00 feed in UTC+8 is stored
    /// as 23:00 on the previous UTC date, so a UTC-date prefix would also match
    /// yesterday's 08:00 result and skip today's feed.
    pub fn latest_feed_event(&self, camera: &str, day: NaiveDate) -> Result<Option<Event>> {
        let conn = self.lock()?;
        let (start, end) = local_day_utc_bounds(day);
        let mut stmt = conn.prepare(
            "SELECT id, camera, label, score, start_time, end_time, snapshot_path, clip_path, note
             FROM events WHERE camera = ?1 AND label = 'feed'
               AND start_time >= ?2 AND start_time < ?3
             ORDER BY start_time DESC LIMIT 1",
        )?;
        let ev = stmt
            .query_row(params![camera, start, end], map_event)
            .optional()?;
        Ok(ev)
    }

    /// Remove feed events whose start falls on this UTC date. Other labels stay.
    pub fn delete_feed_events_on_day(
        &self,
        camera: Option<&str>,
        day: chrono::NaiveDate,
    ) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let like = format!("{day}%");
        let mut stmt = conn.prepare(
            "SELECT id, camera, label, score, start_time, end_time, snapshot_path, clip_path, note
             FROM events WHERE label = 'feed' AND start_time LIKE ?1 AND (?2 IS NULL OR camera = ?2)",
        )?;
        let rows = stmt
            .query_map(params![like, camera], map_event)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        conn.execute(
            "DELETE FROM events WHERE label = 'feed' AND start_time LIKE ?1 AND (?2 IS NULL OR camera = ?2)",
            params![like, camera],
        )?;
        Ok(rows)
    }

    pub fn delete_event(&self, id: Uuid) -> Result<()> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM events WHERE id = ?1", params![id.to_string()])?;
        Ok(())
    }

    pub fn count_events(&self, camera: Option<&str>) -> Result<i64> {
        let conn = self.lock()?;
        let n = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE (?1 IS NULL OR camera = ?1)",
            params![camera],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    pub fn list_events(&self, camera: Option<&str>, limit: i64, offset: i64) -> Result<Vec<Event>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, camera, label, score, start_time, end_time, snapshot_path, clip_path, note FROM events
             WHERE (?1 IS NULL OR camera = ?1)
             ORDER BY start_time DESC, id DESC
             LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt
            .query_map(params![camera, limit, offset], map_event)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_event(&self, id: &str) -> Result<Option<Event>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, camera, label, score, start_time, end_time, snapshot_path, clip_path, note FROM events WHERE id = ?1",
        )?;
        let ev = stmt.query_row(params![id], map_event).optional()?;
        Ok(ev)
    }

    pub fn delete_segments_under(&self, dir: &Path) -> Result<usize> {
        let prefix = dir.to_string_lossy().to_string();
        let alt = prefix.replace('\\', "/");
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT id, path FROM segments")?;
        let ids: Vec<i64> = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(|r| r.ok())
            .filter_map(|(id, path)| {
                let fwd = path.replace('\\', "/");
                let under = |file: &str, root: &str| {
                    file.starts_with(root)
                        && file.as_bytes().get(root.len()).is_some_and(|b| *b == b'/' || *b == b'\\')
                };
                if under(&path, &prefix) || under(&fwd, &alt) {
                    Some(id)
                } else {
                    None
                }
            })
            .collect();
        for id in &ids {
            conn.execute("DELETE FROM segments WHERE id = ?1", params![id])?;
        }
        Ok(ids.len())
    }

    pub fn close_open_events(&self, end: DateTime<Utc>) -> Result<usize> {
        let conn = self.lock()?;
        let n = conn.execute(
            "UPDATE events SET end_time = ?1 WHERE end_time IS NULL",
            params![end.to_rfc3339()],
        )?;
        Ok(n)
    }

    pub fn delete_old_events(
        &self,
        before: DateTime<Utc>,
    ) -> Result<Vec<(Option<String>, Option<String>)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, snapshot_path, clip_path FROM events WHERE start_time < ?1",
        )?;
        let rows: Vec<(String, Option<String>, Option<String>)> = stmt
            .query_map(params![before.to_rfc3339()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        let paths: Vec<(Option<String>, Option<String>)> =
            rows.iter().map(|(_, s, c)| (s.clone(), c.clone())).collect();
        for (id, _, _) in &rows {
            conn.execute("DELETE FROM events WHERE id = ?1", params![id])?;
        }
        Ok(paths)
    }
}

fn map_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_else(|_| Uuid::nil()),
        camera: row.get(1)?,
        label: row.get(2)?,
        score: row.get(3)?,
        start_time: parse_dt(&row.get::<_, String>(4)?),
        end_time: row
            .get::<_, Option<String>>(5)?
            .as_deref()
            .map(parse_dt),
        snapshot_path: row.get(6)?,
        clip_path: row.get(7)?,
        note: row.get(8)?,
    })
}

fn parse_dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

/// Local midnight through the next local midnight, as RFC3339 UTC strings.
fn local_day_utc_bounds(day: NaiveDate) -> (String, String) {
    let start = local_midnight_utc(day);
    let end = day
        .succ_opt()
        .map(local_midnight_utc)
        .unwrap_or_else(|| start + chrono::Duration::days(1));
    (start.to_rfc3339(), end.to_rfc3339())
}

fn local_midnight_utc(day: NaiveDate) -> DateTime<Utc> {
    let naive = day.and_hms_opt(0, 0, 0).expect("midnight");
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&naive))
}
