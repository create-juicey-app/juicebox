// js/owned.js

import { ownedList, ownedPanel } from "./ui.js";
import {
  escapeHtml,
  copyToClipboard,
  flashCopied,
  showSnack,
} from "./utils.js";
import { deleteHandler } from "./delete.js";

export const ownedHandler = {
  /**
   * Highlights an owned file chip by name (adds a CSS class for a moment)
   * @param {string} name
   */
  highlightOwned(name) {
    if (!name || !ownedList) return;
    const chip = ownedList.querySelector(`.owned-chip[data-name="${name}"]`);
    if (!chip) return;
    chip.classList.add("highlight");
    setTimeout(() => chip.classList.remove("highlight"), 1800);
    chip.scrollIntoView({ behavior: "smooth", block: "center" });
  },

  ownedMeta: new Map(),
  ownedCache: new Set(),
  ownedInitialRender: false,
  loading: false,
  skeletonCount: 3,
  refreshTimer: null,
  renderState: "init",
  lastSignature: "",

  // FLIP animation state
  chipPositions: new Map(), // Map<name, DOMRect>
  chipElements: new Map(), // Map<name, HTMLElement>
  createListItem(kind) {
    const li = document.createElement("li");
    li.className = `owned-item${kind ? ` owned-item-${kind}` : ""}`;
    if (kind) li.dataset.kind = kind;
    return li;
  },
  getListItemByKind(kind) {
    if (!ownedList || !kind) return null;
    return ownedList.querySelector(`li[data-kind="${kind}"]`);
  },
  removeListItemsByKinds(kinds = []) {
    if (!ownedList || !Array.isArray(kinds)) return;
    kinds.forEach((kind) => {
      ownedList
        .querySelectorAll(`li[data-kind="${kind}"]`)
        .forEach((node) => node.remove());
    });
  },
  setLoading(state) {
    this.loading = state;
    if (!ownedPanel) return;
    ownedPanel.classList.toggle("owned-loading", state);
    if (state) {
      this.renderSkeleton();
    } else {
      this.clearSkeleton();
    }
  },

  markRefreshing() {
    if (!ownedPanel) return;
    ownedPanel.classList.add("owned-refreshing");
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
    this.refreshTimer = setTimeout(() => this.clearRefreshing(), 650);
  },

  clearRefreshing() {
    if (!ownedPanel) return;
    ownedPanel.classList.remove("owned-refreshing");
    if (this.refreshTimer) {
      clearTimeout(this.refreshTimer);
      this.refreshTimer = null;
    }
  },

  renderSkeleton() {
    if (!ownedList || ownedList.querySelector('[data-skeleton="true"]')) return;
    this.renderState = "loading";
    this.removeListItemsByKinds(["empty", "grid"]);
    const grid = document.createElement("div");
    grid.className = "owned-grid";
    grid.dataset.skeleton = "true";
    grid.setAttribute("aria-hidden", "true");
    for (let i = 0; i < this.skeletonCount; i += 1) {
      const skeleton = document.createElement("div");
      skeleton.className = "owned-skeleton";
      skeleton.innerHTML = `<div class="skeleton-line"></div><div class="skeleton-line compact"></div><div class="skeleton-bar"></div>`;
      grid.appendChild(skeleton);
    }
    const wrapper = this.createListItem("skeleton");
    wrapper.setAttribute("aria-hidden", "true");
    wrapper.appendChild(grid);
    ownedList.appendChild(wrapper);
    this.prepareGridEnter(grid, { instant: true });
  },

  clearSkeleton() {
    if (!ownedList) return;
    this.removeListItemsByKinds(["skeleton"]);
  },

  prepareGridEnter(grid, { instant = false } = {}) {
    if (!grid) return;
    grid.classList.remove("morph-ready", "skip-transition", "morph-exit");
    if (instant) {
      grid.classList.add("skip-transition", "morph-ready");
      requestAnimationFrame(() => grid.classList.remove("skip-transition"));
      return;
    }
    requestAnimationFrame(() => grid.classList.add("morph-ready"));
  },

  formatRemaining(sec) {
    if (sec <= 0) return "expired";
    if (sec < 60) return `${Math.floor(sec)}s`;
    if (sec < 3600) return `${Math.floor(sec / 60)}m ${Math.floor(sec % 60)}s`;
    if (sec < 86400)
      return `${Math.floor(sec / 3600)}h ${Math.floor((sec % 3600) / 60)}m`;
    return `${Math.floor(sec / 86400)}d ${Math.floor((sec % 86400) / 3600)}h`;
  },

  buildSignature(names) {
    return names
      .map((n) => {
        const meta = this.ownedMeta.get(n) || {};
        const expires = meta.expires ?? "";
        const total = meta.total ?? "";
        const original = meta.original ?? "";
        const set = meta.set ?? "";
        return `${n}|${expires}|${total}|${original}|${set}`;
      })
      .join("::");
  },

  capturePositions() {
    if (!ownedList) return;
    const grid = ownedList.querySelector('.owned-grid[data-role="owned"]');
    if (!grid) return;

    this.chipPositions.clear();
    this.chipElements.clear();

    grid.querySelectorAll(".owned-chip[data-name]").forEach((chip) => {
      const name = chip.dataset.name;
      if (name) {
        this.chipPositions.set(name, chip.getBoundingClientRect());
        this.chipElements.set(name, chip);
      }
    });
  },

  animateToNewPositions(newNames) {
    if (!ownedList || this.chipPositions.size === 0) return;
    const grid = ownedList.querySelector('.owned-grid[data-role="owned"]');
    if (!grid) return;

    const animations = [];

    // Animate existing chips to new positions
    grid.querySelectorAll(".owned-chip[data-name]").forEach((chip) => {
      const name = chip.dataset.name;
      const oldRect = this.chipPositions.get(name);

      if (oldRect) {
        const newRect = chip.getBoundingClientRect();
        const deltaX = oldRect.left - newRect.left;
        const deltaY = oldRect.top - newRect.top;

        if (Math.abs(deltaX) > 0.5 || Math.abs(deltaY) > 0.5) {
          chip.style.transform = `translate(${deltaX}px, ${deltaY}px)`;
          chip.style.transition = "none";

          requestAnimationFrame(() => {
            chip.style.transition =
              "transform 0.32s cubic-bezier(0.4, 0.0, 0.2, 1)";
            chip.style.transform = "";
          });

          animations.push(
            new Promise((resolve) => {
              const handler = () => {
                chip.removeEventListener("transitionend", handler);
                resolve();
              };
              chip.addEventListener("transitionend", handler);
              setTimeout(resolve, 400); // fallback
            })
          );
        }
      } else {
        // New chip - fade in
        chip.style.opacity = "0";
        chip.style.transform = "scale(0.9)";
        chip.style.transition = "none";

        requestAnimationFrame(() => {
          chip.style.transition =
            "opacity 0.28s ease-out, transform 0.28s cubic-bezier(0.4, 0.0, 0.2, 1)";
          chip.style.opacity = "";
          chip.style.transform = "";
        });
      }
    });

    // Animate removed chips out
    this.chipPositions.forEach((rect, name) => {
      if (!newNames.includes(name)) {
        const oldChip = this.chipElements.get(name);
        if (oldChip && oldChip.parentElement) {
          oldChip.style.transition =
            "opacity 0.22s ease-in, transform 0.22s ease-in";
          oldChip.style.opacity = "0";
          oldChip.style.transform = "scale(0.85)";

          animations.push(
            new Promise((resolve) => {
              setTimeout(() => {
                if (oldChip.parentElement) {
                  oldChip.remove();
                }
                resolve();
              }, 240);
            })
          );
        }
      }
    });

    return Promise.all(animations);
  },

  updateExistingChipsAnimated(names) {
    if (!ownedList) return false;
    const grid = ownedList.querySelector('.owned-grid[data-role="owned"]');
    if (!grid) return false;

    const existingChips = Array.from(
      grid.querySelectorAll(".owned-chip[data-name]")
    );
    const existingNames = existingChips.map((chip) => chip.dataset.name);

    // Check if we can do an in-place animated update
    const sameSet =
      names.length === existingNames.length &&
      names.every((n) => existingNames.includes(n));

    if (!sameSet) return false;

    // Capture current positions before any changes
    this.capturePositions();

    const nowSec = Date.now() / 1000;
    const chipMap = new Map();
    existingChips.forEach((chip) => chipMap.set(chip.dataset.name, chip));

    // Reorder chips in DOM to match new order
    const fragment = document.createDocumentFragment();
    names.forEach((name) => {
      const chip = chipMap.get(name);
      if (!chip) return;

      const meta = this.ownedMeta.get(name) || {};
      const expiry = Number(meta.expires) || -1;
      if (expiry >= 0) {
        chip.dataset.exp = expiry;
      } else {
        delete chip.dataset.exp;
      }
      if (meta.total) {
        chip.dataset.total = meta.total;
      } else {
        delete chip.dataset.total;
      }

      const remainRaw = expiry > 0 ? expiry - nowSec : -1;
      const displayName = (meta.original && meta.original.trim()) || name;
      const titleFull =
        displayName === name ? name : `${displayName} (${name})`;

      const nameEl = chip.querySelector(".name");
      if (nameEl) {
        nameEl.textContent = displayName;
        nameEl.title = titleFull;
      }

      const ttlEl = chip.querySelector(".ttl");
      if (ttlEl) ttlEl.textContent = this.formatRemaining(remainRaw);

      let percent = 100;
      const setValue =
        typeof meta.set === "number"
          ? meta.set
          : meta.set
          ? Number(meta.set)
          : null;
      if (setValue !== null && !Number.isNaN(setValue) && expiry > setValue) {
        const total = expiry - setValue;
        const remain = expiry - nowSec;
        percent = Math.max(0, Math.min(100, (remain / total) * 100));
      } else if (remainRaw <= 0) {
        percent = 0;
      }

      const ttlBar = chip.querySelector(".ttl-bar");
      if (ttlBar) {
        ttlBar.style.transition = "width 0.4s ease-out";
        ttlBar.style.width = `${percent.toFixed(2)}%`;
      }

      const linkInput = chip.querySelector("input.link-input");
      if (linkInput) {
        const newValue = `${location.origin}/f/${name}`;
        if (linkInput.value !== newValue) linkInput.value = newValue;
      }

      fragment.appendChild(chip);
    });

    grid.appendChild(fragment);

    // Animate to new positions
    this.animateToNewPositions(names);

    return true;
  },

  mountOwnedList(names, { instant = false, animate = false } = {}) {
    if (!ownedList) return;
    if (animate && !instant) {
      this.capturePositions();
    }
    this.removeListItemsByKinds(["empty", "skeleton"]);
    const existingItem = this.getListItemByKind("grid");
    let grid =
      existingItem?.querySelector('.owned-grid[data-role="owned"]') ||
      document.createElement("div");
    if (!grid.dataset.role) {
      grid.className = "owned-grid";
      grid.dataset.role = "owned";
    }
    grid.innerHTML = "";
    if (!existingItem) {
      const wrapper = this.createListItem("grid");
      wrapper.appendChild(grid);
      ownedList.appendChild(wrapper);
    }
    const nowSec = Date.now() / 1000;
    const createChip = (n) => {
      const meta = this.ownedMeta.get(n) || {};
      const chip = document.createElement("div");
      chip.className = "owned-chip";
      chip.dataset.name = n;

      const expiry = Number(meta.expires) || -1;
      if (expiry >= 0) chip.dataset.exp = expiry;
      if (meta.total) chip.dataset.total = meta.total;

      const remainRaw = expiry > 0 ? expiry - nowSec : -1;
      const displayName = (meta.original && meta.original.trim()) || n;
      const titleFull = displayName === n ? n : `${displayName} (${n})`;

      let percent = 100;
      const setValue =
        typeof meta.set === "number"
          ? meta.set
          : meta.set
          ? Number(meta.set)
          : null;
      if (setValue !== null && !Number.isNaN(setValue) && expiry > setValue) {
        const total = expiry - setValue;
        const remain = expiry - nowSec;
        percent = Math.max(0, Math.min(100, (remain / total) * 100));
      } else if (remainRaw <= 0) {
        percent = 0;
      }

      chip.innerHTML = `<div class="top"><div class="name" title="${escapeHtml(
        titleFull
      )}">${escapeHtml(
        displayName
      )}</div><div class="actions"></div></div><div class="ttl-row"><span class="ttl">${this.formatRemaining(
        remainRaw
      )}</span><span class="ttl-bar-wrap"><span class="ttl-bar" style="width:${percent.toFixed(
        2
      )}%;"></span></span></div>`;

      const linkInput = document.createElement("input");
      linkInput.type = "text";
      linkInput.readOnly = true;
      linkInput.className = "link-input";
      linkInput.value = `${location.origin}/f/${n}`;
      linkInput.title = "Click to copy direct link";
      linkInput.setAttribute("aria-label", "Direct file link (click to copy)");
      linkInput.addEventListener("click", () => {
        linkInput.select();
        copyToClipboard(linkInput.value).then(() => flashCopied());
      });
      chip.appendChild(linkInput);

      const copyBtn = document.createElement("button");
      copyBtn.className = "small";
      copyBtn.type = "button";
      copyBtn.textContent = "📋";
      copyBtn.title = "Copy direct link";
      copyBtn.addEventListener("click", () =>
        copyToClipboard(`${location.origin}/f/${n}`).then(() => flashCopied())
      );

      const delBtn = document.createElement("button");
      delBtn.className = "small";
      delBtn.type = "button";
      delBtn.textContent = "❌";
      delBtn.title = "Delete file from server";
      delBtn.addEventListener("click", () => {
        fetch(`/d/${encodeURIComponent(n)}`, { method: "DELETE" })
          .then(async (r) => {
            if (r.ok) {
              this.ownedCache.delete(n);
              this.ownedMeta.delete(n);
              deleteHandler.removeFromUploads(n);
              this.renderOwned();
            } else {
              let msg = "Delete failed.";
              try {
                const err = await r.json();
                if (err && err.message) msg = err.message;
              } catch {}
              showSnack(msg);
            }
          })
          .catch(() => showSnack("Delete failed."));
      });

      chip.querySelector(".actions").append(copyBtn, delBtn);
      return chip;
    };

    // Clear and rebuild grid
    if (existingItem) {
      existingItem.innerHTML = "";
    }

    names.forEach((n) => {
      const chip = createChip(n);
      grid.appendChild(chip);
    });

    if (animate && !instant) {
      this.animateToNewPositions(names);
    } else {
      this.prepareGridEnter(grid, { instant });
    }
  },

  mountEmptyState({ instant = false } = {}) {
    if (!ownedList) return;
    this.removeListItemsByKinds(["grid", "skeleton"]);
    const grid = document.createElement("div");
    grid.className = "owned-grid";
    grid.dataset.empty = "true";
    grid.setAttribute("aria-hidden", "false");
    const empty = document.createElement("div");
    empty.className = "owned-empty";
    empty.innerHTML = `<strong>No files found ):</strong><span>Your uploads will land here once they finish.</span>`;
    grid.appendChild(empty);
    const wrapper = this.createListItem("empty");
    wrapper.appendChild(grid);
    ownedList.appendChild(wrapper);
    this.prepareGridEnter(grid, { instant });
  },

  renderEmptyState(options = {}) {
    if (!ownedList) return;
    this.clearSkeleton();
    this.mountEmptyState(options);
    if (ownedPanel) ownedPanel.setAttribute("data-state", "empty");
    this.renderState = "empty";
    this.lastSignature = "empty";
  },

  normalizeRemoteName(raw) {
    let value = raw == null ? "" : String(raw).trim();
    if (!value) return null;
    try {
      if (/^https?:\/\//i.test(value)) {
        const url = new URL(value);
        value = url.pathname || "";
      }
    } catch {}
    value = value.split("?")[0].split("#")[0].replace(/^\/+/, "");
    if (value.startsWith("f/")) value = value.slice(2);
    if (value.startsWith("d/")) value = value.slice(2);
    if (value.includes("/")) {
      const parts = value.split("/");
      value = parts.pop() || parts.pop() || "";
    }
    try {
      value = decodeURIComponent(value);
    } catch {}
    return value.trim() || null;
  },

  applyResponse(data) {
    if (!data || !Array.isArray(data.files)) {
      return false;
    }
    const normalized = data.files
      .map((f) => this.normalizeRemoteName(f))
      .filter(Boolean);
    this.ownedCache = new Set(normalized);
    this.ownedMeta.clear();
    if (Array.isArray(data.metas)) {
      data.metas.forEach((m) => {
        const key = this.normalizeRemoteName(m.file);
        if (!key) return;
        this.ownedMeta.set(key, {
          expires: m.expires,
          original: m.original || "",
          total: m.total,
          set: m.set,
        });
      });
    }
    return true;
  },

  async loadExisting() {
    this.setLoading(true);
    try {
      const response = await fetch("/mine");
      if (!response.ok) return;
      const data = await response.json();
      if (window.DEBUG_LOGS) console.log("[owned.js] /mine response:", data);
      this.applyResponse(data);
    } catch (e) {
      if (window.DEBUG_LOGS) console.error("[owned.js] loadExisting error:", e);
    } finally {
      this.setLoading(false);
      this.renderOwned();
    }
  },

  renderOwned() {
    if (!ownedList) return;
    this.clearSkeleton();
    this.clearRefreshing();

    const names = [...this.ownedCache].sort();

    if (ownedPanel) {
      ownedPanel.setAttribute(
        "data-state",
        names.length ? "has-files" : "empty"
      );
    }

    if (!names.length) {
      if (this.renderState !== "empty") {
        const instant = this.renderState === "init";
        this.mountEmptyState({ instant });
        this.renderState = "empty";
      } else {
        const existing = ownedList.querySelector('[data-empty="true"]');
        if (!existing) {
          this.mountEmptyState({ instant: true });
        } else {
          this.prepareGridEnter(existing, { instant: true });
        }
      }
      this.lastSignature = "empty";
      this.ownedInitialRender = true;
      return;
    }

    const signature = this.buildSignature(names);
    const canAnimate = this.renderState === "list" && this.ownedInitialRender;

    if (canAnimate && signature === this.lastSignature) {
      // Just update values with smooth transitions
      this.updateExistingChipsAnimated(names);
      this.lastSignature = signature;
      this.ownedInitialRender = true;
      return;
    }

    if (canAnimate) {
      // Try animated update if possible
      const animated = this.updateExistingChipsAnimated(names);
      if (animated) {
        this.renderState = "list";
        this.lastSignature = signature;
        this.ownedInitialRender = true;
        return;
      }
    }

    // Fall back to full rebuild with animation
    const instant = this.renderState === "init";
    this.mountOwnedList(names, { instant, animate: canAnimate && !instant });
    this.renderState = "list";
    this.lastSignature = signature;
    this.ownedInitialRender = true;
  },

  async refreshOwned() {
    const useSkeleton = !this.ownedInitialRender;
    if (useSkeleton) {
      this.setLoading(true);
    } else {
      this.markRefreshing();
    }

    try {
      const response = await fetch("/mine", { cache: "no-store" });
      if (!response.ok) return;
      const data = await response.json();
      this.applyResponse(data);
    } catch (e) {
      if (window.DEBUG_LOGS) console.error("[owned.js] refreshOwned error:", e);
    } finally {
      if (useSkeleton) {
        this.setLoading(false);
      } else {
        this.clearRefreshing();
      }
      this.renderOwned();
    }
  },

  addOwned(remoteName) {
    const normalized = this.normalizeRemoteName(remoteName);
    if (!normalized || this.ownedCache.has(normalized)) return;
    this.ownedCache.add(normalized);
    this.refreshOwned();
  },
};
