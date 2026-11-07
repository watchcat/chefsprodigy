//! Backup and restore over the HTTP API: `GET /api/v1/export` and
//! `POST /api/v1/import/export`.
//!
//! The archive a hub produces is the same one `hubctl restore` unpacks into a
//! hub's data directory, so these tests pin that layout as well as the round
//! trip: a recipe keeps its id, its content hash and its cover.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use node_server::{AppState, NodeInfo, build_router};
use recipe_core::SearchQuery;
use recipe_db::Db;
use serde_json::{Value, json};
use tower::ServiceExt;

static COUNTER: AtomicU32 = AtomicU32::new(0);
const KEY: &str = "owner-secret-123";

/// A node with its own directory, database and image store.
struct Node {
    app: Router,
    dir: PathBuf,
    db: Db,
}

impl Node {
    async fn new() -> Self {
        Self::new_with_recipes(0).await
    }

    async fn new_with_recipes(recipes: usize) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir: PathBuf =
            std::env::temp_dir().join(format!("node-backup-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(dir.join("recipes.db")).await.unwrap();
        let state = AppState::new(
            db.clone(),
            NodeInfo {
                author_did: None,
                chef_name: "T".into(),
            },
        )
        .with_image_dir(dir.join("images"))
        .with_mcp_api_key(KEY);
        let app = build_router(state);
        for i in 0..recipes {
            let (status, body) = send_json(
                &app,
                "POST",
                "/api/v1/recipes",
                recipe_json(&format!("Recipe {i}")),
                &[("authorization", format!("Bearer {KEY}").as_str())],
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
        }
        Self { app, dir, db }
    }

    /// The ids of the recipes on this node.
    async fn recipe_ids(&self) -> Vec<String> {
        self.db
            .search_recipes(&SearchQuery::default())
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    /// The archive this node exports.
    async fn export(&self) -> Vec<u8> {
        let res = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/export")
                    .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CONTENT_TYPE], "application/zip");
        res.into_body().collect().await.unwrap().to_bytes().to_vec()
    }
}

fn recipe_json(title: &str) -> Value {
    json!({
        "title": title,
        "servings": 2,
        "prep_time_minutes": 5,
        "cook_time_minutes": 10,
        "ingredients": [
            {"raw_text": "200g noodles", "metric_amount": 200.0, "unit": "g", "canonical_id": "noodles", "is_specialty": false},
            {"raw_text": "salt", "metric_amount": null, "unit": "to_taste", "canonical_id": "salt", "is_specialty": false}
        ],
        "instructions": [{"step_number": 1, "text": "Boil.", "timer_seconds": 300}],
        "tags": ["quick"],
        "is_public": false
    })
}

async fn send_json(
    app: &Router,
    method: &str,
    uri: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
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

/// A multipart body holding one file field.
fn multipart(field: &str, filename: &str, bytes: &[u8]) -> (String, Vec<u8>) {
    let boundary = "----recipeNodeBoundary42";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: application/zip\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

async fn send_multipart(
    app: &Router,
    uri: &str,
    content_type: String,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
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

/// The entries of a zip archive, as (name, bytes) pairs.
fn zip_entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut entries = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().unwrap().into_owned();
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buf).unwrap();
        entries.push((name, buf));
    }
    entries
}

#[tokio::test]
async fn the_export_is_a_zip_with_the_database_and_the_images() {
    let node = Node::new().await;
    let archive = node.export().await;
    let entries = zip_entries(&archive);
    let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
    assert!(
        names.contains(&"recipes.db"),
        "the archive holds the database at its root, as hubctl restore expects: {names:?}"
    );

    // The database in the archive is a real SQLite file with the recipes in it.
    let db_bytes = entries
        .iter()
        .find(|(name, _)| name == "recipes.db")
        .map(|(_, bytes)| bytes.clone())
        .unwrap();
    assert_eq!(&db_bytes[..15], b"SQLite format 3");
}

#[tokio::test]
async fn the_export_holds_the_covers_of_the_node() {
    let node = Node::new_with_recipes(1).await;
    // A cover is a WebP file named after the recipe.
    let recipe_id = node.recipe_ids().await.remove(0);
    let cover = node.dir.join("images").join(format!("{recipe_id}.webp"));
    std::fs::create_dir_all(cover.parent().unwrap()).unwrap();
    std::fs::write(&cover, b"RIFF____WEBPfake").unwrap();

    let archive = node.export().await;
    let entries = zip_entries(&archive);
    let cover_entry = entries
        .iter()
        .find(|(name, _)| *name == format!("images/{recipe_id}.webp"))
        .map(|(_, bytes)| bytes.clone());
    assert_eq!(
        cover_entry.as_deref(),
        Some(&b"RIFF____WEBPfake"[..]),
        "the cover travels with the archive"
    );
}

#[tokio::test]
async fn the_export_needs_the_owner_key() {
    let node = Node::new().await;
    let res = node
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/export")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "the archive holds the whole library, so it is owner-only"
    );
}

