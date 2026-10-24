-- Initial schema for the Standalone Recipe Node.
-- NOTE: foreign_keys is a per-connection pragma and is set on pool init (see recipe-db).

CREATE TABLE recipes (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL,
    description TEXT,
    servings INTEGER NOT NULL DEFAULT 4,
    prep_time_minutes INTEGER,
    cook_time_minutes INTEGER,
    instructions TEXT NOT NULL, -- JSON array of InstructionStep
    tags TEXT NOT NULL DEFAULT '[]', -- JSON array of strings
    image_path TEXT,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    origin_url TEXT,
    author_did TEXT,
    content_hash TEXT NOT NULL,
    is_public BOOLEAN NOT NULL DEFAULT 0
);

CREATE INDEX idx_recipes_public ON recipes(is_public) WHERE is_public = 1;
CREATE INDEX idx_recipes_hash ON recipes(content_hash);
CREATE INDEX idx_recipes_author ON recipes(author_did);

CREATE TABLE recipe_ingredients (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    recipe_id TEXT NOT NULL,
    raw_text TEXT NOT NULL,
    metric_amount REAL,
    unit TEXT NOT NULL,
    canonical_id TEXT NOT NULL,
    is_specialty BOOLEAN NOT NULL DEFAULT 0,
    position INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (recipe_id) REFERENCES recipes(id) ON DELETE CASCADE
);

CREATE INDEX idx_recipe_ingredients_recipe_id ON recipe_ingredients(recipe_id);
CREATE INDEX idx_recipe_ingredients_canonical ON recipe_ingredients(canonical_id);

CREATE VIRTUAL TABLE recipes_fts USING fts5(
    recipe_id UNINDEXED,
    title,
    description,
    ingredients_raw,
    tags
);

CREATE TRIGGER recipes_ai AFTER INSERT ON recipes BEGIN
    INSERT INTO recipes_fts(recipe_id, title, description, ingredients_raw, tags)
    VALUES (NEW.id, NEW.title, COALESCE(NEW.description, ''), '', NEW.tags);
END;

CREATE TRIGGER recipes_au AFTER UPDATE ON recipes BEGIN
    UPDATE recipes_fts
    SET title = NEW.title,
        description = COALESCE(NEW.description, ''),
        tags = NEW.tags
    WHERE recipe_id = NEW.id;
END;

CREATE TRIGGER recipes_ad AFTER DELETE ON recipes BEGIN
    DELETE FROM recipes_fts WHERE recipe_id = OLD.id;
END;

-- SQLite does not allow "AFTER INSERT OR UPDATE OR DELETE" in a single trigger,
-- so the ingredient -> FTS sync is split into three triggers.
CREATE TRIGGER ingredients_fts_sync_ai AFTER INSERT ON recipe_ingredients BEGIN
    UPDATE recipes_fts
    SET ingredients_raw = (
        SELECT COALESCE(GROUP_CONCAT(raw_text || ' ' || canonical_id, ' '), '')
        FROM recipe_ingredients
        WHERE recipe_id = NEW.recipe_id
    )
    WHERE recipe_id = NEW.recipe_id;
END;

CREATE TRIGGER ingredients_fts_sync_au AFTER UPDATE ON recipe_ingredients BEGIN
    UPDATE recipes_fts
    SET ingredients_raw = (
        SELECT COALESCE(GROUP_CONCAT(raw_text || ' ' || canonical_id, ' '), '')
        FROM recipe_ingredients
        WHERE recipe_id = NEW.recipe_id
    )
    WHERE recipe_id = NEW.recipe_id;
END;

CREATE TRIGGER ingredients_fts_sync_ad AFTER DELETE ON recipe_ingredients BEGIN
    UPDATE recipes_fts
    SET ingredients_raw = (
        SELECT COALESCE(GROUP_CONCAT(raw_text || ' ' || canonical_id, ' '), '')
        FROM recipe_ingredients
        WHERE recipe_id = OLD.recipe_id
    )
    WHERE recipe_id = OLD.recipe_id;
END;
