//! `/admin/*`: hub registration, quotas, BYOK keys, usage and cache statistics,
//! behind `GATEWAY_ADMIN_TOKEN`.

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::auth::ct_eq;
use crate::byok;
use crate::db::{CacheStats, Hub, NewHub, Quota, QuotaPatch, UsageRow};
use crate::error::GatewayError;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/hubs", post(create_hub).get(list_hubs))
        .route("/admin/hubs/{name}", patch(update_hub).delete(delete_hub))
        .route("/admin/hubs/{name}/byok", post(set_byok))
        .route("/admin/hubs/{name}/byok/{vendor}", delete(remove_byok))
        .route("/admin/usage", get(usage))
        .route("/admin/cache/stats", get(cache_stats))
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

/// A hub as the admin API shows it: quota, status and which vendors have BYOK keys.
#[derive(Debug, Serialize)]
pub struct AdminHub {
    pub name: String,
    pub status: String,
    pub created_at: String,
    pub quota: Quota,
    pub byok_vendors: Vec<String>,
}

fn admin_view(hub: &Hub, master: Option<&str>) -> AdminHub {
    let byok_vendors = master
        .and_then(|m| byok::keys_from_blob(m, hub.byok_json_enc.as_deref()).ok())
        .map(|keys| keys.into_keys().collect())
        .unwrap_or_default();
    AdminHub {
        name: hub.name.clone(),
        status: hub.status.clone(),
        created_at: hub.created_at.clone(),
        quota: hub.quota,
        byok_vendors,
    }
}

#[derive(Debug, Deserialize)]
struct CreateHub {
    name: String,
    #[serde(default)]
    quota: Option<QuotaPatch>,
}

async fn create_hub(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Json(body): Json<CreateHub>,
) -> Result<(StatusCode, Json<NewHub>), GatewayError> {
    let mut quota = Quota::default();
    if let Some(patch) = &body.quota {
        patch.apply(&mut quota);
    }
    let hub = state.db().create_hub_with(body.name.trim(), quota).await?;
    tracing::info!(hub = %hub.name, "hub registered");
    Ok((StatusCode::CREATED, Json(hub)))
}

async fn list_hubs(
    State(state): State<AppState>,
    _admin: RequireAdmin,
) -> Result<Json<Vec<AdminHub>>, GatewayError> {
    let master = state.config().master_key.as_deref();
    Ok(Json(
        state
            .db()
            .list_hubs()
            .await?
            .iter()
            .map(|h| admin_view(h, master))
            .collect(),
    ))
}

#[derive(Debug, Deserialize)]
struct HubPatch {
    #[serde(flatten)]
    quota: QuotaPatch,
    status: Option<String>,
}

async fn update_hub(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(name): Path<String>,
    Json(body): Json<HubPatch>,
) -> Result<Json<AdminHub>, GatewayError> {
    if let Some(status) = &body.status
        && !matches!(status.as_str(), "active" | "disabled")
    {
        return Err(GatewayError::BadRequest(
            "status must be 'active' or 'disabled'".to_string(),
        ));
    }
    let hub = state
        .db()
        .update_hub(&name, &body.quota, body.status.as_deref())
        .await?
        .ok_or_else(|| GatewayError::NotFound(format!("hub {name} not found")))?;
    tracing::info!(hub = %hub.name, status = %hub.status, "hub updated");
    Ok(Json(admin_view(&hub, state.config().master_key.as_deref())))
}

async fn delete_hub(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(name): Path<String>,
) -> Result<StatusCode, GatewayError> {
    if state.db().delete_hub(&name).await? {
        tracing::info!(hub = %name, "hub deleted");
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(GatewayError::NotFound(format!("hub {name} not found")))
    }
}

#[derive(Debug, Deserialize)]
struct ByokBody {
    vendor: String,
    key: String,
}

fn master_key(state: &AppState) -> Result<&str, GatewayError> {
    state.config().master_key.as_deref().ok_or_else(|| {
        GatewayError::Unavailable(
            "BYOK disabled: set GATEWAY_MASTER_KEY to store hubs' own vendor keys".to_string(),
        )
    })
}

async fn set_byok(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(name): Path<String>,
    Json(body): Json<ByokBody>,
) -> Result<StatusCode, GatewayError> {
    let master = master_key(&state)?;
    let vendor = body.vendor.trim();
    let key = body.key.trim();
    if vendor.is_empty() || key.is_empty() {
        return Err(GatewayError::BadRequest(
            "vendor and key are required".to_string(),
        ));
    }
    let hub = state
        .db()
        .hub_by_name(&name)
        .await?
        .ok_or_else(|| GatewayError::NotFound(format!("hub {name} not found")))?;
    let mut keys = byok::keys_from_blob(master, hub.byok_json_enc.as_deref())?;
    keys.insert(vendor.to_string(), key.to_string());
    let blob = byok::blob_from_keys(master, &keys)?;
    state.db().set_byok_blob(hub.id, blob.as_deref()).await?;
    tracing::info!(hub = %hub.name, vendor, "BYOK key stored");
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_byok(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path((name, vendor)): Path<(String, String)>,
) -> Result<StatusCode, GatewayError> {
    let master = master_key(&state)?;
    let hub = state
        .db()
        .hub_by_name(&name)
        .await?
        .ok_or_else(|| GatewayError::NotFound(format!("hub {name} not found")))?;
    let mut keys = byok::keys_from_blob(master, hub.byok_json_enc.as_deref())?;
    keys.remove(&vendor);
    let blob = byok::blob_from_keys(master, &keys)?;
    state.db().set_byok_blob(hub.id, blob.as_deref()).await?;
    tracing::info!(hub = %hub.name, %vendor, "BYOK key removed");
    Ok(StatusCode::NO_CONTENT)
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

async fn cache_stats(
    State(state): State<AppState>,
    _admin: RequireAdmin,
) -> Result<Json<CacheStats>, GatewayError> {
    Ok(Json(state.db().cache_stats().await?))
}
