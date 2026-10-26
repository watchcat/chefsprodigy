//! Loopback health probe behind the container `HEALTHCHECK`
//! (`node-server healthcheck`), so the image needs neither curl nor a shell.
//!
//! The request is a hand-written HTTP/1.1 GET over a plain TCP socket: it is a
//! few lines long, adds no HTTP client dependency to the binary, and cannot be
//! diverted by proxy environment variables the way a client library can.

use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// How long the probe waits for a complete answer before reporting unhealthy.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// Probe `/api/v1/health` on `host:port`. `Ok` when the node answers 200 with
/// `{"status":"ok"}`; otherwise an error saying what went wrong.
pub async fn check(host: &str, port: u16, timeout: Duration) -> anyhow::Result<()> {
    tokio::time::timeout(timeout, probe(host, port))
        .await
        .map_err(|_| anyhow!("no answer from {host}:{port} within {timeout:?}"))?
}

async fn probe(host: &str, port: u16) -> anyhow::Result<()> {
    let mut stream = TcpStream::connect((host, port)).await.with_context(|| format!("connecting to {host}:{port}"))?;
    let request =
        format!("GET /api/v1/health HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: node-server-healthcheck\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.context("sending the request")?;
    let mut raw = Vec::new();
    // `Connection: close` makes the server end the stream after one response.
    stream.read_to_end(&mut raw).await.context("reading the response")?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").ok_or_else(|| anyhow!("malformed response from {host}:{port}"))?;
    let status_line = head.lines().next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| anyhow!("malformed status line {status_line:?}"))?;
    if status != 200 {
        bail!("health endpoint answered HTTP {status}");
    }
    let body = body.trim();
    let value: serde_json::Value = serde_json::from_str(body).with_context(|| format!("unexpected health body: {body}"))?;
    if value.get("status").and_then(|s| s.as_str()) != Some("ok") {
        bail!("unexpected health body: {body}");
    }
    Ok(())
}
