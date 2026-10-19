//! Length-prefixed JSON IPC over TCP or Unix domain sockets.
//!
//! Image pixels live in the shared frame ring (`shm`); IPC only notifies
//! which slot/seq to read.

use crate::types::Detection;
use anyhow::{bail, Context, Result};
use bytes::{Buf, BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME: usize = 256 * 1024;

/// Notify infer to read a frame from shared memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferRequest {
    pub slot: u32,
    pub seq: u64,
    pub camera_id: String,
    pub width: u32,
    pub height: u32,
    /// "rgb8" (preferred) or "jpeg"
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferResponse {
    pub detections: Vec<Detection>,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub error: Option<String>,
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, body: &[u8]) -> Result<()> {
    if body.len() > MAX_FRAME {
        bail!("frame too large: {}", body.len());
    }
    let mut hdr = BytesMut::with_capacity(4);
    hdr.put_u32(body.len() as u32);
    w.write_all(&hdr).await?;
    w.write_all(body).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Vec<u8>> {
    let mut hdr = [0u8; 4];
    r.read_exact(&mut hdr).await.context("reading frame header")?;
    let mut cur = &hdr[..];
    let len = cur.get_u32() as usize;
    if len > MAX_FRAME {
        bail!("frame length {} exceeds max", len);
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await.context("reading frame body")?;
    Ok(buf)
}

pub async fn send_json<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, msg: &T) -> Result<()> {
    let body = serde_json::to_vec(msg)?;
    write_frame(w, &body).await
}

pub async fn recv_json<R: AsyncRead + Unpin, T: for<'de> Deserialize<'de>>(r: &mut R) -> Result<T> {
    let body = read_frame(r).await?;
    Ok(serde_json::from_slice(&body)?)
}

pub fn is_tcp_endpoint(endpoint: &str) -> bool {
    if endpoint.contains('/') || endpoint.contains('\\') {
        return false;
    }
    endpoint.contains(':') && !endpoint.starts_with('/')
}
