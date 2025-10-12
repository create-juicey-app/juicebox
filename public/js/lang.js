// Lightweight language preference helper
// Stores the selected language in localStorage and keeps query parameters in sync
(function (win, doc) {
  "use strict";

  const STORAGE_KEY = "jb.lang";
  const QUERY_KEY = "lang";
  const ALLOWED = ["en", "fr", "es", "uk"];
  const ALLOWED_SET = new Set(ALLOWED);

  const navigate =
    typeof win.__JBLangNavigate === "function"
      ? (url, mode) => {
          try {
            win.__JBLangNavigate(url, mode);
          } catch (err) {
            if (win.DEBUG_LOGS) {
              console.warn("[lang] custom navigate failed", err);
            }
          }
        }
      : (url, mode) => {
          if (mode === "replace") {
            win.location.replace(url);
          } else {
            win.location.assign(url);
          }
        };

  function sanitize(raw) {
    if (typeof raw !== "string") return null;
    const trimmed = raw.trim().toLowerCase();
    return ALLOWED_SET.has(trimmed) ? trimmed : null;
  }

  function getStored() {
    try {
      const stored =
        win.localStorage.getItem(STORAGE_KEY) ||
        win.localStorage.getItem("lang");
      return sanitize(stored);
    } catch (err) {
      if (win.DEBUG_LOGS) {
        console.warn("[lang] Unable to read localStorage", err);
      }
      return null;
    }
  }

  function setStored(lang) {
    const sanitized = sanitize(lang);
    if (!sanitized) return null;
    try {
      win.localStorage.setItem(STORAGE_KEY, sanitized);
    } catch (err) {
      if (win.DEBUG_LOGS) {
        console.warn("[lang] Unable to write localStorage", err);
      }
    }
    return sanitized;
  }

  function getUrlLang() {
    try {
      return sanitize(new URL(win.location.href).searchParams.get(QUERY_KEY));
    } catch (_) {
      return null;
    }
  }

  function ensureLanguage({ rewrite = true } = {}) {
    const url = new URL(win.location.href);
    const urlLang = sanitize(url.searchParams.get(QUERY_KEY));
    if (urlLang) {
      setStored(urlLang);
      if (rewrite) {
        rewriteLinks();
      }
      return urlLang;
    }
    const stored = getStored();
    if (stored) {
      url.searchParams.set(QUERY_KEY, stored);
      navigate(url.toString(), "replace");
      return stored;
    }
    if (rewrite) {
      rewriteLinks();
    }
    return null;
  }

  function applyLanguage(lang, { replace = false } = {}) {
    const sanitized = setStored(lang);
    if (!sanitized) return null;
    const url = new URL(win.location.href);
    if (url.searchParams.get(QUERY_KEY) === sanitized) {
      rewriteLinks();
      return sanitized;
    }
    url.searchParams.set(QUERY_KEY, sanitized);
    navigate(url.toString(), replace ? "replace" : "assign");
    return sanitized;
  }

  function rewriteLinks(root) {
    const lang = getStored() || getUrlLang();
    if (!lang) return;
    const base = root || doc;
    const anchors = base.querySelectorAll("a[href]");
    anchors.forEach((anchor) => {
      if (anchor.dataset.langSkip === "true") return;
      const href = anchor.getAttribute("href");
      if (!href || href.startsWith("#")) return;
      const lowered = href.trim().toLowerCase();
      if (
        lowered.startsWith("mailto:") ||
        lowered.startsWith("tel:") ||
        lowered.startsWith("javascript:")
      ) {
        return;
      }
      let target;
      try {
        target = new URL(href, win.location.origin);
      } catch (_) {
        return;
      }
      if (target.origin !== win.location.origin) return;
      if (target.searchParams.get(QUERY_KEY) === lang) return;
      target.searchParams.set(QUERY_KEY, lang);
      const serialized = target.pathname + target.search + target.hash;
      anchor.setAttribute("href", serialized);
    });
  }

  function syncSelect(select) {
    if (!select) return;
    const current = getUrlLang() || getStored();
    if (current && select.value !== current) {
      const option = Array.from(select.options).find(
        (opt) => opt.value === current
      );
      if (option) select.value = current;
    }
    select.addEventListener("change", () => {
      applyLanguage(select.value);
    });
  }

  const api = {
    allowed: () => ALLOWED.slice(),
    sanitize,
    getStored,
    applyLanguage,
    ensureLanguage,
    rewriteLinks,
    syncSelect,
  };

  win.JBLang = api;

  const applied = ensureLanguage({ rewrite: false });
  if (!applied) {
    rewriteLinks();
  }
  const readyState = doc.readyState;
  if (readyState === "loading") {
    doc.addEventListener("DOMContentLoaded", () => rewriteLinks());
  } else {
    rewriteLinks();
  }
})(window, document);
