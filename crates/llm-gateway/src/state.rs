//! Shared state: database, configuration, the upstream HTTP client and the
//! per-hub concurrency limits.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Semaphore;

use crate::config::Config;
use crate::db::Db;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    config: Config,
    http: reqwest::Client,
    limits: Mutex<HashMap<i64, Arc<Semaphore>>>,
}

impl AppState {
    pub fn new(db: Db, config: Config) -> Self {
        let http = reqwest::Client::builder()
            .timeout(config.upstream_timeout)
            .build()
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "upstream client without timeout");
                reqwest::Client::new()
            });
        Self {
            inner: Arc::new(Inner {
                db,
                config,
                http,
                limits: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    /// The semaphore bounding a hub's in-flight upstream calls.
    pub fn hub_permits(&self, hub_id: i64) -> Arc<Semaphore> {
        let mut limits = self
            .inner
            .limits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        limits
            .entry(hub_id)
            .or_insert_with(|| Arc::new(Semaphore::new(self.inner.config.per_hub_concurrency)))
            .clone()
    }
}
