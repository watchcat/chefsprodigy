//! Pool initialization and migration tests. Each test opens its own
//! on-disk SQLite file so WAL mode (unsupported for in-memory DBs) is exercised.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use recipe_db::{Db, MIGRATOR};
use sqlx::Row;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_db_path(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("recipe-db-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{name}.db"))
}

async fn pragma_text(db: &Db, name: &str) -> String {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA {name}")))
        .fetch_one(db.pool())
        .await
        .unwrap();
    // PRAGMA results may come back as INTEGER or TEXT depending on the pragma.
    row.try_get::<String, _>(0)
        .or_else(|_| row.try_get::<i64, _>(0).map(|v| v.to_string()))
        .unwrap()
}

#[tokio::test]
async fn open_creates_database_file_if_missing() {
    let path = temp_db_path("create");
    assert!(!path.exists());
    let _db = Db::open(&path).await.unwrap();
    assert!(path.exists());
}

#[tokio::test]
async fn open_enforces_required_pragmas() {
    let db = Db::open(temp_db_path("pragmas")).await.unwrap();
    assert_eq!(pragma_text(&db, "journal_mode").await, "wal");
    assert_eq!(
        pragma_text(&db, "synchronous").await,
        "1",
        "synchronous must be NORMAL (1)"
    );
    assert_eq!(pragma_text(&db, "foreign_keys").await, "1");
    assert_eq!(pragma_text(&db, "busy_timeout").await, "5000");
}

#[tokio::test]
async fn open_applies_migrations() {
    let db = Db::open(temp_db_path("migrate")).await.unwrap();
    let rows = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type IN ('table','trigger') ORDER BY name",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    let names: Vec<String> = rows.iter().map(|r| r.get::<String, _>(0)).collect();
    for expected in [
        "recipes",
        "recipe_ingredients",
        "recipes_fts",
        "recipes_ai",
        "recipes_au",
        "recipes_ad",
        "ingredients_fts_sync_ai",
        "ingredients_fts_sync_au",
        "ingredients_fts_sync_ad",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing {expected} in {names:?}"
        );
    }

    let applied: i64 = sqlx::query("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .get(0);
    assert_eq!(applied as usize, MIGRATOR.iter().count());
    assert!(applied >= 1);
}

#[tokio::test]
async fn open_is_idempotent_on_existing_database() {
    let path = temp_db_path("reopen");
    let first = Db::open(&path).await.unwrap();
    sqlx::query("INSERT INTO recipes(id, title, instructions, content_hash) VALUES ('r1', 'Pho', '[]', 'h')")
        .execute(first.pool())
        .await
        .unwrap();
    first.close().await;

    let second = Db::open(&path).await.unwrap();
    let count: i64 = sqlx::query("SELECT COUNT(*) FROM recipes")
        .fetch_one(second.pool())
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 1);
}

#[tokio::test]
async fn foreign_keys_cascade_delete_ingredients() {
    let db = Db::open(temp_db_path("cascade")).await.unwrap();
    sqlx::query("INSERT INTO recipes(id, title, instructions, content_hash) VALUES ('r1', 'Pho', '[]', 'h')")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO recipe_ingredients(recipe_id, raw_text, unit, canonical_id) VALUES ('r1', '2 star anise', 'pcs', 'star_anise')")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM recipes WHERE id = 'r1'")
        .execute(db.pool())
        .await
        .unwrap();

    let remaining: i64 = sqlx::query("SELECT COUNT(*) FROM recipe_ingredients")
        .fetch_one(db.pool())
        .await
        .unwrap()
        .get(0);
    assert_eq!(remaining, 0, "ON DELETE CASCADE requires foreign_keys = ON");
}

#[tokio::test]
async fn orphan_ingredient_insert_is_rejected() {
    let db = Db::open(temp_db_path("orphan")).await.unwrap();
    let result = sqlx::query("INSERT INTO recipe_ingredients(recipe_id, raw_text, unit, canonical_id) VALUES ('missing', 'x', 'g', 'x')")
        .execute(db.pool())
        .await;
    assert!(result.is_err(), "foreign key violation must be rejected");
}

#[tokio::test]
async fn fts_index_tracks_ingredients() {
    let db = Db::open(temp_db_path("fts")).await.unwrap();
    sqlx::query("INSERT INTO recipes(id, title, instructions, content_hash) VALUES ('r1', 'Pho', '[]', 'h')")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO recipe_ingredients(recipe_id, raw_text, unit, canonical_id) VALUES ('r1', '2 star anise', 'pcs', 'star_anise')")
        .execute(db.pool())
        .await
        .unwrap();
    let hit: String =
        sqlx::query("SELECT recipe_id FROM recipes_fts WHERE recipes_fts MATCH 'anise'")
            .fetch_one(db.pool())
            .await
            .unwrap()
            .get(0);
    assert_eq!(hit, "r1");
}

#[tokio::test]
async fn open_fails_cleanly_for_unwritable_path() {
    let err = Db::open("/nonexistent-dir-xyz/sub/recipes.db")
        .await
        .unwrap_err();
    assert!(matches!(err, recipe_db::DbError::Sqlx(_)), "got {err:?}");
}
