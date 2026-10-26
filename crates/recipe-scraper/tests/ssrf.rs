//! SSRF guard: user-supplied URLs must never reach loopback, private, link-local
//! (cloud metadata), multicast or unspecified addresses, on any redirect hop.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use recipe_scraper::llm::HeuristicExtractor;
use recipe_scraper::ssrf::{blocked_reason, check_resolved};
use recipe_scraper::{RecipeImporter, ScraperError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PAGE: &str = r#"<html><head><script type="application/ld+json">
{"@type":"Recipe","name":"Guarded Pho","recipeIngredient":["salt"],"recipeInstructions":["Boil."]}
</script></head><body></body></html>"#;

fn guarded() -> RecipeImporter {
    RecipeImporter::new(Arc::new(HeuristicExtractor))
}

/// A local HTTP server answering every request with `response`; returns its base
/// URL and a counter of accepted connections.
async fn serve(response: String) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let response = response.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (base, hits)
}

fn redirect_to(location: &str) -> String {
    format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

fn html(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn assert_blocked<T: std::fmt::Debug>(result: Result<T, ScraperError>, needle: &str) {
    match result {
        Err(ScraperError::BlockedUrl { .. }) => {
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains(needle),
                "error {msg:?} should mention {needle:?}"
            );
        }
        other => panic!("expected a BlockedUrl error mentioning {needle:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn literal_private_addresses_are_refused_without_connecting() {
    let importer = guarded();
    let (base, hits) = serve(html(PAGE)).await;
    assert_blocked(
        importer.import_url(&format!("{base}/pho")).await,
        "127.0.0.1",
    );
    assert_blocked(
        importer.fetch_bytes(&format!("{base}/cover.jpg")).await,
        "127.0.0.1",
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "the guard must refuse before opening a connection"
    );

    assert_blocked(
        importer
            .import_url("http://169.254.169.254/latest/meta-data/")
            .await,
        "169.254.169.254",
    );
    assert_blocked(
        importer.import_url("http://127.0.0.1:8080/").await,
        "127.0.0.1",
    );
    assert_blocked(importer.import_url("http://10.1.2.3/").await, "10.1.2.3");
    assert_blocked(importer.import_url("http://[::1]:8080/").await, "::1");
    assert_blocked(
        importer.import_url("http://[::ffff:10.0.0.1]/").await,
        "10.0.0.1",
    );
}

#[tokio::test]
async fn hostnames_resolving_to_private_addresses_are_refused() {
    // `localhost` resolves to a loopback address on every system.
    assert_blocked(
        guarded().import_url("http://localhost:8080/").await,
        "localhost",
    );
}

#[tokio::test]
async fn schemes_other_than_http_and_https_are_refused() {
    let importer = guarded();
    assert_blocked(importer.import_url("file:///etc/passwd").await, "file");
    assert_blocked(importer.import_url("ftp://example.com/recipe").await, "ftp");
    assert_blocked(
        importer.fetch_bytes("gopher://example.com/").await,
        "gopher",
    );
    assert_blocked(importer.import_url("not a url").await, "not a url");
}

#[tokio::test]
async fn redirects_to_blocked_addresses_are_refused_on_every_hop() {
    // The local server stands in for a public site, so loopback must be allowed here;
    // link-local (cloud metadata) stays blocked even then.
    let importer = guarded().allow_private_hosts();
    let (base, hits) = serve(redirect_to("http://169.254.169.254/latest/meta-data/")).await;
    assert_blocked(
        importer.import_url(&format!("{base}/moved")).await,
        "169.254.169.254",
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "only the first hop was fetched"
    );

    let (base, _) = serve(redirect_to("file:///etc/passwd")).await;
    assert_blocked(importer.import_url(&format!("{base}/moved")).await, "file");
}

#[tokio::test]
async fn allowed_local_servers_still_import() {
    let (base, _) = serve(html(PAGE)).await;
    let result = guarded()
        .allow_private_hosts()
        .import_url(&format!("{base}/pho"))
        .await
        .unwrap();
    assert_eq!(result.recipe.title, "Guarded Pho");
}

#[test]
fn address_classification_matches_the_spec_ranges() {
    let blocked = [
        "127.0.0.1",
        "127.255.255.254",
        "10.0.0.1",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.0.1",
        "100.64.0.1",
        "100.127.255.255",
        "169.254.169.254",
        "0.0.0.0",
        "224.0.0.1",
        "255.255.255.255",
        "192.0.0.192",
        "::1",
        "::",
        "fe80::1",
        "fc00::1",
        "fd12::1",
        "ff02::1",
        "::ffff:10.0.0.1",
        "::ffff:169.254.169.254",
        "64:ff9b::7f00:1",
        "2002:a00:1::",
    ];
    for a in blocked {
        let ip: IpAddr = a.parse().unwrap();
        assert!(blocked_reason(ip).is_some(), "{a} should be blocked");
    }
    let public = [
        "93.184.216.34",
        "8.8.8.8",
        "172.32.0.1",
        "100.128.0.1",
        "2606:4700:4700::1111",
        "2a00:1450:4001:80b::200e",
        "::ffff:8.8.8.8",
        "64:ff9b::808:808",
    ];
    for a in public {
        let ip: IpAddr = a.parse().unwrap();
        assert!(blocked_reason(ip).is_none(), "{a} should be public");
    }
}

#[test]
fn resolved_addresses_are_checked_by_name() {
    let gateway: IpAddr = "172.18.0.5".parse().unwrap();
    let err = check_resolved("llm-gateway", [gateway], false).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("llm-gateway") && msg.contains("172.18.0.5"),
        "{msg}"
    );

    let public: IpAddr = "93.184.216.34".parse().unwrap();
    assert!(check_resolved("example.com", [public], false).is_ok());
    assert!(
        check_resolved("mixed.example", [public, gateway], false).is_err(),
        "one private answer among public ones is enough to refuse"
    );
    assert!(check_resolved("db.internal", [gateway], true).is_ok());
    let metadata: IpAddr = "169.254.169.254".parse().unwrap();
    assert!(
        check_resolved("metadata.internal", [metadata], true).is_err(),
        "the test flag never unlocks link-local addresses"
    );
}
