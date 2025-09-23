// --- Dynamic config fetch ---
window.MAX_FILE_BYTES = 500 * 1024 * 1024;
window.MAX_FILE_SIZE_STR = "500MB";
fetch("/api/config", { cache: "no-store" })
  .then((r) => (r.ok ? r.json() : null))
  .then((cfg) => {
    if (cfg && typeof cfg.max_file_bytes === "number")
      window.MAX_FILE_BYTES = cfg.max_file_bytes;
    if (cfg && typeof cfg.max_file_size_str === "string")
      window.MAX_FILE_SIZE_STR = cfg.max_file_size_str;
  })
  .catch(() => {});

// Rewritten client logic for stable uploads & deletes
const dropZone = document.getElementById("dropZone");
// ensure input exists
if (!document.getElementById("fileInput")) {
  const inp = document.createElement("input");
  inp.type = "file";
  inp.multiple = true;
  inp.id = "fileInput";
  inp.style.display = "none";
  dropZone.appendChild(inp);
}
const fileInput = document.getElementById("fileInput");
const list = document.getElementById("fileList");
const snackbar = document.getElementById("snackbar");
const ownedList = document.getElementById("ownedList");
const ownedPanel = document.getElementById("ownedPanel");
const ttlSelect = document.getElementById("ttlSelect");
const ttlValueLabel = document.getElementById("ttlValue");
const ttlMap = ["1h", "3h", "12h", "1d", "3d", "7d", "14d"];
function getTTL() {
  if (!ttlSelect) return "3d";
  if (ttlSelect.tagName === "INPUT" && ttlSelect.type === "range") {
    return ttlMap[parseInt(ttlSelect.value, 10)] || "3d";
  }
  return ttlSelect.value;
}
let ownedMeta = new Map(); // name -> expires (unix seconds)
if (ttlSelect) {
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

// Updated: resilient flashCopied (creates snackbar if missing)
function flashCopied(msg = "Link copied") {
  let sb = snackbar || document.getElementById("snackbar");
  if (!sb) {
    sb = document.createElement("div");
    sb.id = "snackbar";
    document.body.appendChild(sb);
  }
  sb.textContent = msg;
  sb.classList.remove("error");
  sb.classList.add("show");
  clearTimeout(flashCopied._t);
  flashCopied._t = setTimeout(() => sb.classList.remove("show"), 1600);
}
async function copyToClipboard(text) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    try {
      document.execCommand("copy");
    } catch {}
    document.body.removeChild(ta);
  }
}
function fmtBytes(b) {
  if (!isFinite(b) || b < 0) return "–";
  if (b === 0) return "0 B";
  const k = 1024;
  const s = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(s.length - 1, Math.floor(Math.log(b) / Math.log(k)));
  return (b / Math.pow(k, i)).toFixed(i ? 2 : 0) + " " + s[i];
}

// Added: generic snackbar helper alias expected by later code (now specialized for errors and shake)
function showSnack(msg, opts) {
  opts = opts || {};
  console.log("[showSnack]", msg, {
    MAX_FILE_BYTES: window.MAX_FILE_BYTES,
    MAX_FILE_SIZE_STR: window.MAX_FILE_SIZE_STR,
    fileSize: opts.fileSize,
    fileSizeStr: opts.fileSizeStr,
    fileName: opts.fileName,
  });
  let sb = document.getElementById("snackbar");
  if (!sb) {
    sb = document.createElement("div");
    sb.id = "snackbar";
    document.body.appendChild(sb);
  }
  sb.textContent = msg;
  sb.classList.add("show", "error");
  clearTimeout(showSnack._t);
  showSnack._t = setTimeout(() => sb.classList.remove("show", "error"), 5000);
  if (dropZone) {
    dropZone.classList.add("shake");
    dropZone.addEventListener(
      "animationend",
      () => dropZone.classList.remove("shake"),
      { once: true }
    );
  }
  if (window._announce) window._announce(msg);
}

let batches = []; // [{files:[] , linksBox?}]
let uploading = false;
let ownedCache = new Set();

// NEW: ensure group header count updates & remove empty group card
function updateGroupHeader(batch) {
  if (!batch || !batch.isGroup || !batch.groupLi) return;
  const headName = batch.groupLi.querySelector(".group-head .name");
  const remaining = batch.files.filter((f) => !f.removed).length;
  if (remaining <= 0) {
    markGroupEmpty(batch);
    return;
  }
  if (headName) {
    headName.textContent = remaining + " file" + (remaining === 1 ? "" : "s");
  }
}
function makeLinkInput(rel, autoCopy = true) {
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
  return inp;
}
const _origMakeLinkInput = makeLinkInput;
makeLinkInput = function (rel, autoCopy) {
  const el = _origMakeLinkInput(rel, autoCopy);
  if (rel.startsWith("f/")) addOwned(rel.slice(2));
  return el;
};
// helper: map ttl code -> seconds
function ttlCodeSeconds(code) {
  return (
    {
      "1h": 3600,
      "3h": 10800,
      "12h": 43200,
      "1d": 86400,
      "3d": 259200,
      "7d": 604800,
      "14d": 1209600,
    }[code] || 259200
  );
}

