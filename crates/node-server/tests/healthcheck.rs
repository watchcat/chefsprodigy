//! `node-server healthcheck`: the loopback probe behind the container HEALTHCHECK.
//! It exits 0 when `/api/v1/health` answers `{"status":"ok"}` and 1 otherwise,
//! without needing curl or a shell in the image.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use node_server::health::{self, DEFAULT_TIMEOUT};
use node_server::{AppState, NodeInfo, build_router};
use recipe_db::Db;

static COUNTER: AtomicU32 = AtomicU32::new(0);
const BIN: &str = env!("CARGO_BIN_EXE_node-server");

async fn real_app() -> Router {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("node-health-test-{}-{n}", std::process::id()));
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
        .with_image_dir(dir.join("images")),
    )
}

/// Serve `app` on an ephemeral loopback port and return that port.
async fn serve(app: Router) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

/// A port nothing listens on (bound once to find it, then released).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn passes_against_a_running_node() {
    let port = serve(real_app().await).await;
    health::check("127.0.0.1", port, DEFAULT_TIMEOUT)
        .await
        .expect("a running node is healthy");
}

#[tokio::test]
async fn fails_when_nothing_listens() {
    let port = free_port();
    let err = health::check("127.0.0.1", port, DEFAULT_TIMEOUT)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains(&format!("127.0.0.1:{port}")),
        "{err}"
    );
}

#[tokio::test]
async fn fails_when_the_endpoint_answers_an_error_status() {
    let app = Router::new().route(
        "/api/v1/health",
        get(|| async { (StatusCode::SERVICE_UNAVAILABLE, "starting") }),
    );
    let port = serve(app).await;
    let err = health::check("127.0.0.1", port, DEFAULT_TIMEOUT)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("503"), "{err}");
}

#[tokio::test]
async fn fails_when_the_body_does_not_say_ok() {
    let app = Router::new().route(
        "/api/v1/health",
        get(|| async { axum::Json(serde_json::json!({ "status": "degraded" })) }),
    );
    let port = serve(app).await;
    let err = health::check("127.0.0.1", port, DEFAULT_TIMEOUT)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("degraded"), "{err}");
}

#[tokio::test]
async fn gives_up_after_the_timeout_on_a_silent_server() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // Accept connections and keep them open without ever answering.
    tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            held.push(socket);
        }
    });
    let started = Instant::now();
    let err = health::check("127.0.0.1", port, Duration::from_millis(300))
        .await
        .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
    assert!(err.to_string().contains("300ms"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_exits_zero_when_the_node_is_healthy() {
    let port = serve(real_app().await).await;
    let output = tokio::task::spawn_blocking(move || {
        Command::new(BIN)
            .args(["healthcheck", "--port", &port.to_string()])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_exits_one_when_nothing_listens() {
    let port = free_port();
    let output = Command::new(BIN)
        .args(["healthcheck", "--port", &port.to_string()])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains(&port.to_string()));
}

#[test]
fn cli_takes_the_port_from_node_port_like_serve_does() {
    let port = free_port();
    let output = Command::new(BIN)
        .arg("healthcheck")
        .env("NODE_PORT", port.to_string())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(&port.to_string()),
        "probe should target NODE_PORT"
    );
}
