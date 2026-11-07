//! The archive the export produces must be the one `hubctl restore` unpacks:
//! `recipes.db` at the root and `images/` beside it. This checks the layout
//! the way `hubctl` checks it, on the bytes the export really produced.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use node_server::{AppState, NodeInfo, build_router};
use recipe_db::Db;
use tower::ServiceExt;

static COUNTER: AtomicU32 = AtomicU32::new(0);
const KEY: &str = "owner-secret-123";

async fn export_archive() -> (Vec<u8>, PathBuf) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("node-archive-layout-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(dir.join("recipes.db")).await.unwrap();
    let state = AppState::new(
        db,
        NodeInfo {
            author_did: None,
            chef_name: "T".into(),
        },
    )
    .with_image_dir(dir.join("images"))
    .with_mcp_api_key(KEY);
    let app = build_router(state);
    let res = app
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
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (bytes, dir)
}

#[tokio::test]
async fn the_archive_has_the_layout_hubctl_restore_unpacks() {
    let (bytes, _dir) = export_archive().await;

    // hubctl restore: `if "recipes.db" not in members` -> refuse.
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
    let members: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().unwrap().into_owned())
        .collect();
    assert!(
        members.iter().any(|m| m == "recipes.db"),
        "hubctl restore refuses an archive without recipes.db: {members:?}"
    );

    // hubctl restore: `rm -rf /data/* && unzip -o archive.zip -d /data`.
    // Everything the archive holds must land inside /data, and the database
    // must land as /data/recipes.db, which is where the node reads it from.
    let data = std::env::temp_dir().join(format!("hubctl-restore-sim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().unwrap().into_owned();
        assert!(
            !name.starts_with('/') && !name.contains(".."),
            "an archive entry must not escape the data directory: {name}"
        );
        let path = data.join(&name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut out = std::fs::File::create(&path).unwrap();
        std::io::copy(&mut entry, &mut out).unwrap();
    }

    let restored_db = data.join("recipes.db");
    assert!(
        restored_db.exists(),
        "the database lands as /data/recipes.db"
    );
    // The unpacked file is a working database, not a truncated one.
    let db = Db::open(&restored_db).await.unwrap();
    let count = db
        .count_recipes(&recipe_core::SearchQuery::default())
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "an empty node's database opens from the unpacked file"
    );

    let _ = std::fs::remove_dir_all(&data);
}
