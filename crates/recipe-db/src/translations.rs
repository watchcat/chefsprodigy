//! Cached per-language translations of recipe wording (`recipe_translations`).

use std::collections::HashMap;

use recipe_core::{RecipeTranslation, TranslationOrigin};
use sqlx::QueryBuilder;
use sqlx::sqlite::Sqlite;
use sqlx::FromRow;

use crate::{Db, DbError};

#[derive(FromRow)]
struct TranslationRow {
    lang: String,
    title: String,
    description: Option<String>,
    ingredients: String,
    instructions: String,
    tags: String,
    origin: String,
    source_updated_at: String,
    created_at: String,
    updated_at: String,
}

impl TryFrom<TranslationRow> for RecipeTranslation {
    type Error = DbError;
    fn try_from(r: TranslationRow) -> Result<Self, DbError> {
        Ok(RecipeTranslation {
            lang: r.lang,
            title: r.title,
            description: r.description,
            ingredients: serde_json::from_str(&r.ingredients)?,
            instructions: serde_json::from_str(&r.instructions)?,
            tags: serde_json::from_str(&r.tags)?,
            origin: r.origin.parse()?,
            source_updated_at: r.source_updated_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

impl Db {
    /// The stored translation of `recipe_id` into `lang`, if any.
    pub async fn get_translation(&self, recipe_id: &str, lang: &str) -> Result<Option<RecipeTranslation>, DbError> {
        let row: Option<TranslationRow> = sqlx::query_as(
            "SELECT lang, title, description, ingredients, instructions, tags, origin, source_updated_at, created_at, updated_at \
             FROM recipe_translations WHERE recipe_id = ? AND lang = ?",
        )
        .bind(recipe_id)
        .bind(lang)
        .fetch_optional(self.pool())
        .await?;
        row.map(RecipeTranslation::try_from).transpose()
    }

    /// Insert or replace a translation (keyed by recipe and language).
    pub async fn upsert_translation(&self, recipe_id: &str, t: &RecipeTranslation) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO recipe_translations (recipe_id, lang, title, description, ingredients, instructions, tags, origin, source_updated_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(recipe_id, lang) DO UPDATE SET title = excluded.title, description = excluded.description, \
             ingredients = excluded.ingredients, instructions = excluded.instructions, tags = excluded.tags, origin = excluded.origin, \
             source_updated_at = excluded.source_updated_at, updated_at = excluded.updated_at",
        )
        .bind(recipe_id)
        .bind(&t.lang)
        .bind(&t.title)
        .bind(&t.description)
        .bind(serde_json::to_string(&t.ingredients)?)
        .bind(serde_json::to_string(&t.instructions)?)
        .bind(serde_json::to_string(&t.tags)?)
        .bind(t.origin.as_str())
        .bind(&t.source_updated_at)
        .bind(&t.created_at)
        .bind(&t.updated_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Remove one translation. Returns whether a row existed.
    pub async fn delete_translation(&self, recipe_id: &str, lang: &str) -> Result<bool, DbError> {
        let r = sqlx::query("DELETE FROM recipe_translations WHERE recipe_id = ? AND lang = ?")
            .bind(recipe_id)
            .bind(lang)
            .execute(self.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Languages a recipe has translations for.
    pub async fn translation_langs(&self, recipe_id: &str) -> Result<Vec<String>, DbError> {
        Ok(sqlx::query_scalar("SELECT lang FROM recipe_translations WHERE recipe_id = ? ORDER BY lang")
            .bind(recipe_id)
            .fetch_all(self.pool())
            .await?)
    }

    /// Localized labels for the given canonical ids (only those stored).
    pub async fn get_terms(&self, canonical_ids: &[String], lang: &str) -> Result<HashMap<String, String>, DbError> {
        if canonical_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT canonical_id, label FROM canonical_terms WHERE lang = ");
        qb.push_bind(lang.to_string()).push(" AND canonical_id IN (");
        let mut sep = qb.separated(", ");
        for id in canonical_ids {
            sep.push_bind(id.clone());
        }
        qb.push(")");
        let rows: Vec<(String, String)> = qb.build_query_as().fetch_all(self.pool()).await?;
        Ok(rows.into_iter().collect())
    }

    /// Insert or replace labels for `lang`.
    pub async fn upsert_terms(&self, lang: &str, labels: &[(String, String)], origin: TranslationOrigin) -> Result<(), DbError> {
        if labels.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool().begin().await?;
        for (id, label) in labels {
            sqlx::query(
                "INSERT INTO canonical_terms (canonical_id, lang, label, origin) VALUES (?, ?, ?, ?) \
                 ON CONFLICT(canonical_id, lang) DO UPDATE SET label = excluded.label, origin = excluded.origin",
            )
            .bind(id)
            .bind(lang)
            .bind(label)
            .bind(origin.as_str())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Recipes whose source language is unknown (oldest first), fully loaded.
    pub async fn recipes_without_language(&self, limit: u32) -> Result<Vec<recipe_core::Recipe>, DbError> {
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM recipes WHERE language IS NULL ORDER BY rowid LIMIT ?")
            .bind(i64::from(limit))
            .fetch_all(self.pool())
            .await?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(r) = self.get_recipe(&id).await? {
                out.push(r);
            }
        }
        Ok(out)
    }

    /// Record a detected source language without touching `updated_at` or the hash.
    pub async fn set_recipe_language(&self, recipe_id: &str, lang: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE recipes SET language = ? WHERE id = ?")
            .bind(lang)
            .bind(recipe_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}
