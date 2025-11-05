/**
 * i18n-owned.js
 * Centralized, dependency-free localization map for "owned" UI strings.
 *
 * Usage (ESM):
 *   import { tOwned, currentLang, OWNED_STRINGS, allowedLanguages } from "./i18n-owned.js";
 *   const label = tOwned("empty_title"); // Uses current page language detection
 *
 * Tests can import OWNED_STRINGS to avoid hardcoding expected strings.
 */

"use strict";

/**
 * Languages supported by the Owned UI strings.
 */
export const allowedLanguages = Object.freeze(["en", "fr", "es", "uk"]);

/**
 * Translation tables for Owned UI.
 * Add any future keys here to keep strings centralized.
 */
export const OWNED_STRINGS = Object.freeze({
  en: Object.freeze({
    expired: "expired",
    empty_title: "No files found ):",
    empty_hint: "Your uploads will land here once they finish.",
  }),
  fr: Object.freeze({
    expired: "expiré",
    empty_title: "Aucun fichier trouvé ):",
    empty_hint: "Vos envois apparaîtront ici lorsqu’ils seront terminés.",
  }),
  es: Object.freeze({
    expired: "expirado",
    empty_title: "No se encontraron archivos ):",
    empty_hint: "Tus subidas aparecerán aquí cuando finalicen.",
  }),
  uk: Object.freeze({
    expired: "прострочено",
    empty_title: "Файлів не знайдено ):",
    empty_hint: "Ваші завантаження з’являться тут, щойно завершаться.",
  }),
});

/**
 * Known translation keys for Owned UI.
 */
export const OWNED_KEYS = Object.freeze(Object.keys(OWNED_STRINGS.en));

/**
 * Resolve a language code to one of the supported values.
 * - Prefers `window.JBLang.current()` when available.
 * - Falls back to `<html lang>` if present.
 * - Defaults to "en" when unknown.
 */
export function currentLang() {
  try {
    if (
      typeof window !== "undefined" &&
      window.JBLang &&
      typeof window.JBLang.current === "function"
    ) {
      const cur = String(window.JBLang.current() || "").toLowerCase();
      if (allowedLanguages.includes(cur)) return cur;
    }
  } catch {
    // ignore and try documentElement.lang
  }

  try {
    if (
      typeof document !== "undefined" &&
      document.documentElement &&
      document.documentElement.lang
    ) {
      const lang = String(document.documentElement.lang).toLowerCase();
      if (allowedLanguages.includes(lang)) return lang;
    }
  } catch {
    // ignore and fall through to default
  }

  return "en";
}

/**
 * Get the translation table for a specific or current language.
 * @param {string} [lang] e.g., "en", "fr", "es", "uk"
 * @returns {Record<string,string>}
 */
export function getOwnedStrings(lang) {
  const code = lang && allowedLanguages.includes(lang) ? lang : currentLang();
  const base = OWNED_STRINGS[code] || OWNED_STRINGS.en;

  // If templates exposed localized strings on window.T, prefer them.
  // Expected keys: owned_expired, owned_empty_title, owned_empty_hint
  try {
    const T = typeof window !== "undefined" ? window.T : null;
    if (T && typeof T === "object") {
      const expired =
        typeof T.owned_expired === "string" && T.owned_expired.trim()
          ? T.owned_expired
          : null;
      const emptyTitle =
        typeof T.owned_empty_title === "string" && T.owned_empty_title.trim()
          ? T.owned_empty_title
          : null;
      const emptyHint =
        typeof T.owned_empty_hint === "string" && T.owned_empty_hint.trim()
          ? T.owned_empty_hint
          : null;

      if (expired || emptyTitle || emptyHint) {
        return Object.freeze({
          ...base,
          ...(expired ? { expired } : null),
          ...(emptyTitle ? { empty_title: emptyTitle } : null),
          ...(emptyHint ? { empty_hint: emptyHint } : null),
        });
      }
    }
  } catch {
    // ignore and fall back to static table
  }

  return base;
}

/**
 * Translate an Owned UI key for a specific or current language.
 * @param {keyof OWNED_STRINGS["en"] | string} key
 * @param {string} [lang]
 * @returns {string}
 */
export function tOwned(key, lang) {
  const table = getOwnedStrings(lang);
  if (Object.prototype.hasOwnProperty.call(table, key)) return table[key];
  // Fallback to English for unknown keys to avoid empty labels
  const fallback = OWNED_STRINGS.en[key];
  return typeof fallback === "string" ? fallback : key;
}

/**
 * Optional: Provide a way to temporarily extend or override strings at runtime,
 * e.g., for A/B testing or white-label builds. This creates a shallow merged table
 * without mutating OWNED_STRINGS (which is frozen).
 *
 * Example:
 *   const i18n = withOwnedOverrides({ fr: { empty_title: "Aucun fichier ):(" } });
 *   i18n.t("empty_title", "fr"); // returns override
 */
export function withOwnedOverrides(patch) {
  const merged = {};
  for (const lang of allowedLanguages) {
    merged[lang] = Object.freeze({
      ...OWNED_STRINGS[lang],
      ...(patch && patch[lang]),
    });
  }
  const API = {
    allowed: () => allowedLanguages.slice(),
    keys: () => OWNED_KEYS.slice(),
    get: (lang) =>
      merged[allowedLanguages.includes(lang) ? lang : currentLang()] ||
      merged.en,
    t: (key, lang) => {
      const table = API.get(lang);
      if (Object.prototype.hasOwnProperty.call(table, key)) return table[key];
      const fb = merged.en[key];
      return typeof fb === "string" ? fb : key;
    },
    export: () => Object.freeze({ ...merged }),
  };
  return API;
}

/**
 * Convenience: localized expired label.
 * @param {string} [lang]
 * @returns {string}
 */
export function expiredLabel(lang) {
  return tOwned("expired", lang);
}

// Optionally expose for debugging in browsers without polluting in Node/test environments.
try {
  if (typeof window !== "undefined") {
    window.JBOwnedI18n = Object.freeze({
      allowed: allowedLanguages.slice(),
      keys: OWNED_KEYS.slice(),
      currentLang,
      getOwnedStrings,
      tOwned,
      expiredLabel,
      withOwnedOverrides,
    });
  }
} catch {
  // no-op
}
