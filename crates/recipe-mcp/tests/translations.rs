//! TranslationService: lazy translation, caching, staleness, human overrides, fallbacks.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use recipe_core::{IngredientItem, InstructionStep, MeasurementUnit, RecipeInput, TranslationInput, TranslationOrigin};
use recipe_db::Db;
use recipe_mcp::TranslationService;
use recipe_scraper::llm::{ExtractedRecipeDraft, HeuristicExtractor, RecipeExtractor, TranslationReply, TranslationRequest};
use recipe_scraper::{RecipeImporter, ScraperError};

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct FakeTranslator {
    calls: AtomicUsize,
    short: bool,
}

impl RecipeExtractor for FakeTranslator {
    fn normalize_ingredients<'a>(&'a self, _r: &'a [String]) -> Pin<Box<dyn Future<Output = Result<Vec<IngredientItem>, ScraperError>> + Send + 'a>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn extract_recipe<'a>(&'a self, _m: &'a str) -> Pin<Box<dyn Future<Output = Result<ExtractedRecipeDraft, ScraperError>> + Send + 'a>> {
        Box::pin(async { Err(ScraperError::LlmUnavailable) })
    }
    fn translate_recipe<'a>(&'a self, req: &'a TranslationRequest) -> Pin<Box<dyn Future<Output = Result<TranslationReply, ScraperError>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let mut ingredients: Vec<String> = req.ingredients.iter().map(|s| format!("[{}] {s}", req.target_lang)).collect();
            if self.short { ingredients.pop(); }
            Ok(TranslationReply {
                title: format!("[{}] {}", req.target_lang, req.title),
                description: req.description.as_ref().map(|d| format!("[{}] {d}", req.target_lang)),
                ingredients,
                instructions: req.instructions.iter().map(|s| format!("[{}] {s}", req.target_lang)).collect(),
                tags: req.tags.clone(),
            })
        })
    }
}

fn input() -> RecipeInput {
    RecipeInput {
        title: "Pho".into(), description: Some("Soep".into()), servings: 2, prep_time_minutes: None, cook_time_minutes: None,
        ingredients: vec![
            IngredientItem { raw_text: "1 kg botten".into(), metric_amount: Some(1000.0), unit: MeasurementUnit::G, canonical_id: "beef_bones".into(), is_specialty: false, canonical_label: None },
            IngredientItem { raw_text: "zout".into(), metric_amount: None, unit: MeasurementUnit::ToTaste, canonical_id: "salt".into(), is_specialty: false, canonical_label: None },
        ],
        instructions: vec![InstructionStep { step_number: 1, text: "Kook.".into(), timer_seconds: None }],
        tags: vec!["soep".into()], image_path: None, origin_url: None, author_did: None, is_public: false, language: Some("nl".into()),
    }
}

async fn setup(extractor: Arc<dyn RecipeExtractor>) -> (Db, TranslationService) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("recipe-i18n-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(dir.join("recipes.db")).await.unwrap();
    (db.clone(), TranslationService::new(db, RecipeImporter::new(extractor)))
}

#[tokio::test]
async fn translates_once_then_serves_from_cache() {
    let fake = Arc::new(FakeTranslator { calls: AtomicUsize::new(0), short: false });
    let (db, svc) = setup(fake.clone()).await;
    let r = db.create_recipe(input()).await.unwrap();

    let loc = svc.localize(r.clone(), Some("ru")).await.unwrap();
    assert_eq!(loc.recipe.title, "[ru] Pho");
    assert_eq!(loc.recipe.ingredients[1].raw_text, "[ru] zout");
    assert_eq!(loc.recipe.ingredients[1].canonical_id, "salt");
    let info = loc.translation.unwrap();
    assert_eq!((info.lang.as_str(), info.origin, info.stale), ("ru", TranslationOrigin::Llm, false));

    let again = svc.localize(r.clone(), Some("ru")).await.unwrap();
    assert_eq!(again.recipe.title, "[ru] Pho");
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1, "second view hits the cache");

    let same = svc.localize(r.clone(), Some("nl")).await.unwrap();
    assert_eq!(same.recipe.title, "Pho");
    assert!(same.translation.is_none(), "asking for the source language translates nothing");
    assert!(svc.localize(r.clone(), None).await.unwrap().translation.is_none());
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_requests_share_one_translation() {
    let fake = Arc::new(FakeTranslator { calls: AtomicUsize::new(0), short: false });
    let (db, svc) = setup(fake.clone()).await;
    let r = db.create_recipe(input()).await.unwrap();
    let futs: Vec<_> = (0..5).map(|_| svc.localize(r.clone(), Some("ru"))).collect();
    let results = futures_join_all(futs).await;
    assert!(results.iter().all(|l| l.as_ref().unwrap().recipe.title == "[ru] Pho"));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
}

