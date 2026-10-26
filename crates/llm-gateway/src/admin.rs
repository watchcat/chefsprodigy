//! `/admin/*`: hub registration and usage, behind `GATEWAY_ADMIN_TOKEN`.
//! v1 covers creating and listing hubs and reading usage; quotas, BYOK and
//! cache statistics arrive with v2 (RN-68).

use axum::extract::{FromRequestParts, Query, State};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::auth::ct_eq;
use crate::db::{Hub, NewHub, UsageRow};
use crate::error::GatewayError;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/hubs", post(create_hub).get(list_hubs))
        .route("/admin/usage", get(usage))
}

/// Extractor: the request carries `Authorization: Bearer <GATEWAY_ADMIN_TOKEN>`.
pub struct RequireAdmin;

impl FromRequestParts<AppState> for RequireAdmin {
    type Rejection = GatewayError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(expected) = state.config().admin_token.as_deref() else {
            return Err(GatewayError::Unavailable(
                "admin API disabled: set GATEWAY_ADMIN_TOKEN to enable it".to_string(),
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
            _ => Err(GatewayError::Unauthorized(
                "missing or invalid admin token".to_string(),
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
struct CreateHub {
    name: String,
}

async fn create_hub(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Json(body): Json<CreateHub>,
) -> Result<(StatusCode, Json<NewHub>), GatewayError> {
    let hub = state.db().create_hub(body.name.trim()).await?;
    tracing::info!(hub = %hub.name, "hub registered");
    Ok((StatusCode::CREATED, Json(hub)))
}

async fn list_hubs(
    State(state): State<AppState>,
    _admin: RequireAdmin,
) -> Result<Json<Vec<Hub>>, GatewayError> {
    Ok(Json(state.db().list_hubs().await?))
}

#[derive(Debug, Deserialize)]
struct UsageQuery {
    hub: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

async fn usage(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Query(q): Query<UsageQuery>,
) -> Result<Json<Vec<UsageRow>>, GatewayError> {
    Ok(Json(
        state
            .db()
            .usage(q.hub.as_deref(), q.from.as_deref(), q.to.as_deref())
            .await?,
    ))
}
