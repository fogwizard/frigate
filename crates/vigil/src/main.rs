mod api;
mod cleanup;
mod ctl;
mod db;
mod detect;
mod events;
mod infer_client;
mod ingest;
mod mp_api;
mod mp_db;
mod proc_guard;
mod record;
mod stream_sign;

use anyhow::Context;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use vigil_common::Config;

#[derive(Parser, Debug)]
#[command(name = "vigil", about = "Vigil NVR — slim Frigate alternative")]
struct Cli {
    /// Path to YAML config (daemon)
    #[arg(short, long, default_value = "config/default.yaml", global = true)]
    config: PathBuf,
    /// Static web root (daemon)
    #[arg(long, default_value = "web", global = true)]
    web_dir: PathBuf,
    /// API base URL (ops subcommands)
    #[arg(long, default_value = "http://127.0.0.1:8970", global = true)]
    url: String,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Start the NVR daemon
    Run,
    /// Ping /api/health
    Health,
    /// Show daemon health and camera status
    Status,
    /// List recent events
    Events {
        #[arg(long)]
        camera: Option<String>,
        #[arg(long, default_value = "20")]
        limit: i64,
    },
    /// List recent recording segments
    Recordings {
        #[arg(long)]
        camera: Option<String>,
        #[arg(long, default_value = "20")]
        limit: i64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None | Some(Command::Run) => run_daemon(cli.config, cli.web_dir).await,
        Some(Command::Health) => ctl::get_json(&cli.url, "/api/health").await,
        Some(Command::Status) => ctl::get_json(&cli.url, "/api/status").await,
        Some(Command::Events { camera, limit }) => {
            ctl::list(&cli.url, "/api/events", camera.as_deref(), limit).await
        }
        Some(Command::Recordings { camera, limit }) => {
            ctl::list(&cli.url, "/api/recordings", camera.as_deref(), limit).await
        }
    }
}

async fn run_daemon(config: PathBuf, web_dir: PathBuf) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    proc_guard::init();

    let cfg = Arc::new(Config::load(&config).context("load config")?);

    std::fs::create_dir_all(&cfg.paths.data_dir)?;
    std::fs::create_dir_all(cfg.recordings_dir())?;
    std::fs::create_dir_all(cfg.clips_dir())?;
    std::fs::create_dir_all(cfg.snapshots_dir())?;
    std::fs::create_dir_all(cfg.live_snapshots_dir())?;

    vigil_modbus::open_bus(&cfg.feed.port, cfg.feed.baud).context("open modbus serial")?;
    let _close_serial = CloseSerial;

    let db = Arc::new(db::Db::open(&cfg.paths.db_path)?);
    db.migrate_miniprogram()?;
    if cfg.miniprogram.enabled {
        let cams: Vec<String> = cfg.cameras.keys().cloned().collect();
        db.seed_miniprogram_demo(&cams)?;
    }
    let infer = Arc::new(infer_client::InferClient::new(cfg.clone())?);
    let infer_flag = infer.connected_flag();
    let events = Arc::new(events::EventEngine::new(cfg.clone(), db.clone()));

    let cam_names: Vec<String> = cfg.cameras.keys().cloned().collect();
    let statuses = detect::new_status_map(&cam_names);

    tokio::spawn(infer.clone().supervise());

    {
        let cfg = cfg.clone();
        let db = db.clone();
        let infer = infer.clone();
        let events = events.clone();
        let statuses = statuses.clone();
        tokio::spawn(async move {
            detect::run_detect_pipeline(cfg, db, infer, events, statuses).await;
        });
    }
    tokio::spawn(events.clone().sweep_timeouts());
    tokio::spawn(cleanup::run_cleanup(cfg.clone(), db.clone()));

    let state = Arc::new(api::AppState {
        cfg: cfg.clone(),
        db,
        started: Instant::now(),
        infer_connected: infer_flag,
        statuses,
        version: env!("CARGO_PKG_VERSION").to_string(),
    });

    let app = api::router(state, web_dir);
    let bind = &cfg.web.bind;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    info!(%bind, cameras = cfg.cameras.len(), "vigil listening");

    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        warn!("shutdown signal — killing tracked child processes");
        proc_guard::kill_all_tracked();
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    proc_guard::kill_all_tracked();
    Ok(())
}

struct CloseSerial;

impl Drop for CloseSerial {
    fn drop(&mut self) {
        vigil_modbus::close_bus();
    }
}
