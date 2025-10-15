// js/upload.js

import { list } from "./ui.js";
import { fmtBytes, showSnack, copyToClipboard, flashCopied } from "./utils.js";
import { getTTL } from "./ui.js";
import { ownedHandler } from "./owned.js";
import { deleteHandler } from "./delete.js";

const MIN_CHUNK_SIZE = 64 * 1024; // 64 KiB (backend minimum)
const MAX_CHUNK_SIZE = 32 * 1024 * 1024; // 32 MiB (backend maximum)
const DEFAULT_CHUNK_SIZE = 8 * 1024 * 1024; // 8 MiB (backend default)
const DEFAULT_CHUNK_THRESHOLD = 128 * 1024 * 1024; // 128 MiB
const MAX_TOTAL_CHUNKS = 20_000;
const STREAMING_OPT_IN =
  typeof window !== "undefined" && window.ENABLE_STREAMING_UPLOADS === true;
const ACTIVE_STATUS_STATES = new Set([
  "hashing",
  "preparing",
  "uploading",
  "finalizing",
]);

export function shouldUseChunk(file) {
  if (!file) return false;
  const override = window.CHUNK_THRESHOLD_BYTES;
  const threshold =
    typeof override === "number" && override > 0
      ? override
      : DEFAULT_CHUNK_THRESHOLD;
  if (window.MAX_FILE_BYTES && file.size > window.MAX_FILE_BYTES) {
    return true;
  }
  return file.size >= threshold;
}

