#!/usr/bin/env bash
# Build the recipe node, run it, and open the web UI for manual testing.
#
#   scripts/devbuild.sh                 debug build, http://127.0.0.1:8080, data in ./data
#   scripts/devbuild.sh --release       optimized build (what the single-binary goal measures)
#   scripts/devbuild.sh --fresh         wipe the data dir first and seed examples/sample-recipe.json
#   scripts/devbuild.sh --port 9090     another port
#   scripts/devbuild.sh --data-dir /tmp/node-data
#   scripts/devbuild.sh --no-open       do not launch a browser (CI, Claude's browser pane)
#
# Settings and secrets are read from .env (copy .env.example). NODE_API_KEY enables /mcp,
# LLM_MODEL enables free-text import, RUST_LOG controls logging. Ctrl+C stops the server.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Load secrets and settings from .env (see .env.example). Variables already set in the
# shell win over the file.
if [[ -f "$ROOT/.env" ]]; then
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line%%#*}"
    [[ "$line" =~ ^[[:space:]]*([A-Za-z_][A-Za-z0-9_]*)[[:space:]]*=[[:space:]]*(.*)$ ]] || continue
    key="${BASH_REMATCH[1]}"; val="${BASH_REMATCH[2]}"
    [[ "$val" =~ ^(.*[^[:space:]])[[:space:]]*$ ]] && val="${BASH_REMATCH[1]}"
    [[ "$val" =~ ^\"(.*)\"$ ]] && val="${BASH_REMATCH[1]}"
    [[ "$val" =~ ^\'(.*)\'$ ]] && val="${BASH_REMATCH[1]}"
    [[ -z "$val" ]] && continue
    [[ -z "${!key:-}" ]] && export "$key=$val"
  done < "$ROOT/.env"
fi

PORT="${NODE_PORT:-8080}"
HOST="${NODE_HOST:-127.0.0.1}"
DATA_DIR="${NODE_DATA_DIR:-$ROOT/data}"
PROFILE=debug
FRESH=0
OPEN=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    --port)      PORT="$2"; shift 2 ;;
    --host)      HOST="$2"; shift 2 ;;
    --data-dir)  DATA_DIR="$2"; shift 2 ;;
    --release)   PROFILE=release; shift ;;
    --fresh)     FRESH=1; shift ;;
    --no-open)   OPEN=0; shift ;;
    -h|--help)   sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
  esac
done

URL="http://$HOST:$PORT"

log() { printf '\033[1;32m▶\033[0m %s\n' "$*"; }
die() { printf '\033[1;31m✖\033[0m %s\n' "$*" >&2; exit 1; }

# --- stop a previous dev server on the port, but never an unrelated program
holder=$(lsof -nP -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null | head -1 || true)
if [[ -n "$holder" ]]; then
  if ps -o command= -p "$holder" | grep -q "node-server"; then
    log "stopping previous node-server (pid $holder) on port $PORT"
    kill "$holder" 2>/dev/null || true
    for _ in $(seq 1 50); do lsof -nP -tiTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1 || break; sleep 0.1; done
  else
    die "port $PORT is in use by another program (pid $holder); pass --port <n>"
  fi
fi

# --- build
# Keep each toolchain's artifacts apart: the Nix and rustup compilers cannot load each
# other's proc-macro libraries, and sharing ./target between them breaks builds.
if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  tc=$(rustc -vV 2>/dev/null | awk '/^release:/{r=$2} /^commit-hash:/{h=substr($2,1,8)} END{print r "-" h}')
  export CARGO_TARGET_DIR="$ROOT/target/rustc-${tc:-unknown}"
fi
if [[ -n "${DEVELOPER_DIR:-}" && ! -d "$DEVELOPER_DIR" ]]; then
  die "DEVELOPER_DIR points to a missing SDK ($DEVELOPER_DIR). Your dev shell is stale: run 'exit' and 'nix develop' again."
fi
log "toolchain: $(rustc --version)  target dir: ${CARGO_TARGET_DIR#$ROOT/}"
if [[ "$PROFILE" == release ]]; then
  log "building node-server (release)"; cargo build --release -p node-server
  BIN="$CARGO_TARGET_DIR/release/node-server"
else
  log "building node-server (debug)"; cargo build -p node-server
  BIN="$CARGO_TARGET_DIR/debug/node-server"
fi

# --- data
if [[ "$FRESH" == 1 && -d "$DATA_DIR" ]]; then
  log "wiping $DATA_DIR"
  rm -rf "$DATA_DIR"
fi
mkdir -p "$DATA_DIR"

# --- run
log "starting $URL  (data: $DATA_DIR, profile: $PROFILE)"
[[ -n "${NODE_API_KEY:-}" ]] && log "MCP endpoint enabled at $URL/mcp"
[[ -n "${LLM_MODEL:-}" ]] && log "LLM extractor: $LLM_MODEL"
"$BIN" serve --host "$HOST" --port "$PORT" --data-dir "$DATA_DIR" &
SERVER_PID=$!
trap 'echo; log "stopping server"; kill $SERVER_PID 2>/dev/null || true; wait $SERVER_PID 2>/dev/null || true' EXIT INT TERM

# --- wait for health
for i in $(seq 1 100); do
  if curl -fs "$URL/api/v1/health" >/dev/null 2>&1; then break; fi
  kill -0 "$SERVER_PID" 2>/dev/null || die "server exited during startup"
  sleep 0.1
  [[ $i -eq 100 ]] && die "server did not become healthy at $URL"
done
log "healthy: $(curl -s "$URL/api/v1/health")"

# --- seed on a fresh database
if [[ "$FRESH" == 1 && -f examples/sample-recipe.json ]]; then
  title=$(curl -s -X POST "$URL/api/v1/recipes" -H 'content-type: application/json' \
    --data @examples/sample-recipe.json | sed -n 's/.*"title":"\([^"]*\)".*/\1/p')
  log "seeded sample recipe: ${title:-?}"
fi

# --- open browser
if [[ "$OPEN" == 1 ]]; then
  if command -v open >/dev/null; then open "$URL"
  elif command -v xdg-open >/dev/null; then xdg-open "$URL" >/dev/null 2>&1
  else log "open $URL in your browser"; fi
fi

log "Ctrl+C to stop"
wait "$SERVER_PID"
