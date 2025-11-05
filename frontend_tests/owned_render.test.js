import {
  jest,
  describe,
  it,
  expect,
  beforeEach,
  afterEach,
} from "@jest/globals";

// Run rAF callbacks synchronously in tests
function rafImmediate() {
  global.requestAnimationFrame = (cb) => cb();
}

function setupDOM() {
  document.body.innerHTML = `
    <main>
      <div id="ownedPanel"></div>
      <ul id="ownedList"></ul>
    </main>
  `;
}

function mockDeps() {
  // Keep deletes lightweight; avoid importing upload.js and network logic
  jest.unstable_mockModule("../public/js/delete.js", () => ({
    deleteHandler: {
      updateDeleteButton: jest.fn(),
      removeFromUploads: jest.fn(),
    },
  }));

  // Utilities used by owned.js; return no-ops
  jest.unstable_mockModule("../public/js/utils.js", () => ({
    escapeHtml: (s) => String(s ?? ""),
    copyToClipboard: () => Promise.resolve(),
    flashCopied: () => {},
    showSnack: () => {},
    animateRemove: (el, cb) => {
      if (cb) cb();
    },
    fmtBytes: (n) => `${n}`,
  }));

  // Telemetry wrapper should just run the callback
  jest.unstable_mockModule("../public/js/telemetry.js", () => ({
    startSpan: async (_name, _attrs, fn) => (typeof fn === "function" ? await fn() : undefined),
    initTelemetry: () => {},
    captureException: () => {},
  }));
}

async function loadOwned() {
  // Import after DOM and mocks are ready so owned.js captures the elements
  const mod = await import("../public/js/owned.js");
  return mod.ownedHandler;
}

describe("owned list rendering and persistence", () => {
  beforeEach(() => {
    jest.resetModules();
    setupDOM();
    rafImmediate();
    mockDeps();
  });

  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("keeps the same grid node and does not disappear when adding a file", async () => {
    const owned = await loadOwned();

    // Initial state with two files
    owned.applyResponse({ files: ["alpha", "beta"] });
    owned.renderOwned();

    const ownedList = document.getElementById("ownedList");
    const grid1 = ownedList.querySelector('.owned-grid[data-role="owned"]');
    expect(grid1).not.toBeNull();
    expect(grid1.isConnected).toBe(true);
    expect(grid1.querySelectorAll(".owned-chip").length).toBe(2);

    const panel = document.getElementById("ownedPanel");
    expect(panel.getAttribute("data-state")).toBe("has-files");

    // Add a new file and re-render
    owned.applyResponse({ files: ["alpha", "beta", "gamma"] });
    owned.renderOwned();

    const grid2 = ownedList.querySelector('.owned-grid[data-role="owned"]');
    expect(grid2).not.toBeNull();
    // The grid element should persist and remain attached
    expect(grid2).toBe(grid1);
    expect(grid2.isConnected).toBe(true);
    expect(grid2.querySelectorAll(".owned-chip").length).toBe(3);

    // Ensure there is exactly one wrapper for grid
    expect(ownedList.querySelectorAll('li[data-kind="grid"]').length).toBe(1);
  });

  it("keeps the grid node and does not disappear when removing a file", async () => {
    const owned = await loadOwned();

    // Start with three files
    owned.applyResponse({ files: ["one", "two", "three"] });
    owned.renderOwned();

    const ownedList = document.getElementById("ownedList");
    const grid1 = ownedList.querySelector('.owned-grid[data-role="owned"]');
    expect(grid1).not.toBeNull();
    expect(grid1.isConnected).toBe(true);
    expect(grid1.querySelectorAll(".owned-chip").length).toBe(3);

    // Remove one file and re-render
    owned.applyResponse({ files: ["one", "three"] });
    owned.renderOwned();

    const grid2 = ownedList.querySelector('.owned-grid[data-role="owned"]');
    expect(grid2).not.toBeNull();
    // The same grid should still be attached (no flicker/disappearance)
    expect(grid2).toBe(grid1);
    expect(grid2.isConnected).toBe(true);
    expect(grid2.querySelectorAll(".owned-chip").length).toBe(2);

    // Owned panel should still indicate files are present
    const panel = document.getElementById("ownedPanel");
    expect(panel.getAttribute("data-state")).toBe("has-files");

    // Only one grid wrapper should exist
    expect(ownedList.querySelectorAll('li[data-kind="grid"]').length).toBe(1);
  });

  it("remains stable across rapid successive renders", async () => {
    const owned = await loadOwned();

    owned.applyResponse({ files: ["a"] });
    owned.renderOwned();

    const ownedList = document.getElementById("ownedList");
    const gridInitial = ownedList.querySelector('.owned-grid[data-role="owned"]');
    expect(gridInitial).not.toBeNull();

    // Rapid re-renders with varying sets
    owned.applyResponse({ files: ["a", "b"] });
    owned.renderOwned();
    owned.applyResponse({ files: ["a"] });
    owned.renderOwned();
    owned.applyResponse({ files: ["a", "b", "c"] });
    owned.renderOwned();

    const gridFinal = ownedList.querySelector('.owned-grid[data-role="owned"]');
    expect(gridFinal).not.toBeNull();
    expect(gridFinal).toBe(gridInitial);
    expect(gridFinal.isConnected).toBe(true);
    expect(gridFinal.querySelectorAll(".owned-chip").length).toBe(3);
  });
});
