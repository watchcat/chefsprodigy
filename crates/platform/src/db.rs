//! `platform.db`: the hub registry and the federation index (spec section 5.3).
//! Same pragmas as the recipe node and the gateway (WAL, NORMAL, foreign keys,
//! busy timeout). Every index change for one recipe (its row, its translations and
//! their FTS rows) is written in one transaction.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::migrate::Migrator;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::{Row, SqliteConnection};

use crate::error::PlatformError;
use crate::index::IndexDoc;

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);
const MAX_CONNECTIONS: u32 = 4;

/// Names `hubctl` refuses as well: they are platform hostnames, not hubs.
pub const RESERVED_NAMES: &[&str] = &[
    "www",
    "api",
    "mail",
    "admin",
    "gateway",
    "llm-gateway",
    "platform",
    "provisioner",
    "traefik",
];

#[derive(Debug, Clone)]
pub struct Db {
    pool: SqlitePool,
}

/// A registered hub with its poll status, as the admin API and CLI list it.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Hub {
    #[serde(skip)]
    pub id: i64,
    pub name: String,
    pub chef_name: Option<String>,
    pub internal_url: String,
    pub public_url: String,
    pub status: String,
    pub created_at: String,
    pub last_polled_at: Option<String>,
    pub last_ok_at: Option<String>,
    pub last_error: Option<String>,
    pub consecutive_failures: i64,
    pub next_poll_at: Option<String>,
    /// Recipes of this hub currently in the index
    pub recipe_count: i64,
}

/// `POST /admin/hubs` and `platform add-hub`: create the hub or update it in place.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubRegistration {
    pub name: String,
    /// Defaults to the hub's cluster Service, `http://hub-<name>.chefs-hubs.svc:8080`
    #[serde(default)]
    pub internal_url: Option<String>,
    pub public_url: String,
    /// Kept when omitted; filled from the hub's manifest while unset
    #[serde(default)]
    pub chef_name: Option<String>,
}

/// `PATCH /admin/hubs/{name}`: only the fields present change.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubPatch {
    /// An empty string clears it (the manifest's name is used again)
    #[serde(default)]
    pub chef_name: Option<String>,
    #[serde(default)]
    pub internal_url: Option<String>,
    #[serde(default)]
    pub public_url: Option<String>,
    /// `active` or `disabled` (disabled hubs are neither polled nor searchable)
    #[serde(default)]
    pub status: Option<String>,
}

/// One line of the public hub directory (`GET /api/v1/hubs`).
#[derive(Debug, Clone, Serialize, sqlx::FromRow, PartialEq)]
pub struct DirectoryEntry {
    pub name: String,
    pub chef_name: Option<String>,
    pub public_url: String,
    /// Public recipes of this hub in the index
    pub recipe_count: i64,
}

/// What the index holds for one recipe, for change detection against a manifest entry.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexedRecipe {
    pub updated_at: String,
    pub content_hash: String,
    /// lang → the translation's own `updated_at`
    pub translations: BTreeMap<String, String>,
}

/// Hub names are subdomain labels: lowercase letters, digits and hyphens.
pub fn valid_hub_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

fn check_hub_name(name: &str) -> Result<(), PlatformError> {
    if !valid_hub_name(name) {
        return Err(PlatformError::BadRequest(format!(
            "{name:?} is not a valid hub name (lowercase letters, digits, hyphens; 1-63 characters)"
        )));
    }
    if RESERVED_NAMES.contains(&name) {
        return Err(PlatformError::BadRequest(format!(
            "{name:?} is reserved and cannot be a hub name"
        )));
    }
    Ok(())
}

/// The hub's address inside the cluster (its `Service` in `chefs-hubs`).
pub fn default_internal_url(name: &str) -> String {
    format!("http://hub-{name}.chefs-hubs.svc:8080")
}

