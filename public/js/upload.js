// js/upload.js


import { list } from './ui.js';
import { fmtBytes, showSnack, copyToClipboard, flashCopied } from './utils.js';
import { getTTL } from './ui.js';
import { ownedHandler } from './owned.js';
import { deleteHandler } from './delete.js';

const MIN_CHUNK_SIZE = 64 * 1024; // 64 KiB (backend minimum)
const MAX_CHUNK_SIZE = 32 * 1024 * 1024; // 32 MiB (backend maximum)
const DEFAULT_CHUNK_SIZE = 8 * 1024 * 1024; // 8 MiB (backend default)
const DEFAULT_CHUNK_THRESHOLD = 128 * 1024 * 1024; // 128 MiB
const MAX_TOTAL_CHUNKS = 20_000;
const STREAMING_OPT_IN =
  typeof window !== "undefined" && window.ENABLE_STREAMING_UPLOADS === true;

export function shouldUseChunk(file) {
  if (!file) return false;
  const override = window.CHUNK_THRESHOLD_BYTES;
  const threshold =
    typeof override === "number" && override > 0 ? override : DEFAULT_CHUNK_THRESHOLD;
  if (window.MAX_FILE_BYTES && file.size > window.MAX_FILE_BYTES) {
    return true;
  }
  return file.size >= threshold;
}

export function selectChunkSize(fileSize) {
  const override = window.PREFERRED_CHUNK_SIZE_BYTES;
  let chunkSize =
    typeof override === "number" && override > 0 ? override : DEFAULT_CHUNK_SIZE;
  chunkSize = Math.max(MIN_CHUNK_SIZE, Math.min(MAX_CHUNK_SIZE, chunkSize));
  const minChunk = Math.ceil(fileSize / MAX_TOTAL_CHUNKS);
  if (minChunk > chunkSize) {
    chunkSize = Math.min(MAX_CHUNK_SIZE, Math.max(MIN_CHUNK_SIZE, minChunk));
  }
  return chunkSize;
}

