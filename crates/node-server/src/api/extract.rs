//! Extractors whose rejections render as JSON `ApiError`s instead of plain text.

use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use crate::ApiError;

/// JSON body extractor with JSON-formatted rejection.
pub struct Json<T>(pub T);

impl<S, T> FromRequest<S> for Json<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let axum::Json(value) = axum::Json::<T>::from_request(req, state).await?;
        Ok(Self(value))
    }
}

/// Query-string extractor with JSON-formatted rejection.
pub struct Query<T>(pub T);

impl<S, T> FromRequestParts<S> for Query<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let axum::extract::Query(value) = axum::extract::Query::<T>::from_request_parts(parts, state).await?;
        Ok(Self(value))
    }
}
