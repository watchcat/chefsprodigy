//! llm-gateway: Messages-format passthrough with hub tokens and usage accounting (v1, RN-66)
//! plus quotas, the canonical-JSON reply cache, BYOK and the admin API (v2, RN-68);
//! docs/SPEC-hosted-platform.md section 5.2.

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
/// (default: 200 with a one-block reply and usage 1/2), optionally after a delay.
#[derive(Clone, Default)]
struct FakeVendor {
    calls: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
    replies: Arc<Mutex<VecDeque<(StatusCode, Value)>>>,
    delay: Arc<Mutex<Duration>>,
}

impl FakeVendor {
    fn reply(&self, status: StatusCode, body: Value) -> &Self {
        self.replies.lock().unwrap().push_back((status, body));
        self
    }

    fn answer_after(&self, delay: Duration) {
        *self.delay.lock().unwrap() = delay;
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
    let delay = *vendor.delay.lock().unwrap();
    tokio::time::sleep(delay).await;
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

/// A request that bypasses the reply cache (temperature 0.5), so tests counting
/// vendor calls or quota see every request; the cache tests set temperature 0.
fn prompt(model: &str) -> Value {
    json!({
        "model": model, "max_tokens": 256, "temperature": 0.5,
        "messages": [{"role": "user", "content": "Normalize: 2 eggs"}]
    })
}

fn deterministic(model: &str) -> Value {
    let mut body = prompt(model);
    body["temperature"] = json!(0);
    body
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

#[tokio::test]
async fn cached_prompt_tokens_count_as_input() {
    // MiniMax and Anthropic report prompt-cache reads and writes outside `input_tokens`;
    // seen with MiniMax-M3: input_tokens 697 plus cache_read_input_tokens 128.
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let demo = state.db().create_hub("demo").await.unwrap();
    vendor.reply(
        StatusCode::OK,
        json!({
            "id": "msg_1", "type": "message", "role": "assistant",
            "content": [{"type": "text", "text": "ok"}],
            "usage": {"input_tokens": 697, "output_tokens": 1,
                      "cache_creation_input_tokens": 50, "cache_read_input_tokens": 128}
        }),
    );
    let (status, _, body) = send(
        &app,
        messages_request(
            "/anthropic/v1/messages",
            Some(("x-api-key", &demo.token)),
            &prompt("MiniMax-M3"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["usage"]["input_tokens"], 697,
        "the reply reaches the hub unchanged"
    );
    let rows = state.db().usage(Some("demo"), None, None).await.unwrap();
    assert_eq!(rows[0].input_tokens, 697 + 50 + 128);
    assert_eq!(rows[0].output_tokens, 1);
}

// ---------------------------------------------------------------- v2 (RN-68): quotas

/// Set quota fields on a hub through the admin API.
async fn patch_hub(app: &Router, name: &str, patch: Value) -> (StatusCode, Value) {
    let (status, _, body) = send(
        app,
        admin_request(
            "PATCH",
            &format!("/admin/hubs/{name}"),
            Some("admin-secret"),
            Some(patch),
        ),
    )
    .await;
    (status, body)
}

async fn call_as(app: &Router, token: &str, body: &Value) -> (StatusCode, HeaderMap, Value) {
    send(
        app,
        messages_request("/anthropic/v1/messages", Some(("x-api-key", token)), body),
    )
    .await
}

/// Upsert a model price (cents per million tokens) through the admin API.
async fn set_price(
    app: &Router,
    vendor: &str,
    model: &str,
    input: f64,
    output: f64,
) -> (StatusCode, Value) {
    let (status, _, body) = send(
        app,
        admin_request(
            "PUT",
            "/admin/prices",
            Some("admin-secret"),
            Some(json!({
                "vendor": vendor, "model": model,
                "input_cents_per_mtok": input, "output_cents_per_mtok": output
            })),
        ),
    )
    .await;
    (status, body)
}

fn assert_quota_exceeded(status: StatusCode, body: &Value, needle: &str) {
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "quota_exceeded", "{body}");
    let mut keys: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["error", "type"], "the envelope RN-67 maps: {body}");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(needle),
        "{message:?} should mention {needle:?}"
    );
}

#[tokio::test]
async fn over_quota_requests_get_429_before_any_upstream_call() {
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    let (status, body) = patch_hub(
        &app,
        "demo",
        json!({"onboarding_days": 0, "daily_request_limit": 2}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    for _ in 0..2 {
        let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
    assert_quota_exceeded(status, &body, "daily limit of 2");
    assert_eq!(
        vendor.calls().len(),
        2,
        "the refused request never reached the vendor"
    );
}

#[tokio::test]
async fn onboarding_days_raise_the_daily_limit_for_new_hubs() {
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    patch_hub(
        &app,
        "demo",
        json!({"onboarding_daily_requests": 3, "onboarding_days": 3, "daily_request_limit": 1}),
    )
    .await;
    for _ in 0..3 {
        let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
    assert_quota_exceeded(status, &body, "daily limit of 3");
    assert_eq!(vendor.calls().len(), 3);
}

#[tokio::test]
async fn monthly_request_limit_and_cost_cap_apply() {
    let (_vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    patch_hub(
        &app,
        "demo",
        json!({"onboarding_days": 0, "daily_request_limit": 10, "monthly_request_limit": 1}),
    )
    .await;
    let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
    assert_quota_exceeded(status, &body, "monthly limit of 1");

    // Cost cap: price the model so every default reply (1 in, 2 out tokens) costs 30 cents.
    let costly = state.db().create_hub("costly").await.unwrap();
    patch_hub(
        &app,
        "costly",
        json!({"onboarding_days": 0, "daily_request_limit": 10, "monthly_request_limit": 100, "monthly_cost_cap_cents": 100}),
    )
    .await;
    let (status, body) = set_price(&app, "minimax", "MiniMax-M3", 10_000_000.0, 10_000_000.0).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for _ in 0..4 {
        let (status, _, body) = call_as(&app, &costly.token, &prompt("MiniMax-M3")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, _, body) = call_as(&app, &costly.token, &prompt("MiniMax-M3")).await;
    assert_quota_exceeded(status, &body, "cost cap");
}

#[tokio::test]
async fn daily_token_limit_is_a_secondary_cap() {
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    patch_hub(
        &app,
        "demo",
        json!({"onboarding_days": 0, "daily_request_limit": 10, "daily_token_limit": 25}),
    )
    .await;
    vendor.reply(StatusCode::OK, reply_json(10, 20));
    let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
    assert_quota_exceeded(status, &body, "token");
}

// ---------------------------------------------------------------- v2: BYOK

fn config_with_master(minimax_base: &str) -> Config {
    let mut cfg = config(minimax_base);
    cfg.master_key = Some("master-secret".into());
    cfg
}

#[tokio::test]
async fn byok_hubs_use_their_own_key_and_skip_the_platform_quota() {
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config_with_master(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    patch_hub(
        &app,
        "demo",
        json!({"onboarding_days": 0, "daily_request_limit": 1}),
    )
    .await;
    let (status, _, body) = send(
        &app,
        admin_request(
            "POST",
            "/admin/hubs/demo/byok",
            Some("admin-secret"),
            Some(json!({"vendor": "minimax", "key": "hub-own-key"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, _, body) = send(
        &app,
        admin_request(
            "POST",
            "/admin/hubs/demo/byok",
            Some("admin-secret"),
            Some(json!({"vendor": "openai", "key": "k"})),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "only vendors the gateway serves: {body}"
    );

    for _ in 0..3 {
        let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let calls = vendor.calls();
    assert_eq!(calls.len(), 3);
    assert!(
        calls
            .iter()
            .all(|(h, _)| header_str(h, "x-api-key") == Some("hub-own-key"))
    );

    let (_, _, usage) = send(
        &app,
        admin_request("GET", "/admin/usage?hub=demo", Some("admin-secret"), None),
    )
    .await;
    assert_eq!(
        usage[0]["requests"], 3,
        "usage is still recorded for BYOK hubs"
    );

    let (_, _, hubs) = send(
        &app,
        admin_request("GET", "/admin/hubs", Some("admin-secret"), None),
    )
    .await;
    let demo = hubs
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == "demo")
        .unwrap();
    assert_eq!(demo["byok_vendors"], json!(["minimax"]));
    assert!(
        demo.to_string().find("hub-own-key").is_none(),
        "keys never leave the gateway"
    );

    let (status, _, _) = send(
        &app,
        admin_request(
            "DELETE",
            "/admin/hubs/demo/byok/minimax",
            Some("admin-secret"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
    assert_quota_exceeded(status, &body, "daily limit of 1");
}

#[tokio::test]
async fn byok_keys_under_a_replaced_master_key_fall_back_and_can_be_cleared() {
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config_with_master(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    let byok = |vendor_name: &str, key: &str| {
        admin_request(
            "POST",
            "/admin/hubs/demo/byok",
            Some("admin-secret"),
            Some(json!({"vendor": vendor_name, "key": key})),
        )
    };
    let (status, _, _) = send(&app, byok("minimax", "old-own-key")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let mut cfg = config_with_master(&base);
    cfg.master_key = Some("a-different-master".into());
    let app = build_router(AppState::new(state.db().clone(), cfg));
    let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        header_str(&vendor.calls()[0].0, "x-api-key"),
        Some("vendor-secret"),
        "unreadable BYOK keys fall back to the platform key"
    );

    let (status, _, body) = send(&app, byok("minimax", "new-own-key")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("DELETE"),
        "{body}"
    );
    let (status, _, _) = send(
        &app,
        admin_request(
            "DELETE",
            "/admin/hubs/demo/byok/minimax",
            Some("admin-secret"),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "unreadable keys can be cleared"
    );
    let (status, _, _) = send(&app, byok("minimax", "new-own-key")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
    assert_eq!(
        header_str(&vendor.calls()[1].0, "x-api-key"),
        Some("new-own-key")
    );
}

#[tokio::test]
async fn byok_needs_the_master_key() {
    let (_vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    state.db().create_hub("demo").await.unwrap();
    let (status, _, body) = send(
        &app,
        admin_request(
            "POST",
            "/admin/hubs/demo/byok",
            Some("admin-secret"),
            Some(json!({"vendor": "minimax", "key": "k"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("GATEWAY_MASTER_KEY")
    );
}

// ---------------------------------------------------------------- v2: cache

#[tokio::test]
async fn identical_requests_with_reordered_keys_hit_the_cache() {
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let a = state.db().create_hub("a").await.unwrap();
    let b = state.db().create_hub("b").await.unwrap();

    let ordered = r#"{"model":"MiniMax-M3","max_tokens":256,"temperature":0,"messages":[{"role":"user","content":"Label: salt, garlic"}]}"#;
    let reordered = r#"{"messages":[{"content":"Label: salt, garlic","role":"user"}],"temperature":0.0,"max_tokens":256,"model":"MiniMax-M3"}"#;
    let raw = |token: &str, body: &str| {
        Request::post("/anthropic/v1/messages")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-api-key", token)
            .body(Body::from(body.to_string()))
            .unwrap()
    };

    let (status, headers, first) = send(&app, raw(&a.token, ordered)).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(header_str(&headers, "x-gateway-cache"), Some("miss"));

    let (status, headers, second) = send(&app, raw(&b.token, reordered)).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(
        header_str(&headers, "x-gateway-cache"),
        Some("hit"),
        "another hub, reordered keys, 0.0 vs 0"
    );
    assert_eq!(second, first, "the cached reply is byte-identical");
    assert_eq!(vendor.calls().len(), 1, "the vendor was called once");

    let (_, _, usage) = send(
        &app,
        admin_request("GET", "/admin/usage", Some("admin-secret"), None),
    )
    .await;
    assert_eq!(
        usage.as_array().unwrap().len(),
        1,
        "a hit records no usage: {usage}"
    );
    let (status, _, stats) = send(
        &app,
        admin_request("GET", "/admin/cache/stats", Some("admin-secret"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    assert_eq!(stats["entries"], 1);
    assert_eq!(stats["hits"], 1);
}

#[tokio::test]
async fn only_deterministic_successful_replies_are_cached() {
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();

    let mut creative = deterministic("MiniMax-M3");
    creative["temperature"] = json!(0.7);
    for _ in 0..2 {
        let (status, headers, _) = call_as(&app, &hub.token, &creative).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(header_str(&headers, "x-gateway-cache"), Some("bypass"));
    }
    assert_eq!(vendor.calls().len(), 2, "temperature 0.7 is never cached");

    vendor
        .reply(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"type": "error", "error": {"type": "api_error", "message": "boom"}}),
        )
        .reply(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"type": "error", "error": {"type": "api_error", "message": "boom"}}),
        );
    let (status, _, _) = call_as(&app, &hub.token, &deterministic("MiniMax-M3")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let (status, headers, _) = call_as(&app, &hub.token, &deterministic("MiniMax-M3")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, "x-gateway-cache"),
        Some("miss"),
        "errors are never cached"
    );
}

#[tokio::test]
async fn cache_entries_expire_and_the_table_is_bounded() {
    let (vendor, base) = fake_vendor().await;
    let mut cfg = config(&base);
    cfg.cache_ttl = Duration::ZERO;
    let (app, state) = gateway(cfg).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    for _ in 0..2 {
        let (_, headers, _) = call_as(&app, &hub.token, &deterministic("MiniMax-M3")).await;
        assert_eq!(
            header_str(&headers, "x-gateway-cache"),
            Some("miss"),
            "a zero TTL expires every entry"
        );
    }
    assert_eq!(vendor.calls().len(), 2);
    let (_, _, stats) = send(
        &app,
        admin_request("GET", "/admin/cache/stats", Some("admin-secret"), None),
    )
    .await;
    assert_eq!(stats["entries"], 0, "expired entries are purged: {stats}");

    let (vendor, base) = fake_vendor().await;
    let mut cfg = config(&base);
    cfg.cache_max_entries = 2;
    let (app, state) = gateway(cfg).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    for i in 0..3 {
        let mut body = deterministic("MiniMax-M3");
        body["messages"][0]["content"] = json!(format!("Label batch {i}"));
        call_as(&app, &hub.token, &body).await;
    }
    assert_eq!(vendor.calls().len(), 3);
    let (_, _, stats) = send(
        &app,
        admin_request("GET", "/admin/cache/stats", Some("admin-secret"), None),
    )
    .await;
    assert_eq!(stats["entries"], 2, "oldest entry evicted: {stats}");
}

// ---------------------------------------------------------------- v2: admin

#[tokio::test]
async fn admin_can_update_disable_and_delete_hubs() {
    let (vendor, base) = fake_vendor().await;
    let (app, _) = gateway(config(&base)).await;
    let (status, _, created) = send(
        &app,
        admin_request(
            "POST",
            "/admin/hubs",
            Some("admin-secret"),
            Some(json!({"name": "jacob", "quota": {"daily_request_limit": 42}})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert!(created["created_at"].is_string() && created["name"] == "jacob");
    assert_eq!(created["quota"]["daily_request_limit"], 42);
    assert_eq!(
        created["quota"]["monthly_request_limit"], 150,
        "other fields keep the free-tier defaults"
    );
    let token = created["token"].as_str().unwrap().to_string();
    let (status, _, body) = call_as(&app, &token, &prompt("MiniMax-M3")).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = patch_hub(&app, "jacob", json!({"monthly_request_limit": 7})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["quota"]["monthly_request_limit"], 7);
    assert_eq!(
        body["quota"]["daily_request_limit"], 42,
        "a patch leaves the other fields alone"
    );

    let (status, body) = patch_hub(&app, "jacob", json!({"status": "disabled"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "disabled");
    let (status, _, _) = call_as(&app, &token, &prompt("MiniMax-M3")).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a disabled hub's token is refused"
    );
    assert_eq!(vendor.calls().len(), 1);

    let (status, body) = patch_hub(&app, "jacob", json!({"status": "active"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = call_as(&app, &token, &prompt("MiniMax-M3")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "re-activated with the same token: {body}"
    );

    let (status, body) = patch_hub(&app, "jacob", json!({"status": "sleeping"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = patch_hub(&app, "nobody", json!({"status": "active"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _, _) = send(
        &app,
        admin_request("DELETE", "/admin/hubs/jacob", Some("admin-secret"), None),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, _, usage) = send(
        &app,
        admin_request("GET", "/admin/usage", Some("admin-secret"), None),
    )
    .await;
    assert_eq!(usage, json!([]), "the hub's usage goes with it");
    let (status, _, _) = send(
        &app,
        admin_request("DELETE", "/admin/hubs/jacob", Some("admin-secret"), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, _, hubs) = send(
        &app,
        admin_request("GET", "/admin/hubs", Some("admin-secret"), None),
    )
    .await;
    assert_eq!(hubs, json!([]));
}

// ---------------------------------------------------------------- v2 (k3s edition) additions

#[tokio::test]
async fn cache_hits_are_free_and_served_even_over_quota() {
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    patch_hub(
        &app,
        "demo",
        json!({"onboarding_days": 0, "daily_request_limit": 1}),
    )
    .await;

    let (status, headers, first) = call_as(&app, &hub.token, &deterministic("MiniMax-M3")).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(header_str(&headers, "x-gateway-cache"), Some("miss"));
    let (status, headers, again) = call_as(&app, &hub.token, &deterministic("MiniMax-M3")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a hit costs nothing, so the used-up quota does not apply: {again}"
    );
    assert_eq!(header_str(&headers, "x-gateway-cache"), Some("hit"));
    assert_eq!(again, first);

    let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
    assert_quota_exceeded(status, &body, "daily limit of 1");
    assert_eq!(vendor.calls().len(), 1);
    let rows = state.db().usage(Some("demo"), None, None).await.unwrap();
    assert_eq!(rows[0].requests, 1, "only the vendor call is counted");
}

#[tokio::test]
async fn concurrent_requests_cannot_overshoot_the_daily_limit() {
    let (vendor, base) = fake_vendor().await;
    vendor.answer_after(Duration::from_millis(300));
    let (app, state) = gateway(config(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    patch_hub(
        &app,
        "demo",
        json!({"onboarding_days": 0, "daily_request_limit": 1}),
    )
    .await;

    let body = prompt("MiniMax-M3");
    let (a, b) = tokio::join!(
        call_as(&app, &hub.token, &body),
        call_as(&app, &hub.token, &body)
    );
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS],
        "the in-flight request holds the last slot: {} / {}",
        a.2,
        b.2
    );
    assert_eq!(vendor.calls().len(), 1);
}

#[tokio::test]
async fn onboarding_ends_and_earlier_periods_do_not_count() {
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    patch_hub(
        &app,
        "demo",
        json!({"onboarding_daily_requests": 30, "onboarding_days": 3, "daily_request_limit": 1, "monthly_request_limit": 2}),
    )
    .await;
    let ten_days_ago = (chrono::Utc::now() - chrono::Duration::days(10))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    sqlx::query("UPDATE hubs SET created_at = ? WHERE name = 'demo'")
        .bind(&ten_days_ago)
        .execute(state.db().pool())
        .await
        .unwrap();
    let id = state.db().list_hubs().await.unwrap()[0].id;
    for _ in 0..5 {
        state
            .db()
            .record_usage(id, "2000-01-15", 10, 10, 0.0)
            .await
            .unwrap();
    }

    let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "January 2000 counts neither today nor this month: {body}"
    );
    let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
    assert_quota_exceeded(status, &body, "daily limit of 1");
    assert_eq!(vendor.calls().len(), 1);
}

#[tokio::test]
async fn token_and_cost_caps_count_the_vendors_prompt_cache_tokens() {
    // MiniMax reports cached prompt tokens outside `input_tokens` (RN-66); the caps see the total.
    let cached_reply = json!({
        "id": "msg_1", "type": "message", "role": "assistant",
        "content": [{"type": "text", "text": "ok"}],
        "usage": {"input_tokens": 10, "output_tokens": 0,
                  "cache_creation_input_tokens": 0, "cache_read_input_tokens": 90}
    });
    let (vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let hub = state.db().create_hub("demo").await.unwrap();
    patch_hub(
        &app,
        "demo",
        json!({"onboarding_days": 0, "daily_token_limit": 100}),
    )
    .await;
    vendor.reply(StatusCode::OK, cached_reply.clone());
    let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, body) = call_as(&app, &hub.token, &prompt("MiniMax-M3")).await;
    assert_quota_exceeded(status, &body, "daily token limit of 100");

    // One cent per input token: 10 + 90 tokens reach the 100-cent cap.
    let costly = state.db().create_hub("costly").await.unwrap();
    patch_hub(
        &app,
        "costly",
        json!({"onboarding_days": 0, "monthly_cost_cap_cents": 100}),
    )
    .await;
    let (status, body) = set_price(&app, "minimax", "MiniMax-M3", 1_000_000.0, 0.0).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    vendor.reply(StatusCode::OK, cached_reply);
    let (status, _, body) = call_as(&app, &costly.token, &prompt("MiniMax-M3")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = state.db().usage(Some("costly"), None, None).await.unwrap();
    assert_eq!(rows[0].cost_cents, 100.0);
    let (status, _, body) = call_as(&app, &costly.token, &prompt("MiniMax-M3")).await;
    assert_quota_exceeded(status, &body, "cost cap of 100 cents");
    assert_eq!(vendor.calls().len(), 2);
}

#[tokio::test]
async fn prices_are_managed_through_the_admin_api() {
    let (_vendor, base) = fake_vendor().await;
    let (app, _) = gateway(config(&base)).await;
    let (status, body) = set_price(&app, "minimax", "MiniMax-M3", 30.0, 120.0).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = set_price(&app, "minimax", "MiniMax-M3", 40.0, 160.0).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _, prices) = send(
        &app,
        admin_request("GET", "/admin/prices", Some("admin-secret"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{prices}");
    assert_eq!(
        prices,
        json!([{"vendor": "minimax", "model": "MiniMax-M3",
                "input_cents_per_mtok": 40.0, "output_cents_per_mtok": 160.0}]),
        "a second PUT replaces the price"
    );

    let (status, body) = set_price(&app, "openai", "gpt-5-mini", 1.0, 1.0).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown vendor: {body}");
    let (status, body) = set_price(&app, "minimax", "MiniMax-M3", -1.0, 1.0).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "negative price: {body}");
    let (status, _, _) = send(&app, admin_request("GET", "/admin/prices", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn quota_values_are_validated() {
    let (_vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    state.db().create_hub("demo").await.unwrap();

    let (status, body) = patch_hub(&app, "demo", json!({"daily_request_limit": -1})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = patch_hub(&app, "demo", json!({"daily_limit": 50})).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a misspelt field is not ignored: {body}"
    );
    let (status, _, body) = send(
        &app,
        admin_request(
            "POST",
            "/admin/hubs",
            Some("admin-secret"),
            Some(json!({"name": "other", "quota": {"monthly_request_limit": -5}})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = patch_hub(&app, "demo", json!({"daily_token_limit": 500})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["quota"]["daily_token_limit"], 500);
    let (status, body) = patch_hub(&app, "demo", json!({"daily_token_limit": 0})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["quota"]["daily_token_limit"],
        Value::Null,
        "0 removes the token cap"
    );
    assert_eq!(
        body["quota"]["daily_request_limit"], 10,
        "untouched fields keep the free tier"
    );
}

#[tokio::test]
async fn rotating_or_disabling_a_hub_keeps_its_quota_and_onboarding_start() {
    let (_vendor, base) = fake_vendor().await;
    let (app, state) = gateway(config(&base)).await;
    let created = state.db().create_hub("demo").await.unwrap();
    patch_hub(&app, "demo", json!({"daily_request_limit": 42})).await;

    assert!(state.db().disable_hub("demo").await.unwrap());
    let hub = state.db().hub_by_name("demo").await.unwrap().unwrap();
    assert_eq!(hub.status, "disabled");
    assert_eq!(hub.quota.daily_request_limit, 42);

    let rotated = state.db().rotate_hub("demo").await.unwrap();
    assert_ne!(rotated.token, created.token);
    assert_eq!(
        rotated.created_at, created.created_at,
        "onboarding is not restarted by a new token"
    );
    let hub = state.db().hub_by_name("demo").await.unwrap().unwrap();
    assert_eq!(hub.status, "active");
    assert_eq!(hub.quota.daily_request_limit, 42);
    assert!(!state.db().disable_hub("ghost").await.unwrap());
}
