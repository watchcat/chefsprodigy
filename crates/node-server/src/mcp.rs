//! Streamable HTTP MCP transport mounted at `/mcp`, protected by a bearer token.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use rmcp::transport::streamable_http_server::StreamableHttpServerConfig;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::StreamableHttpService;

use crate::{ApiError, AppState};

/// Environment variable / flag that enables the endpoint.
pub const API_KEY_ENV: &str = "NODE_API_KEY";

pub fn router(state: &AppState) -> Router<AppState> {
    let mut config = StreamableHttpServerConfig::default();
    config = if state.mcp_allowed_hosts().is_empty() {
        config.disable_allowed_hosts()
    } else {
        config.with_allowed_hosts(state.mcp_allowed_hosts().iter().cloned())
    };
    let factory_state = state.clone();
    let service = StreamableHttpService::new(
        move || Ok(factory_state.mcp_server()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    Router::new()
        .nest_service("/mcp", service)
        .route_layer(middleware::from_fn_with_state(state.clone(), require_bearer))
}

/// Constant-time equality to avoid leaking key prefixes through timing.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn require_bearer(State(state): State<AppState>, req: Request<Body>, next: Next) -> Result<Response, ApiError> {
    let Some(expected) = state.mcp_api_key() else {
        return Err(ApiError::ServiceUnavailable(format!("MCP endpoint disabled: set {API_KEY_ENV} to enable it")));
    };
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(str::trim);
    match presented {
        Some(token) if ct_eq(token.as_bytes(), expected.as_bytes()) => Ok(next.run(req).await),
        _ => Err(ApiError::Unauthorized("missing or invalid bearer token".to_string())),
    }
}
