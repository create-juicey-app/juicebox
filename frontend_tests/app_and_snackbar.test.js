/**
 * Tests covering:
 * - utils.flashCopied behavior (DOM creation, class toggling, timeout handling)
 * - utils.showSnack behavior (DOM creation, class toggling, shake animation, aria live)
 * - app.initializeApp orchestration and idempotence with module mocks
 */

import { flashCopied, showSnack } from "../public/js/utils.js";

describe("utils.flashCopied", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    jest.useFakeTimers();
  });

  afterEach(() => {
    jest.useRealTimers();
    document.body.innerHTML = "";
  });

  it("creates snackbar if missing, shows message, and hides after timeout", () => {
    expect(document.getElementById("snackbar")).toBeNull();

    flashCopied("Copied!");
    const sb = document.getElementById("snackbar");
    expect(sb).not.toBeNull();
    expect(sb.textContent).toBe("Copied!");
    expect(sb.classList.contains("show")).toBe(true);
    expect(sb.classList.contains("error")).toBe(false);

    // Before timeout expiry, it should still be visible
    jest.advanceTimersByTime(1599);
    expect(sb.classList.contains("show")).toBe(true);

    // After timeout expiry, it should hide
    jest.advanceTimersByTime(2);
    expect(sb.classList.contains("show")).toBe(false);
  });

  it("reuses existing snackbar and resets timeout", () => {
    document.body.innerHTML = '<div id="snackbar" class="show">Old</div>';
    const sb = document.getElementById("snackbar");
    flashCopied("Again");
    expect(sb.textContent).toBe("Again");
    expect(sb.classList.contains("show")).toBe(true);

    // Not yet expired
    jest.advanceTimersByTime(1000);
    expect(sb.classList.contains("show")).toBe(true);

    // Calling again should reset the timer
    flashCopied("Third");
    expect(sb.textContent).toBe("Third");
    jest.advanceTimersByTime(1599);
    expect(sb.classList.contains("show")).toBe(true);

    jest.advanceTimersByTime(2);
    expect(sb.classList.contains("show")).toBe(false);
  });

  it("removes error class if present", () => {
    document.body.innerHTML = '<div id="snackbar" class="error">X</div>';
    const sb = document.getElementById("snackbar");
    expect(sb.classList.contains("error")).toBe(true);

    flashCopied("Fixed");
    expect(sb.classList.contains("error")).toBe(false);
    expect(sb.classList.contains("show")).toBe(true);
  });
});

describe("utils.showSnack", () => {
  beforeEach(() => {
    document.body.innerHTML = `
      <div id="dropZone"></div>
    `;
    window._announce = jest.fn();
    jest.useFakeTimers();
  });

  afterEach(() => {
    jest.useRealTimers();
    document.body.innerHTML = "";
    delete window._announce;
  });

  it("creates snackbar if missing, shows error, announces, and auto-hides", () => {
    showSnack("Oops, error happened!", { some: "opt" });

    const sb = document.getElementById("snackbar");
    expect(sb).not.toBeNull();
    expect(sb.textContent).toBe("Oops, error happened!");
    expect(sb.classList.contains("show")).toBe(true);
    expect(sb.classList.contains("error")).toBe(true);

    // Announcement called
    expect(window._announce).toHaveBeenCalledWith("Oops, error happened!");

    // Not yet expired
    jest.advanceTimersByTime(4999);
    expect(sb.classList.contains("show")).toBe(true);
    expect(sb.classList.contains("error")).toBe(true);

    // After timeout
    jest.advanceTimersByTime(2);
    expect(sb.classList.contains("show")).toBe(false);
    expect(sb.classList.contains("error")).toBe(false);
  });

  it("adds shake to dropZone and removes it after animationend", () => {
    const dz = document.getElementById("dropZone");
    expect(dz).not.toBeNull();

    showSnack("Shake it");
    expect(dz.classList.contains("shake")).toBe(true);

    // Simulate CSS animation end
    dz.dispatchEvent(new Event("animationend"));
    expect(dz.classList.contains("shake")).toBe(false);
  });

  it("resets timer on repeated calls and updates text", () => {
    showSnack("First");
    const sb = document.getElementById("snackbar");
    expect(sb.textContent).toBe("First");

    // Advance close to the end but not past
    jest.advanceTimersByTime(4800);
    expect(sb.classList.contains("show")).toBe(true);

    // Call again to reset timer and change text
    showSnack("Second");
    expect(sb.textContent).toBe("Second");

    // Still within new timer window
    jest.advanceTimersByTime(4900);
    expect(sb.classList.contains("show")).toBe(true);

    // After expiry
    jest.advanceTimersByTime(200);
    expect(sb.classList.contains("show")).toBe(false);
    expect(sb.classList.contains("error")).toBe(false);
  });
});

