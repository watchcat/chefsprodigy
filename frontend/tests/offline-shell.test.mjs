import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const dist = new URL("../dist/", import.meta.url);
const read = (name) => readFileSync(new URL(name, dist), "utf8");

function appShell() {
  const source = read("sw.js");
  const list = source.match(/const APP_SHELL = \[([\s\S]*?)\];/);
  assert.ok(list, "sw.js declares APP_SHELL");
  return [...list[1].matchAll(/"([^"]+)"/g)].map((match) => match[1]);
}

test("every stylesheet the hub imports is precached for offline use", () => {
  const shell = appShell();
  const imports = [...read("styles.css").matchAll(/@import\s+url\(["']?([^"')]+)["']?\)/g)].map((m) => m[1]);
  assert.ok(imports.length > 0, "styles.css imports the shared tokens");
  for (const path of imports) {
    assert.ok(shell.includes(path), `${path} is imported by styles.css but missing from APP_SHELL in sw.js`);
  }
});

test("every local stylesheet and script in index.html is precached", () => {
  const shell = appShell();
  const html = read("index.html");
  const local = [...html.matchAll(/(?:href|src)="(\/[^"]+\.(?:css|js))"/g)].map((m) => m[1]);
  for (const path of local) {
    assert.ok(shell.includes(path), `${path} is loaded by index.html but missing from APP_SHELL in sw.js`);
  }
});
