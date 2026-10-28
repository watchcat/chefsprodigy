//! Owner vs. public reader: with NODE_API_KEY configured, anonymous callers only read
//! public recipes, cannot write or import, and cannot trigger live model calls.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use http_body_util::BodyExt;
use node_server::{AppState, NodeInfo, build_router};
use recipe_core::IngredientItem;
use recipe_db::Db;
use recipe_scraper::llm::{
    ExtractedRecipeDraft, HeuristicExtractor, RecipeExtractor, TranslationReply, TranslationRequest,
};
use recipe_scraper::{RecipeImporter, ScraperError};
use serde_json::{Value, json};
use tower::ServiceExt;

static COUNTER: AtomicU32 = AtomicU32::new(0);
const KEY: &str = "owner-secret-123";

struct CountingTranslator {
    calls: AtomicUsize,
}

impl RecipeExtractor for CountingTranslator {
    fn normalize_ingredients<'a>(
        &'a self,
        _r: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IngredientItem>, ScraperError>> + Send + 'a>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn extract_recipe<'a>(
        &'a self,
        _m: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<ExtractedRecipeDraft, ScraperError>> + Send + 'a>> {
        Box::pin(async { Err(ScraperError::LlmUnavailable) })
    }
    fn translate_recipe<'a>(
        &'a self,
        req: &'a TranslationRequest,
    ) -> Pin<Box<dyn Future<Output = Result<TranslationReply, ScraperError>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(TranslationReply {
                title: format!("[{}] {}", req.target_lang, req.title),
                description: req.description.clone(),
                ingredients: req
                    .ingredients
                    .iter()
                    .map(|s| format!("[{}] {s}", req.target_lang))
                    .collect(),
                instructions: req
                    .instructions
                    .iter()
                    .map(|s| format!("[{}] {s}", req.target_lang))
                    .collect(),
                tags: req.tags.clone(),
            })
        })
    }
    fn translate_terms<'a>(
        &'a self,
        ids: &'a [String],
        lang: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<std::collections::HashMap<String, String>, ScraperError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ids
                .iter()
                .map(|i| (i.clone(), format!("{lang}:{i}")))
                .collect())
        })
    }
}

