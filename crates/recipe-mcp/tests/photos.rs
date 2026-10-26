//! Photo import through the service and the MCP tool, with a fake vision extractor.

use std::future::Future;
use std::io::Cursor;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use recipe_core::{IngredientItem, InstructionStep, MeasurementUnit};
use recipe_db::Db;
use recipe_mcp::{ImportError, ImportService, RecipeMcpServer};
use recipe_scraper::images::{ImageStore, NormalizedPhoto};
use recipe_scraper::llm::{
    ExtractedRecipeDraft, HeuristicExtractor, PhotoExtraction, PhotoInfo, PhotoKind,
    RecipeExtractor,
};
use recipe_scraper::{RecipeImporter, ScraperError};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResponse};
use serde_json::json;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb([90, 140, 60]));
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
    buf.into_inner()
}

/// Returns a fixed recipe and classifies photos by the kinds given; records what it saw.
struct FakeVision {
    kinds: Vec<PhotoKind>,
    seen: Mutex<Vec<(u32, u32)>>,
}

impl RecipeExtractor for FakeVision {
    fn normalize_ingredients<'a>(
        &'a self,
        _raw: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IngredientItem>, ScraperError>> + Send + 'a>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn extract_recipe<'a>(
        &'a self,
        _m: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<ExtractedRecipeDraft, ScraperError>> + Send + 'a>> {
        Box::pin(async { Err(ScraperError::LlmUnavailable) })
    }
    fn supports_images(&self) -> bool {
        true
    }
    fn extract_from_photos<'a>(
        &'a self,
        photos: &'a [NormalizedPhoto],
    ) -> Pin<Box<dyn Future<Output = Result<PhotoExtraction, ScraperError>> + Send + 'a>> {
        Box::pin(async move {
            *self.seen.lock().unwrap() = photos.iter().map(|p| (p.width, p.height)).collect();
            Ok(PhotoExtraction {
                recipe: ExtractedRecipeDraft {
                    title: "Photo Borscht".into(),
                    description: None,
                    servings: Some(6),
                    prep_time_minutes: None,
                    cook_time_minutes: Some(90),
                    ingredients: vec![IngredientItem {
                        raw_text: "2 beets".into(),
                        metric_amount: Some(2.0),
                        unit: MeasurementUnit::Pcs,
                        canonical_id: "beetroot".into(),
                        is_specialty: false,
                        canonical_label: None,
                    }],
                    instructions: vec![InstructionStep {
                        step_number: 1,
                        text: "Simmer for 90 minutes.".into(),
                        timer_seconds: None,
                    }],
                    tags: vec!["soup".into()],
                    language: None,
                },
                photos: self
                    .kinds
                    .iter()
                    .enumerate()
                    .map(|(i, k)| PhotoInfo {
                        index: i,
                        kind: *k,
                        notes: None,
                    })
                    .collect(),
                unreadable: vec![],
                language: Some("ru".into()),
            })
        })
    }
}

async fn setup(extractor: Arc<dyn RecipeExtractor>) -> (Db, ImportService, PathBuf) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("recipe-photo-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(dir.join("recipes.db")).await.unwrap();
    let service = ImportService::new(
        db.clone(),
        RecipeImporter::new(extractor),
        ImageStore::new(dir.join("images")),
    );
    (db, service, dir)
}

#[tokio::test]
async fn photos_are_normalized_recipe_saved_and_dish_photo_becomes_cover() {
    let fake = Arc::new(FakeVision {
        kinds: vec![PhotoKind::RecipeText, PhotoKind::Dish],
        seen: Mutex::new(vec![]),
    });
    let (db, service, dir) = setup(fake.clone()).await;

    let recipe = service
        .import_photos(vec![png(3200, 2400), png(800, 600)], true)
        .await
        .unwrap();
    assert_eq!(recipe.title, "Photo Borscht");
    assert!(recipe.is_public);
    assert_eq!(
        recipe.instructions[0].timer_seconds,
        Some(5400),
        "timer filled from text"
    );
    assert_eq!(
        recipe.image_path.as_deref(),
        Some(format!("images/{}.webp", recipe.id).as_str())
    );
    assert!(
        dir.join("images")
            .join(format!("{}.webp", recipe.id))
            .exists()
    );
    assert_eq!(
        *fake.seen.lock().unwrap(),
        vec![(1600, 1200), (800, 600)],
        "photos downscaled before the model sees them"
    );
    assert!(db.get_recipe(&recipe.id).await.unwrap().is_some());
}

#[tokio::test]
async fn text_only_photos_import_without_cover() {
    let fake = Arc::new(FakeVision {
        kinds: vec![PhotoKind::RecipeText],
        seen: Mutex::new(vec![]),
    });
    let (_, service, _) = setup(fake).await;
    let recipe = service
        .import_photos(vec![png(500, 700)], false)
        .await
        .unwrap();
    assert_eq!(recipe.image_path, None);
}

#[tokio::test]
async fn photo_count_and_content_are_validated() {
    let fake = Arc::new(FakeVision {
        kinds: vec![],
        seen: Mutex::new(vec![]),
    });
    let (_, service, _) = setup(fake).await;
    assert!(matches!(
        service.import_photos(vec![], false).await.unwrap_err(),
        ImportError::InvalidInput(_)
    ));
    let seven = (0..7).map(|_| png(10, 10)).collect();
    assert!(matches!(
        service.import_photos(seven, false).await.unwrap_err(),
        ImportError::InvalidInput(_)
    ));
    let err = service
        .import_photos(vec![b"garbage".to_vec()], false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ImportError::Scraper(ScraperError::Image(_))),
        "{err:?}"
    );
}

#[tokio::test]
async fn model_without_vision_is_reported() {
    let (_, service, _) = setup(Arc::new(HeuristicExtractor)).await;
    let err = service
        .import_photos(vec![png(10, 10)], false)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ImportError::Scraper(ScraperError::ModelCannotSeeImages(_))
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn mcp_import_recipe_accepts_photo_paths() {
    let fake = Arc::new(FakeVision {
        kinds: vec![PhotoKind::Both],
        seen: Mutex::new(vec![]),
    });
    let (db, _, dir) = setup(fake.clone()).await;
    let photo = dir.join("card.png");
    std::fs::write(&photo, png(300, 300)).unwrap();

    let server = RecipeMcpServer::new(
        db.clone(),
        RecipeImporter::new(fake),
        ImageStore::new(dir.join("images")),
    );
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("server");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.expect("client");

    let params = CallToolRequestParams::new("import_recipe").with_arguments(
        json!({"photo_paths": [photo.to_string_lossy()]})
            .as_object()
            .cloned()
            .unwrap(),
    );
    let CallToolResponse::Complete(result) = client.call_tool_once(params).await.unwrap() else {
        panic!("unexpected")
    };
    assert_ne!(result.is_error, Some(true), "{:?}", result.content);
    let out = result.structured_content.unwrap();
    assert_eq!(out["title"], "Photo Borscht");
    assert!(out["image_path"].as_str().unwrap().ends_with(".webp"));

    let params = CallToolRequestParams::new("import_recipe").with_arguments(
        json!({"photo_paths": ["/nonexistent/photo.jpg"]})
            .as_object()
            .cloned()
            .unwrap(),
    );
    let CallToolResponse::Complete(result) = client.call_tool_once(params).await.unwrap() else {
        panic!("unexpected")
    };
    assert_eq!(result.is_error, Some(true));
    let text = result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<String>();
    assert!(text.contains("photo.jpg"), "{text}");
    client.cancel().await.unwrap();
}