describe("app.initializeApp orchestration", () => {
  // We will mock internal modules used by app.js
  beforeEach(() => {
    jest.resetModules();
    jest.useFakeTimers();

    // Ensure no auto-boot runs when importing app.js
    // by forcing readyState to 'loading' so boot is deferred to DOMContentLoaded.
    Object.defineProperty(document, "readyState", {
      value: "loading",
      configurable: true,
    });

    // Minimal DOM needed by UI features if they are touched
    document.body.innerHTML = `
      <div id="dropZone"></div>
      <div id="ownedPanel"></div>
    `;

    // Provide noop JBLang hooks so app can call them
    window.JBLang = {
      rewriteLinks: jest.fn(),
      enableAutoRewrite: jest.fn(),
    };
  });

  afterEach(() => {
    jest.useRealTimers();
    document.body.innerHTML = "";
    delete window.JBLang;
  });

  it("initializes once and wires modules together", async () => {
    const mockFetchConfig = jest.fn().mockResolvedValue({
      telemetry: { sentry: { enabled: true, dsn: "dsn://test" } },
    });
    const mockInitTelemetry = jest.fn();
    const mockSetupTTL = jest.fn();
    const mockSetupUI = jest.fn();
    const mockOwnedHandler = { loadExisting: jest.fn().mockResolvedValue() };
    const mockUploadHandler = {};
    const mockSetupEventListeners = jest.fn();
    const mockApplyOther = jest.fn();

    jest.mock("../public/js/config.js", () => ({
      fetchConfig: mockFetchConfig,
    }));
    jest.mock("../public/js/telemetry.js", () => ({
      initTelemetry: mockInitTelemetry,
      captureException: jest.fn(),
    }));
    jest.mock("../public/js/ui.js", () => ({
      setupTTL: mockSetupTTL,
      setupUI: mockSetupUI,
    }));
    jest.mock("../public/js/upload.js", () => ({
      uploadHandler: mockUploadHandler,
    }));
    jest.mock("../public/js/owned.js", () => ({
      ownedHandler: mockOwnedHandler,
    }));
    jest.mock("../public/js/events.js", () => ({
      setupEventListeners: mockSetupEventListeners,
    }));
    jest.mock("../public/js/other.js", () => ({
      applyother: mockApplyOther,
    }));

    const { initializeApp } = await import("../public/js/app.js");

    const p1 = initializeApp();
    await p1;

    // Calls and wiring
    expect(mockFetchConfig).toHaveBeenCalledTimes(1);
    expect(mockInitTelemetry).toHaveBeenCalledTimes(1);
    expect(mockApplyOther).toHaveBeenCalledWith(
      mockUploadHandler,
      mockOwnedHandler,
    );
    expect(mockSetupTTL).toHaveBeenCalledTimes(1);
    expect(mockSetupUI).toHaveBeenCalledTimes(1);
    expect(mockOwnedHandler.loadExisting).toHaveBeenCalledTimes(1);
    expect(mockSetupEventListeners).toHaveBeenCalledTimes(1);

    // Lang helpers called if exposed on window
    expect(window.JBLang.rewriteLinks).toHaveBeenCalledTimes(1);
    expect(window.JBLang.rewriteLinks).toHaveBeenCalledWith(document);
    expect(window.JBLang.enableAutoRewrite).toHaveBeenCalledTimes(1);

    // Idempotence: second call returns same promise and does not re-run steps
    const p2 = initializeApp();
    await p2;
    // Ensure second call didn't re-run any steps
    expect(mockFetchConfig).toHaveBeenCalledTimes(1);
    expect(mockInitTelemetry).toHaveBeenCalledTimes(1);
    expect(mockApplyOther).toHaveBeenCalledTimes(1);
    expect(mockSetupTTL).toHaveBeenCalledTimes(1);
    expect(mockSetupUI).toHaveBeenCalledTimes(1);
    expect(mockOwnedHandler.loadExisting).toHaveBeenCalledTimes(1);
    expect(mockSetupEventListeners).toHaveBeenCalledTimes(1);
  });

  it("handles absence of JBLang gracefully", async () => {
    // No JBLang provided
    delete window.JBLang;

    const mockFetchConfig = jest.fn().mockResolvedValue(null);
    const mockInitTelemetry = jest.fn();
    const mockSetupTTL = jest.fn();
    const mockSetupUI = jest.fn();
    const mockOwnedHandler = { loadExisting: jest.fn().mockResolvedValue() };
    const mockUploadHandler = {};
    const mockSetupEventListeners = jest.fn();
    const mockApplyOther = jest.fn();

    jest.mock("../public/js/config.js", () => ({
      fetchConfig: mockFetchConfig,
    }));
    jest.mock("../public/js/telemetry.js", () => ({
      initTelemetry: mockInitTelemetry,
      captureException: jest.fn(),
    }));
    jest.mock("../public/js/ui.js", () => ({
      setupTTL: mockSetupTTL,
      setupUI: mockSetupUI,
    }));
    jest.mock("../public/js/upload.js", () => ({
      uploadHandler: mockUploadHandler,
    }));
    jest.mock("../public/js/owned.js", () => ({
      ownedHandler: mockOwnedHandler,
    }));
    jest.mock("../public/js/events.js", () => ({
      setupEventListeners: mockSetupEventListeners,
    }));
    jest.mock("../public/js/other.js", () => ({
      applyother: mockApplyOther,
    }));

    const { initializeApp } = await import("../public/js/app.js");
    await initializeApp();

    expect(mockFetchConfig).toHaveBeenCalledTimes(1);
    expect(mockInitTelemetry).toHaveBeenCalledTimes(1);
    expect(mockApplyOther).toHaveBeenCalledTimes(1);
    expect(mockSetupTTL).toHaveBeenCalledTimes(1);
    expect(mockSetupUI).toHaveBeenCalledTimes(1);
    expect(mockOwnedHandler.loadExisting).toHaveBeenCalledTimes(1);
    expect(mockSetupEventListeners).toHaveBeenCalledTimes(1);
  });
});
