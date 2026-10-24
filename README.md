# Chef's Prodigy

A self-hosted, AI-native recipe node: one small Rust binary that stores your recipes in
SQLite, imports them from web pages, pasted text, photos or a Mealie backup, normalizes
every ingredient to metric units with a canonical id, serves a phone-friendly web app with
a Cook Mode, speaks MCP so your AI assistant can search and plan with your recipes, and
translates recipes on demand into the language you read.

## Features

- **Import from anywhere**: Schema.org JSON-LD pages (free, no model needed), unstructured
  text (social captions, chat messages), photos of recipe cards or cookbook pages with the
  finished dish as cover, and Mealie 1.x backups.
- **3-tier ingredients**: original wording, metric amount and unit, snake_case canonical id
  with a specialty flag, so shopping lists can be merged and split between the supermarket
  and the specialty store.
- **Web app (PWA)**: library with full-text search, tag and pantry filters, serving scaling,
  Cook Mode with per-step timers and wake lock, editor with cover upload, shopping list
  builder, UI in English, Russian and Dutch.
- **MCP server** over stdio and Streamable HTTP: `search_recipes`, `get_recipe_details`,
  `import_recipe`, `build_smart_shopping_list`.
- **Multilingual content**: each recipe keeps its source language; viewing it in another
  language translates it once with the configured model, caches the result, and lets you
  correct it by hand. Search finds recipes through their translations.
- **Federation ready**: deterministic `content_hash` per recipe and a public manifest for a
  future aggregator.
- **Bring your own model**: any provider rig-core knows (MiniMax, Anthropic, OpenAI,
  Ollama, ...). Without one, everything still works except free-text import, photo import
  and translation.

## Quick start

Requirements: Rust 1.97 or newer (a Nix dev shell is provided in the parent folder's
`flake.nix`; `rust-toolchain.toml` covers rustup users).

```sh
cp .env.example .env            # set LLM_MODEL and the vendor key to enable AI features
scripts/devbuild.sh             # builds, starts http://127.0.0.1:8080 and opens it
```

`scripts/devbuild.sh --fresh` wipes `./data` and seeds a sample recipe; `--release` builds
the optimized binary. For a plain run:

```sh
cargo run --release -p node-server -- serve --port 8080
```

Data (SQLite database and WebP covers) lives in `./data` by default (`--data-dir`).

### Configuration (`.env`)

| Variable | Purpose |
|---|---|
| `LLM_MODEL` | rig catalog reference, e.g. `minimax/MiniMax-M3`, `anthropic/claude-haiku-4-5`, `ollama/llama3.2` |
| `MINIMAX_API_KEY`, `ANTHROPIC_API_KEY`, ... | the vendor's key; `LLM_API_KEY` is a generic override |
| `NODE_API_KEY` | enables the bearer-protected `/mcp` endpoint |
| `NODE_PORT`, `NODE_HOST`, `NODE_DATA_DIR`, `NODE_CHEF_NAME`, `NODE_AUTHOR_DID` | server settings |

## Using it

- **Web UI**: `http://127.0.0.1:8080`. Pick a language in the header; recipes written in
  another language are translated when you open them.
- **REST API**: documented in [docs/MCP_AND_API_SPEC.md](docs/MCP_AND_API_SPEC.md). Example:

  ```sh
  curl -s -X POST localhost:8080/api/v1/import/url \
    -H 'content-type: application/json' \
    -d '{"url":"https://www.bbcgoodfood.com/recipes/classic-lasagne-0"}' | jq .title
  ```

- **MCP for Claude Desktop / Claude Code** (stdio):

  ```json
  { "mcpServers": { "recipes": { "command": "/path/to/node-server",
                                  "args": ["mcp-stdio", "--data-dir", "/path/to/data"] } } }
  ```

  Remote clients use `http://host:8080/mcp` with `Authorization: Bearer $NODE_API_KEY`.

## Development

```sh
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 scripts/i18n-check.py     # UI message bundle parity
python3 scripts/kanban.py serve   # live kanban board over docs/KANBAN.md
```

Workspace layout and coding rules: [CLAUDE.md](CLAUDE.md). Design documents live in
`docs/` (architecture and schemas, API and MCP spec, web UI, photo import, i18n). The
frontend is a dependency-free PWA in `frontend/dist`, embedded into the binary at build time.
Work is tracked on the kanban board in [docs/KANBAN.md](docs/KANBAN.md).

## License

MIT
