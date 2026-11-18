use crate::proc_guard;
use anyhow::{Context, Result};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};
use tokio::time::{sleep, Duration};
use tracing::{info, warn};
use vigil_common::ipc::{self, InferRequest, InferResponse};
use vigil_common::shm::FrameRing;
use vigil_common::Config;

#[cfg(unix)]
use tokio::net::UnixStream;

pub struct InferClient {
    cfg: Arc<Config>,
    connected: Arc<RwLock<bool>>,
    stream: Mutex<Option<BoxStream>>,
    child: Mutex<Option<Child>>,
    ring: Arc<FrameRing>,
}

enum BoxStream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl AsyncRead for BoxStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            BoxStream::Tcp(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            #[cfg(unix)]
            BoxStream::Unix(s) => std::pin::Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for BoxStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        match &mut *self {
            BoxStream::Tcp(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            #[cfg(unix)]
            BoxStream::Unix(s) => std::pin::Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        match &mut *self {
            BoxStream::Tcp(s) => std::pin::Pin::new(s).poll_flush(cx),
            #[cfg(unix)]
            BoxStream::Unix(s) => std::pin::Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        match &mut *self {
            BoxStream::Tcp(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            #[cfg(unix)]
            BoxStream::Unix(s) => std::pin::Pin::new(s).poll_shutdown(cx),
        }
    }
}

impl InferClient {
    pub fn new(cfg: Arc<Config>) -> Result<Self> {
        let ring = FrameRing::open(
            &cfg.infer.shm_path,
            cfg.infer.shm_slots,
            cfg.infer.shm_slot_bytes,
            true,
        )?;
        Ok(Self {
            cfg,
            connected: Arc::new(RwLock::new(false)),
            stream: Mutex::new(None),
            child: Mutex::new(None),
            ring: Arc::new(ring),
        })
    }

    pub fn connected_flag(&self) -> Arc<RwLock<bool>> {
        self.connected.clone()
    }

    pub fn ring(&self) -> Arc<FrameRing> {
        self.ring.clone()
    }

    pub async fn ensure_running(&self) -> Result<()> {
        if self.stream.lock().await.is_some() && *self.connected.read().await {
            return Ok(());
        }

        {
            let mut child_guard = self.child.lock().await;
            if let Some(child) = child_guard.as_mut() {
                if child.try_wait()?.is_some() {
                    *child_guard = None;
                }
            }
        }

        if self.try_connect_once().await.is_ok() {
            info!(endpoint = %self.cfg.infer.endpoint, "vigil-infer already listening");
            return Ok(());
        }
        info!(endpoint = %self.cfg.infer.endpoint, "vigil-infer not listening, starting it");

        {
            let mut child_guard = self.child.lock().await;
            let need_spawn = match child_guard.as_mut() {
                Some(child) => child.try_wait()?.is_some(),
                None => true,
            };
            if need_spawn {
                *child_guard = None;
                self.spawn_infer(&mut child_guard).await?;
            }
        }

        self.connect_with_retry().await
    }

    async fn try_connect_once(&self) -> Result<()> {
        let endpoint = self.cfg.infer.endpoint.clone();
        let s = connect_endpoint(&endpoint).await?;
        *self.stream.lock().await = Some(s);
        *self.connected.write().await = true;
        info!(%endpoint, "connected to vigil-infer");
        Ok(())
    }

    async fn spawn_infer(&self, child_guard: &mut Option<Child>) -> Result<()> {
        let endpoint = &self.cfg.infer.endpoint;
        if !ipc::is_tcp_endpoint(endpoint) {
            #[cfg(unix)]
            {
                if let Some(parent) = std::path::Path::new(endpoint).parent() {
                    let _ = std::fs::create_dir_all(parent);
                    let _ = std::fs::remove_file(endpoint);
                }
            }
        }

        let model = &self.cfg.openvino.model;
        let device = &self.cfg.openvino.device;
        let labels = self.cfg.detect.labels.join(",");
        let threshold = self.cfg.detect.score_threshold.to_string();
        let shm = self.cfg.infer.shm_path.clone();
        let slots = self.cfg.infer.shm_slots.to_string();
        let slot_bytes = self.cfg.infer.shm_slot_bytes.to_string();

        let bin = resolve_sidecar_bin(&self.cfg.infer.binary);
        let mut cmd = Command::new(&bin);
        if let Ok(cwd) = std::env::current_dir() {
            cmd.current_dir(cwd);
        }
        cmd.args([
            "--endpoint",
            endpoint,
            "--model",
            model,
            "--device",
            device,
            "--labels",
            &labels,
            "--threshold",
            &threshold,
            "--shm",
            &shm,
            "--shm-slots",
            &slots,
            "--shm-slot-bytes",
            &slot_bytes,
        ])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);

        proc_guard::apply_spawn_hooks(&mut cmd);
        match cmd.spawn() {
            Ok(mut child) => {
                proc_guard::track_child(&child);
                sleep(Duration::from_millis(500)).await;
                if let Ok(Some(status)) = child.try_wait() {
                    if let Some(pid) = child.id() {
                        proc_guard::untrack_pid(pid);
                    }
                    warn!(
                        ?status,
                        %endpoint,
                        "vigil-infer exited right after spawn (port busy?). Use existing listener or free the port."
                    );
                    return Ok(());
                }
                info!(bin = %bin, "spawned vigil-infer");
                *child_guard = Some(child);
            }
            Err(e) => {
                warn!(error = %e, "could not spawn vigil-infer; will try connect only");
            }
        }
        Ok(())
    }

    async fn connect_with_retry(&self) -> Result<()> {
        let endpoint = self.cfg.infer.endpoint.clone();
        let mut backoff = 1u64;
        // CoreML cold compile on Apple Silicon can exceed ~40s; allow ~3 min.
        for _ in 0..36 {
            match self.try_connect_once().await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    warn!(error = %e, "infer connect failed");
                    sleep(Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(5);
                }
            }
        }
        *self.connected.write().await = false;
        anyhow::bail!("unable to connect to infer at {endpoint}")
    }

    /// Frame already published to shm; notify infer with slot+seq only.
    pub async fn infer_slot(&self, slot: u32, seq: u64) -> Result<InferResponse> {
        let req = InferRequest { slot, seq };

        let mut guard = self.stream.lock().await;
        if guard.is_none() {
            drop(guard);
            self.ensure_running().await?;
            guard = self.stream.lock().await;
        }
        let stream = guard.as_mut().context("no infer stream")?;
        if let Err(e) = ipc::send_json(stream, &req).await {
            *self.connected.write().await = false;
            *guard = None;
            return Err(e);
        }
        match ipc::recv_json::<_, InferResponse>(stream).await {
            Ok(resp) => Ok(resp),
            Err(e) => {
                *self.connected.write().await = false;
                *guard = None;
                Err(e)
            }
        }
    }

    pub async fn supervise(self: Arc<Self>) {
        loop {
            {
                let mut child_guard = self.child.lock().await;
                if let Some(child) = child_guard.as_mut() {
                    if let Ok(Some(status)) = child.try_wait() {
                        warn!(?status, "owned vigil-infer exited");
                        *child_guard = None;
                        *self.connected.write().await = false;
                        *self.stream.lock().await = None;
                    }
                }
            }

            if let Err(e) = self.ensure_running().await {
                warn!(error = %e, "infer supervisor");
            }
            sleep(Duration::from_secs(3)).await;
        }
    }
}

fn resolve_sidecar_bin(name: &str) -> String {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            #[cfg(windows)]
            let candidate = dir.join(format!("{name}.exe"));
            #[cfg(not(windows))]
            let candidate = dir.join(name);
            if candidate.exists() {
                return candidate.to_string_lossy().to_string();
            }
        }
    }
    name.to_string()
}

async fn connect_endpoint(endpoint: &str) -> Result<BoxStream> {
    if ipc::is_tcp_endpoint(endpoint) {
        let s = TcpStream::connect(endpoint)
            .await
            .with_context(|| format!("tcp connect {endpoint}"))?;
        Ok(BoxStream::Tcp(s))
    } else {
        #[cfg(unix)]
        {
            let s = UnixStream::connect(endpoint)
                .await
                .with_context(|| format!("unix connect {endpoint}"))?;
            Ok(BoxStream::Unix(s))
        }
        #[cfg(not(unix))]
        {
            anyhow::bail!("unix sockets unsupported; set infer.endpoint to host:port");
        }
    }
}
