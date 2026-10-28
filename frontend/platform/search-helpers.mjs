export const PLATFORM_LANGUAGES = new Set(["en", "ru", "nl"]);
export const DEFAULT_LIMIT = 12;

function text(value) {
  return typeof value === "string" ? value.trim() : "";
}

export function safeHttpUrl(value) {
  try {
    const url = new URL(String(value || ""));
    return url.protocol === "http:" || url.protocol === "https:" ? url.href : null;
  } catch {
    return null;
  }
}

export function searchStateFromQuery(search, fallbackLanguage = "en") {
  const params = new URLSearchParams(search || "");
  const candidate = text(params.get("lang"));
  const lang = PLATFORM_LANGUAGES.has(candidate) ? candidate : fallbackLanguage;
  const offset = Math.max(0, Number.parseInt(params.get("offset") || "0", 10) || 0);
  const limit = Math.min(50, Math.max(1, Number.parseInt(params.get("limit") || String(DEFAULT_LIMIT), 10) || DEFAULT_LIMIT));
  return {
    q: text(params.get("q")),
    ingredient: text(params.get("ingredient")),
    lang: PLATFORM_LANGUAGES.has(lang) ? lang : "en",
    offset,
    limit,
  };
}

export function searchQueryFromState(state) {
  const params = new URLSearchParams();
  const q = text(state && state.q);
  const ingredient = text(state && state.ingredient);
  const lang = text(state && state.lang);
  const offset = Math.max(0, Number.parseInt(state && state.offset, 10) || 0);
  const limit = Math.min(50, Math.max(1, Number.parseInt(state && state.limit, 10) || DEFAULT_LIMIT));
  if (q) params.set("q", q);
  if (ingredient) params.set("ingredient", ingredient);
  params.set("lang", PLATFORM_LANGUAGES.has(lang) ? lang : "en");
  if (limit !== DEFAULT_LIMIT) params.set("limit", String(limit));
  if (offset) params.set("offset", String(offset));
  return params.toString();
}

export function pageRange({ total = 0, offset = 0, limit = DEFAULT_LIMIT } = {}) {
  const count = Math.max(0, Number(total) || 0);
  if (!count) return { first: 0, last: 0, page: 0, pages: 0 };
  const safeLimit = Math.max(1, Number(limit) || DEFAULT_LIMIT);
  const safeOffset = Math.max(0, Number(offset) || 0);
  return {
    first: Math.min(count, safeOffset + 1),
    last: Math.min(count, safeOffset + safeLimit),
    page: Math.floor(safeOffset / safeLimit) + 1,
    pages: Math.ceil(count / safeLimit),
  };
}
