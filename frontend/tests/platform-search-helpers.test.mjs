import test from "node:test";
import assert from "node:assert/strict";

import {
  pageRange,
  safeHttpUrl,
  searchQueryFromState,
  searchStateFromQuery,
} from "../platform/search-helpers.mjs";

test("search state round-trips shareable Russian search parameters", () => {
  const state = searchStateFromQuery("?q=%D0%B3%D0%BE%D1%80%D0%BE%D1%85%D0%BE%D0%B2%D1%8B%D0%B9&ingredient=split_pea%2Conion&lang=ru&offset=24&limit=12");
  assert.deepEqual(state, {
    q: "гороховый",
    ingredient: "split_pea,onion",
    lang: "ru",
    offset: 24,
    limit: 12,
  });
  assert.equal(
    searchQueryFromState(state),
    "q=%D0%B3%D0%BE%D1%80%D0%BE%D1%85%D0%BE%D0%B2%D1%8B%D0%B9&ingredient=split_pea%2Conion&lang=ru&offset=24",
  );
});

test("search state clamps unsafe pagination and defaults unsupported languages", () => {
  assert.deepEqual(searchStateFromQuery("?lang=javascript:evil&offset=-7&limit=999"), {
    q: "",
    ingredient: "",
    lang: "en",
    offset: 0,
    limit: 50,
  });
  assert.deepEqual(pageRange({ total: 25, offset: 12, limit: 12 }), {
    first: 13,
    last: 24,
    page: 2,
    pages: 3,
  });
});

test("only web URLs can become platform links and images", () => {
  assert.equal(safeHttpUrl("https://hub.example/recipe"), "https://hub.example/recipe");
  assert.equal(safeHttpUrl("http://hub.example/image.webp"), "http://hub.example/image.webp");
  assert.equal(safeHttpUrl("javascript:alert(1)"), null);
  assert.equal(safeHttpUrl("data:text/html,unsafe"), null);
  assert.equal(safeHttpUrl("/relative/path"), null);
});
