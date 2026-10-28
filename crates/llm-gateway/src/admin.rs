//! `/admin/*`: hub registration, quotas and status, BYOK keys, model prices, usage and
//! cache statistics, behind `GATEWAY_ADMIN_TOKEN` (docs/SPEC-hosted-platform.md 5.2).

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::ct_eq;
use crate::byok;
use crate::db::{CacheStats, Hub, HubStatus, ModelPrice, NewHub, Quota, QuotaPatch, UsageRow};
use crate::error::GatewayError;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/hubs", post(create_hub).get(list_hubs))
        .route("/admin/hubs/{name}", patch(update_hub).delete(delete_hub))
        .route("/admin/hubs/{name}/byok", post(set_byok))
        .route("/admin/hubs/{name}/byok/{vendor}", delete(remove_byok))
        .route("/admin/prices", get(list_prices).put(set_price))
        .route("/admin/usage", get(usage))
        .route("/admin/cache/stats", get(cache_stats))
}

/// BYOK keys and prices are stored per vendor name; refuse names the gateway does not serve,
/// so a typo (`MiniMax` for `minimax`) is not silently never used.
fn known_vendor(state: &AppState, vendor: &str) -> Result<(), GatewayError> {
    let names: Vec<&str> = state
        .config()
        .vendors
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    if names.contains(&vendor) {
        Ok(())
    } else {
        Err(GatewayError::BadRequest(format!(
            "unknown vendor {vendor:?}; this gateway serves {}",
            names.join(", ")
        )))
    }
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

/// `POST /admin/hubs` answer: the token (shown once) and the quota in force.
#[derive(Debug, Serialize)]
struct CreatedHub {
    #[serde(flatten)]
    hub: NewHub,
    quota: Quota,
}

async fn create_hub(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Json(body): Json<CreateHub>,
) -> Result<(StatusCode, Json<CreatedHub>), GatewayError> {
    let mut quota = Quota::default();
    if let Some(patch) = &body.quota {
        patch.validate()?;
        patch.apply(&mut quota);
    }
    let hub = state.db().create_hub_with(body.name.trim(), quota).await?;
    tracing::info!(hub = %hub.name, "hub registered");
    Ok((StatusCode::CREATED, Json(CreatedHub { hub, quota })))
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

/// `PATCH /admin/hubs/{name}`: any quota field plus `status` (`active` | `disabled`).
/// Disabling refuses the hub's token; re-activating keeps it (`add-hub --rotate` issues a
/// new one). Unknown fields are refused.
async fn update_hub(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<AdminHub>, GatewayError> {
    let Value::Object(mut fields) = body else {
        return Err(GatewayError::BadRequest(
            "expected a JSON object of quota fields and/or status".to_string(),
        ));
    };
    let status = match fields.remove("status") {
        None | Some(Value::Null) => None,
        Some(value) => Some(serde_json::from_value::<HubStatus>(value).map_err(|_| {
            GatewayError::BadRequest("status must be 'active' or 'disabled'".to_string())
        })?),
    };
    let patch: QuotaPatch = serde_json::from_value(Value::Object(fields))
        .map_err(|e| GatewayError::BadRequest(format!("invalid quota patch: {e}")))?;
    let hub = state
        .db()
        .update_hub(&name, &patch, status)
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
    known_vendor(&state, vendor)?;
    let hub = state
        .db()
        .hub_by_name(&name)
        .await?
        .ok_or_else(|| GatewayError::NotFound(format!("hub {name} not found")))?;
    let mut keys = byok::keys_from_blob(master, hub.byok_json_enc.as_deref()).map_err(|_| {
        GatewayError::Conflict(format!(
            "hub {name} has BYOK keys that do not decrypt with this GATEWAY_MASTER_KEY; \
             clear them with DELETE /admin/hubs/{name}/byok/<vendor> first"
        ))
    })?;
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
    // Keys stored under an earlier master key are unusable: removing one clears them all.
    let mut keys = byok::keys_from_blob(master, hub.byok_json_enc.as_deref()).unwrap_or_else(|e| {
        tracing::warn!(hub = %hub.name, error = %e, "unreadable BYOK keys cleared");
        Default::default()
    });
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

async fn list_prices(
    State(state): State<AppState>,
    _admin: RequireAdmin,
) -> Result<Json<Vec<ModelPrice>>, GatewayError> {
    Ok(Json(state.db().prices().await?))
}

/// `PUT /admin/prices {vendor, model, input_cents_per_mtok, output_cents_per_mtok}`: insert
/// or replace the price used for cost estimates and the monthly cost cap. `model` is the
/// name hubs send (`MiniMax-M3`), matched exactly.
async fn set_price(
    State(state): State<AppState>,
    _admin: RequireAdmin,
    Json(mut price): Json<ModelPrice>,
) -> Result<Json<ModelPrice>, GatewayError> {
    price.vendor = price.vendor.trim().to_string();
    price.model = price.model.trim().to_string();
    known_vendor(&state, &price.vendor)?;
    if price.model.is_empty() {
        return Err(GatewayError::BadRequest("model is required".to_string()));
    }
    let valid = |cents: f64| cents.is_finite() && cents >= 0.0;
    if !valid(price.input_cents_per_mtok) || !valid(price.output_cents_per_mtok) {
        return Err(GatewayError::BadRequest(
            "prices are cents per million tokens, zero or positive".to_string(),
        ));
    }
    state.db().set_price(&price).await?;
    tracing::info!(vendor = %price.vendor, model = %price.model, "price set");
    Ok(Json(price))
}

async fn cache_stats(
    State(state): State<AppState>,
    _admin: RequireAdmin,
) -> Result<Json<CacheStats>, GatewayError> {
    Ok(Json(state.db().cache_stats().await?))
}
