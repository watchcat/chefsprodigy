//! Hub tokens: generation, hashing and extraction from request headers.

use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Constant-time byte comparison for secrets.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 32 random bytes as 64 hex characters.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex(&bytes)
}

/// SHA-256 of a token, hex: what the database stores.
pub fn hash_token(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The credential a hub presents: `x-api-key` (what rig sends) or `Authorization: Bearer`.
pub fn presented_token(headers: &HeaderMap) -> Option<String> {
    if let Some(key) = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|k| !k.is_empty())
    {
        return Some(key.to_string());
    }
    headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}
