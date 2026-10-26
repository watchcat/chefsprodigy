//! Embedded web UI served from `frontend/dist` (compiled into the binary).

use axum::body::Body;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../frontend/dist"]
struct Assets;

/// Serve an embedded asset; unknown paths fall back to `index.html` so the
/// PWA can handle client-side routing.
pub async fn serve_embedded(req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path).or_else(|| Assets::get("index.html")) {
        Some(file) => {
            let mime = mime_guess::from_path(if Assets::get(path).is_some() {
                path
            } else {
                "index.html"
            })
            .first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref())],
                Body::from(file.data.into_owned()),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "ui not bundled").into_response(),
    }
}
