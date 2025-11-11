import {
  jest,
  describe,
  it,
  expect,
  beforeEach,
  afterEach,
} from "@jest/globals";

import { setupBfcacheSupport, copyToClipboard } from "../public/js/utils.js";

describe("utils.setupBfcacheSupport", () => {
  let warnSpy;

  beforeEach(() => {
    jest.useFakeTimers();
    warnSpy = jest.spyOn(console, "warn").mockImplementation(() => {});
    document.body.innerHTML = "";
  });

  afterEach(() => {
    jest.useRealTimers();
    warnSpy.mockRestore();
    delete window.DEBUG_LOGS;
    document.body.innerHTML = "";
  });

  it("invokes onSaveState on pagehide and onRestoreState on pageshow with persisted=true", () => {
    const onSaveState = jest.fn();
    const onRestoreState = jest.fn();

    setupBfcacheSupport({ onSaveState, onRestoreState });

    // pagehide should trigger onSaveState
    const ph = new Event("pagehide");
    window.dispatchEvent(ph);
    expect(onSaveState).toHaveBeenCalledTimes(1);
    expect(onSaveState).toHaveBeenCalledWith(ph);

    // pageshow with persisted=true should trigger onRestoreState
    const psPersisted = new Event("pageshow");
    Object.defineProperty(psPersisted, "persisted", { value: true });
    window.dispatchEvent(psPersisted);
    expect(onRestoreState).toHaveBeenCalledTimes(1);
    expect(onRestoreState).toHaveBeenCalledWith(psPersisted);

    // pageshow with persisted=false should not call onRestoreState again
    const psNotPersisted = new Event("pageshow");
    Object.defineProperty(psNotPersisted, "persisted", { value: false });
    window.dispatchEvent(psNotPersisted);
    expect(onRestoreState).toHaveBeenCalledTimes(1);
  });

  it("logs a warning on unload when DEBUG_LOGS is enabled", () => {
    window.DEBUG_LOGS = true;

    setupBfcacheSupport();

    const ev = new Event("unload");
    window.dispatchEvent(ev);

    // At least one warning should be emitted
    expect(warnSpy).toHaveBeenCalled();
    const messages = warnSpy.mock.calls.map((c) => String(c[0]));
    expect(messages.some((m) => m.includes("Avoid using unload event"))).toBe(
      true,
    );
  });

  it("does not throw when callbacks are not provided", () => {
    expect(() => setupBfcacheSupport()).not.toThrow();

    // Dispatch events to ensure no errors occur without callbacks
    window.dispatchEvent(new Event("pagehide"));
    const ps = new Event("pageshow");
    Object.defineProperty(ps, "persisted", { value: true });
    window.dispatchEvent(ps);
    window.dispatchEvent(new Event("unload"));
  });
});

describe("utils.copyToClipboard fallback", () => {
  let originalClipboard;
  let execSpy;

  beforeEach(() => {
    document.body.innerHTML = "";
    originalClipboard = navigator.clipboard;
    execSpy = undefined;
  });

  afterEach(() => {
    // Only restore if mockRestore exists (jest.fn doesn't have it)
    if (execSpy && typeof execSpy.mockRestore === "function")
      execSpy.mockRestore();
    if (originalClipboard === undefined) {
      try {
        delete navigator.clipboard;
      } catch {
        // ignore
      }
    } else {
      Object.defineProperty(navigator, "clipboard", {
        value: originalClipboard,
        configurable: true,
      });
    }
    document.body.innerHTML = "";
  });

  it("uses navigator.clipboard.writeText when available", async () => {
    const writeText = jest.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });

    // Provide execCommand stub so we can assert it was NOT used
    document.execCommand = jest.fn();
    execSpy = document.execCommand;

    await copyToClipboard("hello world");

    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenCalledWith("hello world");
    expect(document.execCommand).not.toHaveBeenCalled();

    // No leftover textareas
    expect(document.querySelectorAll("textarea").length).toBe(0);
  });

  it("falls back to execCommand when clipboard.writeText rejects", async () => {
    const writeText = jest.fn().mockRejectedValue(new Error("no permission"));
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });

    document.execCommand = jest.fn().mockImplementation(() => true);
    execSpy = document.execCommand;

    const appendSpy = jest.spyOn(document.body, "appendChild");
    const removeSpy = jest.spyOn(document.body, "removeChild");

    await copyToClipboard("fallback text");

    expect(writeText).toHaveBeenCalledTimes(1);
    expect(document.execCommand).toHaveBeenCalledTimes(1);
    expect(document.execCommand).toHaveBeenCalledWith("copy");

    // A textarea should be appended then removed
    expect(appendSpy).toHaveBeenCalled();
    expect(removeSpy).toHaveBeenCalled();

    const appendedEl = appendSpy.mock.calls[0][0];
    const removedEl = removeSpy.mock.calls[0][0];
    expect(appendedEl).toBeInstanceOf(HTMLElement);
    expect(removedEl).toBe(appendedEl);
    expect(appendedEl.tagName).toBe("TEXTAREA");

    // No leftover textareas
    expect(document.querySelectorAll("textarea").length).toBe(0);

    appendSpy.mockRestore();
    removeSpy.mockRestore();
  });

  it("falls back to execCommand when clipboard is not available", async () => {
    // Remove clipboard entirely
    try {
      delete navigator.clipboard;
    } catch {
      Object.defineProperty(navigator, "clipboard", {
        value: undefined,
        configurable: true,
      });
    }

    document.execCommand = jest.fn().mockImplementation(() => true);
    execSpy = document.execCommand;

    const appendSpy = jest.spyOn(document.body, "appendChild");
    const removeSpy = jest.spyOn(document.body, "removeChild");

    await copyToClipboard("no api");

    expect(document.execCommand).toHaveBeenCalledWith("copy");
    expect(appendSpy).toHaveBeenCalled();
    expect(removeSpy).toHaveBeenCalled();

    // Ensure no residual elements
    expect(document.querySelectorAll("textarea").length).toBe(0);

    appendSpy.mockRestore();
    removeSpy.mockRestore();
  });

  it("still removes textarea if execCommand throws", async () => {
    try {
      delete navigator.clipboard;
    } catch {
      Object.defineProperty(navigator, "clipboard", {
        value: undefined,
        configurable: true,
      });
    }

    document.execCommand = jest.fn().mockImplementation(() => {
      throw new Error("execCommand not allowed");
    });
    execSpy = document.execCommand;

    const appendSpy = jest.spyOn(document.body, "appendChild");
    const removeSpy = jest.spyOn(document.body, "removeChild");

    await expect(copyToClipboard("err")).resolves.toBeUndefined();

    // Textarea should still be removed even if execCommand throws
    expect(appendSpy).toHaveBeenCalled();
    expect(removeSpy).toHaveBeenCalled();

    // No leftover textareas
    expect(document.querySelectorAll("textarea").length).toBe(0);

    appendSpy.mockRestore();
    removeSpy.mockRestore();
  });
});
