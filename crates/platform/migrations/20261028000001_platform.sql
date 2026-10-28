-- platform storage (docs/SPEC-hosted-platform.md section 5.3): the hub registry and the
-- federation index. Only what hubs publish is stored: public recipes and their translations.

CREATE TABLE hubs (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,              -- subdomain label
    chef_name TEXT,                         -- operator's value; filled from the manifest when NULL
    internal_url TEXT NOT NULL,             -- http://hub-<name>.chefs-hubs.svc:8080 in the cluster
    public_url TEXT NOT NULL,               -- https://<name>.chefslegacy.com
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    created_at TEXT NOT NULL,
    last_polled_at TEXT,
    last_ok_at TEXT,
    last_error TEXT,
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    next_poll_at TEXT                       -- backoff: the scheduled loop skips the hub until then
);

-- One row per (hub, recipe) in the recipe's source language.
CREATE TABLE recipes_index (
    hub_id INTEGER NOT NULL REFERENCES hubs(id) ON DELETE CASCADE,
    recipe_id TEXT NOT NULL,
    title TEXT NOT NULL,
    tags_json TEXT NOT NULL DEFAULT '[]',
    canonical_json TEXT NOT NULL DEFAULT '[]',   -- sorted, distinct canonical ingredient ids
    language TEXT,                               -- source language, NULL when the hub does not know it
    content_hash TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    image_url TEXT,                              -- public_url + image_path
    fts_rowid INTEGER NOT NULL,                  -- this row's entry in recipes_fts
    indexed_at TEXT NOT NULL,
    PRIMARY KEY (hub_id, recipe_id)
);
CREATE INDEX recipes_index_content_hash ON recipes_index(content_hash);

-- One row per federated translation of an indexed recipe.
CREATE TABLE translations_index (
    hub_id INTEGER NOT NULL,
    recipe_id TEXT NOT NULL,
    lang TEXT NOT NULL,
    title TEXT NOT NULL,
    tags_json TEXT NOT NULL DEFAULT '[]',
    origin TEXT NOT NULL,                   -- llm | human
    stale INTEGER NOT NULL DEFAULT 0,       -- the recipe changed after the translation was made
    updated_at TEXT NOT NULL,               -- the translation's own timestamp (change detection)
    fts_rowid INTEGER NOT NULL,
    PRIMARY KEY (hub_id, recipe_id, lang),
    FOREIGN KEY (hub_id, recipe_id) REFERENCES recipes_index(hub_id, recipe_id) ON DELETE CASCADE
);

-- One row per (hub, recipe, lang): the source row plus one per federated translation, so
-- translated titles and tags are searchable. Maintained by the poller in the same
-- transaction as the two tables above (their fts_rowid columns point here).
CREATE VIRTUAL TABLE recipes_fts USING fts5(
    hub_id UNINDEXED,
    recipe_id UNINDEXED,
    lang UNINDEXED,
    title,
    tags,
    canonical,
    tokenize = 'unicode61 remove_diacritics 2'
);

-- Forks: recipes on several hubs (or twice on one) that share a content hash. Search
-- shows each group as one dish with the other copies under `also_at`.
CREATE VIEW forks AS
SELECT content_hash, COUNT(*) AS copies, COUNT(DISTINCT hub_id) AS hubs
FROM recipes_index
GROUP BY content_hash
HAVING COUNT(*) > 1;
