import {
  jest,
  describe,
  it,
  expect,
  beforeEach,
  afterEach,
} from "@jest/globals";
import { OWNED_STRINGS } from "../public/js/i18n-owned.js";

function rafImmediate() {
  // Force rAF to execute synchronously to avoid animation timing in tests
  global.requestAnimationFrame = (cb) => cb();
}

async function loadOwnedModule() {
  jest.resetModules();
  return await import("../public/js/owned.js");
}

describe("owned.js i18n for empty state and expired label", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    rafImmediate();
  });

  afterEach(() => {
    document.body.innerHTML = "";
    // Reset lang and JBLang stub
    document.documentElement.lang = "en";
    // eslint-disable-next-line no-undef
    delete window.JBLang;
  });

  const cases = Object.entries(OWNED_STRINGS).map(([lang, tbl]) => ({
    lang,
    title: tbl.empty_title,
    hint: tbl.empty_hint,
    expired: tbl.expired,
  }));

  it.each(cases)(
    "localizes empty state and expired label for %s",
    async ({ lang, title, hint, expired }) => {
      // Stub current language
      // eslint-disable-next-line no-undef
      window.JBLang = { current: () => lang };

      // Prepare DOM before importing module (ui.js reads on import)
      document.body.innerHTML = `<ul id="ownedList" role="list"></ul>`;

      const { ownedHandler } = await loadOwnedModule();

      // Render empty state with instant to bypass animation
      ownedHandler.mountEmptyState({ instant: true });

      const strong = document.querySelector(".owned-empty strong");
      const span = document.querySelector(".owned-empty span");

      expect(strong).not.toBeNull();
      expect(span).not.toBeNull();

      expect(strong.textContent).toBe(title);
      expect(span.textContent).toBe(hint);

      // Verify expired label translation
      expect(ownedHandler.formatRemaining(-1)).toBe(expired);
      expect(ownedHandler.formatRemaining(0)).toBe(expired);
    },
  );

  it("falls back to document.documentElement.lang when JBLang is absent", async () => {
    // Ensure no JBLang
    // eslint-disable-next-line no-undef
    delete window.JBLang;

    // Fallback via document lang
    document.documentElement.lang = "fr";

    // Prepare DOM before importing module (ui.js reads on import)
    document.body.innerHTML = `<ul id="ownedList" role="list"></ul>`;

    const { ownedHandler } = await loadOwnedModule();

    ownedHandler.mountEmptyState({ instant: true });

    const strong = document.querySelector(".owned-empty strong");
    const span = document.querySelector(".owned-empty span");

    expect(strong).not.toBeNull();
    expect(span).not.toBeNull();

    const expected = OWNED_STRINGS.fr;
    expect(strong.textContent).toBe(expected.empty_title);
    expect(span.textContent).toBe(expected.empty_hint);
    expect(ownedHandler.formatRemaining(-5)).toBe(expected.expired);
  });
});
