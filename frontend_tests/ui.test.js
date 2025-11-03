import {
  jest,
  describe,
  it,
  expect,
  beforeEach,
  afterEach,
} from "@jest/globals";

function rafImmediate() {
  // Force rAF to execute synchronously in tests
  global.requestAnimationFrame = (cb) => cb();
}

async function loadUiModule() {
  jest.resetModules();
  return await import("../public/js/ui.js");
}

describe("ui.getTTL", () => {
  beforeEach(() => {
    document.body.innerHTML = "";
    window.localStorage.clear();
    jest.useFakeTimers();
    rafImmediate();
  });

  afterEach(() => {
    jest.useRealTimers();
    document.body.innerHTML = "";
  });

  it("returns default '3d' when ttlSelect is missing", async () => {
    // No ttlSelect in DOM
    const { getTTL } = await loadUiModule();
    expect(getTTL()).toBe("3d");
  });

  it("maps input[type=range] value to ttl code", async () => {
    document.body.innerHTML = `
      <div id="dropZone"></div>
      <input id="ttlSelect" type="range" value="2" />
      <span id="ttlValue"></span>
    `;
    const { getTTL } = await loadUiModule();
    expect(getTTL()).toBe("12h"); // index 2 in the ttlMap
  });

  it("returns select value when ttlSelect is a <select>", async () => {
    document.body.innerHTML = `
      <div id="dropZone"></div>
      <select id="ttlSelect">
        <option value="1h">1h</option>
        <option value="3h">3h</option>
        <option value="12h">12h</option>
        <option value="1d">1d</option>
        <option value="3d">3d</option>
        <option value="7d" selected>7d</option>
        <option value="14d">14d</option>
      </select>
    `;
    const { getTTL } = await loadUiModule();
    expect(getTTL()).toBe("7d");
  });
});

describe("ui.setupTTL", () => {
  beforeEach(() => {
    document.body.innerHTML = `
      <div id="dropZone"></div>
      <input id="ttlSelect" type="range" value="0" />
      <span id="ttlValue"></span>
    `;
    window.localStorage.clear();
    jest.useFakeTimers();
    rafImmediate();
  });

  afterEach(() => {
    jest.useRealTimers();
    document.body.innerHTML = "";
    window.localStorage.clear();
  });

  it("hydrates from localStorage and updates label; persists on change", async () => {
    window.localStorage.setItem("ttlChoice", "12h");
    const { setupTTL } = await loadUiModule();

    const ttlSelect = document.getElementById("ttlSelect");
    const ttlValueLabel = document.getElementById("ttlValue");

    // Initialize
    setupTTL();

    // Should map saved "12h" to index 2 on the range input
    expect(ttlSelect.value).toBe("2");
    expect(ttlValueLabel.textContent).toBe("12h");
    expect(window.localStorage.getItem("ttlChoice")).toBe("12h");

    // Change slider to index 5 ("7d")
    ttlSelect.value = "5";
    ttlSelect.dispatchEvent(new Event("input", { bubbles: true }));

    expect(ttlValueLabel.textContent).toBe("7d");
    expect(window.localStorage.getItem("ttlChoice")).toBe("7d");

    // Change event should also persist and update
    ttlSelect.value = "3"; // "1d"
    ttlSelect.dispatchEvent(new Event("change", { bubbles: true }));
    expect(ttlValueLabel.textContent).toBe("1d");
    expect(window.localStorage.getItem("ttlChoice")).toBe("1d");
  });
});

describe("ui.setupUI ripple and panel behavior", () => {
  beforeEach(() => {
    // Provide minimal DOM: dropZone and ownedPanel for animations
    document.body.innerHTML = `
      <div id="dropZone"><span class="icon"></span></div>
      <div id="ownedPanel"></div>
    `;
    jest.useFakeTimers();
    rafImmediate();
  });

  afterEach(() => {
    jest.useRealTimers();
    document.body.innerHTML = "";
  });

  it("adds reveal classes to ownedPanel and 'animate' to dropZone after delay", async () => {
    const { setupUI } = await loadUiModule();

    const ownedPanel = document.getElementById("ownedPanel");
    const dropZone = document.getElementById("dropZone");

    expect(ownedPanel.classList.contains("reveal-start")).toBe(false);
    expect(ownedPanel.classList.contains("reveal")).toBe(false);

    setupUI();

    // reveal-start is added synchronously; reveal added on next rAF (timers flush)
    expect(ownedPanel.classList.contains("reveal-start")).toBe(true);
    jest.advanceTimersByTime(0); // flush rAF polyfill
    expect(ownedPanel.classList.contains("reveal")).toBe(true);

    // "animate" is added after 500ms
    expect(dropZone.classList.contains("animate")).toBe(false);
    jest.advanceTimersByTime(499);
    expect(dropZone.classList.contains("animate")).toBe(false);
    jest.advanceTimersByTime(1);
    expect(dropZone.classList.contains("animate")).toBe(true);
  });

  it("creates ripple on click and triggers hidden file input click; ripple is removed on animationend", async () => {
    const { setupUI, fileInput } = await loadUiModule();

    const dropZone = document.getElementById("dropZone");
    // Ensure we have a realistic bounding box for ripple math
    dropZone.getBoundingClientRect = () => ({
      left: 10,
      top: 20,
      width: 200,
      height: 100,
      right: 210,
      bottom: 120,
      x: 10,
      y: 20,
      toJSON: () => {},
    });

    // Spy on hidden file input click
    const clickSpy = jest
      .spyOn(fileInput, "click")
      .mockImplementation(() => {});

    setupUI();

    // Trigger click roughly in the center-left area
    dropZone.dispatchEvent(
      new MouseEvent("click", {
        bubbles: true,
        cancelable: true,
        clientX: 50,
        clientY: 60,
      }),
    );

    // Ripple should have been injected
    const ripple = dropZone.querySelector(".ripple");
    expect(ripple).not.toBeNull();
    // Ripple size equals max(rect.width, rect.height) = 200
    expect(ripple.style.width).toBe("200px");
    expect(ripple.style.height).toBe("200px");
    // Position computed from event and rect: left = 50 - 10 - 100 = -60; top = 60 - 20 - 100 = -60
    expect(ripple.style.left).toBe("-60px");
    expect(ripple.style.top).toBe("-60px");

    // Hidden input receives click
    expect(clickSpy).toHaveBeenCalledTimes(1);

    // Ripple removed after animationend
    ripple.dispatchEvent(new Event("animationend", { bubbles: true }));
    expect(dropZone.querySelector(".ripple")).toBeNull();
  });
});