function addBatch(fileList) {
  if (!fileList || !fileList.length) return;
  // Filter out zero-byte (often directory placeholder or malformed) files to prevent 0B uploads
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
  fileList = cleaned;
  if (!fileList.length) return;
  const batch = {
    files: [...fileList].map((f) => ({
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
  };
  batch.isGroup = batch.files.length > 1; // NEW flag
  batches.push(batch);
  renderList();
  autoUpload();
}

function renderList() {
  batches.forEach((batch) => {
    if (batch.isGroup) {
      if (!batch.groupLi) {
        const li = document.createElement("li");
        li.className = "group-batch";
        li.innerHTML =
          '<div class="file-row group-head"><div class="name"></div><div class="size"></div><div class="actions"></div></div><div class="group-files"></div>';
        const headName = li.querySelector(".group-head .name");
        headName.textContent = batch.files.length + " files";
        const headSize = li.querySelector(".group-head .size");
        headSize.textContent = "";
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
        const nameEl = entry.querySelector(".name");
        nameEl.textContent = f.file.name;
        const sizeEl = entry.querySelector(".size");
        sizeEl.textContent = fmtBytes(f.file.size);
        const actions = entry.querySelector(".actions");
        const del = document.createElement("button");
        del.type = "button";
        del.className = "remove";
        del.textContent = "x";
        del.title = "Remove";
        del.setAttribute("aria-label", "Remove file from queue");
        del.addEventListener("click", (e) => {
          e.stopPropagation();
          handleDeleteClick(f, batch);
        });
        f.deleteBtn = del;
        actions.appendChild(del);
        f.bar = entry.querySelector(".bar");
        f.barSpan = f.bar.querySelector("span");
        f.container = entry;
        filesWrap.appendChild(entry);
      });
      return; // skip legacy per-file li path
    }
    // Legacy single-file path
    const multi = batch.files.length > 1; // will be false here
    if (multi && !batch.linksBox) {
      batch.linksBox = document.createElement("div");
      batch.linksBox.className = "links";
    }
    batch.files.forEach((f, idx) => {
      if (f.container) return; // already rendered
      const li = document.createElement("li");
      f.container = li;
      const row = document.createElement("div");
      row.className = "file-row";
      const name = document.createElement("div");
      name.className = "name";
      name.textContent = f.file ? f.file.name : f.remoteName || "file";
      const size = document.createElement("div");
      size.className = "size";
      size.textContent = f.file ? fmtBytes(f.file.size) : "";
      const actions = document.createElement("div");
      actions.className = "actions";
      const del = document.createElement("button");
      del.type = "button";
      del.className = "remove";
      del.textContent = "x";
      del.title = "Remove";
      del.setAttribute("aria-label", "Remove file from queue");
      del.addEventListener("click", (e) => {
        e.stopPropagation();
        handleDeleteClick(f, batch);
      });
      f.deleteBtn = del;
      actions.appendChild(del);
      row.appendChild(name);
      row.appendChild(size);
      row.appendChild(actions);
      li.appendChild(row);
      const bar = document.createElement("div");
      bar.className = "bar";
      bar.innerHTML = "<span></span>";
      bar.title = "Upload progress";
      const span = bar.querySelector("span");
      span.setAttribute("aria-label", "Upload progress");
      f.bar = bar;
      f.barSpan = span;
      li.appendChild(bar);
      list.appendChild(li);
      li.classList.add("adding");
      requestAnimationFrame(() => li.classList.add("in"));
    });
  });
}

function updateDeleteButton(f) {
  if (!f.deleteBtn) return;
  if (f.deleting) {
    f.deleteBtn.disabled = true;
    f.deleteBtn.textContent = "…";
    f.deleteBtn.title = "Deleting...";
    f.deleteBtn.setAttribute("aria-label", "Deleting file");
    return;
  }
  if (!f.remoteName) {
    f.deleteBtn.textContent = "x";
    f.deleteBtn.disabled = false;
    f.deleteBtn.title = "Remove (not uploaded)";
    f.deleteBtn.setAttribute("aria-label", "Remove file from queue");
  } else {
    f.deleteBtn.textContent = "❌";
    f.deleteBtn.disabled = false;
    f.deleteBtn.title = "Delete from server";
    f.deleteBtn.setAttribute("aria-label", "Delete file from server");
  }
}

// NEW: graceful empty-group removal (shows placeholder then fades)
function markGroupEmpty(batch) {
  if (!batch || !batch.groupLi || batch._emptying) return;
  batch._emptying = true;
  const li = batch.groupLi;
  // Replace inner file list with an empty message (keep height for quick fade)
  const filesWrap = li.querySelector(".group-files");
  if (filesWrap) {
    filesWrap.innerHTML =
      '<div class="group-empty-msg" aria-live="polite">All files removed</div>';
  }
  li.classList.add("group-empty-start");
  requestAnimationFrame(() => {
    li.classList.add("group-empty-fade");
    li.addEventListener(
      "transitionend",
      () => {
        animateRemove(li, () => {
          batches = batches.filter((b) => b !== batch);
        });
      },
      { once: true }
    );
  });
}

// NEW helper: remove an entry from a grouped batch & clean up card if empty
function removeGroupedEntry(batch, f) {
  f.removed = true;
  const finalize = () => {
    batch.files = batch.files.filter((x) => !x.removed);
    if (!batch.files.length) {
      markGroupEmpty(batch);
    } else {
      updateGroupHeader(batch);
    }
  };
  if (f.container) {
    f.container.classList.add("removing");
    animateRemove(f.container, finalize);
  } else finalize();
}

// Adjusted deletion to support grouped batches
function handleDeleteClick(f, batch) {
  if (f.deleting) return;
  if (batch && batch.isGroup) {
    if (!f.remoteName) {
      if (f.xhr) {
        f.canceled = true;
        f.aborted = true;
        try {
          f.xhr.abort();
        } catch {}
      }
      removeGroupedEntry(batch, f);
      return;
    }
    // remote deletion path for grouped
    deleteRemote(f, batch);
    return;
  }
  // Non-group (existing logic with linksBox reattach not needed now for groups)
  if (batch && batch.linksBox && batch.linksBox.parentElement === f.container) {
    batch.linksBox.remove();
  }
  if (!f.remoteName) {
    if (f.xhr) {
      f.canceled = true;
      f.aborted = true;
      try {
        f.xhr.abort();
      } catch {}
    }
    if (f.container)
      animateRemove(f.container, () => {
        batch.files = batch.files.filter((x) => x !== f);
        if (!batch.files.length) {
          batches = batches.filter((b) => b !== batch);
        }
      });
    else {
      batch.files = batch.files.filter((x) => x !== f);
      if (!batch.files.length) {
        batches = batches.filter((b) => b !== batch);
      }
    }
    return;
  }
  deleteRemote(f, batch);
}

function deleteRemote(f, batch) {
  if (!f.remoteName || f.deleting) return;
  f.deleting = true;
  updateDeleteButton(f);
  let timeoutId = setTimeout(() => {
    // force cleanup if backend silent
    if (f.deleting) {
      f.deleting = false;
      updateDeleteButton(f);
      if (batch && batch.isGroup) {
        removeGroupedEntry(batch, f);
      } else {
        if (f.container)
          animateRemove(f.container, () => {
            batch.files = batch.files.filter((x) => x !== f);
            if (!batch.files.length)
              batches = batches.filter((b) => b !== batch);
          });
      }
    }
  }, 8000);
  if (batch && batch.isGroup) {
    // grouped removal
    fetch("/d/" + encodeURIComponent(f.remoteName), { method: "DELETE" })
      .then((r) => {
        clearTimeout(timeoutId);
        if (r.ok) {
          ownedCache.delete(f.remoteName);
          ownedMeta.delete(f.remoteName);
          f.deleting = false;
          removeGroupedEntry(batch, f);
          renderOwned([...ownedCache]);
          autoUpload();
        } else {
          f.deleting = false;
          updateDeleteButton(f);
        }
      })
      .catch(() => {
        clearTimeout(timeoutId);
        f.deleting = false;
        updateDeleteButton(f);
      });
    return;
  }
  fetch("/d/" + encodeURIComponent(f.remoteName), { method: "DELETE" })
    .then((r) => {
      clearTimeout(timeoutId);
      if (r.ok) {
        if (f.container)
          animateRemove(f.container, () => {
            batch.files = batch.files.filter((x) => x !== f);
            if (!batch.files.length)
              batches = batches.filter((b) => b !== batch);
            ownedCache.delete(f.remoteName);
            renderOwned([...ownedCache]);
            autoUpload();
          });
        else {
          batch.files = batch.files.filter((x) => x !== f);
          if (!batch.files.length) batches = batches.filter((b) => b !== batch);
          ownedCache.delete(f.remoteName);
          renderOwned([...ownedCache]);
          autoUpload();
        }
      } else {
        f.deleting = false;
        updateDeleteButton(f);
      }
    })
    .catch(() => {
      clearTimeout(timeoutId);
      f.deleting = false;
      updateDeleteButton(f);
    });
}

async function uploadSequential() {
  if (uploading) return;
  uploading = true;
  for (const batch of batches) {
    for (let i = 0; i < batch.files.length; i++) {
      const f = batch.files[i];
      if (!f || f.removed) continue;
      if (f.done || f.deleting) continue;
      await uploadOne(f, batch);
    }
    // After a pass, drop any removed placeholders
    batch.files = batch.files.filter((f) => !f.removed);
  }
  // Prune empty batches
  batches = batches.filter((b) => b.files.length);
  uploading = false;
}

function uploadOne(f, batch) {
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
    xhr.onabort = () => {
      if (finished) return;
      finished = true;
      f.canceled = true;
      if (f.barSpan) {
        f.barSpan.style.opacity = ".35";
      }
      resolve();
    };
    xhr.onload = () => {
      if (finished) return;
      if (f.canceled) {
        finished = true;
        resolve();
        return;
      }
      const ok = xhr.status >= 200 && xhr.status < 300;
      if (ok) {
        if (f.barSpan) {
          if (!f.barSpan._animated) {
            f.barSpan._animated = true;
          }
          f.barSpan.style.width = "100%";
          requestAnimationFrame(() => {
            f.barSpan.classList.add("complete");
            setTimeout(() => {
              if (f.bar && f.barSpan.classList.contains("complete"))
                f.bar.classList.add("divider");
            }, 1000);
          });
        }
        let rel = null;
        try {
          const data = xhr.response || JSON.parse(xhr.responseText || "{}");
          rel = data.files && data.files[0];
        } catch {}
        if (rel) {
          f.remoteName = rel.startsWith("f/") ? rel.slice(2) : rel;
        }
        const ttlSeconds = ttlCodeSeconds(ttlVal);
        const exp = Math.floor(Date.now() / 1000) + ttlSeconds;
        ownedMeta.set(f.remoteName, {
          expires: exp,
          total: ttlSeconds,
          original: f.file && f.file.name ? f.file.name : "",
        });
        // Explicitly add to owned list (in case makeLinkInput side-effect missed)
        if (f.remoteName) {
          try {
            addOwned(f.remoteName);
          } catch (_) {}
        }
        f.done = true;
        updateDeleteButton(f);
        if (f.remoteName) {
          const input = makeLinkInput(
            "f/" + f.remoteName,
            !batch.files.some((x) => !x.done)
          );
          if (batch.isGroup) {
            // Per-file link inside grouped batch entry
            if (f.container) {
              let linksRow = f.container.querySelector(".links");
              if (!linksRow) {
                linksRow = document.createElement("div");
                linksRow.className = "links";
                f.container.appendChild(linksRow);
              }
              linksRow.appendChild(input);
            }
            // Assign group expiration dataset once (first completed file) so global countdown logic updates one TTL line
            if (!batch.groupLi.dataset.exp) {
              batch.groupLi.dataset.exp = exp;
              batch.groupLi.dataset.total = ttlSeconds;
            }
          } else {
            const links = document.createElement("div");
            links.className = "links";
            links.appendChild(input);
            f.container.appendChild(links);
          }
        }
        finished = true;
        resolve();
      } else {
        {
          f.container?.classList.add("error");
          updateDeleteButton(f);
        }
        finished = true;
        resolve();
      }
    };
    xhr.onerror = () => {
      if (finished) return;
      if (!f.canceled) {
        f.container?.classList.add("error");
        updateDeleteButton(f);
      }
      finished = true;
      resolve();
    };
    xhr.send(fd);
    updateDeleteButton(f);
  });
}

