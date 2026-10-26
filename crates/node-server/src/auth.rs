//! Owner vs. public reader.
//!
//! The node has one owner, identified by `NODE_API_KEY`. Requests prove ownership
//! with `Authorization: Bearer <key>` or with the `node_session` cookie set by
//! `POST /api/v1/auth/login`. Without a configured key the node runs in
//! single-user mode and every caller is the owner.

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::api::extract::Json as JsonBody;
use crate::{ApiError, AppState};

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "node_session";
/// Session lifetime in seconds (30 days).
const SESSION_MAX_AGE: u64 = 30 * 24 * 3600;

/// Constant-time equality to avoid leaking secrets through timing.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The value stored in the session cookie: a digest of the key, so the key
/// itself never reaches the browser.
pub fn session_token(api_key: &str) -> String {
    let digest = Sha256::new().chain_update(b"chefsprodigy-session:").chain_update(api_key.as_bytes()).finalize();
    digest.iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim)
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).find_map(|line| {
        line.split(';').map(str::trim).find_map(|pair| pair.strip_prefix(name).and_then(|rest| rest.strip_prefix('=')))
    })
}

/// Whether the request carries proof of ownership. Always true without a key.
pub fn is_owner(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(key) = state.mcp_api_key() else { return true };
    if let Some(token) = bearer_token(headers) {
        if ct_eq(token.as_bytes(), key.as_bytes()) {
            return true;
        }
    }
    if let Some(cookie) = cookie_value(headers, SESSION_COOKIE) {
        if ct_eq(cookie.as_bytes(), session_token(key).as_bytes()) {
            return true;
        }
    }
    false
}

/// Extractor that rejects anyone but the owner with a JSON 401.
pub struct RequireOwner;

impl FromRequestParts<AppState> for RequireOwner {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        if is_owner(state, &parts.headers) {
            Ok(Self)
        } else {
            Err(ApiError::Unauthorized("this action requires the node owner's key or a login session".to_string()))
        }
    }
}

/// Extractor that never fails and reports whether the caller is the owner.
pub struct OptionalOwner(pub bool);

impl FromRequestParts<AppState> for OptionalOwner {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        Ok(Self(is_owner(state, &parts.headers)))
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub api_key: String,
}

fn session_cookie(value: &str, max_age: u64) -> HeaderValue {
    HeaderValue::from_str(&format!("{SESSION_COOKIE}={value}; Max-Age={max_age}; HttpOnly; SameSite=Lax; Path=/"))
        .unwrap_or_else(|_| HeaderValue::from_static(""))
}

async fn login(State(state): State<AppState>, JsonBody(req): JsonBody<LoginRequest>) -> Result<Response, ApiError> {
    let Some(key) = state.mcp_api_key() else {
        // Single-user mode: nothing to prove, nothing to set.
        return Ok(Json(json!({ "authenticated": true, "single_user": true })).into_response());
    };
    if !ct_eq(req.api_key.trim().as_bytes(), key.as_bytes()) {
        return Err(ApiError::Unauthorized("invalid api key".to_string()));
    }
    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, session_cookie(&session_token(key), SESSION_MAX_AGE))],
        Json(json!({ "authenticated": true })),
    )
        .into_response())
}

async fn logout() -> Response {
    (StatusCode::OK, [(header::SET_COOKIE, session_cookie("", 0))], Json(json!({ "authenticated": false }))).into_response()
}

async fn me(OptionalOwner(owner): OptionalOwner) -> Json<serde_json::Value> {
    Json(json!({ "authenticated": owner }))
}
