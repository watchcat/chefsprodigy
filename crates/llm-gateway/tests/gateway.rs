//! llm-gateway v1: Messages-format passthrough with hub tokens and usage accounting
//! (docs/SPEC-hosted-platform.md section 5.2, without cache and quotas).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::BodyExt;
use llm_gateway::{AppState, Config, Db, Vendor, build_router};
use serde_json::{Value, json};
use tower::ServiceExt;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A stand-in vendor: records every request and answers from a queue
/// (default: 200 with a one-block reply and usage 1/2).
#[derive(Clone, Default)]
struct FakeVendor {
    calls: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
    replies: Arc<Mutex<VecDeque<(StatusCode, Value)>>>,
}

impl FakeVendor {
    fn reply(&self, status: StatusCode, body: Value) -> &Self {
        self.replies.lock().unwrap().push_back((status, body));
        self
    }

    fn calls(&self) -> Vec<(HeaderMap, Value)> {
        self.calls.lock().unwrap().clone()
    }
}

async fn vendor_handler(
    State(vendor): State<FakeVendor>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    vendor.calls.lock().unwrap().push((headers, body));
    let (status, reply) = vendor
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or((StatusCode::OK, reply_json(1, 2)));
    (status, Json(reply))
}

/// Serve a fake vendor on loopback; returns the recorder and the base URL.
async fn fake_vendor() -> (FakeVendor, String) {
    let vendor = FakeVendor::default();
    let app = Router::new()
        .route("/v1/messages", post(vendor_handler))
        .with_state(vendor.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (vendor, base)
}

fn reply_json(input: u64, output: u64) -> Value {
    json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "MiniMax-M3",
        "content": [{"type": "text", "text": "{\"title\":\"Pho\"}"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": input, "output_tokens": output}
    })
}

async fn gateway(config: Config) -> (Router, AppState) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("gateway-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(dir.join("gateway.db")).await.unwrap();
    let state = AppState::new(db, config);
    (build_router(state.clone()), state)
}

fn config(minimax_base: &str) -> Config {
    let mut config = Config::default();
    config
        .vendors
        .push(Vendor::new("minimax", minimax_base, Some("vendor-secret")));
    config.admin_token = Some("admin-secret".into());
    config.retry_backoff = Duration::from_millis(10);
    config
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, headers, json)
}

fn messages_request(path: &str, token: Option<(&str, &str)>, body: &Value) -> Request<Body> {
    let mut req = Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header("anthropic-version", "2023-06-01");
    if let Some((name, value)) = token {
        req = req.header(name, value);
    }
    req.body(Body::from(body.to_string())).unwrap()
}

