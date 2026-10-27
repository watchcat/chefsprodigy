//! REST API tests driving the Axum router in-process against a real SQLite file.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use node_server::{AppState, NodeInfo, build_router};
use recipe_db::Db;
use serde_json::{Value, json};
use tower::ServiceExt;

static COUNTER: AtomicU32 = AtomicU32::new(0);

async fn app() -> Router {
    app_with(|state| state).await
}

/// An app whose state is adjusted by `configure` before the router is built.
async fn app_with(configure: impl FnOnce(AppState) -> AppState) -> Router {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("node-api-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(dir.join("recipes.db")).await.unwrap();
    let state = AppState::new(
        db,
        NodeInfo {
            author_did: Some("did:key:z6MkTest".to_string()),
            chef_name: "Test Chef".to_string(),
        },
    );
    build_router(configure(state))
}

async fn send(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(v) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, json)
}

fn recipe_json(title: &str, public: bool) -> Value {
    json!({
        "title": title,
        "servings": 2,
        "prep_time_minutes": 5,
        "cook_time_minutes": 10,
        "ingredients": [
            {"raw_text": "200g noodles", "metric_amount": 200.0, "unit": "g", "canonical_id": "noodles", "is_specialty": false},
            {"raw_text": "salt", "metric_amount": null, "unit": "to_taste", "canonical_id": "salt", "is_specialty": false}
        ],
        "instructions": [{"step_number": 1, "text": "Boil.", "timer_seconds": 300}],
        "tags": ["quick"],
        "is_public": public
    })
}

#[tokio::test]
async fn health_endpoint_reports_ok() {
    let app = app().await;
    let (status, body) = send(&app, "GET", "/api/v1/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn create_recipe_returns_201_with_id_and_hash() {
    let app = app().await;
    let (status, body) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe_json("Noodles", false)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["id"].as_str().unwrap().len(), 26);
    assert_eq!(body["content_hash"].as_str().unwrap().len(), 64);
    assert_eq!(body["title"], "Noodles");

    let id = body["id"].as_str().unwrap();
    let (status, fetched) = send(&app, "GET", &format!("/api/v1/recipes/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched, body);
}

#[tokio::test]
async fn create_recipe_rejects_invalid_json_with_4xx_json_error() {
    let app = app().await;
    let (status, body) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(json!({"servings": "two"})),
    )
    .await;
    assert!(status.is_client_error(), "got {status}");
    assert!(body["error"].is_string(), "error body must be JSON: {body}");
}

#[tokio::test]
async fn get_unknown_recipe_returns_404_json_error() {
    let app = app().await;
    let (status, body) = send(&app, "GET", "/api/v1/recipes/doesnotexist", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].is_string(), "{body}");
}

#[tokio::test]
async fn get_recipe_with_servings_scales_amounts() {
    let app = app().await;
    let (_, created) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe_json("Noodles", false)),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let (status, body) = send(
        &app,
        "GET",
        &format!("/api/v1/recipes/{id}?servings=4"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["servings"], 4);
    assert_eq!(body["ingredients"][0]["metric_amount"], 400.0);
    assert_eq!(body["ingredients"][1]["metric_amount"], Value::Null);
    assert_eq!(body["content_hash"], created["content_hash"]);
}

