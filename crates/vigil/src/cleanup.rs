use crate::db::Db;
use std::sync::Arc;
use vigil_common::Config;

pub async fn run_cleanup(cfg: Arc<Config>, db: Arc<Db>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        tick.tick().await;
        let cfg = cfg.clone();
        let db = db.clone();
        let result = tokio::task::spawn_blocking(move || {
            crate::record::enforce_data_budget(&cfg, &db)
        })
        .await;
        match result {
            Ok(Err(e)) => tracing::warn!(error = %e, "cleanup"),
            Err(e) => tracing::warn!(error = %e, "cleanup task"),
            Ok(Ok(())) => {}
        }
    }
}
