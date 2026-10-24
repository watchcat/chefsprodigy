#!/usr/bin/env python3
"""Kanban board tooling for docs/KANBAN.md (standard library only).

  python3 scripts/kanban.py serve [--port 8765]   live board with drag-and-drop; moves are written back to KANBAN.md
                                                 (replaces a board already on that port; --reuse opens it instead)
  python3 scripts/kanban.py render [--open]       static read-only export to docs/kanban.html

The Markdown file stays the source of truth. The server binds to 127.0.0.1 and
only ever rewrites docs/KANBAN.md, preserving each story block verbatim.
"""
from __future__ import annotations

import argparse
import html
import json
import os
import re
import sys
import signal
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import webbrowser
from datetime import date
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "docs" / "KANBAN.md"
OUT = ROOT / "docs" / "kanban.html"

STORY_RE = re.compile(r"^### (RN-\d+)\s*·\s*(.+?)(?:\s*\(([A-Z](?:/[A-Z])?)\))?\s*$")
SECTION_RE = re.compile(r"^## (.+?)\s*$")
LOCK = threading.Lock()


# ---------------------------------------------------------------- parsing

class Story:
    def __init__(self, header: str) -> None:
        m = STORY_RE.match(header)
        if not m:
            raise ValueError(f"bad story header: {header!r}")
        self.id, self.title, self.size = m.group(1), m.group(2), m.group(3) or ""
        self.lines: list[str] = [header]  # raw block, header included

    @property
    def body(self) -> list[str]:
        return [l.strip() for l in self.lines[1:] if l.strip() and not l.strip().lower().startswith("- acceptance:")]

    @property
    def acceptance(self) -> list[str]:
        return [l.strip().split(":", 1)[1].strip() for l in self.lines[1:] if l.strip().lower().startswith("- acceptance:")]

    def to_json(self) -> dict:
        return {"id": self.id, "title": self.title, "size": self.size,
                "body": [b[2:] if b.startswith("- ") else b for b in self.body], "acceptance": self.acceptance}


class Board:
    def __init__(self, preamble: list[str], columns: list[tuple[str, list[Story]]]) -> None:
        self.preamble, self.columns = preamble, columns

    @classmethod
    def parse(cls, text: str) -> "Board":
        preamble: list[str] = []
        columns: list[tuple[str, list[Story]]] = []
        story: Story | None = None
        for raw in text.splitlines():
            line = raw.rstrip("\n")
            if m := SECTION_RE.match(line):
                columns.append((m.group(1), []))
                story = None
                continue
            if not columns:
                preamble.append(line)
                continue
            if STORY_RE.match(line):
                story = Story(line)
                columns[-1][1].append(story)
                continue
            if line.strip() in ("---", "_(empty)_"):
                story = None  # section separator / placeholder, re-emitted by dump()
                continue
            if story is not None:
                story.lines.append(line)
        return cls(preamble, columns)

    def find(self, story_id: str) -> tuple[int, int]:
        for ci, (_, stories) in enumerate(self.columns):
            for si, s in enumerate(stories):
                if s.id == story_id:
                    return ci, si
        raise KeyError(story_id)

    def move(self, story_id: str, to_column: str, index: int) -> None:
        ci, si = self.find(story_id)
        names = [n for n, _ in self.columns]
        if to_column not in names:
            raise KeyError(to_column)
        story = self.columns[ci][1].pop(si)
        target = self.columns[names.index(to_column)][1]
        target.insert(max(0, min(index, len(target))), story)

    def to_json(self) -> dict:
        return {"columns": [{"name": n, "stories": [s.to_json() for s in st]} for n, st in self.columns]}

    def dump(self) -> str:
        out: list[str] = list(self.preamble)
        # Drop trailing blanks and rules from the preamble; sections add their own.
        while out and (not out[-1].strip() or out[-1].strip() == "---"):
            out.pop()
        for name, stories in self.columns:
            out += ["", "---", "", f"## {name}", ""]
            if not stories:
                out.append("_(empty)_")
                continue
            for s in stories:
                block = list(s.lines)
                while block and not block[-1].strip():
                    block.pop()
                out += block + [""]
            if out and out[-1] == "":
                out.pop()
        return "\n".join(out).rstrip("\n") + "\n"


