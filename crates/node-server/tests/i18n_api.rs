//! `lang` handling on the recipe endpoints and the translation endpoints.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
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

struct EchoTranslator;
impl RecipeExtractor for EchoTranslator {
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
}

async fn app(extractor: Arc<dyn RecipeExtractor>) -> Router {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("node-i18n-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(dir.join("recipes.db")).await.unwrap();
    build_router(
        AppState::new(
            db,
            NodeInfo {
                author_did: None,
                chef_name: "T".into(),
            },
        )
        .with_importer(RecipeImporter::new(extractor))
        .with_image_dir(dir.join("images")),
    )
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
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
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
        },
    )
}

/// Records how often the node asks the model for ingredient labels.
struct CountingTerms(std::sync::atomic::AtomicUsize);

impl RecipeExtractor for CountingTerms {
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
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(ids
                .iter()
                .map(|id| (id.clone(), format!("{lang}:{id}")))
                .collect())
        })
    }
}

#[tokio::test]
async fn everyday_ingredients_are_labelled_without_a_model_call() {
    let counter = Arc::new(CountingTerms(std::sync::atomic::AtomicUsize::new(0)));
    let app = app(counter.clone()).await;
    let recipe = json!({"title": "Soup", "servings": 2,
        "ingredients": [
            {"raw_text": "1 tsp salt", "metric_amount": 5.0, "unit": "ml", "canonical_id": "salt", "is_specialty": false},
            {"raw_text": "2 cloves garlic", "metric_amount": 2.0, "unit": "pcs", "canonical_id": "garlic", "is_specialty": false},
            {"raw_text": "1 onion", "metric_amount": 1.0, "unit": "pcs", "canonical_id": "onion", "is_specialty": false}
        ],
        "instructions": [{"step_number": 1, "text": "Simmer.", "timer_seconds": null}], "tags": []});
    let (_, r) = send(&app, "POST", "/api/v1/recipes", Some(recipe), &[]).await;
    let id = r["id"].as_str().unwrap().to_string();
    for (lang, labels) in [
        ("ru", ["соль", "чеснок", "лук"]),
        ("nl", ["zout", "knoflook", "ui"]),
        ("en", ["salt", "garlic", "onion"]),
    ] {
        let (_, body) = send(
            &app,
            "GET",
            &format!("/api/v1/recipes/{id}?lang={lang}"),
            None,
            &[],
        )
        .await;
        let got: Vec<&str> = body["ingredients"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["canonical_label"].as_str().unwrap_or("<none>"))
            .collect();
        assert_eq!(got, labels, "{lang}: {body}");
    }
    assert_eq!(
        counter.0.load(Ordering::SeqCst),
        0,
        "no model call for seeded everyday ingredients"
    );
}

fn dutch() -> Value {
    json!({"title": "Pho", "description": "Soep", "language": "nl", "servings": 2,
        "ingredients": [{"raw_text": "1 kg botten", "metric_amount": 1000.0, "unit": "g", "canonical_id": "beef_bones", "is_specialty": false}],
        "instructions": [{"step_number": 1, "text": "Kook.", "timer_seconds": null}], "tags": ["soep"]})
}

#[tokio::test]
async fn language_is_stored_and_returned() {
    let app = app(Arc::new(HeuristicExtractor)).await;
    let (status, r) = send(&app, "POST", "/api/v1/recipes", Some(dutch()), &[]).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(r["language"], "nl");
    assert!(r["translation"].is_null());
    let (_, list) = send(&app, "GET", "/api/v1/recipes", None, &[]).await;
    assert_eq!(list["items"][0]["language"], "nl");
}

