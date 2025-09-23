// js/config.js

// Default values
window.MAX_FILE_BYTES = 500 * 1024 * 1024;
window.MAX_FILE_SIZE_STR = "500MB";

/**
 * Fetches dynamic configuration from the server and updates global settings.
 * We keep these on the `window` object because many enhancement patches rely on it.
 * @returns {Promise<void>}
 */
export function fetchConfig() {
  return fetch("/api/config", { cache: "no-store" })
    .then((r) => (r.ok ? r.json() : null))
    .then((cfg) => {
      if (cfg && typeof cfg.max_file_bytes === "number")
        window.MAX_FILE_BYTES = cfg.max_file_bytes;
      if (cfg && typeof cfg.max_file_size_str === "string")
        window.MAX_FILE_SIZE_STR = cfg.max_file_size_str;
    })
    .catch(() => {
      console.warn("Could not fetch dynamic config, using defaults.");
    });
}