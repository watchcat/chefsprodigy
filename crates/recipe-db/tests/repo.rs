//! Repository tests against a real on-disk SQLite database.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use recipe_core::{
    IngredientItem, InstructionStep, MeasurementUnit, RecipeInput, SearchQuery,
    calculate_content_hash,
};
use recipe_db::Db;

static COUNTER: AtomicU32 = AtomicU32::new(0);

async fn fresh_db() -> Db {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("recipe-repo-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    Db::open(dir.join("recipes.db")).await.unwrap()
}

fn ing(
    canonical: &str,
    amount: Option<f32>,
    unit: MeasurementUnit,
    specialty: bool,
) -> IngredientItem {
    IngredientItem {
        raw_text: format!("raw {canonical}"),
        metric_amount: amount,
        unit,
        canonical_id: canonical.to_string(),
        is_specialty: specialty,
        canonical_label: None,
    }
}

fn input(
    title: &str,
    tags: &[&str],
    ingredients: Vec<IngredientItem>,
    prep: Option<u16>,
    cook: Option<u16>,
) -> RecipeInput {
    RecipeInput {
        title: title.to_string(),
        description: Some(format!("{title} description")),
        servings: 2,
        prep_time_minutes: prep,
        cook_time_minutes: cook,
        ingredients,
        instructions: vec![
            InstructionStep {
                step_number: 1,
                text: "Do the thing.".to_string(),
                timer_seconds: Some(60),
            },
            InstructionStep {
                step_number: 2,
                text: "Serve.".to_string(),
                timer_seconds: None,
            },
        ],
        tags: tags.iter().map(|t| t.to_string()).collect(),
        image_path: None,
        origin_url: None,
        author_did: None,
        is_public: false,
        language: None,
    }
}

fn pho() -> RecipeInput {
    input(
        "Beef Pho",
        &["vietnamese", "soup"],
        vec![
            ing("beef_bones", Some(1000.0), MeasurementUnit::G, false),
            ing("star_anise", Some(3.0), MeasurementUnit::Pcs, true),
            ing("fish_sauce", Some(30.0), MeasurementUnit::Ml, true),
            ing("salt", None, MeasurementUnit::ToTaste, false),
        ],
        Some(30),
        Some(360),
    )
}

fn omelette() -> RecipeInput {
    input(
        "Cheese Omelette",
        &["breakfast", "quick"],
        vec![
            ing("egg", Some(3.0), MeasurementUnit::Pcs, false),
            ing("cheddar", Some(50.0), MeasurementUnit::G, false),
            ing("salt", None, MeasurementUnit::ToTaste, false),
        ],
        Some(5),
        Some(5),
    )
}

#[tokio::test]
async fn create_then_get_round_trips_every_field() {
    let db = fresh_db().await;
    let created = db.create_recipe(pho()).await.unwrap();

    assert_eq!(created.id.len(), 26, "ULID ids are 26 chars");
    assert_eq!(created.content_hash, calculate_content_hash(&created));
    assert!(!created.created_at.is_empty());

    let fetched = db
        .get_recipe(&created.id)
        .await
        .unwrap()
        .expect("recipe exists");
    assert_eq!(fetched, created);
    assert_eq!(fetched.ingredients.len(), 4);
    assert_eq!(fetched.ingredients[1].canonical_id, "star_anise");
    assert!(fetched.ingredients[1].is_specialty);
    assert_eq!(fetched.ingredients[3].unit, MeasurementUnit::ToTaste);
    assert_eq!(fetched.ingredients[3].metric_amount, None);
    assert_eq!(fetched.instructions[0].timer_seconds, Some(60));
    assert_eq!(fetched.tags, vec!["vietnamese", "soup"]);
}

#[tokio::test]
async fn get_unknown_id_returns_none() {
    let db = fresh_db().await;
    assert!(db.get_recipe("nope").await.unwrap().is_none());
}

#[tokio::test]
async fn update_replaces_ingredients_and_recomputes_hash() {
    let db = fresh_db().await;
    let created = db.create_recipe(pho()).await.unwrap();

    let mut edited = pho();
    edited.title = "Chicken Pho".to_string();
    edited.ingredients = vec![ing("chicken", Some(800.0), MeasurementUnit::G, false)];
    let updated = db
        .update_recipe(&created.id, edited)
        .await
        .unwrap()
        .expect("exists");

    assert_eq!(updated.id, created.id);
    assert_eq!(updated.created_at, created.created_at);
    assert_eq!(updated.title, "Chicken Pho");
    assert_eq!(updated.ingredients.len(), 1);
    assert_ne!(updated.content_hash, created.content_hash);
    assert_eq!(updated.content_hash, calculate_content_hash(&updated));

    let fetched = db.get_recipe(&created.id).await.unwrap().unwrap();
    assert_eq!(fetched, updated);

    // No orphaned ingredient rows remain.
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recipe_ingredients")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1);
}

#[tokio::test]
async fn update_unknown_id_returns_none() {
    let db = fresh_db().await;
    assert!(db.update_recipe("nope", pho()).await.unwrap().is_none());
}

#[tokio::test]
async fn delete_removes_recipe_and_reports_existence() {
    let db = fresh_db().await;
    let created = db.create_recipe(pho()).await.unwrap();
    assert!(db.delete_recipe(&created.id).await.unwrap());
    assert!(!db.delete_recipe(&created.id).await.unwrap());
    assert!(db.get_recipe(&created.id).await.unwrap().is_none());
}

#[tokio::test]
async fn search_without_filters_lists_newest_first() {
    let db = fresh_db().await;
    let a = db.create_recipe(omelette()).await.unwrap();
    let b = db.create_recipe(pho()).await.unwrap();
    let results = db.search_recipes(&SearchQuery::default()).await.unwrap();
    let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec![b.id.as_str(), a.id.as_str()]);
    assert_eq!(results[0].total_time_minutes, Some(390));
    assert_eq!(
        results[0].canonical_ingredients,
        vec!["beef_bones", "star_anise", "fish_sauce", "salt"]
    );
    assert_eq!(results[1].tags, vec!["breakfast", "quick"]);
}

