//! Runtime configuration: the admin token and the poller's timing and limits, from
//! the environment in production and built directly in tests.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    /// Bearer token for `/admin/*`; `None` disables the admin API (503)
    pub admin_token: Option<String>,
    /// Pause between scheduled poll passes (spec: 10 minutes)
    pub poll_interval: Duration,
    /// Random extra delay added to every pause, so restarts do not synchronize
    pub poll_jitter: Duration,
    /// First retry delay after a failed poll of a hub; doubles per consecutive failure
    pub backoff_base: Duration,
    /// Upper bound of the per-hub backoff
    pub backoff_max: Duration,
    /// Whole-request timeout for manifest and recipe fetches
    pub http_timeout: Duration,
    /// TCP connect timeout for hub fetches
    pub connect_timeout: Duration,
    /// Largest manifest body accepted from a hub
    pub max_manifest_bytes: usize,
    /// Largest federated recipe body (recipe plus translations) accepted from a hub
    pub max_recipe_bytes: usize,
}

const DEFAULT_INTERVAL: Duration = Duration::from_secs(600);

impl Default for Config {
    fn default() -> Self {
        Self {
            admin_token: None,
            poll_interval: DEFAULT_INTERVAL,
            poll_jitter: Duration::from_secs(60),
            backoff_base: DEFAULT_INTERVAL,
            backoff_max: Duration::from_secs(3600),
            http_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(3),
            max_manifest_bytes: 8 * 1024 * 1024,
            max_recipe_bytes: 2 * 1024 * 1024,
        }
    }
}

impl Config {
    /// Read `PLATFORM_ADMIN_TOKEN`, `PLATFORM_POLL_INTERVAL_SECS`,
    /// `PLATFORM_POLL_JITTER_SECS`, `PLATFORM_BACKOFF_MAX_SECS` and
    /// `PLATFORM_HTTP_TIMEOUT_SECS`. The backoff starts at the poll interval.
    pub fn from_env() -> Self {
        let env = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let secs = |key: &str| {
            env(key)
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs)
        };
        let defaults = Self::default();
        let poll_interval = secs("PLATFORM_POLL_INTERVAL_SECS")
            .filter(|d| !d.is_zero())
            .unwrap_or(defaults.poll_interval);
        Self {
            admin_token: env("PLATFORM_ADMIN_TOKEN"),
            poll_interval,
            poll_jitter: secs("PLATFORM_POLL_JITTER_SECS").unwrap_or(defaults.poll_jitter),
            backoff_base: poll_interval,
            backoff_max: secs("PLATFORM_BACKOFF_MAX_SECS")
                .unwrap_or(defaults.backoff_max)
                .max(poll_interval),
            http_timeout: secs("PLATFORM_HTTP_TIMEOUT_SECS")
                .filter(|d| !d.is_zero())
                .unwrap_or(defaults.http_timeout),
            ..defaults
        }
    }

    /// Delay before the next poll of a hub after `failures` consecutive failures
    /// (1 → base, 2 → 2 × base, …, capped at `backoff_max`).
    pub fn backoff(&self, failures: u32) -> Duration {
        let exponent = failures.saturating_sub(1).min(20);
        self.backoff_base
            .saturating_mul(1u32 << exponent)
            .min(self.backoff_max)
    }
}
