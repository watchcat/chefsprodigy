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
