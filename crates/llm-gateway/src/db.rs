//! `gateway.db`: hubs (hashed tokens, quota columns, encrypted BYOK keys), per-day usage,
//! the reply cache and model prices. Same pragmas as the recipe node (WAL, NORMAL, foreign
//! keys, busy timeout).

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqliteRow;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

use crate::auth::{hash_token, new_token};
use crate::error::GatewayError;
use crate::quota::UsageTotals;

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);
const MAX_CONNECTIONS: u32 = 4;

#[derive(Debug, Clone)]
pub struct Db {
    pool: SqlitePool,
}

/// Per-hub quota columns (free tier by default, docs/SPEC-hosted-platform.md section 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quota {
    pub onboarding_daily_requests: i64,
    pub onboarding_days: i64,
    pub daily_request_limit: i64,
    /// Secondary cap on input + output tokens per day; `None` means no cap
    pub daily_token_limit: Option<i64>,
    pub monthly_request_limit: i64,
    pub monthly_cost_cap_cents: i64,
}

impl Default for Quota {
    fn default() -> Self {
        Self {
            onboarding_daily_requests: 30,
            onboarding_days: 3,
            daily_request_limit: 10,
            daily_token_limit: None,
            monthly_request_limit: 150,
            monthly_cost_cap_cents: 100,
        }
    }
}

/// Partial quota update: absent fields keep their value; `daily_token_limit: 0`
/// removes that cap. Unknown fields are refused so a misspelt limit is not silently ignored.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaPatch {
    pub onboarding_daily_requests: Option<i64>,
    pub onboarding_days: Option<i64>,
    pub daily_request_limit: Option<i64>,
    pub daily_token_limit: Option<i64>,
    pub monthly_request_limit: Option<i64>,
    pub monthly_cost_cap_cents: Option<i64>,
}

impl QuotaPatch {
    /// Limits are counts and cents: negative values are refused.
    pub fn validate(&self) -> Result<(), GatewayError> {
        let fields = [
            ("onboarding_daily_requests", self.onboarding_daily_requests),
            ("onboarding_days", self.onboarding_days),
            ("daily_request_limit", self.daily_request_limit),
            ("daily_token_limit", self.daily_token_limit),
            ("monthly_request_limit", self.monthly_request_limit),
            ("monthly_cost_cap_cents", self.monthly_cost_cap_cents),
        ];
        match fields.iter().find(|(_, v)| v.is_some_and(|v| v < 0)) {
            Some((name, _)) => Err(GatewayError::BadRequest(format!(
                "{name} must be zero or positive"
            ))),
            None => Ok(()),
        }
    }

    pub fn apply(&self, quota: &mut Quota) {
        if let Some(v) = self.onboarding_daily_requests {
            quota.onboarding_daily_requests = v;
        }
        if let Some(v) = self.onboarding_days {
            quota.onboarding_days = v;
        }
        if let Some(v) = self.daily_request_limit {
            quota.daily_request_limit = v;
        }
        if let Some(v) = self.daily_token_limit {
            quota.daily_token_limit = (v > 0).then_some(v);
        }
        if let Some(v) = self.monthly_request_limit {
            quota.monthly_request_limit = v;
        }
        if let Some(v) = self.monthly_cost_cap_cents {
            quota.monthly_cost_cap_cents = v;
        }
    }
}

/// `hubs.status`; a disabled hub's token is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HubStatus {
    Active,
    Disabled,
}

impl HubStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }
}

/// A registered hub (never the token or its hash; the BYOK blob stays encrypted).
#[derive(Debug, Clone, Serialize)]
pub struct Hub {
    #[serde(skip)]
    pub id: i64,
    pub name: String,
    pub status: String,
    pub created_at: String,
    pub quota: Quota,
    #[serde(skip)]
    pub byok_json_enc: Option<Vec<u8>>,
}

/// A freshly registered hub with its token, shown once. `llm-gateway add-hub --json` prints
/// exactly these three fields and hubctl parses them, so the shape stays fixed.
#[derive(Debug, Clone, Serialize)]
pub struct NewHub {
    pub name: String,
    pub token: String,
    pub created_at: String,
}

/// A cached vendor reply.
#[derive(Debug, Clone)]
pub struct CachedReply {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CacheStats {
    pub entries: i64,
    pub bytes: i64,
    pub hits: i64,
}

/// A `model_prices` row: estimated cost in cents per million tokens.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelPrice {
    pub vendor: String,
    pub model: String,
    pub input_cents_per_mtok: f64,
    pub output_cents_per_mtok: f64,
}