#[tokio::test]
async fn lang_query_accept_language_and_source_precedence() {
    let app = app(Arc::new(EchoTranslator)).await;
    let (_, r) = send(&app, "POST", "/api/v1/recipes", Some(dutch()), &[]).await;
    let id = r["id"].as_str().unwrap();

    let (status, ru) = send(
        &app,
        "GET",
        &format!("/api/v1/recipes/{id}?lang=ru&servings=4"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ru}");
    assert_eq!(ru["title"], "[ru] Pho");
    assert_eq!(ru["ingredients"][0]["raw_text"], "[ru] 1 kg botten");
    assert_eq!(
        ru["ingredients"][0]["metric_amount"], 2000.0,
        "scaling still applies"
    );
    assert_eq!(ru["language"], "nl");
    assert_eq!(ru["translation"]["lang"], "ru");
    assert_eq!(ru["translation"]["origin"], "llm");
    assert_eq!(ru["translation"]["stale"], false);

    let (_, en) = send(
        &app,
        "GET",
        &format!("/api/v1/recipes/{id}"),
        None,
        &[("accept-language", "en-GB,en;q=0.9,nl;q=0.8")],
    )
    .await;
    assert_eq!(
        en["title"], "[en] Pho",
        "Accept-Language used when lang is absent"
    );

    let (_, src) = send(
        &app,
        "GET",
        &format!("/api/v1/recipes/{id}?lang=source"),
        None,
        &[("accept-language", "ru")],
    )
    .await;
    assert_eq!(src["title"], "Pho");
    assert!(src["translation"].is_null());

    let (_, same) = send(
        &app,
        "GET",
        &format!("/api/v1/recipes/{id}?lang=nl"),
        None,
        &[],
    )
    .await;
    assert!(
        same["translation"].is_null(),
        "source language asks for no translation"
    );

    let (status, _) = send(
        &app,
        "GET",
        &format!("/api/v1/recipes/{id}?lang=klingon!"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn human_translations_can_be_put_and_deleted() {
    let app = app(Arc::new(EchoTranslator)).await;
    let (_, r) = send(&app, "POST", "/api/v1/recipes", Some(dutch()), &[]).await;
    let id = r["id"].as_str().unwrap().to_string();

    let body = json!({"title": "Фо", "description": "Суп", "ingredients": ["1 кг костей"], "instructions": ["Варить."], "tags": ["суп"]});
    let (status, t) = send(
        &app,
        "PUT",
        &format!("/api/v1/recipes/{id}/translations/ru"),
        Some(body.clone()),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(t["title"], "Фо");
    assert_eq!(t["translation"]["origin"], "human");

    let (_, ru) = send(
        &app,
        "GET",
        &format!("/api/v1/recipes/{id}?lang=ru"),
        None,
        &[],
    )
    .await;
    assert_eq!(ru["title"], "Фо");

    let mut edited = dutch();
    edited["title"] = json!("Pho 2");
    send(
        &app,
        "PUT",
        &format!("/api/v1/recipes/{id}"),
        Some(edited),
        &[],
    )
    .await;
    let (_, ru) = send(
        &app,
        "GET",
        &format!("/api/v1/recipes/{id}?lang=ru"),
        None,
        &[],
    )
    .await;
    assert_eq!(
        ru["title"], "Фо",
        "human translation kept after a source edit"
    );
    assert_eq!(ru["translation"]["stale"], true);

    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/api/v1/recipes/{id}/translations/ru"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, ru) = send(
        &app,
        "GET",
        &format!("/api/v1/recipes/{id}?lang=ru"),
        None,
        &[],
    )
    .await;
    assert_eq!(ru["title"], "[ru] Pho 2", "regenerated after delete");

    let (status, _) = send(
        &app,
        "PUT",
        "/api/v1/recipes/nope/translations/ru",
        Some(body.clone()),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &app,
        "PUT",
        &format!("/api/v1/recipes/{id}/translations/bad-code!"),
        Some(body),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn without_llm_translation_is_absent_not_an_error() {
    let app = app(Arc::new(HeuristicExtractor)).await;
    let (_, r) = send(&app, "POST", "/api/v1/recipes", Some(dutch()), &[]).await;
    let id = r["id"].as_str().unwrap();
    let (status, ru) = send(
        &app,
        "GET",
        &format!("/api/v1/recipes/{id}?lang=ru"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ru["title"], "Pho");
    assert!(ru["translation"].is_null());
    let (status, langs) = send(&app, "GET", "/api/v1/i18n/languages", None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(langs["ui"], json!(["en", "ru", "nl"]));
    assert_eq!(langs["content_translation"], false);
}

mod labels_and_search {
    use super::*;
    use std::collections::HashMap;

    struct EchoWithTerms;
    impl RecipeExtractor for EchoWithTerms {
        fn normalize_ingredients<'a>(
            &'a self,
            _r: &'a [String],
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
        fn translate_recipe<'a>(
            &'a self,
            req: &'a TranslationRequest,
        ) -> Pin<Box<dyn Future<Output = Result<TranslationReply, ScraperError>> + Send + 'a>>
        {
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

    #[tokio::test]
    async fn labels_on_recipe_and_shopping_list_and_translated_search() {
        let app = app(Arc::new(EchoWithTerms)).await;
        let (_, r) = send(&app, "POST", "/api/v1/recipes", Some(dutch()), &[]).await;
        let id = r["id"].as_str().unwrap().to_string();

        let (_, ru) = send(
            &app,
            "GET",
            &format!("/api/v1/recipes/{id}?lang=ru"),
            None,
            &[],
        )
        .await;
        assert_eq!(
            ru["ingredients"][0]["canonical_label"], "говяжьи кости",
            "seeded dictionary (RN-75) answers before the model"
        );
        let (_, src) = send(&app, "GET", &format!("/api/v1/recipes/{id}"), None, &[]).await;
        assert!(
            src["ingredients"][0].get("canonical_label").is_none(),
            "no label without a display language"
        );

        let (_, list) = send(&app, "GET", "/api/v1/recipes?q=botten&lang=ru", None, &[]).await;
        assert_eq!(
            list["items"][0]["title"], "[ru] Pho",
            "translation from the detail view is reused"
        );
        assert_eq!(list["items"][0]["translation_lang"], "ru");
        let (_, hit) = send(&app, "GET", "/api/v1/recipes?q=%5Bru%5D", None, &[]).await;
        assert_eq!(hit["total"], 1, "translated text is searchable");

        let (status, sl) = send(
            &app,
            "POST",
            "/api/v1/shopping-list?lang=ru",
            Some(json!({"items": [{"recipe_id": id, "servings": 2}]})),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{sl}");
        assert_eq!(sl["basic_groceries"][0]["canonical_label"], "говяжьи кости");
        let (_, sl) = send(
            &app,
            "POST",
            "/api/v1/shopping-list",
            Some(json!({"items": [{"recipe_id": id, "servings": 2}]})),
            &[("accept-language", "nl")],
        )
        .await;
        assert_eq!(
            sl["basic_groceries"][0]["canonical_label"], "runderbotten",
            "Accept-Language applies here too"
        );
    }
}
