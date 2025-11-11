import {
  jest,
  describe,
  it,
  expect,
  beforeEach,
  afterEach,
} from "@jest/globals";

function rafImmediate() {
  global.requestAnimationFrame = (cb) => cb();
}

async function loadOwnedModule() {
  jest.resetModules();
  return await import("../public/js/owned.js");
}

describe("owned.js neutral empty state and ttl formatting", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    rafImmediate();
  });

  afterEach(() => {
    document.body.innerHTML = "";
    document.documentElement.lang = "en";
    // eslint-disable-next-line no-undef
    delete window.JBLang;
  });

  it("renders neutral empty state without localized text", async () => {
    document.body.innerHTML = `<ul id="ownedList" role="list"></ul>`;
    const { ownedHandler } = await loadOwnedModule();
    ownedHandler.mountEmptyState({ instant: true });

    const empty = document.querySelector(".owned-empty");
    expect(empty).not.toBeNull();

    const block = empty.querySelector(".owned-empty-block");
    expect(block).not.toBeNull();

    // No legacy strong/span text nodes
    expect(empty.querySelector("strong")).toBeNull();
    expect(empty.querySelector("span")).toBeNull();
  });

  it("returns raw seconds or empty string for expired TTL", async () => {
    document.body.innerHTML = `<ul id="ownedList" role="list"></ul>`;
    const { ownedHandler } = await loadOwnedModule();

    expect(ownedHandler.formatRemaining(-5)).toBe("");
    expect(ownedHandler.formatRemaining(0)).toBe("");
    expect(ownedHandler.formatRemaining(9)).toBe("9");
    expect(ownedHandler.formatRemaining(61)).toBe("61");
  });
});
