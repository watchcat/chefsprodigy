(() => {
  "use strict";

  const API_ROOT = "/api/v1";
  const PAGE_SIZE = 12;
  const MAX_PHOTO_COUNT = 6;
  const MAX_PHOTO_BYTES = 20 * 1024 * 1024;
  const UI_LANGUAGES = ["en", "ru", "nl"];
  const UI_LANGUAGE_STORAGE_KEY = "ui.lang";
  const main = document.querySelector("#main-content");
  const libraryLink = document.querySelector("#library-link");
  const shoppingLink = document.querySelector("#shopping-link");
  const importDialog = document.querySelector("#import-dialog");
  const deleteDialog = document.querySelector("#delete-dialog");
  const installAppButton = document.querySelector("#install-app-button");
  const languageSelect = document.querySelector("#ui-language");
  const toastRegion = document.querySelector("#toast-region");

  const state = {
    language: initialLanguage(),
    bundles: {},
    missingMessageKeys: new Set(),
    route: { name: "library" },
    filters: loadFilters(),
    library: {
      items: [],
      limit: PAGE_SIZE,
      offset: 0,
      page: 1,
      total: 0,
      loading: false,
      error: null,
    },
    recipe: null,
    detailServings: null,
    sourceViewRecipeId: null,
    sourceTranslation: null,
    requestVersion: 0,
    searchTimer: null,
    cook: null,
    pendingDelete: null,
    installPrompt: null,
    editorFieldSequence: 0,
    photoPreviewUrls: [],
    shopping: {
      recipes: [],
      loading: false,
      error: null,
      selections: loadShoppingSelections(),
      pantry: loadShoppingPantry(),
      result: null,
      status: "",
      statusTone: "default",
      submitting: false,
      checked: loadShoppingChecks(),
    },
  };

  function initialLanguage() {
    try {
      const saved = localStorage.getItem(UI_LANGUAGE_STORAGE_KEY);
      if (UI_LANGUAGES.includes(saved)) return saved;
    } catch {
      // Fall back to the browser language when storage is unavailable.
    }
    const preferred = Array.isArray(navigator.languages) && navigator.languages.length
      ? navigator.languages
      : [navigator.language];
    for (const candidate of preferred) {
      const language = String(candidate || "").toLowerCase().split("-", 1)[0];
      if (UI_LANGUAGES.includes(language)) return language;
    }
    return "en";
  }

  async function loadBundle(language) {
    if (!UI_LANGUAGES.includes(language)) return {};
    if (state.bundles[language]) return state.bundles[language];
    const response = await fetch(`/i18n/${encodeURIComponent(language)}.json`, { cache: "no-cache" });
    if (!response.ok) throw new Error(`Unable to load the ${language} interface messages.`);
    const bundle = await response.json();
    state.bundles[language] = bundle && typeof bundle === "object" ? bundle : {};
    return state.bundles[language];
  }

  function t(key, vars = {}) {
    const localized = state.bundles[state.language] && state.bundles[state.language][key];
    const english = state.bundles.en && state.bundles.en[key];
    const template = typeof localized === "string" ? localized : typeof english === "string" ? english : key;
    if (template === key && !state.missingMessageKeys.has(key)) {
      state.missingMessageKeys.add(key);
      console.warn(`Missing UI message: ${key}`);
    }
    return template.replace(/\{([A-Za-z0-9_]+)\}/g, (_, name) => String(vars[name] ?? `{${name}}`));
  }

  function plural(key, count, vars = {}) {
    const category = new Intl.PluralRules(state.language).select(Number(count) || 0);
    const fallbackCategory = category === "other" ? "other" : "other";
    const fullKey = `${key}.${category}`;
    const fallbackKey = `${key}.${fallbackCategory}`;
    const message = (state.bundles[state.language] && state.bundles[state.language][fullKey])
      || (state.bundles.en && state.bundles.en[fullKey])
      || (state.bundles[state.language] && state.bundles[state.language][fallbackKey])
      || (state.bundles.en && state.bundles.en[fallbackKey])
      || fullKey;
    return String(message).replace(/\{([A-Za-z0-9_]+)\}/g, (_, name) => String((name === "count" ? count : vars[name]) ?? `{${name}}`));
  }

  function formatNumber(value, options = {}) {
    return new Intl.NumberFormat(state.language, options).format(Number(value) || 0);
  }

  function formatUnit(value, unit) {
    const amount = Number(value);
    if (!Number.isFinite(amount) || amount <= 0) return unit === "to_taste" ? t("unit.to_taste") : t("amount.as_needed");
    const config = {
      g: { unit: "gram", key: "unit.g" },
      ml: { unit: "milliliter", key: "unit.ml" },
      pcs: { unit: "piece", key: "unit.pcs" },
    }[unit];
    if (!config) return formatNumber(amount, { maximumFractionDigits: amount < 10 ? 1 : 0 });
    try {
      return new Intl.NumberFormat(state.language, {
        style: "unit",
        unit: config.unit,
        unitDisplay: "short",
        maximumFractionDigits: amount < 10 ? 1 : 0,
      }).format(amount);
    } catch {
      return `${formatNumber(amount, { maximumFractionDigits: amount < 10 ? 1 : 0 })} ${t(config.key)}`;
    }
  }

  function applyStaticTranslations() {
    document.documentElement.lang = state.language;
    document.querySelectorAll("[data-i18n]").forEach((element) => {
      element.textContent = t(element.getAttribute("data-i18n"));
    });
    document.querySelectorAll("[data-i18n-attr]").forEach((element) => {
      const assignments = String(element.getAttribute("data-i18n-attr") || "").split(",");
      assignments.forEach((assignment) => {
        const [attribute, key] = assignment.split(":", 2).map((part) => part.trim());
        if (attribute && key) element.setAttribute(attribute, t(key));
      });
    });
    const english = state.bundles.en || {};
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    const textNodes = [];
    while (walker.nextNode()) textNodes.push(walker.currentNode);
    textNodes.forEach((node) => {
      const raw = node.nodeValue || "";
      const key = raw.trim();
      if (!key || !Object.hasOwn(english, key)) return;
      node.nodeValue = raw.replace(key, t(key));
    });
    if (languageSelect) languageSelect.value = state.language;
  }

  async function setLanguage(language) {
    const next = UI_LANGUAGES.includes(language) ? language : "en";
    try {
      await Promise.all([loadBundle("en"), loadBundle(next)]);
      state.language = next;
      state.sourceViewRecipeId = null;
      state.sourceTranslation = null;
      try {
        localStorage.setItem(UI_LANGUAGE_STORAGE_KEY, next);
      } catch {
        // The selected language remains active for this session.
      }
      applyStaticTranslations();
      await handleRoute();
    } catch (error) {
      showToast(error.message, "error");
    }
  }

  function loadFilters() {
    try {
      const saved = JSON.parse(localStorage.getItem("chefs-prodigy:filters") || "{}");
      return {
        query: typeof saved.query === "string" ? saved.query.slice(0, 180) : "",
        tag: typeof saved.tag === "string" ? saved.tag.slice(0, 80) : "",
        ingredient: typeof saved.ingredient === "string" ? saved.ingredient.slice(0, 80) : "",
      };
    } catch {
      return { query: "", tag: "", ingredient: "" };
    }
  }

  function saveFilters() {
    try {
      localStorage.setItem("chefs-prodigy:filters", JSON.stringify(state.filters));
    } catch {
      // Filtering still works when browser storage is unavailable.
    }
  }

  function normalizeShoppingServings(value, fallback = 1) {
    const servings = Math.round(Number(value));
    return Number.isInteger(servings) && servings >= 1 && servings <= 99 ? servings : fallback;
  }

  function loadShoppingSelections() {
    try {
      const stored = JSON.parse(localStorage.getItem("chefs-prodigy:shopping-selections") || "[]");
      if (!Array.isArray(stored)) return [];
      const seen = new Set();
      return stored.reduce((selections, item) => {
        const id = typeof item?.id === "string" ? item.id.trim().slice(0, 180) : "";
        if (!id || seen.has(id)) return selections;
        seen.add(id);
        selections.push({ id, servings: normalizeShoppingServings(item?.servings) });
        return selections;
      }, []);
    } catch {
      return [];
    }
  }

  function saveShoppingSelections() {
    try {
      localStorage.setItem("chefs-prodigy:shopping-selections", JSON.stringify(state.shopping.selections));
    } catch {
      // The builder remains usable when browser storage is unavailable.
    }
  }

  function loadShoppingPantry() {
    try {
      const value = localStorage.getItem("chefs-prodigy:shopping-pantry");
      return typeof value === "string" ? value.slice(0, 1000) : "";
    } catch {
      return "";
    }
  }

  function saveShoppingPantry() {
    try {
      localStorage.setItem("chefs-prodigy:shopping-pantry", state.shopping.pantry);
    } catch {
      // Pantry exclusions still apply for this session when storage is unavailable.
    }
  }

  function loadShoppingChecks() {
    try {
      const stored = JSON.parse(localStorage.getItem("chefs-prodigy:shopping-checks") || "{}");
      if (!stored || typeof stored !== "object" || Array.isArray(stored)) return {};
      return Object.fromEntries(Object.entries(stored).filter(([key, value]) => key.length <= 300 && value === true).slice(0, 500));
    } catch {
      return {};
    }
  }

  function saveShoppingChecks() {
    try {
      localStorage.setItem("chefs-prodigy:shopping-checks", JSON.stringify(state.shopping.checked));
    } catch {
      // Checked-off rows remain visible for this session when storage is unavailable.
    }
  }

  function create(tagName, options = {}, children = []) {
    const element = document.createElement(tagName);
    const { className, text, attributes = {}, dataset = {} } = options;
    if (className) element.className = className;
    if (text !== undefined && text !== null) element.textContent = localizedText(text);
    for (const [name, value] of Object.entries(attributes)) {
      if (value === false || value === null || value === undefined) continue;
      if (value === true) {
        element.setAttribute(name, "");
      } else {
        element.setAttribute(name, String(value));
      }
    }
    for (const [name, value] of Object.entries(dataset)) {
      element.dataset[name] = String(value);
    }
    appendChildren(element, children);
    return element;
  }

  function appendChildren(parent, children) {
    for (const child of Array.isArray(children) ? children.flat(Infinity) : [children]) {
      if (child === null || child === undefined || child === false) continue;
      parent.append(child instanceof Node ? child : document.createTextNode(localizedText(child)));
    }
    return parent;
  }

  function localizedText(value) {
    if (typeof value !== "string") return String(value);
    return state.bundles.en && Object.hasOwn(state.bundles.en, value) ? t(value) : value;
  }

 function button(label, action, options = {}) {
   return create("button", {
     className: options.className || "button button-secondary",
     text: label,
     attributes: {
       type: "button",
       disabled: options.disabled || false,
       "data-action": action,
       "aria-label": options.ariaLabel,
        ...(options.attributes || {}),
     },
     dataset: options.dataset || {},
   });
 }

  function setView(view) {
    main.replaceChildren(view);
    main.focus({ preventScroll: true });
  }

  function showToast(message, tone = "default") {
    const toast = create("div", { className: `toast${tone === "error" ? " toast-error" : ""}`, text: message, attributes: { role: "status" } });
    toastRegion.append(toast);
    window.setTimeout(() => toast.remove(), 4200);
  }

  async function apiRequest(path, options = {}) {
    const headers = new Headers(options.headers || {});
    if (options.body && !(options.body instanceof FormData) && !headers.has("Content-Type")) headers.set("Content-Type", "application/json");

    let response;
    try {
      response = await fetch(`${API_ROOT}${path}`, { ...options, headers });
    } catch {
      throw new Error("Could not reach your recipe node. Check that the node is running and try again.");
    }

    if (!response.ok) throw await responseError(response);
    if (response.status === 204) return null;
    return response.json();
  }

  async function responseError(response) {
    let message = `The recipe node returned ${response.status}.`;
    try {
      const body = await response.json();
      if (body && typeof body.error === "string" && body.error.trim()) message = body.error;
    } catch {
      try {
        const text = await response.text();
        if (text.trim()) message = text.trim();
      } catch {
        // Keep the status-based message.
      }
    }
    const error = new Error(message);
    error.status = response.status;
    return error;
  }

  function parseRoute() {
    const fragment = location.hash.replace(/^#/, "") || "/";
    const [pathname, queryString = ""] = fragment.split("?", 2);
    const path = pathname.startsWith("/") ? pathname : "/";
    const segments = path.split("/").filter(Boolean);
    const query = new URLSearchParams(queryString);
    if (segments.length === 0) return { name: "library", page: pageFromHash(query.get("page")) };
    if (segments.length === 1 && segments[0] === "new") return { name: "editor", mode: "create" };
    if (segments.length === 1 && segments[0] === "shopping") return { name: "shopping" };
    if (segments[0] !== "recipe" || !segments[1]) return { name: "not-found" };

    try {
      const id = decodeURIComponent(segments[1]);
      if (!id) return { name: "not-found" };
      if (segments.length === 2) return { name: "recipe", id };
      if (segments.length === 3 && segments[2] === "cook") return { name: "cook", id };
      if (segments.length === 3 && segments[2] === "edit") return { name: "editor", mode: "edit", id };
      if (segments.length === 3 && segments[2] === "translation") return { name: "translation-editor", id };
    } catch {
      return { name: "not-found" };
    }
    return { name: "not-found" };
  }

  function pageFromHash(value) {
    const page = Number(value);
    return Number.isInteger(page) && page > 0 && page <= 100000 ? page : 1;
  }

  function navigate(path) {
    const next = `#${path}`;
    if (location.hash === next) {
      handleRoute();
    } else {
      location.hash = next;
    }
  }

  function navigateLibraryPage(page) {
    const target = pageFromHash(page);
    navigate(target === 1 ? "/" : `/?page=${target}`);
  }

  function updateNavigation() {
    const inLibrary = state.route.name === "library";
    const inShopping = state.route.name === "shopping";
    if (inLibrary) libraryLink.setAttribute("aria-current", "page");
    else libraryLink.removeAttribute("aria-current");
    if (shoppingLink) {
      if (inShopping) shoppingLink.setAttribute("aria-current", "page");
      else shoppingLink.removeAttribute("aria-current");
    }
  }

  async function handleRoute() {
    const previousRoute = state.route;
    const nextRoute = parseRoute();
    state.route = nextRoute;
    updateNavigation();

    if (previousRoute.name === "cook" && (nextRoute.name !== "cook" || nextRoute.id !== previousRoute.id)) {
      stopCookSession();
    }

    if (nextRoute.name === "library") {
      document.title = "Chef's Prodigy · Recipe library";
      await loadLibrary();
      return;
    }
    if (nextRoute.name === "shopping") {
      document.title = "Shopping list · Chef's Prodigy";
      await loadShopping();
      return;
    }
    if (nextRoute.name === "recipe" || nextRoute.name === "cook") {
      await loadRecipe(nextRoute.id, nextRoute.name === "cook");
      return;
    }
    if (nextRoute.name === "editor") {
      await loadEditor(nextRoute.mode === "edit" ? nextRoute.id : null);
      return;
    }
    if (nextRoute.name === "translation-editor") {
      await loadTranslationEditor(nextRoute.id);
      return;
    }

    document.title = "Page not found · Chef's Prodigy";
    setView(renderStatus({
      title: t("That page is not on the menu."),
      message: t("Return to your recipe library to keep cooking."),
      actionLabel: "Open library",
      action: "go-library",
    }));
  }

  async function loadLibrary() {
    const version = ++state.requestVersion;
    const page = state.route.name === "library" ? state.route.page : 1;
    state.library.page = page;
    state.library.offset = (page - 1) * PAGE_SIZE;
    state.library.items = [];
    state.library.error = null;
    state.library.loading = true;
    setView(renderLibrary());

    const query = new URLSearchParams({ limit: String(PAGE_SIZE), offset: String(state.library.offset), lang: state.language });
    if (state.filters.query.trim()) query.set("q", state.filters.query.trim());
    if (state.filters.tag.trim()) query.set("tag", state.filters.tag.trim());
    if (state.filters.ingredient.trim()) query.set("ingredient", state.filters.ingredient.trim());

    try {
      const payload = await apiRequest(`/recipes?${query.toString()}`);
      if (version !== state.requestVersion || state.route.name !== "library") return;

      const incoming = Array.isArray(payload.items) ? payload.items : [];
      const limit = Number(payload.limit);
      state.library.limit = Number.isInteger(limit) && limit > 0 ? limit : PAGE_SIZE;
      const total = Number(payload.total);
      state.library.total = Number.isInteger(total) && total >= 0 ? total : incoming.length;
      const pageCount = libraryPageCount();
      if (page > pageCount) {
        navigateLibraryPage(pageCount);
        return;
      }
      state.library.items = incoming;
      state.library.loading = false;
      state.library.error = null;
      setView(renderLibrary());
    } catch (error) {
      if (version !== state.requestVersion || state.route.name !== "library") return;
      state.library.loading = false;
      state.library.error = error.message;
      setView(renderLibrary());
    }
  }

  async function loadRecipe(id, cookMode) {
   const version = ++state.requestVersion;
   const viewingSource = state.sourceViewRecipeId === id;
   setView(renderLoading(cookMode ? "Preparing Cook Mode…" : viewingSource ? "Loading recipe…" : t("Translating recipe…")));

    const selectedServings = !cookMode && state.recipe && state.recipe.id === id && Number.isInteger(state.detailServings)
      ? state.detailServings
      : null;
   const query = new URLSearchParams();
   if (selectedServings) query.set("servings", String(selectedServings));
   query.set("lang", state.sourceViewRecipeId === id ? "source" : state.language);
   const queryString = `?${query.toString()}`;
   try {
     const recipe = await apiRequest(`/recipes/${encodeURIComponent(id)}${queryString}`);
      if (version !== state.requestVersion || state.route.id !== id) return;

      state.recipe = recipe;
      state.detailServings = recipe.servings;
      document.title = `${recipe.title || "Recipe"} · Chef's Prodigy`;
      if (cookMode) {
        ensureCookSession(recipe);
        setView(renderCookMode(recipe));
        requestWakeLock();
      } else {
        setView(renderRecipe(recipe));
      }
    } catch (error) {
      if (version !== state.requestVersion || state.route.id !== id) return;
      setView(renderStatus({
        title: t("We could not load that recipe."),
        message: error.message,
        error: true,
        actionLabel: "Back to library",
        action: "go-library",
      }));
    }
  }

  async function loadTranslationEditor(id) {
    const version = ++state.requestVersion;
    setView(renderLoading(t("Opening translation correction…")));
    try {
      const recipe = await apiRequest(`/recipes/${encodeURIComponent(id)}?lang=${encodeURIComponent(state.language)}`);
      if (version !== state.requestVersion || state.route.name !== "translation-editor" || state.route.id !== id) return;
      if (!recipe.translation || recipe.translation.lang !== state.language) {
        setView(renderStatus({
          title: t("No translation is available to correct in this language."),
          message: t("Open the recipe in its display language first, then return here after a translation is available."),
          actionLabel: "Back to recipe",
          action: "back-to-recipe",
        }));
        return;
      }
      state.recipe = recipe;
      document.title = `${t("Fix translation")} · ${recipe.title || "Recipe"} · Chef's Prodigy`;
      setView(renderTranslationEditor(recipe));
    } catch (error) {
      if (version !== state.requestVersion || state.route.name !== "translation-editor" || state.route.id !== id) return;
      setView(renderStatus({
        title: t("We could not open this translation."),
        message: error.message,
        error: true,
        actionLabel: "Back to recipe",
        action: "back-to-recipe",
      }));
    }
  }

  function emptyRecipeDraft() {
    return {
      title: t(""),
      description: "",
      servings: 4,
      prep_time_minutes: null,
      cook_time_minutes: null,
      ingredients: [{ raw_text: "", metric_amount: null, unit: "g", canonical_id: "", is_specialty: false }],
      instructions: [{ step_number: 1, text: t(""), timer_seconds: null }],
      tags: [],
      image_path: null,
      origin_url: null,
      is_public: false,
    };
  }

  async function loadEditor(id) {
    const version = ++state.requestVersion;
    if (!id) {
      state.recipe = null;
      document.title = "Create recipe · Chef's Prodigy";
      setView(renderRecipeEditor(emptyRecipeDraft(), "create"));
      return;
    }

    setView(renderLoading("Opening recipe editor…"));
    try {
      const recipe = await apiRequest(`/recipes/${encodeURIComponent(id)}?lang=source`);
      if (version !== state.requestVersion || state.route.name !== "editor" || state.route.id !== id) return;
      state.recipe = recipe;
      document.title = `Edit ${stringValue(recipe.title, "recipe")} · Chef's Prodigy`;
      setView(renderRecipeEditor(recipe, "edit"));
    } catch (error) {
      if (version !== state.requestVersion || state.route.name !== "editor" || state.route.id !== id) return;
      setView(renderStatus({
        title: t("We could not open that recipe for editing."),
        message: error.message,
        error: true,
        actionLabel: "Back to library",
        action: "go-library",
      }));
    }
  }

  function renderLibrary() {
    const root = create("section", { className: "library-view" });
    const hero = create("div", { className: "hero" }, [
      create("div", { className: "hero-copy" }, [
        create("p", { className: "eyebrow", text: t("Your private kitchen archive") }),
        create("h1", { text: t("Cook what you want to remember.") }),
        create("p", {
          className: "hero-description",
          text: t("Search the recipes you love, pull in a new find, and move through every step without leaving the counter."),
        }),
      ]),
      create("div", { className: "hero-actions" }, [
        button(t("Create a recipe"), "new-recipe", { className: "button button-secondary" }),
        button(t("Plan a shopping list"), "open-shopping", { className: "button button-secondary" }),
        button(t("Import a recipe"), "open-import", { className: "button button-coral" }),
      ]),
    ]);
    root.append(hero, renderSearchPanel());

    const collectionHeader = create("div", { className: "collection-header" }, [
      create("h2", { className: "collection-title", text: collectionTitle() }),
      create("p", { className: "collection-meta", text: collectionMeta() }),
    ]);
    root.append(collectionHeader);

    if (state.library.error) {
      root.append(renderStatus({
        title: t("The library could not be reached."),
        message: state.library.error,
        error: true,
        actionLabel: "Try again",
        action: "retry-library",
      }));
      return root;
    }

    if (state.library.loading && state.library.items.length === 0) {
      root.append(renderSkeletonGrid());
      return root;
    }

    if (state.library.items.length === 0) {
      const filtered = Boolean(state.filters.query || state.filters.tag || state.filters.ingredient);
      root.append(renderStatus({
        title: filtered ? "No recipes match those ingredients." : "Your recipe shelf is ready.",
        message: filtered
          ? "Try a broader search, clear a filter, or bring in a recipe from a link or text."
          : "Import a recipe from a favourite site or paste one in to start your collection.",
        actionLabel: filtered ? "Clear filters" : "Import your first recipe",
        action: filtered ? "clear-filters" : "open-import",
      }));
      return root;
    }

    const grid = create("ul", { className: "recipe-grid", attributes: { "aria-label": "Recipe results" } });
    state.library.items.forEach((recipe) => grid.append(create("li", {}, [renderRecipeCard(recipe)])));
    root.append(grid);

    const pagination = renderPagination();
    if (pagination) root.append(pagination);
    return root;
  }

  async function loadShopping() {
    const version = ++state.requestVersion;
    state.shopping.loading = true;
    state.shopping.error = null;
    setView(renderShopping());

    try {
      const recipes = [];
      let offset = 0;
      let total = null;
      for (let request = 0; request < 100; request += 1) {
        const payload = await apiRequest(`/recipes?limit=100&offset=${offset}&lang=${encodeURIComponent(state.language)}`);
        if (version !== state.requestVersion || state.route.name !== "shopping") return;
        const incoming = safeArray(payload && payload.items);
        const reportedTotal = Number(payload && payload.total);
        if (Number.isInteger(reportedTotal) && reportedTotal >= 0) total = reportedTotal;
        recipes.push(...incoming);
        if (!incoming.length || total === null || recipes.length >= total) break;
        offset += incoming.length;
      }

      const seen = new Set();
      state.shopping.recipes = recipes.filter((recipe) => {
        const id = stringValue(recipe && recipe.id);
        if (!id || seen.has(id)) return false;
        seen.add(id);
        return true;
      });
      state.shopping.loading = false;
      state.shopping.error = null;
      setView(renderShopping());
    } catch (error) {
      if (version !== state.requestVersion || state.route.name !== "shopping") return;
      state.shopping.loading = false;
      state.shopping.error = error.message;
      setView(renderShopping());
    }
  }

  function renderShopping() {
    const root = create("section", { className: "shopping-view" });
    root.append(create("a", { className: "back-link", text: t("← Back to library"), attributes: { href: "#/" } }));
    root.append(create("div", { className: "shopping-heading" }, [
      create("div", {}, [
        create("p", { className: "eyebrow", text: t("Plan your week") }),
        create("h1", { text: t("Your shopping list") }),
        create("p", { className: "shopping-description", text: t("Choose the recipes you want to cook, set the servings, and keep your own pantry out of the cart.") }),
      ]),
      create("p", { className: "shopping-selection-count", text: `${shoppingItems().length} recipe${shoppingItems().length === 1 ? "" : "s"} selected` }),
    ]));

    if (state.shopping.loading) {
      root.append(renderLoading("Gathering your recipe library…"));
      return root;
    }
    if (state.shopping.error) {
      root.append(renderStatus({
        title: t("The shopping builder could not load your recipes."),
        message: state.shopping.error,
        error: true,
        actionLabel: "Try again",
        action: "retry-shopping",
      }));
      return root;
    }
    if (!state.shopping.recipes.length) {
      root.append(renderStatus({
        title: t("Add recipes before building a list."),
        message: t("Once your library has a recipe, you can choose servings and combine its ingredients here."),
        actionLabel: "Open library",
        action: "go-library",
      }));
      return root;
    }

    const selected = shoppingItems();
    const pantryId = "shopping-pantry";
    const recipeChoices = create("div", { className: "shopping-recipe-choices", attributes: { "aria-label": "Choose recipes for your shopping list" } }, state.shopping.recipes.map((recipe) => renderShoppingRecipeChoice(recipe)));
    const form = create("form", { className: "shopping-form", attributes: { id: "shopping-list-form" } }, [
      create("section", { className: "shopping-panel shopping-recipes-panel" }, [
        create("div", { className: "shopping-panel-heading" }, [
          create("h2", { text: t("Choose recipes") }),
          create("p", { text: t("Select as many recipes as you plan to cook, then set each serving count.") }),
        ]),
        recipeChoices,
      ]),
      create("section", { className: "shopping-panel shopping-pantry-panel" }, [
        create("div", { className: "shopping-panel-heading" }, [
          create("h2", { text: t("Already in your pantry?") }),
          create("p", { text: t("Optional. Leave out ingredients you already have at home.") }),
        ]),
        create("label", { className: "field-label", text: t("Pantry ingredients"), attributes: { for: pantryId } }),
        create("input", {
          className: "text-input",
          attributes: { id: pantryId, type: "text", value: state.shopping.pantry, placeholder: "e.g. salt, olive oil, black pepper", autocomplete: "off" },
          dataset: { shoppingRole: "pantry" },
        }),
        create("p", { className: "field-hint", text: t("Separate ingredients with commas. Spaces are normalized to canonical IDs automatically.") }),
        create("p", {
          className: `form-status${state.shopping.statusTone === "success" ? " is-success" : ""}`,
          text: state.shopping.status,
          attributes: { "data-shopping-status": "", "aria-live": "polite" },
        }),
        create("div", { className: "form-actions shopping-actions" }, [
          button(t("Clear basket"), "clear-shopping-basket", { className: "button button-secondary", disabled: selected.length === 0 || state.shopping.submitting }),
          create("button", {
            className: "button button-primary",
            text: state.shopping.submitting ? "Building list…" : "Build shopping list",
            attributes: { type: "submit", disabled: selected.length === 0 || state.shopping.submitting },
          }),
        ]),
      ]),
    ]);
    root.append(form);

    if (state.shopping.result) root.append(renderShoppingResults(state.shopping.result));
    return root;
  }

  function shoppingSelectionFor(id) {
    return state.shopping.selections.find((selection) => selection.id === id) || null;
  }

  function shoppingItems() {
    return state.shopping.selections.map((selection) => ({
      recipe_id: selection.id,
      servings: normalizeShoppingServings(selection.servings),
    }));
  }

  function renderShoppingRecipeChoice(recipe) {
    const id = stringValue(recipe && recipe.id);
    const title = stringValue(recipe && recipe.title, "Untitled recipe");
    const selection = shoppingSelectionFor(id);
    const selected = Boolean(selection);
    const defaultServings = normalizeShoppingServings(recipe && recipe.servings);
    const servings = selected ? normalizeShoppingServings(selection.servings, defaultServings) : defaultServings;
    const stableId = id.replace(/[^A-Za-z0-9_-]/g, "");
    const toggleId = `shopping-select-${stableId}`;
    const servingsId = `shopping-servings-${stableId}`;
    return create("article", { className: `shopping-recipe-choice${selected ? " is-selected" : ""}` }, [
      create("div", { className: "shopping-recipe-toggle" }, [
        create("input", {
          attributes: { id: toggleId, type: "checkbox", checked: selected },
          dataset: { shoppingRole: "recipe-toggle", recipeId: id, recipeServings: defaultServings },
        }),
        create("label", { className: "shopping-recipe-label", attributes: { for: toggleId } }, [
          create("strong", { text: title }),
          create("span", { text: `Recipe serves ${defaultServings}` }),
        ]),
      ]),
      create("label", { className: "shopping-servings-field", attributes: { for: servingsId } }, [
        create("span", { text: t("Cook for") }),
        create("input", {
          attributes: { id: servingsId, type: "number", min: "1", max: "99", step: "1", inputmode: "numeric", value: servings, disabled: !selected, "aria-label": `Servings for ${title}` },
          dataset: { shoppingRole: "recipe-servings", recipeId: id, recipeServings: defaultServings },
        }),
      ]),
    ]);
  }

  function renderShoppingResults(result) {
    const basic = safeArray(result && result.basic_groceries);
    const specialty = safeArray(result && result.specialty_items);
    const missing = safeArray(result && result.missing_recipes).map((id) => stringValue(id)).filter(Boolean);
    const count = basic.length + specialty.length;
    const root = create("section", { className: "shopping-results", attributes: { "aria-labelledby": "shopping-results-title" } }, [
      create("div", { className: "shopping-results-heading" }, [
        create("div", {}, [
          create("p", { className: "eyebrow", text: t("Ready to shop") }),
          create("h2", { text: t("Your grocery list"), attributes: { id: "shopping-results-title" } }),
        ]),
        create("p", { className: "shopping-results-count", text: `${count} ingredient${count === 1 ? "" : "s"}` }),
      ]),
      count === 0 ? create("p", { className: "shopping-empty", text: t("Everything for these recipes is already in your pantry.") }) : null,
      renderShoppingBucket("basic", "Everyday groceries", "Your local supermarket", basic),
      renderShoppingBucket("specialty", "Specialty items", "Find these at a specialist or online", specialty),
      missing.length ? create("aside", { className: "shopping-missing", attributes: { role: "status" } }, [
        create("strong", { text: t("Some recipes are no longer on this node.") }),
        create("span", { text: `${missing.length} saved selection${missing.length === 1 ? "" : "s"} could not be included.` }),
      ]) : null,
    ]);
    return root;
  }

  function renderShoppingBucket(bucket, title, description, lines) {
    const section = create("section", { className: `shopping-bucket shopping-bucket-${bucket}`, attributes: { "aria-labelledby": `shopping-${bucket}-title` } }, [
      create("div", { className: "shopping-bucket-heading" }, [
        create("div", {}, [
          create("h3", { text: title, attributes: { id: `shopping-${bucket}-title` } }),
          create("p", { text: description }),
        ]),
        create("span", { className: "shopping-bucket-count", text: `${lines.length}` }),
      ]),
    ]);
    if (!lines.length) {
      section.append(create("p", { className: "shopping-bucket-empty", text: t("Nothing in this bucket right now.") }));
      return section;
    }
    const list = create("ul", { className: "shopping-line-list" });
    lines.forEach((line, index) => list.append(renderShoppingLine(bucket, line, index)));
    section.append(list);
    return section;
  }

  function shoppingLineKey(bucket, line) {
    return `${bucket}:${stringValue(line && line.canonical_id)}:${stringValue(line && line.unit)}`;
  }

  function shoppingLineName(line) {
    const localized = stringValue(line && line.canonical_label);
    if (localized) return localized;
    return stringValue(line && line.canonical_id, "Unlabelled ingredient").replace(/_/g, " ").replace(/\b\w/g, (letter) => letter.toUpperCase());
  }

  function shoppingLineAmount(line) {
    return formatIngredientAmount({ metric_amount: line && line.total_amount, unit: line && line.unit });
  }

  function renderShoppingLine(bucket, line, index) {
    const key = shoppingLineKey(bucket, line);
    const checked = state.shopping.checked[key] === true;
    const id = `shopping-check-${bucket}-${index}`;
    const recipeCount = Math.max(0, Math.trunc(Number(line && line.recipe_count) || 0));
    const examples = safeArray(line && line.raw_examples).map((example) => String(example).trim()).filter(Boolean);
    return create("li", { className: `shopping-line${checked ? " is-checked" : ""}` }, [
      create("input", {
        attributes: { id, type: "checkbox", checked, "aria-label": `Mark ${shoppingLineName(line)} as bought` },
        dataset: { shoppingRole: "line-check", shoppingKey: key },
      }),
      create("label", { className: "shopping-line-copy", attributes: { for: id } }, [
        create("span", { className: "shopping-line-name", text: shoppingLineName(line) }),
        create("span", { className: "shopping-line-context", text: `${recipeCount} recipe${recipeCount === 1 ? "" : "s"}${examples.length ? ` · ${examples[0]}` : ""}` }),
      ]),
      create("strong", { className: "shopping-line-amount", text: shoppingLineAmount(line) }),
    ]);
  }

  function renderPagination() {
    const pageCount = libraryPageCount();
    if (pageCount <= 1) return null;
    const page = state.library.page;
    return create("nav", { className: "pagination", attributes: { "aria-label": "Recipe pages" } }, [
      create("p", { className: "pagination-summary", text: `Page ${page} of ${pageCount}`, attributes: { "aria-live": "polite" } }),
      create("div", { className: "pagination-actions" }, [
        button(t("Previous"), "previous-page", {
          className: "button button-secondary",
          ariaLabel: "Go to previous recipe page",
          disabled: page === 1,
        }),
        button(t("Next"), "next-page", {
          className: "button button-primary",
          ariaLabel: "Go to next recipe page",
          disabled: page === pageCount,
        }),
      ]),
    ]);
  }

  function renderSearchPanel() {
    const form = create("form", { className: "search-panel search-form", attributes: { id: "search-form", role: "search" } });
    form.append(
      searchField("Search recipes", "query", "Dish, cuisine, or ingredient", state.filters.query),
      searchField("Tag", "tag", "e.g. weeknight", state.filters.tag),
      searchField("Pantry ingredient", "ingredient", "e.g. chickpeas", state.filters.ingredient),
      create("div", { className: "search-actions" }, [
        create("button", { className: "button button-primary", text: t("Search"), attributes: { type: "submit" } }),
        button(t("Clear"), "clear-filters", { className: "button button-secondary", ariaLabel: "Clear recipe search filters" }),
      ]),
    );
    return form;
  }

  function searchField(label, name, placeholder, value) {
    const id = `search-${name}`;
    return create("label", { className: "search-field", attributes: { for: id } }, [
      create("span", { className: "field-label", text: label }),
      create("input", {
        className: "text-input",
        attributes: { id, name, type: "search", placeholder, autocomplete: "off", value },
      }),
    ]);
  }

  function renderRecipeCard(recipe) {
    const title = stringValue(recipe.title, "Untitled recipe");
    const card = create("a", {
      className: "recipe-card",
      attributes: { href: `#/recipe/${encodeURIComponent(recipe.id || "")}`, "aria-label": `Open ${title}` },
    });
    const cover = renderCover(recipe.image_path, title, "recipe-card-cover");
    const metadata = create("div", { className: "recipe-meta" }, [
      create("span", { text: `${positiveNumber(recipe.servings, 0) || "—"} servings` }),
      recipe.total_time_minutes !== null && recipe.total_time_minutes !== undefined
        ? create("span", { text: formatMinutes(recipe.total_time_minutes) })
        : create("span", { text: t("Time flexible") }),
    ]);
    const tags = Array.isArray(recipe.tags) && recipe.tags.length
      ? create("div", { className: "tag-list" }, recipe.tags.slice(0, 3).map((tag) => create("span", { className: "tag", text: tag })))
      : create("span", { className: "tag", text: `${Array.isArray(recipe.canonical_ingredients) ? recipe.canonical_ingredients.length : 0} ingredients` });

    card.append(cover, create("div", { className: "recipe-card-body" }, [
      create("h3", { className: "recipe-card-title", text: title }),
      metadata,
      tags,
    ]));
    return card;
  }

  function renderCover(imagePath, title, className) {
    const cover = create("div", { className });
    const source = imageUrl(imagePath);
    if (source) {
      cover.append(create("img", { attributes: { src: source, alt: `Cover for ${title}`, loading: "lazy" } }));
    } else {
      cover.append(create("div", { className: "recipe-card-placeholder", text: t("Home-cooked goodness") }));
    }
    return cover;
  }

  function languageName(language) {
    const code = stringValue(language, "source");
    try {
      return new Intl.DisplayNames([state.language], { type: "language" }).of(code) || code;
    } catch {
      return code.toUpperCase();
    }
  }

  function displayedTranslation(recipe) {
    if (recipe && recipe.translation) return recipe.translation;
    if (recipe && state.sourceViewRecipeId === recipe.id) return state.sourceTranslation;
    return null;
  }

  function renderTranslationNotice(recipe, translation) {
    if (!translation) return null;
    const viewingSource = state.sourceViewRecipeId === recipe.id;
    const origin = translation.origin === "human" ? t("human correction") : t("machine translation");
    const description = t("Translated from {language} · {origin}", {
      language: languageName(recipe.language),
      origin,
    });
    return create("aside", { className: `translation-notice${viewingSource ? " is-source" : ""}`, attributes: { role: "status" } }, [
      create("div", { className: "translation-notice-copy" }, [
        create("strong", { text: description }),
        viewingSource ? create("span", { text: t("Showing the original wording.") }) : null,
        translation.origin === "human" && translation.stale ? create("span", { className: "translation-stale", text: t("Source changed since this correction.") }) : null,
      ]),
      create("div", { className: "translation-notice-actions" }, [
        button(viewingSource ? t("Show translation") : t("Show original"), viewingSource ? "show-translation" : "show-original", {
          className: "button button-secondary button-small",
          dataset: { id: recipe.id },
        }),
        button(t("Fix translation"), "edit-translation", {
          className: "button button-secondary button-small",
          dataset: { id: recipe.id },
        }),
      ]),
    ]);
  }

  function renderRecipe(recipe) {
    const title = stringValue(recipe.title, "Untitled recipe");
    const translation = displayedTranslation(recipe);
    const root = create("section", { className: "recipe-view" });
    root.append(create("a", { className: "back-link", text: t("← Back to library"), attributes: { href: "#/" } }));

    const translationNotice = renderTranslationNotice(recipe, translation);
    if (translationNotice) root.append(translationNotice);

    const summary = create("div", { className: "recipe-summary" }, [
      create("div", { className: "tag-list" }, safeArray(recipe.tags).map((tag) => create("span", { className: "tag", text: tag }))),
      create("h1", { text: title }),
      recipe.description ? create("p", { className: "recipe-description", text: recipe.description }) : null,
      create("div", { className: "recipe-detail-meta" }, recipeMetadata(recipe)),
      create("div", { className: "recipe-actions" }, [
        button(t("Start Cook Mode"), "open-cook", { className: "button button-coral", dataset: { id: recipe.id } }),
        button(t("Edit recipe"), "edit-recipe", { className: "button button-secondary", dataset: { id: recipe.id } }),
        button(t("Delete recipe"), "request-delete", { className: "button button-danger", dataset: { id: recipe.id } }),
      ]),
    ]);
    root.append(create("div", { className: "recipe-hero" }, [renderCover(recipe.image_path, title, "recipe-cover-large"), summary]));

    const ingredients = create("section", { className: "recipe-section" });
    const servingInput = create("input", {
      attributes: {
        type: "number",
        min: "1",
        max: "99",
        step: "1",
        value: positiveNumber(recipe.servings, 1),
        inputmode: "numeric",
        "data-role": "servings",
        "aria-label": "Recipe servings",
      },
    });
    ingredients.append(
      create("div", { className: "section-heading" }, [
        create("h2", { text: t("Ingredients") }),
        create("label", { className: "serving-control" }, [create("span", { text: t("Serves") }), servingInput]),
      ]),
      renderIngredients(recipe.ingredients),
    );

    const instructions = create("section", { className: "recipe-section" }, [
      create("div", { className: "section-heading" }, [create("h2", { text: t("Method") })]),
      renderInstructions(recipe.instructions),
    ]);
    if (safeHttpUrl(recipe.origin_url)) {
      instructions.append(create("p", { className: "recipe-source" }, [
        "Original recipe: ",
        create("a", { text: t("open source"), attributes: { href: recipe.origin_url, target: "_blank", rel: "noopener noreferrer" } }),
      ]));
    }

    root.append(create("div", { className: "recipe-layout" }, [ingredients, instructions]));
    return root;
  }

  function renderTranslationEditor(recipe) {
    const translation = recipe.translation;
    const root = create("section", { className: "editor-view translation-editor-view" });
    root.append(create("a", { className: "back-link", text: t("← Back to recipe"), attributes: { href: `#/recipe/${encodeURIComponent(recipe.id)}` } }));

    const form = create("form", {
      className: "recipe-editor-form translation-editor-form",
      attributes: { id: "translation-editor-form", "data-recipe-id": recipe.id, "data-translation-lang": translation.lang },
    });
    form.append(create("div", { className: "editor-heading" }, [
      create("div", {}, [
        create("p", { className: "eyebrow", text: t("Translation correction") }),
        create("h1", { text: t("Fix translation") }),
        create("p", { className: "editor-description", text: t("Review the displayed recipe wording. Quantities, units, and recipe structure remain unchanged.") }),
      ]),
    ]));

    const titleId = editorFieldId("translation-title");
    const descriptionId = editorFieldId("translation-description");
    const tagsId = editorFieldId("translation-tags");
    form.append(create("section", { className: "editor-section" }, [
      create("div", { className: "editor-section-heading" }, [
        create("h2", { text: t("Recipe details") }),
        create("p", { text: t("These wording changes become a human correction and are kept when the source recipe changes.") }),
      ]),
      create("div", { className: "editor-grid" }, [
        editorField(t("Translation title"), editorTextInput({ id: titleId, value: stringValue(recipe.title), required: true, dataset: { translationField: "title" } })),
        create("div", { className: "editor-field editor-field-wide" }, [
          create("label", { className: "field-label", text: t("Translated description"), attributes: { for: descriptionId } }),
          editorTextArea({ id: descriptionId, value: recipe.description || "", rows: 4, dataset: { translationField: "description" } }),
        ]),
        editorField(t("Translated tags"), editorTextInput({ id: tagsId, value: safeArray(recipe.tags).join(", "), dataset: { translationField: "tags" } }), t("Separate tags with commas.")),
      ]),
    ]));

    const ingredientRows = create("div", { className: "editor-rows" }, safeArray(recipe.ingredients).map((item, index) => {
      const id = editorFieldId("translation-ingredient");
      return create("fieldset", { className: "editor-row translation-editor-row", attributes: { "data-translation-ingredient": "" } }, [
        create("legend", { className: "editor-row-title", text: t("Translated ingredient {number}", { number: index + 1 }) }),
        editorField(t("Ingredient wording"), editorTextArea({ id, value: stringValue(item && item.raw_text), required: true, rows: 2, dataset: { translationField: "ingredient" } })),
      ]);
    }));
    form.append(create("section", { className: "editor-section" }, [
      create("div", { className: "editor-section-heading" }, [create("h2", { text: t("Ingredients") }), create("p", { text: t("Keep every ingredient in the same order as the recipe.") })]),
      ingredientRows,
    ]));

    const instructionRows = create("div", { className: "editor-rows" }, safeArray(recipe.instructions).map((step, index) => {
      const id = editorFieldId("translation-step");
      return create("fieldset", { className: "editor-row translation-editor-row", attributes: { "data-translation-instruction": "" } }, [
        create("legend", { className: "editor-row-title", text: t("Translated step {number}", { number: index + 1 }) }),
        editorField(t("Instruction wording"), editorTextArea({ id, value: stringValue(step && step.text), required: true, rows: 3, dataset: { translationField: "instruction" } })),
      ]);
    }));
    form.append(create("section", { className: "editor-section" }, [
      create("div", { className: "editor-section-heading" }, [create("h2", { text: t("Method") }), create("p", { text: t("Keep every step in the same order as the recipe.") })]),
      instructionRows,
    ]));

    form.append(create("p", { className: "form-status editor-status", attributes: { "data-translation-status": "", "aria-live": "polite" } }), create("div", { className: "form-actions editor-actions" }, [
      button(t("Cancel"), "cancel-translation", { className: "button button-secondary" }),
      create("button", { className: "button button-primary", text: t("Save correction"), attributes: { type: "submit" } }),
    ]));
    root.append(form);
    return root;
  }

  function editorFieldId(prefix) {
    state.editorFieldSequence += 1;
    return `editor-${prefix}-${state.editorFieldSequence}`;
  }

  function numberValue(value) {
    if (value === null || value === undefined || value === "") return "";
    const number = Number(value);
    return Number.isFinite(number) ? String(number) : "";
  }

  function editorField(label, control, hint = null) {
    const id = control.getAttribute("id");
    const content = [create("label", { className: "field-label", text: label, attributes: id ? { for: id } : {} }), control];
    if (hint) content.push(create("p", { className: "field-hint", text: hint }));
    return create("div", { className: "editor-field" }, content);
  }

  function editorTextInput({ id, value = "", type = "text", placeholder = "", required = false, attributes = {}, dataset = {} }) {
    return create("input", {
      className: "text-input",
      attributes: { id, type, value, placeholder, required, ...attributes },
      dataset,
    });
  }

  function editorTextArea({ id, value = "", placeholder = "", required = false, rows = 4, dataset = {} }) {
    return create("textarea", {
      className: "text-input text-area editor-text-area",
      text: value,
      attributes: { id, placeholder, required, rows },
      dataset,
    });
  }

  function renderRecipeEditor(recipe, mode) {
    const isEdit = mode === "edit";
    const root = create("section", { className: "editor-view" });
    const backPath = isEdit ? `#/recipe/${encodeURIComponent(recipe.id)}` : "#/";
    root.append(create("a", { className: "back-link", text: isEdit ? "← Back to recipe" : "← Back to library", attributes: { href: backPath } }));

    const form = create("form", {
      className: "recipe-editor-form",
      attributes: { id: "recipe-editor-form", "data-editor-mode": mode, "data-recipe-id": isEdit ? recipe.id : "" },
    });
    form.append(create("div", { className: "editor-heading" }, [
      create("div", {}, [
        create("p", { className: "eyebrow", text: isEdit ? "Refine a saved recipe" : "Build a recipe from scratch" }),
        create("h1", { text: isEdit ? `Edit ${stringValue(recipe.title, "recipe")}` : "Create a recipe" }),
        create("p", { className: "editor-description", text: t("Keep the original wording alongside the metric details that make your recipe easy to shop and cook.") }),
      ]),
      isEdit && recipe.image_path ? renderCover(recipe.image_path, stringValue(recipe.title, "Recipe"), "editor-cover-preview") : null,
    ]));

    const titleId = editorFieldId("title");
    const descriptionId = editorFieldId("description");
    const servingsId = editorFieldId("servings");
    const prepId = editorFieldId("prep");
    const cookId = editorFieldId("cook");
    const tagsId = editorFieldId("tags");
    const originId = editorFieldId("origin");
    const coverId = editorFieldId("cover");
    const basics = create("section", { className: "editor-section" }, [
      create("div", { className: "editor-section-heading" }, [create("h2", { text: t("Recipe details") }), create("p", { text: t("The essentials for your library.") })]),
      create("div", { className: "editor-grid" }, [
        editorField("Recipe title", editorTextInput({ id: titleId, value: stringValue(recipe.title), placeholder: "e.g. Weeknight tomato pasta", required: true, dataset: { editorField: "title" } })),
        editorField("Servings", editorTextInput({ id: servingsId, type: "number", value: positiveNumber(recipe.servings, 1), required: true, attributes: { min: "1", max: "99", step: "1", inputmode: "numeric" }, dataset: { editorField: "servings" } })),
        create("div", { className: "editor-field editor-field-wide" }, [
          create("label", { className: "field-label", text: t("Description"), attributes: { for: descriptionId } }),
          editorTextArea({ id: descriptionId, value: recipe.description || "", placeholder: "A short note about this recipe", rows: 4, dataset: { editorField: "description" } }),
        ]),
        editorField("Prep time (minutes)", editorTextInput({ id: prepId, type: "number", value: numberValue(recipe.prep_time_minutes), attributes: { min: "0", max: "65535", step: "1", inputmode: "numeric" }, dataset: { editorField: "prep" } }), "Leave blank if it varies."),
        editorField("Cook time (minutes)", editorTextInput({ id: cookId, type: "number", value: numberValue(recipe.cook_time_minutes), attributes: { min: "0", max: "65535", step: "1", inputmode: "numeric" }, dataset: { editorField: "cook" } }), "Leave blank if it varies."),
        editorField("Tags", editorTextInput({ id: tagsId, value: safeArray(recipe.tags).join(", "), placeholder: "e.g. pasta, weeknight", dataset: { editorField: "tags" } }), "Separate tags with commas."),
        editorField("Original link", editorTextInput({ id: originId, type: "url", value: recipe.origin_url || "", placeholder: "https://example.com/original-recipe", attributes: { inputmode: "url" }, dataset: { editorField: "originUrl" } }), "Optional. Kept as your recipe source."),
      ]),
      create("label", { className: "check-field editor-public-field" }, [
        create("input", { attributes: { type: "checkbox", checked: recipe.is_public === true }, dataset: { editorField: "isPublic" } }),
        create("span", { text: t("Make this recipe public for future federation") }),
      ]),
    ]);

    const coverSection = create("section", { className: "editor-section editor-cover-section" }, [
      create("div", { className: "editor-section-heading" }, [create("h2", { text: t("Cover image") }), create("p", { text: isEdit ? "Choose a new image to replace the current cover." : "Choose an image now; it uploads after the recipe is saved." })]),
      editorField("Recipe cover", create("input", {
        className: "file-input",
        attributes: { id: coverId, type: "file", accept: "image/jpeg,image/png,image/webp", "aria-label": "Recipe cover image", "aria-describedby": `${coverId}-hint` },
        dataset: { editorField: "cover" },
      }), "JPEG, PNG, or WebP. The node converts the saved cover to WebP."),
    ]);
    coverSection.querySelector(".field-hint").id = `${coverId}-hint`;

    const ingredientRows = create("div", { className: "editor-rows", attributes: { id: "editor-ingredients", "aria-live": "polite" } });
    const ingredients = safeArray(recipe.ingredients);
    (ingredients.length ? ingredients : [emptyRecipeDraft().ingredients[0]]).forEach((item) => ingredientRows.append(renderIngredientEditorRow(item)));
    const ingredientSection = create("section", { className: "editor-section" }, [
      create("div", { className: "editor-section-heading editor-section-heading-action" }, [
        create("div", {}, [create("h2", { text: t("Ingredients") }), create("p", { text: t("Store the original text and its normalized shopping details.") })]),
        button(t("Add ingredient"), "add-editor-ingredient", { className: "button button-secondary button-small" }),
      ]),
      ingredientRows,
    ]);

    const instructionRows = create("div", { className: "editor-rows", attributes: { id: "editor-instructions", "aria-live": "polite" } });
    const instructions = safeArray(recipe.instructions);
    (instructions.length ? instructions : [emptyRecipeDraft().instructions[0]]).forEach((step, index) => instructionRows.append(renderInstructionEditorRow(step, index + 1)));
    const instructionSection = create("section", { className: "editor-section" }, [
      create("div", { className: "editor-section-heading editor-section-heading-action" }, [
        create("div", {}, [create("h2", { text: t("Method") }), create("p", { text: t("Add each cooking step in the order you perform it.") })]),
        button(t("Add step"), "add-editor-instruction", { className: "button button-secondary button-small" }),
      ]),
      instructionRows,
    ]);

    form.append(basics, coverSection, ingredientSection, instructionSection, create("p", { className: "form-status editor-status", attributes: { "data-editor-status": "", "aria-live": "polite" } }), create("div", { className: "form-actions editor-actions" }, [
      button(t("Cancel"), "cancel-editor", { className: "button button-secondary" }),
      create("button", { className: "button button-primary", text: isEdit ? "Save changes" : "Create recipe", attributes: { type: "submit" } }),
    ]));
    root.append(form);
    return root;
  }

  function renderIngredientEditorRow(item) {
    const rawId = editorFieldId("ingredient-raw");
    const amountId = editorFieldId("ingredient-amount");
    const unitId = editorFieldId("ingredient-unit");
    const canonicalId = editorFieldId("ingredient-canonical");
    const unit = ["g", "ml", "pcs", "to_taste"].includes(item && item.unit) ? item.unit : "g";
    const row = create("fieldset", { className: "editor-row ingredient-editor-row", attributes: { "data-ingredient-row": "" } });
    const unitSelect = create("select", { className: "text-input", attributes: { id: unitId }, dataset: { editorField: "unit", editorRole: "ingredient-unit" } }, [
      ["g", "grams"], ["ml", "millilitres"], ["pcs", "pieces"], ["to_taste", "to taste"],
    ].map(([value, label]) => create("option", { text: label, attributes: { value, selected: value === unit } })));
    row.append(
      create("legend", { className: "editor-row-title", text: t("Ingredient") }),
      button(t("Remove ingredient"), "remove-editor-ingredient", { className: "button button-tertiary button-small editor-remove", ariaLabel: "Remove this ingredient" }),
      create("div", { className: "editor-row-grid ingredient-editor-grid" }, [
        editorField("Original ingredient text", editorTextInput({ id: rawId, value: stringValue(item && item.raw_text), placeholder: "e.g. 2 ripe tomatoes", required: true, dataset: { editorField: "rawText" } })),
        editorField("Amount", editorTextInput({ id: amountId, type: "number", value: numberValue(item && item.metric_amount), placeholder: "e.g. 250", attributes: { min: "0", step: "any", inputmode: "decimal", disabled: unit === "to_taste" }, dataset: { editorField: "metricAmount", editorRole: "ingredient-amount" } })),
        editorField("Unit", unitSelect),
        editorField("Canonical ID", editorTextInput({ id: canonicalId, value: stringValue(item && item.canonical_id), placeholder: "e.g. ripe_tomatoes", required: true, attributes: { pattern: "[a-z0-9_]+" }, dataset: { editorField: "canonicalId" } }), "Use lowercase letters, numbers, and underscores."),
        create("label", { className: "check-field editor-specialty-field" }, [
          create("input", { attributes: { type: "checkbox", checked: item && item.is_specialty === true }, dataset: { editorField: "isSpecialty" } }),
          create("span", { text: t("Specialty ingredient") }),
        ]),
      ]),
    );
    return row;
  }

  function renderInstructionEditorRow(step, position) {
    const textId = editorFieldId("instruction-text");
    const timerId = editorFieldId("instruction-timer");
    const row = create("fieldset", { className: "editor-row instruction-editor-row", attributes: { "data-instruction-row": "" } });
    row.append(
      create("legend", { className: "editor-row-title", text: `Step ${position}` }),
      button(t("Remove step"), "remove-editor-instruction", { className: "button button-tertiary button-small editor-remove", ariaLabel: `Remove step ${position}` }),
      create("div", { className: "editor-row-grid instruction-editor-grid" }, [
        create("div", { className: "editor-field editor-field-wide" }, [
          create("label", { className: "field-label", text: t("Instruction"), attributes: { for: textId } }),
          editorTextArea({ id: textId, value: stringValue(step && step.text), placeholder: "Describe what to do in this step", required: true, rows: 3, dataset: { editorField: "text" } }),
        ]),
        editorField("Timer (seconds)", editorTextInput({ id: timerId, type: "number", value: numberValue(step && step.timer_seconds), placeholder: "e.g. 600", attributes: { min: "1", max: "4294967295", step: "1", inputmode: "numeric" }, dataset: { editorField: "timerSeconds" } }), "Optional. Cook Mode uses this timer."),
      ]),
    );
    return row;
  }

  function recipeMetadata(recipe) {
    const parts = [];
    if (positiveNumber(recipe.servings, 0)) parts.push(create("span", { text: `${recipe.servings} servings` }));
    if (recipe.prep_time_minutes !== null && recipe.prep_time_minutes !== undefined) parts.push(create("span", { text: `${formatMinutes(recipe.prep_time_minutes)} prep` }));
    if (recipe.cook_time_minutes !== null && recipe.cook_time_minutes !== undefined) parts.push(create("span", { text: `${formatMinutes(recipe.cook_time_minutes)} cook` }));
    if (!parts.length) parts.push(create("span", { text: t("Saved to your recipe node") }));
    return parts;
  }

  function renderIngredients(ingredients) {
    const list = create("ul", { className: "ingredient-list" });
    const items = safeArray(ingredients);
    if (!items.length) {
      list.append(create("li", { className: "status-label", text: t("No ingredients were extracted for this recipe.") }));
      return list;
    }
    items.forEach((item) => {
      const amount = formatIngredientAmount(item);
      const canonical = stringValue(item.canonical_id, "unclassified").replaceAll("_", " ");
      const canonicalLabel = stringValue(item.canonical_label, canonical);
      const labels = [create("span", { text: canonicalLabel })];
      if (canonicalLabel !== canonical) labels.push(create("span", { text: canonical }));
      if (item.is_specialty) labels.push(create("span", { className: "tag tag-specialty", text: t("specialty") }));
      list.append(create("li", { className: "ingredient" }, [
        create("span", { className: "ingredient-amount", text: amount }),
        create("div", { className: "ingredient-copy" }, [
          create("p", { className: "ingredient-name", text: stringValue(item.raw_text, canonical) }),
          create("div", { className: "ingredient-canonical" }, labels),
        ]),
      ]));
    });
    return list;
  }

  function renderInstructions(instructions) {
    const list = create("ol", { className: "instruction-list" });
    const steps = safeArray(instructions);
    if (!steps.length) {
      list.append(create("li", { className: "status-label", text: t("No cooking steps were extracted for this recipe.") }));
      return list;
    }
    steps.forEach((step, index) => {
      const number = positiveNumber(step.step_number, index + 1);
      list.append(create("li", { className: "instruction" }, [
        create("span", { className: "step-number", text: number }),
        create("div", {}, [
          create("p", { text: stringValue(step.text, "No instruction provided.") }),
          timerSeconds(step) ? create("span", { className: "timer-hint", text: `Timer: ${formatCountdown(timerSeconds(step))}` }) : null,
        ]),
      ]));
    });
    return list;
  }

  function ensureCookSession(recipe) {
    if (state.cook && state.cook.recipeId === recipe.id) return;
    stopCookSession();
    state.cook = {
      recipeId: recipe.id,
      stepIndex: 0,
      remainingSeconds: null,
      running: false,
      interval: null,
      wakeLock: null,
    };
  }

  function renderCookMode(recipe) {
    ensureCookSession(recipe);
    const steps = safeArray(recipe.instructions);
    const cook = state.cook;
    const root = create("section", { className: "cook-shell" });
    root.append(create("div", { className: "cook-topbar" }, [
      create("a", { className: "back-link", text: t("← Recipe"), attributes: { href: `#/recipe/${encodeURIComponent(recipe.id)}` } }),
      create("p", { className: "cook-title", text: recipe.title || "Cook Mode" }),
    ]));

    if (!steps.length) {
      root.append(renderStatus({
        title: t("This recipe has no steps yet."),
        message: t("Return to the recipe to review the imported details."),
        actionLabel: "Back to recipe",
        action: "back-to-recipe",
      }));
      return root;
    }

    const index = Math.min(Math.max(cook.stepIndex, 0), steps.length - 1);
    cook.stepIndex = index;
    const step = steps[index];
    const progress = ((index + 1) / steps.length) * 100;
    root.append(create("div", { className: "cook-progress", attributes: { "aria-label": `Step ${index + 1} of ${steps.length}` } }, [
      create("span", { attributes: { style: `width: ${progress}%` } }),
    ]));

    const content = create("div", { className: "cook-card" }, [
      create("p", { className: "cook-eyebrow", text: `Step ${index + 1} of ${steps.length}` }),
      create("h1", { className: "cook-step-title", text: `Step ${positiveNumber(step.step_number, index + 1)}` }),
      create("p", { className: "cook-step-text", text: stringValue(step.text, "No instruction provided.") }),
    ]);

    const duration = timerSeconds(step);
    if (duration) content.append(renderTimer(duration));
    content.append(renderCookNavigation(recipe.id, index, steps.length));
    root.append(content);
    return root;
  }

  function renderTimer(duration) {
    const remaining = state.cook.remainingSeconds === null ? duration : state.cook.remainingSeconds;
    const isRunning = state.cook.running;
    return create("section", { className: "timer-card", attributes: { "aria-label": "Step timer" } }, [
      create("div", {}, [
        create("p", { className: "timer-label", text: t("Step timer") }),
        create("p", { className: "timer-display", text: formatCountdown(remaining), attributes: { "data-timer-display": "" } }),
      ]),
      create("div", { className: "timer-actions" }, [
        button(isRunning ? "Pause" : remaining <= 0 ? "Start again" : "Start timer", "toggle-timer", {
          className: "button button-primary button-small",
          dataset: { duration },
          attributes: { "data-timer-toggle": "" },
        }),
        button(t("Reset"), "reset-timer", { className: "button button-secondary button-small", dataset: { duration } }),
      ]),
    ]);
  }

  function renderCookNavigation(recipeId, index, count) {
    return create("div", { className: "cook-navigation" }, [
      button(t("Previous"), "previous-step", { className: "button button-secondary", disabled: index === 0 }),
      create("span", { className: "cook-count", text: `${index + 1} / ${count}` }),
      button(index + 1 === count ? "Finish cooking" : "Next step", index + 1 === count ? "finish-cook" : "next-step", {
        className: "button button-coral",
        dataset: { id: recipeId },
      }),
    ]);
  }

  function renderLoading(label) {
    return create("section", { className: "status-panel" }, [
      create("p", { className: "eyebrow", text: t("Chef's Prodigy") }),
      create("h2", { text: label }),
      create("p", { text: t("Connecting to your local recipe node…") }),
    ]);
  }

  function renderSkeletonGrid() {
    const grid = create("div", { className: "skeleton-grid", attributes: { "aria-label": "Loading recipes", "aria-busy": "true" } });
    for (let index = 0; index < 6; index += 1) grid.append(create("div", { className: "skeleton-card" }));
    return grid;
  }

  function renderStatus({ title, message, error = false, actionLabel, action }) {
    return create("section", { className: `status-panel${error ? " status-panel-error" : ""}`, attributes: { role: error ? "alert" : "status" } }, [
      create("p", { className: "eyebrow", text: error ? "A small kitchen hiccup" : "Chef's Prodigy" }),
      create("h2", { text: title }),
      create("p", { text: message }),
      actionLabel && action ? button(actionLabel, action, { className: error ? "button button-coral" : "button button-primary" }) : null,
    ]);
  }

  function collectionTitle() {
    const filtered = state.filters.query || state.filters.tag || state.filters.ingredient;
    return filtered ? "Filtered recipes" : "Your recipe library";
  }

  function collectionMeta() {
    if (state.library.loading && !state.library.items.length) return "Finding your recipes…";
    const count = state.library.items.length;
    if (count === 0) return "Ready when you are";
    const total = state.library.total;
    const start = state.library.offset + 1;
    const end = Math.min(state.library.offset + count, total);
    return `Showing ${start}–${end} of ${total} recipe${total === 1 ? "" : "s"}`;
  }

  function libraryPageCount() {
    const limit = Math.max(1, state.library.limit);
    return Math.max(1, Math.ceil(state.library.total / limit));
  }

  function imageUrl(imagePath) {
    const match = /^images\/([A-Za-z0-9_-]+\.webp)$/.exec(String(imagePath || ""));
    return match ? `/images/${encodeURIComponent(match[1])}` : null;
  }

  function stringValue(value, fallback = "") {
    return typeof value === "string" && value.trim() ? value.trim() : fallback;
  }

  function safeArray(value) {
    return Array.isArray(value) ? value : [];
  }

  function positiveNumber(value, fallback) {
    const number = Number(value);
    return Number.isFinite(number) && number > 0 ? number : fallback;
  }

  function timerSeconds(step) {
    const number = Number(step && step.timer_seconds);
    return Number.isInteger(number) && number > 0 ? number : null;
  }

  function formatMinutes(value) {
    const minutes = Number(value);
    if (!Number.isFinite(minutes) || minutes <= 0) return t("Time flexible");
    if (minutes < 60) {
      return new Intl.NumberFormat(state.language, { style: "unit", unit: "minute", unitDisplay: "short", maximumFractionDigits: 0 }).format(Math.round(minutes));
    }
    const hours = Math.floor(minutes / 60);
    const remainder = Math.round(minutes % 60);
    const hourLabel = new Intl.NumberFormat(state.language, { style: "unit", unit: "hour", unitDisplay: "short", maximumFractionDigits: 0 }).format(hours);
    if (!remainder) return hourLabel;
    const minuteLabel = new Intl.NumberFormat(state.language, { style: "unit", unit: "minute", unitDisplay: "short", maximumFractionDigits: 0 }).format(remainder);
    return `${hourLabel} ${minuteLabel}`;
  }

  function formatCountdown(seconds) {
    const total = Math.max(0, Math.floor(Number(seconds) || 0));
    const minutes = Math.floor(total / 60);
    const remainder = total % 60;
    const digits = new Intl.NumberFormat(state.language, { minimumIntegerDigits: 2, useGrouping: false });
    return `${digits.format(minutes)}:${digits.format(remainder)}`;
  }

  function formatIngredientAmount(item) {
    const rawAmount = Number(item && item.metric_amount);
    return formatUnit(rawAmount, item && item.unit);
  }

  function safeHttpUrl(value) {
    try {
      const url = new URL(value);
      return url.protocol === "https:" || url.protocol === "http:";
    } catch {
      return false;
    }
  }

  function updateInstallAppButton() {
    if (installAppButton) installAppButton.hidden = !state.installPrompt;
  }

  function captureInstallPrompt(event) {
    if (!event || typeof event.prompt !== "function") return;
    event.preventDefault();
    state.installPrompt = event;
    updateInstallAppButton();
  }

  async function promptInstall() {
    const prompt = state.installPrompt;
    if (!prompt) return;
    state.installPrompt = null;
    updateInstallAppButton();
    try {
      await prompt.prompt();
      const choice = await prompt.userChoice;
      showToast(choice && choice.outcome === "accepted" ? "Chef's Prodigy is installing." : "Install prompt dismissed. You can install it whenever you are ready.");
    } catch {
      showToast("Your browser could not open the install prompt. Try the browser menu instead.", "error");
    }
  }

  function openImportDialog() {
    document.querySelectorAll(".form-status").forEach((status) => {
      status.textContent = "";
      status.classList.remove("is-success");
    });
    clearMealieReport();
    resetPhotoImport();
    setImportTab("url");
    if (typeof importDialog.showModal === "function") {
      if (!importDialog.open) importDialog.showModal();
    } else {
      importDialog.setAttribute("open", "");
    }
    window.setTimeout(() => document.querySelector("#import-url")?.focus(), 0);
  }

  function closeImportDialog() {
    resetPhotoImport();
    if (typeof importDialog.close === "function" && importDialog.open) importDialog.close();
    else importDialog.removeAttribute("open");
  }

  function setImportTab(tab) {
    const tabs = ["url", "text", "photo", "mealie"];
    const selected = tabs.includes(tab) ? tab : "url";
    tabs.forEach((name) => {
      const tabButton = document.querySelector(`#${name}-tab`);
      const panel = document.querySelector(`#${name}-import-panel`);
      if (!tabButton || !panel) return;
      const isSelected = name === selected;
      tabButton.setAttribute("aria-selected", String(isSelected));
      tabButton.classList.toggle("is-active", isSelected);
      panel.hidden = !isSelected;
    });
  }

  function clearPhotoPreviews() {
    state.photoPreviewUrls.forEach((url) => URL.revokeObjectURL(url));
    state.photoPreviewUrls = [];
    document.querySelector("[data-photo-preview-list]")?.replaceChildren();
  }

  function resetPhotoImport() {
    const form = document.querySelector("#photo-import-form");
    const input = form?.querySelector("[data-role='photo-picker']");
    if (input instanceof HTMLInputElement) input.value = "";
    clearPhotoPreviews();
    const selection = document.querySelector("#photo-selection");
    const summary = document.querySelector("[data-photo-selection-summary]");
    const progress = document.querySelector("[data-photo-import-progress]");
    if (selection) selection.hidden = true;
    if (summary) summary.textContent = "";
    if (progress) progress.hidden = true;
  }

  function selectedPhotoFiles(input) {
    return input instanceof HTMLInputElement ? Array.from(input.files || []) : [];
  }

  function photoSelectionError(files) {
    if (!files.length) return "Choose at least one recipe photo.";
    if (files.length > MAX_PHOTO_COUNT) return `Choose no more than ${MAX_PHOTO_COUNT} photos at once.`;
    const oversized = files.find((file) => file.size > MAX_PHOTO_BYTES);
    if (oversized) return `${stringValue(oversized.name, "That photo")} is larger than 20 MB.`;
    const unsupported = files.find((file) => file.type && !file.type.startsWith("image/"));
    if (unsupported) return `${stringValue(unsupported.name, "That file")} is not an image.`;
    return "";
  }

  function updatePhotoSelection(input) {
    const files = selectedPhotoFiles(input);
    const selection = document.querySelector("#photo-selection");
    const summary = document.querySelector("[data-photo-selection-summary]");
    const previewList = document.querySelector("[data-photo-preview-list]");
    const status = document.querySelector('[data-form-status="photo"]');
    clearPhotoPreviews();

    if (!files.length) {
      if (selection) selection.hidden = true;
      if (summary) summary.textContent = "";
      if (status) status.textContent = "";
      return;
    }

    const selectionError = photoSelectionError(files);
    if (status) {
      status.textContent = selectionError;
      status.classList.remove("is-success");
    }
    if (selection) selection.hidden = false;
    if (summary) summary.textContent = `${files.length} photo${files.length === 1 ? "" : "s"} selected`;
    if (!previewList) return;

    files.forEach((file) => {
      const card = create("li", { className: "photo-preview-card" });
      try {
        const url = URL.createObjectURL(file);
        state.photoPreviewUrls.push(url);
        card.append(create("img", {
          className: "photo-preview-image",
          attributes: { src: url, alt: `Preview of ${stringValue(file.name, "selected photo")}`, loading: "lazy" },
        }));
      } catch {
        card.append(create("div", { className: "photo-preview-image", text: t("Preview unavailable") }));
      }
      card.append(create("span", { className: "photo-preview-name", text: stringValue(file.name, "Selected photo") }));
      previewList.append(card);
    });
  }

  function setPhotoImportProgress(form, active, message = "") {
    const progress = form.querySelector("[data-photo-import-progress]");
    const label = form.querySelector("[data-photo-import-progress-label]");
    if (!progress) return;
    progress.hidden = !active;
    if (label) label.textContent = message;
  }

  function openDeleteDialog(id) {
    if (!state.recipe || state.recipe.id !== id) return;
    state.pendingDelete = { id, title: stringValue(state.recipe.title, "this recipe") };
    document.querySelector("#delete-recipe-title").textContent = state.pendingDelete.title;
    const status = document.querySelector("#delete-dialog-status");
    status.textContent = "";
    status.classList.remove("is-success");
    deleteDialog.querySelectorAll("button").forEach((control) => { control.disabled = false; });

    if (typeof deleteDialog.showModal === "function") {
      if (!deleteDialog.open) deleteDialog.showModal();
    } else {
      deleteDialog.setAttribute("open", "");
    }
    window.setTimeout(() => deleteDialog.querySelector("[data-action='confirm-delete']")?.focus(), 0);
  }

  function closeDeleteDialog() {
    if (typeof deleteDialog.close === "function" && deleteDialog.open) deleteDialog.close();
    else deleteDialog.removeAttribute("open");
    state.pendingDelete = null;
  }

  async function confirmDelete() {
    const target = state.pendingDelete;
    if (!target) return;

    const status = document.querySelector("#delete-dialog-status");
    const controls = Array.from(deleteDialog.querySelectorAll("button"));
    controls.forEach((control) => { control.disabled = true; });
    status.textContent = "Deleting recipe…";
    status.classList.remove("is-success");

    try {
      await apiRequest(`/recipes/${encodeURIComponent(target.id)}`, { method: "DELETE" });
      state.recipe = null;
      state.detailServings = null;
      closeDeleteDialog();
      showToast(`Deleted ${target.title} from your library.`);
      navigate("/");
    } catch (error) {
      if (error.status === 404) {
        state.recipe = null;
        state.detailServings = null;
        closeDeleteDialog();
        showToast("This recipe was already deleted. Your library has been refreshed.", "error");
        navigate("/");
        return;
      }
      status.textContent = error.message;
      controls.forEach((control) => { control.disabled = false; });
    }
  }

  async function submitImport(form) {
    const kind = form.id === "url-import-form" ? "url" : "text";
    const status = document.querySelector(`[data-form-status="${kind}"]`);
    const submitButton = form.querySelector("button[type='submit']");
    const formData = new FormData(form);
    let path;
    let payload;

    if (kind === "url") {
      path = "/import/url";
      payload = { url: String(formData.get("url") || "").trim(), is_public: formData.get("isPublic") === "on" };
    } else {
      path = "/import/text";
      const originUrl = String(formData.get("originUrl") || "").trim();
      payload = {
        text: String(formData.get("text") || "").trim(),
        origin_url: originUrl || null,
        is_public: formData.get("isPublic") === "on",
      };
    }

    status.textContent = kind === "url" ? "Importing recipe from the link…" : "Extracting your recipe text…";
    status.classList.remove("is-success");
    submitButton.disabled = true;
    try {
      const recipe = await apiRequest(path, { method: "POST", body: JSON.stringify(payload) });
      status.textContent = "Recipe imported. Opening it now…";
      status.classList.add("is-success");
      form.reset();
      window.setTimeout(() => closeImportDialog(), 300);
      showToast(`Added ${stringValue(recipe.title, "your recipe")} to the library.`);
      navigate(`/recipe/${encodeURIComponent(recipe.id)}`);
    } catch (error) {
      status.textContent = error.message;
    } finally {
      submitButton.disabled = false;
    }
  }

  function clearMealieReport() {
    const report = document.querySelector("#mealie-import-report");
    report.replaceChildren();
    report.hidden = true;
  }

  function renderMealieReport(result) {
    const report = document.querySelector("#mealie-import-report");
    const imported = Math.max(0, Math.trunc(Number(result && result.imported_count) || 0));
    const failed = Math.max(0, Math.trunc(Number(result && result.failed_count) || 0));
    const errors = safeArray(result && result.errors).map((error) => String(error)).filter(Boolean);
    const summary = failed || errors.length ? "Backup import finished with notes." : "Backup import finished successfully.";
    const counts = create("ul", { className: "import-report-counts" }, [
      create("li", {}, [create("strong", { text: imported }), " imported"]),
      create("li", {}, [create("strong", { text: failed }), " failed"]),
    ]);
    const content = [
      create("h3", { text: t("Import report") }),
      create("p", { text: summary }),
      counts,
    ];
    if (errors.length) {
      content.push(create("ul", { className: "import-error-list" }, errors.map((error) => create("li", { text: error }))));
    }
    report.replaceChildren(create("div", { className: "import-report-card" }, content));
    report.hidden = false;
  }

  async function submitMealieBackup(form) {
    const status = document.querySelector('[data-form-status="mealie"]');
    const submitButton = form.querySelector("button[type='submit']");
    const fileInput = form.querySelector("input[type='file']");
    const file = fileInput.files && fileInput.files[0];
    if (!file) {
      status.textContent = "Choose a Mealie .zip backup first.";
      return;
    }
    if (!file.name.toLowerCase().endsWith(".zip")) {
      status.textContent = "Choose a .zip backup exported from Mealie.";
      return;
    }

    const upload = new FormData();
    upload.append("file", file, file.name);
    clearMealieReport();
    status.textContent = "Uploading and importing your backup…";
    status.classList.remove("is-success");
    submitButton.disabled = true;
    try {
      const result = await apiRequest("/import/mealie-backup", { method: "POST", body: upload });
      renderMealieReport(result);
      const failed = Math.max(0, Math.trunc(Number(result.failed_count) || 0));
      const imported = Math.max(0, Math.trunc(Number(result.imported_count) || 0));
      status.textContent = failed ? "Import completed with some failures. See the report below." : "Import completed. Your recipe library has been refreshed.";
      status.classList.toggle("is-success", failed === 0);
      form.reset();
      showToast(`Imported ${imported} recipe${imported === 1 ? "" : "s"}.`);
      if (state.route.name === "library") loadLibrary();
      else navigate("/");
    } catch (error) {
      status.textContent = error.message;
    } finally {
      submitButton.disabled = false;
    }
  }

  async function submitPhotoImport(form) {
    const status = form.querySelector('[data-form-status="photo"]');
    const submitButton = form.querySelector("button[type='submit']");
    const photoInput = form.querySelector("[data-role='photo-picker']");
    const files = selectedPhotoFiles(photoInput);
    const selectionError = photoSelectionError(files);
    if (selectionError) {
      status.textContent = selectionError;
      status.classList.remove("is-success");
      return;
    }

    const formData = new FormData(form);
    const upload = new FormData();
    files.forEach((file) => upload.append("photos", file, file.name || "recipe-photo"));
    upload.append("is_public", formData.get("isPublic") === "on" ? "true" : "false");

    status.textContent = `Analyzing ${files.length} photo${files.length === 1 ? "" : "s"}. This can take up to 20 seconds…`;
    status.classList.remove("is-success");
    setPhotoImportProgress(form, true, "Reading recipe text and choosing a cover photo…");
    submitButton.disabled = true;
    try {
      const recipe = await apiRequest("/import/photo", { method: "POST", body: upload });
      status.textContent = "Recipe imported. Opening it now…";
      status.classList.add("is-success");
      form.reset();
      clearPhotoPreviews();
      const selection = document.querySelector("#photo-selection");
      if (selection) selection.hidden = true;
      window.setTimeout(() => closeImportDialog(), 300);
      showToast(`Added ${stringValue(recipe.title, "your recipe")} from ${files.length} photo${files.length === 1 ? "" : "s"}.`);
      navigate(`/recipe/${encodeURIComponent(recipe.id)}`);
    } catch (error) {
      status.textContent = error.message;
    } finally {
      setPhotoImportProgress(form, false);
      submitButton.disabled = false;
    }
  }

  function editorStatus(form, message, success = false) {
    const status = form.querySelector("[data-editor-status]");
    if (!status) return;
    status.textContent = message;
    status.classList.toggle("is-success", success);
  }

  function editorFieldValue(container, name) {
    const element = container.querySelector(`[data-editor-field="${name}"]`);
    return element && "value" in element ? String(element.value || "").trim() : "";
  }

  function editorCheckboxValue(container, name) {
    const element = container.querySelector(`[data-editor-field="${name}"]`);
    return Boolean(element && element instanceof HTMLInputElement && element.checked);
  }

  function editorInteger(value, label, { required = false, minimum = 0, maximum = Number.MAX_SAFE_INTEGER } = {}) {
    if (!value) {
      if (required) throw new Error(`${label} is required.`);
      return null;
    }
    const number = Number(value);
    if (!Number.isInteger(number) || number < minimum || number > maximum) {
      throw new Error(`${label} must be a whole number between ${minimum} and ${maximum}.`);
    }
    return number;
  }

  function editorAmount(value, unit, label) {
    if (unit === "to_taste" || !value) return null;
    const number = Number(value);
    if (!Number.isFinite(number) || number <= 0) throw new Error(`${label} must be a positive number.`);
    return number;
  }

  function editorTags(value) {
    const seen = new Set();
    return value.split(",").map((tag) => tag.trim()).filter((tag) => {
      const key = tag.toLocaleLowerCase();
      if (!tag || seen.has(key)) return false;
      seen.add(key);
      return true;
    });
  }

  function editorPayload(form) {
    const title = editorFieldValue(form, "title");
    if (!title) throw new Error("Recipe title is required.");
    const servings = editorInteger(editorFieldValue(form, "servings"), "Servings", { required: true, minimum: 1, maximum: 99 });
    const originUrl = editorFieldValue(form, "originUrl");
    if (originUrl && !safeHttpUrl(originUrl)) throw new Error("Original link must begin with http:// or https://.");

    const ingredientRows = Array.from(form.querySelectorAll("[data-ingredient-row]"));
    if (!ingredientRows.length) throw new Error("Add at least one ingredient.");
    const ingredients = ingredientRows.map((row, index) => {
      const rawText = editorFieldValue(row, "rawText");
      const canonicalId = editorFieldValue(row, "canonicalId");
      const unit = editorFieldValue(row, "unit");
      if (!rawText) throw new Error(`Ingredient ${index + 1} needs its original text.`);
      if (!/^[a-z0-9_]+$/.test(canonicalId)) throw new Error(`Ingredient ${index + 1} needs a lowercase canonical ID using letters, numbers, or underscores.`);
      if (!["g", "ml", "pcs", "to_taste"].includes(unit)) throw new Error(`Ingredient ${index + 1} has an unsupported unit.`);
      return {
        raw_text: rawText,
        metric_amount: editorAmount(editorFieldValue(row, "metricAmount"), unit, `Ingredient ${index + 1} amount`),
        unit,
        canonical_id: canonicalId,
        is_specialty: editorCheckboxValue(row, "isSpecialty"),
      };
    });

    const instructionRows = Array.from(form.querySelectorAll("[data-instruction-row]"));
    if (!instructionRows.length) throw new Error("Add at least one cooking step.");
    const instructions = instructionRows.map((row, index) => {
      const text = editorFieldValue(row, "text");
      if (!text) throw new Error(`Step ${index + 1} needs an instruction.`);
      return {
        step_number: index + 1,
        text,
        timer_seconds: editorInteger(editorFieldValue(row, "timerSeconds"), `Step ${index + 1} timer`, { minimum: 1, maximum: 4294967295 }),
      };
    });

    return {
      title,
      description: editorFieldValue(form, "description") || null,
      servings,
      prep_time_minutes: editorInteger(editorFieldValue(form, "prep"), "Prep time", { maximum: 65535 }),
      cook_time_minutes: editorInteger(editorFieldValue(form, "cook"), "Cook time", { maximum: 65535 }),
      ingredients,
      instructions,
      tags: editorTags(editorFieldValue(form, "tags")),
      origin_url: originUrl || null,
      is_public: editorCheckboxValue(form, "isPublic"),
    };
  }

  async function uploadEditorCover(recipeId, file) {
    if (!file) return null;
    if (!file.type.startsWith("image/")) throw new Error("Choose a JPEG, PNG, or WebP image for the cover.");
    const upload = new FormData();
    upload.append("file", file, file.name || "cover-image");
    return apiRequest(`/recipes/${encodeURIComponent(recipeId)}/image`, { method: "PUT", body: upload });
  }

  async function submitRecipeEditor(form) {
    const status = form.querySelector("[data-editor-status]");
    const submitButton = form.querySelector("button[type='submit']");
    const mode = form.getAttribute("data-editor-mode") || "create";
    const existingId = form.getAttribute("data-recipe-id") || "";
    const coverInput = form.querySelector('[data-editor-field="cover"]');
    const cover = coverInput && coverInput instanceof HTMLInputElement && coverInput.files ? coverInput.files[0] : null;
    let savedRecipe = null;

    submitButton.disabled = true;
    try {
      const payload = editorPayload(form);
      editorStatus(form, mode === "edit" ? "Saving your changes…" : "Creating your recipe…");
      savedRecipe = await apiRequest(mode === "edit" ? `/recipes/${encodeURIComponent(existingId)}` : "/recipes", {
        method: mode === "edit" ? "PUT" : "POST",
        body: JSON.stringify(payload),
      });
      if (cover) {
        editorStatus(form, "Saving recipe details and uploading the cover…");
        savedRecipe = await uploadEditorCover(savedRecipe.id, cover);
      }
      state.recipe = savedRecipe;
      editorStatus(form, mode === "edit" ? "Changes saved. Opening your recipe…" : "Recipe created. Opening it now…", true);
      showToast(`${stringValue(savedRecipe.title, "Recipe")} is saved to your library.`);
      navigate(`/recipe/${encodeURIComponent(savedRecipe.id)}`);
    } catch (error) {
      if (savedRecipe) {
        state.recipe = savedRecipe;
        form.setAttribute("data-editor-mode", "edit");
        form.setAttribute("data-recipe-id", savedRecipe.id);
        editorStatus(form, `Recipe details were saved, but the cover could not be uploaded: ${error.message}`);
        showToast("Recipe saved, but its cover needs another try.", "error");
      } else {
        editorStatus(form, error.message);
      }
    } finally {
      submitButton.disabled = false;
      if (status && !status.textContent) editorStatus(form, "");
    }
  }

  function translationFieldValue(container, name) {
    const element = container.querySelector(`[data-translation-field="${name}"]`);
    return element && "value" in element ? String(element.value || "").trim() : "";
  }

  function translationPayload(form) {
    const title = translationFieldValue(form, "title");
    if (!title) throw new Error(t("Translation title is required."));
    const ingredients = Array.from(form.querySelectorAll("[data-translation-ingredient]")).map((row, index) => {
      const value = translationFieldValue(row, "ingredient");
      if (!value) throw new Error(t("Translated ingredient {number} is required.", { number: index + 1 }));
      return value;
    });
    const instructions = Array.from(form.querySelectorAll("[data-translation-instruction]")).map((row, index) => {
      const value = translationFieldValue(row, "instruction");
      if (!value) throw new Error(t("Translated step {number} is required.", { number: index + 1 }));
      return value;
    });
    return {
      title,
      description: translationFieldValue(form, "description") || null,
      ingredients,
      instructions,
      tags: editorTags(translationFieldValue(form, "tags")),
    };
  }

  async function submitTranslationEditor(form) {
    const id = stringValue(form.getAttribute("data-recipe-id"));
    const lang = stringValue(form.getAttribute("data-translation-lang"));
    const status = form.querySelector("[data-translation-status]");
    const submitButton = form.querySelector("button[type='submit']");
    if (!id || !lang) return;
    if (submitButton) submitButton.disabled = true;
    try {
      if (status) status.textContent = t("Saving correction…");
      const recipe = await apiRequest(`/recipes/${encodeURIComponent(id)}/translations/${encodeURIComponent(lang)}`, {
        method: "PUT",
        body: JSON.stringify(translationPayload(form)),
      });
      state.recipe = recipe;
      state.sourceViewRecipeId = null;
      state.sourceTranslation = null;
      if (status) {
        status.textContent = t("Correction saved. Opening your translated recipe…");
        status.classList.add("is-success");
      }
      showToast(t("Translation correction saved."));
      navigate(`/recipe/${encodeURIComponent(id)}`);
    } catch (error) {
      if (status) status.textContent = error.message;
    } finally {
      if (submitButton) submitButton.disabled = false;
    }
  }

  function showOriginalRecipe(id) {
    if (!state.recipe || state.recipe.id !== id || !state.recipe.translation) return;
    state.sourceTranslation = state.recipe.translation;
    state.sourceViewRecipeId = id;
    loadRecipe(id, false);
  }

  function showTranslatedRecipe(id) {
    state.sourceViewRecipeId = null;
    state.sourceTranslation = null;
    loadRecipe(id, false);
  }

  function cancelTranslationEditor() {
    if (state.route.name === "translation-editor" && state.route.id) navigate(`/recipe/${encodeURIComponent(state.route.id)}`);
  }

  function addEditorIngredient() {
    const rows = document.querySelector("#editor-ingredients");
    if (!rows) return;
    const row = renderIngredientEditorRow(emptyRecipeDraft().ingredients[0]);
    rows.append(row);
    row.querySelector("[data-editor-field='rawText']")?.focus();
  }

  function addEditorInstruction() {
    const rows = document.querySelector("#editor-instructions");
    if (!rows) return;
    const row = renderInstructionEditorRow(emptyRecipeDraft().instructions[0], rows.querySelectorAll("[data-instruction-row]").length + 1);
    rows.append(row);
    row.querySelector("[data-editor-field='text']")?.focus();
  }

  function renumberEditorInstructions() {
    document.querySelectorAll("#editor-instructions [data-instruction-row]").forEach((row, index) => {
      const title = row.querySelector(".editor-row-title");
      const remove = row.querySelector("[data-action='remove-editor-instruction']");
      if (title) title.textContent = `Step ${index + 1}`;
      if (remove) remove.setAttribute("aria-label", `Remove step ${index + 1}`);
    });
  }

  function syncIngredientAmount(row) {
    const unit = row.querySelector("[data-editor-role='ingredient-unit']");
    const amount = row.querySelector("[data-editor-role='ingredient-amount']");
    if (!(unit instanceof HTMLSelectElement) || !(amount instanceof HTMLInputElement)) return;
    const isToTaste = unit.value === "to_taste";
    amount.disabled = isToTaste;
    if (isToTaste) amount.value = "";
  }

  function cancelEditor() {
    if (state.route.name !== "editor") return;
    if (state.route.mode === "edit" && state.route.id) navigate(`/recipe/${encodeURIComponent(state.route.id)}`);
    else navigate("/");
  }

  function applySearch(form) {
    const values = new FormData(form);
    state.filters = {
      query: String(values.get("query") || "").trim(),
      tag: String(values.get("tag") || "").trim(),
      ingredient: String(values.get("ingredient") || "").trim(),
    };
    saveFilters();
    navigateLibraryPage(1);
  }

  function clearFilters() {
    state.filters = { query: "", tag: "", ingredient: "" };
    saveFilters();
    navigateLibraryPage(1);
  }

  function updateShoppingRecipeSelection(input) {
    const id = stringValue(input.dataset.recipeId);
    if (!id) return;
    if (input.checked) {
      const existing = shoppingSelectionFor(id);
      if (!existing) {
        state.shopping.selections.push({ id, servings: normalizeShoppingServings(input.dataset.recipeServings) });
      }
    } else {
      state.shopping.selections = state.shopping.selections.filter((selection) => selection.id !== id);
    }
    state.shopping.result = null;
    state.shopping.status = "";
    state.shopping.statusTone = "default";
    saveShoppingSelections();
    if (state.route.name === "shopping") setView(renderShopping());
  }

  function updateShoppingServings(input) {
    const id = stringValue(input.dataset.recipeId);
    const selection = shoppingSelectionFor(id);
    if (!selection) return;
    const entered = Math.round(Number(input.value));
    if (!Number.isInteger(entered) || entered < 1 || entered > 99) {
      input.value = String(selection.servings);
      showToast("Choose between 1 and 99 servings.", "error");
      return;
    }
    selection.servings = entered;
    state.shopping.result = null;
    state.shopping.status = "";
    state.shopping.statusTone = "default";
    saveShoppingSelections();
    if (state.route.name === "shopping") setView(renderShopping());
  }

  function updateShoppingPantry(input) {
    state.shopping.pantry = String(input.value || "").slice(0, 1000);
    saveShoppingPantry();
  }

  function normalizePantryIds(value) {
    const seen = new Set();
    return String(value || "").split(/[\n,;]+/).map((item) => item.trim().toLowerCase()
      .replace(/[\s-]+/g, "_")
      .replace(/[^a-z0-9_]/g, ""))
      .filter((item) => item && !seen.has(item) && (seen.add(item), true));
  }

  function toggleShoppingLine(input) {
    const key = stringValue(input.dataset.shoppingKey);
    if (!key) return;
    if (input.checked) state.shopping.checked[key] = true;
    else delete state.shopping.checked[key];
    saveShoppingChecks();
    const row = input.closest(".shopping-line");
    if (row) row.classList.toggle("is-checked", input.checked);
  }

  function clearShoppingBasket() {
    state.shopping.selections = [];
    state.shopping.result = null;
    state.shopping.status = "";
    state.shopping.statusTone = "default";
    saveShoppingSelections();
    if (state.route.name === "shopping") setView(renderShopping());
  }

  async function submitShoppingList(form) {
    const items = shoppingItems();
    const status = form.querySelector("[data-shopping-status]");
    const submitButton = form.querySelector("button[type='submit']");
    if (!items.length) {
      if (status) status.textContent = "Choose at least one recipe first.";
      return;
    }

    state.shopping.submitting = true;
    state.shopping.status = "";
    state.shopping.statusTone = "default";
    if (status) status.textContent = "Combining ingredients from your selected recipes…";
    if (submitButton) submitButton.disabled = true;
    try {
      const result = await apiRequest(`/shopping-list?lang=${encodeURIComponent(state.language)}`, {
        method: "POST",
        body: JSON.stringify({
          items,
          pantry_exclude_canonical_ids: normalizePantryIds(state.shopping.pantry),
        }),
      });
      state.shopping.result = result;
      state.shopping.status = "Your shopping list is ready.";
      state.shopping.statusTone = "success";
      state.shopping.submitting = false;
      if (state.route.name === "shopping") setView(renderShopping());
      showToast("Your shopping list is ready.");
    } catch (error) {
      state.shopping.result = null;
      state.shopping.status = error.message;
      state.shopping.statusTone = "default";
      state.shopping.submitting = false;
      if (state.route.name === "shopping") setView(renderShopping());
    }
  }

  function changeServings(input) {
    const servings = Math.round(Number(input.value));
    if (!Number.isInteger(servings) || servings < 1 || servings > 99 || !state.route.id) {
      input.value = String(state.detailServings || 1);
      showToast("Choose between 1 and 99 servings.", "error");
      return;
    }
    state.detailServings = servings;
    loadRecipe(state.route.id, false);
  }

  function changeCookStep(delta) {
    if (!state.cook || !state.recipe) return;
    const next = state.cook.stepIndex + delta;
    const count = safeArray(state.recipe.instructions).length;
    if (next < 0 || next >= count) return;
    stopTimer();
    state.cook.stepIndex = next;
    state.cook.remainingSeconds = null;
    setView(renderCookMode(state.recipe));
  }

  function finishCook() {
    if (!state.recipe) return;
    stopCookSession();
    showToast("Nicely done. Your recipe is waiting whenever you need it.");
    navigate(`/recipe/${encodeURIComponent(state.recipe.id)}`);
  }

  function toggleTimer(duration) {
    if (!state.cook) return;
    if (state.cook.running) {
      stopTimer();
      updateTimerUI(duration);
      return;
    }
    if (state.cook.remainingSeconds === null || state.cook.remainingSeconds <= 0) state.cook.remainingSeconds = duration;
    state.cook.running = true;
    requestWakeLock();
    state.cook.interval = window.setInterval(() => {
      if (!state.cook) return;
      state.cook.remainingSeconds = Math.max(0, (state.cook.remainingSeconds || 0) - 1);
      updateTimerUI(duration);
      if (state.cook.remainingSeconds <= 0) {
        stopTimer();
        updateTimerUI(duration);
        showToast("Step timer finished.");
      }
    }, 1000);
    updateTimerUI(duration);
  }

  function resetTimer(duration) {
    if (!state.cook) return;
    stopTimer();
    state.cook.remainingSeconds = duration;
    updateTimerUI(duration);
  }

  function stopTimer() {
    if (!state.cook) return;
    if (state.cook.interval !== null) window.clearInterval(state.cook.interval);
    state.cook.interval = null;
    state.cook.running = false;
  }

  function updateTimerUI(duration) {
    if (!state.cook) return;
    const remaining = state.cook.remainingSeconds === null ? duration : state.cook.remainingSeconds;
    const display = document.querySelector("[data-timer-display]");
    const toggle = document.querySelector("[data-timer-toggle]");
    if (display) display.textContent = formatCountdown(remaining);
    if (toggle) toggle.textContent = state.cook.running ? "Pause" : remaining <= 0 ? "Start again" : "Start timer";
  }

  async function requestWakeLock() {
    if (!state.cook || state.route.name !== "cook" || state.cook.wakeLock || !("wakeLock" in navigator)) return;
    try {
      const lock = await navigator.wakeLock.request("screen");
      if (!state.cook || state.route.name !== "cook") {
        await lock.release();
        return;
      }
      state.cook.wakeLock = lock;
      lock.addEventListener("release", () => {
        if (state.cook && state.cook.wakeLock === lock) state.cook.wakeLock = null;
      });
    } catch {
      // Cook Mode remains useful when Wake Lock is denied or unsupported.
    }
  }

  function stopCookSession() {
    if (!state.cook) return;
    stopTimer();
    const lock = state.cook.wakeLock;
    state.cook.wakeLock = null;
    state.cook = null;
    if (lock) lock.release().catch(() => undefined);
  }

  function onClick(event) {
    const target = event.target.closest("[data-action]");
    if (!target) return;
    const action = target.dataset.action;
    if (!action) return;

    if (action === "new-recipe") {
      event.preventDefault();
      navigate("/new");
    } else if (action === "open-shopping") {
      event.preventDefault();
      navigate("/shopping");
    } else if (action === "edit-recipe") {
      event.preventDefault();
      navigate(`/recipe/${encodeURIComponent(target.dataset.id || "")}/edit`);
    } else if (action === "edit-translation") {
      event.preventDefault();
      navigate(`/recipe/${encodeURIComponent(target.dataset.id || "")}/translation`);
    } else if (action === "show-original") {
      event.preventDefault();
      showOriginalRecipe(target.dataset.id || "");
    } else if (action === "show-translation") {
      event.preventDefault();
      showTranslatedRecipe(target.dataset.id || "");
    } else if (action === "cancel-editor") {
      event.preventDefault();
      cancelEditor();
    } else if (action === "cancel-translation") {
      event.preventDefault();
      cancelTranslationEditor();
    } else if (action === "add-editor-ingredient") {
      event.preventDefault();
      addEditorIngredient();
    } else if (action === "remove-editor-ingredient") {
      event.preventDefault();
      target.closest("[data-ingredient-row]")?.remove();
    } else if (action === "add-editor-instruction") {
      event.preventDefault();
      addEditorInstruction();
    } else if (action === "remove-editor-instruction") {
      event.preventDefault();
      target.closest("[data-instruction-row]")?.remove();
      renumberEditorInstructions();
    } else if (action === "open-import") {
      event.preventDefault();
      openImportDialog();
    } else if (action === "close-import") {
      event.preventDefault();
      closeImportDialog();
    } else if (action === "request-delete") {
      event.preventDefault();
      openDeleteDialog(target.dataset.id || "");
    } else if (action === "cancel-delete") {
      event.preventDefault();
      closeDeleteDialog();
    } else if (action === "confirm-delete") {
      event.preventDefault();
      confirmDelete();
    } else if (action === "switch-import-tab") {
      event.preventDefault();
      setImportTab(target.dataset.tab);
    } else if (action === "clear-filters") {
      event.preventDefault();
      clearFilters();
    } else if (action === "retry-library") {
      event.preventDefault();
      loadLibrary();
    } else if (action === "retry-shopping") {
      event.preventDefault();
      loadShopping();
    } else if (action === "clear-shopping-basket") {
      event.preventDefault();
      clearShoppingBasket();
    } else if (action === "previous-page") {
      event.preventDefault();
      navigateLibraryPage(state.library.page - 1);
    } else if (action === "next-page") {
      event.preventDefault();
      navigateLibraryPage(state.library.page + 1);
    } else if (action === "install-app") {
      event.preventDefault();
      promptInstall();
    } else if (action === "go-library") {
      event.preventDefault();
      navigate("/");
    } else if (action === "open-cook") {
      event.preventDefault();
      navigate(`/recipe/${encodeURIComponent(target.dataset.id || "")}/cook`);
    } else if (action === "back-to-recipe") {
      event.preventDefault();
      if (state.recipe) navigate(`/recipe/${encodeURIComponent(state.recipe.id)}`);
    } else if (action === "previous-step") {
      event.preventDefault();
      changeCookStep(-1);
    } else if (action === "next-step") {
      event.preventDefault();
      changeCookStep(1);
    } else if (action === "finish-cook") {
      event.preventDefault();
      finishCook();
    } else if (action === "toggle-timer") {
      event.preventDefault();
      toggleTimer(Number(target.dataset.duration));
    } else if (action === "reset-timer") {
      event.preventDefault();
      resetTimer(Number(target.dataset.duration));
    }
  }

  function onSubmit(event) {
    const form = event.target;
    if (!(form instanceof HTMLFormElement)) return;
    if (form.id === "search-form") {
      event.preventDefault();
      applySearch(form);
    } else if (form.id === "shopping-list-form") {
      event.preventDefault();
      submitShoppingList(form);
    } else if (form.id === "recipe-editor-form") {
      event.preventDefault();
      if (!form.reportValidity()) return;
      submitRecipeEditor(form);
    } else if (form.id === "translation-editor-form") {
      event.preventDefault();
      if (!form.reportValidity()) return;
      submitTranslationEditor(form);
    } else if (form.id === "url-import-form" || form.id === "text-import-form" || form.id === "photo-import-form" || form.id === "mealie-import-form") {
      event.preventDefault();
      if (!form.reportValidity()) return;
      if (form.id === "photo-import-form") submitPhotoImport(form);
      else if (form.id === "mealie-import-form") submitMealieBackup(form);
      else submitImport(form);
    }
  }

  function onInput(event) {
    const element = event.target;
    if (!(element instanceof HTMLInputElement)) return;
    if (element.dataset.role === "servings") {
      changeServings(element);
      return;
    }
    if (element.dataset.shoppingRole === "pantry") {
      updateShoppingPantry(element);
      return;
    }
    if (!element.closest("#search-form")) return;
    if (state.searchTimer !== null) window.clearTimeout(state.searchTimer);
    state.searchTimer = window.setTimeout(() => {
      const form = document.querySelector("#search-form");
      if (form) applySearch(form);
    }, 450);
  }

  function onChange(event) {
    const element = event.target;
    if (element instanceof HTMLInputElement && element.dataset.role === "photo-picker") {
      updatePhotoSelection(element);
      return;
    }
    if (element instanceof HTMLInputElement && element.dataset.shoppingRole === "recipe-toggle") {
      updateShoppingRecipeSelection(element);
      return;
    }
    if (element instanceof HTMLInputElement && element.dataset.shoppingRole === "recipe-servings") {
      updateShoppingServings(element);
      return;
    }
    if (element instanceof HTMLInputElement && element.dataset.shoppingRole === "line-check") {
      toggleShoppingLine(element);
      return;
    }
    if (!(element instanceof HTMLSelectElement) || element.dataset.editorRole !== "ingredient-unit") return;
    const row = element.closest("[data-ingredient-row]");
    if (row) syncIngredientAmount(row);
  }

  document.addEventListener("click", onClick);
  document.addEventListener("submit", onSubmit);
  document.addEventListener("input", onInput);
  document.addEventListener("change", onChange);
  window.addEventListener("hashchange", handleRoute);
  if (languageSelect) {
    languageSelect.addEventListener("change", () => {
      setLanguage(languageSelect.value);
    });
  }
  window.addEventListener("online", () => showToast("Back online. Your recipe node is reachable again."));
  window.addEventListener("offline", () => showToast("You are offline. Recipe data needs your local node connection.", "error"));
  window.addEventListener("beforeinstallprompt", captureInstallPrompt);
  window.addEventListener("appinstalled", () => {
    state.installPrompt = null;
    updateInstallAppButton();
    showToast("Chef's Prodigy was installed on this device.");
  });
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible" && state.route.name === "cook") requestWakeLock();
  });
  importDialog.addEventListener("click", (event) => {
    if (event.target === importDialog) closeImportDialog();
  });
  deleteDialog.addEventListener("click", (event) => {
    if (event.target === deleteDialog) closeDeleteDialog();
  });
  deleteDialog.addEventListener("cancel", () => {
    state.pendingDelete = null;
  });

  if ("serviceWorker" in navigator) {
    window.addEventListener("load", () => {
      navigator.serviceWorker.register("/sw.js").catch(() => undefined);
    });
  }

  async function initialize() {
    try {
      await Promise.all([loadBundle("en"), loadBundle(state.language)]);
    } catch {
      state.language = "en";
      try {
        await loadBundle("en");
      } catch {
        // Static English markup remains available when an offline first load cannot fetch a bundle.
      }
    }
    applyStaticTranslations();
    updateInstallAppButton();
    setImportTab("url");
    handleRoute();
  }

  initialize();
})();
