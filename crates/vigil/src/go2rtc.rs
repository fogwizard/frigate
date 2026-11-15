//! Start go2rtc from the project directory when its API port is not already open.

use crate::proc_guard;
use anyhow::{Context, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};
use vigil_common::Config;

fn child_slot() -> &'static Mutex<Option<Child>> {
    static SLOT: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Binary is not on this machine; go2rtc is started on another host.
fn remote_only() -> &'static AtomicBool {
    static REMOTE: AtomicBool = AtomicBool::new(false);
    &REMOTE
}

pub async fn ensure(cfg: &Config, cwd: &Path) -> Result<()> {
    let api = cfg.go2rtc.api.as_str();
    if port_open(api).await {
        info!(%api, "go2rtc already listening");
        return Ok(());
    }
    if locate_binary(&cfg.go2rtc.binary, cwd).is_none() {
        remote_only().store(true, Ordering::Relaxed);
        let cmd = remote_launch_command(&cfg.go2rtc.config);
        let project = cwd.display().to_string();
        warn!(
            "本机没有找到 go2rtc，请在远程服务器的项目目录中执行:\n  {cmd}\n项目目录: {project}"
        );
        return Ok(());
    }
    info!(%api, "go2rtc not listening, starting it");
    spawn(cfg, cwd).await?;
    for _ in 0..25 {
        if port_open(api).await {
            info!(%api, "go2rtc is up");
            return Ok(());
        }
        sleep(Duration::from_millis(200)).await;
    }
    anyhow::bail!("go2rtc did not listen on {api}");
}

pub async fn supervise(cfg: Arc<Config>, cwd: PathBuf) {
    loop {
        sleep(Duration::from_secs(3)).await;
        if remote_only().load(Ordering::Relaxed) {
            continue;
        }
        {
            let mut child = child_slot().lock().await;
            if let Some(proc) = child.as_mut() {
                if let Ok(Some(status)) = proc.try_wait() {
                    warn!(?status, "owned go2rtc exited");
                    *child = None;
                }
            }
        }
        if port_open(&cfg.go2rtc.api).await {
            continue;
        }
        if let Err(e) = spawn(&cfg, &cwd).await {
            warn!(error = %e, "go2rtc restart failed");
        }
    }
}

async fn spawn(cfg: &Config, cwd: &Path) -> Result<()> {
    let mut slot = child_slot().lock().await;
    if let Some(proc) = slot.as_mut() {
        if proc.try_wait()?.is_none() {
            return Ok(());
        }
        *slot = None;
    }

    let Some(bin) = locate_binary(&cfg.go2rtc.binary, cwd) else {
        anyhow::bail!("go2rtc binary not found");
    };
    let config = resolve_under(cwd, &cfg.go2rtc.config);
    if !config.is_file() {
        anyhow::bail!("go2rtc config not found: {}", config.display());
    }

    info!(
        bin = %bin.display(),
        config = %config.display(),
        cwd = %cwd.display(),
        "spawning go2rtc"
    );
    let mut cmd = Command::new(&bin);
    cmd.arg("--config")
        .arg(&config)
        .current_dir(cwd)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    proc_guard::apply_spawn_hooks(&mut cmd);
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn go2rtc {}", bin.display()))?;
    proc_guard::track_child(&child);
    *slot = Some(child);
    Ok(())
}

fn locate_binary(configured: &str, cwd: &Path) -> Option<PathBuf> {
    let path = Path::new(configured);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    if candidate.is_file() {
        return Some(candidate);
    }
    let name = path.file_name().unwrap_or(OsStr::new("go2rtc"));
    find_on_path(name)
}

fn find_on_path(name: &OsStr) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(name).with_extension("exe");
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

/// Command to run on the machine that hosts go2rtc, from the project directory.
fn remote_launch_command(config: &str) -> String {
    format!("go2rtc --config {}", shell_arg(config))
}

fn shell_arg(value: &str) -> String {
    if value.chars().any(|c| c.is_whitespace()) {
        format!("\"{value}\"")
    } else {
        value.to_string()
    }
}

fn resolve_under(cwd: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

async fn port_open(addr: &str) -> bool {
    tokio::time::timeout(Duration::from_millis(400), TcpStream::connect(addr))
        .await
        .ok()
        .and_then(|r| r.ok())
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_path_is_under_the_project_directory() {
        let cwd = Path::new(r"C:\work\frigate");
        let got = resolve_under(cwd, "packaging/go2rtc.yaml");
        assert_eq!(got, cwd.join("packaging/go2rtc.yaml"));
        let abs = Path::new(r"D:\cfg\go2rtc.yaml");
        assert_eq!(resolve_under(cwd, abs.to_str().unwrap()), abs);
    }

    #[test]
    fn remote_command_uses_the_project_config() {
        assert_eq!(
            remote_launch_command("packaging/go2rtc.yaml"),
            "go2rtc --config packaging/go2rtc.yaml"
        );
        assert_eq!(
            remote_launch_command("pack aging/go2rtc.yaml"),
            "go2rtc --config \"pack aging/go2rtc.yaml\""
        );
    }
}
