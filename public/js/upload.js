// js/upload.js

import { list } from './ui.js';
import { fmtBytes, showSnack, copyToClipboard, flashCopied, ttlCodeSeconds } from './utils.js';
import { getTTL } from './ui.js';
import { ownedHandler } from './owned.js';
import { deleteHandler } from './delete.js';

export const uploadHandler = {
  batches: [],
  uploading: false,

  addBatch(fileList) {
    if (!fileList || !fileList.length) return;
    let skippedEmpty = false;
    const cleaned = [...fileList].filter((f) => {
      if (f.size === 0) {
        skippedEmpty = true;
        return false;
      }
      return true;
    });
    if (skippedEmpty) {
      try {
        showSnack("Skipped empty files");
      } catch {}
    }
    if (!cleaned.length) return;

    const batch = {
      files: cleaned.map((f) => ({
        file: f,
        remoteName: null,
        done: false,
        deleting: false,
        bar: null,
        barSpan: null,
        container: null,
        linksBox: null,
        xhr: null,
      })),
      isGroup: cleaned.length > 1,
    };
    this.batches.push(batch);
    this.renderList();
    this.autoUpload();
  },

  renderList() {
    this.batches.forEach((batch) => {
      if (batch.isGroup) {
        if (!batch.groupLi) {
          const li = document.createElement("li");
          li.className = "group-batch";
          li.innerHTML =
            '<div class="file-row group-head"><div class="name"></div><div class="size"></div><div class="actions"></div></div><div class="group-files"></div>';
          li.querySelector(".group-head .name").textContent = batch.files.length + " files";
          list.appendChild(li);
          batch.groupLi = li;
          li.classList.add("adding");
          requestAnimationFrame(() => li.classList.add("in"));
        }
        const filesWrap = batch.groupLi.querySelector(".group-files");
        batch.files.forEach((f) => {
          if (f.container) return;
          const entry = document.createElement("div");
          entry.className = "file-entry";
          entry.innerHTML =
            '<div class="file-row"><div class="name"></div><div class="size"></div><div class="actions"></div></div><div class="bar"><span></span></div>';
          entry.querySelector(".name").textContent = f.file.name;
          entry.querySelector(".size").textContent = fmtBytes(f.file.size);
          const del = document.createElement("button");
          del.type = "button";
          del.className = "remove";
          del.textContent = "x";
          del.title = "Remove";
          del.setAttribute("aria-label", "Remove file from queue");
          del.addEventListener("click", (e) => {
            e.stopPropagation();
            deleteHandler.handleDeleteClick(f, batch);
          });
          f.deleteBtn = del;
          entry.querySelector(".actions").appendChild(del);
          f.bar = entry.querySelector(".bar");
          f.barSpan = f.bar.querySelector("span");
          f.container = entry;
          filesWrap.appendChild(entry);
        });
        return;
      }
      // Legacy single-file path
      batch.files.forEach((f) => {
        if (f.container) return;
        const li = document.createElement("li");
        f.container = li;
        li.innerHTML = `<div class="file-row"><div class="name">${f.file.name}</div><div class="size">${fmtBytes(f.file.size)}</div><div class="actions"></div></div><div class="bar"><span></span></div>`;
        const del = document.createElement("button");
        del.type = "button";
        del.className = "remove";
        del.textContent = "x";
        del.title = "Remove";
        del.setAttribute("aria-label", "Remove file from queue");
        del.addEventListener("click", (e) => {
            e.stopPropagation();
            deleteHandler.handleDeleteClick(f, batch);
        });
        f.deleteBtn = del;
        li.querySelector(".actions").appendChild(del);
        f.bar = li.querySelector(".bar");
        f.barSpan = f.bar.querySelector("span");
        list.appendChild(li);
        li.classList.add("adding");
        requestAnimationFrame(() => li.classList.add("in"));
      });
    });
  },

  makeLinkInput(rel, autoCopy = true) {
    const full = location.origin + "/" + rel;
    const inp = document.createElement("input");
    inp.type = "text";
    inp.readOnly = true;
    inp.value = full;
    inp.className = "link-input";
    inp.title = "Click to copy direct download link";
    inp.setAttribute("aria-label", "Download link (click to copy)");
    if (autoCopy) {
      copyToClipboard(full).then(() => flashCopied());
    }
    inp.addEventListener("click", () => {
      inp.select();
      copyToClipboard(inp.value).then(() => flashCopied());
    });
    // The patch for addOwned will be applied in enhancements.js
    return inp;
  },

  async uploadSequential() {
    if (this.uploading) return;
    this.uploading = true;
    for (const batch of this.batches) {
      for (let i = 0; i < batch.files.length; i++) {
        const f = batch.files[i];
        if (!f || f.removed || f.done || f.deleting) continue;
        await this.uploadOne(f, batch);
      }
      batch.files = batch.files.filter((f) => !f.removed);
    }
    this.batches = this.batches.filter((b) => b.files.length);
    this.uploading = false;
  },

  uploadOne(f, batch) {
    return new Promise((resolve) => {
      const fd = new FormData();
      const ttlVal = getTTL();
      fd.append("ttl", ttlVal);
      fd.append("file", f.file, f.file.name);
      const xhr = new XMLHttpRequest();
      f.xhr = xhr;
      xhr.open("POST", "/upload");
      xhr.responseType = "json";
      let finished = false;

      xhr.upload.onprogress = (e) => {
        if (e.lengthComputable && f.barSpan) {
          const pct = (e.loaded / f.file.size) * 100;
          f.barSpan.style.width = pct.toFixed(2) + "%";
        }
      };

      xhr.onload = () => {
        if (finished || f.canceled) return resolve();
        finished = true;
        const ok = xhr.status >= 200 && xhr.status < 300;
        if (ok) {
          if (f.barSpan) {
            f.barSpan.style.width = "100%";
            requestAnimationFrame(() => {
              f.barSpan.classList.add("complete");
              setTimeout(() => f.bar?.classList.add("divider"), 1000);
            });
          }
          let rel = null;
          try {
            const data = xhr.response || JSON.parse(xhr.responseText || "{}");
            rel = data.files && data.files[0];
          } catch {}
          if (rel) {
            f.remoteName = rel.startsWith("f/") ? rel.slice(2) : rel;
            ownedHandler.addOwned(f.remoteName); // Add to owned list
            const ttlSeconds = ttlCodeSeconds(ttlVal);
            const exp = Math.floor(Date.now() / 1000) + ttlSeconds;
            ownedHandler.ownedMeta.set(f.remoteName, {
              expires: exp,
              total: ttlSeconds,
              original: f.file?.name || "",
            });
          }
          f.done = true;
          deleteHandler.updateDeleteButton(f);
          if (f.remoteName) {
            const input = this.makeLinkInput("f/" + f.remoteName, !batch.files.some((x) => !x.done));
            if (batch.isGroup) {
                let linksRow = f.container.querySelector(".links") || document.createElement("div");
                linksRow.className = "links";
                f.container.appendChild(linksRow);
                linksRow.appendChild(input);
            } else {
                const links = document.createElement("div");
                links.className = "links";
                links.appendChild(input);
                f.container.appendChild(links);
            }
          }
        } else {
          f.container?.classList.add("error");
          deleteHandler.updateDeleteButton(f);
        }
        resolve();
      };

      xhr.onerror = xhr.onabort = () => {
        if (finished) return;
        finished = true;
        if (!f.canceled) f.container?.classList.add("error");
        deleteHandler.updateDeleteButton(f);
        resolve();
      };

      xhr.send(fd);
      deleteHandler.updateDeleteButton(f);
    });
  },

  autoUpload() {
    this.uploadSequential();
  },
};