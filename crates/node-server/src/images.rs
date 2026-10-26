//! Serves stored cover images from the image directory.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::{ApiError, AppState};

pub async fn serve_image(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let path = state
        .images()
        .file_path(&name)
        .ok_or_else(|| ApiError::not_found("image not found"))?;
    match tokio::fs::read(&path).await {
        Ok(bytes) => Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "image/webp"),
                (header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            Body::from(bytes),
        )
            .into_response()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(ApiError::not_found("image not found"))
        }
        Err(e) => Err(ApiError::Internal(e.into())),
    }
}
