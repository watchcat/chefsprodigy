//! End-to-end MCP tests: a real rmcp client talks to the server over an in-memory pipe.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use recipe_core::{IngredientItem, InstructionStep, MeasurementUnit, RecipeInput};
use recipe_db::Db;
use recipe_mcp::RecipeMcpServer;
use recipe_scraper::images::ImageStore;
use recipe_scraper::{HeuristicExtractor, RecipeImporter};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use serde_json::{Value, json};

static COUNTER: AtomicU32 = AtomicU32::new(0);

async fn setup() -> (RunningService<RoleClient, ()>, Db) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf = std::env::temp_dir().join(format!("recipe-mcp-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(dir.join("recipes.db")).await.unwrap();
    let server = RecipeMcpServer::new(db.clone(), RecipeImporter::new(Arc::new(HeuristicExtractor)), ImageStore::new(dir.join("images")));

    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("server handshake");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.expect("client handshake");
    (client, db)
}

fn ing(id: &str, amount: Option<f32>, unit: MeasurementUnit, specialty: bool) -> IngredientItem {
    IngredientItem { raw_text: format!("raw {id}"), metric_amount: amount, unit, canonical_id: id.into(), is_specialty: specialty, canonical_label: None }
}

fn input(title: &str, servings: u8, prep: Option<u16>, cook: Option<u16>, tags: &[&str], ingredients: Vec<IngredientItem>) -> RecipeInput {
    RecipeInput {
        title: title.into(),
        description: None,
        servings,
        prep_time_minutes: prep,
        cook_time_minutes: cook,
        ingredients,
        instructions: vec![InstructionStep { step_number: 1, text: "Cook.".into(), timer_seconds: None }],
        tags: tags.iter().map(|t| t.to_string()).collect(),
        image_path: None,
        origin_url: None,
        author_did: None,
        is_public: false,
        language: None,
    }
}

async fn call(client: &RunningService<RoleClient, ()>, name: &str, args: Value) -> CallToolResult {
    let mut params = CallToolRequestParams::new(name.to_string());
    if let Some(obj) = args.as_object() {
        params = params.with_arguments(obj.clone());
    }
    match client.call_tool_once(params).await.unwrap() {
        CallToolResponse::Complete(r) => r,
        other => panic!("unexpected response: {other:?}"),
    }
}

fn structured(r: &CallToolResult) -> Value {
    assert_ne!(r.is_error, Some(true), "tool errored: {:?}", r.content);
    r.structured_content.clone().expect("structured content")
}

fn error_text(r: &CallToolResult) -> String {
    assert_eq!(r.is_error, Some(true), "expected error, got {r:?}");
    r.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n")
}

#[tokio::test]
async fn lists_the_four_spec_tools_with_schemas() {
    let (client, _) = setup().await;
    let tools = client.list_all_tools().await.unwrap();
    let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort();
    assert_eq!(names, vec!["build_smart_shopping_list", "get_recipe_details", "import_recipe", "search_recipes"]);
    for t in &tools {
        assert!(t.description.as_ref().map(|d| !d.is_empty()).unwrap_or(false), "{} needs a description", t.name);
    }
    let search = tools.iter().find(|t| t.name == "search_recipes").unwrap();
    let schema = serde_json::to_value(&search.input_schema).unwrap();
    for key in ["query", "include_canonical_ingredients", "exclude_canonical_ingredients", "max_total_time_minutes", "limit"] {
        assert!(schema["properties"].get(key).is_some(), "search_recipes schema missing {key}: {schema}");
    }
    let shopping = tools.iter().find(|t| t.name == "build_smart_shopping_list").unwrap();
    let schema = serde_json::to_value(&shopping.input_schema).unwrap();
    assert!(schema["required"].as_array().unwrap().contains(&json!("items")));
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn search_recipes_returns_summaries_with_missing_ingredients() {
    let (client, db) = setup().await;
    let omelette = db.create_recipe(input("Cheese Omelette", 1, Some(5), Some(5), &["breakfast"], vec![ing("egg", Some(3.0), MeasurementUnit::Pcs, false), ing("cheddar", Some(50.0), MeasurementUnit::G, false)])).await.unwrap();
    db.create_recipe(input("Beef Pho", 4, Some(30), Some(360), &["soup"], vec![ing("beef_bones", Some(1000.0), MeasurementUnit::G, false), ing("egg", Some(1.0), MeasurementUnit::Pcs, false)])).await.unwrap();

    let out = structured(&call(&client, "search_recipes", json!({"include_canonical_ingredients": ["egg"], "max_total_time_minutes": 30})).await);
    let recipes = out["recipes"].as_array().unwrap();
    assert_eq!(recipes.len(), 1, "{out}");
    assert_eq!(recipes[0]["id"], omelette.id);
    assert_eq!(recipes[0]["title"], "Cheese Omelette");
    assert_eq!(recipes[0]["servings"], 1);
    assert_eq!(recipes[0]["total_time_minutes"], 10);
    assert_eq!(recipes[0]["canonical_ingredients"], json!(["egg", "cheddar"]));
    assert_eq!(recipes[0]["missing_ingredients"], json!(["cheddar"]));

    let out = structured(&call(&client, "search_recipes", json!({"query": "pho"})).await);
    assert_eq!(out["recipes"][0]["title"], "Beef Pho");

    let out = structured(&call(&client, "search_recipes", json!({"exclude_canonical_ingredients": ["egg"]})).await);
    assert_eq!(out["recipes"].as_array().unwrap().len(), 0);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn get_recipe_details_scales_and_reports_missing() {
    let (client, db) = setup().await;
    let r = db.create_recipe(input("Rice", 2, None, Some(20), &[], vec![ing("rice", Some(300.0), MeasurementUnit::G, false)])).await.unwrap();

    let out = structured(&call(&client, "get_recipe_details", json!({"recipe_id": r.id, "target_servings": 6})).await);
    assert_eq!(out["servings"], 6);
    assert_eq!(out["ingredients"][0]["metric_amount"], 900.0);
    assert_eq!(out["content_hash"], r.content_hash);

    let out = structured(&call(&client, "get_recipe_details", json!({"recipe_id": r.id})).await);
    assert_eq!(out["servings"], 2);

    let err = error_text(&call(&client, "get_recipe_details", json!({"recipe_id": "nope"})).await);
    assert!(err.contains("not found"), "{err}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn build_smart_shopping_list_aggregates_across_recipes() {
    let (client, db) = setup().await;
    let pho = db.create_recipe(input("Pho", 2, None, None, &[], vec![ing("beef_bones", Some(1000.0), MeasurementUnit::G, false), ing("star_anise", Some(3.0), MeasurementUnit::Pcs, true), ing("salt", None, MeasurementUnit::ToTaste, false)])).await.unwrap();
    let om = db.create_recipe(input("Omelette", 1, None, None, &[], vec![ing("egg", Some(3.0), MeasurementUnit::Pcs, false), ing("salt", None, MeasurementUnit::ToTaste, false)])).await.unwrap();

    let out = structured(&call(&client, "build_smart_shopping_list", json!({
        "items": [{"recipe_id": pho.id, "servings": 4}, {"recipe_id": om.id, "servings": 2}, {"recipe_id": "ghost", "servings": 1}],
        "pantry_exclude_canonical_ids": ["salt"]
    })).await);
    let basic = out["basic_groceries"].as_array().unwrap();
    assert_eq!(basic.iter().map(|l| l["canonical_id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["beef_bones", "egg"]);
    assert_eq!(basic[0]["total_amount"], 2000.0);
    assert_eq!(basic[0]["unit"], "g");
    assert_eq!(basic[1]["total_amount"], 6.0);
    let specialty = out["specialty_items"].as_array().unwrap();
    assert_eq!(specialty.len(), 1);
    assert_eq!(specialty[0]["canonical_id"], "star_anise");
    assert_eq!(specialty[0]["total_amount"], 6.0);
    assert_eq!(out["missing_recipes"], json!(["ghost"]));

    let err = error_text(&call(&client, "build_smart_shopping_list", json!({"items": []})).await);
    assert!(err.contains("at least one"), "{err}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn import_recipe_validates_input_and_reports_llm_absence() {
    let (client, db) = setup().await;
    let err = error_text(&call(&client, "import_recipe", json!({})).await);
    assert!(err.contains("source_url") && err.contains("raw_text"), "{err}");

    let err = error_text(&call(&client, "import_recipe", json!({"raw_text": "Fry two eggs"})).await);
    assert!(err.contains("LLM_MODEL"), "{err}");

    let err = error_text(&call(&client, "import_recipe", json!({"source_url": "ftp://x/y"})).await);
    assert!(err.contains("http"), "{err}");

    let err = error_text(&call(&client, "import_recipe", json!({"source_url": "http://127.0.0.1:9/nothing"})).await);
    assert!(err.contains("fetch"), "{err}");

    assert!(db.search_recipes(&Default::default()).await.unwrap().is_empty(), "nothing persisted on failure");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn import_recipe_from_jsonld_site_persists_recipe() {
    let (client, db) = setup().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let body = r#"<script type="application/ld+json">{"@type":"Recipe","name":"Net Toast","recipeYield":"2","recipeIngredient":["2 slices bread"],"recipeInstructions":"Toast for 3 minutes."}</script>"#;
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    let out = structured(&call(&client, "import_recipe", json!({"source_url": format!("{base}/toast"), "is_public": true})).await);
    assert_eq!(out["title"], "Net Toast");
    assert_eq!(out["servings"], 2);
    assert_eq!(out["is_public"], true);
    assert_eq!(out["ingredients"][0]["canonical_id"], "bread");
    assert_eq!(out["instructions"][0]["timer_seconds"], 180);
    assert!(db.get_recipe(out["id"].as_str().unwrap()).await.unwrap().is_some());
    client.cancel().await.unwrap();
}
