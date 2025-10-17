// js/delete.js

import { animateRemove } from "./utils.js";
import { uploadHandler } from "./upload.js";
import { ownedHandler } from "./owned.js";
import { startSpan } from "./telemetry.js";

export const deleteHandler = {
  updateDeleteButton(f) {
    if (!f.deleteBtn) return;
    if (f.deleting) {
      f.deleteBtn.disabled = true;
      f.deleteBtn.textContent = "…";
      f.deleteBtn.title = "Deleting...";
      f.deleteBtn.setAttribute("aria-label", "Deleting file");
    } else if (f.failed) {
      f.deleteBtn.textContent = "❌";
      f.deleteBtn.disabled = false;
      f.deleteBtn.title = "Remove failed upload";
      f.deleteBtn.setAttribute("aria-label", "Remove failed upload from queue");
    } else if (!f.remoteName) {
      f.deleteBtn.textContent = "❌";
      f.deleteBtn.disabled = false;
      f.deleteBtn.title = "Remove (not uploaded)";
      f.deleteBtn.setAttribute("aria-label", "Remove file from upload queue");
    } else if (!f.done) {
      f.deleteBtn.textContent = "❌";
      f.deleteBtn.disabled = false;
      f.deleteBtn.title = "Cancel upload";
      f.deleteBtn.setAttribute("aria-label", "Cancel upload");
    } else {
      f.deleteBtn.textContent = "❌";
      f.deleteBtn.disabled = false;
      f.deleteBtn.title = "Delete from server";
      f.deleteBtn.setAttribute("aria-label", "Delete uploaded file");
    }
  },

  handleDeleteClick(f, batch) {
    if (f.deleting) return;
    if (!f.remoteName || !f.done) {
      uploadHandler.cancelPendingUpload(f);
      if (f.container) {
        animateRemove(f.container, () => {
          batch.files = batch.files.filter((x) => x !== f);
          if (!batch.files.length) {
            uploadHandler.batches = uploadHandler.batches.filter(
              (b) => b !== batch
            );
          }
        });
      }
      return;
    }
    this.deleteRemote(f, batch);
  },

  deleteRemote(f, batch) {
    if (!f.remoteName || f.deleting) return;
    const targetName = ownedHandler.normalizeRemoteName(f.remoteName);
    if (!targetName) {
      showSnack("Invalid file reference.");
      this.updateDeleteButton(f);
      return;
    }
    f.remoteName = targetName;
    f.deleting = true;
    this.updateDeleteButton(f);

    startSpan(
      "file.delete",
      {
        op: "http.client",
        attributes: {
          "http.method": "DELETE",
          "http.url": `/d/${targetName}`,
          "file.name": targetName,
        },
      },
      async () => {
        try {
          const r = await fetch("/d/" + encodeURIComponent(targetName), {
            method: "DELETE",
          });
          if (r.ok) {
            ownedHandler.ownedCache.delete(targetName);
            ownedHandler.ownedMeta.delete(targetName);
            ownedHandler.refreshOwned();
            this.removeFromUploads(targetName);
            if (f.container) {
              animateRemove(f.container, () => {
                batch.files = batch.files.filter((x) => x !== f);
                if (!batch.files.length) {
                  uploadHandler.batches = uploadHandler.batches.filter(
                    (b) => b !== batch
                  );
                }
              });
            } else {
              batch.files = batch.files.filter((x) => x !== f);
              if (!batch.files.length) {
                uploadHandler.batches = uploadHandler.batches.filter(
                  (b) => b !== batch
                );
              }
            }
          } else {
            let msg = "Delete failed.";
            try {
              const err = await r.json();
              if (err && err.message) msg = err.message;
            } catch {}
            showSnack(msg);
            f.deleting = false;
            this.updateDeleteButton(f);
          }
        } catch (err) {
          showSnack("Delete failed.");
          f.deleting = false;
          this.updateDeleteButton(f);
        }
      }
    );
  },

  removeFromUploads(remoteName) {
    const targetName = ownedHandler.normalizeRemoteName(remoteName);
    if (!targetName) return;
    uploadHandler.batches.forEach((batch) => {
      batch.files.forEach((f) => {
        const entryName = ownedHandler.normalizeRemoteName(f.remoteName);
        if (entryName === targetName) {
          f.remoteName = entryName;
          if (f.container) {
            animateRemove(f.container, () => {
              batch.files = batch.files.filter((x) => x !== f);
              if (!batch.files.length) {
                uploadHandler.batches = uploadHandler.batches.filter(
                  (b) => b !== batch
                );
              }
            });
          }
        }
      });
    });
  },
};
