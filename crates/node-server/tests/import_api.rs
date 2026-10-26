//! Import endpoint tests: text, URL (against a local stand-in site) and Mealie upload.

use std::future::Future;
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use http_body_util::BodyExt;
use node_server::{AppState, NodeInfo, build_router};
use recipe_core::{IngredientItem, InstructionStep, MeasurementUnit};
use recipe_db::Db;
use recipe_scraper::llm::{ExtractedRecipeDraft, HeuristicExtractor, RecipeExtractor};
use recipe_scraper::{RecipeImporter, ScraperError};
use serde_json::{Value, json};
use tower::ServiceExt;

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct FakeLlm;

impl RecipeExtractor for FakeLlm {
    fn normalize_ingredients<'a>(
        &'a self,
        raw: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IngredientItem>, ScraperError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(raw
                .iter()
                .map(|r| IngredientItem {
                    raw_text: r.clone(),
                    metric_amount: Some(1.0),
                    unit: MeasurementUnit::Pcs,
                    canonical_id: r.replace(' ', "_"),
                    is_specialty: false,
                    canonical_label: None,
                })
                .collect())
        })
    }

    fn extract_recipe<'a>(
        &'a self,
        markdown: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<ExtractedRecipeDraft, ScraperError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(ExtractedRecipeDraft {
                title: format!("Extracted: {}", markdown.lines().next().unwrap_or("")),
                description: None,
                servings: Some(2),
                prep_time_minutes: None,
                cook_time_minutes: Some(10),
                ingredients: vec![IngredientItem {
                    raw_text: "2 eggs".into(),
                    metric_amount: Some(2.0),
                    unit: MeasurementUnit::Pcs,
                    canonical_id: "egg".into(),
                    is_specialty: false,
                    canonical_label: None,
                }],
                instructions: vec![InstructionStep {
                    step_number: 1,
                    text: "Fry for 3 minutes.".into(),
                    timer_seconds: None,
                }],
                tags: vec!["Breakfast".into()],
                language: None,
            })
        })
    }
}

async fn app_with(extractor: Arc<dyn RecipeExtractor>) -> (Router, PathBuf) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("node-import-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(dir.join("recipes.db")).await.unwrap();
    let state = AppState::new(
        db,
        NodeInfo {
            author_did: None,
            chef_name: "T".into(),
        },
    )
    .with_importer(RecipeImporter::new(extractor))
    .with_image_dir(dir.join("images"));
    (build_router(state), dir)
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

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 50, 50]));
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
    buf.into_inner()
}

/// Spin up a local "recipe blog" serving one JSON-LD page and a cover image.
async fn fake_site() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let page = r#"<html><head><script>var secret=1;</script><script type="application/ld+json">
        {"@type":"Recipe","name":"Site Pho","image":"/cover.png","recipeYield":"3","cookTime":"PT20M",
          "recipeIngredient":["2 star anise","salt"],"recipeInstructions":[{"@type":"HowToStep","text":"Simmer for 20 minutes."}]}
        </script></head><body><h1>Site Pho</h1></body></html>"#
        .to_string();
    let site = Router::new()
        .route(
            "/pho",
            get(move || {
                let p = page.clone();
                async move { ([(header::CONTENT_TYPE, "text/html")], p) }
            }),
        )
        .route(
            "/cover.png",
            get(|| async { ([(header::CONTENT_TYPE, "image/png")], png(1600, 800)) }),
        )
        .route(
            "/plain",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/html")],
                    "<h1>Plain Eggs</h1><p>Fry them.</p>",
                )
            }),
        );
    tokio::spawn(async move { axum::serve(listener, site).await.unwrap() });
    base
}

