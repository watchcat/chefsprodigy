-- llm-gateway storage (docs/SPEC-hosted-platform.md section 5.2). Quota, cache and BYOK
-- columns exist from the start so v2 (RN-68) needs no schema change.
CREATE TABLE hubs (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    token_hash TEXT NOT NULL UNIQUE,        -- SHA-256 hex of the hub token; the token itself is never stored
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    created_at TEXT NOT NULL,
    onboarding_daily_requests INTEGER NOT NULL DEFAULT 30,
    onboarding_days INTEGER NOT NULL DEFAULT 3,
    daily_request_limit INTEGER NOT NULL DEFAULT 10,
    daily_token_limit INTEGER,
    monthly_request_limit INTEGER NOT NULL DEFAULT 150,
    monthly_cost_cap_cents INTEGER NOT NULL DEFAULT 100,
    byok_json_enc BLOB
);

CREATE TABLE usage_daily (
    hub_id INTEGER NOT NULL REFERENCES hubs(id) ON DELETE CASCADE,
    day TEXT NOT NULL,                      -- UTC calendar day, YYYY-MM-DD
    requests INTEGER NOT NULL DEFAULT 0,    -- successful upstream calls
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cost_cents REAL NOT NULL DEFAULT 0,     -- estimated from model_prices; 0 when no price is known
    PRIMARY KEY (hub_id, day)
);

CREATE TABLE cache (
    key_hash TEXT PRIMARY KEY,
    vendor TEXT NOT NULL,
    model TEXT NOT NULL,
    status INTEGER NOT NULL,
    body BLOB NOT NULL,
    created_at TEXT NOT NULL,
    last_hit TEXT NOT NULL,
    hits INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE model_prices (
    vendor TEXT NOT NULL,
    model TEXT NOT NULL,
    input_cents_per_mtok REAL NOT NULL,
    output_cents_per_mtok REAL NOT NULL,
    PRIMARY KEY (vendor, model)
);
