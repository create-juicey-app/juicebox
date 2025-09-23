// js/events.js

import { dropZone, fileInput } from './ui.js';
import { uploadHandler } from './upload.js';
import { ownedHandler } from './owned.js';

function setupUploadEvents() {
  if (!dropZone) return;
  ["dragenter", "dragover"].forEach((evt) =>
    dropZone.addEventListener(evt, (e) => {
      e.preventDefault();
      e.stopPropagation();
      dropZone.classList.add("drag");
    })
  );
  ["dragleave", "dragend", "drop"].forEach((evt) =>
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
      if (fileInput.files && fileInput.files.length) {
        uploadHandler.addBatch(fileInput.files);
      }
      try {
        fileInput.value = "";
      } catch {}
    });
  }
}

export function setupEventListeners() {
  setupUploadEvents();

  dropZone.addEventListener("drop", (e) => {
    if (window.__JB_PREFILTER_ACTIVE) return; // Handled by enhancement
    if (e.dataTransfer && e.dataTransfer.files.length) {
      uploadHandler.addBatch(e.dataTransfer.files);
    }
  });

  // Paste support is in other.js

  // Periodic refresh
  setInterval(() => ownedHandler.refreshOwned(), 15000);
  window.addEventListener("focus", () => ownedHandler.refreshOwned());
}