async fn app_with(key: Option<&str>, extractor: Arc<dyn RecipeExtractor>) -> Router {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("node-auth-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(dir.join("recipes.db")).await.unwrap();
    let mut state = AppState::new(
        db,
        NodeInfo {
            author_did: None,
            chef_name: "T".into(),
        },
    )
    .with_importer(RecipeImporter::new(extractor))
    .with_image_dir(dir.join("images"));
    if let Some(k) = key {
        state = state.with_mcp_api_key(k);
    }
    build_router(state)
}

async fn req(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Value) {
    let mut r = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    let body = match body {
        Some(v) => {
            r = r.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(r.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
        },
    )
}

fn bearer() -> (&'static str, String) {
    ("authorization", format!("Bearer {KEY}"))
}

fn recipe(title: &str, public: bool) -> Value {
    json!({"title": title, "language": "nl", "servings": 2, "is_public": public,
        "ingredients": [
            {"raw_text": "1 ei", "metric_amount": 1.0, "unit": "pcs", "canonical_id": "egg", "is_specialty": false},
            {"raw_text": "1 el gochujang", "metric_amount": 15.0, "unit": "ml", "canonical_id": "gochujang", "is_specialty": true}
        ],
        "instructions": [{"step_number": 1, "text": "Bak.", "timer_seconds": null}], "tags": ["snel"]})
}

async fn seed(app: &Router) -> (String, String) {
    let b = bearer();
    let (s, _, pub_r) = req(
        app,
        "POST",
        "/api/v1/recipes",
        Some(recipe("Public dish", true)),
        &[(b.0, &b.1)],
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{pub_r}");
    let (s, _, priv_r) = req(
        app,
        "POST",
        "/api/v1/recipes",
        Some(recipe("Private dish", false)),
        &[(b.0, &b.1)],
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{priv_r}");
    (
        pub_r["id"].as_str().unwrap().to_string(),
        priv_r["id"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn anonymous_readers_only_see_public_recipes() {
    let app = app_with(Some(KEY), Arc::new(HeuristicExtractor)).await;
    let (public_id, private_id) = seed(&app).await;

    let (s, _, list) = req(&app, "GET", "/api/v1/recipes", None, &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], public_id);
    let (_, _, q) = req(&app, "GET", "/api/v1/recipes?q=dish", None, &[]).await;
    assert_eq!(q["total"], 1, "search and count respect visibility");

    let (s, _, _) = req(
        &app,
        "GET",
        &format!("/api/v1/recipes/{public_id}"),
        None,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, body) = req(
        &app,
        "GET",
        &format!("/api/v1/recipes/{private_id}"),
        None,
        &[],
    )
    .await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "private recipes do not exist for anonymous callers: {body}"
    );

    let b = bearer();
    let (_, _, all) = req(&app, "GET", "/api/v1/recipes", None, &[(b.0, &b.1)]).await;
    assert_eq!(all["total"], 2, "the owner sees everything");
    let (s, _, _) = req(
        &app,
        "GET",
        &format!("/api/v1/recipes/{private_id}"),
        None,
        &[(b.0, &b.1)],
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    let (s, _, sl) = req(&app, "POST", "/api/v1/shopping-list", Some(json!({"items": [{"recipe_id": public_id, "servings": 2}, {"recipe_id": private_id, "servings": 2}]})), &[]).await;
    assert_eq!(s, StatusCode::OK, "{sl}");
    assert_eq!(
        sl["missing_recipes"],
        json!([private_id]),
        "private recipes are treated as missing"
    );
    assert_eq!(sl["basic_groceries"][0]["total_amount"], 1.0);
}

#[tokio::test]
async fn anonymous_callers_cannot_write_or_import() {
    let app = app_with(Some(KEY), Arc::new(HeuristicExtractor)).await;
    let (public_id, _) = seed(&app).await;
    let attempts: Vec<(&str, String, Option<Value>)> = vec![
        ("POST", "/api/v1/recipes".into(), Some(recipe("x", true))),
        (
            "PUT",
            format!("/api/v1/recipes/{public_id}"),
            Some(recipe("x", true)),
        ),
        ("DELETE", format!("/api/v1/recipes/{public_id}"), None),
        (
            "PUT",
            format!("/api/v1/recipes/{public_id}/translations/ru"),
            Some(json!({"title": "x"})),
        ),
        (
            "DELETE",
            format!("/api/v1/recipes/{public_id}/translations/ru"),
            None,
        ),
        (
            "POST",
            "/api/v1/import/url".into(),
            Some(json!({"url": "https://example.com"})),
        ),
        (
            "POST",
            "/api/v1/import/text".into(),
            Some(json!({"text": "x"})),
        ),
    ];
    for (m, uri, body) in attempts {
        let (s, _, b) = req(&app, m, &uri, body, &[]).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{m} {uri}: {b}");
        assert!(b["error"].is_string(), "{m} {uri}: json error expected");
    }
    // multipart endpoints too
    for uri in [
        "/api/v1/import/mealie-backup",
        "/api/v1/import/photo",
        &format!("/api/v1/recipes/{public_id}/image"),
    ] {
        let method = if uri.ends_with("/image") {
            "PUT"
        } else {
            "POST"
        };
        let r = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "multipart/form-data; boundary=x")
            .body(Body::from("--x--\r\n"))
            .unwrap();
        let res = app.clone().oneshot(r).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
    let (_, _, still) = req(&app, "GET", "/api/v1/recipes", None, &[]).await;
    assert_eq!(still["total"], 1, "nothing was changed");
}

#[tokio::test]
async fn anonymous_readers_get_cached_translations_but_never_trigger_the_model() {
    let fake = Arc::new(CountingTranslator {
        calls: AtomicUsize::new(0),
    });
    let app = app_with(Some(KEY), fake.clone()).await;
    let (public_id, _) = seed(&app).await;

    let (s, _, r) = req(
        &app,
        "GET",
        &format!("/api/v1/recipes/{public_id}?lang=ru"),
        None,
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        r["title"], "Public dish",
        "no cached translation: source wording"
    );
    assert!(r["translation"].is_null());
    assert_eq!(
        r["ingredients"][0]["canonical_label"], "яйцо",
        "a seeded dictionary label counts as cached, so anonymous readers get it for free"
    );
    assert!(
        r["ingredients"][1].get("canonical_label").is_none(),
        "an uncached label is not fetched for anonymous readers"
    );
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        0,
        "anonymous calls must not spend model credits"
    );

    let b = bearer();
    let (_, _, owner) = req(
        &app,
        "GET",
        &format!("/api/v1/recipes/{public_id}?lang=ru"),
        None,
        &[(b.0, &b.1)],
    )
    .await;
    assert_eq!(owner["title"], "[ru] Public dish");
    let after_owner = fake.calls.load(Ordering::SeqCst);
    assert!(after_owner >= 1);

    let (_, _, cached) = req(
        &app,
        "GET",
        &format!("/api/v1/recipes/{public_id}?lang=ru"),
        None,
        &[],
    )
    .await;
    assert_eq!(
        cached["title"], "[ru] Public dish",
        "cached translation is served to everyone"
    );
    assert_eq!(
        cached["ingredients"][1]["canonical_label"], "ru:gochujang",
        "cached labels too"
    );
    assert_eq!(cached["ingredients"][0]["canonical_label"], "яйцо");
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        after_owner,
        "serving the cache made no model call"
    );
}

#[tokio::test]
async fn login_sets_a_session_cookie_that_grants_owner_access() {
    let app = app_with(Some(KEY), Arc::new(HeuristicExtractor)).await;

    let (s, _, me) = req(&app, "GET", "/api/v1/auth/me", None, &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(me["authenticated"], false);

    let (s, _, bad) = req(
        &app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"api_key": "nope"})),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "{bad}");

    let (s, headers, ok) = req(
        &app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"api_key": KEY})),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{ok}");
    assert_eq!(ok["authenticated"], true);
    let set_cookie = headers
        .get(header::SET_COOKIE)
        .expect("Set-Cookie")
        .to_str()
        .unwrap()
        .to_string();
    assert!(set_cookie.starts_with("node_session="), "{set_cookie}");
    assert!(
        set_cookie.contains("HttpOnly")
            && set_cookie.contains("SameSite=Lax")
            && set_cookie.contains("Path=/"),
        "{set_cookie}"
    );
    assert!(
        !set_cookie.contains(KEY),
        "the raw key must not be stored in the cookie"
    );
    let cookie = set_cookie.split(';').next().unwrap().to_string();

    let (_, _, me) = req(&app, "GET", "/api/v1/auth/me", None, &[("cookie", &cookie)]).await;
    assert_eq!(me["authenticated"], true);
    let (s, _, created) = req(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe("Via cookie", false)),
        &[("cookie", &cookie)],
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let (s, _, _) = req(
        &app,
        "GET",
        &format!("/api/v1/recipes/{}", created["id"].as_str().unwrap()),
        None,
        &[("cookie", &cookie)],
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    let (_, _, _) = req(
        &app,
        "GET",
        "/api/v1/auth/me",
        None,
        &[("cookie", "node_session=forged")],
    )
    .await;
    let (s, _, _) = req(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe("Forged", false)),
        &[("cookie", "node_session=forged")],
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let (s, headers, _) = req(
        &app,
        "POST",
        "/api/v1/auth/logout",
        None,
        &[("cookie", &cookie)],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let cleared = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
}

#[tokio::test]
async fn bearer_header_grants_owner_access() {
    let app = app_with(Some(KEY), Arc::new(HeuristicExtractor)).await;
    let b = bearer();
    let (_, _, me) = req(&app, "GET", "/api/v1/auth/me", None, &[(b.0, &b.1)]).await;
    assert_eq!(me["authenticated"], true);
    let (s, _, _) = req(
        &app,
        "GET",
        "/api/v1/auth/me",
        None,
        &[("authorization", "Bearer wrong")],
    )
    .await;
    assert_eq!(s, StatusCode::OK, "me never fails, it reports");
    let (s, _, _) = req(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe("x", true)),
        &[("authorization", "Bearer wrong")],
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn without_a_configured_key_everyone_is_the_owner() {
    let app = app_with(None, Arc::new(HeuristicExtractor)).await;
    let (_, _, me) = req(&app, "GET", "/api/v1/auth/me", None, &[]).await;
    assert_eq!(me["authenticated"], true);
    let (s, _, r) = req(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe("Dev mode", false)),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    let (_, _, list) = req(&app, "GET", "/api/v1/recipes", None, &[]).await;
    assert_eq!(list["total"], 1, "private recipes visible in dev mode");
    let (s, _, _) = req(
        &app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"api_key": "anything"})),
        &[],
    )
    .await;
    assert_eq!(s, StatusCode::OK, "login is a no-op success without a key");
}
