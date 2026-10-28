//! The `platform` binary as hubctl drives it through `kubectl exec`: `add-hub`
//! (idempotent upsert), `remove-hub`, `poll`, `hubs` and the `healthcheck` probe.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::routing::get;
use axum::{Json, Router};
use platform::{AppState, Config, Db, build_router};
use recipe_core::{
    FederatedRecipe, FederationManifest, ManifestEntry, Recipe, calculate_content_hash,
};
use serde_json::Value;

static COUNTER: AtomicU32 = AtomicU32::new(0);
const BIN: &str = env!("CARGO_BIN_EXE_platform");
const ID: &str = "01JACOB0000000000000000SOUP";

fn temp_dir() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("platform-cli-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Run the binary against `dir` and return (exit success, stdout, stderr).
async fn run(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let mut full: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    full.push("--data-dir".into());
    full.push(dir.to_str().unwrap().to_string());
    let output =
        tokio::task::spawn_blocking(move || Command::new(BIN).args(&full).output().unwrap())
            .await
            .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn json(stdout: &str) -> Value {
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout}"))
}

fn soup() -> Recipe {
    let mut r: Recipe = serde_json::from_value(serde_json::json!({
        "id": ID, "title": "Erwtensoep", "description": null, "servings": 4,
        "prep_time_minutes": null, "cook_time_minutes": null,
        "ingredients": [{"raw_text": "split peas", "metric_amount": 500.0, "unit": "g",
                          "canonical_id": "split_pea", "is_specialty": false}],
        "instructions": [{"step_number": 1, "text": "Kook.", "timer_seconds": null}],
        "tags": ["soep"], "image_path": null, "origin_url": null, "author_did": null,
        "content_hash": "", "is_public": true, "created_at": "2026-10-01T10:00:00Z",
        "updated_at": "2026-10-01T10:00:00Z", "language": "nl"
    }))
    .unwrap();
    r.content_hash = calculate_content_hash(&r);
    r
}

/// One public recipe behind the hub's two federation endpoints.
async fn fake_hub() -> String {
    let recipe = soup();
    let manifest = FederationManifest {
        author_did: None,
        chef_name: "Jacob".into(),
        node_url: None,
        recipes: vec![ManifestEntry {
            id: recipe.id.clone(),
            updated_at: recipe.updated_at.clone(),
            content_hash: recipe.content_hash.clone(),
            translations: vec![],
        }],
    };
    let fed = FederatedRecipe {
        recipe,
        translations: vec![],
    };
    let app = Router::new()
        .route(
            "/api/v1/federation/manifest",
            get(move || async move { Json(manifest) }),
        )
        .route(
            &format!("/api/v1/federation/recipes/{ID}"),
            get(move || async move { Json(fed) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

async fn count(dir: &Path, sql: &'static str) -> i64 {
    let db = Db::open(dir.join("platform.db")).await.unwrap();
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn healthcheck_exits_zero_against_a_running_platform_and_one_otherwise() {
    let dir = temp_dir();
    let db = Db::open(dir.join("platform.db")).await.unwrap();
    let app = build_router(AppState::new(db, Config::default()).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let status = tokio::task::spawn_blocking(move || {
        Command::new(BIN)
            .args(["healthcheck", "--port", &port.to_string()])
            .status()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(status.success(), "healthy platform → exit 0");

    let dead = free_port();
    let output = tokio::task::spawn_blocking(move || {
        Command::new(BIN)
            .args(["healthcheck", "--port", &dead.to_string()])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains(&dead.to_string()));
}

#[tokio::test]
async fn add_hub_is_an_idempotent_upsert() {
    let dir = temp_dir();
    let args = [
        "add-hub",
        "jacob",
        "--internal-url",
        "http://hub-jacob.chefs-hubs.svc:8080",
        "--public-url",
        "https://jacob.chefslegacy.com",
    ];
    let (ok, stdout, stderr) =
        run(&dir, &[&args[..], &["--chef", "Jacob", "--json"]].concat()).await;
    assert!(ok, "{stderr}");
    let v = json(&stdout);
    assert_eq!(v["name"], "jacob");
    assert_eq!(v["internal_url"], "http://hub-jacob.chefs-hubs.svc:8080");
    assert_eq!(v["public_url"], "https://jacob.chefslegacy.com");
    assert_eq!(v["chef_name"], "Jacob");
    assert_eq!(v["status"], "active");
    assert_eq!(v["created"], true);

    let (ok, stdout, stderr) = run(
        &dir,
        &[&args[..], &["--chef", "Jacob K.", "--json"]].concat(),
    )
    .await;
    assert!(ok, "{stderr}");
    let v = json(&stdout);
    assert_eq!(v["created"], false, "the second run updates");
    assert_eq!(v["chef_name"], "Jacob K.");

    let (ok, stdout, stderr) = run(&dir, &args).await;
    assert!(ok, "{stderr}");
    assert!(stdout.contains("jacob"), "{stdout}");
    let db = Db::open(dir.join("platform.db")).await.unwrap();
    let hubs = db.list_hubs().await.unwrap();
    assert_eq!(hubs.len(), 1);
    assert_eq!(
        hubs[0].chef_name.as_deref(),
        Some("Jacob K."),
        "no --chef keeps the name"
    );
}

#[tokio::test]
async fn add_hub_defaults_the_internal_url_and_rejects_bad_input() {
    let dir = temp_dir();
    let (ok, stdout, stderr) = run(
        &dir,
        &[
            "add-hub",
            "maria",
            "--public-url",
            "https://maria.chefslegacy.com",
            "--json",
        ],
    )
    .await;
    assert!(ok, "{stderr}");
    assert_eq!(
        json(&stdout)["internal_url"],
        "http://hub-maria.chefs-hubs.svc:8080"
    );

    let (ok, _, stderr) = run(
        &dir,
        &[
            "add-hub",
            "Bad_Name",
            "--public-url",
            "https://x.chefslegacy.com",
        ],
    )
    .await;
    assert!(!ok);
    assert!(stderr.contains("not a valid hub name"), "{stderr}");
    let (ok, _, stderr) = run(&dir, &["add-hub", "ok", "--public-url", "not a url"]).await;
    assert!(!ok);
    assert!(stderr.contains("public_url"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_indexes_registered_hubs_and_remove_hub_deletes_their_rows() {
    let dir = temp_dir();
    let hub = fake_hub().await;
    let (ok, _, stderr) = run(
        &dir,
        &[
            "add-hub",
            "jacob",
            "--internal-url",
            &hub,
            "--public-url",
            "https://jacob.chefslegacy.com",
        ],
    )
    .await;
    assert!(ok, "{stderr}");

    let (ok, stdout, stderr) = run(&dir, &["poll", "--json"]).await;
    assert!(ok, "{stdout}{stderr}");
    let reports = json(&stdout);
    assert_eq!(reports[0]["hub"], "jacob");
    assert_eq!(reports[0]["ok"], true);
    assert_eq!(reports[0]["fetched"], 1);
    assert_eq!(count(&dir, "SELECT COUNT(*) FROM recipes_index").await, 1);

    let (ok, stdout, _) = run(&dir, &["poll", "--hub", "jacob"]).await;
    assert!(ok);
    assert!(stdout.contains("jacob: ok"), "{stdout}");
    assert!(stdout.contains("1 unchanged"), "{stdout}");

    let (ok, stdout, stderr) = run(&dir, &["hubs", "--json"]).await;
    assert!(ok, "{stderr}");
    let hubs = json(&stdout);
    assert_eq!(hubs[0]["name"], "jacob");
    assert_eq!(hubs[0]["recipe_count"], 1);
    assert!(hubs[0]["last_ok_at"].is_string());

    let (ok, stdout, stderr) = run(&dir, &["remove-hub", "jacob"]).await;
    assert!(ok, "{stderr}");
    assert!(stdout.contains("removed"), "{stdout}");
    assert_eq!(count(&dir, "SELECT COUNT(*) FROM recipes_index").await, 0);
    assert_eq!(count(&dir, "SELECT COUNT(*) FROM recipes_fts").await, 0);
    assert_eq!(count(&dir, "SELECT COUNT(*) FROM hubs").await, 0);
    let (ok, stdout, _) = run(&dir, &["remove-hub", "jacob"]).await;
    assert!(ok, "removing an unknown hub is not an error");
    assert!(stdout.contains("not registered"), "{stdout}");
}

#[tokio::test]
async fn poll_exits_one_when_a_hub_fails() {
    let dir = temp_dir();
    let dead = format!("http://127.0.0.1:{}", free_port());
    let (ok, _, stderr) = run(
        &dir,
        &[
            "add-hub",
            "dead",
            "--internal-url",
            &dead,
            "--public-url",
            "https://dead.chefslegacy.com",
        ],
    )
    .await;
    assert!(ok, "{stderr}");
    let (ok, stdout, _) = run(&dir, &["poll"]).await;
    assert!(!ok);
    assert!(stdout.contains("dead: error"), "{stdout}");
}
