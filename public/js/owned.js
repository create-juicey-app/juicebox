// js/owned.js

import { ownedList, ownedPanel } from './ui.js';
import { animateRemove, escapeHtml } from './utils.js';
import { copyToClipboard, flashCopied } from './utils.js';
import { deleteHandler } from './delete.js';

export const ownedHandler = {
  /**
   * Highlights an owned file chip by name (adds a CSS class for a moment)
   * @param {string} name
   */
  highlightOwned(name) {
    if (!name) return;
    const chip = ownedList && ownedList.querySelector(`.owned-chip[data-name="${name}"]`);
    if (chip) {
      chip.classList.add("highlight");
      setTimeout(() => chip.classList.remove("highlight"), 1800);
      chip.scrollIntoView({ behavior: "smooth", block: "center" });
    }
  },
  ownedMeta: new Map(),
  ownedCache: new Set(),
  ownedInitialRender: false,

  async loadExisting() {
    try {
      const r = await fetch("/mine");
      if (!r.ok) return;
      const data = await r.json();
  if (window.DEBUG_LOGS) console.log('[owned.js] /mine response:', data);
      if (data && Array.isArray(data.files)) {
        data.files.forEach((f) => this.addOwned(f.replace(/^f\//, "")));
        if (Array.isArray(data.metas)) {
          data.metas.forEach((m) => {
            const name = m.file.replace(/^f\//, "");
            this.ownedMeta.set(name, {
              expires: m.expires,
              original: m.original || "",
              total: m.total,
              set: m.set,
            });
          });
        }
  if (window.DEBUG_LOGS) console.log('[owned.js] ownedMeta after /mine:', Array.from(this.ownedMeta.entries()));
        this.renderOwned();
      }
  } catch (e) { if (window.DEBUG_LOGS) console.error('[owned.js] loadExisting error:', e); }
  },

  renderOwned() {
    if (!ownedList || !ownedPanel) return;
    const names = [...this.ownedCache];
    const want = new Set(names);
    const existing = new Map();
    ownedList.querySelectorAll(".owned-chip").forEach((chip) => existing.set(chip.dataset.name, chip));

    existing.forEach((chip, name) => {
      if (!want.has(name)) {
        animateRemove(chip, () => {
          if (!ownedList.querySelector(".owned-chip")) this.hideOwnedPanel();
        });
      }
    });

    if (!names.length) {
      this.hideOwnedPanel();
      return;
    }
    this.showOwnedPanel();
    const nowSec = Date.now() / 1000;
    names.sort();
    function formatRemain(sec) {
      if (sec <= 0) return "expired";
      if (sec < 60) return `${Math.floor(sec)}s`;
      if (sec < 3600) return `${Math.floor(sec / 60)}m ${Math.floor(sec % 60)}s`;
      if (sec < 86400) return `${Math.floor(sec / 3600)}h ${Math.floor((sec % 3600) / 60)}m`;
      return `${Math.floor(sec / 86400)}d ${Math.floor((sec % 86400) / 3600)}h`;
    }
    // Batch DOM updates using DocumentFragment
    let grid = ownedList.querySelector(".owned-grid");
    if (!grid) {
      grid = document.createElement("div");
      grid.className = "owned-grid";
      ownedList.appendChild(grid);
    }
    const frag = document.createDocumentFragment();
    names.forEach((n) => {
      if (existing.has(n)) return;
      const meta = this.ownedMeta.get(n) || {};
      const exp = meta.expires || -1;
      const remain = exp - nowSec;
      const chip = document.createElement("div");
      chip.className = "owned-chip";
      chip.dataset.name = n;
      if (exp >= 0) chip.dataset.exp = exp;
      if (meta.total) chip.dataset.total = meta.total;
      const displayName = meta.original?.trim() || n;
      const titleFull = displayName === n ? n : `${displayName} (${n})`;
      let percent = 100;
      const set = typeof meta.set === 'number' ? meta.set : (meta.set ? Number(meta.set) : null);
      if (set !== null && !isNaN(set) && exp > set) {
        const total = exp - set;
        const remain = exp - nowSec;
        percent = (remain / total) * 100;
        percent = Math.max(0, Math.min(100, percent));
        percent = Math.round(percent * 100) / 100;
      } else if (remain <= 0) {
        percent = 0;
      }
      let barWidth = `${percent}%`;
      chip.innerHTML = `<div class="top"><div class="name" title="${escapeHtml(titleFull)}">${escapeHtml(displayName)}</div><div class="actions"></div></div><div class="ttl-row"><span class="ttl">${formatRemain(remain)}</span><span class="ttl-bar-wrap"><span class="ttl-bar" style="width:${barWidth};"></span></span></div>`;
      chip.style.position = "relative";
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
      copyBtn.textContent = "📋";
      copyBtn.title = "Copy direct link";
      copyBtn.addEventListener("click", () => copyToClipboard(`${location.origin}/f/${n}`).then(() => flashCopied()));
      const delBtn = document.createElement("button");
      delBtn.className = "small";
      delBtn.textContent = "❌";
      delBtn.title = "Delete file from server";
      delBtn.addEventListener("click", () => {
        fetch(`/d/${encodeURIComponent(n)}`, { method: "DELETE" }).then(async (r) => {
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
        });
      });
      chip.querySelector(".actions").append(copyBtn, delBtn);
      frag.appendChild(chip);
    });
    grid.appendChild(frag);
    this.ownedInitialRender = true;
  },

  hideOwnedPanel() {
    if (ownedPanel.style.display === "none" || ownedPanel.classList.contains("closing")) return;
    ownedPanel.classList.add("closing");
    ownedPanel.addEventListener("animationend", () => {
        ownedPanel.style.display = "none";
        ownedPanel.classList.remove("closing");
      }, { once: true });
  },

  showOwnedPanel() {
    if (ownedPanel.style.display !== "none") return;
    ownedPanel.style.display = "";
    ownedPanel.classList.add("opening");
    ownedPanel.addEventListener("animationend", () => ownedPanel.classList.remove("opening"), { once: true });
  },

  async refreshOwned() {
    try {
      const r = await fetch("/mine", { cache: "no-store" });
      if (!r.ok) return;
      const data = await r.json();
      if (data && Array.isArray(data.files)) {
        this.ownedCache = new Set(data.files.map((f) => f.replace(/^f\//, "")));
        if (Array.isArray(data.metas)) {
          this.ownedMeta.clear();
          data.metas.forEach((m) =>
            this.ownedMeta.set(m.file.replace(/^f\//, ""), {
              expires: m.expires,
              original: m.original || "",
              total: m.total,
              set: m.set,
            })
          );
        }
        this.renderOwned();
      }
    } catch {}
  },

  addOwned(remoteName) {
  if (!remoteName || this.ownedCache.has(remoteName)) return;
  this.ownedCache.add(remoteName);
  // Only refresh from server to get canonical expiration (and render)
  this.refreshOwned();
  },
};