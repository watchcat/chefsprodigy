import {
  DEFAULT_LIMIT,
  PLATFORM_LANGUAGES,
  pageRange,
  safeHttpUrl,
  searchQueryFromState,
  searchStateFromQuery,
} from "/assets/search-helpers.mjs";

const form = document.querySelector("#platform-search-form");
const queryInput = document.querySelector("#platform-query");
const ingredientInput = document.querySelector("#platform-ingredient");
const languageSelect = document.querySelector("#platform-language");
const results = document.querySelector("#platform-results");
const resultSummary = document.querySelector("#search-result-summary");
const previousButton = document.querySelector("#platform-prev-page");
const nextButton = document.querySelector("#platform-next-page");
const pageStatus = document.querySelector("#platform-page-status");
const hubDirectory = document.querySelector("#platform-hubs");

let messages = {};
let state = searchStateFromQuery(window.location.search);
let searchRequest = 0;

function stringValue(value, fallback = "") {
  return typeof value === "string" && value.trim() ? value.trim() : fallback;
}

function t(key, replacements = {}) {
  const template = stringValue(messages[key], key);
  return template.replace(/\{([a-zA-Z0-9_]+)\}/g, (_, name) => String(replacements[name] ?? ""));
}

function create(tagName, { className, text, attributes } = {}) {
  const element = document.createElement(tagName);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = text;
  if (attributes) {
    Object.entries(attributes).forEach(([name, value]) => {
      if (value !== null && value !== undefined && value !== "") element.setAttribute(name, String(value));
    });
  }
  return element;
}

async function loadMessages(language) {
  const safeLanguage = PLATFORM_LANGUAGES.has(language) ? language : "en";
  try {
    const response = await fetch(`/assets/i18n/${encodeURIComponent(safeLanguage)}.json`);
    if (!response.ok) throw new Error("message bundle unavailable");
    const bundle = await response.json();
    return bundle && typeof bundle === "object" ? bundle : {};
  } catch {
    if (safeLanguage !== "en") return loadMessages("en");
    return {};
  }
}

function applyStaticTranslations() {
  document.documentElement.lang = state.lang;
  document.title = t("Chef's Prodigy · Find a recipe");
  document.querySelectorAll("[data-i18n]").forEach((element) => {
    element.textContent = t(element.getAttribute("data-i18n"));
  });
  document.querySelectorAll("[data-i18n-attr]").forEach((element) => {
    const [attribute, key] = String(element.getAttribute("data-i18n-attr") || "").split(":");
    if (attribute && key) element.setAttribute(attribute, t(key));
  });
}

async function setLanguage(language) {
  state.lang = PLATFORM_LANGUAGES.has(language) ? language : "en";
  messages = await loadMessages(state.lang);
  languageSelect.value = state.lang;
  applyStaticTranslations();
}

function updateLocation({ replace = false } = {}) {
  const query = searchQueryFromState(state);
  const url = `${window.location.pathname}?${query}`;
  window.history[replace ? "replaceState" : "pushState"](state, "", url);
}

function setSearchBusy(busy) {
  results.setAttribute("aria-busy", String(busy));
  previousButton.disabled = busy;
  nextButton.disabled = busy;
}

function formatCount(total) {
  const count = Math.max(0, Number(total) || 0);
  return new Intl.NumberFormat(state.lang).format(count);
}

function showResultMessage(className, message) {
  results.replaceChildren(create("p", { className, text: message }));
}

function externalRecipeLink(url, title, className) {
  const safeUrl = safeHttpUrl(url);
  if (!safeUrl) return create("span", { className, text: title });
  return create("a", {
    className,
    text: title,
    attributes: { href: safeUrl },
  });
}

function renderHit(hit) {
  const card = create("article", { className: "result-card" });
  const imageUrl = safeHttpUrl(hit && hit.image_url);
  if (imageUrl) {
    card.append(create("img", {
      className: "result-card-image",
      attributes: {
        src: imageUrl,
        alt: "",
        loading: "lazy",
        referrerpolicy: "no-referrer",
      },
    }));
  }

  const body = create("div", { className: "result-card-body" });
  const title = stringValue(hit && hit.title, t("Untitled recipe"));
  body.append(externalRecipeLink(hit && hit.url, title, "result-card-title"));

  const hub = stringValue(hit && hit.hub, t("An independent hub"));
  const meta = create("div", { className: "card-meta" });
  meta.append(create("span", { text: t("From {hub}", { hub }) }));
  body.append(meta);

  const tags = Array.isArray(hit && hit.tags) ? hit.tags : [];
  if (tags.length) {
    const tagList = create("div", { className: "tag-list", attributes: { "aria-label": t("Recipe tags") } });
    tags.slice(0, 8).forEach((tag) => tagList.append(create("span", { className: "tag", text: String(tag) })));
    body.append(tagList);
  }

  const sourceLanguage = stringValue(hit && hit.source_language);
  const matchedLanguage = stringValue(hit && hit.matched_lang);
  if (matchedLanguage && sourceLanguage && matchedLanguage !== sourceLanguage) {
    body.append(create("p", {
      className: "translation-hint",
      text: t("Shown in {language}", { language: languageName(matchedLanguage) }),
    }));
  }

  const alsoAt = Array.isArray(hit && hit.also_at) ? hit.also_at : [];
  if (alsoAt.length) {
    const alternate = create("div", { className: "also-at" });
    alternate.append(create("strong", { text: t("Also at") }));
    const links = create("div", { className: "also-at-list" });
    alsoAt.forEach((place) => {
      const name = stringValue(place && place.hub, t("Another hub"));
      links.append(externalRecipeLink(place && place.url, name, "also-at-link"));
    });
    alternate.append(links);
    body.append(alternate);
  }

  card.append(body);
  return card;
}

