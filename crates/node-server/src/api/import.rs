//! `/api/v1/import/*`: URL, raw text and Mealie backup imports.

use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use recipe_mcp::MealieImportReport;
use serde::Deserialize;

use crate::api::extract::Json as JsonBody;
use crate::{ApiError, AppState};

/// Upper bound for Mealie backup uploads.
const MAX_UPLOAD_BYTES: usize = 512 * 1024 * 1024;
/// Upper bound for a photo import request (6 photos of 20 MB).
const MAX_PHOTO_UPLOAD_BYTES: usize = 128 * 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/import/url", post(import_url))
        .route("/import/text", post(import_text))
        .route("/import/mealie-backup", post(import_mealie).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)))
        .route("/import/photo", post(import_photo).layer(DefaultBodyLimit::max(MAX_PHOTO_UPLOAD_BYTES)))
}

#[derive(Debug, Deserialize)]
pub struct UrlImportRequest {
    pub url: String,
    #[serde(default)]
    pub is_public: bool,
}

#[derive(Debug, Deserialize)]
pub struct TextImportRequest {
    pub text: String,
    #[serde(default)]
    pub origin_url: Option<String>,
    #[serde(default)]
    pub is_public: bool,
}

async fn import_url(State(state): State<AppState>, JsonBody(req): JsonBody<UrlImportRequest>) -> Result<Response, ApiError> {
    let recipe = state.imports().import_url(&req.url, req.is_public).await?;
    Ok((StatusCode::CREATED, Json(recipe)).into_response())
}

async fn import_text(State(state): State<AppState>, JsonBody(req): JsonBody<TextImportRequest>) -> Result<Response, ApiError> {
    let recipe = state.imports().import_text(&req.text, req.origin_url.as_deref(), req.is_public).await?;
    Ok((StatusCode::CREATED, Json(recipe)).into_response())
}

async fn import_mealie(State(state): State<AppState>, mut multipart: Multipart) -> Result<Json<MealieImportReport>, ApiError> {
    let mut archive: Option<Vec<u8>> = None;
    while let Some(field) = multipart.next_field().await.map_err(|e| ApiError::BadRequest(e.body_text()))? {
        if field.file_name().is_some() || field.name() == Some("file") {
            let bytes = field.bytes().await.map_err(|e| ApiError::BadRequest(e.body_text()))?;
            archive = Some(bytes.to_vec());
            break;
        }
    }
    let archive = archive.ok_or_else(|| ApiError::BadRequest("multipart upload must contain a zip file field".to_string()))?;
    Ok(Json(state.imports().import_mealie(archive).await?))
}

/// `POST /import/photo`: multipart with one or more `photos` files and an optional
/// `is_public` text field. Reads the recipe with the vision model.
async fn import_photo(State(state): State<AppState>, mut multipart: Multipart) -> Result<Response, ApiError> {
    let mut photos: Vec<Vec<u8>> = Vec::new();
    let mut is_public = false;
    while let Some(field) = multipart.next_field().await.map_err(|e| ApiError::BadRequest(e.body_text()))? {
        let name = field.name().map(str::to_string);
        if field.file_name().is_some() || matches!(name.as_deref(), Some("photos" | "photo" | "file")) {
            photos.push(field.bytes().await.map_err(|e| ApiError::BadRequest(e.body_text()))?.to_vec());
        } else if name.as_deref() == Some("is_public") {
            let text = field.text().await.map_err(|e| ApiError::BadRequest(e.body_text()))?;
            is_public = matches!(text.trim(), "true" | "1" | "on" | "yes");
        }
    }
    let recipe = state.imports().import_photos(photos, is_public).await?;
    Ok((StatusCode::CREATED, Json(recipe)).into_response())
}
