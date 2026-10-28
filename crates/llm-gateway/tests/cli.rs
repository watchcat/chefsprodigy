//! The binary: `healthcheck` exit codes for the container HEALTHCHECK and `add-hub`.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use llm_gateway::{AppState, Config, Db, build_router};

static COUNTER: AtomicU32 = AtomicU32::new(0);
const BIN: &str = env!("CARGO_BIN_EXE_llm-gateway");

fn temp_dir() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("gateway-cli-test-{}-{n}", std::process::id()));
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

#[tokio::test(flavor = "multi_thread")]
async fn healthcheck_exits_zero_against_a_running_gateway_and_one_otherwise() {
    let dir = temp_dir();
    let db = Db::open(dir.join("gateway.db")).await.unwrap();
    let app = build_router(AppState::new(db, Config::default()));
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
    assert!(status.success(), "healthy gateway → exit 0");

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
async fn add_hub_prints_a_token_that_the_database_accepts() {
    let dir = temp_dir();
    let output = tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || {
            Command::new(BIN)
                .args(["add-hub", "demo", "--data-dir", dir.to_str().unwrap()])
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let token = stdout
        .split_whitespace()
        .find(|w| w.len() == 64 && w.chars().all(|c| c.is_ascii_hexdigit()))
        .expect("a 64-hex token on stdout");

    let db = Db::open(dir.join("gateway.db")).await.unwrap();
    let hub = db
        .find_hub_by_token(token)
        .await
        .unwrap()
        .expect("token known");
    assert_eq!(hub.name, "demo");
    assert!(
        db.find_hub_by_token("0".repeat(64).as_str())
            .await
            .unwrap()
            .is_none()
    );
}

/// Run the binary against `dir` and return (exit success, stdout, stderr).
async fn run(dir: &std::path::Path, args: &[&str]) -> (bool, String, String) {
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

fn json_token(stdout: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("JSON on stdout");
    v["token"].as_str().expect("token field").to_string()
}

#[tokio::test]
async fn add_hub_json_prints_one_object_with_the_token() {
    let dir = temp_dir();
    let (ok, stdout, stderr) = run(&dir, &["add-hub", "demo", "--json"]).await;
    assert!(ok, "{stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["name"], "demo");
    assert_eq!(v["token"].as_str().unwrap().len(), 64);
    assert!(v["created_at"].is_string());
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["created_at", "name", "token"],
        "hubctl parses exactly this shape"
    );
}

#[tokio::test]
async fn add_hub_twice_fails_without_rotate_and_rotate_replaces_the_token() {
    let dir = temp_dir();
    let (_, first, _) = run(&dir, &["add-hub", "demo", "--json"]).await;
    let old = json_token(&first);

    let (ok, _, stderr) = run(&dir, &["add-hub", "demo", "--json"]).await;
    assert!(!ok, "second add-hub without --rotate must fail");
    assert!(stderr.contains("already exists"), "{stderr}");

    let (ok, second, stderr) = run(&dir, &["add-hub", "demo", "--rotate", "--json"]).await;
    assert!(ok, "{stderr}");
    let new = json_token(&second);
    assert_ne!(old, new);

    let db = Db::open(dir.join("gateway.db")).await.unwrap();
    assert!(
        db.find_hub_by_token(&old).await.unwrap().is_none(),
        "old token revoked"
    );
    assert_eq!(
        db.find_hub_by_token(&new).await.unwrap().unwrap().name,
        "demo"
    );
}

#[tokio::test]
async fn rotate_registers_a_hub_that_does_not_exist_yet() {
    let dir = temp_dir();
    let (ok, stdout, stderr) = run(&dir, &["add-hub", "fresh", "--rotate", "--json"]).await;
    assert!(ok, "{stderr}");
    let db = Db::open(dir.join("gateway.db")).await.unwrap();
    let hub = db
        .find_hub_by_token(&json_token(&stdout))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hub.name, "fresh");
}

#[tokio::test]
async fn disable_hub_revokes_the_token_and_rotate_reactivates_it() {
    let dir = temp_dir();
    let (_, stdout, _) = run(&dir, &["add-hub", "demo", "--json"]).await;
    let token = json_token(&stdout);

    let (ok, _, stderr) = run(&dir, &["disable-hub", "demo"]).await;
    assert!(ok, "{stderr}");
    let db = Db::open(dir.join("gateway.db")).await.unwrap();
    assert!(db.find_hub_by_token(&token).await.unwrap().is_none());
    assert_eq!(db.list_hubs().await.unwrap()[0].status, "disabled");

    let (ok, _, stderr) = run(&dir, &["disable-hub", "ghost"]).await;
    assert!(ok, "disabling an unknown hub is a no-op: {stderr}");

    let (_, stdout, _) = run(&dir, &["add-hub", "demo", "--rotate", "--json"]).await;
    let hub = db
        .find_hub_by_token(&json_token(&stdout))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hub.status, "active");
}

#[tokio::test]
async fn usage_prints_rows_as_json_filtered_by_hub_and_day() {
    let dir = temp_dir();
    run(&dir, &["add-hub", "jacob"]).await;
    run(&dir, &["add-hub", "maria"]).await;
    let db = Db::open(dir.join("gateway.db")).await.unwrap();
    let hubs = db.list_hubs().await.unwrap();
    db.record_usage(hubs[0].id, "2026-10-26", 100, 50, 0.0)
        .await
        .unwrap();
    db.record_usage(hubs[0].id, "2026-10-27", 10, 5, 0.0)
        .await
        .unwrap();
    db.record_usage(hubs[1].id, "2026-10-27", 7, 3, 0.0)
        .await
        .unwrap();

    let (ok, stdout, stderr) = run(&dir, &["usage"]).await;
    assert!(ok, "{stderr}");
    let all: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(all.len(), 3);

    let (_, stdout, _) = run(&dir, &["usage", "--hub", "jacob", "--from", "2026-10-27"]).await;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["hub"], "jacob");
    assert_eq!(rows[0]["requests"], 1);
    assert_eq!(rows[0]["input_tokens"], 10);
    let mut keys: Vec<&str> = rows[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "cost_cents",
            "day",
            "hub",
            "input_tokens",
            "output_tokens",
            "requests"
        ],
        "hubctl parses exactly these fields"
    );
}
