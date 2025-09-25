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


  // --- Concurrency Patch ---
  async uploadConcurrent(concurrency = 4) {
    if (this.uploading) return;
    this.uploading = true;
    const allFiles = [];
    for (const batch of this.batches) {
      for (const f of batch.files) {
        if (!f.removed && !f.done && !f.deleting) {
          allFiles.push({ f, batch });
        }
      }
    }
    let idx = 0;
    const uploadNext = async () => {
      if (idx >= allFiles.length) return;
      const { f, batch } = allFiles[idx++];
      // Calculate hash and check with server before uploading
      try {
        f.hash = await this.calculateFileHash(f.file);
        const exists = await this.checkFileHash(f.hash);
        if (exists) {
          showSnack("Duplicate file: already uploaded.");
          f.done = true;
          f.removed = true;
          if (f.container) {
            f.container.classList.add("dupe-remove");
            setTimeout(() => {
              if (f.container && f.container.parentNode) {
                f.container.parentNode.removeChild(f.container);
              }
            }, 400);
          }
          return uploadNext();
        }
      } catch (e) {
        // If hash fails, proceed with upload anyway
        console.warn("Hash check failed, proceeding with upload", e);
      }
      await this.uploadOne(f, batch);
      await uploadNext();
    };
    // Start N concurrent uploads
    const workers = [];
    for (let i = 0; i < concurrency; i++) {
      workers.push(uploadNext());
    }
    await Promise.all(workers);
    // Clean up removed files and empty batches
    for (const batch of this.batches) {
      batch.files = batch.files.filter((f) => !f.removed);
    }
    this.batches = this.batches.filter((b) => b.files.length);
    this.uploading = false;
  },

  async calculateFileHash(file) {
    // Returns a hex string of the SHA-256 hash of the file using Web Crypto API
    const buffer = await file.arrayBuffer();
    const hashBuffer = await crypto.subtle.digest('SHA-256', buffer);
    // Convert buffer to hex string
    const hashArray = Array.from(new Uint8Array(hashBuffer));
    return hashArray.map(b => b.toString(16).padStart(2, '0')).join('');
  },

  async checkFileHash(hash) {
    // Returns true if file with this hash exists on server
    try {
      const resp = await fetch(`/checkhash?hash=${encodeURIComponent(hash)}`);
      if (!resp.ok) return false;
      const data = await resp.json();
      return !!data.exists;
    } catch {
      return false;
    }
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
        // Handle 409 Conflict (duplicate file hash)
        if (xhr.status === 409) {
          try {
            showSnack("Duplicate file: already uploaded.");
          } catch {}
          // Try to get the original file hash from response
          let origHash = null;
          try {
            const data = xhr.response || JSON.parse(xhr.responseText || "{}{}");
            origHash = data.hash || data.file_hash || (data.files && data.files[0]);
          } catch {}
          // Highlight the original file in the list if possible
          if (origHash) {
            // Try to find a file entry in any batch with this hash as remoteName
            let found = null;
            for (const b of this.batches) {
              for (const fileObj of b.files) {
                if (fileObj.remoteName === origHash) {
                  found = fileObj;
                  break;
                }
              }
              if (found) break;
            }
            if (found && found.container) {
              found.container.classList.add("dupe-highlight");
              setTimeout(() => found.container.classList.remove("dupe-highlight"), 1800);
            }
          }
          // Animate removal of this file from the list
          if (f.container) {
            f.container.classList.add("dupe-remove");
            setTimeout(() => {
              if (f.container && f.container.parentNode) {
                f.container.parentNode.removeChild(f.container);
              }
              f.removed = true;
              // Remove from batch.files
              batch.files = batch.files.filter(x => x !== f);
              // If group batch and now empty, remove groupLi
              if (batch.isGroup && batch.files.length === 0 && batch.groupLi) {
                batch.groupLi.classList.add("dupe-remove");
                setTimeout(() => {
                  batch.groupLi?.parentNode?.removeChild(batch.groupLi);
                }, 400);
              }
            }, 400);
          } else {
            f.removed = true;
            batch.files = batch.files.filter(x => x !== f);
          }
          deleteHandler.updateDeleteButton(f);
          return resolve();
        }
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
            const data = xhr.response || JSON.parse(xhr.responseText || "{}{}");
            rel = data.files && data.files[0];
          } catch {}
          if (rel) {
            f.remoteName = rel.startsWith("f/") ? rel.slice(2) : rel;
            ownedHandler.addOwned(f.remoteName); // Add to owned list
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
    // Use concurrent upload with a limit (e.g., 4)
    this.uploadConcurrent(4);
  },
};