#[tokio::test]
async fn search_full_text_matches_title_ingredient_and_tag() {
    let db = fresh_db().await;
    let pho_id = db.create_recipe(pho()).await.unwrap().id;
    let omelette_id = db.create_recipe(omelette()).await.unwrap().id;

    let by_title = db
        .search_recipes(&SearchQuery {
            query: Some("pho".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(by_title.len(), 1);
    assert_eq!(by_title[0].id, pho_id);

    let by_ingredient = db
        .search_recipes(&SearchQuery {
            query: Some("cheddar".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(by_ingredient.len(), 1);
    assert_eq!(by_ingredient[0].id, omelette_id);

    let by_tag = db
        .search_recipes(&SearchQuery {
            query: Some("vietnamese".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(by_tag.len(), 1);
    assert_eq!(by_tag[0].id, pho_id);

    let prefix = db
        .search_recipes(&SearchQuery {
            query: Some("omel".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(prefix.len(), 1, "prefix matching is enabled");
}

#[tokio::test]
async fn search_full_text_tolerates_fts_syntax_characters() {
    let db = fresh_db().await;
    db.create_recipe(pho()).await.unwrap();
    for q in ["beef \"pho\"", "pho*", "(pho) AND", "pho OR", "-pho", "\""] {
        let r = db
            .search_recipes(&SearchQuery {
                query: Some(q.into()),
                ..Default::default()
            })
            .await;
        assert!(r.is_ok(), "query {q:?} must not error: {r:?}");
    }
    let none = db
        .search_recipes(&SearchQuery {
            query: Some("   ".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(none.len(), 1, "blank query behaves like no query");
}

#[tokio::test]
async fn search_filters_by_tag_time_and_excluded_ingredients() {
    let db = fresh_db().await;
    let pho_id = db.create_recipe(pho()).await.unwrap().id;
    let omelette_id = db.create_recipe(omelette()).await.unwrap().id;

    let by_tag = db
        .search_recipes(&SearchQuery {
            tag: Some("quick".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        by_tag.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec![omelette_id.as_str()]
    );

    let fast = db
        .search_recipes(&SearchQuery {
            max_total_time_minutes: Some(30),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        fast.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec![omelette_id.as_str()]
    );

    let no_eggs = db
        .search_recipes(&SearchQuery {
            exclude_canonical_ingredients: vec!["egg".into()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        no_eggs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec![pho_id.as_str()]
    );
}

#[tokio::test]
async fn search_with_pantry_requires_overlap_and_ranks_by_it() {
    let db = fresh_db().await;
    let pho_id = db.create_recipe(pho()).await.unwrap().id;
    let omelette_id = db.create_recipe(omelette()).await.unwrap().id;
    let _toast = db
        .create_recipe(input(
            "Toast",
            &[],
            vec![ing("bread", Some(2.0), MeasurementUnit::Pcs, false)],
            Some(2),
            None,
        ))
        .await
        .unwrap();

    // Pantry overlaps omelette on 2 items (egg, salt) and pho on 1 (salt); toast on none.
    let results = db
        .search_recipes(&SearchQuery {
            include_canonical_ingredients: vec!["egg".into(), "salt".into()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec![omelette_id.as_str(), pho_id.as_str()]
    );
}

#[tokio::test]
async fn search_paginates_with_limit_and_offset() {
    let db = fresh_db().await;
    for i in 0..5 {
        db.create_recipe(input(&format!("Recipe {i}"), &[], vec![], None, None))
            .await
            .unwrap();
    }
    let page1 = db
        .search_recipes(&SearchQuery {
            limit: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();
    let page2 = db
        .search_recipes(&SearchQuery {
            limit: Some(2),
            offset: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();
    let page3 = db
        .search_recipes(&SearchQuery {
            limit: Some(2),
            offset: Some(4),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(page2.len(), 2);
    assert_eq!(page3.len(), 1);
    assert_ne!(page1[0].id, page2[0].id);

    let capped = db
        .search_recipes(&SearchQuery {
            limit: Some(10_000),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(capped.len(), 5);
}

#[tokio::test]
async fn public_manifest_lists_only_public_recipes() {
    let db = fresh_db().await;
    let mut public = pho();
    public.is_public = true;
    let public = db.create_recipe(public).await.unwrap();
    let _private = db.create_recipe(omelette()).await.unwrap();

    let manifest = db.public_manifest().await.unwrap();
    assert_eq!(manifest.len(), 1);
    assert_eq!(manifest[0].id, public.id);
    assert_eq!(manifest[0].content_hash, public.content_hash);
    assert_eq!(manifest[0].updated_at, public.updated_at);
}

#[tokio::test]
async fn get_public_recipe_hides_private_ones() {
    let db = fresh_db().await;
    let private = db.create_recipe(omelette()).await.unwrap();
    let mut p = pho();
    p.is_public = true;
    let public = db.create_recipe(p).await.unwrap();

    assert!(db.get_public_recipe(&private.id).await.unwrap().is_none());
    assert_eq!(
        db.get_public_recipe(&public.id).await.unwrap().unwrap().id,
        public.id
    );
}

#[tokio::test]
async fn count_recipes_matches_search_filters_ignoring_pagination() {
    let db = fresh_db().await;
    db.create_recipe(pho()).await.unwrap();
    db.create_recipe(omelette()).await.unwrap();
    for i in 0..3 {
        db.create_recipe(input(
            &format!("Filler {i}"),
            &["quick"],
            vec![ing("egg", Some(1.0), MeasurementUnit::Pcs, false)],
            Some(1),
            None,
        ))
        .await
        .unwrap();
    }
    assert_eq!(db.count_recipes(&SearchQuery::default()).await.unwrap(), 5);
    assert_eq!(
        db.count_recipes(&SearchQuery {
            limit: Some(1),
            offset: Some(4),
            ..Default::default()
        })
        .await
        .unwrap(),
        5,
        "limit/offset do not affect the count"
    );
    assert_eq!(
        db.count_recipes(&SearchQuery {
            query: Some("pho".into()),
            ..Default::default()
        })
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        db.count_recipes(&SearchQuery {
            tag: Some("quick".into()),
            ..Default::default()
        })
        .await
        .unwrap(),
        4
    );
    assert_eq!(
        db.count_recipes(&SearchQuery {
            exclude_canonical_ingredients: vec!["egg".into()],
            ..Default::default()
        })
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        db.count_recipes(&SearchQuery {
            include_canonical_ingredients: vec!["cheddar".into()],
            ..Default::default()
        })
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        db.count_recipes(&SearchQuery {
            max_total_time_minutes: Some(5),
            ..Default::default()
        })
        .await
        .unwrap(),
        3
    );
}

mod i18n {
    use super::*;
    use recipe_core::{RecipeTranslation, TranslationOrigin};

    fn translation(
        lang: &str,
        origin: TranslationOrigin,
        source_updated_at: &str,
    ) -> RecipeTranslation {
        RecipeTranslation {
            lang: lang.into(),
            title: format!("title-{lang}"),
            description: Some("d".into()),
            ingredients: vec!["a".into(), "b".into()],
            instructions: vec!["s1".into()],
            tags: vec!["t".into()],
            origin,
            source_updated_at: source_updated_at.into(),
            created_at: "c".into(),
            updated_at: "u".into(),
        }
    }

    #[tokio::test]
    async fn language_round_trips_and_appears_in_summaries() {
        let db = fresh_db().await;
        let mut i = pho();
        i.language = Some("nl".into());
        let r = db.create_recipe(i).await.unwrap();
        assert_eq!(
            db.get_recipe(&r.id)
                .await
                .unwrap()
                .unwrap()
                .language
                .as_deref(),
            Some("nl")
        );
        let s = db.search_recipes(&SearchQuery::default()).await.unwrap();
        assert_eq!(s[0].language.as_deref(), Some("nl"));
        let mut e = pho();
        e.language = Some("en".into());
        assert_eq!(
            db.update_recipe(&r.id, e)
                .await
                .unwrap()
                .unwrap()
                .language
                .as_deref(),
            Some("en")
        );
    }

    #[tokio::test]
    async fn translations_upsert_fetch_delete_and_cascade() {
        let db = fresh_db().await;
        let r = db.create_recipe(pho()).await.unwrap();
        assert!(db.get_translation(&r.id, "ru").await.unwrap().is_none());

        db.upsert_translation(
            &r.id,
            &translation("ru", TranslationOrigin::Llm, &r.updated_at),
        )
        .await
        .unwrap();
        let t = db.get_translation(&r.id, "ru").await.unwrap().unwrap();
        assert_eq!(
            (t.title.as_str(), t.origin),
            ("title-ru", TranslationOrigin::Llm)
        );
        assert_eq!(t.ingredients, vec!["a", "b"]);

        db.upsert_translation(
            &r.id,
            &translation("ru", TranslationOrigin::Human, &r.updated_at),
        )
        .await
        .unwrap();
        let t = db.get_translation(&r.id, "ru").await.unwrap().unwrap();
        assert_eq!(t.origin, TranslationOrigin::Human, "upsert replaces");
        assert_eq!(db.translation_langs(&r.id).await.unwrap(), vec!["ru"]);

        let hit: Option<String> = sqlx::query_scalar(
            "SELECT recipe_id FROM translations_fts WHERE translations_fts MATCH 'title'",
        )
        .fetch_optional(db.pool())
        .await
        .unwrap();
        assert_eq!(hit.as_deref(), Some(r.id.as_str()), "fts trigger synced");

        assert!(db.delete_translation(&r.id, "ru").await.unwrap());
        assert!(!db.delete_translation(&r.id, "ru").await.unwrap());

        db.upsert_translation(
            &r.id,
            &translation("nl", TranslationOrigin::Llm, &r.updated_at),
        )
        .await
        .unwrap();
        db.delete_recipe(&r.id).await.unwrap();
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recipe_translations")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(left, 0, "cascade");
    }
}

mod i18n_search {
    use super::*;
    use recipe_core::{RecipeTranslation, TranslationOrigin};

    fn ru(title: &str, ingredients: &[&str], source_updated_at: &str) -> RecipeTranslation {
        RecipeTranslation {
            lang: "ru".into(),
            title: title.into(),
            description: None,
            ingredients: ingredients.iter().map(|s| s.to_string()).collect(),
            instructions: vec![],
            tags: vec!["суп".into()],
            origin: TranslationOrigin::Llm,
            source_updated_at: source_updated_at.into(),
            created_at: "c".into(),
            updated_at: "u".into(),
        }
    }

    #[tokio::test]
    async fn search_matches_translated_text_and_dedupes_recipes() {
        let db = fresh_db().await;
        let pho_r = db.create_recipe(pho()).await.unwrap();
        db.create_recipe(omelette()).await.unwrap();
        db.upsert_translation(
            &pho_r.id,
            &ru(
                "Говяжий фо",
                &["говяжьи кости", "бадьян"],
                &pho_r.updated_at,
            ),
        )
        .await
        .unwrap();

        let hits = db
            .search_recipes(&SearchQuery {
                query: Some("бадьян".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
            vec![pho_r.id.as_str()]
        );
        assert_eq!(
            db.count_recipes(&SearchQuery {
                query: Some("бадьян".into()),
                ..Default::default()
            })
            .await
            .unwrap(),
            1
        );

        // "pho" matches the source title and "фо" the translation: still one row.
        let hits = db
            .search_recipes(&SearchQuery {
                query: Some("pho".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        let hits = db
            .search_recipes(&SearchQuery {
                query: Some("суп".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "translated tags are indexed too");
        assert_eq!(
            db.count_recipes(&SearchQuery {
                query: Some("pho".into()),
                ..Default::default()
            })
            .await
            .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn summaries_use_the_translation_when_a_lang_is_requested() {
        let db = fresh_db().await;
        let pho_r = db.create_recipe(pho()).await.unwrap();
        let om = db.create_recipe(omelette()).await.unwrap();
        db.upsert_translation(
            &pho_r.id,
            &ru("Говяжий фо", &["говяжьи кости"], &pho_r.updated_at),
        )
        .await
        .unwrap();

        let items = db
            .search_recipes(&SearchQuery {
                lang: Some("ru".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let pho_s = items.iter().find(|s| s.id == pho_r.id).unwrap();
        assert_eq!(pho_s.title, "Говяжий фо");
        assert_eq!(pho_s.tags, vec!["суп"]);
        assert_eq!(pho_s.translation_lang.as_deref(), Some("ru"));
        let om_s = items.iter().find(|s| s.id == om.id).unwrap();
        assert_eq!(
            om_s.title, "Cheese Omelette",
            "untranslated recipes keep the source"
        );
        assert_eq!(om_s.translation_lang, None);

        let plain = db.search_recipes(&SearchQuery::default()).await.unwrap();
        assert_eq!(
            plain.iter().find(|s| s.id == pho_r.id).unwrap().title,
            "Beef Pho"
        );
    }

    #[tokio::test]
    async fn canonical_terms_round_trip() {
        let db = fresh_db().await;
        assert!(
            db.get_terms(&["garlic".into()], "ru")
                .await
                .unwrap()
                .is_empty()
        );
        db.upsert_terms(
            "ru",
            &[
                ("garlic".into(), "чеснок".into()),
                ("salt".into(), "соль".into()),
            ],
            TranslationOrigin::Llm,
        )
        .await
        .unwrap();
        let m = db
            .get_terms(&["garlic".into(), "salt".into(), "egg".into()], "ru")
            .await
            .unwrap();
        assert_eq!(m.get("garlic").map(String::as_str), Some("чеснок"));
        assert_eq!(m.len(), 2);
        db.upsert_terms(
            "ru",
            &[("garlic".into(), "чесночок".into())],
            TranslationOrigin::Human,
        )
        .await
        .unwrap();
        assert_eq!(
            db.get_terms(&["garlic".into()], "ru").await.unwrap()["garlic"],
            "чесночок"
        );
    }
}

mod backfill {
    use super::*;

    #[tokio::test]
    async fn recipes_without_language_are_listed_and_set_without_touching_updated_at() {
        let db = fresh_db().await;
        let a = db.create_recipe(pho()).await.unwrap();
        let mut with = omelette();
        with.language = Some("en".into());
        let b = db.create_recipe(with).await.unwrap();

        let missing = db.recipes_without_language(10).await.unwrap();
        assert_eq!(
            missing.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec![a.id.as_str()]
        );
        assert_eq!(
            missing[0].ingredients.len(),
            4,
            "full recipes, so a text sample can be built"
        );

        db.set_recipe_language(&a.id, "nl").await.unwrap();
        let again = db.get_recipe(&a.id).await.unwrap().unwrap();
        assert_eq!(again.language.as_deref(), Some("nl"));
        assert_eq!(again.updated_at, a.updated_at, "backfill is not an edit");
        assert_eq!(again.content_hash, a.content_hash);
        assert!(db.recipes_without_language(10).await.unwrap().is_empty());
        let _ = b;
    }
}
