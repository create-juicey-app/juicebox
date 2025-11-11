import { fetchConfig } from "./config.js";
import { initTelemetry, captureException } from "./telemetry.js";
import { setupTTL, setupUI } from "./ui.js";
import { uploadHandler } from "./upload.js";
import { ownedHandler } from "./owned.js";
import { setupEventListeners } from "./events.js";
import { applyother } from "./other.js";
import { showSnack } from "./utils.js";

let initPromise = null;

export async function initializeApp() {
  if (!initPromise) {
    initPromise = (async () => {
      // Show "Your Files" skeleton immediately before any awaits
      try {
        ownedHandler.setLoading(true);
      } catch {}
      const config = await fetchConfig();
      try {
        initTelemetry(config?.telemetry);
      } catch (telemetryErr) {
        if (window.DEBUG_LOGS)
          console.warn("[app] Telemetry initialization failed", telemetryErr);
      }
      applyother(uploadHandler, ownedHandler);
      setupTTL();
      setupUI();
      await ownedHandler.loadExisting();
      if (window.JBLang) {
        if (typeof window.JBLang.rewriteLinks === "function") {
          window.JBLang.rewriteLinks(document);
        }
        if (typeof window.JBLang.enableAutoRewrite === "function") {
          window.JBLang.enableAutoRewrite();
        }
      }
      setupEventListeners();
    })().catch((err) => {
      initPromise = null;
      captureException(err, { phase: "initializeApp" });
      if (window.DEBUG_LOGS) console.error("[app] Failed to initialize", err);
      try {
        showSnack("We had trouble starting up. Please refresh and try again.");
      } catch {}
      throw err;
    });
  }
  return initPromise;
}

function boot() {
  initializeApp().catch(() => {});
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot, { once: true });
} else {
  boot();
}