#[tokio::test]
async fn put_recipe_updates_and_recomputes_hash() {
    let app = app().await;
    let (_, created) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe_json("Noodles", false)),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let mut edited = recipe_json("Better Noodles", false);
    edited["ingredients"][0]["canonical_id"] = json!("rice_noodles");
    let (status, body) = send(&app, "PUT", &format!("/api/v1/recipes/{id}"), Some(edited)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], created["id"]);
    assert_eq!(body["title"], "Better Noodles");
    assert_ne!(body["content_hash"], created["content_hash"]);
    assert_eq!(body["created_at"], created["created_at"]);

    let (status, _) = send(
        &app,
        "PUT",
        "/api/v1/recipes/missing",
        Some(recipe_json("x", false)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_recipe_returns_204_then_404() {
    let app = app().await;
    let (_, created) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe_json("Noodles", false)),
    )
    .await;
    let id = created["id"].as_str().unwrap();
    let (status, _) = send(&app, "DELETE", &format!("/api/v1/recipes/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, "DELETE", &format!("/api/v1/recipes/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&app, "GET", &format!("/api/v1/recipes/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn list_recipes_supports_query_tag_and_ingredient_filters() {
    let app = app().await;
    send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe_json("Garlic Noodles", false)),
    )
    .await;
    let mut other = recipe_json("Tomato Soup", false);
    other["tags"] = json!(["soup"]);
    other["ingredients"] = json!([{"raw_text": "tomato", "metric_amount": 400.0, "unit": "g", "canonical_id": "tomato", "is_specialty": false}]);
    send(&app, "POST", "/api/v1/recipes", Some(other)).await;

    let (status, all) = send(&app, "GET", "/api/v1/recipes", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all["items"].as_array().unwrap().len(), 2);
    assert_eq!(all["limit"], 20);
    assert_eq!(all["offset"], 0);
    assert_eq!(all["total"], 2);

    let (_, by_q) = send(&app, "GET", "/api/v1/recipes?q=garlic", None).await;
    assert_eq!(by_q["items"].as_array().unwrap().len(), 1);
    assert_eq!(by_q["items"][0]["title"], "Garlic Noodles");
    assert_eq!(
        by_q["items"][0]["canonical_ingredients"],
        json!(["noodles", "salt"])
    );
    assert_eq!(by_q["items"][0]["total_time_minutes"], 15);

    let (_, by_tag) = send(&app, "GET", "/api/v1/recipes?tag=soup", None).await;
    assert_eq!(by_tag["items"].as_array().unwrap().len(), 1);
    assert_eq!(by_tag["items"][0]["title"], "Tomato Soup");

    let (_, by_ing) = send(&app, "GET", "/api/v1/recipes?ingredient=tomato", None).await;
    assert_eq!(by_ing["items"].as_array().unwrap().len(), 1);
    assert_eq!(by_ing["items"][0]["title"], "Tomato Soup");

    let (_, paged) = send(&app, "GET", "/api/v1/recipes?limit=1&offset=1", None).await;
    assert_eq!(paged["items"].as_array().unwrap().len(), 1);
    assert_eq!(paged["limit"], 1);
    assert_eq!(paged["offset"], 1);
    assert_eq!(paged["total"], 2, "total counts all matches, not the page");
    let (_, by_q) = send(&app, "GET", "/api/v1/recipes?q=garlic", None).await;
    assert_eq!(by_q["total"], 1);
}

#[tokio::test]
async fn federation_manifest_exposes_node_info_and_public_recipes_only() {
    let app = app().await;
    let (_, public) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe_json("Public", true)),
    )
    .await;
    send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe_json("Private", false)),
    )
    .await;

    let (status, body) = send(&app, "GET", "/api/v1/federation/manifest", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["author_did"], "did:key:z6MkTest");
    assert_eq!(body["chef_name"], "Test Chef");
    let recipes = body["recipes"].as_array().unwrap();
    assert_eq!(recipes.len(), 1);
    assert_eq!(recipes[0]["id"], public["id"]);
    assert_eq!(recipes[0]["content_hash"], public["content_hash"]);
    assert_eq!(recipes[0]["updated_at"], public["updated_at"]);
}

