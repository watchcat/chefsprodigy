//! Bearer-protected Streamable HTTP MCP endpoint.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use http_body_util::BodyExt;
use node_server::state::default_mcp_allowed_hosts_with;
use node_server::{AppState, NodeInfo, build_router};
use recipe_db::Db;
use serde_json::{Value, json};
use tower::ServiceExt;

static COUNTER: AtomicU32 = AtomicU32::new(0);

async fn app(api_key: Option<&str>) -> Router {
    app_with(api_key, |state| state).await
}

/// An app whose state is adjusted by `configure` before the router is built.
async fn app_with(api_key: Option<&str>, configure: impl FnOnce(AppState) -> AppState) -> Router {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("node-mcp-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(dir.join("recipes.db")).await.unwrap();
    let mut state = AppState::new(
        db,
        NodeInfo {
            author_did: None,
            chef_name: "T".into(),
        },
    );
    if let Some(k) = api_key {
        state = state.with_mcp_api_key(k);
    }
    build_router(configure(state))
}

fn initialize_body() -> String {
    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-03-26",
        "capabilities": {},
        "clientInfo": {"name": "test", "version": "0"}
    }})
    .to_string()
}

/// POST an `initialize` request to `/mcp` with the given `Host` header.
async fn post_initialize(app: &Router, auth: Option<&str>, host: &str) -> Response {
    let mut req = Request::post("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::HOST, host);
    if let Some(a) = auth {
        req = req.header(header::AUTHORIZATION, a);
    }
    app.clone()
        .oneshot(req.body(Body::from(initialize_body())).unwrap())
        .await
        .unwrap()
}

async fn post_mcp(app: &Router, auth: Option<&str>) -> (StatusCode, String) {
    let res = post_initialize(app, auth, "localhost").await;
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn mcp_requires_bearer_token() {
    let app = app(Some("s3cret")).await;
    let (status, body) = post_mcp(&app, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert!(v["error"].is_string());

    let (status, _) = post_mcp(&app, Some("Bearer wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = post_mcp(&app, Some("Basic s3cret")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_initialize_succeeds_with_valid_token() {
    let app = app(Some("s3cret")).await;
    let (status, body) = post_mcp(&app, Some("Bearer s3cret")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Streamable HTTP may answer as SSE: skip the empty priming event and take the first JSON payload.
    let payload = body
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim)
        .find(|d| !d.is_empty())
        .unwrap_or(body.as_str());
    let v: Value = serde_json::from_str(payload).expect(&body);
    assert_eq!(v["result"]["serverInfo"]["name"], "recipe-node");
    assert!(v["result"]["capabilities"]["tools"].is_object(), "{v}");
}

#[tokio::test]
async fn mcp_is_disabled_without_configured_key() {
    let app = app(None).await;
    let (status, body) = post_mcp(&app, Some("Bearer anything")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.contains("NODE_API_KEY"));
}

#[tokio::test]
async fn mcp_sse_responses_tell_proxies_not_to_buffer_or_cache() {
    let app = app(Some("s3cret")).await;
    let res = post_initialize(&app, Some("Bearer s3cret"), "localhost").await;
    assert_eq!(res.status(), StatusCode::OK);
    let headers = res.headers();
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        content_type.starts_with("text/event-stream"),
        "expected an SSE answer, got {content_type:?}"
    );
    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-cache")
    );
    assert_eq!(
        headers
            .get("x-accel-buffering")
            .and_then(|v| v.to_str().ok()),
        Some("no")
    );
}

#[tokio::test]
async fn public_url_host_is_accepted_on_mcp_by_default() {
    let hosts = default_mcp_allowed_hosts_with(Some("https://jacob.example.com"));
    let app = app_with(Some("s3cret"), |state| state.with_mcp_allowed_hosts(hosts)).await;

    let res = post_initialize(&app, Some("Bearer s3cret"), "jacob.example.com").await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = post_initialize(&app, Some("Bearer s3cret"), "localhost").await;
    assert_eq!(res.status(), StatusCode::OK, "loopback names stay allowed");
    let res = post_initialize(&app, Some("Bearer s3cret"), "evil.example.com").await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[test]
fn default_allowed_hosts_add_the_public_url_host() {
    assert_eq!(
        default_mcp_allowed_hosts_with(None),
        ["localhost", "127.0.0.1", "::1"]
    );
    assert_eq!(
        default_mcp_allowed_hosts_with(Some("https://jacob.example.com/")),
        ["localhost", "127.0.0.1", "::1", "jacob.example.com"]
    );
    assert_eq!(
        default_mcp_allowed_hosts_with(Some("http://hub.local:8080")),
        ["localhost", "127.0.0.1", "::1", "hub.local:8080"],
        "an explicit port is kept so the Host header must carry it too"
    );
    assert_eq!(
        default_mcp_allowed_hosts_with(Some("not a url")),
        ["localhost", "127.0.0.1", "::1"]
    );
}