def load() -> Board:
    return Board.parse(SRC.read_text(encoding="utf-8"))


def save(board: Board) -> None:
    text = board.dump()
    fd, tmp = tempfile.mkstemp(dir=SRC.parent, prefix=".kanban-", suffix=".md")
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        f.write(text)
    os.replace(tmp, SRC)


# -------------------------------------------------------------- rendering

TEMPLATE = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Recipe Node Board</title>
<style>
:root{--bg:#f4f2ec;--panel:#ffffff;--ink:#1f2a26;--muted:#66736e;--line:#e2ded4;--accent:#173f36;--chip:#eef3f0;
 --backlog:#9a7b2f;--in-progress:#2b6cb0;--review:#8a4fb5;--done:#2f855a;--code:#f0ede5;--ok:#2f855a;--err:#c53030}
@media (prefers-color-scheme:dark){:root{--bg:#141817;--panel:#1d2321;--ink:#e8ebe9;--muted:#9aa7a1;--line:#2d3633;--accent:#8fd1bd;--chip:#26302d;--code:#262d2a}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--ink);font:15px/1.45 -apple-system,BlinkMacSystemFont,"Segoe UI",Inter,Roboto,sans-serif}
header.top{display:flex;flex-wrap:wrap;gap:12px 20px;align-items:baseline;padding:20px 24px 8px}
header.top h1{margin:0;font-size:22px;letter-spacing:-.01em}
header.top .meta{color:var(--muted);font-size:13px}
header.top input{margin-left:auto;min-width:220px;padding:8px 12px;border:1px solid var(--line);border-radius:8px;background:var(--panel);color:var(--ink);font:inherit}
.board{display:grid;grid-template-columns:repeat(4,minmax(260px,1fr));gap:16px;padding:12px 24px 32px}
@media (max-width:1100px){.board{grid-template-columns:repeat(2,minmax(260px,1fr))}}
@media (max-width:640px){.board{grid-template-columns:1fr;padding:12px 16px 24px}header.top{padding:16px}}
.col{background:color-mix(in srgb,var(--panel) 70%,var(--bg));border:1px solid var(--line);border-radius:12px;padding:12px;min-height:200px;transition:background .15s}
.col.over{background:color-mix(in srgb,var(--c) 10%,var(--bg));border-color:var(--c)}
.col h2{margin:0 0 10px;font-size:13px;text-transform:uppercase;letter-spacing:.08em;display:flex;align-items:center;gap:8px;padding-left:10px;border-left:3px solid var(--c)}
.col-backlog{--c:var(--backlog)}.col-in-progress{--c:var(--in-progress)}.col-review{--c:var(--review)}.col-done{--c:var(--done)}
.count{background:var(--chip);color:var(--muted);border-radius:999px;padding:1px 8px;font-size:12px;font-weight:600}
.cards{display:flex;flex-direction:column;gap:10px;min-height:60px}
.card{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:12px 14px;box-shadow:0 1px 2px rgba(0,0,0,.04)}
.card[draggable=true]{cursor:grab}.card.dragging{opacity:.4}.card.drop-before{box-shadow:0 -3px 0 0 var(--c)}.card.drop-after{box-shadow:0 3px 0 0 var(--c)}
.card header{display:flex;justify-content:space-between;align-items:center;margin-bottom:4px}
.card .id{font:600 12px ui-monospace,SFMono-Regular,Menlo,monospace;color:var(--c)}
.size{font-size:11px;font-weight:700;border-radius:6px;padding:2px 7px;background:var(--chip);color:var(--muted)}
.size-L{background:#fde8e8;color:#9b2c2c}.size-M{background:#fdf3d9;color:#8a5a00}.size-S{background:#e3f6ea;color:#276749}
@media (prefers-color-scheme:dark){.size-L{background:#4a2323;color:#f8b4b4}.size-M{background:#473a14;color:#f6d47a}.size-S{background:#1f3d2b;color:#9ae6b4}}
.card h3{margin:0 0 6px;font-size:15px;line-height:1.3}
.card p{margin:0 0 6px;color:var(--muted);font-size:13.5px}
.col-done .card p{display:none}.col-done .card{padding:9px 12px}.col-done .card h3{font-size:14px;margin:0}
details{margin-top:6px;font-size:13px}summary{cursor:pointer;color:var(--accent);font-weight:600}
details ul{margin:6px 0 0;padding-left:18px;color:var(--muted)}
code{font:12.5px ui-monospace,SFMono-Regular,Menlo,monospace;background:var(--code);padding:1px 5px;border-radius:4px}
.empty{color:var(--muted);font-style:italic;font-size:13px;margin:4px 0}
.card.hidden{display:none}
#toast{position:fixed;left:50%;bottom:20px;transform:translateX(-50%);background:var(--ink);color:var(--bg);padding:8px 14px;border-radius:8px;font-size:13px;opacity:0;transition:opacity .2s;pointer-events:none}
#toast.show{opacity:1}#toast.err{background:var(--err);color:#fff}
.badge{font-size:11px;font-weight:700;letter-spacing:.06em;text-transform:uppercase;border-radius:6px;padding:2px 7px;background:var(--chip);color:var(--muted)}
.badge.live{background:#e3f6ea;color:#276749}
</style>
</head>
<body>
<header class="top">
  <h1>Recipe Node Board</h1>
  <span class="badge" id="mode"></span>
  <span class="meta" id="meta"></span>
  <input type="search" id="q" placeholder="Filter stories…" aria-label="Filter stories">
</header>
<main class="board" id="board"></main>
<div id="toast" role="status"></div>
<script>
const LIVE = __LIVE__;
let board = __BOARD__;
const GENERATED = "__GENERATED__";
const esc = s => s.replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const inline = s => s.split('`').map((p,i) => i%2 ? `<code>${esc(p)}</code>` : esc(p)).join('');
const key = n => n.toLowerCase().replace(/ /g,'-');
const toastEl = document.getElementById('toast');
let toastTimer;
function toast(msg, err){ toastEl.textContent = msg; toastEl.className = 'show' + (err ? ' err' : ''); clearTimeout(toastTimer); toastTimer = setTimeout(() => toastEl.className = '', err ? 4000 : 1500); }

function render(){
  const total = board.columns.reduce((n,c) => n + c.stories.length, 0);
  document.getElementById('mode').textContent = LIVE ? 'live · drag to move' : 'static export';
  document.getElementById('mode').className = 'badge' + (LIVE ? ' live' : '');
  document.getElementById('meta').textContent = `${total} stories · ${LIVE ? 'docs/KANBAN.md' : 'generated ' + GENERATED + ' from docs/KANBAN.md'}`;
  document.getElementById('board').innerHTML = board.columns.map(col => `
    <section class="col col-${key(col.name)}" data-col="${esc(col.name)}" aria-label="${esc(col.name)}">
      <h2>${esc(col.name)} <span class="count">${col.stories.length}</span></h2>
      <div class="cards">${col.stories.map(s => `
        <article class="card" data-id="${s.id}" ${LIVE ? 'draggable="true"' : ''} data-search="${esc([s.id, s.title, ...s.body, ...s.acceptance].join(' ').toLowerCase())}">
          <header><span class="id">${s.id}</span>${s.size ? `<span class="size size-${s.size.split('/')[0]}">${esc(s.size)}</span>` : ''}</header>
          <h3>${inline(s.title)}</h3>
          ${s.body.map(b => `<p>${inline(b)}</p>`).join('')}
          ${s.acceptance.length ? `<details><summary>Acceptance</summary><ul>${s.acceptance.map(a => `<li>${inline(a)}</li>`).join('')}</ul></details>` : ''}
        </article>`).join('') || '<p class="empty">Nothing here</p>'}</div>
    </section>`).join('');
  applyFilter();
  if (LIVE) wireDnd();
}

const q = document.getElementById('q');
function applyFilter(){
  const t = q.value.trim().toLowerCase();
  document.querySelectorAll('.card').forEach(c => c.classList.toggle('hidden', !!t && !c.dataset.search.includes(t)));
  document.querySelectorAll('.col').forEach(col => { col.querySelector('.count').textContent = col.querySelectorAll('.card:not(.hidden)').length; });
}
q.addEventListener('input', applyFilter);

// ---- drag and drop (live mode)
let dragId = null;
function clearMarks(){ document.querySelectorAll('.drop-before,.drop-after').forEach(c => c.classList.remove('drop-before','drop-after')); document.querySelectorAll('.col.over').forEach(c => c.classList.remove('over')); }
function wireDnd(){
  document.querySelectorAll('.card').forEach(card => {
    card.addEventListener('dragstart', e => { dragId = card.dataset.id; card.classList.add('dragging'); e.dataTransfer.effectAllowed = 'move'; e.dataTransfer.setData('text/plain', dragId); });
    card.addEventListener('dragend', () => { card.classList.remove('dragging'); clearMarks(); dragId = null; });
  });
  document.querySelectorAll('.col').forEach(col => {
    col.addEventListener('dragover', e => {
      if (!dragId) return;
      e.preventDefault(); e.dataTransfer.dropEffect = 'move';
      clearMarks(); col.classList.add('over');
      const target = e.target.closest('.card');
      if (target && target.dataset.id !== dragId) {
        const r = target.getBoundingClientRect();
        target.classList.add(e.clientY < r.top + r.height/2 ? 'drop-before' : 'drop-after');
      }
    });
    col.addEventListener('dragleave', e => { if (!col.contains(e.relatedTarget)) clearMarks(); });
    col.addEventListener('drop', async e => {
      e.preventDefault();
      const id = dragId || e.dataTransfer.getData('text/plain');
      const to = col.dataset.col;
      const cards = [...col.querySelectorAll('.card')].filter(c => c.dataset.id !== id);
      const target = e.target.closest('.card');
      let index = cards.length;
      if (target && target.dataset.id !== id) {
        const r = target.getBoundingClientRect();
        index = cards.indexOf(target) + (e.clientY < r.top + r.height/2 ? 0 : 1);
      }
      clearMarks();
      await move(id, to, index);
    });
  });
}
async function move(id, to, index){
  try {
    const res = await fetch('/api/move', { method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify({ id, to, index }) });
    const data = await res.json();
    if (!res.ok) throw new Error(data.error || res.statusText);
    board = data; render(); toast(`${id} → ${to} · saved to KANBAN.md`);
  } catch (err) { toast('Move failed: ' + err.message, true); }
}
if (LIVE) {
  // Pick up edits made to the Markdown file in an editor.
  setInterval(async () => { try { const r = await fetch('/api/board'); const d = await r.json(); if (JSON.stringify(d) !== JSON.stringify(board)) { board = d; render(); } } catch {} }, 3000);
}
render();
</script>
</body>
</html>
"""


def page(board: Board, live: bool) -> str:
    return (TEMPLATE.replace("__LIVE__", "true" if live else "false")
            .replace("__BOARD__", json.dumps(board.to_json()).replace("</", "<\\/"))
            .replace("__GENERATED__", date.today().isoformat()))


def counts(board: Board) -> str:
    return json.dumps({n: len(s) for n, s in board.columns})


# ----------------------------------------------------------------- server

class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):  # log writes only; the page polls /api/board every few seconds
        if args and str(args[0]).startswith("POST "):
            sys.stderr.write("%s %s\n" % (self.log_date_time_string(), fmt % args))

    def _send(self, status: int, body: bytes, ctype: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def _json(self, status: int, obj: dict) -> None:
        self._send(status, json.dumps(obj).encode(), "application/json")

    def do_GET(self) -> None:
        if self.path in ("/", "/index.html"):
            with LOCK:
                body = page(load(), live=True)
            self._send(200, body.encode(), "text/html; charset=utf-8")
        elif self.path == "/api/board":
            with LOCK:
                self._json(200, load().to_json())
        elif self.path == "/api/health":
            self._json(200, {"app": "kanban", "pid": os.getpid(), "file": str(SRC)})
        else:
            self._json(404, {"error": "not found"})

    def do_POST(self) -> None:
        if self.path == "/api/shutdown":
            self._json(200, {"ok": True, "pid": os.getpid()})
            threading.Thread(target=self.server.shutdown, daemon=True).start()
            return
        if self.path != "/api/move":
            return self._json(404, {"error": "not found"})
        try:
            length = int(self.headers.get("Content-Length") or 0)
            req = json.loads(self.rfile.read(length) or b"{}")
            story_id, to, index = str(req["id"]), str(req["to"]), int(req.get("index", 0))
        except (KeyError, ValueError, json.JSONDecodeError) as e:
            return self._json(400, {"error": f"bad request: {e}"})
        with LOCK:
            board = load()
            try:
                board.move(story_id, to, index)
            except KeyError as e:
                return self._json(404, {"error": f"unknown story or column: {e}"})
            save(board)
            self._json(200, board.to_json())


def existing_instance(port: int) -> dict | None:
    """Health info of a kanban server already on `port`, or None if the port holds something else."""
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/health", timeout=2) as r:
            info = json.loads(r.read() or b"{}")
            return info if info.get("app") == "kanban" else None
    except (urllib.error.URLError, json.JSONDecodeError, OSError):
        return None


def legacy_kanban_pid(port: int) -> int | None:
    """PID of a `kanban.py serve` process listening on `port` (one too old to answer /api/health)."""
    try:
        out = subprocess.run(["lsof", "-nP", f"-tiTCP:{port}", "-sTCP:LISTEN"], capture_output=True, text=True, timeout=5).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    for pid in out.split():
        try:
            cmd = subprocess.run(["ps", "-o", "command=", "-p", pid], capture_output=True, text=True, timeout=5).stdout
        except (OSError, subprocess.SubprocessError):
            continue
        if "kanban.py" in cmd and "serve" in cmd:
            return int(pid)
    return None


def bind(port: int) -> ThreadingHTTPServer | None:
    try:
        return ThreadingHTTPServer(("127.0.0.1", port), Handler)
    except OSError as e:
        if e.errno != 48 and "in use" not in str(e):
            raise
        return None


def cmd_serve(port: int, open_browser: bool, reuse: bool) -> int:
    url = f"http://127.0.0.1:{port}/"
    server = bind(port)
    if server is None:
        old = existing_instance(port)
        if old is None:
            legacy = legacy_kanban_pid(port)
            if legacy is None:
                print(f"cannot listen on 127.0.0.1:{port}: address in use by another program. Pass --port <n>.", file=sys.stderr)
                return 1
            old = {"pid": legacy, "legacy": True}
        if reuse:
            print(f"kanban board already running at {url} (pid {old.get('pid')}); reusing it", flush=True)
            if open_browser:
                webbrowser.open(url)
            return 0
        print(f"replacing previous board server (pid {old.get('pid')}) on port {port}", flush=True)
        if old.get("legacy"):
            try:
                os.kill(int(old["pid"]), signal.SIGTERM)
            except OSError as e:
                print(f"could not stop the previous server: {e}", file=sys.stderr)
                return 1
        else:
            try:
                urllib.request.urlopen(urllib.request.Request(f"{url}api/shutdown", method="POST", data=b""), timeout=2).read()
            except (urllib.error.URLError, OSError) as e:
                print(f"could not stop the previous server: {e}", file=sys.stderr)
                return 1
        deadline = time.time() + 5
        while server is None and time.time() < deadline:
            time.sleep(0.1)
            server = bind(port)
        if server is None:
            print(f"port {port} did not free up; stop pid {old.get('pid')} manually or pass --port <n>.", file=sys.stderr)
            return 1
    print(f"kanban board at {url}  (editing {SRC.relative_to(ROOT)}; Ctrl+C to stop)", flush=True)
    if open_browser:
        webbrowser.open(url)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


def cmd_render(open_browser: bool) -> int:
    board = load()
    if not board.columns:
        print(f"no '## ' sections found in {SRC}", file=sys.stderr)
        return 1
    OUT.write_text(page(board, live=False), encoding="utf-8")
    print(f"wrote {OUT.relative_to(ROOT)}  {counts(board)}")
    if open_browser:
        webbrowser.open(OUT.as_uri())
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("serve", help="live board with drag-and-drop")
    s.add_argument("--port", type=int, default=8765)
    s.add_argument("--no-open", action="store_true", help="do not open a browser")
    s.add_argument("--reuse", action="store_true", help="if a board is already running on the port, open it instead of replacing it")
    r = sub.add_parser("render", help="static export to docs/kanban.html")
    r.add_argument("--open", action="store_true")
    a = ap.parse_args()
    if a.cmd == "serve":
        return cmd_serve(a.port, not a.no_open, a.reuse)
    return cmd_render(a.open)


if __name__ == "__main__":
    raise SystemExit(main())