function languageName(language) {
  const names = new Intl.DisplayNames([state.lang], { type: "language" });
  try {
    return names.of(language) || language;
  } catch {
    return language;
  }
}

function renderSearch(page) {
  const hits = Array.isArray(page && page.hits) ? page.hits : [];
  const total = Math.max(0, Number(page && page.total) || 0);
  const offset = Math.max(0, Number(page && page.offset) || 0);
  const limit = Math.max(1, Number(page && page.limit) || state.limit);
  state = { ...state, offset, limit };
  const range = pageRange({ total, offset, limit });
  resultSummary.textContent = total
    ? t("Showing {first}–{last} of {total}", { first: range.first, last: range.last, total: formatCount(total) })
    : t("No recipes found");
  pageStatus.textContent = total
    ? t("Page {page} of {pages}", { page: range.page, pages: range.pages })
    : "";
  previousButton.disabled = offset === 0;
  nextButton.disabled = offset + limit >= total;

  if (!hits.length) {
    showResultMessage("empty-state", state.q || state.ingredient
      ? t("Try another phrase or ingredient.")
      : t("No public recipes have been shared yet. Check back soon."));
    return;
  }
  results.replaceChildren(...hits.map(renderHit));
}

async function loadSearch() {
  const currentRequest = ++searchRequest;
  setSearchBusy(true);
  showResultMessage("empty-state", t("Searching public recipe hubs…"));
  const query = searchQueryFromState(state);
  try {
    const response = await fetch(`/api/v1/search?${query}`, { headers: { Accept: "application/json" } });
    if (!response.ok) throw new Error(t("The search service is unavailable. Please try again."));
    const page = await response.json();
    if (currentRequest !== searchRequest) return;
    renderSearch(page);
  } catch (error) {
    if (currentRequest !== searchRequest) return;
    resultSummary.textContent = t("Search unavailable");
    pageStatus.textContent = "";
    previousButton.disabled = true;
    nextButton.disabled = true;
    showResultMessage("error-state", stringValue(error && error.message, t("The search service is unavailable. Please try again.")));
  } finally {
    if (currentRequest === searchRequest) setSearchBusy(false);
  }
}

function renderDirectory(payload) {
  const hubs = Array.isArray(payload && payload.hubs) ? payload.hubs : [];
  hubDirectory.setAttribute("aria-busy", "false");
  if (!hubs.length) {
    hubDirectory.replaceChildren(create("p", { className: "empty-state", text: t("No hubs are listed yet.") }));
    return;
  }
  const cards = hubs.map((hub) => {
    const name = stringValue(hub && hub.name, t("Independent hub"));
    const chef = stringValue(hub && hub.chef_name, name);
    const count = Math.max(0, Number(hub && hub.recipe_count) || 0);
    const link = externalRecipeLink(hub && hub.public_url, chef, "hub-card");
    link.append(
      create("span", { text: name }),
      create("small", { text: t("{count} public recipes", { count: formatCount(count) }) }),
    );
    return link;
  });
  hubDirectory.replaceChildren(...cards);
}

async function loadDirectory() {
  hubDirectory.setAttribute("aria-busy", "true");
  hubDirectory.replaceChildren(create("p", { className: "empty-state", text: t("Loading hubs…") }));
  try {
    const response = await fetch("/api/v1/hubs", { headers: { Accept: "application/json" } });
    if (!response.ok) throw new Error();
    renderDirectory(await response.json());
  } catch {
    hubDirectory.setAttribute("aria-busy", "false");
    hubDirectory.replaceChildren(create("p", { className: "error-state", text: t("The hub directory is unavailable right now.") }));
  }
}

async function applyLocation({ replace = true } = {}) {
  const next = searchStateFromQuery(window.location.search, state.lang);
  state = { ...state, ...next };
  await setLanguage(state.lang);
  queryInput.value = state.q;
  ingredientInput.value = state.ingredient;
  if (replace) updateLocation({ replace: true });
  await loadSearch();
}

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  state = {
    ...state,
    q: stringValue(queryInput.value),
    ingredient: stringValue(ingredientInput.value),
    lang: languageSelect.value,
    offset: 0,
  };
  await setLanguage(state.lang);
  updateLocation();
  await loadSearch();
});

languageSelect.addEventListener("change", async () => {
  state = { ...state, lang: languageSelect.value, offset: 0 };
  await setLanguage(state.lang);
  updateLocation();
  await loadSearch();
  await loadDirectory();
});

previousButton.addEventListener("click", async () => {
  state = { ...state, offset: Math.max(0, state.offset - state.limit) };
  updateLocation();
  await loadSearch();
});

nextButton.addEventListener("click", async () => {
  state = { ...state, offset: state.offset + state.limit };
  updateLocation();
  await loadSearch();
});

window.addEventListener("popstate", () => { applyLocation({ replace: false }); });

await applyLocation();
await loadDirectory();
