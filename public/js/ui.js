// js/ui.js

// --- DOM Element References ---
export const dropZone = document.getElementById("dropZone");
if (dropZone && !document.getElementById("fileInput")) {
  const inp = document.createElement("input");
  inp.type = "file";
  inp.multiple = true;
  inp.id = "fileInput";
  inp.style.display = "none";
  dropZone.appendChild(inp);
}
export const fileInput = document.getElementById("fileInput");
export const list = document.getElementById("fileList");
export const snackbar = document.getElementById("snackbar");
export const ownedList = document.getElementById("ownedList");
export const ownedPanel = document.getElementById("ownedPanel");
export const ttlSelect = document.getElementById("ttlSelect");
export const ttlValueLabel = document.getElementById("ttlValue");

// --- TTL Logic ---
const ttlMap = ["1h", "3h", "12h", "1d", "3d", "7d", "14d"];
const QUOTA_MESSAGE_FALLBACK =
  "Maximum storage quota has been reached. You cannot upload for now.";

if (
  dropZone &&
  dropZone.dataset &&
  dropZone.dataset.quotaDisabled === "true"
) {
  const reason =
    dropZone.dataset.quotaMessage && dropZone.dataset.quotaMessage.trim()
      ? dropZone.dataset.quotaMessage
      : QUOTA_MESSAGE_FALLBACK;
  disableUploads(reason);
}

export function getTTL() {
  if (!ttlSelect) return "3d";
  if (ttlSelect.tagName === "INPUT" && ttlSelect.type === "range") {
    return ttlMap[parseInt(ttlSelect.value, 10)] || "3d";
  }
  return ttlSelect.value;
}

export function setupTTL() {
  if (!ttlSelect) return;
  const saved = localStorage.getItem("ttlChoice");
  if (saved && ttlMap.includes(saved))
    ttlSelect.value = String(ttlMap.indexOf(saved));

  function updateTTL() {
    const code = getTTL();
    if (ttlValueLabel) ttlValueLabel.textContent = code;
    localStorage.setItem("ttlChoice", code);
  }
  ttlSelect.addEventListener("input", updateTTL);
  ttlSelect.addEventListener("change", updateTTL);
  updateTTL();
}

// Ensure a theme is set; default to dark when none is specified or detectable
// Run immediately to guarantee dark-first rendering before additional UI code
ensureTheme({ defaultTheme: "dark" });
try {
  document.documentElement.style.colorScheme = "dark";
} catch {}
export function ensureTheme({ defaultTheme = "dark" } = {}) {
  const root = document.documentElement;
  if (!root) return;
  // If already specified, respect it
  let theme = root.getAttribute("data-theme");
  // Try localStorage if attribute not present
  if (!theme) {
    try {
      theme =
        localStorage.getItem("jb.theme") ||
        localStorage.getItem("theme") ||
        null;
    } catch (_) {
      theme = null;
    }
  }
  // Normalize and set default if still missing
  theme =
    typeof theme === "string" && theme.trim() ? theme.trim() : defaultTheme;
  if (root.getAttribute("data-theme") !== theme) {
    root.setAttribute("data-theme", theme);
  }
  // Persist for next loads
  try {
    localStorage.setItem("jb.theme", theme);
  } catch (_) {}
}

export function openFilePicker(target = fileInput) {
  if (window.JB_UPLOADS_DISABLED) return;
  if (dropZone && dropZone.classList.contains("is-disabled")) return;
  const input = target || document.getElementById("fileInput");
  if (input) {
    try {
      if (typeof input.showPicker === "function") {
        input.showPicker();
      } else {
        input.click();
      }
    } catch {
      try {
        input.click();
      } catch {}
    }
  }
}