function autoUpload() {
  uploadSequential();
}

async function loadExisting() {
  try {
    const r = await fetch("/mine");
    if (!r.ok) return;
    const data = await r.json();
    if (data && Array.isArray(data.files)) {
      data.files.forEach((f) => addOwned(f.replace(/^f\//, "")));
      if (Array.isArray(data.metas)) {
        data.metas.forEach((m) => {
          const name = m.file.replace(/^f\//, "");
          ownedMeta.set(name, {
            expires: m.expires,
            original: m.original || "",
          });
        });
      }
    }
  } catch {}
}
let ownedInitialRender = false;
function renderOwned(names) {
  if (!ownedList || !ownedPanel) return;
  // helper to escape html for original filenames
  function escapeHtml(s) {
    return (s || "").replace(
      /[&<>"']/g,
      (c) =>
        ({
          "&": "&amp;",
          "<": "&lt;",
          ">": "&gt;",
          '"': "&quot;",
          "'": "&#39;",
        }[c])
    );
  }
  const want = new Set(names);
  // Existing chips map
  const existing = new Map();
  ownedList
    .querySelectorAll(".owned-chip")
    .forEach((chip) => existing.set(chip.dataset.name, chip));
  // Remove chips no longer present
  existing.forEach((chip, name) => {
    if (!want.has(name)) {
      if (!chip.classList.contains("removing")) {
        // Changed: use animateRemove so CSS vars are set for smooth cardSquish animation
        animateRemove(chip, () => {
          if (!ownedList.querySelector(".owned-chip")) hideOwnedPanel();
        });
      }
    }
  });
  if (!names.length) {
    hideOwnedPanel();
    return;
  }
  showOwnedPanel();
  const nowSec = Date.now() / 1000;
  names.sort();
  names.forEach((n) => {
    let chip = existing.get(n);
    let meta = ownedMeta.get(n);
    let exp = -1,
      total = null;
    if (typeof meta === "number") {
      exp = meta;
    } else if (meta && typeof meta === "object") {
      exp = meta.expires || -1;
      total = meta.total || null;
    }
    const remain = exp - nowSec;
    function fmtRemain(sec) {
      if (exp < 0) return "...";
      if (sec <= 0) return "expired";
      const units = [
        ["d", 86400],
        ["h", 3600],
        ["m", 60],
        ["s", 1],
      ];
      let rem = sec;
      let out = [];
      for (const [u, v] of units) {
        if (out.length >= 2) break;
        if (rem >= v) {
          const val = Math.floor(rem / v);
          out.push(val + u);
          rem %= v;
        }
      }
      return out.length ? out.join(" ") : "secs";
    }
    if (!chip) {
      console.log(meta);
      console.log(meta.original);
      chip = document.createElement("div");
      chip.className = "owned-chip";
      chip.dataset.name = n;
      if (exp >= 0) chip.dataset.exp = exp;
      if (total) chip.dataset.total = total;
      const displayName =
        meta && meta.original && meta.original.trim()
          ? meta.original.trim()
          : n;
      const titleFull = displayName === n ? n : displayName + " (" + n + ")";
      if (ownedInitialRender) {
        chip.classList.add("adding");
        requestAnimationFrame(() => chip.classList.add("in"));
      }
      chip.innerHTML = `<div class="top"><div class="name" title="${escapeHtml(
        titleFull
      )}">${escapeHtml(
        displayName
      )}</div><div class="actions"></div></div><div class="ttl" style="font-size:.5rem;opacity:.55;letter-spacing:.4px;">${fmtRemain(
        remain
      )}</div>`;
      const actions = chip.querySelector(".actions");
      const copyBtn = document.createElement("button");
      copyBtn.className = "small";
      copyBtn.textContent = "📋";
      copyBtn.setAttribute("title", "Copy direct link");
      copyBtn.setAttribute("aria-label", "Copy file link to clipboard");
      copyBtn.addEventListener("click", () => {
        copyToClipboard(location.origin + "/f/" + n).then(() => flashCopied());
      });
      actions.appendChild(copyBtn);
      const delBtn = document.createElement("button");
      delBtn.className = "small";
      delBtn.textContent = "❌";
      delBtn.setAttribute("title", "Delete file from server");
      delBtn.setAttribute("aria-label", "Delete file from server");
      delBtn.addEventListener("click", () => {
        fetch("/d/" + encodeURIComponent(n), { method: "DELETE" }).then((r) => {
          if (r.ok) {
            ownedCache.delete(n);
            ownedMeta.delete(n);
            removeFromUploads(n);
            renderOwned([...ownedCache]);
          }
        });
      });
      actions.appendChild(delBtn);
      const mini = document.createElement("input");
      mini.type = "text";
      mini.readOnly = true;
      mini.className = "link-input mini";
      mini.value = location.origin + "/f/" + n;
      mini.title = "Click to copy direct link";
      mini.setAttribute("aria-label", "File direct link (click to copy)");
      mini.addEventListener("click", () => {
        mini.select();
        copyToClipboard(mini.value).then(() => flashCopied());
      });
      chip.appendChild(mini);
      // Insert into grid container (create if missing)
      let grid = ownedList.querySelector(".owned-grid");
      if (!grid) {
        grid = document.createElement("div");
        grid.className = "owned-grid";
        ownedList.appendChild(grid);
      }
      grid.appendChild(chip);
    } else {
      // Update meta/time remaining
      if (exp >= 0) chip.dataset.exp = exp;
      else chip.removeAttribute("data-exp");
      if (total) chip.dataset.total = total;
      else chip.removeAttribute("data-total");
      const ttlEl = chip.querySelector(".ttl");
      if (ttlEl) ttlEl.textContent = fmtRemain(remain);
    }
    if (total && remain > 0 && remain / total <= 0.01) {
      chip.classList.add("expiring");
    } else {
      chip.classList.remove("expiring");
    }
  });
  // New: set UL title to list of original (or fallback) filenames
  try {
    const titleList = names.map((n) => {
      const meta = ownedMeta.get(n);
      return meta && meta.original && meta.original.trim()
        ? meta.original.trim()
        : n;
    });
    if (titleList.length)
      ownedList.title = "Your files: " + titleList.join(", ");
    else ownedList.title = "List of files you have uploaded";
  } catch {}
  ownedInitialRender = true;
}
function hideOwnedPanel() {
  if (ownedPanel.style.display === "none") return;
  if (!ownedPanel.classList.contains("closing")) {
    ownedPanel.classList.remove("opening");
    ownedPanel.classList.add("closing");
    ownedPanel.addEventListener(
      "animationend",
      () => {
        ownedPanel.style.display = "none";
        ownedPanel.classList.remove("closing");
        try {
          window.scrollTo({ top: 0, behavior: "smooth" });
        } catch {
          window.scrollTo(0, 0);
        }
      },
      { once: true }
    );
  }
}
function showOwnedPanel() {
  if (ownedPanel.style.display === "none" || !ownedPanel.style.display) {
    if (ownedPanel.style.display === "none") {
      ownedPanel.style.display = "";
      ownedPanel.classList.add("opening");
      ownedPanel.addEventListener(
        "animationend",
        () => ownedPanel.classList.remove("opening"),
        { once: true }
      );
    }
  }
}
async function refreshOwned() {
  try {
    const r = await fetch("/mine", { cache: "no-store" });
    if (!r.ok) return;
    const data = await r.json();
    if (data && Array.isArray(data.files)) {
      const set = new Set(data.files.map((f) => f.replace(/^f\//, "")));
      ownedCache = set;
      if (Array.isArray(data.metas)) {
        ownedMeta.clear();
        data.metas.forEach((m) =>
          ownedMeta.set(m.file.replace(/^f\//, ""), {
            expires: m.expires,
            original: m.original || "",
          })
        );
      }
      renderOwned([...ownedCache]);
    }
  } catch {}
}
function addOwned(remoteName) {
  if (!remoteName) return;
  if (!ownedCache.has(remoteName)) {
    ownedCache.add(remoteName);
    renderOwned([...ownedCache]);
  }
}

// New helper: remove a file (by remoteName) from current upload batches & UI
function removeFromUploads(remoteName) {
  if (!remoteName) return;
  batches.slice().forEach((batch) => {
    batch.files.slice().forEach((f) => {
      if (f.remoteName === remoteName) {
        if (f.container) {
          animateRemove(f.container, () => {
            batch.files = batch.files.filter((x) => x !== f);
            if (!batch.files.length)
              batches = batches.filter((b) => b !== batch);
          });
        } else {
          batch.files = batch.files.filter((x) => x !== f);
          if (!batch.files.length) batches = batches.filter((b) => b !== batch);
        }
      }
    });
  });
}

// Added: setupUploadEvents implementation (was previously missing)
function setupUploadEvents() {
  if (!dropZone) return;
  ["dragenter", "dragover"].forEach((evt) =>
    dropZone.addEventListener(evt, (e) => {
      e.preventDefault();
      e.stopPropagation();
      dropZone.classList.add("drag");
    })
  );
  ["dragleave", "dragend"].forEach((evt) =>
    dropZone.addEventListener(evt, (e) => {
      e.preventDefault();
      e.stopPropagation();
      dropZone.classList.remove("drag");
    })
  );
  document.addEventListener("dragover", (e) => e.preventDefault());
  document.addEventListener("drop", (e) => e.preventDefault());
  if (fileInput) {
    fileInput.addEventListener("change", () => {
      if (fileInput.files && fileInput.files.length) addBatch(fileInput.files);
      try {
        fileInput.value = "";
      } catch {}
    });
  }
}

// Existing bubble-phase drop handler; modify to avoid duplicate addBatch when prefilter active
dropZone.addEventListener("drop", (e) => {
  e.preventDefault();
  e.stopPropagation();
  dropZone.classList.remove("drag");
  if (window.__JB_PREFILTER_ACTIVE) return;
  if (e.dataTransfer && e.dataTransfer.files.length)
    addBatch(e.dataTransfer.files);
});
// Removed manual fileInput.click() to avoid duplicate dialogs; label already opens the file picker.

setupUploadEvents();
loadExisting();
// Removed generic paste listener to avoid duplicate uploads; dedicated paste handler later re-dispatches change.
// document.addEventListener('paste', e=>{ if(e.clipboardData && e.clipboardData.files.length) addBatch(e.clipboardData.files); });
refreshOwned();
setInterval(refreshOwned, 15000);
window.addEventListener("focus", refreshOwned);

// Animation helper: squish & fade then remove (updated for smooth sibling reposition)
function animateRemove(el, cb) {
  if (!el) {
    cb && cb();
    return;
  }
  const parent = el.parentElement;
  const enableFLIP =
    parent &&
    (parent.id === "fileList" ||
      parent.classList.contains("owned-grid") ||
      parent.classList.contains("group-files") ||
      (parent.closest &&
        (parent.closest("#fileList") || parent.closest(".owned-grid"))));
  if (enableFLIP) {
    // Capture initial sibling positions (excluding the element being removed)
    const siblings = Array.from(parent.children).filter((c) => c !== el);
    const first = siblings.map((ch) => ({
      el: ch,
      rect: ch.getBoundingClientRect(),
    }));
    const rect = el.getBoundingClientRect();
    // Clone overlay to animate the removed element out of flow
    const clone = el.cloneNode(true);
    clone.classList.add("removal-clone");
    Object.assign(clone.style, {
      position: "fixed",
      top: rect.top + "px",
      left: rect.left + "px",
      width: rect.width + "px",
      height: rect.height + "px",
      margin: 0,
      boxSizing: "border-box",
      pointerEvents: "none",
      zIndex: 999,
      transition: "transform .45s var(--e-out), opacity .45s var(--e-out)",
    });
    document.body.appendChild(clone);
    // Remove original immediately (no jump thanks to FLIP below)
    try {
      el.remove();
    } catch {}
    // Force reflow before measuring new positions
    // eslint-disable-next-line no-unused-expressions
    parent.offsetHeight;
    const last = siblings.map((ch) => ({
      el: ch,
      rect: ch.getBoundingClientRect(),
    }));
    // Apply FLIP transforms
    last.forEach((item) => {
      const firstMatch = first.find((f) => f.el === item.el);
      if (!firstMatch) return;
      const dx = firstMatch.rect.left - item.rect.left;
      const dy = firstMatch.rect.top - item.rect.top;
      if (dx || dy) {
        item.el.style.transition = "none";
        item.el.style.transform = `translate(${dx}px,${dy}px)`;
        // Force style
        item.el.getBoundingClientRect();
        requestAnimationFrame(() => {
          item.el.style.transition = "transform .45s var(--e-out)";
          item.el.style.transform = "";
          item.el.addEventListener(
            "transitionend",
            () => {
              item.el.style.transition = "";
            },
            { once: true }
          );
        });
      }
    });
    // Animate the clone fading/shrinking out
    requestAnimationFrame(() => {
      clone.style.opacity = "0";
      clone.style.transform = "scale(.92)";
      clone.addEventListener(
        "transitionend",
        () => {
          try {
            clone.remove();
          } catch {}
          cb && cb();
        },
        { once: true }
      );
    });
    return;
  }
  // Fallback (original height-collapse animation)
  const rect = el.getBoundingClientRect();
  const cs = getComputedStyle(el);
  el.style.setProperty("--orig-h", rect.height + "px");
  el.style.setProperty("--orig-mt", cs.marginTop);
  el.style.setProperty("--orig-mb", cs.marginBottom);
  el.style.setProperty("--orig-pt", cs.paddingTop);
  el.style.setProperty("--orig-pb", cs.paddingBottom);
  el.classList.add("removing");
  el.addEventListener(
    "animationend",
    () => {
      try {
        el.remove();
      } catch {}
      cb && cb();
    },
    { once: true }
  );
}

// Panel reveal for owned panel (initial)
if (ownedPanel) {
  ownedPanel.classList.add("reveal-start");
  requestAnimationFrame(() => ownedPanel.classList.add("reveal"));
}

// Additional drop-zone animation setup
if (dropZone) {
  // trigger idle animation after slight delay
  setTimeout(() => dropZone.classList.add("animate"), 500);
  const iconEl = dropZone.querySelector(".icon");
  function setOpen(open) {
    if (iconEl) {
      iconEl.textContent = open ? "📂" : "📁";
    }
  }

  // Added: show box emoji (📦) when user is dragging multiple files, an archive, or a directory
  let _dragSpecial = false;
  function updateDragIcon(e) {
    if (!iconEl || !e || !e.dataTransfer) return;
    const dt = e.dataTransfer;
    let special = false;
    if (dt.items) {
      const items = Array.from(dt.items).filter((it) => it.kind === "file");
      if (items.length > 1) special = true; // multi-file
      for (const it of items) {
        if (special) break;
        const entry = it.webkitGetAsEntry && it.webkitGetAsEntry();
        if (entry && entry.isDirectory) {
          special = true;
          break;
        }
        const f = it.getAsFile && it.getAsFile();
        if (f) {
          const name = f.name.toLowerCase();
          if (/\.(zip|tar|tgz|gz|rar|7z|bz2|xz|zipx)$/.test(name)) {
            special = true;
            break;
          }
        }
      }
    }
    if (special) {
      _dragSpecial = true;
      iconEl.textContent = "📦";
    } else if (!_dragSpecial) {
      // fallback to open folder while hovering
      iconEl.textContent = "📂";
    }
  }
  ["dragenter", "dragover"].forEach((evt) => {
    dropZone.addEventListener(
      evt,
      (ev) => {
        updateDragIcon(ev);
      },
      true
    );
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
  // ripple + click (non-capture to let normal handlers run first)
  dropZone.addEventListener("click", (e) => {
    // Prevent duplicate ripples: remove existing
    dropZone.querySelectorAll(".ripple").forEach((r) => {
      try {
        r.remove();
      } catch {}
    });
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
    // Open file picker unless user clicked an existing interactive element
    if (fileInput && !e.defaultPrevented && e.target !== fileInput) {
      fileInput.click();
    }
  });
}

// Attach per-file expiration after upload without altering original uploadOne body
(function () {
  if (typeof uploadOne === "function") {
    const _origUploadOne = uploadOne;
    uploadOne = function (f, batch) {
      // Use getTTL() so range slider numeric value is mapped to code (e.g. 1 -> 3h)
      const ttlVal =
        typeof getTTL === "function"
          ? getTTL()
          : (ttlSelect && ttlSelect.value) || "3d";
      return _origUploadOne(f, batch).then(() => {
        if (f.done && f.remoteName && !f.expires) {
          const seconds =
            typeof ttlCodeSeconds === "function"
              ? ttlCodeSeconds(ttlVal)
              : {
                  "1h": 3600,
                  "3h": 10800,
                  "12h": 43200,
                  "1d": 86400,
                  "3d": 259200,
                  "7d": 604800,
                  "14d": 1209600,
                }[ttlVal] || 259200;
          f.expires = Math.floor(Date.now() / 1000) + seconds;
          f.total = seconds;
          if (f.container) {
            f.container.dataset.exp = f.expires;
            f.container.dataset.total = seconds;
          }
        }
      });
    };
  }

  let expiryWatcherStarted = false;
  function startExpiryWatcher() {
    if (expiryWatcherStarted) return;
    expiryWatcherStarted = true;
    setInterval(() => {
      const now = Date.now() / 1000;
      // Recently uploaded list
      batches.forEach((batch) =>
        batch.files.forEach((f) => {
          if (f.remoteName && f.expires && !f.expired) {
            if (
              f.total &&
              (f.expires - now) / f.total <= 0.01 &&
              f.expires - now > 0
            ) {
              f.container?.classList.add("expiring");
            }
            if (now >= f.expires) {
              f.expired = true;
              if (f.container) {
                f.container.classList.add("expired", "removing");
                f.container.addEventListener(
                  "animationend",
                  () => {
                    try {
                      f.container.remove();
                    } catch {}
                  },
                  { once: true }
                );
              }
            }
          }
        })
      );
      // Owned panel chips
      if (ownedList) {
        ownedList.querySelectorAll(".owned-chip[data-exp]").forEach((chip) => {
          const exp = parseFloat(chip.dataset.exp || "0");
          if (exp) {
            const total = parseFloat(chip.dataset.total || "0");
            const remain = exp - now;
            if (remain > 0 && remain / total <= 0.01) {
              chip.classList.add("expiring");
            }
          }
          if (exp && now >= exp) {
            if (!chip.classList.contains("removing")) {
              chip.classList.add("removing");
              chip.addEventListener(
                "animationend",
                () => {
                  chip.remove();
                  if (!ownedList.querySelector(".owned-chip")) {
                    ownedPanel.style.display = "none";
                  }
                },
                { once: true }
              );
            }
          }
        });
      }
    }, 5000);
  }
  startExpiryWatcher();
})();

(function () {
  // Use dynamic max file size from backend
  const SIZE_LIMIT =
    typeof window.MAX_FILE_BYTES === "number"
      ? window.MAX_FILE_BYTES
      : 500 * 1024 * 1024;
  const SIZE_LIMIT_STR =
    typeof window.MAX_FILE_SIZE_STR === "string"
      ? window.MAX_FILE_SIZE_STR
      : "500MB";
  // Safe fallback if no previous handleFiles existed
  const origHandleFiles =
    window.handleFiles ||
    function (files) {
      addBatch(files);
    };
  window.handleFiles = function (fileList) {
    const arr = Array.from(fileList);
    const filtered = [];
    arr.forEach((f) => {
      if (f.size > SIZE_LIMIT) {
        showSnack(
          "Refused " +
            f.name +
            " (" +
            fmtBytes(f.size) +
            ", over " +
            SIZE_LIMIT_STR +
            ")",
          {
            fileSize: f.size,
            fileSizeStr: fmtBytes(f.size),
            fileName: f.name,
          }
        );
        console.error("File rejected (too large)", {
          name: f.name,
          size: f.size,
          size_fmt: fmtBytes(f.size),
          SIZE_LIMIT,
          SIZE_LIMIT_STR,
        });
      } else {
        filtered.push(f);
      }
    });
    return origHandleFiles(filtered);
  };
  if (typeof uploadOne === "function") {
    const _uo = uploadOne;
    uploadOne = function (f, b) {
      if (f.file.size > SIZE_LIMIT) {
        f.error = true;
        markFileTooLarge(f, "File exceeds " + SIZE_LIMIT_STR + " limit");
        return Promise.resolve();
      }
      return new Promise((res) => {
        _uo(f, b).then(() => res());
      });
    };
  }
  function markFileTooLarge(f, msg) {
    if (!f.container) {
      const li = document.createElement("li");
      li.className = "error";
      li.textContent = f.file.name + " - " + msg;
      li.style.color = "#e53935";
      list.appendChild(li);
      f.container = li;
    } else {
      f.container.classList.add("error");
      f.container.style.color = "#e53935";
    }
  }
  // Override local showSnack for this closure to reuse global error styling
  function showSnack(msg) {
    console.log("[showSnack]", msg, {
      MAX_FILE_BYTES: window.MAX_FILE_BYTES,
      MAX_FILE_SIZE_STR: window.MAX_FILE_SIZE_STR,
    });
    if (window.showSnack) {
      window.showSnack(msg);
    }
  }
})();

// Live countdown updater (updates every second for owned chips & any file rows with data-exp)
(function () {
  function formatRemain(sec) {
    if (sec <= 0) return "expired";
    if (sec < 60) return Math.floor(sec) + "s";
    if (sec < 3600) {
      const m = Math.floor(sec / 60);
      const s = Math.floor(sec % 60);
      return m + "m " + s + "s";
    }
    if (sec < 86400) {
      const h = Math.floor(sec / 3600);
      const m = Math.floor((sec % 3600) / 60);
      return h + "h " + m + "m";
    }
    const d = Math.floor(sec / 86400);
    const h = Math.floor((sec % 86400) / 3600);
    return d + "d " + h + "h";
  }
  function tick() {
    const now = Date.now() / 1000;
    // Owned chips
    document.querySelectorAll(".owned-chip[data-exp]").forEach((chip) => {
      const exp = parseFloat(chip.dataset.exp || "0");
      if (!exp) return;
      const remain = exp - now;
      const ttlEl = chip.querySelector(".ttl");
      if (ttlEl) {
        ttlEl.textContent = formatRemain(remain);
      }
      const total = parseFloat(chip.dataset.total || "0");
      if (total > 0 && remain > 0 && remain / total <= 0.01) {
        chip.classList.add("expiring");
      } else if (remain > 0) {
        chip.classList.remove("expiring");
      }
    });
    // File list items (if they store exp data)
    document.querySelectorAll("#fileList li[data-exp]").forEach((li) => {
      const exp = parseFloat(li.dataset.exp || "0");
      if (!exp) return;
      const remain = exp - now;
      let ttlEl = li.querySelector(".ttl-inline");
      if (!ttlEl) {
        ttlEl = document.createElement("div");
        ttlEl.className = "ttl-inline";
        ttlEl.style.cssText =
          "font-size:.5rem;opacity:.55;letter-spacing:.4px;margin-top:2px";
        li.appendChild(ttlEl);
      }
      ttlEl.textContent = "Expires: " + formatRemain(remain);
      const total = parseFloat(li.dataset.total || "0");
      if (total > 0 && remain > 0 && remain / total <= 0.01) {
        li.classList.add("expiring");
      } else if (remain > 0) {
        li.classList.remove("expiring");
      }
    });
  }
  setInterval(tick, 1000);
  tick();
})();

// Accessibility other
(function () {
  const dz = document.querySelector(".drop-zone");
  if (dz) {
    dz.setAttribute("role", "button");
    dz.setAttribute("tabindex", "0");
    dz.setAttribute(
      "aria-label",
      "Upload files: activate to choose or drag and drop"
    );
    dz.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        dz.click();
      }
    });
  }
  const list = document.getElementById("fileList");
  if (list) {
    list.setAttribute("aria-label", "Upload queue");
  }
  // Observe progress bar width changes to update aria-valuenow
  const updateBar = (el) => {
    if (!el) return;
    const span = el.querySelector("span");
    if (span) {
      span.setAttribute("role", "progressbar");
      span.setAttribute("aria-valuemin", "0");
      span.setAttribute("aria-valuemax", "100");
      const w = parseFloat(span.style.width) || 0;
      span.setAttribute("aria-valuenow", Math.round(w));
    }
  };
  const mo = new MutationObserver((m) =>
    m.forEach((x) => {
      if (x.type === "attributes" && x.target.tagName === "SPAN") {
        const s = x.target;
        if (s.getAttribute("role") === "progressbar") {
          const w = parseFloat(s.style.width) || 0;
          s.setAttribute("aria-valuenow", Math.round(w));
        }
      }
    })
  );
  document.addEventListener("DOMContentLoaded", () => {
    document.querySelectorAll(".bar").forEach((b) => {
      updateBar(b);
      const span = b.querySelector("span");
      if (span)
        mo.observe(span, { attributes: true, attributeFilter: ["style"] });
    });
  });
  // Patch global upload width changes if function exists
  const origSetWidth = (el, w) => {
    if (el) {
      el.style.width = w + "%";
      el.setAttribute("aria-valuenow", Math.round(w));
    }
  };
  // Expose helper if needed
  window._a11ySetBarWidth = origSetWidth;
  // Announce events
  const live = document.getElementById("liveStatus");
  window._announce = (msg) => {
    if (live) {
      live.textContent = "";
      setTimeout(() => (live.textContent = msg), 40);
    }
  };
  // Hook copy snackbar
  const sb = document.getElementById("snackbar");
  if (sb) {
    const obs = new MutationObserver(() => {
      if (sb.classList.contains("show"))
        window._announce("Action: " + sb.textContent.trim());
    });
    obs.observe(sb, { attributes: true, attributeFilter: ["class"] });
  }
})();
(function () {
  // Assign stable IDs if missing for skip targets
  const ownedPanelEl =
    window.ownedPanel || document.querySelector(".owned-panel, .owned");
  if (ownedPanelEl && !ownedPanelEl.id) ownedPanelEl.id = "ownedFiles";
  const fileListEl = document.getElementById("fileList");
  if (fileListEl) fileListEl.setAttribute("tabindex", "-1");

  // Enhance any TTL slider dynamically (range input with data-ttl-range)
  document
    .querySelectorAll("input[type=range][data-ttl-range]")
    .forEach((range) => {
      if (range.closest(".range-wrapper")) return;
      const wrap = document.createElement("div");
      wrap.className = "range-wrapper";
      range.parentNode.insertBefore(wrap, range);
      wrap.appendChild(range);
      const extra = document.createElement("div");
      extra.className = "range-extra";
      extra.setAttribute("aria-hidden", "true");
      const dec = document.createElement("button");
      dec.type = "button";
      dec.textContent = "-";
      dec.setAttribute("aria-label", "Decrease TTL");
      const inc = document.createElement("button");
      inc.type = "button";
      inc.textContent = "+";
      inc.setAttribute("aria-label", "Increase TTL");
      const lab = document.createElement("div");
      lab.style.fontSize = ".55rem";
      lab.style.textAlign = "center";
      lab.style.opacity = ".8";
      function syncLabel() {
        lab.textContent =
          range.getAttribute("aria-label") || "Value " + range.value;
      }
      syncLabel();
      dec.addEventListener("click", () => {
        range.stepDown();
        range.dispatchEvent(new Event("input", { bubbles: true }));
        syncLabel();
      });
      inc.addEventListener("click", () => {
        range.stepUp();
        range.dispatchEvent(new Event("input", { bubbles: true }));
        syncLabel();
      });
      extra.appendChild(inc);
      extra.appendChild(dec);
      extra.appendChild(lab);
      wrap.appendChild(extra);
      range.addEventListener("focus", () => {
        wrap.classList.add("keyboard-focus");
      });
      range.addEventListener("blur", () => {
        setTimeout(() => {
          if (!wrap.contains(document.activeElement))
            wrap.classList.remove("keyboard-focus");
        }, 10);
      });
      wrap.addEventListener("keydown", (e) => {
        if (e.key === "Escape") {
          wrap.classList.remove("keyboard-focus");
          range.blur();
        }
      });
      // Update on input
      range.addEventListener("input", syncLabel);
    });

  // Show controls only for keyboard navigation (detect last input modality)
  let lastPointer = false;
  window.addEventListener("pointerdown", () => {
    lastPointer = true;
  });
  window.addEventListener("keydown", (e) => {
    if (e.key !== "Tab") return;
    lastPointer = false;
  });
  document.addEventListener("focusin", (e) => {
    const wrap =
      e.target && e.target.closest && e.target.closest(".range-wrapper");
    if (wrap) {
      if (lastPointer) {
        wrap.classList.remove("keyboard-focus");
      } else {
        wrap.classList.add("keyboard-focus");
      }
    }
  });
})();
// Ensure owned panel (when created) reuses the ownedFiles id target for skip link
(function () {
  const placeholder = document.getElementById("ownedFiles");
  const observer = new MutationObserver(() => {
    const panel = document.querySelector(".owned-panel, .owned");
    if (panel && !panel.id) {
      panel.id = "ownedFiles";
      panel.setAttribute("tabindex", "-1");
      if (document.activeElement === placeholder) {
        panel.focus();
      }
      observer.disconnect();
    }
  });
  observer.observe(document.body, { subtree: true, childList: true });
})();
(function () {
  const MAX_BYTES =
    typeof window.MAX_FILE_BYTES === "number"
      ? window.MAX_FILE_BYTES
      : 500 * 1024 * 1024;
  const MAX_BOXES = 10;
  window.__JB_PREFILTER_ACTIVE = true; // flag to disable legacy drop addBatch path
  const snack = document.getElementById("snackbar");
  // Use global showSnack; fallback if missing
  function showSnackLocal(msg, opts) {
    opts = opts || {};
    const isError = opts.error !== false; // default true
    // If error and global showSnack exists, use it (includes shake)
    if (isError && window.showSnack) {
      window.showSnack(msg);
      return;
    }
    if (!snack) return;
    snack.textContent = msg;
    // Neutral path: ensure we remove error styling & no shake
    if (isError) {
      snack.classList.add("error");
    } else {
      snack.classList.remove("error");
    }
    snack.classList.add("show");
    clearTimeout(showSnackLocal._t);
    showSnackLocal._t = setTimeout(
      () => snack.classList.remove("show", "error"),
      isError ? 5000 : 1800
    );
  }
  function activeBoxes() {
    return document.querySelectorAll("#fileList li:not(.removing)").length;
  }
  const dz = document.querySelector(".drop-zone");
  const input = dz && dz.querySelector("input[type=file]");
  if (!input) return;

  // === New: Folder (directory) support with client-side ZIP packaging ===
  // Build CRC32 table once
  let _crcTable;
  function crc32(buf) {
    if (!_crcTable) {
      _crcTable = new Uint32Array(256);
      for (let n = 0; n < 256; n++) {
        let c = n;
        for (let k = 0; k < 8; k++) {
          c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
        }
        _crcTable[n] = c >>> 0;
      }
    }
    let crc = 0 ^ -1;
    for (let i = 0; i < buf.length; i++) {
      crc = _crcTable[(crc ^ buf[i]) & 0xff] ^ (crc >>> 8);
    }
    return (crc ^ -1) >>> 0;
  }
  function msToDosDateTime(ms) {
    const d = new Date(ms);
    let year = d.getFullYear();
    if (year < 1980) year = 1980;
    const dosTime =
      (d.getHours() << 11) |
      (d.getMinutes() << 5) |
      Math.floor(d.getSeconds() / 2);
    const dosDate =
      ((year - 1980) << 9) | ((d.getMonth() + 1) << 5) | d.getDate();
    return { dosTime, dosDate };
  }
  function pushU16(arr, v) {
    arr.push(v & 0xff, (v >> 8) & 0xff);
  }
  function pushU32(arr, v) {
    arr.push(v & 0xff, (v >> 8) & 0xff, (v >> 16) & 0xff, (v >> 24) & 0xff);
  }
  function pushStr(arr, str) {
    for (let i = 0; i < str.length; i++) {
      const c = str.charCodeAt(i);
      arr.push(c & 0xff);
    }
  }
  async function buildZipFile(fileEntries, rootName) {
    // fileEntries: [{file, relPath}]

    const localParts = [];
    const centralParts = [];
    let offset = 0;
    for (const entry of fileEntries) {
      const file = entry.file;
      const data = new Uint8Array(await file.arrayBuffer());
      const nameInZip =
        (rootName ? rootName + "/" : "") + entry.relPath.replace(/\\/g, "/");
      const { dosTime, dosDate } = msToDosDateTime(
        file.lastModified || Date.now()
      );
      const crc = crc32(data);
      const local = []; // Local file header
      pushU32(local, 0x04034b50);
      pushU16(local, 20);
      pushU16(local, 0);
      pushU16(local, 0);
      pushU16(local, dosTime);
      pushU16(local, dosDate);
      pushU32(local, crc);
      pushU32(local, data.length);
      pushU32(local, data.length);
      pushU16(local, nameInZip.length);
      pushU16(local, 0);
      pushStr(local, nameInZip);
      localParts.push(new Uint8Array(local));
      localParts.push(data); // Central directory
      const central = [];
      pushU32(central, 0x02014b50);
      pushU16(central, 20);
      pushU16(central, 20);
      pushU16(central, 0);
      pushU16(central, 0);
      pushU16(central, dosTime);
      pushU16(central, dosDate);
      pushU32(central, crc);
      pushU32(central, data.length);
      pushU32(central, data.length);
      pushU16(central, nameInZip.length);
      pushU16(central, 0);
      pushU16(central, 0);
      pushU16(central, 0);
      pushU16(central, 0);
      pushU32(central, 0);
      pushU32(central, offset);
      pushStr(central, nameInZip);
      centralParts.push(new Uint8Array(central));
      offset += local.length + data.length;
    }
    // Combine local parts length
    let centralSize = centralParts.reduce((n, a) => n + a.length, 0);
    let centralOffset = offset;
    const end = [];
    pushU32(end, 0x06054b50);
    pushU16(end, 0);
    pushU16(end, 0);
    const count = fileEntries.length;
    pushU16(end, count);
    pushU16(end, count);
    pushU32(end, centralSize);
    pushU32(end, centralOffset);
    pushU16(end, 0);
    const blobParts = [...localParts, ...centralParts, new Uint8Array(end)];
    const zipBlob = new Blob(blobParts, { type: "application/zip" });
    const zipFile = new File([zipBlob], (rootName || "folder") + ".zip", {
      type: "application/zip",
      lastModified: Date.now(),
    });
    return zipFile;
  }
  async function traverseDirectoryEntry(dirEntry) {
    // returns File objects list wrapped in {file, relPath}
    const out = [];
    async function walk(entry, path) {
      if (entry.isFile) {
        await new Promise((res) =>
          entry.file(
            (f) => {
              out.push({ file: f, relPath: path + f.name });
              res();
            },
            () => res()
          )
        );
      } else if (entry.isDirectory) {
        const reader = entry.createReader();
        async function readAll() {
          await new Promise((resolve) => {
            reader.readEntries(async (entries) => {
              if (!entries.length) return resolve();
              for (const e of entries) {
                await walk(e, path + entry.name + "/");
              }
              await readAll();
              resolve();
            });
          });
        }
        await readAll();
      }
    }
    await walk(dirEntry, "");
    return out;
  }
  async function packageDirectories(entries) {
    // entries: DataTransferItemEntry directories
    const zips = [];
    for (const dir of entries) {
      const name = dir.name || "folder";
      showSnackLocal("Packaging " + name + " ...", { error: false });
      let files = await traverseDirectoryEntry(dir);
      if (!files.length) {
        showSnackLocal("Empty folder skipped: " + name, { error: false });
        continue;
      } // size check
      const totalSize = files.reduce((n, o) => n + o.file.size, 0);
      if (totalSize > MAX_BYTES) {
        showSnackLocal(
          `Folder too large (> ${window.MAX_FILE_SIZE_STR}): ` + name
        );
        continue;
      }
      // Build zip

      const zipFile = await buildZipFile(files, name);
      if (zipFile.size > MAX_BYTES) {
        showSnackLocal(
          `Zipped folder exceeds ${window.MAX_FILE_SIZE_STR}: ` + name
        );
        continue;
      }
      zips.push(zipFile);
      showSnackLocal("Folder zipped: " + name, { error: false });
    }
    return zips;
  }
  // === End folder ZIP support ===

  // Return list of acceptable files given leftover slots
  function filterFileList(list, leftover) {
    const accept = [];
    let sizeRejected = 0;
    let emptySkipped = 0;
    for (const f of list) {
      if (accept.length >= leftover) break;
      if (f.size === 0) {
        emptySkipped++;
        continue;
      }
      if (f.size > MAX_BYTES) {
        sizeRejected++;
        continue;
      }
      accept.push(f);
    }
    return { accept, sizeRejected, emptySkipped };
  }
  function applyAcceptedToInput(inp, files) {
    const dt = new DataTransfer();
    files.forEach((f) => dt.items.add(f));
    inp.files = dt.files;
  }
  function handleSelection(files) {
    const current = activeBoxes();
    if (current >= MAX_BOXES) {
      showSnackLocal("Maximum of 10 files in queue");
      return false;
    }
    if (!files || !files.length) return false; // nothing dragged
    const leftover = MAX_BOXES - current;
    const { accept, sizeRejected, emptySkipped } = filterFileList(
      files,
      leftover
    );
    // Feedback hierarchy
    if (accept.length === 0) {
      if (sizeRejected && !emptySkipped) {
        showSnackLocal(`File too large (limit ${window.MAX_FILE_SIZE_STR})`);
        return false;
      }
      if (emptySkipped && !sizeRejected) {
        showSnackLocal("File appears empty");
        return false;
      }
      if (sizeRejected && emptySkipped) {
        showSnackLocal("Files skipped (empty / too large)");
        return false;
      }
      // Fallback: only reason left would be no slots actually available
      if (leftover <= 0) {
        showSnackLocal("No slots available");
      }
      return false;
    }
    if (sizeRejected || emptySkipped) {
      const parts = [];
      if (accept.length !== files.length)
        parts.push("added " + accept.length + " of " + files.length);
      if (sizeRejected)
        parts.push(sizeRejected + ` too large (> ${window.MAX_FILE_SIZE_STR})`);
      if (emptySkipped) parts.push(emptySkipped + " empty");
      showSnackLocal(parts.join("; "));
    }
    applyAcceptedToInput(input, accept);
    return true;
  }
  // Capture phase to pre-filter before existing listeners (also handle folders)
  input.addEventListener(
    "change",
    (e) => {
      const list = Array.from(input.files || []);
      handleSelection(list);
    },
    true
  );
  if (dz) {
    dz.addEventListener(
      "drop",
      (e) => {
        (async () => {
          if (!e.dataTransfer) return; // already handled upstream
          const items = Array.from(e.dataTransfer.items || []);
          const dirEntries = items
            .map((it) => it.webkitGetAsEntry && it.webkitGetAsEntry())
            .filter((en) => en && en.isDirectory);
          let producedZips = [];
          if (dirEntries.length) {
            e.preventDefault();
            e.stopPropagation();
            try {
              producedZips = await packageDirectories(dirEntries);
            } catch (err) {
              showSnackLocal("Folder packaging failed");
            }
          }
          // Non-directory file objects (ignore ones representing directories).
          const regularFiles = Array.from(e.dataTransfer.files || []);
          const combined = producedZips.concat(regularFiles);
          if (!handleSelection(combined)) {
            dz.classList.remove("drag");
            return;
          }
          const evt = new Event("change", { bubbles: true });
          input.dispatchEvent(evt);
          // Ensure drag visual state cleared post processing
          dz.classList.remove("drag");
        })();
      },
      true
    );
    // Global safety: clear drag state after any drop anywhere
    window.addEventListener("drop", () => dz.classList.remove("drag"), true);
  }
})();

// Clipboard paste support (Ctrl+V): paste images/files directly
(function () {
  const fi = fileInput;
  const dz = dropZone;
  if (!fi) return;
  window.addEventListener("paste", (e) => {
    const cd = e.clipboardData;
    if (!cd) return;
    const files = cd.files;
    const items = cd.items;
    const dt = new DataTransfer();
    let added = 0;
    if (files && files.length) {
      for (const f of files) {
        dt.items.add(f);
        added++;
      }
    } else if (items && items.length) {
      for (const it of items) {
        if (it.kind === "file") {
          const f = it.getAsFile();
          if (f) {
            dt.items.add(f);
            added++;
          }
        }
      }
    }
    if (added > 0) {
      fi.files = dt.files;
      fi.dispatchEvent(new Event("change", { bubbles: true }));
      if (dz) {
        dz.classList.add("pasted");
        setTimeout(() => dz.classList.remove("pasted"), 1200);
      }
    }
  });
})();
// Add 413 fallback + chunked upload support
(function () {
  if (typeof uploadOne === "function") {
    const _origUploadOne = uploadOne;
    uploadOne = function (f, batch) {
      return new Promise((resolve) => {
        _origUploadOne(f, batch).then(() => {
          // If already succeeded / failed / canceled, stop.
          if (f.done || f.error || f.canceled) return resolve();
          // Detect 413 from classic upload attempt
          if (f.xhr && f.xhr.status === 413) {
            const hdr =
              f.xhr.getResponseHeader &&
              f.xhr.getResponseHeader("X-File-Too-Large");
            if (hdr === "1") {
              // actual file too large for server policy
              if (f.container) f.container.classList.add("error");
              if (window.showSnack)
                window.showSnack("File exceeds server limit");
              return resolve();
            }
            // Fallback: chunked upload
            if (window.showSnack)
              window.showSnack("Retrying large file with chunked upload...");
            chunkedUpload(f, batch).then(() => resolve());
            return;
          }
          resolve();
        });
      });
    };
  }
  function extractName(rel) {
    if (!rel) return rel;
    const idx = rel.indexOf("/f/");
    if (idx !== -1) {
      return rel.substring(idx + 3).replace(/^\//, "");
    }
    return rel.replace(/^f\//, "");
  }
  function finalizeSuccess(f, batch, ttlVal) {
    if (f.barSpan) {
      f.barSpan.style.width = "100%";
      requestAnimationFrame(() => {
        f.barSpan.classList.add("complete");
        setTimeout(() => {
          if (f.bar && f.barSpan.classList.contains("complete"))
            f.bar.classList.add("divider");
        }, 1000);
      });
    }
    const ttlSeconds = ttlCodeSeconds(ttlVal);
    const exp = Math.floor(Date.now() / 1000) + ttlSeconds;
    ownedMeta.set(f.remoteName, { expires: exp, total: ttlSeconds });
    f.done = true;
    updateDeleteButton(f);
    if (f.remoteName) {
      const input = makeLinkInput(
        "f/" + f.remoteName,
        !batch.files.some((x) => !x.done)
      );
      if (batch.isGroup) {
        if (f.container) {
          let linksRow = f.container.querySelector(".links");
          if (!linksRow) {
            linksRow = document.createElement("div");
            linksRow.className = "links";
            f.container.appendChild(linksRow);
          }
          linksRow.appendChild(input);
        }
        if (batch.groupLi && !batch.groupLi.dataset.exp) {
          batch.groupLi.dataset.exp = exp;
          batch.groupLi.dataset.total = ttlSeconds;
        }
      } else {
        if (batch.files.length > 1) {
          if (batch.linksBox) batch.linksBox.appendChild(input);
        } else {
          const links = document.createElement("div");
          links.className = "links";
          links.appendChild(input);
          f.container.appendChild(links);
        }
      }
    }
  }
  function chunkedUpload(f, batch) {
    const CHUNK_SIZE = 8 * 1024 * 1024; // 8MB
    const ttlVal = getTTL();
    const total = f.file.size;
    const id =
      (crypto.randomUUID && crypto.randomUUID()) ||
      Date.now().toString(36) + Math.random().toString(36).slice(2, 10);
    const count = Math.ceil(total / CHUNK_SIZE);
    let uploaded = 0;
    function updateBar() {
      if (f.barSpan) {
        const pct = (uploaded / total) * 100;
        f.barSpan.style.width = pct.toFixed(2) + "%";
      }
    }
    return (async () => {
      for (let i = 0; i < count; i++) {
        if (f.canceled) return; // stop if user canceled
        const start = i * CHUNK_SIZE;
        const end = Math.min(start + CHUNK_SIZE, total);
        const blob = f.file.slice(start, end);
        let resp;
        try {
          resp = await fetch("/upload/chunk", {
            method: "POST",
            headers: {
              "X-Upload-Id": id,
              "X-Filename": f.file.name,
              "X-Chunk-Index": String(i),
              "X-Chunk-Count": String(count),
              "X-Total-Size": String(total),
              "X-Ttl": ttlVal,
            },
            body: blob,
          });
        } catch {
          resp = null;
        }
        if (!resp || !resp.ok) {
          if (f.container) f.container.classList.add("error");
          return;
        }
        uploaded = end;
        updateBar();
        if (i === count - 1) {
          let data = null;
          try {
            data = await resp.json();
          } catch {}
          let rel = data && data.files && data.files[0];
          if (rel) {
            f.remoteName = extractName(rel);
            finalizeSuccess(f, batch, ttlVal);
          }
        }
      }
    })();
  }
})();
(function enforceActiveLimit() {
  const MAX_ACTIVE = 5;
  const dz = document.getElementById("dropZone");
  if (dz && !document.getElementById("activeLimitNote")) {
    const note = document.createElement("div");
    note.id = "activeLimitNote";
    note.className = "muted";
    note.style.fontSize = ".57rem";
    note.style.letterSpacing = ".4px";
    dz.insertAdjacentElement("afterend", note);
  }
  const origUploadOne = uploadOne;
  uploadOne = function (f, batch) {
    return origUploadOne(f, batch).then(() => {
      if (f && f.xhr && f.xhr.response && typeof f.xhr.response === "object") {
        const r = f.xhr.response;
        if (r && typeof r.remaining === "number")
          window.__ACTIVE_SLOTS_REMAINING = r.remaining;
      }
    });
  };
  const origAddBatch = addBatch;
  addBatch = function (fileList) {
    const filesArr = Array.from(fileList);
    let inFlight = 0;
    batches.forEach((b) =>
      b.files.forEach((f) => {
        if (!f.remoteName && !f.removed) inFlight++;
      })
    );
    const totalOwned = window.ownedCache ? window.ownedCache.size : 0;
    const activeProjected = totalOwned + inFlight;
    const slotsFromProjection = MAX_ACTIVE - activeProjected;
    const knownRemaining =
      typeof window.__ACTIVE_SLOTS_REMAINING === "number"
        ? window.__ACTIVE_SLOTS_REMAINING
        : null;
    const available =
      knownRemaining !== null
        ? Math.min(knownRemaining, slotsFromProjection)
        : slotsFromProjection;
    function shake(msg) {
      showSnack(msg);
      if (dz) {
        dz.classList.add("shake");
        dz.addEventListener(
          "animationend",
          () => dz.classList.remove("shake"),
          { once: true }
        );
      }
    }
    if (available <= 0) {
      shake("Limit 5: no free slots. Delete a file first.");
      return;
    }
    if (filesArr.length > available) {
      if (filesArr.length > 1) {
        shake(
          "Group needs " +
            filesArr.length +
            " slots; only " +
            available +
            " free. Delete files to upload this group."
        );
      } else {
        shake("Need 1 free slot; none available.");
      }
      return; // atomic: reject entire selection
    }
    return origAddBatch(filesArr);
  };
})();
(function patchUploadResponseHandling() {
  // this function sucks.
  if (typeof uploadOne === "function") {
    const orig = uploadOne;
    uploadOne = function (f, batch) {
      return orig(f, batch).then(() => {
        if (f && f.xhr && f.xhr.response) {
          const resp = f.xhr.response;
          if (resp && resp.truncated) {
            showSnack("Some files skipped: active file cap (5)");
          }
          if (typeof resp.remaining === "number") {
            window.__ACTIVE_SLOTS_REMAINING = resp.remaining;
          }
        }
      });
    };
  }
})();
(function activeSlotsAfterDelete() {
  const MAX_ACTIVE = 5;
  function recompute() {
    try {
      const owned =
        window.ownedCache && window.ownedCache.size
          ? window.ownedCache.size
          : 0;
      window.__ACTIVE_SLOTS_REMAINING = Math.max(0, MAX_ACTIVE - owned);
    } catch {}
  }
  window.recomputeActiveSlots = recompute;
  if (!window.__fetchPatchedForSlots) {
    window.__fetchPatchedForSlots = true;
    const origFetch = window.fetch.bind(window);
    window.fetch = function (input, init) {
      const isDel = typeof input === "string" && input.startsWith("/d/");
      const m = (init && init.method) || "GET";
      if (isDel && m === "DELETE") {
        const fname = decodeURIComponent(input.substring(3));
        return origFetch(input, init).then((r) => {
          if (r.ok) {
            if (window.ownedCache && window.ownedCache.delete(fname)) {
            }
            recompute();
            // also update UI note if present
            const note = document.getElementById("activeLimitNote");
            if (note && typeof window.__ACTIVE_SLOTS_REMAINING === "number") {
              note.textContent =
                "Limit: max 5 active files per IP (slots left " +
                window.__ACTIVE_SLOTS_REMAINING +
                ").";
            }
          }
          return r;
        });
      }
      return origFetch(input, init);
    };
  }
  // Initial recompute after owned list load cycle
  setTimeout(recompute, 1500);
})();