const HUB_COLUMNS: &str = "id, name, status, created_at, onboarding_daily_requests, onboarding_days, \
    daily_request_limit, daily_token_limit, monthly_request_limit, monthly_cost_cap_cents, byok_json_enc";

fn hub_from_row(r: &SqliteRow) -> Hub {
    Hub {
        id: r.get("id"),
        name: r.get("name"),
        status: r.get("status"),
        created_at: r.get("created_at"),
        quota: Quota {
            onboarding_daily_requests: r.get("onboarding_daily_requests"),
            onboarding_days: r.get("onboarding_days"),
            daily_request_limit: r.get("daily_request_limit"),
            daily_token_limit: r.get("daily_token_limit"),
            monthly_request_limit: r.get("monthly_request_limit"),
            monthly_cost_cap_cents: r.get("monthly_cost_cap_cents"),
        },
        byok_json_enc: r.get("byok_json_enc"),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UsageRow {
    pub hub: String,
    pub day: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_cents: f64,
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

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Entries created at or before this instant are older than `ttl`.
fn cache_cutoff(ttl: Duration) -> String {
    let ttl = chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::MAX);
    chrono::Utc::now()
        .checked_sub_signed(ttl)
        .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Db {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, GatewayError> {
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
        tracing::info!(path = %path.as_ref().display(), "gateway database ready");
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Register a hub with the free-tier quota; returns its token (stored only as a hash).
    pub async fn create_hub(&self, name: &str) -> Result<NewHub, GatewayError> {
        self.create_hub_with(name, Quota::default()).await
    }

    pub async fn create_hub_with(&self, name: &str, quota: Quota) -> Result<NewHub, GatewayError> {
        if !valid_hub_name(name) {
            return Err(GatewayError::BadRequest(format!(
                "{name:?} is not a valid hub name (lowercase letters, digits, hyphens; 1-63 characters)"
            )));
        }
        let token = new_token();
        let created_at = now();
        let result = sqlx::query(
            "INSERT INTO hubs (name, token_hash, status, created_at, onboarding_daily_requests, onboarding_days, \
             daily_request_limit, daily_token_limit, monthly_request_limit, monthly_cost_cap_cents) \
             VALUES (?, ?, 'active', ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(name)
        .bind(hash_token(&token))
        .bind(&created_at)
        .bind(quota.onboarding_daily_requests)
        .bind(quota.onboarding_days)
        .bind(quota.daily_request_limit)
        .bind(quota.daily_token_limit)
        .bind(quota.monthly_request_limit)
        .bind(quota.monthly_cost_cap_cents)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(NewHub {
                name: name.to_string(),
                token,
                created_at,
            }),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                Err(GatewayError::Conflict(format!("hub {name} already exists")))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Issue a new token for `name`, registering the hub when it is unknown and
    /// re-activating it when it was disabled. The previous token stops working; quota
    /// columns and `created_at` (the onboarding start) are kept.
    pub async fn rotate_hub(&self, name: &str) -> Result<NewHub, GatewayError> {
        if !valid_hub_name(name) {
            return self.create_hub(name).await;
        }
        let token = new_token();
        let result = sqlx::query(
            "UPDATE hubs SET token_hash = ?, status = 'active' WHERE name = ? RETURNING created_at",
        )
        .bind(hash_token(&token))
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        match result {
            Some(row) => Ok(NewHub {
                name: name.to_string(),
                token,
                created_at: row.get("created_at"),
            }),
            None => self.create_hub(name).await,
        }
    }

    /// Mark the hub disabled so its token is refused (`disable-hub`, `PATCH {"status":
    /// "disabled"}`). Returns whether a hub matched.
    pub async fn disable_hub(&self, name: &str) -> Result<bool, GatewayError> {
        Ok(self
            .update_hub(name, &QuotaPatch::default(), Some(HubStatus::Disabled))
            .await?
            .is_some())
    }

    pub async fn list_hubs(&self) -> Result<Vec<Hub>, GatewayError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {HUB_COLUMNS} FROM hubs ORDER BY name"
        )))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(hub_from_row).collect())
    }

    pub async fn hub_by_name(&self, name: &str) -> Result<Option<Hub>, GatewayError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {HUB_COLUMNS} FROM hubs WHERE name = ?"
        )))
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(hub_from_row))
    }

    /// The active hub owning `token`, if any.
    pub async fn find_hub_by_token(&self, token: &str) -> Result<Option<Hub>, GatewayError> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {HUB_COLUMNS} FROM hubs WHERE token_hash = ? AND status = 'active'"
        )))
        .bind(hash_token(token))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(hub_from_row))
    }

    /// Apply a quota patch and/or a status change in one statement; `None` when the hub
    /// does not exist. The token is untouched (a re-activated hub keeps it; `rotate_hub`
    /// issues a new one).
    pub async fn update_hub(
        &self,
        name: &str,
        patch: &QuotaPatch,
        status: Option<HubStatus>,
    ) -> Result<Option<Hub>, GatewayError> {
        patch.validate()?;
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE hubs SET status = COALESCE(?1, status), \
             onboarding_daily_requests = COALESCE(?2, onboarding_daily_requests), \
             onboarding_days = COALESCE(?3, onboarding_days), \
             daily_request_limit = COALESCE(?4, daily_request_limit), \
             daily_token_limit = CASE WHEN ?5 IS NULL THEN daily_token_limit WHEN ?5 = 0 THEN NULL ELSE ?5 END, \
             monthly_request_limit = COALESCE(?6, monthly_request_limit), \
             monthly_cost_cap_cents = COALESCE(?7, monthly_cost_cap_cents) \
             WHERE name = ?8 RETURNING {HUB_COLUMNS}"
        )))
        .bind(status.map(HubStatus::as_str))
        .bind(patch.onboarding_daily_requests)
        .bind(patch.onboarding_days)
        .bind(patch.daily_request_limit)
        .bind(patch.daily_token_limit)
        .bind(patch.monthly_request_limit)
        .bind(patch.monthly_cost_cap_cents)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(hub_from_row))
    }

    /// Remove a hub and, through the foreign key, its usage rows.
    pub async fn delete_hub(&self, name: &str) -> Result<bool, GatewayError> {
        let result = sqlx::query("DELETE FROM hubs WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Store (or clear, with `None`) a hub's encrypted BYOK map.
    pub async fn set_byok_blob(
        &self,
        hub_id: i64,
        blob: Option<&[u8]>,
    ) -> Result<(), GatewayError> {
        sqlx::query("UPDATE hubs SET byok_json_enc = ? WHERE id = ?")
            .bind(blob)
            .bind(hub_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Usage summed over the UTC days `first..=last` (`YYYY-MM-DD`).
    pub async fn usage_sum(
        &self,
        hub_id: i64,
        first: &str,
        last: &str,
    ) -> Result<UsageTotals, GatewayError> {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(requests), 0) AS requests, COALESCE(SUM(input_tokens), 0) AS input_tokens, \
             COALESCE(SUM(output_tokens), 0) AS output_tokens, COALESCE(SUM(cost_cents), 0.0) AS cost_cents \
             FROM usage_daily WHERE hub_id = ? AND day >= ? AND day <= ?",
        )
        .bind(hub_id)
        .bind(first)
        .bind(last)
        .fetch_one(&self.pool)
        .await?;
        Ok(UsageTotals {
            requests: row.get("requests"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            cost_cents: row.get("cost_cents"),
        })
    }

    /// The cached reply for `key` if it is younger than `ttl`, counted as a hit (`hits`,
    /// `last_hit` for the LRU order). Created and checked at second resolution, so a zero
    /// TTL never hits.
    pub async fn cache_hit(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Result<Option<CachedReply>, GatewayError> {
        let row = sqlx::query(
            "UPDATE cache SET hits = hits + 1, last_hit = ? WHERE key_hash = ? AND created_at > ? \
             RETURNING status, body",
        )
        .bind(now())
        .bind(key)
        .bind(cache_cutoff(ttl))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| CachedReply {
            status: u16::try_from(r.get::<i64, _>("status")).unwrap_or(200),
            body: r.get("body"),
        }))
    }

    pub async fn cache_put(
        &self,
        key: &str,
        vendor: &str,
        model: &str,
        status: u16,
        body: &[u8],
    ) -> Result<(), GatewayError> {
        let stamp = now();
        sqlx::query(
            "INSERT OR REPLACE INTO cache (key_hash, vendor, model, status, body, created_at, last_hit, hits) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 0)",
        )
        .bind(key)
        .bind(vendor)
        .bind(model)
        .bind(i64::from(status))
        .bind(body)
        .bind(&stamp)
        .bind(&stamp)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Drop entries older than `ttl`, then keep the `max_entries` most recently hit ones.
    pub async fn cache_evict(&self, max_entries: usize, ttl: Duration) -> Result<(), GatewayError> {
        sqlx::query("DELETE FROM cache WHERE created_at <= ?")
            .bind(cache_cutoff(ttl))
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "DELETE FROM cache WHERE key_hash IN (\
                 SELECT key_hash FROM cache ORDER BY last_hit DESC, created_at DESC LIMIT -1 OFFSET ?)",
        )
        .bind(i64::try_from(max_entries).unwrap_or(i64::MAX))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn cache_stats(&self) -> Result<CacheStats, GatewayError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS entries, COALESCE(SUM(LENGTH(body)), 0) AS bytes, COALESCE(SUM(hits), 0) AS hits FROM cache",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(CacheStats {
            entries: row.get("entries"),
            bytes: row.get("bytes"),
            hits: row.get("hits"),
        })
    }

    /// Add one successful request to the hub's row for `day` (UTC, `YYYY-MM-DD`).
    pub async fn record_usage(
        &self,
        hub_id: i64,
        day: &str,
        input_tokens: i64,
        output_tokens: i64,
        cost_cents: f64,
    ) -> Result<(), GatewayError> {
        sqlx::query(
            "INSERT INTO usage_daily (hub_id, day, requests, input_tokens, output_tokens, cost_cents) \
             VALUES (?, ?, 1, ?, ?, ?) \
             ON CONFLICT(hub_id, day) DO UPDATE SET requests = requests + 1, \
             input_tokens = input_tokens + excluded.input_tokens, \
             output_tokens = output_tokens + excluded.output_tokens, \
             cost_cents = cost_cents + excluded.cost_cents",
        )
        .bind(hub_id)
        .bind(day)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(cost_cents)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Usage rows, newest day first, optionally for one hub and a day range (inclusive).
    pub async fn usage(
        &self,
        hub: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<UsageRow>, GatewayError> {
        let rows = sqlx::query(
            "SELECT h.name, u.day, u.requests, u.input_tokens, u.output_tokens, u.cost_cents \
             FROM usage_daily u JOIN hubs h ON h.id = u.hub_id \
             WHERE (?1 IS NULL OR h.name = ?1) AND (?2 IS NULL OR u.day >= ?2) AND (?3 IS NULL OR u.day <= ?3) \
             ORDER BY u.day DESC, h.name",
        )
        .bind(hub)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| UsageRow {
                hub: r.get("name"),
                day: r.get("day"),
                requests: r.get("requests"),
                input_tokens: r.get("input_tokens"),
                output_tokens: r.get("output_tokens"),
                cost_cents: r.get("cost_cents"),
            })
            .collect())
    }

    /// `(input, output)` cents per million tokens, when a price is configured.
    pub async fn price_for(
        &self,
        vendor: &str,
        model: &str,
    ) -> Result<Option<(f64, f64)>, GatewayError> {
        let row = sqlx::query(
            "SELECT input_cents_per_mtok, output_cents_per_mtok FROM model_prices WHERE vendor = ? AND model = ?",
        )
        .bind(vendor)
        .bind(model)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| (r.get(0), r.get(1))))
    }

    /// Insert or replace a model price.
    pub async fn set_price(&self, price: &ModelPrice) -> Result<(), GatewayError> {
        sqlx::query(
            "INSERT INTO model_prices (vendor, model, input_cents_per_mtok, output_cents_per_mtok) \
             VALUES (?, ?, ?, ?) ON CONFLICT(vendor, model) DO UPDATE SET \
             input_cents_per_mtok = excluded.input_cents_per_mtok, \
             output_cents_per_mtok = excluded.output_cents_per_mtok",
        )
        .bind(&price.vendor)
        .bind(&price.model)
        .bind(price.input_cents_per_mtok)
        .bind(price.output_cents_per_mtok)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn prices(&self) -> Result<Vec<ModelPrice>, GatewayError> {
        let rows = sqlx::query(
            "SELECT vendor, model, input_cents_per_mtok, output_cents_per_mtok FROM model_prices \
             ORDER BY vendor, model",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| ModelPrice {
                vendor: r.get("vendor"),
                model: r.get("model"),
                input_cents_per_mtok: r.get("input_cents_per_mtok"),
                output_cents_per_mtok: r.get("output_cents_per_mtok"),
            })
            .collect())
    }
}