// --- Other UI Initializations ---
export function setupUI() {
  ensureTheme();
  // Panel reveal for owned panel (initial)
  if (ownedPanel) {
    ownedPanel.classList.add("reveal-start");
    requestAnimationFrame(() => ownedPanel.classList.add("reveal"));
  }

  // Drop-zone animation setup
  if (dropZone) {
    if (dropZone.classList.contains("is-disabled")) {
      disableUploads();
      return;
    }
    setTimeout(() => dropZone.classList.add("animate"), 500);
    const iconEl = dropZone.querySelector(".icon");
    const setOpen = (open) => {
      if (iconEl) iconEl.textContent = open ? "📂" : "📁";
    };

    let _dragSpecial = false;
    const updateDragIcon = (e) => {
      if (!iconEl || !e || !e.dataTransfer) return;
      const dt = e.dataTransfer;
      let special = dt.items && Array.from(dt.items).length > 1;
      if (special) {
        _dragSpecial = true;
        iconEl.textContent = "📦";
      } else if (!_dragSpecial) {
        iconEl.textContent = "📂";
      }
    };
    dropZone.addEventListener("mouseleave", () => setOpen(false));
    dropZone.addEventListener("focusout", () => setOpen(false));

    ["dragenter", "dragover"].forEach((evt) => {
      dropZone.addEventListener(evt, (ev) => updateDragIcon(ev), true);
    });
    ["dragleave", "drop"].forEach((evt) => {
      dropZone.addEventListener(evt, () => {
        if (_dragSpecial) {
          _dragSpecial = false;
          setOpen(false);
        }
      });
    });

    dropZone.addEventListener("mouseenter", () => setOpen(true));
    dropZone.addEventListener("focusin", () => setOpen(true));
    dropZone.addEventListener("dragenter", () => setOpen(true));
    dropZone.addEventListener(
      "dragleave",
      () => !dropZone.classList.contains("drag") && setOpen(false),
    );

    // Ripple click effect
    dropZone.addEventListener("click", (e) => {
      dropZone.querySelectorAll(".ripple").forEach((r) => r.remove());
      const rect = dropZone.getBoundingClientRect();
      const size = Math.max(rect.width, rect.height);
      const ripple = document.createElement("span");
      ripple.className = "ripple";
      ripple.style.width = ripple.style.height = size + "px";
      ripple.style.left = e.clientX - rect.left - size / 2 + "px";
      ripple.style.top = e.clientY - rect.top - size / 2 + "px";
      dropZone.appendChild(ripple);
      ripple.addEventListener("animationend", () => ripple.remove(), {
        once: true,
      });
      if (!e.defaultPrevented && e.target !== fileInput) {
        openFilePicker();
      }
    });
  }
}

export function getQuotaMessage() {
  if (dropZone && dropZone.dataset && dropZone.dataset.quotaMessage) {
    return dropZone.dataset.quotaMessage;
  }
  if (
    window.JB_QUOTA_INFO &&
    typeof window.JB_QUOTA_INFO.quota_message === "string" &&
    window.JB_QUOTA_INFO.quota_message.trim()
  ) {
    return window.JB_QUOTA_INFO.quota_message.trim();
  }
  if (window.JBLang && typeof window.JBLang.quota_blocked === "string") {
    return window.JBLang.quota_blocked;
  }
  return QUOTA_MESSAGE_FALLBACK;
}

export function disableUploads(reason = getQuotaMessage()) {
  window.JB_UPLOADS_DISABLED = true;
  if (!window.JB_QUOTA_INFO || typeof window.JB_QUOTA_INFO !== "object") {
    window.JB_QUOTA_INFO = {};
  }
  window.JB_QUOTA_INFO.uploads_blocked = true;
  window.JB_QUOTA_INFO.quota_message = reason;
  if (dropZone) {
    dropZone.classList.add("is-disabled");
    dropZone.setAttribute("aria-disabled", "true");
    dropZone.dataset.quotaDisabled = "true";
    dropZone.dataset.quotaMessage = reason;
    dropZone.style.pointerEvents = "none";
    dropZone.style.cursor = "not-allowed";
  }
  if (fileInput) {
    fileInput.disabled = true;
    fileInput.setAttribute("aria-disabled", "true");
  }
  const hint = document.getElementById("dropHint");
  if (hint) {
    if (dropZone && !dropZone.dataset.hintOriginal) {
      dropZone.dataset.hintOriginal = hint.textContent || "";
    }
    hint.textContent = reason;
  }
  document.querySelectorAll(".choose-files-round").forEach((btn) => {
    btn.classList.add("is-disabled");
    btn.setAttribute("aria-disabled", "true");
    btn.setAttribute("disabled", "true");
  });
}

export function enableUploads() {
  window.JB_UPLOADS_DISABLED = false;
  if (dropZone) {
    dropZone.classList.remove("is-disabled");
    dropZone.removeAttribute("aria-disabled");
    dropZone.dataset.quotaDisabled = "false";
    dropZone.style.removeProperty("pointer-events");
    dropZone.style.removeProperty("cursor");
  }
  if (fileInput) {
    fileInput.disabled = false;
    fileInput.removeAttribute("aria-disabled");
  }
  const hint = document.getElementById("dropHint");
  if (hint && dropZone) {
    const original = dropZone.dataset.hintOriginal;
    if (original) {
      hint.textContent = original;
    }
  }
  document.querySelectorAll(".choose-files-round").forEach((btn) => {
    btn.classList.remove("is-disabled");
    btn.removeAttribute("aria-disabled");
    btn.removeAttribute("disabled");
  });
}

export function applyQuotaState(quota) {
  const blocked = quota && quota.uploads_blocked;
  if (blocked) {
    const reason =
      typeof quota.message === "string" && quota.message.trim()
        ? quota.message.trim()
        : getQuotaMessage();
    disableUploads(reason);
  } else {
    enableUploads();
  }
}
