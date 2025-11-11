import {
  describe,
  it,
  expect,
  beforeEach,
  afterEach,
  jest,
} from "@jest/globals";

import { deleteHandler } from "../public/js/delete.js";
import { uploadHandler } from "../public/js/upload.js";

// Helper to flush pending microtasks (for async callbacks inside deleteRemote)
async function flushMicrotasks() {
  await Promise.resolve();
  await Promise.resolve();
}

function makeFile(overrides = {}) {
  const btn = document.createElement("button");
  const container = document.createElement("div");
  // Ensure container is attached so getComputedStyle/getBoundingClientRect are safe
  document.body.appendChild(container);
  return {
    deleteBtn: btn,
    container,
    remoteName: null,
    done: false,
    failed: false,
    deleting: false,
    ...overrides,
  };
}

describe("delete.js basic behavior without deep mocks", () => {
  beforeEach(() => {
    // Fresh DOM and globals
    document.body.innerHTML = `
      <div id="dropZone"></div>
    `;
    // Reset upload batches
    uploadHandler.batches = [];
    // Provide a default fetch stub; individual tests override as needed
    global.fetch = jest
      .fn()
      .mockResolvedValue({ ok: true, json: async () => ({}) });
  });

  afterEach(() => {
    document.body.innerHTML = "";
    uploadHandler.batches = [];
    delete global.fetch;
  });

  describe("updateDeleteButton", () => {
    it("sets deleting state", () => {
      const f = makeFile({ deleting: true });
      deleteHandler.updateDeleteButton(f);
      expect(f.deleteBtn.disabled).toBe(true);
      expect(f.deleteBtn.title).toBe("Deleting...");
      expect(f.deleteBtn.getAttribute("aria-label")).toBe("Deleting file");
    });

    it("sets failed upload state", () => {
      const f = makeFile({ failed: true });
      deleteHandler.updateDeleteButton(f);
      expect(f.deleteBtn.disabled).toBe(false);
      expect(f.deleteBtn.title).toBe("Remove failed upload");
      expect(f.deleteBtn.getAttribute("aria-label")).toBe(
        "Remove failed upload from queue",
      );
    });

    it("sets 'not uploaded' state when no remoteName", () => {
      const f = makeFile();
      deleteHandler.updateDeleteButton(f);
      expect(f.deleteBtn.disabled).toBe(false);
      expect(f.deleteBtn.title).toBe("Remove (not uploaded)");
      expect(f.deleteBtn.getAttribute("aria-label")).toBe(
        "Remove file from upload queue",
      );
    });

    it("sets 'cancel upload' state when remoteName but not done", () => {
      const f = makeFile({ remoteName: "abc", done: false });
      deleteHandler.updateDeleteButton(f);
      expect(f.deleteBtn.disabled).toBe(false);
      expect(f.deleteBtn.title).toBe("Cancel upload");
      expect(f.deleteBtn.getAttribute("aria-label")).toBe("Cancel upload");
    });

    it("sets 'delete from server' when remoteName and done", () => {
      const f = makeFile({ remoteName: "abc", done: true });
      deleteHandler.updateDeleteButton(f);
      expect(f.deleteBtn.disabled).toBe(false);
      expect(f.deleteBtn.title).toBe("Delete from server");
      expect(f.deleteBtn.getAttribute("aria-label")).toBe(
        "Delete uploaded file",
      );
    });

    it("no-op if deleteBtn missing", () => {
      const f = { deleting: true };
      // should not throw
      deleteHandler.updateDeleteButton(f);
    });
  });

  describe("handleDeleteClick", () => {
    it("returns early when already deleting", () => {
      const f = makeFile({ deleting: true });
      const batch = { files: [f] };
      deleteHandler.handleDeleteClick(f, batch);
      expect(batch.files).toHaveLength(1);
      expect(f.deleting).toBe(true);
    });

    it("removes local pending upload (no remoteName/done=false) and prunes batch list", () => {
      const f = makeFile({ remoteName: null, done: false });
      const batch = { files: [f] };
      uploadHandler.batches = [batch];

      // Trigger delete click; this will call animateRemove which waits for animationend
      deleteHandler.handleDeleteClick(f, batch);

      // Fire animationend to execute removal callback
      f.container.dispatchEvent(new Event("animationend"));

      expect(batch.files).toHaveLength(0);
      expect(uploadHandler.batches).toHaveLength(0);
    });

    it("delegates to deleteRemote when remoteName is set and done=true", async () => {
      // Provide successful delete
      global.fetch = jest
        .fn()
        .mockResolvedValue({ ok: true, json: async () => ({}) });

      const f = makeFile({ remoteName: "to-delete", done: true });
      // Avoid animation complexities: no container => direct removal branch
      delete f.container;
      const batch = { files: [f] };
      uploadHandler.batches = [batch];

      deleteHandler.handleDeleteClick(f, batch);
      await flushMicrotasks();

      expect(batch.files).toHaveLength(0);
      expect(uploadHandler.batches).toHaveLength(0);
    });
  });

  describe("deleteRemote end-to-end with DOM snackbar", () => {
    it("no-op if remoteName missing", () => {
      const f = makeFile({ remoteName: "", done: true });
      const batch = { files: [f] };
      deleteHandler.deleteRemote(f, batch);
      // Nothing changed
      expect(batch.files).toHaveLength(1);
    });

    it("successful deletion removes file and batch (no container)", async () => {
      global.fetch = jest
        .fn()
        .mockResolvedValue({ ok: true, json: async () => ({}) });

      const f = makeFile({ remoteName: "file-ok", done: true });
      delete f.container;
      const batch = { files: [f] };
      uploadHandler.batches = [batch];

      deleteHandler.deleteRemote(f, batch);
      await flushMicrotasks();

      expect(batch.files).toHaveLength(0);
      expect(uploadHandler.batches).toHaveLength(0);
    });

    it("failed deletion shows server message and resets button state", async () => {
      global.fetch = jest
        .fn()
        .mockResolvedValue({
          ok: false,
          json: async () => ({ message: "Nope." }),
        });

      const f = makeFile({ remoteName: "file-fail", done: true });
      const batch = { files: [f] };
      uploadHandler.batches = [batch];

      deleteHandler.deleteRemote(f, batch);
      await flushMicrotasks();

      // Snackbar DOM should exist with error text
      const sb = document.getElementById("snackbar");
      expect(sb).not.toBeNull();
      expect(sb.textContent).toBe("Nope.");
      expect(sb.classList.contains("show")).toBe(true);
      expect(sb.classList.contains("error")).toBe(true);

      // File still present, and UI reset for retry
      expect(batch.files).toHaveLength(1);
      expect(f.deleting).toBe(false);
      expect(f.deleteBtn.title).toBe("Delete from server");
    });

    it("failed deletion without message shows generic error", async () => {
      global.fetch = jest
        .fn()
        .mockResolvedValue({ ok: false, json: async () => ({}) });

      const f = makeFile({ remoteName: "file-fail2", done: true });
      const batch = { files: [f] };
      uploadHandler.batches = [batch];

      deleteHandler.deleteRemote(f, batch);
      await flushMicrotasks();

      const sb = document.getElementById("snackbar");
      expect(sb).not.toBeNull();
      expect(sb.textContent).toBe("Delete failed.");
      expect(f.deleting).toBe(false);
    });

    it("network error shows generic error and resets button", async () => {
      global.fetch = jest.fn().mockRejectedValue(new Error("network down"));

      const f = makeFile({ remoteName: "file-err", done: true });
      const batch = { files: [f] };
      uploadHandler.batches = [batch];

      deleteHandler.deleteRemote(f, batch);
      await flushMicrotasks();

      const sb = document.getElementById("snackbar");
      expect(sb).not.toBeNull();
      expect(sb.textContent).toBe("Delete failed.");
      expect(f.deleting).toBe(false);
      expect(f.deleteBtn.title).toBe("Delete from server");
    });
  });

  describe("removeFromUploads removes matching entries across batches (with container)", () => {
    it("removes matching remote names and prunes empty batches", () => {
      const f1 = makeFile({ remoteName: "target" });
      const f2 = makeFile({ remoteName: "other" });
      const f3 = makeFile({ remoteName: "target" });

      const batchA = { files: [f1, f2] };
      const batchB = { files: [f3] };
      uploadHandler.batches = [batchA, batchB];

      deleteHandler.removeFromUploads("target");

      // simulate animation end for both containers created by animateRemove
      f1.container.dispatchEvent(new Event("animationend"));
      f3.container.dispatchEvent(new Event("animationend"));

      expect(batchA.files).toEqual([f2]);
      expect(batchB.files).toHaveLength(0);

      // If a batch becomes empty, it should be pruned
      expect(uploadHandler.batches).toContain(batchA);
      expect(uploadHandler.batches).not.toContain(batchB);
    });

    it("does nothing if name doesn't match any entry", () => {
      const f1 = makeFile({ remoteName: "a" });
      const f2 = makeFile({ remoteName: "b" });
      const batch = { files: [f1, f2] };
      uploadHandler.batches = [batch];

      deleteHandler.removeFromUploads("c");
      // Fire possible animation events (no-op if no listeners)
      f1.container.dispatchEvent(new Event("animationend"));
      f2.container.dispatchEvent(new Event("animationend"));

      expect(batch.files).toHaveLength(2);
      expect(uploadHandler.batches).toHaveLength(1);
    });
  });
});
