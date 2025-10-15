// js/delete.js

import { animateRemove } from "./utils.js";
import { uploadHandler } from "./upload.js";
import { ownedHandler } from "./owned.js";

export const deleteHandler = {
  updateDeleteButton(f) {
    if (!f.deleteBtn) return;
    if (f.deleting) {
      f.deleteBtn.disabled = true;
      f.deleteBtn.textContent = "…";
      f.deleteBtn.title = "Deleting...";
      f.deleteBtn.setAttribute("aria-label", "Deleting file");
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

  removeGroupedEntry(batch, f) {
    // Remove file from batch.files directly
    batch.files = batch.files.filter((x) => x !== f);
    const finalize = () => {
      if (!batch.files.length) {
        animateRemove(batch.groupLi);
        uploadHandler.batches = uploadHandler.batches.filter(
          (b) => b !== batch
        );
      } else {
        // Optionally update group header/UI here if needed
      }
    };
    if (f.container) {
      animateRemove(f.container, finalize);
    } else finalize();
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
    f.deleting = true;
    this.updateDeleteButton(f);
    fetch("/d/" + encodeURIComponent(f.remoteName), { method: "DELETE" })
      .then(async (r) => {
        if (r.ok) {
          ownedHandler.ownedCache.delete(f.remoteName);
          ownedHandler.ownedMeta.delete(f.remoteName);
          ownedHandler.renderOwned();
          this.removeFromUploads(f.remoteName); // <-- Ensure removal from upload section
          // Always use removeGroupedEntry if batch and groupLi exist
          if (batch && batch.groupLi) {
            this.removeGroupedEntry(batch, f);
          } else if (f.container) {
            animateRemove(f.container, () => {
              batch.files = batch.files.filter((x) => x !== f);
              if (!batch.files.length) {
                uploadHandler.batches = uploadHandler.batches.filter(
                  (b) => b !== batch
                );
              }
            });
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
      })
      .catch(() => {
        showSnack("Delete failed.");
        f.deleting = false;
        this.updateDeleteButton(f);
      });
  },

  removeFromUploads(remoteName) {
    if (!remoteName) return;
    uploadHandler.batches.forEach((batch) => {
      batch.files.forEach((f) => {
        if (f.remoteName === remoteName) {
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
