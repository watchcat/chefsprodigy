//! platform v1 (docs/SPEC-hosted-platform.md section 5.3): hub registry, federation
//! poller, recipes/translations index with per-language FTS rows, fork grouping and
//! the search API, driven against fake hubs that serve the hub's real federation DTOs.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use http_body_util::BodyExt;
use platform::{AppState, Config, Db, HubRegistration, PollMode, build_router};
use recipe_core::{
    FederatedRecipe, FederationManifest, IngredientItem, InstructionStep, ManifestEntry,
    ManifestTranslation, MeasurementUnit, Recipe, RecipeTranslation, TranslationOrigin,
    calculate_content_hash,
};
use serde_json::{Value, json};
use tower::ServiceExt;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("platform-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ----------------------------------------------------------------------------- fake hub

#[derive(Default)]
struct HubData {
    chef_name: String,
    recipes: BTreeMap<String, FederatedRecipe>,
    fail_manifest: bool,
    redirect_manifest_to: Option<String>,
    padding: usize,
    manifest_hits: usize,
    recipe_hits: HashMap<String, usize>,
    /// Listed in the manifest but already gone when the recipe is fetched (a race).
    vanish_on_fetch: HashSet<String>,
}

/// A stand-in hub serving `GET /api/v1/federation/manifest` and
/// `GET /api/v1/federation/recipes/{id}` with recipe-core's DTOs.
#[derive(Clone)]
struct FakeHub {
    data: Arc<Mutex<HubData>>,
    url: String,
}

impl FakeHub {
    async fn start(chef: &str) -> Self {
        let data = Arc::new(Mutex::new(HubData {
            chef_name: chef.to_string(),
            ..HubData::default()
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hub = Self { data, url };
        let app = Router::new()
            .route("/api/v1/federation/manifest", get(fake_manifest))
            .route("/api/v1/federation/recipes/{id}", get(fake_recipe))
            .with_state(hub.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        hub
    }

    fn put(&self, recipe: Recipe) {
        let mut d = self.data.lock().unwrap();
        let translations = d
            .recipes
            .get(&recipe.id)
            .map(|f| f.translations.clone())
            .unwrap_or_default();
        d.recipes.insert(
            recipe.id.clone(),
            FederatedRecipe {
                recipe,
                translations,
            },
        );
    }

    fn translate(&self, id: &str, lang: &str, title: &str, tags: &[&str], updated_at: &str) {
        let mut d = self.data.lock().unwrap();
        let fed = d.recipes.get_mut(id).unwrap();
        fed.translations.retain(|t| t.lang != lang);
        fed.translations.push(RecipeTranslation {
            lang: lang.to_string(),
            title: title.to_string(),
            description: None,
            ingredients: vec![],
            instructions: vec![],
            tags: tags.iter().map(|t| t.to_string()).collect(),
            origin: TranslationOrigin::Human,
            source_updated_at: fed.recipe.updated_at.clone(),
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
        });
    }

    fn drop_translation(&self, id: &str, lang: &str) {
        let mut d = self.data.lock().unwrap();
        d.recipes
            .get_mut(id)
            .unwrap()
            .translations
            .retain(|t| t.lang != lang);
    }

    fn unpublish(&self, id: &str) {
        self.data
            .lock()
            .unwrap()
            .recipes
            .get_mut(id)
            .unwrap()
            .recipe
            .is_public = false;
    }

    fn touch(&self, id: &str, updated_at: &str) {
        let mut d = self.data.lock().unwrap();
        d.recipes.get_mut(id).unwrap().recipe.updated_at = updated_at.to_string();
    }

    fn set_failing(&self, failing: bool) {
        self.data.lock().unwrap().fail_manifest = failing;
    }

    fn redirect_manifest(&self, to: &str) {
        self.data.lock().unwrap().redirect_manifest_to = Some(to.to_string());
    }

    fn vanish_on_fetch(&self, id: &str) {
        self.data
            .lock()
            .unwrap()
            .vanish_on_fetch
            .insert(id.to_string());
    }

    fn pad_manifest(&self, bytes: usize) {
        self.data.lock().unwrap().padding = bytes;
    }

    fn manifest_hits(&self) -> usize {
        self.data.lock().unwrap().manifest_hits
    }

    fn recipe_hits(&self, id: &str) -> usize {
        self.data
            .lock()
            .unwrap()
            .recipe_hits
            .get(id)
            .copied()
            .unwrap_or(0)
    }
}

async fn fake_manifest(State(hub): State<FakeHub>) -> Response {
    let mut d = hub.data.lock().unwrap();
    d.manifest_hits += 1;
    if d.fail_manifest {
        return (StatusCode::INTERNAL_SERVER_ERROR, "database is locked").into_response();
    }
    if let Some(to) = &d.redirect_manifest_to {
        return Redirect::temporary(to).into_response();
    }
    let recipes = d
        .recipes
        .values()
        .filter(|f| f.recipe.is_public)
        .map(|f| ManifestEntry {
            id: f.recipe.id.clone(),
            updated_at: f.recipe.updated_at.clone(),
            content_hash: f.recipe.content_hash.clone(),
            translations: f
                .translations
                .iter()
                .map(|t| ManifestTranslation {
                    lang: t.lang.clone(),
                    origin: t.origin,
                    stale: t.is_stale_for(&f.recipe),
                    updated_at: t.updated_at.clone(),
                })
                .collect(),
        })
        .collect();
    let manifest = FederationManifest {
        author_did: None,
        chef_name: d.chef_name.clone(),
        node_url: None,
        recipes,
    };
    if d.padding > 0 {
        let mut value = serde_json::to_value(&manifest).unwrap();
        value["padding"] = Value::String("x".repeat(d.padding));
        return Json(value).into_response();
    }
    Json(manifest).into_response()
}

async fn fake_recipe(State(hub): State<FakeHub>, Path(id): Path<String>) -> Response {
    let mut d = hub.data.lock().unwrap();
    *d.recipe_hits.entry(id.clone()).or_default() += 1;
    match d.recipes.get(&id) {
        Some(f) if f.recipe.is_public && !d.vanish_on_fetch.contains(&id) => {
            Json(f.clone()).into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "recipe not found"})),
        )
            .into_response(),
    }
}

const T0: &str = "2026-10-01T10:00:00Z";

fn recipe(
    id: &str,
    title: &str,
    lang: &str,
    tags: &[&str],
    canonical: &[&str],
    steps: &[&str],
) -> Recipe {
    let mut r = Recipe {
        id: id.to_string(),
        title: title.to_string(),
        description: None,
        servings: 4,
        prep_time_minutes: Some(10),
        cook_time_minutes: Some(60),
        ingredients: canonical
            .iter()
            .map(|c| IngredientItem {
                raw_text: c.replace('_', " "),
                metric_amount: Some(100.0),
                unit: MeasurementUnit::G,
                canonical_id: c.to_string(),
                is_specialty: false,
                canonical_label: None,
            })
            .collect(),
        instructions: steps
            .iter()
            .enumerate()
            .map(|(i, s)| InstructionStep {
                step_number: i as u16 + 1,
                text: s.to_string(),
                timer_seconds: None,
            })
            .collect(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        image_path: Some(format!("images/{id}.webp")),
        origin_url: None,
        author_did: None,
        content_hash: String::new(),
        is_public: true,
        created_at: T0.to_string(),
        updated_at: T0.to_string(),
        language: Some(lang.to_string()),
    };
    r.content_hash = calculate_content_hash(&r);
    r
}

const SOUP: &str = "01JACOB0000000000000000SOUP";
const STEW: &str = "01JACOB0000000000000000STEW";
const PIE: &str = "01MARIA00000000000000000PIE";

fn erwtensoep() -> Recipe {
    recipe(
        SOUP,
        "Erwtensoep",
        "nl",
        &["soep", "winter"],
        &["split_pea", "smoked_sausage", "celeriac"],
        &["Week de erwten.", "Kook alles twee uur."],
    )
}

fn hachee() -> Recipe {
    recipe(
        STEW,
        "Hachee",
        "nl",
        &["stoofpot"],
        &["beef", "onion", "garlic"],
        &["Bak het vlees.", "Stoof drie uur."],
    )
}

fn apple_pie() -> Recipe {
    recipe(
        PIE,
        "Apple pie",
        "en",
        &["dessert", "baking"],
        &["apple", "butter", "flour"],
        &["Make the dough.", "Bake for an hour."],
    )
}

// ----------------------------------------------------------------------------- platform

struct Platform {
    app: Router,
    state: AppState,
}

fn config() -> Config {
    Config {
        admin_token: Some("admin-secret".into()),
        ..Config::default()
    }
}

async fn platform(config: Config) -> Platform {
    let db = Db::open(temp_dir().join("platform.db")).await.unwrap();
    let state = AppState::new(db, config).unwrap();
    Platform {
        app: build_router(state.clone()),
        state,
    }
}

impl Platform {
    async fn register(&self, name: &str, hub: &FakeHub) {
        self.state
            .db()
            .upsert_hub(&HubRegistration {
                name: name.to_string(),
                internal_url: Some(hub.url.clone()),
                public_url: format!("https://{name}.chefslegacy.com"),
                chef_name: None,
            })
            .await
            .unwrap();
    }

    async fn poll(&self) -> Vec<platform::PollReport> {
        self.state.poller().poll(PollMode::Force).await.unwrap()
    }

    async fn search(&self, pairs: &[(&str, &str)]) -> Value {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs)
            .finish();
        let (status, body) = self.get(&format!("/api/v1/search?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        send(&self.app, Request::get(uri).body(Body::empty()).unwrap()).await
    }

    async fn count(&self, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.state.db().pool())
            .await
            .unwrap()
    }
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
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

async fn send_raw(
    app: &Router,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, bytes)
}

fn admin(method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    match body {
        Some(body) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    }
}

fn hits(page: &Value) -> &Vec<Value> {
    page["hits"].as_array().expect("hits array")
}

fn titles(page: &Value) -> Vec<String> {
    hits(page)
        .iter()
        .map(|h| h["title"].as_str().unwrap().to_string())
        .collect()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

fn ts(value: &Option<String>) -> chrono::DateTime<chrono::FixedOffset> {
    chrono::DateTime::parse_from_rfc3339(value.as_deref().expect("timestamp")).unwrap()
}

// ----------------------------------------------------------------------------- acceptance

#[tokio::test]
async fn two_hubs_are_indexed_from_their_manifests() {
    let jacob = FakeHub::start("Jacob").await;
    jacob.put(erwtensoep());
    jacob.put(hachee());
    let maria = FakeHub::start("Maria").await;
    maria.put(apple_pie());
    let p = platform(config()).await;
    p.register("jacob", &jacob).await;
    p.register("maria", &maria).await;

    let reports = p.poll().await;
    assert_eq!(reports.len(), 2);
    for r in &reports {
        assert!(r.ok, "{r:?}");
    }
    let jacob_report = reports.iter().find(|r| r.hub == "jacob").unwrap();
    assert_eq!((jacob_report.fetched, jacob_report.removed), (2, 0));

    let all = p.search(&[]).await;
    assert_eq!(all["total"], 3);
    let mut found = titles(&all);
    found.sort();
    assert_eq!(found, ["Apple pie", "Erwtensoep", "Hachee"]);

    let page = p.search(&[("q", "erwtensoep")]).await;
    assert_eq!(page["total"], 1);
    let hit = &hits(&page)[0];
    assert_eq!(hit["hub"], "jacob");
    assert_eq!(hit["public_url"], "https://jacob.chefslegacy.com");
    assert_eq!(hit["recipe_id"], SOUP);
    assert_eq!(
        hit["url"],
        format!("https://jacob.chefslegacy.com/#/recipe/{SOUP}")
    );
    assert_eq!(
        hit["image_url"],
        format!("https://jacob.chefslegacy.com/images/{SOUP}.webp"),
        "cover URL = public_url + image path"
    );
    assert_eq!(hit["source_language"], "nl");
    assert_eq!(hit["matched_lang"], "nl");
    assert_eq!(strings(&hit["tags"]), ["soep", "winter"]);
    assert_eq!(hit["also_at"], json!([]));

    let page = p.search(&[("q", "dessert")]).await;
    assert_eq!(titles(&page), ["Apple pie"], "tags are searchable");
    assert_eq!(hits(&page)[0]["hub"], "maria");

    let (status, dir) = p.get("/api/v1/hubs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        dir,
        json!({"hubs": [
            {"name": "jacob", "chef_name": "Jacob", "public_url": "https://jacob.chefslegacy.com", "recipe_count": 2},
            {"name": "maria", "chef_name": "Maria", "public_url": "https://maria.chefslegacy.com", "recipe_count": 1},
        ]}),
        "the chef name comes from the manifest when the registry has none"
    );
    assert!(
        !dir.to_string().contains("127.0.0.1"),
        "internal URLs stay private"
    );

    let hub = p.state.db().get_hub("jacob").await.unwrap().unwrap();
    assert!(hub.last_polled_at.is_some());
    assert_eq!(hub.last_ok_at, hub.last_polled_at);
    assert_eq!(hub.last_error, None);
}

#[tokio::test]
async fn a_translation_added_on_a_hub_appears_in_ru_search_after_the_next_poll() {
    let jacob = FakeHub::start("Jacob").await;
    jacob.put(erwtensoep());
    let p = platform(config()).await;
    p.register("jacob", &jacob).await;
    p.poll().await;
    assert_eq!(
        p.search(&[("q", "гороховый"), ("lang", "ru")]).await["total"],
        0
    );
    let before = erwtensoep();

    jacob.translate(
        SOUP,
        "ru",
        "Гороховый суп",
        &["суп", "зима"],
        "2026-10-05T08:00:00Z",
    );
    let reports = p.poll().await;
    assert_eq!(
        reports[0].fetched, 1,
        "the new translation alone triggers a fetch"
    );
    assert_eq!(jacob.recipe_hits(SOUP), 2);
    let after = jacob.data.lock().unwrap().recipes[SOUP].recipe.clone();
    assert_eq!(
        (after.updated_at, after.content_hash),
        (before.updated_at, before.content_hash),
        "the recipe itself did not change"
    );

    let page = p.search(&[("q", "гороховый"), ("lang", "ru")]).await;
    assert_eq!(page["total"], 1);
    let hit = &hits(&page)[0];
    assert_eq!(hit["title"], "Гороховый суп");
    assert_eq!(strings(&hit["tags"]), ["суп", "зима"]);
    assert_eq!(hit["matched_lang"], "ru");
    assert_eq!(hit["source_language"], "nl");
    assert_eq!(hit["recipe_id"], SOUP);

    let page = p.search(&[("q", "суп")]).await;
    assert_eq!(
        titles(&page),
        ["Гороховый суп"],
        "translated tags are searchable"
    );
    assert_eq!(
        hits(&page)[0]["matched_lang"],
        "ru",
        "without lang the matching row's language is shown"
    );
}

#[tokio::test]
async fn forks_sharing_a_content_hash_are_grouped_with_also_at() {
    let jacob = FakeHub::start("Jacob").await;
    jacob.put(erwtensoep());
    let maria = FakeHub::start("Maria").await;
    let mut fork = erwtensoep();
    fork.id = "01MARIA0000000000000000SOUP".into();
    fork.title = "Snert".into();
    fork.tags = vec!["soep".into()];
    fork.content_hash = calculate_content_hash(&fork);
    assert_eq!(fork.content_hash, erwtensoep().content_hash, "same dish");
    maria.put(fork);
    maria.put(apple_pie());
    let p = platform(config()).await;
    p.register("jacob", &jacob).await;
    p.register("maria", &maria).await;
    p.poll().await;

    let page = p.search(&[("q", "erwtensoep")]).await;
    assert_eq!(page["total"], 1);
    let hit = &hits(&page)[0];
    assert_eq!(hit["hub"], "jacob");
    assert_eq!(
        hit["also_at"],
        json!([{
            "hub": "maria",
            "public_url": "https://maria.chefslegacy.com",
            "recipe_id": "01MARIA0000000000000000SOUP",
            "url": "https://maria.chefslegacy.com/#/recipe/01MARIA0000000000000000SOUP",
        }])
    );

    let page = p.search(&[("q", "snert")]).await;
    assert_eq!(hits(&page)[0]["hub"], "maria", "the matching copy leads");
    assert_eq!(hits(&page)[0]["also_at"][0]["hub"], "jacob");

    let page = p.search(&[("q", "soep")]).await;
    assert_eq!(page["total"], 1, "both copies match but form one dish");
    assert_eq!(hits(&page)[0]["also_at"].as_array().unwrap().len(), 1);

    let all = p.search(&[]).await;
    assert_eq!(all["total"], 2, "three recipes, two dishes");
    assert_eq!(p.count("SELECT COUNT(*) FROM forks").await, 1);
}

#[tokio::test]
async fn unpublished_recipes_and_removed_translations_leave_the_index_on_the_next_poll() {
    let jacob = FakeHub::start("Jacob").await;
    jacob.put(erwtensoep());
    jacob.put(hachee());
    jacob.translate(
        SOUP,
        "ru",
        "Гороховый суп",
        &["суп"],
        "2026-10-05T08:00:00Z",
    );
    jacob.translate(SOUP, "en", "Pea soup", &["soup"], "2026-10-05T08:00:00Z");
    jacob.translate(STEW, "ru", "Хаше", &["рагу"], "2026-10-05T08:00:00Z");
    let p = platform(config()).await;
    p.register("jacob", &jacob).await;
    p.poll().await;
    assert_eq!(p.count("SELECT COUNT(*) FROM recipes_index").await, 2);
    assert_eq!(p.count("SELECT COUNT(*) FROM translations_index").await, 3);
    assert_eq!(
        p.count("SELECT COUNT(*) FROM recipes_fts").await,
        5,
        "one FTS row per (hub, recipe, lang)"
    );
    assert_eq!(titles(&p.search(&[("q", "хаше")]).await), ["Хаше"]);

    jacob.unpublish(STEW);
    jacob.drop_translation(SOUP, "ru");
    let reports = p.poll().await;
    assert!(reports[0].ok, "{:?}", reports[0]);
    assert_eq!(reports[0].removed, 1);

    assert_eq!(p.search(&[("q", "хаше")]).await["total"], 0);
    assert_eq!(p.search(&[("q", "hachee")]).await["total"], 0);
    assert_eq!(
        p.search(&[("q", "гороховый"), ("lang", "ru")]).await["total"],
        0
    );
    let page = p.search(&[("q", "erwtensoep"), ("lang", "ru")]).await;
    assert_eq!(
        titles(&page),
        ["Erwtensoep"],
        "no ru translation any more: source wording"
    );
    assert_eq!(hits(&page)[0]["matched_lang"], "nl");
    assert_eq!(titles(&p.search(&[("q", "pea")]).await), ["Pea soup"]);
    assert_eq!(p.count("SELECT COUNT(*) FROM recipes_index").await, 1);
    assert_eq!(p.count("SELECT COUNT(*) FROM translations_index").await, 1);
    assert_eq!(p.count("SELECT COUNT(*) FROM recipes_fts").await, 2);

    // Listed in the manifest but gone by the time it is fetched: removed as well.
    jacob.touch(SOUP, "2026-10-09T09:00:00Z");
    jacob.vanish_on_fetch(SOUP);
    let reports = p.poll().await;
    assert!(reports[0].ok, "{:?}", reports[0]);
    assert_eq!(p.count("SELECT COUNT(*) FROM recipes_index").await, 0);
    assert_eq!(p.count("SELECT COUNT(*) FROM recipes_fts").await, 0);
}

// ----------------------------------------------------------------------------- poller

#[tokio::test]
async fn only_changed_recipes_and_translations_are_fetched() {
    let jacob = FakeHub::start("Jacob").await;
    jacob.put(erwtensoep());
    jacob.put(hachee());
    let p = platform(config()).await;
    p.register("jacob", &jacob).await;
    p.poll().await;
    assert_eq!((jacob.recipe_hits(SOUP), jacob.recipe_hits(STEW)), (1, 1));

    let reports = p.poll().await;
    assert_eq!((reports[0].fetched, reports[0].unchanged), (0, 2));
    assert_eq!((jacob.recipe_hits(SOUP), jacob.recipe_hits(STEW)), (1, 1));

    let mut edited = hachee();
    edited.title = "Hachee met rode kool".into();
    edited.updated_at = "2026-10-07T12:00:00Z".into();
    jacob.put(edited);
    p.poll().await;
    assert_eq!((jacob.recipe_hits(SOUP), jacob.recipe_hits(STEW)), (1, 2));
    assert_eq!(
        titles(&p.search(&[("q", "kool")]).await),
        ["Hachee met rode kool"]
    );

    jacob.translate(SOUP, "ru", "Гороховый суп", &[], "2026-10-08T08:00:00Z");
    p.poll().await;
    assert_eq!((jacob.recipe_hits(SOUP), jacob.recipe_hits(STEW)), (2, 2));
    jacob.translate(SOUP, "ru", "Суп из гороха", &[], "2026-10-09T08:00:00Z");
    p.poll().await;
    assert_eq!(
        jacob.recipe_hits(SOUP),
        3,
        "a translation's own updated_at counts"
    );
    assert_eq!(
        titles(&p.search(&[("q", "гороха"), ("lang", "ru")]).await),
        ["Суп из гороха"]
    );
    assert_eq!(
        p.search(&[("q", "гороховый")]).await["total"],
        0,
        "the old wording is gone"
    );
}

#[tokio::test]
async fn a_failing_hub_backs_off_and_records_last_error() {
    let jacob = FakeHub::start("Jacob").await;
    jacob.put(erwtensoep());
    let mut cfg = config();
    cfg.backoff_base = Duration::from_secs(60);
    cfg.backoff_max = Duration::from_secs(150);
    let p = platform(cfg).await;
    p.register("jacob", &jacob).await;
    p.state.poller().poll(PollMode::Due).await.unwrap();
    assert_eq!(p.search(&[]).await["total"], 1);
    let ok_at = p
        .state
        .db()
        .get_hub("jacob")
        .await
        .unwrap()
        .unwrap()
        .last_ok_at;
    assert!(ok_at.is_some());

    jacob.set_failing(true);
    let reports = p.poll().await;
    assert!(!reports[0].ok);
    assert!(
        reports[0].error.as_deref().unwrap().contains("500"),
        "{:?}",
        reports[0]
    );
    let hub = p.state.db().get_hub("jacob").await.unwrap().unwrap();
    assert_eq!(hub.consecutive_failures, 1);
    assert!(hub.last_error.as_deref().unwrap().contains("500"));
    assert_eq!(hub.last_ok_at, ok_at, "last_ok_at keeps the last success");
    assert!(ts(&hub.last_polled_at) >= ts(&hub.last_ok_at));
    assert_eq!(
        ts(&hub.next_poll_at) - ts(&hub.last_polled_at),
        chrono::Duration::seconds(60)
    );

    let manifest_hits = jacob.manifest_hits();
    let reports = p.state.poller().poll(PollMode::Due).await.unwrap();
    assert!(reports.is_empty(), "a backed-off hub is not due yet");
    assert_eq!(jacob.manifest_hits(), manifest_hits);

    p.poll().await;
    let hub = p.state.db().get_hub("jacob").await.unwrap().unwrap();
    assert_eq!(hub.consecutive_failures, 2);
    assert_eq!(
        ts(&hub.next_poll_at) - ts(&hub.last_polled_at),
        chrono::Duration::seconds(120),
        "exponential"
    );
    p.poll().await;
    let hub = p.state.db().get_hub("jacob").await.unwrap().unwrap();
    assert_eq!(
        ts(&hub.next_poll_at) - ts(&hub.last_polled_at),
        chrono::Duration::seconds(150),
        "capped"
    );
    assert_eq!(
        p.search(&[]).await["total"],
        1,
        "a failing hub keeps its index"
    );

    jacob.set_failing(false);
    let reports = p.poll().await;
    assert!(reports[0].ok);
    let hub = p.state.db().get_hub("jacob").await.unwrap().unwrap();
    assert_eq!(hub.consecutive_failures, 0);
    assert_eq!(hub.last_error, None);
    assert_eq!(hub.next_poll_at, None);
    assert_eq!(hub.last_ok_at, hub.last_polled_at);
}

#[tokio::test]
async fn redirects_to_another_host_are_not_followed() {
    let elsewhere = FakeHub::start("Elsewhere").await;
    elsewhere.put(apple_pie());
    let jacob = FakeHub::start("Jacob").await;
    jacob.redirect_manifest(&format!("{}/api/v1/federation/manifest", elsewhere.url));
    let p = platform(config()).await;
    p.register("jacob", &jacob).await;
    let reports = p.poll().await;
    assert!(!reports[0].ok);
    assert_eq!(
        elsewhere.manifest_hits(),
        0,
        "the redirect was not followed"
    );
    assert_eq!(p.search(&[]).await["total"], 0);
}

#[tokio::test]
async fn oversized_manifests_are_refused() {
    let jacob = FakeHub::start("Jacob").await;
    jacob.put(erwtensoep());
    jacob.pad_manifest(4096);
    let mut cfg = config();
    cfg.max_manifest_bytes = 1024;
    let p = platform(cfg).await;
    p.register("jacob", &jacob).await;
    let reports = p.poll().await;
    assert!(!reports[0].ok);
    assert!(
        reports[0].error.as_deref().unwrap().contains("too large"),
        "{:?}",
        reports[0]
    );
    assert_eq!(jacob.recipe_hits(SOUP), 0);
}

#[tokio::test]
async fn the_background_loop_polls_registered_hubs_and_stops_on_shutdown() {
    let jacob = FakeHub::start("Jacob").await;
    jacob.put(erwtensoep());
    let mut cfg = config();
    cfg.poll_interval = Duration::from_millis(50);
    cfg.poll_jitter = Duration::ZERO;
    let p = platform(cfg).await;
    p.register("jacob", &jacob).await;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(p.state.poller().clone().run(stopped));

    let mut indexed = false;
    for _ in 0..100 {
        if p.search(&[]).await["total"] == 1 {
            indexed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(indexed, "the loop indexed the hub");
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the loop stops on shutdown")
        .unwrap();
}

// ----------------------------------------------------------------------------- search

#[tokio::test]
async fn search_prefers_the_requested_language_and_falls_back_to_the_source() {
    let jacob = FakeHub::start("Jacob").await;
    jacob.put(erwtensoep());
    jacob.put(hachee());
    jacob.translate(
        SOUP,
        "ru",
        "Гороховый суп",
        &["суп", "зима"],
        "2026-10-05T08:00:00Z",
    );
    let p = platform(config()).await;
    p.register("jacob", &jacob).await;
    p.poll().await;

    let page = p.search(&[("q", "erwtensoep"), ("lang", "ru")]).await;
    let hit = &hits(&page)[0];
    assert_eq!(
        hit["title"], "Гороховый суп",
        "a source match is shown in the requested language"
    );
    assert_eq!(strings(&hit["tags"]), ["суп", "зима"]);
    assert_eq!(hit["matched_lang"], "ru");

    let page = p.search(&[("q", "erwtensoep"), ("lang", "ru-RU")]).await;
    assert_eq!(page["lang"], "ru", "lang is normalized");
    assert_eq!(hits(&page)[0]["matched_lang"], "ru");

    let page = p.search(&[("q", "erwtensoep"), ("lang", "de")]).await;
    let hit = &hits(&page)[0];
    assert_eq!(
        hit["title"], "Erwtensoep",
        "no de translation: source wording"
    );
    assert_eq!(strings(&hit["tags"]), ["soep", "winter"]);
    assert_eq!(hit["matched_lang"], "nl");

    let page = p.search(&[("lang", "ru")]).await;
    let mut found = titles(&page);
    found.sort();
    assert_eq!(
        found,
        ["Hachee", "Гороховый суп"],
        "browsing in ru mixes translations and sources"
    );

    let page = p.search(&[("q", "гороховый")]).await;
    assert_eq!(titles(&page), ["Гороховый суп"]);
    assert_eq!(hits(&page)[0]["matched_lang"], "ru");
}

#[tokio::test]
async fn ingredient_filter_matches_canonical_ids() {
    let jacob = FakeHub::start("Jacob").await;
    jacob.put(erwtensoep());
    jacob.put(hachee());
    let maria = FakeHub::start("Maria").await;
    maria.put(apple_pie());
    let p = platform(config()).await;
    p.register("jacob", &jacob).await;
    p.register("maria", &maria).await;
    p.poll().await;

    assert_eq!(
        titles(&p.search(&[("ingredient", "garlic")]).await),
        ["Hachee"]
    );
    assert_eq!(
        titles(&p.search(&[("ingredient", "Smoked Sausage")]).await),
        ["Erwtensoep"],
        "normalized like a canonical id"
    );
    assert_eq!(
        p.search(&[("ingredient", "garlic,apple")]).await["total"],
        0,
        "several ingredients must all be present"
    );
    assert_eq!(
        titles(&p.search(&[("q", "stoofpot"), ("ingredient", "beef")]).await),
        ["Hachee"]
    );
    assert_eq!(
        titles(&p.search(&[("q", "celeriac")]).await),
        ["Erwtensoep"],
        "canonical ids are in the FTS row"
    );
    assert_eq!(
        p.search(&[("q", "\"*)(")]).await["total"],
        3,
        "a query without searchable tokens lists everything"
    );
}

#[tokio::test]
async fn search_pages_through_dish_groups() {
    let jacob = FakeHub::start("Jacob").await;
    for i in 0..5 {
        let mut r = recipe(
            &format!("01JACOB00000000000000SOUP{i}"),
            &format!("Soup number {i}"),
            "en",
            &["soup"],
            &["water", &format!("vegetable_{i}")],
            &["Boil."],
        );
        r.updated_at = format!("2026-10-0{}T10:00:00Z", i + 1);
        jacob.put(r);
    }
    let p = platform(config()).await;
    p.register("jacob", &jacob).await;
    p.poll().await;

    let first = p.search(&[("q", "soup"), ("limit", "2")]).await;
    assert_eq!(
        (first["total"].clone(), first["limit"].clone()),
        (json!(5), json!(2))
    );
    let second = p
        .search(&[("q", "soup"), ("limit", "2"), ("offset", "2")])
        .await;
    let third = p
        .search(&[("q", "soup"), ("limit", "2"), ("offset", "4")])
        .await;
    let mut seen: Vec<String> = [&first, &second, &third]
        .iter()
        .flat_map(|page| titles(page))
        .collect();
    assert_eq!(seen.len(), 5);
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 5, "pages do not overlap");
    assert_eq!(
        titles(&p.search(&[("limit", "1")]).await),
        ["Soup number 4"],
        "browsing lists the newest first"
    );
    assert_eq!(p.search(&[("limit", "500")]).await["limit"], 50, "capped");
}

// ----------------------------------------------------------------------------- admin

#[tokio::test]
async fn admin_api_is_disabled_without_a_token() {
    let p = platform(Config::default()).await;
    for (method, uri) in [
        ("GET", "/admin/hubs"),
        ("POST", "/admin/poll"),
        ("DELETE", "/admin/hubs/jacob"),
    ] {
        let (status, body) = send(&p.app, admin(method, uri, Some("anything"), None)).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {uri}: {body}"
        );
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("PLATFORM_ADMIN_TOKEN")
        );
    }
    let (status, _) = send(
        &p.app,
        admin(
            "POST",
            "/admin/hubs",
            Some("anything"),
            Some(json!({"name": "jacob", "public_url": "https://jacob.chefslegacy.com"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn admin_api_manages_hubs_and_triggers_a_poll() {
    let jacob = FakeHub::start("Jacob").await;
    jacob.put(erwtensoep());
    let p = platform(config()).await;
    let new_jacob = json!({
        "name": "jacob",
        "internal_url": jacob.url,
        "public_url": "https://jacob.chefslegacy.com/",
        "chef_name": "Jacob K.",
    });

    for token in [None, Some("wrong")] {
        let (status, _) = send(
            &p.app,
            admin("POST", "/admin/hubs", token, Some(new_jacob.clone())),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let tok = Some("admin-secret");
    let (status, body) = send(
        &p.app,
        admin("POST", "/admin/hubs", tok, Some(new_jacob.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["name"], "jacob");
    assert_eq!(
        body["public_url"], "https://jacob.chefslegacy.com",
        "trailing slash dropped"
    );
    assert_eq!(body["status"], "active");
    let (status, _) = send(&p.app, admin("POST", "/admin/hubs", tok, Some(new_jacob))).await;
    assert_eq!(status, StatusCode::OK, "registering again is an update");

    let (status, body) = send(
        &p.app,
        admin(
            "POST",
            "/admin/hubs",
            tok,
            Some(json!({"name": "maria", "public_url": "https://maria.chefslegacy.com"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        body["internal_url"], "http://hub-maria.chefs-hubs.svc:8080",
        "the cluster Service is the default internal URL"
    );
    for bad in [
        json!({"name": "Bad_Name", "public_url": "https://x.chefslegacy.com"}),
        json!({"name": "www", "public_url": "https://x.chefslegacy.com"}),
        json!({"name": "ok", "public_url": "ftp://x.chefslegacy.com"}),
        json!({"name": "ok", "public_url": "https://x.chefslegacy.com", "internal_url": "http://user:pw@hub"}),
    ] {
        let (status, body) =
            send(&p.app, admin("POST", "/admin/hubs", tok, Some(bad.clone()))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    let (status, _) = send(&p.app, admin("DELETE", "/admin/hubs/maria", tok, None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = send(&p.app, admin("POST", "/admin/poll", tok, None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body[0]["hub"], "jacob");
    assert_eq!(body[0]["ok"], true);
    assert_eq!(body[0]["fetched"], 1);
    let (status, _) = send(&p.app, admin("POST", "/admin/poll?hub=ghost", tok, None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = send(&p.app, admin("POST", "/admin/poll?hub=jacob", tok, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["unchanged"], 1);

    let (_, body) = send(&p.app, admin("GET", "/admin/hubs", tok, None)).await;
    assert_eq!(body[0]["name"], "jacob");
    assert_eq!(
        body[0]["chef_name"], "Jacob K.",
        "the operator's chef name wins"
    );
    assert_eq!(body[0]["recipe_count"], 1);
    assert!(body[0]["last_ok_at"].is_string());
    assert_eq!(body[0]["last_error"], Value::Null);
    assert_eq!(body[0]["internal_url"], jacob.url);

    let (status, body) = send(
        &p.app,
        admin(
            "PATCH",
            "/admin/hubs/jacob",
            tok,
            Some(json!({"public_url": "https://chef-jacob.example.org"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let page = p.search(&[]).await;
    let hit = &hits(&page)[0];
    assert_eq!(hit["public_url"], "https://chef-jacob.example.org");
    assert_eq!(
        hit["image_url"],
        format!("https://chef-jacob.example.org/images/{SOUP}.webp")
    );

    let (status, _) = send(
        &p.app,
        admin(
            "PATCH",
            "/admin/hubs/jacob",
            tok,
            Some(json!({"status": "disabled"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        p.search(&[]).await["total"],
        0,
        "disabled hubs are not searchable"
    );
    assert_eq!(p.get("/api/v1/hubs").await.1, json!({"hubs": []}));
    assert!(p.poll().await.is_empty(), "disabled hubs are not polled");
    let (status, _) = send(
        &p.app,
        admin(
            "PATCH",
            "/admin/hubs/jacob",
            tok,
            Some(json!({"status": "bogus"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = send(
        &p.app,
        admin(
            "PATCH",
            "/admin/hubs/ghost",
            tok,
            Some(json!({"status": "active"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &p.app,
        admin(
            "PATCH",
            "/admin/hubs/jacob",
            tok,
            Some(json!({"status": "active"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p.search(&[]).await["total"], 1);

    let (status, _) = send(&p.app, admin("DELETE", "/admin/hubs/jacob", tok, None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(p.count("SELECT COUNT(*) FROM recipes_index").await, 0);
    assert_eq!(p.count("SELECT COUNT(*) FROM recipes_fts").await, 0);
    let (status, _) = send(&p.app, admin("DELETE", "/admin/hubs/jacob", tok, None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn health_answers_ok_and_unknown_paths_are_json_404s() {
    let p = platform(Config::default()).await;
    let (status, body) = p.get("/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    let (status, body) = p.get("/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn apex_search_page_serves_embedded_assets_without_exposing_admin() {
    let p = platform(Config::default()).await;

    let (status, headers, body) =
        send_raw(&p.app, Request::get("/").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let html = String::from_utf8(body).unwrap();
    assert!(html.contains("Chef's Prodigy"));
    assert!(html.contains("platform-search-form"));

    for (path, expected_type, marker) in [
        ("/assets/platform.css", "text/css", "platform-results"),
        ("/assets/platform.js", "javascript", "loadSearch"),
        ("/assets/tokens.css", "text/css", "--ink-950"),
        (
            "/assets/i18n/en.json",
            "application/json",
            "Search every kitchen",
        ),
    ] {
        let (status, headers, body) =
            send_raw(&p.app, Request::get(path).body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .contains(expected_type),
            "{path}: {:?}",
            headers[header::CONTENT_TYPE]
        );
        assert!(String::from_utf8(body).unwrap().contains(marker), "{path}");
    }

    let (status, headers, _) = send_raw(
        &p.app,
        Request::get("/assets/missing.js")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );

    let (status, body) = p.get("/admin/hubs").await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "admin stays internal-only"
    );
    assert!(body["error"].is_string());
}
