use crate::db::Db;
use std::sync::Arc;
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
    for camera in cfg.cameras.keys() {
        crate::record::prune_camera(cfg, db, camera);
    }
    Ok(())
}