#[tokio::test]
async fn import_text_creates_recipe_via_extractor() {
    let (app, _) = app_with(Arc::new(FakeLlm)).await;
    let (status, body) = send(
        &app,
        "POST",
        "/api/v1/import/text",
        Some(json!({"text": "Fry two eggs", "origin_url": "https://ig/p/1", "is_public": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["title"], "Extracted: Fry two eggs");
    assert_eq!(body["origin_url"], "https://ig/p/1");
    assert_eq!(body["is_public"], true);
    assert_eq!(
        body["instructions"][0]["timer_seconds"], 180,
        "timer filled from text"
    );
    assert_eq!(body["tags"], json!(["breakfast"]));
    assert_eq!(body["content_hash"].as_str().unwrap().len(), 64);

    let (_, fetched) = send(
        &app,
        "GET",
        &format!("/api/v1/recipes/{}", body["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(fetched, body);
}

#[tokio::test]
async fn import_text_without_llm_returns_503_json() {
    let (app, _) = app_with(Arc::new(HeuristicExtractor)).await;
    let (status, body) = send(
        &app,
        "POST",
        "/api/v1/import/text",
        Some(json!({"text": "Fry two eggs"})),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body["error"].as_str().unwrap().contains("LLM_MODEL"));
}

#[tokio::test]
async fn import_text_rejects_blank_and_malformed_bodies() {
    let (app, _) = app_with(Arc::new(FakeLlm)).await;
    let (status, _) = send(
        &app,
        "POST",
        "/api/v1/import/text",
        Some(json!({"text": "   "})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = send(
        &app,
        "POST",
        "/api/v1/import/text",
        Some(json!({"nope": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn import_url_uses_jsonld_stores_webp_cover_and_serves_it() {
    let (app, dir) = app_with(Arc::new(HeuristicExtractor)).await;
    let base = fake_site().await;
    let (status, body) = send(
        &app,
        "POST",
        "/api/v1/import/url",
        Some(json!({"url": format!("{base}/pho"), "is_public": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["title"], "Site Pho");
    assert_eq!(body["servings"], 3);
    assert_eq!(body["cook_time_minutes"], 20);
    assert_eq!(body["origin_url"], format!("{base}/pho"));
    assert_eq!(body["ingredients"][0]["canonical_id"], "star_anise");
    assert_eq!(body["instructions"][0]["timer_seconds"], 1200);
    let id = body["id"].as_str().unwrap();
    assert_eq!(body["image_path"], format!("images/{id}.webp"));
    assert!(dir.join("images").join(format!("{id}.webp")).exists());

    let res = app
        .clone()
        .oneshot(
            Request::get(format!("/images/{id}.webp"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "image/webp");
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let img = image::load_from_memory(&bytes).unwrap();
    assert_eq!(img.width(), 1200, "downsized from 1600");
}

#[tokio::test]
async fn import_url_without_jsonld_falls_back_to_extractor() {
    let (app, _) = app_with(Arc::new(FakeLlm)).await;
    let base = fake_site().await;
    let (status, body) = send(
        &app,
        "POST",
        "/api/v1/import/url",
        Some(json!({"url": format!("{base}/plain")})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["title"], "Extracted: # Plain Eggs");
    assert_eq!(body["is_public"], false);
    assert_eq!(body["image_path"], Value::Null);
}

#[tokio::test]
async fn import_url_failures_map_to_clean_errors() {
    let (app, _) = app_with(Arc::new(FakeLlm)).await;
    let base = fake_site().await;
    let (status, body) = send(
        &app,
        "POST",
        "/api/v1/import/url",
        Some(json!({"url": format!("{base}/missing")})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body["error"].is_string());

    let (status, _) = send(
        &app,
        "POST",
        "/api/v1/import/url",
        Some(json!({"url": "http://127.0.0.1:9/unreachable"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);

    let (status, _) = send(
        &app,
        "POST",
        "/api/v1/import/url",
        Some(json!({"url": "not a url"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = send(
        &app,
        "POST",
        "/api/v1/import/url",
        Some(json!({"is_public": true})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

fn mealie_zip() -> Vec<u8> {
    let db = json!({
        "recipes": [
            {"id": "r-1", "slug": "pho", "name": "Mealie Pho", "recipe_yield": "4 servings", "prep_time": "30 minutes", "cook_time": "6 hours"},
            {"id": "r-2", "slug": "toast", "name": "Mealie Toast"}
        ],
        "recipes_ingredients": [
            {"id": "i-1", "recipe_id": "r-1", "position": 0, "quantity": 3.0, "food_id": "f-1", "original_text": "3 star anise"},
            {"id": "i-2", "recipe_id": "r-2", "position": 0, "quantity": 2.0, "food_id": "f-2"}
        ],
        "ingredient_foods": [{"id": "f-1", "name": "star anise"}, {"id": "f-2", "name": "bread"}],
        "recipe_instructions": [{"id": "s-1", "recipe_id": "r-1", "position": 0, "text": "Simmer."}],
        "tags": [{"id": "t-1", "name": "Soup"}],
        "recipes_to_tags": [{"recipe_id": "r-1", "tag_id": "t-1"}]
    });
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    w.start_file("database.json", opts).unwrap();
    w.write_all(db.to_string().as_bytes()).unwrap();
    w.start_file("data/recipes/r-1/images/original.png", opts)
        .unwrap();
    w.write_all(&png(20, 10)).unwrap();
    w.finish().unwrap().into_inner()
}

fn multipart(field: &str, filename: &str, bytes: &[u8]) -> (String, Vec<u8>) {
    let boundary = "----recipeNodeBoundary42";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: application/zip\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

async fn upload(app: &Router, ct: String, body: Vec<u8>) -> (StatusCode, Value) {
    let req = Request::post("/api/v1/import/mealie-backup")
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into())),
    )
}

#[tokio::test]
async fn mealie_backup_upload_imports_all_recipes_with_images() {
    let (app, dir) = app_with(Arc::new(HeuristicExtractor)).await;
    let (ct, body) = multipart("file", "backup.zip", &mealie_zip());
    let (status, report) = upload(&app, ct, body).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["imported_count"], 2);
    assert_eq!(report["failed_count"], 0);
    assert_eq!(report["errors"], json!([]));

    let (_, list) = send(&app, "GET", "/api/v1/recipes?q=mealie", None).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    let pho = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["title"] == "Mealie Pho")
        .unwrap();
    assert!(pho["image_path"].as_str().unwrap().ends_with(".webp"));
    assert_eq!(pho["total_time_minutes"], 390);
    assert_eq!(pho["tags"], json!(["soup"]));
    assert_eq!(
        std::fs::read_dir(dir.join("images")).unwrap().count(),
        1,
        "only pho had an image"
    );
}

#[tokio::test]
async fn mealie_backup_upload_rejects_non_zip_and_missing_file() {
    let (app, _) = app_with(Arc::new(HeuristicExtractor)).await;
    let (ct, body) = multipart("file", "x.zip", b"not a zip at all");
    let (status, report) = upload(&app, ct, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{report}");
    assert!(report["error"].is_string());

    let (ct, body) = multipart("comment", "", b"hello");
    let (status, _) = upload(&app, ct, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn image_route_rejects_traversal_and_unknown_files() {
    let (app, _) = app_with(Arc::new(HeuristicExtractor)).await;
    for path in [
        "/images/nope.webp",
        "/images/..%2F..%2Fetc%2Fpasswd",
        "/images/x.png",
    ] {
        let res = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

fn image_multipart(bytes: &[u8]) -> (String, Vec<u8>) {
    multipart("file", "cover.png", bytes)
}

#[tokio::test]
async fn cover_upload_stores_webp_and_updates_recipe() {
    let (app, dir) = app_with(Arc::new(HeuristicExtractor)).await;
    let (_, created) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(json!({"title": "Manual", "ingredients": [], "instructions": []})),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["image_path"], Value::Null);

    let (ct, body) = image_multipart(&png(1400, 700));
    let req = Request::put(format!("/api/v1/recipes/{id}/image"))
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let updated: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(updated["image_path"], format!("images/{id}.webp"));
    assert_eq!(
        updated["content_hash"], created["content_hash"],
        "cover does not change the content hash"
    );
    let stored = image::open(dir.join("images").join(format!("{id}.webp"))).unwrap();
    assert_eq!(stored.width(), 1200, "downsized");

    let (_, fetched) = send(&app, "GET", &format!("/api/v1/recipes/{id}"), None).await;
    assert_eq!(fetched["image_path"], updated["image_path"]);
}

#[tokio::test]
async fn cover_upload_rejects_garbage_and_unknown_recipe() {
    let (app, _) = app_with(Arc::new(HeuristicExtractor)).await;
    let (_, created) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(json!({"title": "Manual"})),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    let (ct, body) = image_multipart(b"definitely not an image");
    let req = Request::put(format!("/api/v1/recipes/{id}/image"))
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let (ct, body) = image_multipart(&png(10, 10));
    let req = Request::put("/api/v1/recipes/nope/image")
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleting_a_recipe_removes_its_cover_file() {
    let (app, dir) = app_with(Arc::new(HeuristicExtractor)).await;
    let (_, created) = send(
        &app,
        "POST",
        "/api/v1/recipes",
        Some(json!({"title": "Manual"})),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let (ct, body) = image_multipart(&png(20, 20));
    let req = Request::put(format!("/api/v1/recipes/{id}/image"))
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
    let file = dir.join("images").join(format!("{id}.webp"));
    assert!(file.exists());

    let (status, _) = send(&app, "DELETE", &format!("/api/v1/recipes/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!file.exists(), "cover file must be removed with the recipe");
}

mod photos {
    use super::*;
    use recipe_scraper::images::NormalizedPhoto;
    use recipe_scraper::llm::{ExtractedRecipeDraft, PhotoExtraction, PhotoInfo, PhotoKind};

    struct VisionFake {
        kind: PhotoKind,
        find_recipe: bool,
    }

    impl RecipeExtractor for VisionFake {
        fn normalize_ingredients<'a>(
            &'a self,
            _raw: &'a [String],
        ) -> Pin<Box<dyn Future<Output = Result<Vec<IngredientItem>, ScraperError>> + Send + 'a>>
        {
            Box::pin(async { Ok(vec![]) })
        }
        fn extract_recipe<'a>(
            &'a self,
            _m: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<ExtractedRecipeDraft, ScraperError>> + Send + 'a>>
        {
            Box::pin(async { Err(ScraperError::LlmUnavailable) })
        }
        fn supports_images(&self) -> bool {
            true
        }
        fn extract_from_photos<'a>(
            &'a self,
            photos: &'a [NormalizedPhoto],
        ) -> Pin<Box<dyn Future<Output = Result<PhotoExtraction, ScraperError>> + Send + 'a>>
        {
            Box::pin(async move {
                if !self.find_recipe {
                    return Err(ScraperError::NoRecipeInPhotos(
                        "only a cat was found".into(),
                    ));
                }
                Ok(PhotoExtraction {
                    recipe: ExtractedRecipeDraft {
                        title: format!("From {} photo(s)", photos.len()),
                        description: None,
                        servings: Some(2),
                        prep_time_minutes: None,
                        cook_time_minutes: None,
                        ingredients: vec![],
                        instructions: vec![],
                        tags: vec![],
                        language: None,
                    },
                    photos: photos
                        .iter()
                        .enumerate()
                        .map(|(i, _)| PhotoInfo {
                            index: i,
                            kind: self.kind,
                            notes: None,
                        })
                        .collect(),
                    unreadable: vec![],
                    language: None,
                })
            })
        }
    }

    fn photos_multipart(photos: &[&[u8]], is_public: bool) -> (String, Vec<u8>) {
        let boundary = "----photoBoundary7";
        let mut body = Vec::new();
        for (i, p) in photos.iter().enumerate() {
            body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"photos\"; filename=\"p{i}.png\"\r\nContent-Type: image/png\r\n\r\n").as_bytes());
            body.extend_from_slice(p);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"is_public\"\r\n\r\n{is_public}\r\n--{boundary}--\r\n").as_bytes());
        (format!("multipart/form-data; boundary={boundary}"), body)
    }

    async fn post_photos(app: &Router, ct: String, body: Vec<u8>) -> (StatusCode, Value) {
        let req = Request::post("/api/v1/import/photo")
            .header(header::CONTENT_TYPE, ct)
            .body(Body::from(body))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes)
                .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into())),
        )
    }

    #[tokio::test]
    async fn photo_import_creates_recipe_with_cover() {
        let (app, dir) = app_with(Arc::new(VisionFake {
            kind: PhotoKind::Both,
            find_recipe: true,
        }))
        .await;
        let (ct, body) = photos_multipart(&[&png(1800, 1200), &png(400, 300)], true);
        let (status, out) = post_photos(&app, ct, body).await;
        assert_eq!(status, StatusCode::CREATED, "{out}");
        assert_eq!(out["title"], "From 2 photo(s)");
        assert_eq!(out["is_public"], true);
        let id = out["id"].as_str().unwrap();
        assert_eq!(out["image_path"], format!("images/{id}.webp"));
        assert!(dir.join("images").join(format!("{id}.webp")).exists());
    }

    #[tokio::test]
    async fn photo_import_error_paths() {
        let (app, _) = app_with(Arc::new(VisionFake {
            kind: PhotoKind::RecipeText,
            find_recipe: false,
        }))
        .await;
        let (ct, body) = photos_multipart(&[&png(50, 50)], false);
        let (status, out) = post_photos(&app, ct, body).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{out}");
        assert!(out["error"].as_str().unwrap().contains("cat"));

        let (ct, body) = photos_multipart(&[], false);
        let (status, _) = post_photos(&app, ct, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (ct, body) = photos_multipart(&[b"not an image"], false);
        let (status, _) = post_photos(&app, ct, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let (app, _) = app_with(Arc::new(HeuristicExtractor)).await;
        let (ct, body) = photos_multipart(&[&png(50, 50)], false);
        let (status, out) = post_photos(&app, ct, body).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{out}");
        assert!(out["error"].as_str().unwrap().contains("image"));
    }
}