/// An `http(s)` base URL without credentials, query or fragment, normalized
/// (lowercase host, default port dropped) and without a trailing slash.
pub fn normalize_base_url(field: &str, raw: &str) -> Result<String, PlatformError> {
    let raw = raw.trim();
    let parsed = url::Url::parse(raw)
        .map_err(|e| PlatformError::BadRequest(format!("{field} {raw:?} is not a URL: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(PlatformError::BadRequest(format!(
            "{field} must be an http or https URL, got {raw:?}"
        )));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(PlatformError::BadRequest(format!(
            "{field} {raw:?} has no host"
        )));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(PlatformError::BadRequest(format!(
            "{field} must not carry credentials"
        )));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(PlatformError::BadRequest(format!(
            "{field} must not have a query or fragment"
        )));
    }
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

/// RFC 3339 UTC with second precision: comparable as text.
pub fn now() -> String {
    timestamp(chrono::Utc::now())
}

pub fn timestamp(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The `Hub` columns; a macro so `concat!` keeps every query a static string.
macro_rules! hub_select {
    () => {
        "SELECT h.id, h.name, h.chef_name, h.internal_url, h.public_url, h.status, \
         h.created_at, h.last_polled_at, h.last_ok_at, h.last_error, h.consecutive_failures, \
         h.next_poll_at, (SELECT COUNT(*) FROM recipes_index r WHERE r.hub_id = h.id) AS recipe_count \
         FROM hubs h"
    };
}

impl Db {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, PlatformError> {
        let options = SqliteConnectOptions::new()
            .filename(path.as_ref())
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT);
        let pool = SqlitePoolOptions::new()
            .max_connections(MAX_CONNECTIONS)
            .connect_with(options)
            .await?;
        MIGRATOR.run(&pool).await?;
        tracing::debug!(path = %path.as_ref().display(), "platform database ready");
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // ------------------------------------------------------------------- registry

    /// Register `reg.name` or update it in place (idempotent, for `hubctl create`).
    /// Re-registering re-activates the hub and clears its backoff so the next pass
    /// polls it. Returns the hub and whether it was newly created.
    pub async fn upsert_hub(&self, reg: &HubRegistration) -> Result<(Hub, bool), PlatformError> {
        let name = reg.name.trim();
        check_hub_name(name)?;
        let internal = match reg.internal_url.as_deref().map(str::trim) {
            Some(url) if !url.is_empty() => normalize_base_url("internal_url", url)?,
            _ => default_internal_url(name),
        };
        let public = normalize_base_url("public_url", &reg.public_url)?;
        let chef = reg
            .chef_name
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty());

        let mut tx = self.pool.begin().await?;
        let existing = sqlx::query("SELECT id, public_url FROM hubs WHERE name = ?")
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?;
        let created = existing.is_none();
        match existing {
            None => {
                sqlx::query(
                    "INSERT INTO hubs (name, chef_name, internal_url, public_url, status, created_at) \
                     VALUES (?, ?, ?, ?, 'active', ?)",
                )
                .bind(name)
                .bind(chef)
                .bind(&internal)
                .bind(&public)
                .bind(now())
                .execute(&mut *tx)
                .await?;
            }
            Some(row) => {
                let id: i64 = row.get("id");
                let old_public: String = row.get("public_url");
                sqlx::query(
                    "UPDATE hubs SET internal_url = ?, public_url = ?, chef_name = COALESCE(?, chef_name), \
                     status = 'active', consecutive_failures = 0, next_poll_at = NULL WHERE id = ?",
                )
                .bind(&internal)
                .bind(&public)
                .bind(chef)
                .bind(id)
                .execute(&mut *tx)
                .await?;
                rewrite_image_urls(&mut tx, id, &old_public, &public).await?;
            }
        }
        tx.commit().await?;
        let hub = self
            .get_hub(name)
            .await?
            .ok_or_else(|| PlatformError::NotFound(format!("hub {name} vanished")))?;
        Ok((hub, created))
    }

    /// Apply `patch` to the hub; `None` when no hub has that name.
    pub async fn update_hub(
        &self,
        name: &str,
        patch: &HubPatch,
    ) -> Result<Option<Hub>, PlatformError> {
        let internal = patch
            .internal_url
            .as_deref()
            .map(|u| normalize_base_url("internal_url", u))
            .transpose()?;
        let public = patch
            .public_url
            .as_deref()
            .map(|u| normalize_base_url("public_url", u))
            .transpose()?;
        if let Some(status) = patch.status.as_deref()
            && !matches!(status, "active" | "disabled")
        {
            return Err(PlatformError::BadRequest(format!(
                "status must be \"active\" or \"disabled\", got {status:?}"
            )));
        }

        let mut tx = self.pool.begin().await?;
        let Some(row) = sqlx::query("SELECT id, public_url FROM hubs WHERE name = ?")
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(None);
        };
        let id: i64 = row.get("id");
        let old_public: String = row.get("public_url");
        if let Some(chef) = patch.chef_name.as_deref() {
            sqlx::query("UPDATE hubs SET chef_name = NULLIF(?, '') WHERE id = ?")
                .bind(chef.trim())
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(internal) = internal {
            sqlx::query("UPDATE hubs SET internal_url = ? WHERE id = ?")
                .bind(internal)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(public) = public {
            sqlx::query("UPDATE hubs SET public_url = ? WHERE id = ?")
                .bind(&public)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            rewrite_image_urls(&mut tx, id, &old_public, &public).await?;
        }
        if let Some(status) = patch.status.as_deref() {
            sqlx::query(
                "UPDATE hubs SET status = ?1, \
                 consecutive_failures = CASE WHEN ?1 = 'active' THEN 0 ELSE consecutive_failures END, \
                 next_poll_at = CASE WHEN ?1 = 'active' THEN NULL ELSE next_poll_at END \
                 WHERE id = ?2",
            )
            .bind(status)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.get_hub(name).await
    }

    /// Delete the hub and everything indexed from it. Returns whether it existed.
    pub async fn remove_hub(&self, name: &str) -> Result<bool, PlatformError> {
        let mut tx = self.pool.begin().await?;
        let Some(id) = sqlx::query_scalar::<_, i64>("SELECT id FROM hubs WHERE name = ?")
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(false);
        };
        sqlx::query(
            "DELETE FROM recipes_fts WHERE rowid IN (\
             SELECT fts_rowid FROM recipes_index WHERE hub_id = ?1 \
             UNION ALL SELECT fts_rowid FROM translations_index WHERE hub_id = ?1)",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM translations_index WHERE hub_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM recipes_index WHERE hub_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM hubs WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn get_hub(&self, name: &str) -> Result<Option<Hub>, PlatformError> {
        Ok(
            sqlx::query_as::<_, Hub>(concat!(hub_select!(), " WHERE h.name = ?"))
                .bind(name)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn list_hubs(&self) -> Result<Vec<Hub>, PlatformError> {
        Ok(
            sqlx::query_as::<_, Hub>(concat!(hub_select!(), " ORDER BY h.name"))
                .fetch_all(&self.pool)
                .await?,
        )
    }

    pub async fn active_hubs(&self) -> Result<Vec<Hub>, PlatformError> {
        Ok(sqlx::query_as::<_, Hub>(concat!(
            hub_select!(),
            " WHERE h.status = 'active' ORDER BY h.name"
        ))
        .fetch_all(&self.pool)
        .await?)
    }

    /// Active hubs whose backoff (if any) has expired at `now`.
    pub async fn due_hubs(&self, now: &str) -> Result<Vec<Hub>, PlatformError> {
        Ok(sqlx::query_as::<_, Hub>(concat!(
            hub_select!(),
            " WHERE h.status = 'active' AND (h.next_poll_at IS NULL OR h.next_poll_at <= ?) \
             ORDER BY h.name"
        ))
        .bind(now)
        .fetch_all(&self.pool)
        .await?)
    }

    /// The public directory: active hubs with their indexed recipe counts.
    pub async fn directory(&self) -> Result<Vec<DirectoryEntry>, PlatformError> {
        Ok(sqlx::query_as::<_, DirectoryEntry>(
            "SELECT h.name, h.chef_name, h.public_url, COUNT(r.recipe_id) AS recipe_count \
             FROM hubs h LEFT JOIN recipes_index r ON r.hub_id = h.id \
             WHERE h.status = 'active' GROUP BY h.id ORDER BY h.name",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    // ------------------------------------------------------------------- poll status

    /// Use the manifest's chef name while the operator has not set one.
    pub async fn fill_chef_name(&self, hub_id: i64, chef: &str) -> Result<(), PlatformError> {
        let chef = chef.trim();
        if chef.is_empty() {
            return Ok(());
        }
        sqlx::query("UPDATE hubs SET chef_name = ? WHERE id = ? AND chef_name IS NULL")
            .bind(chef.chars().take(120).collect::<String>())
            .bind(hub_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_poll_ok(&self, hub_id: i64, at: &str) -> Result<(), PlatformError> {
        sqlx::query(
            "UPDATE hubs SET last_polled_at = ?1, last_ok_at = ?1, last_error = NULL, \
             consecutive_failures = 0, next_poll_at = NULL WHERE id = ?2",
        )
        .bind(at)
        .bind(hub_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_poll_failure(
        &self,
        hub_id: i64,
        at: &str,
        error: &str,
        failures: i64,
        next_poll_at: &str,
    ) -> Result<(), PlatformError> {
        sqlx::query(
            "UPDATE hubs SET last_polled_at = ?, last_error = ?, consecutive_failures = ?, \
             next_poll_at = ? WHERE id = ?",
        )
        .bind(at)
        .bind(error)
        .bind(failures)
        .bind(next_poll_at)
        .bind(hub_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ------------------------------------------------------------------- index

    /// Every indexed recipe of the hub with its translations' timestamps.
    pub async fn index_state(
        &self,
        hub_id: i64,
    ) -> Result<HashMap<String, IndexedRecipe>, PlatformError> {
        let mut state: HashMap<String, IndexedRecipe> = HashMap::new();
        let recipes = sqlx::query(
            "SELECT recipe_id, updated_at, content_hash FROM recipes_index WHERE hub_id = ?",
        )
        .bind(hub_id)
        .fetch_all(&self.pool)
        .await?;
        for r in recipes {
            state.insert(
                r.get("recipe_id"),
                IndexedRecipe {
                    updated_at: r.get("updated_at"),
                    content_hash: r.get("content_hash"),
                    translations: BTreeMap::new(),
                },
            );
        }
        let translations = sqlx::query(
            "SELECT recipe_id, lang, updated_at FROM translations_index WHERE hub_id = ?",
        )
        .bind(hub_id)
        .fetch_all(&self.pool)
        .await?;
        for t in translations {
            let recipe_id: String = t.get("recipe_id");
            if let Some(entry) = state.get_mut(&recipe_id) {
                entry
                    .translations
                    .insert(t.get("lang"), t.get("updated_at"));
            }
        }
        Ok(state)
    }

    /// Replace everything indexed for `doc.recipe_id` on the hub: the recipe row, its
    /// translations and one FTS row per language.
    pub async fn index_recipe(&self, hub_id: i64, doc: &IndexDoc) -> Result<(), PlatformError> {
        let mut tx = self.pool.begin().await?;
        delete_fts_rows(&mut tx, hub_id, &doc.recipe_id).await?;
        sqlx::query("DELETE FROM translations_index WHERE hub_id = ? AND recipe_id = ?")
            .bind(hub_id)
            .bind(&doc.recipe_id)
            .execute(&mut *tx)
            .await?;
        let source_lang = doc.language.clone().unwrap_or_default();
        let fts_rowid = insert_fts(
            &mut tx,
            hub_id,
            &doc.recipe_id,
            &source_lang,
            &doc.title,
            &doc.tags,
            &doc.canonical,
        )
        .await?;
        sqlx::query(
            "INSERT INTO recipes_index (hub_id, recipe_id, title, tags_json, canonical_json, language, \
             content_hash, updated_at, image_url, fts_rowid, indexed_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(hub_id, recipe_id) DO UPDATE SET title = excluded.title, \
             tags_json = excluded.tags_json, canonical_json = excluded.canonical_json, \
             language = excluded.language, content_hash = excluded.content_hash, \
             updated_at = excluded.updated_at, image_url = excluded.image_url, \
             fts_rowid = excluded.fts_rowid, indexed_at = excluded.indexed_at",
        )
        .bind(hub_id)
        .bind(&doc.recipe_id)
        .bind(&doc.title)
        .bind(serde_json::to_string(&doc.tags)?)
        .bind(serde_json::to_string(&doc.canonical)?)
        .bind(&doc.language)
        .bind(&doc.content_hash)
        .bind(&doc.updated_at)
        .bind(&doc.image_url)
        .bind(fts_rowid)
        .bind(now())
        .execute(&mut *tx)
        .await?;
        for t in &doc.translations {
            let fts_rowid = insert_fts(
                &mut tx,
                hub_id,
                &doc.recipe_id,
                &t.lang,
                &t.title,
                &t.tags,
                &[],
            )
            .await?;
            sqlx::query(
                "INSERT INTO translations_index (hub_id, recipe_id, lang, title, tags_json, origin, \
                 stale, updated_at, fts_rowid) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(hub_id)
            .bind(&doc.recipe_id)
            .bind(&t.lang)
            .bind(&t.title)
            .bind(serde_json::to_string(&t.tags)?)
            .bind(&t.origin)
            .bind(t.stale)
            .bind(&t.updated_at)
            .bind(fts_rowid)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Drop a recipe, its translations and their FTS rows from the index.
    pub async fn remove_recipe(&self, hub_id: i64, recipe_id: &str) -> Result<(), PlatformError> {
        let mut tx = self.pool.begin().await?;
        delete_fts_rows(&mut tx, hub_id, recipe_id).await?;
        sqlx::query("DELETE FROM translations_index WHERE hub_id = ? AND recipe_id = ?")
            .bind(hub_id)
            .bind(recipe_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM recipes_index WHERE hub_id = ? AND recipe_id = ?")
            .bind(hub_id)
            .bind(recipe_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

async fn delete_fts_rows(
    conn: &mut SqliteConnection,
    hub_id: i64,
    recipe_id: &str,
) -> Result<(), PlatformError> {
    sqlx::query(
        "DELETE FROM recipes_fts WHERE rowid IN (\
         SELECT fts_rowid FROM recipes_index WHERE hub_id = ?1 AND recipe_id = ?2 \
         UNION ALL SELECT fts_rowid FROM translations_index WHERE hub_id = ?1 AND recipe_id = ?2)",
    )
    .bind(hub_id)
    .bind(recipe_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn insert_fts(
    conn: &mut SqliteConnection,
    hub_id: i64,
    recipe_id: &str,
    lang: &str,
    title: &str,
    tags: &[String],
    canonical: &[String],
) -> Result<i64, PlatformError> {
    let result = sqlx::query(
        "INSERT INTO recipes_fts (hub_id, recipe_id, lang, title, tags, canonical) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(hub_id)
    .bind(recipe_id)
    .bind(lang)
    .bind(title)
    .bind(tags.join(" "))
    .bind(canonical.join(" "))
    .execute(&mut *conn)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Keep cover URLs pointing at the hub's current public URL.
async fn rewrite_image_urls(
    conn: &mut SqliteConnection,
    hub_id: i64,
    old_public: &str,
    new_public: &str,
) -> Result<(), PlatformError> {
    if old_public == new_public {
        return Ok(());
    }
    let prefix = format!("{old_public}/");
    sqlx::query(
        "UPDATE recipes_index SET image_url = ?1 || '/' || substr(image_url, ?2 + 1) \
         WHERE hub_id = ?3 AND image_url IS NOT NULL AND substr(image_url, 1, ?2) = ?4",
    )
    .bind(new_public)
    .bind(prefix.chars().count() as i64)
    .bind(hub_id)
    .bind(&prefix)
    .execute(&mut *conn)
    .await?;
    Ok(())
}
