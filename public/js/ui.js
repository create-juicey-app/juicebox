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

// --- Other UI Initializations ---
export function setupUI() {
  // Panel reveal for owned panel (initial)
  if (ownedPanel) {
    ownedPanel.classList.add("reveal-start");
    requestAnimationFrame(() => ownedPanel.classList.add("reveal"));
  }

  // Drop-zone animation setup
  if (dropZone) {
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
    };    dropZone.addEventListener("mouseleave", () => setOpen(false));
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
      () => !dropZone.classList.contains("drag") && setOpen(false)
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
      if (fileInput && !e.defaultPrevented && e.target !== fileInput) {
        fileInput.click();
      }
    });
  }
}