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
    ownedList
      .querySelectorAll('[data-empty="true"], .owned-grid[data-role="owned"]')
      .forEach((node) => node.remove());
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
    ownedList.appendChild(grid);
    this.prepareGridEnter(grid, { instant: true });
  },

  clearSkeleton() {
    if (!ownedList) return;
    ownedList
      .querySelectorAll('[data-skeleton="true"]')
      .forEach((node) => node.remove());
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

  updateExistingChips(names) {
    if (!ownedList) return false;
    const grid = ownedList.querySelector('.owned-grid[data-role="owned"]');
    if (!grid) return false;
    const chips = Array.from(grid.children).filter((child) =>
      child.classList?.contains("owned-chip")
    );
    if (chips.length !== names.length) return false;

    const nowSec = Date.now() / 1000;

    names.forEach((n, idx) => {
      const chip = chips[idx];
      if (!chip) return;
      const meta = this.ownedMeta.get(n) || {};
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
      const displayName = (meta.original && meta.original.trim()) || n;
      const titleFull = displayName === n ? n : `${displayName} (${n})`;

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
      if (ttlBar) ttlBar.style.width = `${percent.toFixed(2)}%`;

      const linkInput = chip.querySelector("input.link-input");
      if (linkInput) {
        const newValue = `${location.origin}/f/${n}`;
        if (linkInput.value !== newValue) linkInput.value = newValue;
      }
    });

    return true;
  },

  mountOwnedList(names, { instant = false } = {}) {
    if (!ownedList) return;
    ownedList
      .querySelectorAll('[data-empty="true"]')
      .forEach((node) => node.remove());
    ownedList
      .querySelectorAll('.owned-grid[data-role="owned"]')
      .forEach((node) => node.remove());

    const grid = document.createElement("div");
    grid.className = "owned-grid";
    grid.dataset.role = "owned";

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

    names.forEach((n) => {
      const chip = createChip(n);
      grid.appendChild(chip);
    });

    ownedList.appendChild(grid);
    this.prepareGridEnter(grid, { instant });
  },

  mountEmptyState({ instant = false } = {}) {
    if (!ownedList) return;
    ownedList
      .querySelectorAll('.owned-grid[data-role="owned"]')
      .forEach((node) => node.remove());
    const existing = ownedList.querySelector('[data-empty="true"]');
    if (existing) {
      this.prepareGridEnter(existing, { instant });
      return;
    }
    const grid = document.createElement("div");
    grid.className = "owned-grid";
    grid.dataset.empty = "true";
    grid.setAttribute("aria-hidden", "false");
    const empty = document.createElement("div");
    empty.className = "owned-empty";
    empty.innerHTML = `<strong>No files found ):</strong><span>Your uploads will land here once they finish.</span>`;
    grid.appendChild(empty);
    ownedList.appendChild(grid);
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

  applyResponse(data) {
    if (!data || !Array.isArray(data.files)) {
      return false;
    }
    this.ownedCache = new Set(data.files.map((f) => f.replace(/^f\//, "")));
    this.ownedMeta.clear();
    if (Array.isArray(data.metas)) {
      data.metas.forEach((m) =>
        this.ownedMeta.set(m.file.replace(/^f\//, ""), {
          expires: m.expires,
          original: m.original || "",
          total: m.total,
          set: m.set,
        })
      );
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
    if (this.renderState === "list" && signature === this.lastSignature) {
      this.updateExistingChips(names);
      this.lastSignature = signature;
      this.ownedInitialRender = true;
      return;
    }

    const instant = this.renderState === "init";
    this.mountOwnedList(names, { instant });
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
    if (!remoteName || this.ownedCache.has(remoteName)) return;
    this.ownedCache.add(remoteName);
    // Only refresh from server to get canonical expiration (and render)
    this.refreshOwned();
  },
};
