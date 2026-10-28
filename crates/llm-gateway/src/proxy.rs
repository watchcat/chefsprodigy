//! `POST /{vendor}/v1/messages`: authenticate the hub, answer from the reply cache when
//! possible, otherwise check the hub's quota (429 before any upstream call), forward the
//! Messages-format request to the vendor with the platform or the hub's own (BYOK) key,
//! record usage and return the reply as is.

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
use crate::db::Hub;
use crate::error::GatewayError;
use crate::state::AppState;
use crate::{byok, cache, quota};

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
    // A hub's own key (BYOK) replaces the platform key and the platform quota.
    let own_key = match (&state.config().master_key, hub.byok_json_enc.as_deref()) {
        (Some(master), Some(blob)) => match byok::keys_from_blob(master, Some(blob)) {
            Ok(keys) => keys.get(&vendor.name).cloned(),
            Err(e) => {
                tracing::error!(hub = %hub.name, error = %e, "BYOK blob unreadable; using the platform key");
                None
            }
        },
        _ => None,
    };
    let byok = own_key.is_some();
    let key = match own_key {
        Some(key) => key,
        None => vendor.api_key.clone().ok_or_else(|| {
            GatewayError::Unavailable(format!(
                "vendor {} is not configured on the gateway (set {}_API_KEY)",
                vendor.name,
                vendor.name.to_ascii_uppercase()
            ))
        })?,
    };

    // A cache hit costs no vendor call, so it is served before (and regardless of) the quota.
    let cache_key =
        cache::cacheable(&request).then(|| cache::cache_key(&vendor.name, &model, &request));
    if let Some(key_hash) = &cache_key
        && let Some(hit) = state
            .db()
            .cache_hit(key_hash, state.config().cache_ttl)
            .await?
    {
        tracing::info!(
            hub = %hub.name,
            vendor = %vendor.name,
            model = %model,
            status = hit.status,
            cache = "hit",
            latency_ms = started.elapsed().as_millis() as u64,
            "served from cache"
        );
        return respond(
            StatusCode::from_u16(hit.status).unwrap_or(StatusCode::OK),
            Body::from(hit.body),
            Some(&HeaderValue::from_static("application/json")),
            None,
            "hit",
        );
    }

    // Over quota → 429 before any upstream call; BYOK hubs skip the platform limits.
    let gate = state.hub_gate(hub.id);
    let admission = if byok {
        None
    } else {
        Some(
            gate.admit(|pending| check_quota(&state, &hub, pending))
                .await?,
        )
    };
    let _permit = gate.acquire().await?;

    let upstream_headers = forwarded_headers(&headers, &key);
    let (status, reply_headers, reply) = send_with_retry(
        state.http(),
        &vendor.messages_url(),
        upstream_headers,
        body.clone(),
        state.config().retry_backoff,
    )
    .await?;

    let mut tokens = None;
    if status.is_success()
        && let Ok(value) = serde_json::from_slice::<Value>(&reply)
    {
        tokens = Some(usage_tokens(&value));
    }
    let (input_tokens, output_tokens) = tokens.unwrap_or((0, 0));
    gate.settle(admission, async {
        if tokens.is_none() {
            return Ok(());
        }
        let cost = match state.db().price_for(&vendor.name, &model).await? {
            Some((input_price, output_price)) => {
                (input_tokens as f64 * input_price + output_tokens as f64 * output_price) / 1e6
            }
            None => 0.0,
        };
        state
            .db()
            .record_usage(hub.id, &today(), input_tokens, output_tokens, cost)
            .await
    })
    .await?;

    let cache_state = match &cache_key {
        Some(key_hash) => {
            if status.is_success() && reply.len() <= state.config().cache_max_body_bytes {
                state
                    .db()
                    .cache_put(key_hash, &vendor.name, &model, status.as_u16(), &reply)
                    .await?;
                state
                    .db()
                    .cache_evict(state.config().cache_max_entries, state.config().cache_ttl)
                    .await?;
            }
            "miss"
        }
        None => "bypass",
    };
    tracing::info!(
        hub = %hub.name,
        vendor = %vendor.name,
        model = %model,
        status = status.as_u16(),
        input_tokens,
        output_tokens,
        byok,
        cache = cache_state,
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

    respond(
        status,
        Body::from(reply),
        reply_headers.get(CONTENT_TYPE),
        reply_headers.get("request-id"),
        cache_state,
    )
}

/// The free-tier check (spec sections 5.2 and 11) against today's and this month's
/// recorded usage plus the `pending` requests already admitted.
async fn check_quota(state: &AppState, hub: &Hub, pending: i64) -> Result<(), GatewayError> {
    let now = chrono::Utc::now();
    let (day, (month_first, month_last)) = quota::periods(now);
    let today = state.db().usage_sum(hub.id, &day, &day).await?;
    let month = state
        .db()
        .usage_sum(hub.id, &month_first, &month_last)
        .await?;
    quota::check(hub, &today, &month, pending, now).map_err(|reason| {
        tracing::info!(hub = %hub.name, %reason, "quota exceeded");
        GatewayError::QuotaExceeded(reason)
    })
}

/// The reply as the hub sees it, plus `x-gateway-cache: hit|miss|bypass`.
fn respond(
    status: StatusCode,
    body: Body,
    content_type: Option<&HeaderValue>,
    request_id: Option<&HeaderValue>,
    cache_state: &'static str,
) -> Result<Response, GatewayError> {
    let mut response = Response::builder()
        .status(status)
        .header("x-gateway-cache", cache_state);
    if let Some(value) = content_type {
        response = response.header(CONTENT_TYPE, value);
    }
    if let Some(value) = request_id {
        response = response.header(HeaderName::from_static("request-id"), value);
    }
    response
        .body(body)
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
