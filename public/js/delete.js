// js/delete.js

import { animateRemove } from './utils.js';
import { uploadHandler } from './upload.js';
import { ownedHandler } from './owned.js';

export const deleteHandler = {
  updateDeleteButton(f) {
    if (!f.deleteBtn) return;
    if (f.deleting) {
      f.deleteBtn.disabled = true;
      f.deleteBtn.textContent = "…";
      f.deleteBtn.title = "Deleting...";
    } else if (!f.remoteName) {
      f.deleteBtn.textContent = "❌";
      f.deleteBtn.disabled = false;
      f.deleteBtn.title = "Remove (not uploaded)";
    } else {
      f.deleteBtn.textContent = "🗑️";
      f.deleteBtn.disabled = false;
      f.deleteBtn.title = "Delete from server";
    }
  },

  removeGroupedEntry(batch, f) {
    f.removed = true;
    const finalize = () => {
      batch.files = batch.files.filter((x) => !x.removed);
      if (!batch.files.length) {
        // markGroupEmpty(batch); // This function is complex, simplified for now
        animateRemove(batch.groupLi);
        uploadHandler.batches = uploadHandler.batches.filter(b => b !== batch);
      } else {
        // updateGroupHeader(batch);
      }
    };
    if (f.container) {
      animateRemove(f.container, finalize);
    } else finalize();
  },

  handleDeleteClick(f, batch) {
    if (f.deleting) return;
    if (!f.remoteName) {
      if (f.xhr) {
        f.canceled = true;
        try { f.xhr.abort(); } catch {}
      }
      if (f.container) {
        animateRemove(f.container, () => {
          batch.files = batch.files.filter((x) => x !== f);
          if (!batch.files.length) {
            uploadHandler.batches = uploadHandler.batches.filter((b) => b !== batch);
          }
        });
      }
    } else {
      this.deleteRemote(f, batch);
    }
  },

  deleteRemote(f, batch) {
    if (!f.remoteName || f.deleting) return;
    f.deleting = true;
    this.updateDeleteButton(f);
    fetch("/d/" + encodeURIComponent(f.remoteName), { method: "DELETE" })
      .then((r) => {
        if (r.ok) {
          ownedHandler.ownedCache.delete(f.remoteName);
          ownedHandler.ownedMeta.delete(f.remoteName);
          ownedHandler.renderOwned();
          if (f.container) {
            animateRemove(f.container, () => {
              batch.files = batch.files.filter((x) => x !== f);
              if (!batch.files.length) {
                uploadHandler.batches = uploadHandler.batches.filter((b) => b !== batch);
              }
            });
          }
        } else {
          f.deleting = false;
          this.updateDeleteButton(f);
        }
      })
      .catch(() => {
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
                uploadHandler.batches = uploadHandler.batches.filter((b) => b !== batch);
              }
            });
          }
        }
      });
    });
  },
};