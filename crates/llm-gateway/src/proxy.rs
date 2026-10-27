//! `POST /{vendor}/v1/messages`: authenticate the hub, forward the Messages-format
//! request to the vendor with the real key, record usage, return the reply as is.

use std::time::Instant;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::header::{
    ACCEPT_ENCODING, AUTHORIZATION, CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, HOST,
    PROXY_AUTHORIZATION, TE, TRAILER, TRANSFER_ENCODING, UPGRADE,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::post;
use serde_json::Value;

use crate::auth::presented_token;
use crate::error::GatewayError;
use crate::state::AppState;

/// Photo imports carry several base64 images; well above axum's 2 MB default.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{vendor}/v1/messages", post(messages))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

async fn messages(
    State(state): State<AppState>,
    Path(prefix): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, GatewayError> {
    let started = Instant::now();
    let token = presented_token(&headers).ok_or_else(|| {
        GatewayError::Unauthorized(
            "missing hub token: send it as x-api-key or Authorization: Bearer".to_string(),
        )
    })?;
    let hub =
        state.db().find_hub_by_token(&token).await?.ok_or_else(|| {
            GatewayError::Unauthorized("unknown or disabled hub token".to_string())
        })?;

    let request: Value = serde_json::from_slice(&body)
        .map_err(|e| GatewayError::BadRequest(format!("request body is not JSON: {e}")))?;
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .ok_or_else(|| GatewayError::BadRequest("request body needs a string `model`".to_string()))?
        .to_string();
    if request.get("stream").and_then(Value::as_bool) == Some(true) {
        return Err(GatewayError::NotImplemented(
            "streaming (stream: true) is not supported by this gateway yet".to_string(),
        ));
    }
    let vendor = state
        .config()
        .vendor_for(&prefix, &model)
        .ok_or_else(|| GatewayError::NotFound(format!("no vendor behind /{prefix}")))?;
    let key = vendor.api_key.as_deref().ok_or_else(|| {
        GatewayError::Unavailable(format!(
            "vendor {} is not configured on the gateway (set {}_API_KEY)",
            vendor.name,
            vendor.name.to_ascii_uppercase()
        ))
    })?;

    let permits = state.hub_permits(hub.id);
    let _permit = permits
        .acquire()
        .await
        .map_err(|_| GatewayError::Unavailable("gateway is shutting down".to_string()))?;

    let upstream_headers = forwarded_headers(&headers, key);
    let (status, reply_headers, reply) = send_with_retry(
        state.http(),
        &vendor.messages_url(),
        upstream_headers,
        body.clone(),
        state.config().retry_backoff,
    )
    .await?;

    let (mut input_tokens, mut output_tokens) = (0, 0);
    if status.is_success()
        && let Ok(value) = serde_json::from_slice::<Value>(&reply)
    {
        (input_tokens, output_tokens) = usage_tokens(&value);
        let cost = match state.db().price_for(&vendor.name, &model).await? {
            Some((input_price, output_price)) => {
                (input_tokens as f64 * input_price + output_tokens as f64 * output_price) / 1e6
            }
            None => 0.0,
        };
        state
            .db()
            .record_usage(hub.id, &today(), input_tokens, output_tokens, cost)
            .await?;
    }
    tracing::info!(
        hub = %hub.name,
        vendor = %vendor.name,
        model = %model,
        status = status.as_u16(),
        input_tokens,
        output_tokens,
        latency_ms = started.elapsed().as_millis() as u64,
        "proxied"
    );
    if state.config().log_content {
        tracing::debug!(
            request = %String::from_utf8_lossy(&body),
            reply = %String::from_utf8_lossy(&reply),
            "content"
        );
    }

    let mut response = Response::builder().status(status);
    for name in [CONTENT_TYPE, HeaderName::from_static("request-id")] {
        if let Some(value) = reply_headers.get(&name) {
            response = response.header(name, value);
        }
    }
    response
        .body(Body::from(reply))
        .map_err(|e| GatewayError::Upstream(e.to_string()))
}

/// The hub's headers minus hop-by-hop, framing and credential headers, with the
/// vendor key in `x-api-key` and the Anthropic version header guaranteed.
fn forwarded_headers(incoming: &HeaderMap, vendor_key: &str) -> HeaderMap {
    const DROP: [HeaderName; 12] = [
        HOST,
        CONTENT_LENGTH,
        AUTHORIZATION,
        PROXY_AUTHORIZATION,
        CONNECTION,
        TRANSFER_ENCODING,
        UPGRADE,
        TE,
        TRAILER,
        ACCEPT_ENCODING,
        HeaderName::from_static("x-api-key"),
        HeaderName::from_static("keep-alive"),
    ];
    let mut out = HeaderMap::new();
    for (name, value) in incoming {
        if !DROP.contains(name) {
            out.append(name.clone(), value.clone());
        }
    }
    if let Ok(value) = HeaderValue::from_str(vendor_key) {
        out.insert("x-api-key", value);
    }
    if !out.contains_key("anthropic-version") {
        out.insert(
            "anthropic-version",
            HeaderValue::from_static(DEFAULT_ANTHROPIC_VERSION),
        );
    }
    out.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    out
}

/// One attempt plus one retry after `backoff` on 5xx, 429 or a connection failure.
async fn send_with_retry(
    client: &reqwest::Client,
    url: &str,
    headers: HeaderMap,
    body: Bytes,
    backoff: std::time::Duration,
) -> Result<(StatusCode, HeaderMap, Bytes), GatewayError> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let sent = client
            .post(url)
            .headers(headers.clone())
            .body(body.clone())
            .send()
            .await;
        match sent {
            Ok(reply) => {
                let status = reply.status();
                let retryable = status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS;
                if retryable && attempt == 1 {
                    tracing::warn!(%url, status = status.as_u16(), "upstream failed; retrying once");
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                let reply_headers = reply.headers().clone();
                let bytes = reply
                    .bytes()
                    .await
                    .map_err(|e| GatewayError::Upstream(e.to_string()))?;
                return Ok((status, reply_headers, bytes));
            }
            Err(e) if attempt == 1 && e.is_connect() => {
                tracing::warn!(%url, error = %e, "upstream unreachable; retrying once");
                tokio::time::sleep(backoff).await;
            }
            Err(e) => return Err(GatewayError::Upstream(e.to_string())),
        }
    }
}

/// `(input, output)` tokens of a Messages reply. Prompt-cache writes and reads are reported
/// outside `input_tokens` (MiniMax and Anthropic alike), so they are added to the input; the
/// cost estimate therefore prices cached reads at the full input rate, an upper bound.
fn usage_tokens(reply: &Value) -> (i64, i64) {
    let usage = reply.get("usage");
    let count = |key: &str| {
        usage
            .and_then(|u| u.get(key))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    let input = count("input_tokens")
        + count("cache_creation_input_tokens")
        + count("cache_read_input_tokens");
    (input, count("output_tokens"))
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}