export function selectChunkSize(fileSize) {
  const override = window.PREFERRED_CHUNK_SIZE_BYTES;
  let chunkSize =
    typeof override === "number" && override > 0
      ? override
      : DEFAULT_CHUNK_SIZE;
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
        removed: false,
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
        hashPromise: null,
        hashChecked: false,
        statusEl: null,
        statusState: null,
        canceled: false,
        lastProgressPercent: -1,
      })),
      isGroup: cleaned.length > 1,
    };
    this.batches.push(batch);
    this.renderList();
    this.autoUpload();
    this.refreshQueueStatuses();
  },

  renderList() {
    this.batches.forEach((batch) => {
      if (batch.isGroup) {
        if (!batch.groupLi) {
          const li = document.createElement("li");
          li.className = "group-batch";
          li.innerHTML =
            '<div class="file-row group-head"><div class="name"></div><div class="size"></div><div class="actions"></div></div><div class="group-files"></div>';
          li.querySelector(".group-head .name").textContent =
            batch.files.length + " files";
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
        li.innerHTML =
          '<div class="file-row"><div class="name"></div><div class="size"></div><div class="actions"></div></div><div class="bar"><span></span></div>';
        const nameEl = li.querySelector(".name");
        const sizeEl = li.querySelector(".size");
        if (nameEl) nameEl.textContent = f.file.name;
        if (sizeEl) sizeEl.textContent = fmtBytes(f.file.size);
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
      inp.title = "Checking chunks… link may briefly be unavailable";
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
    const rel = f.remoteName.startsWith("f/")
      ? f.remoteName
      : `f/${f.remoteName}`;
    const container = this.ensureLinkContainer(f, batch);
    if (!container) return null;
    const pending = !!options.pending;
    const autoCopy = !!options.autoCopy;
    const highlight = !!options.highlight;

    if (!f.linkInput || !f.linkInput.isConnected) {
      f.linkInput = this.makeLinkInput(rel, {
        autoCopy: autoCopy && !pending,
        pending,
      });
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
        ? "Checking chunks… link may briefly be unavailable"
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

  collectPendingEntries() {
    const entries = [];
    for (const batch of this.batches) {
      for (const f of batch.files) {
        if (f.removed || f.done || f.deleting) continue;
        entries.push({ f, batch });
      }
    }
    return entries;
  },

  refreshQueueStatuses() {
    const entries = this.collectPendingEntries().filter(({ f }) => !f.canceled);
    const waiting = entries.filter(
      ({ f }) => !ACTIVE_STATUS_STATES.has(f.statusState || "")
    );
    const total = waiting.length;
    if (total === 0) {
      for (const { f } of entries) {
        if (f.statusState === "queue") {
          this.setStatusMessage(f, "");
        }
      }
      return;
    }
    waiting.forEach(({ f }, index) => {
      const position = index + 1;
      const message = total > 1 ? `Queued (${position} of ${total})` : "Queued";
      this.setStatusMessage(f, message, {
        state: "queue",
        ariaLive: position === 1 ? "polite" : "off",
      });
    });
  },

  // --- Concurrency Patch ---
  async uploadConcurrent(concurrency = 4) {
    if (this.uploading) return;
    this.uploading = true;
    const allFiles = this.collectPendingEntries();
    if (!allFiles.length) {
      this.uploading = false;
      return;
    }
    let idx = 0;
    this.refreshQueueStatuses();
    const uploadNext = async () => {
      while (true) {
        const current = idx++;
        if (current >= allFiles.length) return;
        const { f, batch } = allFiles[current];
        if (f.canceled || f.removed) {
          this.refreshQueueStatuses();
          continue;
        }

        if (!f.hashChecked) {
          const shouldShowStatus = this.shouldUseChunk(f.file) && !f.canceled;
          try {
            if (shouldShowStatus) {
              this.setStatusMessage(f, "Calculating checksum…", {
                state: "hashing",
                ariaLive: current === 0 ? "assertive" : "polite",
              });
            }
            if (!f.hashPromise) {
              f.hashPromise = this.calculateFileHash(f.file);
            }
            const hash = await f.hashPromise;
            if (f.canceled) {
              if (shouldShowStatus) this.setStatusMessage(f, "");
              this.refreshQueueStatuses();
              continue;
            }
            f.hash = hash;
            const exists = await this.checkFileHash(hash);
            if (f.canceled) {
              if (shouldShowStatus) this.setStatusMessage(f, "");
              this.refreshQueueStatuses();
              continue;
            }
            f.hashChecked = true;
            if (exists) {
              if (shouldShowStatus) this.setStatusMessage(f, "");
              this.handlePreUploadDuplicate(f, batch);
              this.refreshQueueStatuses();
              continue;
            }
          } catch (e) {
            f.hashChecked = true;
            if (window.DEBUG_LOGS)
              console.warn("Hash check failed, proceeding with upload", e);
          } finally {
            if (shouldShowStatus && !f.canceled) {
              this.setStatusMessage(f, "");
            }
            this.refreshQueueStatuses();
          }
        }

        if (f.canceled || f.removed) {
          this.refreshQueueStatuses();
          continue;
        }

        this.refreshQueueStatuses();
        await this.uploadOne(f, batch);
        this.refreshQueueStatuses();
      }
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
    this.refreshQueueStatuses();
    this.uploading = false;
  },

  async calculateFileHash(file) {
    // Use a Web Worker for hashing to avoid blocking the main thread
    if (!window._fileHashWorker) {
      window._fileHashWorker = new Worker("/js/file-hash-worker.js");
      window._fileHashWorker._pending = [];
      window._fileHashWorker.onmessage = function (e) {
        const cb = window._fileHashWorker._pending.shift();
        if (cb) cb(e.data);
      };
    }
    return new Promise((resolve, reject) => {
      window._fileHashWorker._pending.push((data) => {
        if (data.hash) resolve(data.hash);
        else reject(new Error(data.error || "Hashing failed"));
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
    if (f.canceled) return;
    f.errorMessage = null;
    const ttlVal = getTTL();
    try {
      if (this.shouldUseChunk(f.file)) {
        if (f.canceled) return;
        await this.uploadChunked(f, batch, ttlVal);
      } else {
        if (f.canceled) return;
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

  now() {
    if (
      typeof performance !== "undefined" &&
      typeof performance.now === "function"
    ) {
      return performance.now();
    }
    return Date.now();
  },

  initChunkSmoothing(f) {
    f.chunkSmoothing = {
      avgBps: null,
      sampleCount: 0,
      lastDurationMs: null,
      lastChunkBytes: null,
      predictedPercent: -1,
      predictedBytes: null,
      active: null,
    };
  },

  beginChunkSmoothing(f, { baseStatus, startBytes, endBytes, isLastChunk }) {
    if (!f?.barSpan || !f?.file) return;
    if (
      typeof requestAnimationFrame !== "function" ||
      typeof cancelAnimationFrame !== "function"
    ) {
      return;
    }
    if (!f.chunkSmoothing) this.initChunkSmoothing(f);
    const smoothing = f.chunkSmoothing;
    const chunkBytes = Math.max(0, (endBytes || 0) - (startBytes || 0));
    if (!chunkBytes) {
      smoothing.active = null;
      return;
    }

    const avgBps = smoothing.avgBps;
    let expectedMs = null;
    if (avgBps && avgBps > 0) {
      expectedMs = (chunkBytes / avgBps) * 1000;
    } else if (smoothing.lastDurationMs && smoothing.lastDurationMs > 0) {
      const baselineBytes = smoothing.lastChunkBytes || chunkBytes;
      const ratio = baselineBytes ? chunkBytes / baselineBytes : 1;
      expectedMs =
        smoothing.lastDurationMs * Math.max(0.25, Math.min(5, ratio));
    }
    if (!expectedMs || !Number.isFinite(expectedMs) || expectedMs <= 24) {
      const fallbackBps = Math.max(256 * 1024, chunkBytes / 3);
      expectedMs = Math.max(400, (chunkBytes / fallbackBps) * 1000);
    }

    if (!expectedMs || !Number.isFinite(expectedMs) || expectedMs <= 0) {
      smoothing.active = null;
      return;
    }

    const activeId = Symbol("chunkPrediction");
    const handler = this;
    const maxHoldRatio = isLastChunk ? 0.999 : 0.996;
    const expectedBps = chunkBytes / (expectedMs / 1000);
    const baselineBps =
      smoothing.avgBps && smoothing.avgBps > 0 ? smoothing.avgBps : expectedBps;
    const maxDisplayBps = Math.max(8 * 1024 * 1024, baselineBps * 4);
    const minBps = Math.max(128, baselineBps * 0.12);
    smoothing.predictedPercent = Math.max(
      smoothing.predictedPercent ?? -1,
      f.lastProgressPercent ?? -1
    );
    const actualUploaded =
      typeof f.uploadedBytes === "number" ? f.uploadedBytes : startBytes;
    smoothing.predictedBytes = Math.max(
      actualUploaded,
      smoothing.predictedBytes ?? actualUploaded
    );

    const active = {
      id: activeId,
      baseStatus,
      startBytes,
      endBytes,
      expectedMs,
      isLastChunk,
      startTime: this.now(),
      lastTick: this.now(),
      targetSpeedBps: Math.min(Math.max(baselineBps, minBps), maxDisplayBps),
      currentSpeedBps: Math.min(Math.max(baselineBps, minBps), maxDisplayBps),
      minSpeedBps: minBps,
      maxSpeedBps: maxDisplayBps,
      raf: null,
    };

    const step = () => {
      if (!f.chunkSmoothing || f.chunkSmoothing.active?.id !== activeId) {
        return;
      }
      const nowTs = handler.now();
      const dtMs = nowTs - active.lastTick;
      if (dtMs <= 0) {
        active.raf = requestAnimationFrame(step);
        return;
      }
      active.lastTick = nowTs;
      const dtSec = dtMs / 1000;
      const elapsed = nowTs - active.startTime;
      const chunkSize = active.endBytes - active.startBytes;

      let targetSpeed = active.targetSpeedBps;
      if (smoothing.avgBps && smoothing.avgBps > 0) {
        targetSpeed = targetSpeed * 0.4 + smoothing.avgBps * 0.6;
      }
      if (elapsed > active.expectedMs) {
        const overtime = elapsed - active.expectedMs;
        const overRatio = overtime / (active.expectedMs + 1);
        const slowFactor = 1 / (1 + overRatio * 1.9);
        targetSpeed = Math.max(active.minSpeedBps, targetSpeed * slowFactor);
      }
      targetSpeed = Math.min(
        active.maxSpeedBps,
        Math.max(active.minSpeedBps, targetSpeed)
      );
      active.currentSpeedBps = active.currentSpeedBps * 0.7 + targetSpeed * 0.3;

      const cushionBytes = Math.max(
        256,
        chunkSize * (active.isLastChunk ? 0.0015 : 0.004)
      );
      const limitedCushion = Math.min(cushionBytes, chunkSize * 0.5);
      const upperBound = Math.max(
        active.startBytes,
        active.endBytes - limitedCushion
      );
      let predictedBytes = smoothing.predictedBytes ?? active.startBytes;
      predictedBytes += active.currentSpeedBps * dtSec;
      const actualBytes =
        typeof f.uploadedBytes === "number"
          ? f.uploadedBytes
          : active.startBytes;
      predictedBytes = Math.max(
        actualBytes,
        Math.min(upperBound, predictedBytes)
      );
      const ratio = chunkSize
        ? (predictedBytes - active.startBytes) / chunkSize
        : 0;
      const clampedRatio = Math.min(maxHoldRatio, Math.max(0, ratio));
      smoothing.predictedBytes = active.startBytes + chunkSize * clampedRatio;
      handler.applyPredictedProgress(f, smoothing.predictedBytes, {
        baseStatus: active.baseStatus,
      });
      active.raf = requestAnimationFrame(step);
    };

    smoothing.active = active;
    active.raf = requestAnimationFrame(step);
  },

  applyPredictedProgress(f, predictedBytes, { baseStatus }) {
    if (!f?.file || !f.barSpan) return;
    const total = f.file.size || 1;
    const capped = Math.min(predictedBytes, total * 0.9999);
    this.updateProgressBar(f, capped, total);

    const smoothing = f.chunkSmoothing;
    if (!smoothing) return;
    const percent = Math.floor(
      Math.min(99, Math.max(0, (capped / total) * 100))
    );
    if (percent <= (f.lastProgressPercent ?? -1)) return;
    if (percent <= smoothing.predictedPercent) return;
    smoothing.predictedPercent = percent;
    if (baseStatus) {
      this.setStatusMessage(f, `${baseStatus} (${percent}%)`, {
        state: "uploading",
        ariaLive: "off",
      });
    }
  },

  stopChunkSmoothing(f, { actualBytes = null } = {}) {
    const smoothing = f?.chunkSmoothing;
    if (!smoothing) return;
    const active = smoothing.active;
    if (active) {
      const { raf } = active;
      if (raf && typeof cancelAnimationFrame === "function") {
        cancelAnimationFrame(raf);
      }
    }
    smoothing.active = null;
    const fallbackBytes =
      typeof actualBytes === "number"
        ? actualBytes
        : typeof f?.uploadedBytes === "number"
        ? f.uploadedBytes
        : null;
    if (f?.file && typeof fallbackBytes === "number") {
      this.updateProgressBar(f, fallbackBytes, f.file.size || 1);
    }
    if (typeof fallbackBytes === "number") {
      smoothing.predictedBytes = fallbackBytes;
      const total = f?.file?.size || 1;
      if (total > 0) {
        smoothing.predictedPercent = Math.floor(
          Math.min(100, Math.max(0, (fallbackBytes / total) * 100))
        );
      }
    }
  },

  recordChunkTiming(f, { chunkBytes, durationMs }) {
    if (!f?.chunkSmoothing || !chunkBytes || !durationMs) return;
    if (!(durationMs > 0)) return;
    const smoothing = f.chunkSmoothing;
    const durationSec = durationMs / 1000;
    const bps = chunkBytes / durationSec;
    if (bps && Number.isFinite(bps) && bps > 0) {
      if (smoothing.avgBps) {
        smoothing.avgBps = smoothing.avgBps * 0.65 + bps * 0.35;
      } else {
        smoothing.avgBps = bps;
      }
    }
    smoothing.sampleCount += 1;
    smoothing.lastDurationMs = durationMs;
    smoothing.lastChunkBytes = chunkBytes;
    const total = f.file?.size || 1;
    const actualPercent = Math.floor(
      Math.min(99, Math.max(0, (f.uploadedBytes / total) * 100))
    );
    smoothing.predictedPercent = actualPercent;
    smoothing.predictedBytes = Math.max(
      smoothing.predictedBytes ?? f.uploadedBytes,
      f.uploadedBytes
    );
  },

  setStatusMessage(f, message, options = {}) {
    if (!f?.container) return;
    const text = typeof message === "string" ? message : "";
    const opts = options ?? {};
    const { ariaLive = "polite", state = null, persist = false } = opts;

    if (!text) {
      if (f.statusEl) {
        if (persist) {
          if (f.statusText) f.statusText.textContent = "";
          f.statusEl.style.display = "none";
          delete f.statusEl.dataset.state;
        } else {
          f.statusEl.remove();
          f.statusEl = null;
          f.statusIcon = null;
          f.statusText = null;
        }
      }
      f.statusState = null;
      return;
    }

    if (!f.statusEl) {
      const el = document.createElement("div");
      el.className = "status-note";
      el.setAttribute("role", "status");
      el.setAttribute("aria-live", ariaLive);
      const icon = document.createElement("span");
      icon.className = "status-icon";
      icon.setAttribute("aria-hidden", "true");
      const textWrap = document.createElement("span");
      textWrap.className = "status-text";
      el.append(icon, textWrap);
      f.container.appendChild(el);
      f.statusEl = el;
      f.statusIcon = icon;
      f.statusText = textWrap;
    } else {
      if (!f.statusIcon) {
        const existingIcon = f.statusEl.querySelector(".status-icon");
        if (existingIcon) {
          f.statusIcon = existingIcon;
        } else {
          const icon = document.createElement("span");
          icon.className = "status-icon";
          icon.setAttribute("aria-hidden", "true");
          f.statusEl.prepend(icon);
          f.statusIcon = icon;
        }
      }
      if (!f.statusText) {
        const existingText = f.statusEl.querySelector(".status-text");
        if (existingText) {
          f.statusText = existingText;
        } else {
          const textWrap = document.createElement("span");
          textWrap.className = "status-text";
          f.statusEl.appendChild(textWrap);
          f.statusText = textWrap;
        }
      }
    }

    f.statusEl.style.display = "flex";
    if (f.statusText) {
      f.statusText.textContent = text;
    }
    if (ariaLive) {
      f.statusEl.setAttribute("aria-live", ariaLive);
    } else {
      f.statusEl.removeAttribute("aria-live");
    }
    if (state) {
      f.statusEl.dataset.state = state;
    } else if (f.statusEl.dataset.state) {
      delete f.statusEl.dataset.state;
    }
    f.statusState = state || null;
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
      f.lastProgressPercent = 0;
      this.setStatusMessage(f, "Uploading…", {
        state: "uploading",
        ariaLive: "polite",
      });

      xhr.upload.onprogress = (e) => {
        if (e.lengthComputable) {
          const total = e.total || f.file.size || 1;
          this.updateProgressBar(f, e.loaded, total);
          const pct = Math.min(100, Math.max(0, (e.loaded / total) * 100));
          const rounded = Math.round(pct);
          if (rounded !== f.lastProgressPercent) {
            this.setStatusMessage(f, `Uploading… ${rounded}%`, {
              state: "uploading",
              ariaLive: "off",
            });
            f.lastProgressPercent = rounded;
          }
        }
      };

      xhr.onload = () => {
        if (finished || f.canceled) return resolve();
        finished = true;
        f.lastProgressPercent = -1;
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
        f.lastProgressPercent = -1;
        if (!f.canceled) f.container?.classList.add("error");
        deleteHandler.updateDeleteButton(f);
        resolve();
      };

      xhr.send(fd);
      deleteHandler.updateDeleteButton(f);
    });
  },

  async uploadChunked(f, batch, ttlVal) {
    if (f.canceled) return;
    const abort = new AbortController();
    f.abortController = abort;
    f.lastProgressPercent = 0;
    this.setStatusMessage(f, "Preparing upload…", {
      state: "preparing",
      ariaLive: "assertive",
    });
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
      const message = await this.extractError(
        initResponse,
        "Failed to start chunked upload."
      );
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
    f.totalChunks =
      initData.total_chunks || Math.ceil(f.file.size / f.chunkSize);
    f.uploadedBytes = 0;

    this.initChunkSmoothing(f);

    try {
      await this.uploadChunksSequentially(f, abort);
    } catch (err) {
      this.stopChunkSmoothing(f);
      if (err?.name !== "AbortError") {
        const message = err.message || "Chunk upload failed.";
        showSnack(message);
        f.container?.classList.add("error");
      }
      this.setStatusMessage(f, "");
      this.removeLinkForFile(f);
      f.lastProgressPercent = -1;
      await this.cancelChunkSession(f);
      throw err;
    }

    if (f.remoteName) {
      this.ensureLinkInput(f, batch, { pending: true, autoCopy: false });
    }
    const totalChunks = f.totalChunks || Math.ceil(f.file.size / f.chunkSize);
    const initialStitchMessage = totalChunks
      ? `Checking chunks (0/${totalChunks})`
      : "Checking chunks…";
    this.setStatusMessage(f, initialStitchMessage, {
      state: "finalizing",
      ariaLive: "polite",
    });
    f.lastStitchMessage = initialStitchMessage;

    let stopChunkStatus = null;
    let completeResponse;
    try {
      if (typeof this.startChunkStatusPolling === "function") {
        if (f.stitchPollingStop) {
          try {
            f.stitchPollingStop();
          } catch {
            // ignore cleanup errors
          }
        }
        stopChunkStatus = this.startChunkStatusPolling(f);
        f.stitchPollingStop = stopChunkStatus;
      }
      completeResponse = await fetch(
        `/chunk/${encodeURIComponent(f.chunkSessionId)}/complete`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ hash: f.hash || null }),
          signal: abort.signal,
        }
      );
    } catch (err) {
      if (stopChunkStatus) {
        stopChunkStatus();
        f.stitchPollingStop = null;
      }
      if (err?.name === "AbortError") {
        this.setStatusMessage(f, "");
        f.lastProgressPercent = -1;
        await this.cancelChunkSession(f);
        throw err;
      }
      showSnack("Failed to finalize upload.");
      f.container?.classList.add("error");
      await this.cancelChunkSession(f);
      this.setStatusMessage(f, "");
      this.removeLinkForFile(f);
      f.lastProgressPercent = -1;
      throw err;
    }

    if (stopChunkStatus) {
      stopChunkStatus();
      f.stitchPollingStop = null;
    }

    if (completeResponse.status === 409) {
      const dupPayload = await completeResponse.json().catch(() => ({}));
      await this.cancelChunkSession(f);
      this.setStatusMessage(f, "");
      this.removeLinkForFile(f);
      f.lastProgressPercent = -1;
      this.markFileDuplicate(f, batch, dupPayload);
      return;
    }

    if (!completeResponse.ok) {
      const message = await this.extractError(
        completeResponse,
        "Upload failed."
      );
      showSnack(message);
      f.container?.classList.add("error");
      await this.cancelChunkSession(f);
      this.setStatusMessage(f, "");
      this.removeLinkForFile(f);
      f.lastProgressPercent = -1;
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
      const humanIndex = index + 1;
      const start = index * chunkBytes;
      const end = Math.min(start + chunkBytes, f.file.size);
      const slice = f.file.slice(start, end);
      const baseStatus =
        totalChunks > 1
          ? `Uploading chunk ${humanIndex} of ${totalChunks}`
          : "Uploading…";
      this.setStatusMessage(f, baseStatus, {
        state: "uploading",
        ariaLive: humanIndex === 1 ? "assertive" : "off",
      });
      const startBytes = start;
      const chunkSize = end - startBytes;
      this.beginChunkSmoothing(f, {
        baseStatus,
        startBytes,
        endBytes: end,
        isLastChunk: humanIndex === totalChunks,
      });
      const chunkStartTime = this.now();
      const send = async (useStream) => {
        const body =
          useStream && typeof slice.stream === "function"
            ? slice.stream()
            : slice;
        const options = {
          method: "PUT",
          headers: { "Content-Type": "application/octet-stream" },
          body,
          signal: abort.signal,
        };
        if (
          useStream &&
          body &&
          typeof body === "object" &&
          typeof body.getReader === "function"
        ) {
          options.duplex = "half";
        }
        return fetch(`/chunk/${sessionId}/${index}`, options);
      };

      const canStream =
        this.streamingAllowed && typeof slice.stream === "function";
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
            console.warn(
              "Streaming chunk upload failed, falling back to buffered mode",
              err
            );
          }
        }
      }

      if (response && !response.ok && canStream && response.status === 400) {
        this.streamingAllowed = false;
        if (typeof window !== "undefined") {
          window.ENABLE_STREAMING_UPLOADS = false;
        }
        if (window.DEBUG_LOGS) {
          console.warn(
            "Chunk upload returned 400 with streaming body; retrying without streaming"
          );
        }
        response = null;
      }

      if (!response) {
        try {
          response = await send(false);
        } catch (err) {
          this.stopChunkSmoothing(f);
          throw streamError || err;
        }
      }

      if (!response.ok) {
        this.stopChunkSmoothing(f);
        const message = await this.extractError(
          response,
          "Chunk upload failed."
        );
        throw new Error(message);
      }
      const durationMs = this.now() - chunkStartTime;
      f.uploadedBytes = end;
      this.stopChunkSmoothing(f, { actualBytes: end });
      this.recordChunkTiming(f, {
        chunkBytes: chunkSize,
        durationMs,
      });
      this.updateProgressBar(f, f.uploadedBytes, f.file.size);
      const percent = Math.round(
        Math.min(100, Math.max(0, (f.uploadedBytes / (f.file.size || 1)) * 100))
      );
      if (percent !== f.lastProgressPercent) {
        const message =
          percent >= 100
            ? `${baseStatus} (100%)`
            : `${baseStatus} (${percent}%)`;
        this.setStatusMessage(f, message, {
          state: "uploading",
          ariaLive: humanIndex === 1 && percent <= 5 ? "assertive" : "off",
        });
        f.lastProgressPercent = percent;
      }
    }
    this.stopChunkSmoothing(f, { actualBytes: f.uploadedBytes });
  },

  startChunkStatusPolling(f) {
    if (!f?.chunkSessionId) {
      return () => {};
    }
    const sessionId = encodeURIComponent(f.chunkSessionId);
    let stopped = false;
    let timer = null;
    let inFlight = false;
    const BASE_INTERVAL_MS = 360;
    const FAST_INTERVAL_MS = 200;
    const MAX_INTERVAL_MS = 2000;
    let currentInterval = FAST_INTERVAL_MS;
    let lastAssembled = 0;
    let lastProgressAt = Date.now();
    const scheduleNext = (delay) => {
      if (stopped) return;
      const clamped = Math.max(
        FAST_INTERVAL_MS,
        Math.min(MAX_INTERVAL_MS, delay)
      );
      if (timer) {
        clearTimeout(timer);
      }
      timer = setTimeout(() => {
        timer = null;
        runPoll();
      }, clamped);
    };
    const applyProgressTiming = (assembled) => {
      if (assembled > lastAssembled) {
        lastAssembled = assembled;
        lastProgressAt = Date.now();
        currentInterval = FAST_INTERVAL_MS;
      } else {
        const elapsed = Date.now() - lastProgressAt;
        const backoffSteps = Math.max(0, Math.floor(elapsed / 1200));
        currentInterval = Math.min(
          MAX_INTERVAL_MS,
          BASE_INTERVAL_MS + backoffSteps * 160
        );
      }
    };
    const runPoll = async () => {
      if (stopped || inFlight) {
        return;
      }
      inFlight = true;
      try {
        const response = await fetch(`/chunk/${sessionId}/status`, {
          headers: { Accept: "application/json" },
        });
        if (stopped) {
          return;
        }
        if (response.status === 404) {
          stopped = true;
          return;
        }
        if (!response.ok) {
          currentInterval = Math.min(
            MAX_INTERVAL_MS,
            Math.max(BASE_INTERVAL_MS, currentInterval * 1.5)
          );
          scheduleNext(currentInterval);
          return;
        }
        const data = await response.json().catch(() => null);
        if (!data) {
          currentInterval = Math.min(
            MAX_INTERVAL_MS,
            Math.max(BASE_INTERVAL_MS, currentInterval * 1.5)
          );
          scheduleNext(currentInterval);
          return;
        }
        const total = data.total_chunks || f.totalChunks || 0;
        const assembledRaw = data.assembled_chunks ?? 0;
        const assembled = total ? Math.min(assembledRaw, total) : assembledRaw;
        const message = total
          ? `Checking chunks (${assembled}/${total})`
          : `Checking chunks (${assembled})`;
        if (f.statusState === "finalizing" && message !== f.lastStitchMessage) {
          this.setStatusMessage(f, message, {
            state: "finalizing",
            ariaLive: assembled <= 1 ? "polite" : "off",
          });
          f.lastStitchMessage = message;
        }
        if (data.completed || (total && assembled >= total)) {
          stopped = true;
          return;
        }
        applyProgressTiming(assembled);
        scheduleNext(currentInterval);
      } catch (err) {
        if (window?.DEBUG_LOGS) {
          console.warn("chunk status poll failed", err);
        }
        if (!stopped) {
          currentInterval = Math.min(
            MAX_INTERVAL_MS,
            Math.max(BASE_INTERVAL_MS, currentInterval * 1.5 + 120)
          );
          scheduleNext(currentInterval);
        }
      } finally {
        inFlight = false;
      }
    };

    runPoll();

    return () => {
      stopped = true;
      if (timer) {
        clearTimeout(timer);
      }
    };
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
      await fetch(`/chunk/${encodeURIComponent(sessionId)}/cancel`, {
        method: "DELETE",
      });
    } catch {
      // ignore network errors on cancel
    } finally {
      f.abortController = null;
    }
  },

  cancelPendingUpload(f) {
    f.canceled = true;
    f.removed = true;
    try {
      showSnack("Upload canceled.");
    } catch {}
    this.stopChunkSmoothing(f);
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
    this.setStatusMessage(f, "");
    f.lastProgressPercent = -1;
    if (f.barSpan) {
      f.barSpan.style.width = "0%";
      f.barSpan.classList.remove("complete");
    }
    f.bar?.classList.remove("divider");
    deleteHandler.updateDeleteButton(f);
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
    f.lastProgressPercent = -1;
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
      setTimeout(
        () => batch.groupLi?.parentNode?.removeChild(batch.groupLi),
        400
      );
    }
    if (!batch.files.length) {
      this.batches = this.batches.filter((b) => b !== batch);
    }
    deleteHandler.updateDeleteButton(f);
  },

  handlePreUploadDuplicate(f, batch) {
    this.setStatusMessage(f, "");
    this.removeLinkForFile(f);
    f.lastProgressPercent = -1;
    try {
      showSnack("Duplicate file: already uploaded.");
    } catch {}
    f.done = true;
    f.removed = true;
    f.canceled = true;
    if (f.container) {
      f.container.classList.add("dupe-remove");
      setTimeout(() => {
        if (f.container && f.container.parentNode) {
          f.container.parentNode.removeChild(f.container);
        }
      }, 400);
    }
    deleteHandler.updateDeleteButton(f);
  },

  handleUploadSuccess(f, batch, payload) {
    this.setStatusMessage(f, "");
    f.lastProgressPercent = -1;
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
