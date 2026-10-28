//! The federation poller (spec section 4.4): every `poll_interval` (+ jitter) it
//! fetches each due hub's manifest, fetches only the recipes whose
//! `(updated_at, content_hash)` or translation `(lang, updated_at)` pairs changed,
//! and removes what a manifest no longer lists. Failures back a hub off
//! exponentially. [`Poller::poll`] runs one pass and is what tests, the admin API
//! and `platform poll` call.
//!
//! Hub addresses come from the operator (hubctl), not from users; the client still
//! refuses redirects to another origin, caps response sizes and keeps timeouts short.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use rand::Rng;
use recipe_core::{FederatedRecipe, FederationManifest};
use reqwest::StatusCode;
use reqwest::header::ACCEPT;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::{Mutex, watch};

use crate::config::Config;
use crate::db::{self, Db, Hub};
use crate::error::PlatformError;
use crate::index::{IndexDoc, valid_recipe_id};

const MAX_REDIRECTS: usize = 3;
const MAX_ERROR_CHARS: usize = 500;

/// Which hubs a pass visits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollMode {
    /// Active hubs whose backoff has expired (the scheduled loop)
    Due,
    /// Every active hub, backoff ignored (operator: admin API, `platform poll`)
    Force,
}

/// The outcome of polling one hub.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct PollReport {
    pub hub: String,
    pub ok: bool,
    /// Recipes fetched and re-indexed (new or changed)
    pub fetched: usize,
    /// Recipes whose manifest entry matched the index
    pub unchanged: usize,
    /// Recipes dropped because the manifest no longer lists them (or they vanished)
    pub removed: usize,
    pub error: Option<String>,
}

/// A hub-side problem: recorded as `last_error` and backed off.
#[derive(Debug, thiserror::Error)]
enum FetchError {
    #[error("{0}")]
    Http(String),
    #[error("{url} answered HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("{url}: response too large (over {max} bytes)")]
    TooLarge { url: String, max: usize },
    #[error("{url}: unexpected body: {error}")]
    Decode { url: String, error: String },
}

enum SyncError {
    Hub(String),
    Internal(PlatformError),
}

impl From<PlatformError> for SyncError {
    fn from(e: PlatformError) -> Self {
        Self::Internal(e)
    }
}

impl From<FetchError> for SyncError {
    fn from(e: FetchError) -> Self {
        Self::Hub(e.to_string())
    }
}

#[derive(Clone)]
pub struct Poller {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    config: Config,
    http: reqwest::Client,
    /// One pass at a time per process.
    pass: Mutex<()>,
}

/// Follow redirects only within the hub's own origin, and only a few.
fn same_origin_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let same_origin = attempt
            .previous()
            .first()
            .is_some_and(|first| first.origin() == attempt.url().origin());
        if !same_origin {
            attempt.error("redirect to another origin refused")
        } else if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else {
            attempt.follow()
        }
    })
}

/// The error and its sources on one line ("… connection refused").
fn describe(e: &(dyn std::error::Error + 'static)) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        let part = s.to_string();
        if !text.contains(&part) {
            text.push_str(": ");
            text.push_str(&part);
        }
        source = s.source();
    }
    text
}

