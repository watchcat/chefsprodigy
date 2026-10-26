//! Deterministic-reply cache keys (docs/SPEC-hosted-platform.md section 5.2): the
//! SHA-256 of vendor, model and the canonical JSON of the request body, so two hubs
//! sending the same content hit the same entry whatever their serializer did.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::auth::sha256_hex;

/// Canonical JSON: object keys sorted recursively, no insignificant whitespace,
/// integral floats written as integers (`0.0` and `0` are the same temperature).
pub fn canonical_json(value: &Value) -> String {
    serde_json::to_string(&canonicalize(value)).unwrap_or_default()
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, Value> =
                map.iter().map(|(k, v)| (k, canonicalize(v))).collect();
            Value::Object(sorted.into_iter().map(|(k, v)| (k.clone(), v)).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 9.0e15 && !n.is_i64() && !n.is_u64() => {
                Value::from(f as i64)
            }
            _ => value.clone(),
        },
        other => other.clone(),
    }
}

/// Only deterministic, non-streaming requests are cached.
pub fn cacheable(body: &Value) -> bool {
    let deterministic = body
        .get("temperature")
        .and_then(Value::as_f64)
        .is_some_and(|t| t == 0.0);
    let streaming = body.get("stream").and_then(Value::as_bool) == Some(true);
    deterministic && !streaming
}

/// Cache key for a request: hub credentials live in headers, so the body alone
/// never identifies the hub.
pub fn cache_key(vendor: &str, model: &str, body: &Value) -> String {
    sha256_hex(format!("{vendor}\n{model}\n{}", canonical_json(body)).as_bytes())
}
