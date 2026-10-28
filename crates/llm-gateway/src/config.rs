//! Runtime configuration: vendors (base URL + key) and gateway settings, from the
//! environment in production and built directly in tests.

use std::time::Duration;

/// An upstream that speaks the Messages format.
#[derive(Debug, Clone)]
pub struct Vendor {
    /// Path prefix and config name: `minimax`, `anthropic`
    pub name: String,
    /// Base URL without the `/v1/messages` suffix and without a trailing slash
    pub base_url: String,
    /// The real vendor key; `None` means the vendor is not configured
    pub api_key: Option<String>,
}

impl Vendor {
    pub fn new(name: &str, base_url: &str, api_key: Option<&str>) -> Self {
        Self {
            name: name.to_string(),
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            api_key: api_key.map(str::to_string).filter(|k| !k.is_empty()),
        }
    }

    pub fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub vendors: Vec<Vendor>,
    /// Bearer token for `/admin/*`; `None` disables the admin API
    pub admin_token: Option<String>,
    /// Log prompt and reply bodies at debug level (`GATEWAY_LOG_CONTENT=1`)
    pub log_content: bool,
    pub upstream_timeout: Duration,
    /// Pause before the single retry on 5xx / 429
    pub retry_backoff: Duration,
    /// In-flight upstream calls allowed per hub
    pub per_hub_concurrency: usize,
    /// Encrypts hubs' own vendor keys (BYOK); `None` disables BYOK
    pub master_key: Option<String>,
    /// Lifetime of a cached reply
    pub cache_ttl: Duration,
    /// Entries kept before the least recently hit ones are evicted
    pub cache_max_entries: usize,
    /// Largest reply body the cache stores
    pub cache_max_body_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            vendors: Vec::new(),
            admin_token: None,
            log_content: false,
            upstream_timeout: Duration::from_secs(120),
            retry_backoff: Duration::from_secs(1),
            per_hub_concurrency: 2,
            master_key: None,
            cache_ttl: Duration::from_secs(30 * 24 * 3600),
            cache_max_entries: 10_000,
            cache_max_body_bytes: 1024 * 1024,
        }
    }
}

impl Config {
    /// Read `MINIMAX_API_KEY`, `ANTHROPIC_API_KEY`, the optional base-URL
    /// overrides (`MINIMAX_ANTHROPIC_API_BASE`, `ANTHROPIC_API_BASE`),
    /// `GATEWAY_ADMIN_TOKEN`, `GATEWAY_MASTER_KEY`, `GATEWAY_LOG_CONTENT`,
    /// `GATEWAY_UPSTREAM_TIMEOUT_SECS`, `GATEWAY_CACHE_TTL_DAYS` and
    /// `GATEWAY_CACHE_MAX_ENTRIES`.
    pub fn from_env() -> Self {
        let env = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let vendors = vec![
            Vendor::new(
                "minimax",
                &env("MINIMAX_ANTHROPIC_API_BASE")
                    .unwrap_or_else(|| "https://api.minimax.io/anthropic".to_string()),
                env("MINIMAX_API_KEY").as_deref(),
            ),
            Vendor::new(
                "anthropic",
                &env("ANTHROPIC_API_BASE")
                    .unwrap_or_else(|| "https://api.anthropic.com".to_string()),
                env("ANTHROPIC_API_KEY").as_deref(),
            ),
        ];
        let timeout = env("GATEWAY_UPSTREAM_TIMEOUT_SECS")
            .and_then(|s| s.parse().ok())
            .map(Duration::from_secs);
        let defaults = Self::default();
        Self {
            vendors,
            admin_token: env("GATEWAY_ADMIN_TOKEN"),
            master_key: env("GATEWAY_MASTER_KEY"),
            log_content: matches!(env("GATEWAY_LOG_CONTENT").as_deref(), Some("1" | "true")),
            upstream_timeout: timeout.unwrap_or(Duration::from_secs(120)),
            cache_ttl: env("GATEWAY_CACHE_TTL_DAYS")
                .and_then(|s| s.parse::<u64>().ok())
                .map(|days| Duration::from_secs(days.saturating_mul(24 * 3600)))
                .unwrap_or(defaults.cache_ttl),
            cache_max_entries: env("GATEWAY_CACHE_MAX_ENTRIES")
                .and_then(|s| s.parse().ok())
                .unwrap_or(defaults.cache_max_entries),
            ..defaults
        }
    }

    /// Vendor by path prefix. The generic `anthropic` prefix (what a hub's
    /// `MINIMAX_ANTHROPIC_API_BASE` ends in) routes MiniMax models to MiniMax and
    /// everything else to Anthropic, so one hub setting serves both.
    pub fn vendor_for(&self, prefix: &str, model: &str) -> Option<&Vendor> {
        let name = match prefix {
            "anthropic" if model.to_ascii_lowercase().starts_with("minimax") => "minimax",
            other => other,
        };
        self.vendors.iter().find(|v| v.name == name)
    }
}