async fn read_capped(
    mut resp: reqwest::Response,
    url: &str,
    max: usize,
) -> Result<Vec<u8>, FetchError> {
    let too_large = || FetchError::TooLarge {
        url: url.to_string(),
        max,
    };
    if resp.content_length().is_some_and(|len| len > max as u64) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| FetchError::Http(describe(&e)))?
    {
        if body.len() + chunk.len() > max {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

impl Poller {
    pub fn new(db: Db, config: Config) -> Result<Self, PlatformError> {
        let http = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .timeout(config.http_timeout)
            .redirect(same_origin_redirects())
            .user_agent(concat!("chefsprodigy-platform/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| PlatformError::Client(describe(&e)))?;
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                config,
                http,
                pass: Mutex::new(()),
            }),
        })
    }

    /// One poll pass over the hubs selected by `mode`, one report per hub.
    pub async fn poll(&self, mode: PollMode) -> Result<Vec<PollReport>, PlatformError> {
        let _pass = self.inner.pass.lock().await;
        let hubs = match mode {
            PollMode::Due => self.inner.db.due_hubs(&db::now()).await?,
            PollMode::Force => self.inner.db.active_hubs().await?,
        };
        let mut reports = Vec::with_capacity(hubs.len());
        for hub in &hubs {
            // A database error on one hub (say it was removed mid-pass) must not starve
            // the others; it is reported but not counted against the hub's backoff.
            let report = self.poll_hub(hub).await.unwrap_or_else(|e| {
                tracing::error!(hub = %hub.name, error = %e, "indexing failed");
                PollReport {
                    hub: hub.name.clone(),
                    error: Some(format!("platform error: {e}")),
                    ..PollReport::default()
                }
            });
            reports.push(report);
        }
        Ok(reports)
    }

    /// Poll one registered, active hub now (backoff ignored).
    pub async fn poll_named(&self, name: &str) -> Result<PollReport, PlatformError> {
        let hub = self
            .inner
            .db
            .get_hub(name)
            .await?
            .ok_or_else(|| PlatformError::NotFound(format!("hub {name} is not registered")))?;
        if hub.status != "active" {
            return Err(PlatformError::BadRequest(format!(
                "hub {name} is {}",
                hub.status
            )));
        }
        let _pass = self.inner.pass.lock().await;
        self.poll_hub(&hub).await
    }

    /// The scheduled loop: a first pass after the jitter, then one every
    /// `poll_interval` + jitter, until `shutdown` turns true (or its sender is dropped).
    /// A pass interrupted by shutdown leaves the index consistent: every recipe is
    /// written in its own transaction.
    pub async fn run(self, mut shutdown: watch::Receiver<bool>) {
        let mut pause = self.jitter();
        loop {
            if *shutdown.borrow() {
                break;
            }
            tokio::select! {
                () = tokio::time::sleep(pause) => {}
                _ = shutdown.changed() => break,
            }
            tokio::select! {
                result = self.poll(PollMode::Due) => match result {
                    Ok(reports) => log_pass(&reports),
                    Err(e) => tracing::error!(error = %e, "poll pass failed"),
                },
                _ = shutdown.changed() => break,
            }
            pause = self.inner.config.poll_interval + self.jitter();
        }
        tracing::info!("poller stopped");
    }

    fn jitter(&self) -> Duration {
        let max = u64::try_from(self.inner.config.poll_jitter.as_millis()).unwrap_or(u64::MAX);
        if max == 0 {
            return Duration::ZERO;
        }
        Duration::from_millis(rand::rng().random_range(0..=max))
    }

    async fn poll_hub(&self, hub: &Hub) -> Result<PollReport, PlatformError> {
        let started = chrono::Utc::now();
        let at = db::timestamp(started);
        let mut report = PollReport {
            hub: hub.name.clone(),
            ..PollReport::default()
        };
        match self.sync(hub, &mut report).await {
            Ok(()) => {
                self.inner.db.record_poll_ok(hub.id, &at).await?;
                report.ok = true;
            }
            Err(SyncError::Hub(error)) => {
                let error: String = error.chars().take(MAX_ERROR_CHARS).collect();
                let failures = hub.consecutive_failures.saturating_add(1);
                let delay = self
                    .inner
                    .config
                    .backoff(u32::try_from(failures).unwrap_or(u32::MAX));
                let next = started
                    + chrono::Duration::from_std(delay).unwrap_or(chrono::Duration::hours(1));
                self.inner
                    .db
                    .record_poll_failure(hub.id, &at, &error, failures, &db::timestamp(next))
                    .await?;
                tracing::warn!(hub = %hub.name, failures, retry_in_secs = delay.as_secs(), error = %error, "hub poll failed");
                report.error = Some(error);
            }
            Err(SyncError::Internal(e)) => return Err(e),
        }
        Ok(report)
    }

    /// Bring the index for `hub` in line with its manifest.
    async fn sync(&self, hub: &Hub, report: &mut PollReport) -> Result<(), SyncError> {
        let db = &self.inner.db;
        let base = hub.internal_url.trim_end_matches('/');
        let manifest_url = format!("{base}/api/v1/federation/manifest");
        let manifest: FederationManifest = self
            .fetch_json(&manifest_url, self.inner.config.max_manifest_bytes)
            .await?
            .ok_or_else(|| FetchError::Status {
                url: manifest_url.clone(),
                status: StatusCode::NOT_FOUND.as_u16(),
            })?;
        db.fill_chef_name(hub.id, &manifest.chef_name).await?;

        let indexed = db.index_state(hub.id).await?;
        let mut listed: HashSet<&str> = HashSet::new();
        let mut problems: Vec<String> = Vec::new();
        for entry in &manifest.recipes {
            if !valid_recipe_id(&entry.id) {
                tracing::warn!(hub = %hub.name, id = %entry.id, "skipping a manifest entry with an invalid id");
                continue;
            }
            if !listed.insert(entry.id.as_str()) {
                continue;
            }
            if indexed.get(&entry.id).is_some_and(|ix| ix.matches(entry)) {
                report.unchanged += 1;
                continue;
            }
            let url = format!("{base}/api/v1/federation/recipes/{}", entry.id);
            match self
                .fetch_json::<FederatedRecipe>(&url, self.inner.config.max_recipe_bytes)
                .await
            {
                Ok(Some(fed)) if fed.recipe.id != entry.id => {
                    problems.push(format!("{url} answered recipe {:?}", fed.recipe.id));
                }
                Ok(Some(fed)) if fed.recipe.is_public => {
                    db.index_recipe(hub.id, &IndexDoc::new(&hub.public_url, &fed))
                        .await?;
                    report.fetched += 1;
                }
                // Unpublished or deleted after the manifest was served: drop it below.
                Ok(_) => {
                    listed.remove(entry.id.as_str());
                }
                Err(e) => problems.push(e.to_string()),
            }
        }
        for id in indexed.keys() {
            if !listed.contains(id.as_str()) {
                db.remove_recipe(hub.id, id).await?;
                report.removed += 1;
            }
        }
        match problems.as_slice() {
            [] => Ok(()),
            [one] => Err(SyncError::Hub(one.clone())),
            [first, ..] => Err(SyncError::Hub(format!(
                "{} recipes could not be fetched; first: {first}",
                problems.len()
            ))),
        }
    }

    /// GET `url` as JSON; `None` on 404.
    async fn fetch_json<T: DeserializeOwned>(
        &self,
        url: &str,
        max: usize,
    ) -> Result<Option<T>, FetchError> {
        let resp = self
            .inner
            .http
            .get(url)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| FetchError::Http(describe(&e)))?;
        let status = resp.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(FetchError::Status {
                url: url.to_string(),
                status: status.as_u16(),
            });
        }
        let body = read_capped(resp, url, max).await?;
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|e| FetchError::Decode {
                url: url.to_string(),
                error: e.to_string(),
            })
    }
}

fn log_pass(reports: &[PollReport]) {
    let fetched: usize = reports.iter().map(|r| r.fetched).sum();
    let removed: usize = reports.iter().map(|r| r.removed).sum();
    let failed = reports.iter().filter(|r| !r.ok).count();
    if fetched + removed + failed > 0 {
        tracing::info!(
            hubs = reports.len(),
            fetched,
            removed,
            failed,
            "poll pass done"
        );
    } else {
        tracing::debug!(hubs = reports.len(), "poll pass done, nothing changed");
    }
}
