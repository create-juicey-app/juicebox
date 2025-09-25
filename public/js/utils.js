// js/utils.js

/**
 * Shows a temporary message in the snackbar.
 * @param {string} msg - The message to display.
 */
export function flashCopied(msg = "Link copied") {
  let sb = document.getElementById("snackbar");
  if (!sb) {
    sb = document.createElement("div");
    sb.id = "snackbar";
    document.body.appendChild(sb);
  }
  sb.textContent = msg;
  sb.classList.remove("error");
  sb.classList.add("show");
  clearTimeout(flashCopied._t);
  flashCopied._t = setTimeout(() => sb.classList.remove("show"), 1600);
}

// --- bfcache support utility ---
// Ensures the app is bfcache-friendly by using pagehide/pageshow instead of unload
// and provides hooks for state save/restore if needed.
export function setupBfcacheSupport({ onSaveState, onRestoreState } = {}) {
  // Warn if unload is used (bad for bfcache)
  window.addEventListener('unload', () => {
    if (window.DEBUG_LOGS) console.warn('Avoid using unload event for bfcache compatibility.');
  });

  // Save state before page is hidden (for bfcache or normal navigation)
  window.addEventListener('pagehide', (e) => {
    if (onSaveState) onSaveState(e);
  });

  // Restore state if coming back from bfcache
  window.addEventListener('pageshow', (e) => {
    if (e.persisted && onRestoreState) onRestoreState(e);
  });
}


/**
 * Copies text to the user's clipboard.
 * @param {string} text - The text to copy.
 */
export async function copyToClipboard(text) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    try {
      document.execCommand("copy");
    } catch {}
    document.body.removeChild(ta);
  }
}

/**
 * Formats a number of bytes into a human-readable string (KB, MB, GB).
 * @param {number} b - The number of bytes.
 * @returns {string}
 */
export function fmtBytes(b) {
  if (!isFinite(b) || b < 0) return "–";
  if (b === 0) return "0 B";
  const k = 1024;
  const s = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(s.length - 1, Math.floor(Math.log(b) / Math.log(k)));
  return (b / Math.pow(k, i)).toFixed(i ? 2 : 0) + " " + s[i];
}

/**
 * Shows an error message in the snackbar and shakes the drop zone.
 * @param {string} msg - The error message.
 * @param {object} [opts] - Additional options.
 */
export function showSnack(msg, opts) {
  opts = opts || {};
  if (window.DEBUG_LOGS) console.log("[showSnack]", msg, opts);
  let sb = document.getElementById("snackbar");
  if (!sb) {
    sb = document.createElement("div");
    sb.id = "snackbar";
    document.body.appendChild(sb);
  }
  sb.textContent = msg;
  sb.classList.add("show", "error");
  clearTimeout(showSnack._t);
  showSnack._t = setTimeout(() => sb.classList.remove("show", "error"), 5000);

  const dropZone = document.getElementById("dropZone");
  if (dropZone) {
    dropZone.classList.add("shake");
    dropZone.addEventListener(
      "animationend",
      () => dropZone.classList.remove("shake"),
      { once: true }
    );
  }
  if (window._announce) window._announce(msg);
}

/**
 * Converts a TTL code (e.g., "3d") to seconds.
 * @param {string} code - The TTL code.
 * @returns {number}
 */
export function ttlCodeSeconds(code) {
  return (
    {
      "1h": 3600,
      "3h": 10800,
      "12h": 43200,
      "1d": 86400,
      "3d": 259200,
      "7d": 604800,
      "14d": 1209600,
    }[code] || 259200
  );
}

/**
 * Animates the removal of a DOM element with a smooth squish/fade effect.
 * @param {HTMLElement} el - The element to remove.
 * @param {Function} [cb] - A callback to run after removal.
 */
export function animateRemove(el, cb) {
  if (!el) {
    cb && cb();
    return;
  }
  // Fallback (original height-collapse animation)
  const rect = el.getBoundingClientRect();
  const cs = getComputedStyle(el);
  el.style.setProperty("--orig-h", rect.height + "px");
  el.style.setProperty("--orig-mt", cs.marginTop);
  el.style.setProperty("--orig-mb", cs.marginBottom);
  el.style.setProperty("--orig-pt", cs.paddingTop);
  el.style.setProperty("--orig-pb", cs.paddingBottom);
  el.classList.add("removing");
  el.addEventListener(
    "animationend",
    () => {
      try {
        el.remove();
      } catch {}
      cb && cb();
    },
    { once: true }
  );
}

/**
 * Escapes HTML special characters in a string.
 * @param {string} s - The string to escape.
 * @returns {string}
 */
export function escapeHtml(s) {
  return (s || "").replace(
    /[&<>"']/g,
    (c) =>
      ({
        "&": "&amp;",
        "<": "&lt;",
        ">": "&gt;",
        '"': "&quot;",
        "'": "&#39;",
      }[c])
  );
}