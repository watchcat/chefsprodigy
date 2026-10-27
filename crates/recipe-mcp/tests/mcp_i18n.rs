//! `language` on the MCP tools.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use recipe_core::{IngredientItem, InstructionStep, MeasurementUnit, RecipeInput};
use recipe_db::Db;
use recipe_mcp::RecipeMcpServer;
use recipe_scraper::images::ImageStore;
use recipe_scraper::llm::{
    ExtractedRecipeDraft, RecipeExtractor, TranslationReply, TranslationRequest,
};
use recipe_scraper::{RecipeImporter, ScraperError};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResponse};
use serde_json::{Value, json};

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct Echo;
impl RecipeExtractor for Echo {
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
    ) -> Pin<Box<dyn Future<Output = Result<HashMap<String, String>, ScraperError>> + Send + 'a>>
    {
        Box::pin(async move {
            Ok(ids
                .iter()
                .map(|id| (id.clone(), format!("{lang}:{id}")))
                .collect())
        })
    }
}

async fn setup() -> (rmcp::service::RunningService<rmcp::RoleClient, ()>, Db) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("recipe-mcp-i18n-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(dir.join("recipes.db")).await.unwrap();
    let server = RecipeMcpServer::new(
        db.clone(),
        RecipeImporter::new(Arc::new(Echo)),
        ImageStore::new(dir.join("images")),
    );
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("server");
        let _ = running.waiting().await;
    });
    (().serve(client_io).await.expect("client"), db)
}

async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    name: &str,
    args: Value,
) -> Value {
    let params = CallToolRequestParams::new(name.to_string())
        .with_arguments(args.as_object().cloned().unwrap());
    match client.call_tool_once(params).await.unwrap() {
        CallToolResponse::Complete(r) => {
            assert_ne!(r.is_error, Some(true), "{:?}", r.content);
            r.structured_content.unwrap()
        }
        other => panic!("{other:?}"),
    }
}

fn pho() -> RecipeInput {
    RecipeInput {
        title: "Pho".into(),
        description: None,
        servings: 2,
        prep_time_minutes: None,
        cook_time_minutes: Some(30),
        ingredients: vec![
            IngredientItem {
                raw_text: "1 kg botten".into(),
                metric_amount: Some(1000.0),
                unit: MeasurementUnit::G,
                canonical_id: "beef_bones".into(),
                is_specialty: false,
                canonical_label: None,
            },
            IngredientItem {
                raw_text: "steranijs".into(),
                metric_amount: Some(3.0),
                unit: MeasurementUnit::Pcs,
                canonical_id: "star_anise".into(),
                is_specialty: true,
                canonical_label: None,
            },
        ],
        instructions: vec![InstructionStep {
            step_number: 1,
            text: "Kook.".into(),
            timer_seconds: None,
        }],
        tags: vec!["soep".into()],
        image_path: None,
        origin_url: None,
        author_did: None,
        is_public: false,
        language: Some("nl".into()),
    }
}

#[tokio::test]
async fn tools_honour_the_language_argument() {
    let (client, db) = setup().await;
    let r = db.create_recipe(pho()).await.unwrap();

    let d = call(
        &client,
        "get_recipe_details",
        json!({"recipe_id": r.id, "language": "ru"}),
    )
    .await;
    assert_eq!(d["title"], "[ru] Pho");
    assert_eq!(d["ingredients"][0]["raw_text"], "[ru] 1 kg botten");
    assert_eq!(d["ingredients"][0]["canonical_label"], "ru:beef_bones");
    assert_eq!(d["translation"]["lang"], "ru");

    let s = call(
        &client,
        "search_recipes",
        json!({"query": "pho", "language": "ru"}),
    )
    .await;
    assert_eq!(
        s["recipes"][0]["title"], "[ru] Pho",
        "translation cached by the previous call is used in summaries"
    );

    let l = call(
        &client,
        "build_smart_shopping_list",
        json!({"items": [{"recipe_id": r.id, "servings": 2}], "language": "ru"}),
    )
    .await;
    assert_eq!(l["basic_groceries"][0]["canonical_label"], "ru:beef_bones");
    assert_eq!(l["specialty_items"][0]["canonical_label"], "ru:star_anise");

    let plain = call(&client, "get_recipe_details", json!({"recipe_id": r.id})).await;
    assert_eq!(plain["title"], "Pho");
    assert!(plain.get("translation").is_none());
    client.cancel().await.unwrap();
}
