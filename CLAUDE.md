# CLAUDE.md — AI Coding Agent Guidelines (Standalone Recipe Node)

## 1. Project Overview
We are building an ultra-lightweight, AI-native Standalone Recipe Node (a modern, fast alternative to Mealie) with a built-in **MCP (Model Context Protocol) server** and federation readiness for a future Central Discovery & Commerce Aggregator.

**Core Technical Goals:**
- Compile into a **single static binary** (embedding the frontend via `rust-embed` and SQLite migrations).
- Idle memory footprint: **< 20 MB RAM**.
- 3-tier ingredient normalization (`raw_text`, `metric_amount`/`unit`, `canonical_id` + `is_specialty` flag).
- Built-in **MCP Server** supporting both `stdio` and `SSE` / Streamable HTTP transports.

---

## 2. Tech Stack (Rust Cargo Workspace)
- **Language & Runtime:** Rust (Edition 2024 / 1.85+), `tokio` (async runtime).
- **Web & HTTP/SSE:** `axum`, `tower-http`.
- **Database:** `sqlx` (`sqlite` driver, compile-time checked queries, `WAL` mode + `FTS5` full-text search).
- **AI & Structured Extraction:** `rig-core` 0.45 (a `CompletionRequest` with `output_schema(schema_for!(T))` against a `DynModel<Completion>` obtained from `Catalog::builtin().connect_with("vendor/model", ConnectOptions)`), `schemars`, `serde`. The model is chosen at runtime via `LLM_MODEL` (e.g. `minimax/MiniMax-M3`, `anthropic/claude-haiku-4-5`, `ollama/llama3.2`); credentials come from the vendor's usual variable (`MINIMAX_API_KEY`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`) or the generic `LLM_API_KEY` override. Settings live in `.env` (template: `.env.example`). Without `LLM_MODEL` the scraper falls back to deterministic heuristics and free-text import is disabled.
- **MCP (Model Context Protocol):** `rmcp` (or native JSON-RPC 2.0 router over `stdio` and `axum::response::Sse`).
- **HTML/Markdown Scraping:** `scraper` (`application/ld+json` extraction), `htmd` (HTML-to-Markdown sanitizer before LLM prompts), `reqwest`.
- **Image Processing:** `image` (on-the-fly cover image conversion to `WebP` during import), `zip` (for Mealie backup archive import).
- **IDs & Hashing:** `ulid` (recipe ID generation), `sha2` (deterministic `content_hash` calculation for deduplication).

---

## 3. Cargo Workspace Structure
Strictly adhere to the following modular Workspace layout:

```text
.
├── Cargo.toml                  # Workspace root
├── CLAUDE.md                   # This file
├── migrations/                 # SQLite migrations for sqlx (Tables + FTS5 triggers)
├── docs/
│   ├── ARCHITECTURE_AND_SCHEMAS.md
│   └── MCP_AND_API_SPEC.md
├── crates/
│   ├── recipe-core/            # Domain models, DTOs, JSON Schemas, content_hash logic
│   ├── recipe-db/              # sqlx SQLite pool, repositories, FTS5 search queries
│   ├── recipe-scraper/         # Schema.org parser + htmd + rig structured-output extractor + Mealie (.zip) importer
│   ├── recipe-mcp/             # MCP Tools + shared ImportService (rig agents consume the MCP tools via the rig-rmcp crate)
│   └── node-server/            # Binary: Axum HTTP/SSE server, CLI (stdio MCP mode), rust-embed static UI
└── frontend/                   # Lightweight UI (PWA, Cook Mode with WakeLock API)
```

---

## 4. Build, Lint & Test Commands
Always use these commands to verify your changes:
- **Check compilation across workspace:** `cargo check --workspace --all-targets`
- **Strict Linter:** `cargo clippy --workspace --all-targets -- -D warnings`
- **Run tests:** `cargo test --workspace`
- **Run node as HTTP server:** `cargo run -p node-server -- serve --port 8080`
- **Run node in local MCP (stdio) mode:** `cargo run -p node-server -- mcp-stdio`
- **Backfill source languages:** `cargo run -p node-server -- backfill-language` (also runs in the background at `serve` startup)
- **Toolchain:** rustc 1.97+ (rig-core 0.45 requires it). Use the Nix dev shell in `../flake.nix` (`nix develop`) or rustup via `rust-toolchain.toml`. Never build with both toolchains in the same `target/` directory: proc-macro libraries are not interchangeable. `scripts/devbuild.sh` uses `target/rustc-<version>-<hash>/` automatically; for plain `cargo` commands set `CARGO_TARGET_DIR` likewise when switching toolchains, or run `cargo clean` first.

---

## 5. Strict Coding Guardrails
1. **No `.unwrap()` or `.expect()` in production code:** Use `thiserror` for domain errors inside library crates (`recipe-core`, `recipe-db`, `recipe-scraper`, `recipe-mcp`) and `anyhow` at the top-level `node-server` binary.
2. **Token Economy in Scraper:** In `recipe-scraper`, ALWAYS attempt to parse `<script type="application/ld+json">` (`Schema.org/Recipe`) locally and for free first. Invoke the LLM via `rig-core` ONLY if structured metadata is missing, incomplete, or when normalizing raw ingredient strings into `canonical_id`.
3. **Never send raw HTML to the LLM:** Before passing web content to `rig`, strip `<script>`, `<style>`, `<nav>`, `<header>`, `<footer>`, `<svg>` tags and convert the DOM into clean Markdown via `htmd`.
4. **SQLite Pragmas on Pool Init:** When initializing `SqlitePool` in `recipe-db`, always enforce:
   - `PRAGMA journal_mode = WAL;`
   - `PRAGMA synchronous = NORMAL;`
   - `PRAGMA foreign_keys = ON;`
   - `PRAGMA busy_timeout = 5000;`
5. **Deterministic `content_hash`:** On every recipe creation or update, recompute `content_hash` (SHA-256 of sorted lowercase `canonical_id`s + quantities + normalized instruction text) so the future Central Aggregator can track forks and duplicates accurately.