fn admin_request(
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Request<Body> {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    match body {
        Some(v) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    }
}

fn prompt(model: &str) -> Value {
    json!({
        "model": model, "max_tokens": 256, "temperature": 0,
        "messages": [{"role": "user", "content": "Normalize: 2 eggs"}]
    })
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

#[tokio::test]
async fn health_reports_ok() {
    let (app, _) = gateway(Config::default()).await;
    let (status, _, body) = send(&app, Request::get("/health").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn requests_without_a_valid_hub_token_are_refused() {
    let (vendor, base) = fake_vendor().await;
    let (app, _) = gateway(config(&base)).await;
    let (status, _, body) = send(
        &app,
        messages_request("/anthropic/v1/messages", None, &prompt("MiniMax-M3")),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "authentication_error");

    let (status, _, _) = send(
        &app,
        messages_request(
            "/anthropic/v1/messages",
            Some(("x-api-key", "not-a-hub-token")),
            &prompt("MiniMax-M3"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        vendor.calls().is_empty(),
        "nothing reaches the vendor without a hub token"
    );
}

#[tokio::test]
async fn messages_are_forwarded_with_the_vendor_key_and_returned_unchanged() {
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    vendor.reply(StatusCode::OK, reply_json(10, 20));

    let (status, headers, body) = send(
        &app,
        messages_request(
            "/anthropic/v1/messages",
            Some(("x-api-key", &hub.token)),
            &prompt("MiniMax-M3"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        reply_json(10, 20),
        "the vendor's reply comes back unchanged"
    );
    assert!(
        header_str(&headers, "content-type")
            .unwrap_or_default()
            .starts_with("application/json")
    );

    let calls = vendor.calls();
    assert_eq!(calls.len(), 1);
    let (upstream_headers, upstream_body) = &calls[0];
    assert_eq!(
        header_str(upstream_headers, "x-api-key"),
        Some("vendor-secret")
    );
    assert_eq!(
        header_str(upstream_headers, "anthropic-version"),
        Some("2023-06-01")
    );
    assert!(upstream_headers.get(header::AUTHORIZATION).is_none());
    assert_eq!(
        upstream_body,
        &prompt("MiniMax-M3"),
        "the body is passed through as is"
    );

    // The bearer form carries the same token.
    let (status, _, _) = send(
        &app,
        messages_request(
            "/anthropic/v1/messages",
            Some(("authorization", &format!("Bearer {}", hub.token))),
            &prompt("MiniMax-M3"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(vendor.calls().len(), 2);
}

#[tokio::test]
async fn usage_is_recorded_per_hub_and_day() {
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let demo = state.db().create_hub("demo").await.unwrap();
    let other = state.db().create_hub("other").await.unwrap();
    vendor
        .reply(StatusCode::OK, reply_json(10, 20))
        .reply(StatusCode::OK, reply_json(5, 7))
        .reply(StatusCode::OK, reply_json(100, 1));
    for token in [&demo.token, &demo.token, &other.token] {
        let (status, _, body) = send(
            &app,
            messages_request(
                "/anthropic/v1/messages",
                Some(("x-api-key", token)),
                &prompt("MiniMax-M3"),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    let (status, _, body) = send(
        &app,
        admin_request("GET", "/admin/usage?hub=demo", Some("admin-secret"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    assert_eq!(rows[0]["hub"], "demo");
    assert_eq!(rows[0]["day"], today);
    assert_eq!(rows[0]["requests"], 2);
    assert_eq!(rows[0]["input_tokens"], 15);
    assert_eq!(rows[0]["output_tokens"], 27);
    assert_eq!(
        rows[0]["cost_cents"], 0.0,
        "no price configured for the model"
    );

    let (_, _, all) = send(
        &app,
        admin_request("GET", "/admin/usage", Some("admin-secret"), None),
    )
    .await;
    assert_eq!(
        all.as_array().unwrap().len(),
        2,
        "one row per hub and day: {all}"
    );
}

#[tokio::test]
async fn admin_api_creates_hubs_once_and_requires_the_admin_token() {
    let (vendor, base) = fake_vendor().await;
    let (app, _) = gateway(config(&base)).await;
    let create = || {
        admin_request(
            "POST",
            "/admin/hubs",
            Some("admin-secret"),
            Some(json!({"name": "jacob"})),
        )
    };

    let (status, _, _) = send(
        &app,
        admin_request("POST", "/admin/hubs", None, Some(json!({"name": "jacob"}))),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(
        &app,
        admin_request(
            "POST",
            "/admin/hubs",
            Some("wrong"),
            Some(json!({"name": "jacob"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, body) = send(&app, create()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["name"], "jacob");
    let token = body["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));

    let (status, _, body) = send(&app, create()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let (status, _, body) = send(
        &app,
        admin_request(
            "POST",
            "/admin/hubs",
            Some("admin-secret"),
            Some(json!({"name": "Bad Name"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, _, body) = send(
        &app,
        admin_request("GET", "/admin/hubs", Some("admin-secret"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let hubs = body.as_array().unwrap();
    assert_eq!(hubs.len(), 1);
    assert_eq!(hubs[0]["name"], "jacob");
    assert_eq!(hubs[0]["status"], "active");
    assert!(hubs[0].get("token").is_none() && hubs[0].get("token_hash").is_none());

    let (status, _, body) = send(
        &app,
        messages_request(
            "/anthropic/v1/messages",
            Some(("x-api-key", &token)),
            &prompt("MiniMax-M3"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(vendor.calls().len(), 1);
}

#[tokio::test]
async fn admin_api_is_disabled_without_a_configured_admin_token() {
    let (_, base) = fake_vendor().await;
    let mut cfg = config(&base);
    cfg.admin_token = None;
    let (app, _) = gateway(cfg).await;
    let (status, _, body) = send(
        &app,
        admin_request(
            "POST",
            "/admin/hubs",
            Some("anything"),
            Some(json!({"name": "jacob"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("GATEWAY_ADMIN_TOKEN")
    );
}

#[tokio::test]
async fn streaming_requests_are_refused_in_v1() {
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    let mut body = prompt("MiniMax-M3");
    body["stream"] = json!(true);
    let (status, _, reply) = send(
        &app,
        messages_request(
            "/anthropic/v1/messages",
            Some(("x-api-key", &hub.token)),
            &body,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{reply}");
    assert!(vendor.calls().is_empty());
}

#[tokio::test]
async fn vendor_errors_pass_through_after_one_retry_and_record_no_usage() {
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    let limit =
        json!({"type": "error", "error": {"type": "rate_limit_error", "message": "slow down"}});
    vendor
        .reply(StatusCode::TOO_MANY_REQUESTS, limit.clone())
        .reply(StatusCode::TOO_MANY_REQUESTS, limit.clone());

    let (status, _, body) = send(
        &app,
        messages_request(
            "/anthropic/v1/messages",
            Some(("x-api-key", &hub.token)),
            &prompt("MiniMax-M3"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body, limit, "the vendor's error body is passed through");
    assert_eq!(vendor.calls().len(), 2, "one retry on 429");

    let (_, _, usage) = send(
        &app,
        admin_request("GET", "/admin/usage?hub=demo", Some("admin-secret"), None),
    )
    .await;
    assert_eq!(usage, json!([]), "failed calls record no usage");
}

#[tokio::test]
async fn server_errors_are_retried_once_then_succeed() {
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    vendor
        .reply(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "busy"}}),
        )
        .reply(StatusCode::OK, reply_json(3, 4));

    let (status, _, body) = send(
        &app,
        messages_request(
            "/anthropic/v1/messages",
            Some(("x-api-key", &hub.token)),
            &prompt("MiniMax-M3"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, reply_json(3, 4));
    assert_eq!(vendor.calls().len(), 2);
}

#[tokio::test]
async fn vendor_is_chosen_by_path_prefix_or_model_name() {
    let (minimax, minimax_base) = fake_vendor().await;
    let (anthropic, anthropic_base) = fake_vendor().await;
    let mut cfg = config(&minimax_base);
    cfg.vendors.push(Vendor::new(
        "anthropic",
        &anthropic_base,
        Some("anthropic-secret"),
    ));
    let (app, state) = gateway(cfg).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    let token = Some(("x-api-key", hub.token.as_str()));

    let (status, _, _) = send(
        &app,
        messages_request("/anthropic/v1/messages", token, &prompt("claude-haiku-4-5")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(anthropic.calls().len(), 1);
    assert_eq!(
        header_str(&anthropic.calls()[0].0, "x-api-key"),
        Some("anthropic-secret")
    );

    let (status, _, _) = send(
        &app,
        messages_request("/anthropic/v1/messages", token, &prompt("MiniMax-M3")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        minimax.calls().len(),
        1,
        "MiniMax models on the /anthropic prefix go to MiniMax"
    );

    let (status, _, _) = send(
        &app,
        messages_request("/minimax/v1/messages", token, &prompt("MiniMax-M2.5")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(minimax.calls().len(), 2);

    let (status, _, body) = send(
        &app,
        messages_request("/openai/v1/messages", token, &prompt("gpt-5-mini")),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["type"], "not_found_error");
}

#[tokio::test]
async fn an_unconfigured_vendor_answers_503_and_bad_bodies_400() {
    let (vendor, base) = fake_vendor().await;
    let mut cfg = config(&base);
    cfg.vendors[0].api_key = None;
    let (app, state) = gateway(cfg).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    let token = Some(("x-api-key", hub.token.as_str()));

    let (status, _, body) = send(
        &app,
        messages_request("/anthropic/v1/messages", token, &prompt("MiniMax-M3")),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("minimax")
    );

    let (status, _, body) = send(
        &app,
        messages_request("/anthropic/v1/messages", token, &json!({"max_tokens": 5})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["type"], "invalid_request_error");

    let req = Request::post("/anthropic/v1/messages")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-api-key", &hub.token)
        .body(Body::from("not json"))
        .unwrap();
    let (status, _, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(vendor.calls().is_empty());
}
