use crate::db::Db;
use chrono::{Duration, Utc};
use std::sync::Arc;
use tracing::info;
use vigil_common::Config;

pub async fn run_cleanup(cfg: Arc<Config>, db: Arc<Db>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
    loop {
        tick.tick().await;
        if let Err(e) = cleanup_once(&cfg, &db) {
            tracing::warn!(error = %e, "cleanup");
        }
    }
}

fn cleanup_once(cfg: &Config, db: &Db) -> anyhow::Result<()> {
    let seg_before = Utc::now() - Duration::days(cfg.record.retain_days as i64);
    let paths = db.delete_old_segments(seg_before)?;
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    if !paths.is_empty() {
        info!(count = paths.len(), "deleted old segments");
    }

    let ev_before = Utc::now() - Duration::days(cfg.events.retain_days as i64);
    let ev_paths = db.delete_old_events(ev_before)?;
    for (snap, clip) in &ev_paths {
        if let Some(s) = snap {
            let _ = std::fs::remove_file(s);
        }
        if let Some(c) = clip {
            let _ = std::fs::remove_file(c);
        }
    }
    if !ev_paths.is_empty() {
        info!(count = ev_paths.len(), "deleted old events");
    }
    Ok(())
}
