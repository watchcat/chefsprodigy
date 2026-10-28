//! Embedded static assets for the platform apex search page.
//!
//! The platform embeds the small `frontend/platform` UI and the canonical hub
//! token file from `frontend/dist/tokens.css`. Unknown paths deliberately stay
//! on the API's JSON 404 fallback; this service has no client-side catch-all.

use axum::body::Body;
use axum::extract::Request;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

use crate::PlatformError;

#[derive(Embed)]
#[folder = "../../frontend"]
#[include = "platform/**"]
#[include = "dist/tokens.css"]
struct Assets;

/// Serve the apex shell and its explicit static files only.
pub async fn serve_embedded(req: Request) -> Result<Response, PlatformError> {
    let asset = match req.uri().path() {
        "/" => "platform/index.html".to_string(),
        "/assets/tokens.css" => "dist/tokens.css".to_string(),
        path if path.starts_with("/assets/") => {
            let suffix = path.trim_start_matches("/assets/");
            format!("platform/{suffix}")
        }
        _ => return Err(PlatformError::NotFound("no such endpoint".to_string())),
    };

    let Some(file) = Assets::get(&asset) else {
        return Err(PlatformError::NotFound("no such endpoint".to_string()));
    };
    let content_type = mime_guess::from_path(&asset)
        .first_or_octet_stream()
        .to_string();
    Ok((
        [(header::CONTENT_TYPE, content_type)],
        Body::from(file.data.into_owned()),
    )
        .into_response())
}