async fn futures_join_all<F: Future>(futs: Vec<F>) -> Vec<F::Output> {
    let mut pending: Vec<Option<Pin<Box<F>>>> = futs.into_iter().map(|f| Some(Box::pin(f))).collect();
    let mut outputs: Vec<Option<F::Output>> = (0..pending.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut all_done = true;
        for (slot, out) in pending.iter_mut().zip(outputs.iter_mut()) {
            if let Some(f) = slot {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => {
                        *out = Some(v);
                        *slot = None;
                    }
                    std::task::Poll::Pending => all_done = false,
                }
            }
        }
        if all_done { std::task::Poll::Ready(()) } else { std::task::Poll::Pending }
    })
    .await;
    outputs.into_iter().map(|o| o.expect("completed")).collect()
}

#[tokio::test]
async fn stale_llm_translation_is_regenerated_but_human_one_is_kept() {
    let fake = Arc::new(FakeTranslator { calls: AtomicUsize::new(0), short: false });
    let (db, svc) = setup(fake.clone()).await;
    let r = db.create_recipe(input()).await.unwrap();
    svc.localize(r.clone(), Some("ru")).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let mut edited = input(); edited.title = "Pho 2".into();
    let r2 = db.update_recipe(&r.id, edited).await.unwrap().unwrap();
    let loc = svc.localize(r2.clone(), Some("ru")).await.unwrap();
    assert_eq!(loc.recipe.title, "[ru] Pho 2", "stale machine translation regenerated");
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2);

    let human = TranslationInput { title: "Фо по-домашнему".into(), description: None, ingredients: vec!["кости".into(), "соль".into()], instructions: vec!["Варить.".into()], tags: vec![] };
    let saved = svc.set_human_translation(&r.id, "ru", human).await.unwrap().unwrap();
    assert_eq!(saved.translation.as_ref().unwrap().origin, TranslationOrigin::Human);
    assert_eq!(saved.recipe.title, "Фо по-домашнему");

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let mut edited = input(); edited.title = "Pho 3".into();
    let r3 = db.update_recipe(&r.id, edited).await.unwrap().unwrap();
    let loc = svc.localize(r3.clone(), Some("ru")).await.unwrap();
    assert_eq!(loc.recipe.title, "Фо по-домашнему", "human text survives a source edit");
    assert!(loc.translation.unwrap().stale);
    assert_eq!(fake.calls.load(Ordering::SeqCst), 2, "no regeneration over a human translation");

    assert!(svc.delete_translation(&r.id, "ru").await.unwrap());
    let loc = svc.localize(r3, Some("ru")).await.unwrap();
    assert_eq!(loc.recipe.title, "[ru] Pho 3");
    assert_eq!(fake.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn failures_fall_back_to_the_source_language() {
    let (db, svc) = setup(Arc::new(FakeTranslator { calls: AtomicUsize::new(0), short: true })).await;
    let r = db.create_recipe(input()).await.unwrap();
    let loc = svc.localize(r.clone(), Some("ru")).await.unwrap();
    assert_eq!(loc.recipe.title, "Pho", "incomplete reply: source shown");
    assert!(loc.translation.is_none());
    assert!(db.get_translation(&r.id, "ru").await.unwrap().is_none(), "nothing cached");

    let (db, svc) = setup(Arc::new(HeuristicExtractor)).await;
    let r = db.create_recipe(input()).await.unwrap();
    let loc = svc.localize(r, Some("ru")).await.unwrap();
    assert_eq!(loc.recipe.title, "Pho");
    assert!(loc.translation.is_none(), "no LLM: source, not an error");
    assert!(!svc.can_translate());
}

mod labels {
    use super::*;
    use std::collections::HashMap;

    struct TermFake { calls: AtomicUsize, batch_sizes: std::sync::Mutex<Vec<usize>> }
    impl RecipeExtractor for TermFake {
        fn normalize_ingredients<'a>(&'a self, _r: &'a [String]) -> Pin<Box<dyn Future<Output = Result<Vec<IngredientItem>, ScraperError>> + Send + 'a>> { Box::pin(async { Ok(vec![]) }) }
        fn extract_recipe<'a>(&'a self, _m: &'a str) -> Pin<Box<dyn Future<Output = Result<ExtractedRecipeDraft, ScraperError>> + Send + 'a>> { Box::pin(async { Err(ScraperError::LlmUnavailable) }) }
        fn translate_terms<'a>(&'a self, ids: &'a [String], lang: &'a str) -> Pin<Box<dyn Future<Output = Result<HashMap<String, String>, ScraperError>> + Send + 'a>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.batch_sizes.lock().unwrap().push(ids.len());
                Ok(ids.iter().filter(|id| id.as_str() != "mystery").map(|id| (id.clone(), format!("{lang}:{id}"))).collect())
            })
        }
    }

    #[tokio::test]
    async fn labels_are_fetched_once_cached_and_batched() {
        let fake = Arc::new(TermFake { calls: AtomicUsize::new(0), batch_sizes: std::sync::Mutex::new(vec![]) });
        let (_, svc) = setup(fake.clone()).await;
        let ids: Vec<String> = vec!["garlic".into(), "salt".into(), "mystery".into()];
        let m = svc.labels(&ids, "ru").await.unwrap();
        assert_eq!(m["garlic"], "ru:garlic");
        assert_eq!(m["mystery"], "mystery", "unanswered ids fall back to the humanized id");
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);

        let m = svc.labels(&["garlic".into(), "black_pepper".into()], "ru").await.unwrap();
        assert_eq!(m["black_pepper"], "ru:black_pepper");
        assert_eq!(fake.calls.load(Ordering::SeqCst), 2, "only the missing id triggers a call");
        assert_eq!(*fake.batch_sizes.lock().unwrap().last().unwrap(), 1);

        let many: Vec<String> = (0..230).map(|i| format!("item_{i}")).collect();
        svc.labels(&many, "nl").await.unwrap();
        let sizes = fake.batch_sizes.lock().unwrap();
        assert_eq!(sizes[sizes.len() - 3..].to_vec(), vec![100, 100, 30], "batched by 100");
    }

    #[tokio::test]
    async fn labels_without_llm_are_humanized_ids() {
        let (_, svc) = setup(Arc::new(HeuristicExtractor)).await;
        let m = svc.labels(&["dark_soy_sauce".into()], "ru").await.unwrap();
        assert_eq!(m["dark_soy_sauce"], "dark soy sauce");
    }

    #[tokio::test]
    async fn localized_recipe_carries_labels_for_ingredients() {
        let fake = Arc::new(TermFake { calls: AtomicUsize::new(0), batch_sizes: std::sync::Mutex::new(vec![]) });
        let (db, svc) = setup(fake).await;
        let r = db.create_recipe(input()).await.unwrap();
        let loc = svc.localize(r.clone(), Some("ru")).await.unwrap();
        assert_eq!(loc.recipe.ingredients[0].canonical_label.as_deref(), Some("ru:beef_bones"));
        assert_eq!(loc.recipe.title, "Pho", "this fake does not translate recipes; labels still apply");
        let src = svc.localize(r, None).await.unwrap();
        assert_eq!(src.recipe.ingredients[0].canonical_label, None, "no label without a display language");
    }
}

