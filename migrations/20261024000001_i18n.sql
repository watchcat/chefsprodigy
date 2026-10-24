-- Multiple languages: source language on recipes, cached translations, localized
-- canonical labels, and a second FTS index over translated text (docs/SPEC-i18n.md).

ALTER TABLE recipes ADD COLUMN language TEXT;

CREATE TABLE recipe_translations (
    recipe_id TEXT NOT NULL REFERENCES recipes(id) ON DELETE CASCADE,
    lang TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT,
    ingredients TEXT NOT NULL,   -- JSON array of raw_text, same order as recipe_ingredients
    instructions TEXT NOT NULL,  -- JSON array of step text, same order as recipe instructions
    tags TEXT NOT NULL DEFAULT '[]',
    origin TEXT NOT NULL CHECK (origin IN ('llm', 'human')),
    source_updated_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (recipe_id, lang)
);

CREATE TABLE canonical_terms (
    canonical_id TEXT NOT NULL,
    lang TEXT NOT NULL,
    label TEXT NOT NULL,
    origin TEXT NOT NULL CHECK (origin IN ('llm', 'human')),
    PRIMARY KEY (canonical_id, lang)
);

CREATE VIRTUAL TABLE translations_fts USING fts5(
    recipe_id UNINDEXED,
    lang UNINDEXED,
    title,
    description,
    ingredients_raw,
    tags
);

CREATE TRIGGER recipe_translations_ai AFTER INSERT ON recipe_translations BEGIN
    INSERT INTO translations_fts(recipe_id, lang, title, description, ingredients_raw, tags)
    VALUES (NEW.recipe_id, NEW.lang, NEW.title, COALESCE(NEW.description, ''), NEW.ingredients, NEW.tags);
END;

CREATE TRIGGER recipe_translations_au AFTER UPDATE ON recipe_translations BEGIN
    UPDATE translations_fts
    SET title = NEW.title,
        description = COALESCE(NEW.description, ''),
        ingredients_raw = NEW.ingredients,
        tags = NEW.tags
    WHERE recipe_id = NEW.recipe_id AND lang = NEW.lang;
END;

CREATE TRIGGER recipe_translations_ad AFTER DELETE ON recipe_translations BEGIN
    DELETE FROM translations_fts WHERE recipe_id = OLD.recipe_id AND lang = OLD.lang;
END;
