//! Shared state: database, configuration, the upstream HTTP client and the per-hub
//! gates (concurrency limit plus quota admissions).

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{Semaphore, SemaphorePermit};

use crate::config::Config;
use crate::db::Db;
use crate::error::GatewayError;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    config: Config,
    http: reqwest::Client,
    gates: Mutex<HashMap<i64, Arc<HubGate>>>,
}

/// One hub's admission control: at most `per_hub_concurrency` upstream calls in flight,
/// and a quota check that counts requests admitted but not yet recorded in `usage_daily`.
pub struct HubGate {
    permits: Semaphore,
    /// Serializes "check the quota and admit" against "record usage and release", so a
    /// request is always counted exactly once: as pending or in `usage_daily`.
    quota_lock: tokio::sync::Mutex<()>,
    pending: AtomicI64,
}

/// A request that passed the quota check; it stops counting as pending when dropped.
pub struct Admission {
    gate: Arc<HubGate>,
}

impl Drop for Admission {
    fn drop(&mut self) {
        self.gate.pending.fetch_sub(1, Ordering::SeqCst);
    }
}

impl HubGate {
    fn new(concurrency: usize) -> Self {
        Self {
            permits: Semaphore::new(concurrency),
            quota_lock: tokio::sync::Mutex::new(()),
            pending: AtomicI64::new(0),
        }
    }

    /// Wait for one of the hub's upstream slots.
    pub async fn acquire(&self) -> Result<SemaphorePermit<'_>, GatewayError> {
        self.permits
            .acquire()
            .await
            .map_err(|_| GatewayError::Unavailable("gateway is shutting down".to_string()))
    }

    /// Run `check` (given the pending count) under the quota lock and admit the request
    /// when it passes.
    pub async fn admit<E, F, Fut>(self: &Arc<Self>, check: F) -> Result<Admission, E>
    where
        F: FnOnce(i64) -> Fut,
        Fut: Future<Output = Result<(), E>>,
    {
        let _lock = self.quota_lock.lock().await;
        check(self.pending.load(Ordering::SeqCst)).await?;
        self.pending.fetch_add(1, Ordering::SeqCst);
        Ok(Admission { gate: self.clone() })
    }

    /// Run `record` (writing usage) and release `admission` under the quota lock, so no
    /// concurrent check sees the request both pending and recorded.
    pub async fn settle<T>(
        &self,
        admission: Option<Admission>,
        record: impl Future<Output = T>,
    ) -> T {
        let _lock = if admission.is_some() {
            Some(self.quota_lock.lock().await)
        } else {
            None
        };
        let out = record.await;
        drop(admission);
        out
    }
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
                gates: Mutex::new(HashMap::new()),
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

    /// The gate for a hub's upstream calls and quota admissions.
    pub fn hub_gate(&self, hub_id: i64) -> Arc<HubGate> {
        let mut gates = self
            .inner
            .gates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        gates
            .entry(hub_id)
            .or_insert_with(|| Arc::new(HubGate::new(self.inner.config.per_hub_concurrency)))
            .clone()
    }
}