mod backfill {
    use super::*;

    fn recipe(title: &str, ingredients: &[&str], step: &str) -> RecipeInput {
        let mut i = input();
        i.title = title.into();
        i.language = None;
        i.ingredients = ingredients.iter().map(|r| IngredientItem { raw_text: r.to_string(), metric_amount: None, unit: MeasurementUnit::ToTaste, canonical_id: "x".into(), is_specialty: false, canonical_label: None }).collect();
        i.instructions = vec![InstructionStep { step_number: 1, text: step.into(), timer_seconds: None }];
        i
    }

    #[tokio::test]
    async fn backfill_fills_detectable_languages_and_leaves_the_rest() {
        let (db, svc) = setup(Arc::new(HeuristicExtractor)).await;
        let nl = db.create_recipe(recipe("Stamppot", &["1 kg aardappelen", "een scheutje melk"], "Kook de aardappelen en stamp alles fijn met de melk en de boter.")).await.unwrap();
        let ru = db.create_recipe(recipe("Борщ", &["свёкла", "капуста"], "Нарежьте свёклу и варите с капустой на медленном огне около часа.")).await.unwrap();
        let unknown = db.create_recipe(recipe("Pho", &["x"], "250 g")).await.unwrap();
        let mut set = input(); set.title = "Already set".into(); set.language = Some("en".into());
        let en = db.create_recipe(set).await.unwrap();

        let report = svc.backfill_languages().await.unwrap();
        assert_eq!((report.scanned, report.detected, report.undetected), (3, 2, 1));
        assert_eq!(db.get_recipe(&nl.id).await.unwrap().unwrap().language.as_deref(), Some("nl"));
        assert_eq!(db.get_recipe(&ru.id).await.unwrap().unwrap().language.as_deref(), Some("ru"));
        assert_eq!(db.get_recipe(&unknown.id).await.unwrap().unwrap().language, None);
        assert_eq!(db.get_recipe(&en.id).await.unwrap().unwrap().language.as_deref(), Some("en"));

        let report = svc.backfill_languages().await.unwrap();
        assert_eq!(report.scanned, 1, "only the undetected one is revisited");
        assert_eq!(report.detected, 0);
    }
}
