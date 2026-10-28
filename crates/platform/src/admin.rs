//! `/admin/*` behind `PLATFORM_ADMIN_TOKEN`: register, change and remove hubs, list
//! their poll status and trigger a poll. Without the token every route answers 503;
//! hubctl uses the CLI through `kubectl exec` instead, so no admin port is needed.

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::db::{Hub, HubPatch, HubRegistration};
use crate::error::PlatformError;
use crate::poller::{PollMode, PollReport};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/hubs", get(list_hubs).post(register_hub))
        .route("/admin/hubs/{name}", patch(update_hub).delete(remove_hub))
        .route("/admin/poll", post(poll))
}

/// Constant-time byte comparison for secrets.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Extractor: the request carries `Authorization: Bearer <PLATFORM_ADMIN_TOKEN>`.
pub struct RequireAdmin;

impl FromRequestParts<AppState> for RequireAdmin {
    type Rejection = PlatformError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(expected) = state.config().admin_token.as_deref() else {
            return Err(PlatformError::Unavailable(
                "admin API disabled: set PLATFORM_ADMIN_TOKEN to enable it, or use the platform CLI"
                    .to_string(),
            ));
        };
        let presented = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.strip_prefix("Bearer ")
                    .or_else(|| v.strip_prefix("bearer "))
            })
            .map(str::trim);
        match presented {
            Some(token) if ct_eq(token.as_bytes(), expected.as_bytes()) => Ok(Self),
            _ => Err(PlatformError::Unauthorized(
                "missing or invalid admin token".to_string(),
            )),
        }
    }
}

async fn list_hubs(
    _admin: RequireAdmin,
    State(state): State<AppState>,
) -> Result<Json<Vec<Hub>>, PlatformError> {
    Ok(Json(state.db().list_hubs().await?))
}

async fn register_hub(
    _admin: RequireAdmin,
    State(state): State<AppState>,
    Json(body): Json<HubRegistration>,
) -> Result<(StatusCode, Json<Hub>), PlatformError> {
    let (hub, created) = state.db().upsert_hub(&body).await?;
    tracing::info!(hub = %hub.name, created, "hub registered");
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(hub)))
}

async fn update_hub(
    _admin: RequireAdmin,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<HubPatch>,
) -> Result<Json<Hub>, PlatformError> {
    match state.db().update_hub(&name, &body).await? {
        Some(hub) => Ok(Json(hub)),
        None => Err(PlatformError::NotFound(format!(
            "hub {name} is not registered"
        ))),
    }
}

async fn remove_hub(
    _admin: RequireAdmin,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, PlatformError> {
    if state.db().remove_hub(&name).await? {
        tracing::info!(hub = %name, "hub removed with its index rows");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(PlatformError::NotFound(format!(
            "hub {name} is not registered"
        )))
    }
}

#[derive(Debug, Deserialize)]
struct PollQuery {
    hub: Option<String>,
}

async fn poll(
    _admin: RequireAdmin,
    State(state): State<AppState>,
    Query(q): Query<PollQuery>,
) -> Result<Json<Vec<PollReport>>, PlatformError> {
    let reports = match q.hub.as_deref().map(str::trim).filter(|h| !h.is_empty()) {
        Some(name) => vec![state.poller().poll_named(name).await?],
        None => state.poller().poll(PollMode::Force).await?,
    };
    Ok(Json(reports))
}
