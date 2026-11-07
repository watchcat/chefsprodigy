//! Snapshot and restore of the recipe database.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use recipe_core::{
    IngredientItem, InstructionStep, MeasurementUnit, RecipeInput, RecipeTranslation, SearchQuery,
    TranslationOrigin,
};
use recipe_db::Db;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "recipe-backup-test-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ing(canonical: &str) -> IngredientItem {
    IngredientItem {
        raw_text: format!("raw {canonical}"),
        metric_amount: Some(1.0),
        unit: MeasurementUnit::Pcs,
        canonical_id: canonical.to_string(),
        is_specialty: false,
        canonical_label: None,
    }
}

fn input(title: &str) -> RecipeInput {
    RecipeInput {
        title: title.to_string(),
        description: Some(format!("{title} description")),
        servings: 2,
        prep_time_minutes: Some(10),
        cook_time_minutes: Some(20),
        ingredients: vec![ing("beef_bones"), ing("egg")],
        instructions: vec![InstructionStep {
            step_number: 1,
            text: "Do the thing.".to_string(),
            timer_seconds: Some(60),
        }],
        tags: vec!["soup".to_string()],
        image_path: None,
        origin_url: None,
        author_did: None,
        is_public: false,
        language: None,
    }
}

fn translation(lang: &str, title: &str) -> RecipeTranslation {
    RecipeTranslation {
        lang: lang.to_string(),
        title: title.to_string(),
        description: Some(format!("{title} description")),
        ingredients: vec!["говядина".to_string(), "яйцо".to_string()],
        instructions: vec!["Сделать".to_string()],
        tags: vec!["суп".to_string()],
        origin: TranslationOrigin::Human,
        source_updated_at: "2026-11-07T00:00:00Z".to_string(),
        created_at: "2026-11-07T00:00:00Z".to_string(),
        updated_at: "2026-11-07T00:00:00Z".to_string(),
    }
}

#[tokio::test]
async fn a_snapshot_keeps_the_recipes_and_their_hashes() {
    let dir = temp_dir("snapshot");
    let db = Db::open(dir.join("recipes.db")).await.unwrap();
    let recipe = db.create_recipe(input("Pho")).await.unwrap();

    let snap = dir.join("snapshot.db");
    db.snapshot_to(&snap).await.unwrap();
    assert!(snap.exists(), "the snapshot file is written");

    let copy = Db::open(&snap).await.unwrap();
    let restored = copy.get_recipe(&recipe.id).await.unwrap().unwrap();
    assert_eq!(restored.id, recipe.id, "the id survives the copy");
    assert_eq!(restored.title, "Pho");
    assert_eq!(restored.content_hash, recipe.content_hash);
    assert_eq!(restored.instructions.len(), recipe.instructions.len());
    assert_eq!(restored.ingredients.len(), recipe.ingredients.len());
    assert_eq!(
        restored.ingredients[0].canonical_id, "beef_bones",
        "the ingredient ids survive the copy"
    );
}

#[tokio::test]
async fn a_snapshot_refuses_to_overwrite_an_existing_file() {
    let dir = temp_dir("snapshot-overwrite");
    let db = Db::open(dir.join("recipes.db")).await.unwrap();
    db.create_recipe(input("Pho")).await.unwrap();

    let snap = dir.join("snapshot.db");
    db.snapshot_to(&snap).await.unwrap();
    let err = db.snapshot_to(&snap).await.unwrap_err();
    assert!(
        err.to_string().contains("already exists"),
        "a stale snapshot must not be mistaken for a fresh one: {err}"
    );
}

#[tokio::test]
async fn a_restored_node_holds_the_same_recipes_with_the_same_ids_and_hashes() {
    let dir = temp_dir("restore");
    let source = Db::open(dir.join("source.db")).await.unwrap();
    let recipe = source.create_recipe(input("Pho")).await.unwrap();
    source
        .upsert_translation(&recipe.id, &translation("ru", "Фо"))
        .await
        .unwrap();

    let snap = dir.join("snapshot.db");
    source.snapshot_to(&snap).await.unwrap();

    let target = Db::open(dir.join("target.db")).await.unwrap();
    let report = target.restore_from(&snap).await.unwrap();
    assert_eq!(report.recipes, 1);
    assert_eq!(report.ingredients, recipe.ingredients.len() as u64);
    assert_eq!(report.translations, 1);

    let restored = target.get_recipe(&recipe.id).await.unwrap().unwrap();
    assert_eq!(restored.id, recipe.id, "the id survives the restore");
    assert_eq!(restored.content_hash, recipe.content_hash);
    assert_eq!(restored.title, recipe.title);
    assert_eq!(restored.ingredients.len(), recipe.ingredients.len());
    assert_eq!(
        restored.ingredients[0].canonical_id, "beef_bones",
        "the ingredient ids survive the restore"
    );

    let restored_translation = target
        .get_translation(&recipe.id, "ru")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored_translation.title, "Фо");
    assert_eq!(restored_translation.origin, TranslationOrigin::Human);
}

#[tokio::test]
async fn a_restore_refuses_a_node_that_already_holds_recipes() {
    let dir = temp_dir("restore-not-empty");
    let source = Db::open(dir.join("source.db")).await.unwrap();
    source.create_recipe(input("Pho")).await.unwrap();
    let snap = dir.join("snapshot.db");
    source.snapshot_to(&snap).await.unwrap();

    let target = Db::open(dir.join("target.db")).await.unwrap();
    target.create_recipe(input("Toast")).await.unwrap();
    let err = target.restore_from(&snap).await.unwrap_err();
    assert!(
        err.to_string().contains("empty node"),
        "restoring over existing recipes must be refused: {err}"
    );
}

#[tokio::test]
async fn a_restore_of_a_missing_file_is_refused() {
    let dir = temp_dir("restore-missing");
    let target = Db::open(dir.join("target.db")).await.unwrap();
    let err = target.restore_from(&dir.join("nope.db")).await.unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
}

#[tokio::test]
async fn a_restored_node_searches_and_counts_its_recipes() {
    let dir = temp_dir("restore-search");
    let source = Db::open(dir.join("source.db")).await.unwrap();
    let recipe = source.create_recipe(input("Pho")).await.unwrap();
    let snap = dir.join("snapshot.db");
    source.snapshot_to(&snap).await.unwrap();

    let target = Db::open(dir.join("target.db")).await.unwrap();
    target.restore_from(&snap).await.unwrap();

    // The FTS index is rebuilt by the insert triggers, so a restored recipe
    // is findable without copying the search tables.
    let hits = target
        .search_recipes(&SearchQuery {
            query: Some("Pho".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(hits.len(), 1, "the restored recipe is searchable");
    assert_eq!(hits[0].id, recipe.id);
    let count = target.count_recipes(&SearchQuery::default()).await.unwrap();
    assert_eq!(count, 1);
}