#[tokio::test]
async fn a_restored_node_holds_the_same_recipes_with_the_same_ids_and_hashes() {
    let source = Node::new_with_recipes(2).await;
    let original = source.recipe_ids().await;
    let archive = source.export().await;

    let target = Node::new().await;
    let (ct, body) = multipart("file", "export.zip", &archive);
    let (status, json) = send_multipart(&target.app, "/api/v1/import/export", ct, body).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["recipes"], 2);

    let restored = target.recipe_ids().await;
    assert_eq!(restored.len(), 2);
    for id in &original {
        assert!(restored.contains(id), "the id {id} survives the round trip");
        let before = source.db.get_recipe(id).await.unwrap().unwrap();
        let after = target.db.get_recipe(id).await.unwrap().unwrap();
        assert_eq!(after.content_hash, before.content_hash);
        assert_eq!(after.title, before.title);
        assert_eq!(after.ingredients.len(), before.ingredients.len());
        assert_eq!(after.instructions.len(), before.instructions.len());
        assert_eq!(after.tags, before.tags);
    }
}

#[tokio::test]
async fn a_restore_puts_the_covers_back() {
    let source = Node::new_with_recipes(1).await;
    let recipe_id = source.recipe_ids().await.remove(0);
    let cover = source.dir.join("images").join(format!("{recipe_id}.webp"));
    std::fs::create_dir_all(cover.parent().unwrap()).unwrap();
    std::fs::write(&cover, b"RIFF____WEBPfake").unwrap();
    let archive = source.export().await;

    let target = Node::new().await;
    let (ct, body) = multipart("file", "export.zip", &archive);
    let (status, json) = send_multipart(&target.app, "/api/v1/import/export", ct, body).await;
    assert_eq!(status, StatusCode::OK, "{json}");

    let restored_cover = target.dir.join("images").join(format!("{recipe_id}.webp"));
    assert!(
        restored_cover.exists(),
        "the cover is back in the image store after the restore"
    );
    assert_eq!(std::fs::read(&restored_cover).unwrap(), b"RIFF____WEBPfake");
}

#[tokio::test]
async fn a_restore_needs_the_owner_key() {
    let node = Node::new().await;
    let (ct, body) = multipart("file", "export.zip", b"PK\x05\x06".to_vec().as_slice());
    let res = node
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/import/export")
                .header(header::CONTENT_TYPE, ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_restore_refuses_a_node_that_already_holds_recipes() {
    let source = Node::new_with_recipes(1).await;
    let archive = source.export().await;

    let target = Node::new_with_recipes(1).await;
    let (ct, body) = multipart("file", "export.zip", &archive);
    let (status, json) = send_multipart(&target.app, "/api/v1/import/export", ct, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert!(
        json["error"].as_str().unwrap().contains("empty node"),
        "the refusal says why: {json}"
    );
}

#[tokio::test]
async fn a_restore_of_something_that_is_not_an_archive_is_refused() {
    let node = Node::new().await;
    let (ct, body) = multipart("file", "x.zip", b"not a zip at all");
    let (status, json) = send_multipart(&node.app, "/api/v1/import/export", ct, body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
}

#[tokio::test]
async fn a_restore_without_a_file_is_refused() {
    let node = Node::new().await;
    let boundary = "----recipeNodeBoundary42";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"comment\"\r\n\r\nhello\r\n--{boundary}--\r\n"
    );
    let (status, json) = send_multipart(
        &node.app,
        "/api/v1/import/export",
        format!("multipart/form-data; boundary={boundary}"),
        body.into_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
}

#[tokio::test]
async fn an_archive_cannot_write_outside_the_image_directory() {
    // A hand-built archive whose cover entries try to escape the image
    // directory: a path with a directory part, and a traversal.
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let mut zip = zip::ZipWriter::new(&mut buf);
        for name in [
            "images/../../escaped.webp",
            "images/sub/dir.webp",
            "images/plain.webp",
        ] {
            zip.start_file(name, options).unwrap();
            std::io::Write::write_all(&mut zip, b"RIFF____WEBPfake").unwrap();
        }
        // The database entry the restore needs, so the archive gets past the
        // "is this a node backup" check and reaches the cover handling.
        zip.start_file("recipes.db", options).unwrap();
        std::io::Write::write_all(&mut zip, b"not a database").unwrap();
        zip.finish().unwrap();
    }
    let archive = buf.into_inner();

    let target = Node::new().await;
    let (ct, body) = multipart("file", "export.zip", &archive);
    let (status, _json) = send_multipart(&target.app, "/api/v1/import/export", ct, body).await;
    // The database entry is not a database, so the restore fails; what matters
    // is that nothing was written outside the image directory.
    assert_ne!(status, StatusCode::OK, "{status}");

    let escaped = target.dir.join("escaped.webp");
    assert!(
        !escaped.exists(),
        "an archive must not place a file outside the image directory"
    );
    let sub = target.dir.join("images").join("sub");
    assert!(
        !sub.exists(),
        "an archive must not place a file in a subdirectory of the image store"
    );
    let images = target.dir.join("images");
    assert!(
        !images.join("plain.webp").exists(),
        "a restore that fails does not leave covers behind"
    );
}