export const uploadHandler = {
  batches: [],
  uploading: false,
  streamingAllowed: STREAMING_OPT_IN,

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
        abortController: null,
        chunkSessionId: null,
        chunkSize: null,
        totalChunks: null,
        uploadedBytes: 0,
        hash: null,
        statusEl: null,
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
        const frag = document.createDocumentFragment();
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
          frag.appendChild(entry);
        });
        filesWrap.appendChild(frag);
        return;
      }
      // Legacy single-file path
      const frag = document.createDocumentFragment();
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
        frag.appendChild(li);
        li.classList.add("adding");
        requestAnimationFrame(() => li.classList.add("in"));
      });
      list.appendChild(frag);
    });
  },

  makeLinkInput(rel, opts = {}) {
    const options = typeof opts === "boolean" ? { autoCopy: opts } : opts;
    const { autoCopy = true, pending = false } = options;
    const full = location.origin + "/" + rel;
    const inp = document.createElement("input");
    inp.type = "text";
    inp.readOnly = true;
    inp.value = full;
    inp.className = "link-input";
    inp.title = "Click to copy direct download link";
    inp.setAttribute("aria-label", "Download link (click to copy)");
    if (pending) {
      inp.classList.add("pending");
      inp.dataset.status = "pending";
      inp.title = "Finalizing upload… link may briefly be unavailable";
      inp.setAttribute("aria-live", "polite");
    }
    if (autoCopy && !pending) {
      copyToClipboard(full).then(() => flashCopied());
    }
    inp.addEventListener("click", () => {
      inp.select();
      copyToClipboard(inp.value).then(() => flashCopied());
    });
    // The patch for addOwned will be applied in enhancements.js
    return inp;
  },

  ensureLinkContainer(f, batch) {
    if (!f?.container) return null;
    if (batch?.isGroup) {
      let linksRow = f.container.querySelector(".links");
      if (!linksRow) {
        linksRow = document.createElement("div");
        linksRow.className = "links";
        f.container.appendChild(linksRow);
      }
      f.linkContainer = linksRow;
      return linksRow;
    }
    if (!f.linkContainer || !f.linkContainer.isConnected) {
      const links = document.createElement("div");
      links.className = "links";
      f.container.appendChild(links);
      f.linkContainer = links;
    }
    return f.linkContainer;
  },

  ensureLinkInput(f, batch, options = {}) {
    if (!f || !f.remoteName) return null;
    const rel = f.remoteName.startsWith("f/") ? f.remoteName : `f/${f.remoteName}`;
    const container = this.ensureLinkContainer(f, batch);
    if (!container) return null;
    const pending = !!options.pending;
    const autoCopy = !!options.autoCopy;
    const highlight = !!options.highlight;

    if (!f.linkInput || !f.linkInput.isConnected) {
      f.linkInput = this.makeLinkInput(rel, { autoCopy: autoCopy && !pending, pending });
      container.appendChild(f.linkInput);
      f.linkAutoCopied = autoCopy && !pending;
      if (!pending) {
        f.linkInput.dataset.status = "ready";
      }
    } else {
      f.linkInput.value = location.origin + "/" + rel;
      f.linkInput.classList.toggle("pending", pending);
      f.linkInput.dataset.status = pending ? "pending" : "ready";
      f.linkInput.title = pending
        ? "Finalizing upload… link may briefly be unavailable"
        : "Click to copy direct download link";
      if (pending) {
        f.linkInput.setAttribute("aria-live", "polite");
        f.linkAutoCopied = false;
      } else {
        f.linkInput.removeAttribute("aria-live");
        if (autoCopy && !f.linkAutoCopied) {
          copyToClipboard(f.linkInput.value).then(() => flashCopied());
          f.linkAutoCopied = true;
        }
      }
    }

    if (!pending && highlight) {
      f.linkInput.classList.add("ready-flash");
      setTimeout(() => f.linkInput?.classList.remove("ready-flash"), 800);
    }

    return f.linkInput;
  },

  removeLinkForFile(f) {
    if (f?.linkInput) {
      f.linkInput.remove();
      f.linkInput = null;
    }
    if (f?.linkContainer && f.linkContainer.childElementCount === 0) {
      f.linkContainer.parentNode?.removeChild(f.linkContainer);
      f.linkContainer = null;
    }
    if (f) {
      f.linkAutoCopied = false;
    }
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
      // Calculate hash and check with server before uploading for non-chunked files
      try {
        if (this.shouldUseChunk(f.file)) {
          f.hash = null;
        } else {
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
        }
      } catch (e) {
        // If hash fails, proceed with upload anyway
        if (window.DEBUG_LOGS) console.warn("Hash check failed, proceeding with upload", e);
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
    // Use a Web Worker for hashing to avoid blocking the main thread
    if (!window._fileHashWorker) {
      window._fileHashWorker = new Worker('/js/file-hash-worker.js');
      window._fileHashWorker._pending = [];
      window._fileHashWorker.onmessage = function(e) {
        const cb = window._fileHashWorker._pending.shift();
        if (cb) cb(e.data);
      };
    }
    return new Promise((resolve, reject) => {
      window._fileHashWorker._pending.push((data) => {
        if (data.hash) resolve(data.hash);
        else reject(new Error(data.error || 'Hashing failed'));
      });
      window._fileHashWorker.postMessage(file);
    });
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

  async uploadOne(f, batch) {
    f.errorMessage = null;
    const ttlVal = getTTL();
    try {
      if (this.shouldUseChunk(f.file)) {
        await this.uploadChunked(f, batch, ttlVal);
      } else {
        await this.uploadMultipart(f, batch, ttlVal);
      }
    } catch (err) {
      if (err?.name === "AbortError" || f.canceled) {
        return;
      }
      if (window.DEBUG_LOGS) console.error("Upload failed", err);
    }
  },

  shouldUseChunk,

  selectChunkSize,

  updateProgressBar(f, loaded, total) {
    if (!f.barSpan || !total) return;
    const pct = Math.min(100, Math.max(0, (loaded / total) * 100));
    f.barSpan.style.width = pct.toFixed(2) + "%";
  },

  setStatusMessage(f, message) {
    if (!f.container) return;
    if (message) {
      if (!f.statusEl) {
        const el = document.createElement("div");
        el.className = "status-note";
        f.container.appendChild(el);
        f.statusEl = el;
      }
      f.statusEl.textContent = message;
      f.statusEl.style.display = "block";
    } else if (f.statusEl) {
      f.statusEl.remove();
      f.statusEl = null;
    }
  },

  async uploadMultipart(f, batch, ttlVal) {
    return new Promise((resolve) => {
      const fd = new FormData();
      fd.append("ttl", ttlVal);
      fd.append("file", f.file, f.file.name);
      const xhr = new XMLHttpRequest();
      f.xhr = xhr;
      xhr.open("POST", "/upload");
      xhr.responseType = "json";
      let finished = false;

      xhr.upload.onprogress = (e) => {
        if (e.lengthComputable) {
          this.updateProgressBar(f, e.loaded, e.total || f.file.size);
        }
      };

      xhr.onload = () => {
        if (finished || f.canceled) return resolve();
        finished = true;
        let payload = xhr.response;
        if (!payload) {
          try {
            payload = JSON.parse(xhr.responseText || "{}");
          } catch {
            payload = {};
          }
        }
        if (xhr.status === 409) {
          this.markFileDuplicate(f, batch, payload);
          return resolve();
        }
        if (xhr.status >= 200 && xhr.status < 300) {
          this.handleUploadSuccess(f, batch, payload);
        } else {
          const msg = (payload && payload.message) || "Upload failed.";
          showSnack(msg);
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

  async uploadChunked(f, batch, ttlVal) {
    const abort = new AbortController();
    f.abortController = abort;
    this.setStatusMessage(f, "Preparing...");
    const chunkSize = this.selectChunkSize(f.file.size);
    const initPayload = {
      filename: f.file.name,
      size: f.file.size,
      ttl: ttlVal,
      chunk_size: chunkSize,
    };
    if (f.hash) {
      initPayload.hash = f.hash;
    }

    let initResponse;
    try {
      initResponse = await fetch("/chunk/init", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(initPayload),
        signal: abort.signal,
      });
    } catch (err) {
      if (err?.name === "AbortError") throw err;
      showSnack("Failed to start chunked upload.");
      f.container?.classList.add("error");
      throw err;
    }

    if (initResponse.status === 409) {
      const dupPayload = await initResponse.json().catch(() => ({}));
      this.setStatusMessage(f, "");
      this.markFileDuplicate(f, batch, dupPayload);
      return;
    }

    if (!initResponse.ok) {
      const message = await this.extractError(initResponse, "Failed to start chunked upload.");
      showSnack(message);
      f.container?.classList.add("error");
      this.setStatusMessage(f, "");
      throw new Error(message);
    }

    const initData = await initResponse.json().catch(() => ({}));
    f.chunkSessionId = initData.session_id;
    if (!f.chunkSessionId) {
      showSnack("Server did not return an upload session.");
      throw new Error("Missing chunk session id");
    }
    if (initData.storage_name) {
      f.remoteName = initData.storage_name.startsWith("f/")
        ? initData.storage_name.slice(2)
        : initData.storage_name;
    }
    f.chunkSize = initData.chunk_size || chunkSize;
    f.totalChunks = initData.total_chunks || Math.ceil(f.file.size / f.chunkSize);
    f.uploadedBytes = 0;

    try {
      await this.uploadChunksSequentially(f, abort);
    } catch (err) {
      if (err?.name !== "AbortError") {
        const message = err.message || "Chunk upload failed.";
        showSnack(message);
        f.container?.classList.add("error");
      }
      this.setStatusMessage(f, "");
      this.removeLinkForFile(f);
      await this.cancelChunkSession(f);
      throw err;
    }

    if (f.remoteName) {
      this.ensureLinkInput(f, batch, { pending: true, autoCopy: false });
    }
    this.setStatusMessage(f, "Finalizing...");

    let completeResponse;
    try {
      completeResponse = await fetch(`/chunk/${encodeURIComponent(f.chunkSessionId)}/complete`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ hash: f.hash || null }),
        signal: abort.signal,
      });
    } catch (err) {
      if (err?.name === "AbortError") {
        this.setStatusMessage(f, "");
        await this.cancelChunkSession(f);
        throw err;
      }
      showSnack("Failed to finalize upload.");
      f.container?.classList.add("error");
      await this.cancelChunkSession(f);
      this.setStatusMessage(f, "");
      this.removeLinkForFile(f);
      throw err;
    }

    if (completeResponse.status === 409) {
      const dupPayload = await completeResponse.json().catch(() => ({}));
      await this.cancelChunkSession(f);
      this.setStatusMessage(f, "");
      this.removeLinkForFile(f);
      this.markFileDuplicate(f, batch, dupPayload);
      return;
    }

    if (!completeResponse.ok) {
      const message = await this.extractError(completeResponse, "Upload failed.");
      showSnack(message);
      f.container?.classList.add("error");
      await this.cancelChunkSession(f);
      this.setStatusMessage(f, "");
      this.removeLinkForFile(f);
      throw new Error(message);
    }

    const payload = await completeResponse.json().catch(() => ({}));
    this.handleUploadSuccess(f, batch, payload);
    f.chunkSessionId = null;
    f.abortController = null;
  },

  async uploadChunksSequentially(f, abort) {
    const sessionId = encodeURIComponent(f.chunkSessionId);
    const chunkBytes = f.chunkSize || this.selectChunkSize(f.file.size);
    const totalChunks = f.totalChunks || Math.ceil(f.file.size / chunkBytes);
    for (let index = 0; index < totalChunks; index++) {
      if (abort.signal.aborted) throw new DOMException("Aborted", "AbortError");
      const start = index * chunkBytes;
      const end = Math.min(start + chunkBytes, f.file.size);
      const slice = f.file.slice(start, end);
      const send = async (useStream) => {
        const body = useStream && typeof slice.stream === "function" ? slice.stream() : slice;
        const options = {
          method: "PUT",
          headers: { "Content-Type": "application/octet-stream" },
          body,
          signal: abort.signal,
        };
        if (useStream && body && typeof body === "object" && typeof body.getReader === "function") {
          options.duplex = "half";
        }
        return fetch(`/chunk/${sessionId}/${index}`, options);
      };

      const canStream = this.streamingAllowed && typeof slice.stream === "function";
      let response = null;
      let streamError = null;

      if (canStream) {
        try {
          response = await send(true);
        } catch (err) {
          streamError = err;
          this.streamingAllowed = false;
          if (typeof window !== "undefined") {
            window.ENABLE_STREAMING_UPLOADS = false;
          }
          if (window.DEBUG_LOGS) {
            console.warn("Streaming chunk upload failed, falling back to buffered mode", err);
          }
        }
      }

      if (response && !response.ok && canStream && response.status === 400) {
        this.streamingAllowed = false;
        if (typeof window !== "undefined") {
          window.ENABLE_STREAMING_UPLOADS = false;
        }
        if (window.DEBUG_LOGS) {
          console.warn("Chunk upload returned 400 with streaming body; retrying without streaming");
        }
        response = null;
      }

      if (!response) {
        try {
          response = await send(false);
        } catch (err) {
          throw streamError || err;
        }
      }

      if (!response.ok) {
        const message = await this.extractError(response, "Chunk upload failed.");
        throw new Error(message);
      }
      f.uploadedBytes = end;
      this.updateProgressBar(f, f.uploadedBytes, f.file.size);
    }
  },

  async extractError(response, fallback) {
    try {
      const data = await response.json();
      if (data && data.message) return data.message;
    } catch {
      // ignore JSON errors
    }
    return fallback;
  },

  findFileEntryByRemoteName(remoteName) {
    if (!remoteName) return null;
    for (const batch of this.batches) {
      for (const fileEntry of batch.files) {
        if (fileEntry.remoteName === remoteName) {
          return { file: fileEntry, batch };
        }
      }
    }
    return null;
  },

  async cancelChunkSession(f) {
    if (!f.chunkSessionId) {
      f.abortController = null;
      return;
    }
    const sessionId = f.chunkSessionId;
    f.chunkSessionId = null;
    try {
      await fetch(`/chunk/${encodeURIComponent(sessionId)}/cancel`, { method: "DELETE" });
    } catch {
      // ignore network errors on cancel
    } finally {
      f.abortController = null;
    }
  },

  cancelPendingUpload(f) {
    f.canceled = true;
    if (f.xhr) {
      try {
        f.xhr.abort();
      } catch {
        // ignore
      }
    }
    if (f.abortController) {
      try {
        f.abortController.abort();
      } catch {
        // ignore
      }
    }
    this.removeLinkForFile(f);
    return this.cancelChunkSession(f);
  },

  extractUploadedPath(payload) {
    if (!payload) return null;
    if (Array.isArray(payload.files) && payload.files.length) {
      return payload.files[0];
    }
    if (payload.file) return payload.file;
    if (payload.meta && payload.meta.file) return payload.meta.file;
    return null;
  },

  markFileDuplicate(f, batch, payload) {
    this.setStatusMessage(f, "");
    this.removeLinkForFile(f);
    try {
      showSnack("Duplicate file: already uploaded.");
    } catch {}
    const rel = this.extractUploadedPath(payload);
    if (rel) {
      const name = rel.startsWith("f/") ? rel.slice(2) : rel;
      if (ownedHandler.highlightOwned) ownedHandler.highlightOwned(name);
      const existing = this.findFileEntryByRemoteName(name);
      if (existing && existing.file && existing.file.container) {
        existing.file.container.classList.add("dupe-highlight");
        setTimeout(() => {
          existing.file.container?.classList.remove("dupe-highlight");
        }, 1800);
      }
    }
    f.removed = true;
    if (f.container) {
      f.container.classList.add("dupe-remove");
      setTimeout(() => {
        f.container?.parentNode?.removeChild(f.container);
      }, 400);
    }
    batch.files = batch.files.filter((x) => x !== f);
    if (batch.isGroup && batch.files.length === 0 && batch.groupLi) {
      batch.groupLi.classList.add("dupe-remove");
      setTimeout(() => batch.groupLi?.parentNode?.removeChild(batch.groupLi), 400);
    }
    if (!batch.files.length) {
      this.batches = this.batches.filter((b) => b !== batch);
    }
    deleteHandler.updateDeleteButton(f);
  },

  handleUploadSuccess(f, batch, payload) {
    this.setStatusMessage(f, "");
    f.uploadedBytes = f.file.size;
    if (f.barSpan) {
      f.barSpan.style.width = "100%";
      requestAnimationFrame(() => {
        f.barSpan.classList.add("complete");
        setTimeout(() => f.bar?.classList.add("divider"), 1000);
      });
    }
    const rel = this.extractUploadedPath(payload);
    if (rel) {
      const remote = rel.startsWith("f/") ? rel.slice(2) : rel;
      f.remoteName = remote;
      ownedHandler.addOwned(remote);
    }
    f.done = true;
    deleteHandler.updateDeleteButton(f);
    if (!f.remoteName) return;
    const autoCopy = !batch.files.some((x) => !x.done);
    this.ensureLinkInput(f, batch, {
      autoCopy,
      pending: false,
      highlight: true,
    });
  },

  autoUpload() {
    // Use concurrent upload with a limit (e.g., 4)
    this.uploadConcurrent(4);
  },
};