#[tokio::test]
async fn federation_recipe_endpoint_hides_private_recipes() {
    let app = app().await;
    let (_, public) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe_json("Public", true)),
    )
    .await;
    let (_, private) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe_json("Private", false)),
    )
    .await;

    let (status, body) = send(
        &app,
        "GET",
        &format!(
            "/api/v1/federation/recipes/{}",
            public["id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut body = body;
    assert_eq!(body["translations"], json!([]));
    body.as_object_mut().unwrap().remove("translations");
    assert_eq!(
        body, public,
        "the recipe fields stay flat on the federated object"
    );

    let (status, _) = send(
        &app,
        "GET",
        &format!(
            "/api/v1/federation/recipes/{}",
            private["id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn embedded_ui_is_served_at_root_and_as_spa_fallback() {
    let app = app().await;
    for path in ["/", "/recipes/some-client-route"] {
        let res = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{path}");
        let ct = res
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(ct.starts_with("text/html"), "{path}: {ct}");
        let html = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec())
            .unwrap();
        assert!(html.contains("<html") && html.contains("</html>"), "{path}");
    }
}

#[tokio::test]
async fn unknown_api_route_returns_404_json_not_html() {
    let app = app().await;
    let (status, body) = send(&app, "GET", "/api/v1/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].is_string(), "{body}");
}

#[tokio::test]
async fn shopping_list_endpoint_aggregates_and_validates() {
    let app = app().await;
    let (_, pho) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe_json("Pho", false)),
    )
    .await;
    let mut other = recipe_json("Omelette", false);
    other["ingredients"] = json!([
        {"raw_text": "3 eggs", "metric_amount": 3.0, "unit": "pcs", "canonical_id": "egg", "is_specialty": false},
        {"raw_text": "100g noodles", "metric_amount": 100.0, "unit": "g", "canonical_id": "noodles", "is_specialty": false},
        {"raw_text": "fish sauce", "metric_amount": 10.0, "unit": "ml", "canonical_id": "fish_sauce", "is_specialty": true}
    ]);
    let (_, om) = send(&app, "POST", "/api/v1/recipes", Some(other)).await;

    let body = json!({
        "items": [{"recipe_id": pho["id"], "servings": 4}, {"recipe_id": om["id"], "servings": 2}, {"recipe_id": "ghost", "servings": 1}],
        "pantry_exclude_canonical_ids": ["salt"]
    });
    let (status, out) = send(&app, "POST", "/api/v1/shopping-list", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let basic = out["basic_groceries"].as_array().unwrap();
    assert_eq!(
        basic
            .iter()
            .map(|l| l["canonical_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["egg", "noodles"]
    );
    assert_eq!(
        basic[1]["total_amount"], 500.0,
        "200g*2 (pho for 4 of 2) + 100g*1 (omelette for 2 of 2)"
    );
    assert_eq!(basic[1]["unit"], "g");
    assert_eq!(out["specialty_items"][0]["canonical_id"], "fish_sauce");
    assert_eq!(out["missing_recipes"], json!(["ghost"]));
    assert!(basic.iter().all(|l| l["canonical_id"] != "salt"));

    let (status, err) = send(
        &app,
        "POST",
        "/api/v1/shopping-list",
        Some(json!({"items": []})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
    assert!(err["error"].as_str().unwrap().contains("at least one"));
}

#[tokio::test]
async fn federation_manifest_exposes_node_url_when_configured() {
    let app = app().await;
    let (_, body) = send(&app, "GET", "/api/v1/federation/manifest", None).await;
    assert_eq!(body["node_url"], Value::Null);

    let app = app_with(|state| state.with_public_url("https://jacob.example.com/")).await;
    let (status, body) = send(&app, "GET", "/api/v1/federation/manifest", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["node_url"], "https://jacob.example.com");
}

#[tokio::test]
async fn federation_manifest_lists_translations_with_their_own_updated_at() {
    let app = app().await;
    let (_, recipe) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe_json("Public", true)),
    )
    .await;
    let id = recipe["id"].as_str().unwrap().to_string();
    let (_, before) = send(&app, "GET", "/api/v1/federation/manifest", None).await;
    assert_eq!(before["recipes"][0]["translations"], json!([]));

    let (status, translated) = send(
        &app,
        "PUT",
        &format!("/api/v1/recipes/{id}/translations/ru"),
        Some(json!({
            "title": "Лапша",
            "ingredients": ["200 г лапши", "соль"],
            "instructions": ["Варить."],
            "tags": ["быстро"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{translated}");

    let (_, after) = send(&app, "GET", "/api/v1/federation/manifest", None).await;
    let entry = &after["recipes"][0];
    assert_eq!(
        entry["updated_at"], recipe["updated_at"],
        "saving a translation leaves the recipe's updated_at alone"
    );
    assert_eq!(entry["content_hash"], recipe["content_hash"]);
    assert_eq!(
        entry["translations"],
        json!([{
            "lang": "ru",
            "origin": "human",
            "stale": false,
            "updated_at": translated["translation"]["translated_at"]
        }])
    );

    // Editing the source moves the recipe's timestamp and marks the translation stale,
    // while the translation keeps its own timestamp.
    send(
        &app,
        "PUT",
        &format!("/api/v1/recipes/{id}"),
        Some(recipe_json("Public v2", true)),
    )
    .await;
    let (_, edited) = send(&app, "GET", "/api/v1/federation/manifest", None).await;
    let entry = &edited["recipes"][0];
    assert_ne!(entry["updated_at"], recipe["updated_at"]);
    assert_eq!(entry["translations"][0]["stale"], true);
    assert_eq!(
        entry["translations"][0]["updated_at"],
        translated["translation"]["translated_at"]
    );
}

#[tokio::test]
async fn federation_recipe_carries_every_stored_translation() {
    let app = app().await;
    let (_, recipe) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe_json("Public", true)),
    )
    .await;
    let id = recipe["id"].as_str().unwrap().to_string();
    for (lang, title) in [("ru", "Лапша"), ("nl", "Noedels")] {
        let (status, body) = send(
            &app,
            "PUT",
            &format!("/api/v1/recipes/{id}/translations/{lang}"),
            Some(json!({"title": title})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    let (status, body) = send(
        &app,
        "GET",
        &format!("/api/v1/federation/recipes/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], recipe["id"]);
    assert_eq!(
        body["title"], "Public",
        "the recipe itself stays in its source language"
    );
    let translations = body["translations"].as_array().unwrap();
    let langs: Vec<&str> = translations
        .iter()
        .map(|t| t["lang"].as_str().unwrap())
        .collect();
    assert_eq!(langs, vec!["nl", "ru"]);
    assert_eq!(translations[1]["title"], "Лапша");
    assert_eq!(translations[1]["origin"], "human");
    assert_eq!(translations[1]["source_updated_at"], recipe["updated_at"]);
    assert!(translations[1]["updated_at"].is_string());
    assert!(translations[1]["created_at"].is_string());
}

#[tokio::test]
async fn federation_never_exposes_translations_of_private_recipes() {
    let app = app().await;
    let (_, private) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(recipe_json("Private", false)),
    )
    .await;
    let id = private["id"].as_str().unwrap();
    let (status, _) = send(
        &app,
        "PUT",
        &format!("/api/v1/recipes/{id}/translations/ru"),
        Some(json!({"title": "Секрет"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, manifest) = send(&app, "GET", "/api/v1/federation/manifest", None).await;
    assert_eq!(manifest["recipes"], json!([]));
    let (status, _) = send(
        &app,
        "GET",
        &format!("/api/v1/federation/recipes/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
