// Lightweight language preference helper
// Stores the selected language in localStorage and keeps query parameters in sync
(function (win, doc) {
  "use strict";

  const STORAGE_KEY = "jb.lang";
  const QUERY_KEY = "lang";
  const ALLOWED = ["en", "fr", "es", "uk"];
  const ALLOWED_SET = new Set(ALLOWED);
  let mutationObserver = null;
  let autoRewriteScheduled = false;

  function debugLog(message, details) {
    if (!win.DEBUG_LOGS) return;
    try {
      console.debug("[lang]", message, details ?? "");
    } catch (_) {}
  }

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

  function setDocumentLang(lang) {
    const sanitized = sanitize(lang);
    if (!sanitized) return;
    try {
      const root = doc.documentElement;
      if (root && root.lang !== sanitized) {
        root.lang = sanitized;
      }
    } catch (_) {}
  }

  function resolveLang() {
    return getUrlLang() || getStored() || sanitize(doc.documentElement.lang) || null;
  }

  function snapshotState() {
    const urlLang = getUrlLang();
    const storedLang = getStored();
    const rawDocLang = doc.documentElement ? doc.documentElement.lang : null;
    const documentLang = sanitize(rawDocLang) || rawDocLang || null;
    return {
      urlLang,
      storedLang,
      documentLang,
      resolvedLang: urlLang || storedLang || documentLang || null,
      search: win.location.search,
      readyState: doc.readyState,
    };
  }

  function logState(context) {
    if (!win.DEBUG_LOGS) return;
    debugLog(context || "state", snapshotState());
  }

  function ensureLanguage({ rewrite = true } = {}) {
    const url = new URL(win.location.href);
    const urlLang = sanitize(url.searchParams.get(QUERY_KEY));
    if (urlLang) {
      setStored(urlLang);
      setDocumentLang(urlLang);
      if (rewrite) {
        rewriteLinks();
      }
      debugLog("ensureLanguage:urlLang", { rewrite, urlLang });
      return urlLang;
    }
    const stored = getStored();
    if (stored) {
      setDocumentLang(stored);
      url.searchParams.set(QUERY_KEY, stored);
      navigate(url.toString(), "replace");
      debugLog("ensureLanguage:stored", { stored });
      return stored;
    }
    if (rewrite) {
      rewriteLinks();
    }
    logState("ensureLanguage:fallback");
    return null;
  }

  function applyLanguage(lang, { replace = false } = {}) {
    const sanitized = setStored(lang);
    if (!sanitized) return null;
    setDocumentLang(sanitized);
    const url = new URL(win.location.href);
    debugLog("applyLanguage", { sanitized, replace });
    if (url.searchParams.get(QUERY_KEY) === sanitized) {
      rewriteLinks();
      return sanitized;
    }
    url.searchParams.set(QUERY_KEY, sanitized);
    navigate(url.toString(), replace ? "replace" : "assign");
    return sanitized;
  }

  function rewriteLinks(root) {
    const lang = resolveLang();
    if (!lang) return;
    debugLog("rewriteLinks:start", {
      lang,
      scope: root && root !== doc ? root.nodeName || "fragment" : "document",
    });
    const base = root || doc;
    const anchors = [];
    if (
      base &&
      base.nodeType === 1 &&
      typeof base.matches === "function" &&
      base.matches("a[href]")
    ) {
      anchors.push(base);
    }
    if (base && typeof base.querySelectorAll === "function") {
      base.querySelectorAll("a[href]").forEach((anchor) => anchors.push(anchor));
    }
    const seen = new Set();
    anchors.forEach((anchor) => {
      if (!anchor || seen.has(anchor)) return;
      seen.add(anchor);
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

  function enableAutoRewrite() {
    if (!("MutationObserver" in win)) {
      debugLog("enableAutoRewrite:unsupported");
      return;
    }
    if (!doc.body) {
      if (autoRewriteScheduled) return;
      autoRewriteScheduled = true;
      doc.addEventListener(
        "DOMContentLoaded",
        () => {
          autoRewriteScheduled = false;
          enableAutoRewrite();
        },
        { once: true }
      );
      debugLog("enableAutoRewrite:waitForBody");
      return;
    }
    debugLog("enableAutoRewrite:attached");
    mutationObserver = new MutationObserver((mutations) => {
      if (mutations.length) {
        debugLog("enableAutoRewrite:mutation", { batches: mutations.length });
      }
      mutations.forEach((mutation) => {
        mutation.addedNodes.forEach((node) => {
          if (!node || node.nodeType !== 1) return;
          rewriteLinks(node);
        });
      });
    });
    mutationObserver.observe(doc.body, { childList: true, subtree: true });
  }

  const api = {
    allowed: () => ALLOWED.slice(),
    sanitize,
    getStored,
    applyLanguage,
    ensureLanguage,
    rewriteLinks,
    syncSelect,
    current: () => resolveLang() || "en",
    enableAutoRewrite,
    inspect: snapshotState,
    logState,
  };

  win.JBLang = api;

  const applied = ensureLanguage({ rewrite: false });
  if (applied) {
    setDocumentLang(applied);
  } else {
    setDocumentLang(resolveLang() || "en");
    rewriteLinks();
  }
  logState("init");
  const onReady = () => {
    rewriteLinks();
    enableAutoRewrite();
    logState("ready");
  };
  if (doc.readyState === "loading") {
    doc.addEventListener("DOMContentLoaded", onReady);
  } else {
    onReady();
  }
})(window, document);
