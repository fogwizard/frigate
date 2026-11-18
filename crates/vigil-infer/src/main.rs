mod engine;
mod yolo;

use anyhow::Context;
use clap::Parser;
use engine::InferEngine;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::{info, warn};
use vigil_common::ipc::{self, InferRequest, InferResponse};
use vigil_common::shm::FrameRing;

#[cfg(unix)]
use tokio::net::UnixListener;

#[derive(Parser, Debug)]
#[command(name = "vigil-infer", about = "YOLO inference sidecar for Vigil (shm frames + ORT)")]
struct Args {
    #[arg(long)]
    endpoint: String,
    #[arg(long, default_value = "models/yolo11n.onnx")]
    model: String,
    #[arg(long, default_value = "CPU")]
    device: String,
    #[arg(long, default_value = "person,cat,dog")]
    labels: String,
    #[arg(long, default_value = "0.5")]
    threshold: f32,
    #[arg(long, default_value = "data/frames.shm")]
    shm: String,
    #[arg(long, default_value = "16")]
    shm_slots: u32,
    #[arg(long, default_value = "1048576")]
    shm_slot_bytes: u32,
}

struct App {
    /// `None` while the ONNX/CoreML session is still loading.
    engine: RwLock<Option<InferEngine>>,
    ring: FrameRing,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let labels: Vec<String> = args
        .labels
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // Bind before model load so vigil can connect while ORT/CoreML cold-starts.
    if ipc::is_tcp_endpoint(&args.endpoint) {
        let listener = TcpListener::bind(&args.endpoint)
            .await
            .with_context(|| format!("bind tcp {}", args.endpoint))?;
        info!(endpoint = %args.endpoint, "vigil-infer listening (tcp); loading engine…");

        let ring = open_ring_retry(&args.shm, args.shm_slots, args.shm_slot_bytes).await?;
        info!(shm = %args.shm, "opened frame shm ring");
        let app = Arc::new(App {
            engine: RwLock::new(None),
            ring,
        });
        tokio::spawn(load_engine(app.clone(), args.model, args.device, labels, args.threshold));
        serve_tcp(listener, app).await
    } else {
        #[cfg(unix)]
        {
            if let Some(parent) = std::path::Path::new(&args.endpoint).parent() {
                std::fs::create_dir_all(parent)?;
            }
            let _ = std::fs::remove_file(&args.endpoint);
            let listener = UnixListener::bind(&args.endpoint)
                .with_context(|| format!("bind unix {}", args.endpoint))?;
            info!(endpoint = %args.endpoint, "vigil-infer listening (unix); loading engine…");

            let ring = open_ring_retry(&args.shm, args.shm_slots, args.shm_slot_bytes).await?;
            info!(shm = %args.shm, "opened frame shm ring");
            let app = Arc::new(App {
                engine: RwLock::new(None),
                ring,
            });
            tokio::spawn(load_engine(app.clone(), args.model, args.device, labels, args.threshold));
            serve_unix_listener(listener, app).await
        }
        #[cfg(not(unix))]
        {
            anyhow::bail!(
                "unix endpoint {} unsupported on this OS; use host:port",
                args.endpoint
            );
        }
    }
}

async fn load_engine(
    app: Arc<App>,
    model: String,
    device: String,
    labels: Vec<String>,
    threshold: f32,
) {
    let result = tokio::task::spawn_blocking(move || {
        InferEngine::load(&model, &device, labels, threshold)
    })
    .await;
    match result {
        Ok(Ok(engine)) => {
            info!(
                device = %engine.device(),
                stub = engine.is_stub(),
                "infer engine ready"
            );
            *app.engine.write().await = Some(engine);
        }
        Ok(Err(e)) => warn!(error = %e, "infer engine failed to load"),
        Err(e) => warn!(error = %e, "infer engine load task joined with error"),
    }
}

async fn open_ring_retry(path: &str, slots: u32, cap: u32) -> anyhow::Result<FrameRing> {
    let mut last = None;
    for i in 0..20 {
        match FrameRing::open(path, slots, cap, false) {
            Ok(r) => return Ok(r),
            Err(e) => {
                last = Some(e);
                // If vigil hasn't created yet, create ourselves so manual starts work.
                if i == 5 {
                    if let Ok(r) = FrameRing::open(path, slots, cap, true) {
                        return Ok(r);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("open shm failed")))
}

async fn serve_tcp(listener: TcpListener, app: Arc<App>) -> anyhow::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        info!(?peer, "client connected");
        let app = app.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, app).await {
                warn!(error = %e, "client session ended");
            }
        });
    }
}

#[cfg(unix)]
async fn serve_unix_listener(
    listener: UnixListener,
    app: Arc<App>,
) -> anyhow::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let app = app.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, app).await {
                warn!(error = %e, "client session ended");
            }
        });
    }
}

async fn handle_client<S>(mut stream: S, app: Arc<App>) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let req: InferRequest = ipc::recv_json(&mut stream).await?;
        let resp = match app.ring.take(req.slot, req.seq) {
            Ok(frame) => {
                let fmt = if frame.format == vigil_common::shm::FORMAT_JPEG {
                    "jpeg"
                } else {
                    "rgb8"
                };
                let engine_guard = app.engine.read().await;
                if let Some(engine) = engine_guard.as_ref() {
                    match engine.infer_frame(fmt, frame.width, frame.height, &frame.data) {
                        Ok(dets) => InferResponse {
                            detections: dets,
                            device: engine.device().to_string(),
                            error: None,
                        },
                        Err(e) => InferResponse {
                            detections: vec![],
                            device: engine.device().to_string(),
                            error: Some(e.to_string()),
                        },
                    }
                } else {
                    InferResponse {
                        detections: vec![],
                        device: "loading".into(),
                        error: Some("infer engine still loading".into()),
                    }
                }
            }
            Err(e) => InferResponse {
                detections: vec![],
                device: "unknown".into(),
                error: Some(format!("shm take: {e}")),
            },
        };
        ipc::send_json(&mut stream, &resp).await?;
    }
}
