//! RN-67: a 429 or 402 from the model endpoint (gateway quota, vendor rate limit,
//! billing) is reported as `ScraperError::ModelQuotaExceeded`, so the API and the
//! UI can say "quota" instead of "model failure". Other failures stay generic.

use recipe_scraper::ScraperError;
use recipe_scraper::llm::{RecipeExtractor, RigExtractor};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A stand-in model endpoint answering every request with one canned reply.
async fn vendor_answering(status: u16, reason: &str, body: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let response = response.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 65536];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    base
}

fn extractor(base: &str) -> RigExtractor {
    RigExtractor::connect("minimax/MiniMax-M3", base, "hub-token").unwrap()
}

fn quota_message(result: Result<impl std::fmt::Debug, ScraperError>) -> String {
    match result {
        Err(ScraperError::ModelQuotaExceeded(message)) => message,
        other => panic!("expected ModelQuotaExceeded, got {other:?}"),
    }
}

#[tokio::test]
async fn a_gateway_quota_refusal_is_a_quota_error_carrying_the_reason() {
    let base = vendor_answering(
        429,
        "Too Many Requests",
        r#"{"type":"error","error":{"type":"quota_exceeded","message":"daily quota of 10 requests used"}}"#,
    )
    .await;
    let message = quota_message(extractor(&base).extract_recipe("Fry two eggs").await);
    assert!(message.contains("quota"), "{message}");
    assert!(
        message.contains("daily quota of 10 requests used"),
        "{message}"
    );
}

#[tokio::test]
async fn a_vendor_rate_limit_is_a_quota_error_naming_the_limit() {
    let base = vendor_answering(
        429,
        "Too Many Requests",
        r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#,
    )
    .await;
    let message = quota_message(extractor(&base).extract_recipe("Fry two eggs").await);
    assert!(message.contains("rate limit"), "{message}");
    assert!(message.contains("slow down"), "{message}");
}

#[tokio::test]
async fn a_payment_required_answer_is_a_quota_error() {
    let base = vendor_answering(
        402,
        "Payment Required",
        r#"{"type":"error","error":{"type":"billing_error","message":"insufficient balance"}}"#,
    )
    .await;
    let message = quota_message(extractor(&base).extract_recipe("Fry two eggs").await);
    assert!(message.contains("payment"), "{message}");
    assert!(message.contains("insufficient balance"), "{message}");
}

#[tokio::test]
async fn a_limit_answer_without_a_json_body_still_says_which_status() {
    let base = vendor_answering(429, "Too Many Requests", "busy").await;
    let message = quota_message(extractor(&base).extract_recipe("Fry two eggs").await);
    assert!(
        message.contains("busy") || message.contains("429"),
        "{message}"
    );
}

#[tokio::test]
async fn other_model_failures_stay_generic() {
    let base = vendor_answering(
        500,
        "Internal Server Error",
        r#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#,
    )
    .await;
    match extractor(&base).extract_recipe("Fry two eggs").await {
        Err(ScraperError::Llm(_)) => {}
        other => panic!("expected a generic Llm error, got {other:?}"),
    }
}
