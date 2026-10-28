//! Shared state: database, configuration and the poller.

use std::sync::Arc;

use crate::config::Config;
use crate::db::Db;
use crate::error::PlatformError;
use crate::poller::Poller;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    config: Config,
    poller: Poller,
}

impl AppState {
    pub fn new(db: Db, config: Config) -> Result<Self, PlatformError> {
        let poller = Poller::new(db.clone(), config.clone())?;
        Ok(Self {
            inner: Arc::new(Inner { db, config, poller }),
        })
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn poller(&self) -> &Poller {
        &self.inner.poller
    }
}
