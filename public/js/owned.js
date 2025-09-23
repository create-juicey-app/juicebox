// js/owned.js

import { ownedList, ownedPanel } from './ui.js';
import { animateRemove, escapeHtml } from './utils.js';
import { copyToClipboard, flashCopied } from './utils.js';
import { deleteHandler } from './delete.js';

export const ownedHandler = {
  ownedMeta: new Map(),
  ownedCache: new Set(),
  ownedInitialRender: false,

  async loadExisting() {
    try {
      const r = await fetch("/mine");
      if (!r.ok) return;
      const data = await r.json();
      if (data && Array.isArray(data.files)) {
        data.files.forEach((f) => this.addOwned(f.replace(/^f\//, "")));
        if (Array.isArray(data.metas)) {
          data.metas.forEach((m) => {
            const name = m.file.replace(/^f\//, "");
            this.ownedMeta.set(name, {
              expires: m.expires,
              original: m.original || "",
            });
          });
        }
        this.renderOwned();
      }
    } catch {}
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
    names.forEach((n) => {
      if (existing.has(n)) return; // Don't re-render existing, just update in countdown
      const meta = this.ownedMeta.get(n) || {};
      const exp = meta.expires || -1;
      const remain = exp - nowSec;
      const fmtRemain = (sec) => (sec <= 0 ? "expired" : "...");

      const chip = document.createElement("div");
      chip.className = "owned-chip";
      chip.dataset.name = n;
      if (exp >= 0) chip.dataset.exp = exp;
      if (meta.total) chip.dataset.total = meta.total;
      
      const displayName = meta.original?.trim() || n;
      const titleFull = displayName === n ? n : `${displayName} (${n})`;

      chip.innerHTML = `<div class="top"><div class="name" title="${escapeHtml(titleFull)}">${escapeHtml(displayName)}</div><div class="actions"></div></div><div class="ttl">${fmtRemain(remain)}</div>`;
      
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
        fetch(`/d/${encodeURIComponent(n)}`, { method: "DELETE" }).then((r) => {
          if (r.ok) {
            this.ownedCache.delete(n);
            this.ownedMeta.delete(n);
            deleteHandler.removeFromUploads(n);
            this.renderOwned();
          }
        });
      });

      chip.querySelector(".actions").append(copyBtn, delBtn);
      let grid = ownedList.querySelector(".owned-grid") || document.createElement("div");
      grid.className = "owned-grid";
      ownedList.appendChild(grid);
      grid.appendChild(chip);
    });
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
    this.renderOwned();
  },
};