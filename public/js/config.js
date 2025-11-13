// Default values
window.MAX_FILE_BYTES = 500 * 1024 * 1024;
window.MAX_FILE_SIZE_STR = "500MB";
window.ENABLE_STREAMING_UPLOADS = false;
window.JB_UPLOADS_DISABLED = false;
window.JB_QUOTA_INFO = null;

function applyQuotaGlobals(quota) {
  if (quota && typeof quota === "object") {
    const normalized = { ...quota };
    if (
      typeof normalized.message === "string" &&
      normalized.message.trim()
    ) {
      normalized.quota_message = normalized.message.trim();
    }
    window.JB_QUOTA_INFO = normalized;
    window.JB_UPLOADS_DISABLED = !!normalized.uploads_blocked;
  } else {
    window.JB_QUOTA_INFO = null;
    window.JB_UPLOADS_DISABLED = false;
  }
}

export async function fetchConfig() {
  try {
    const resp = await fetch("/api/config", { cache: "no-store" });
    if (!resp.ok) {
      if (window.DEBUG_LOGS)
        console.warn("Config fetch returned non-success status", resp.status);
      return null;
    }
    const cfg = await resp.json();
    if (cfg && typeof cfg.max_file_bytes === "number") {
      window.MAX_FILE_BYTES = cfg.max_file_bytes;
    }
    if (cfg && typeof cfg.max_file_size_str === "string") {
      window.MAX_FILE_SIZE_STR = cfg.max_file_size_str;
    }
    if (cfg && typeof cfg.enable_streaming_uploads === "boolean") {
      window.ENABLE_STREAMING_UPLOADS = cfg.enable_streaming_uploads;
    }
    if (cfg && typeof cfg.quota === "object" && cfg.quota !== null) {
      applyQuotaGlobals(cfg.quota);
    } else {
      applyQuotaGlobals(null);
    }
    return cfg;
  } catch (err) {
    if (window.DEBUG_LOGS)
      console.warn("Could not fetch dynamic config, using defaults.", err);
    return null;
  }
}

export async function fetchQuotaStatus() {
  try {
    const resp = await fetch("/api/quota", { cache: "no-store" });
    if (!resp.ok) {
      if (window.DEBUG_LOGS)
        console.warn("Quota fetch returned non-success status", resp.status);
      return null;
    }
    const data = await resp.json();
    if (data && typeof data.quota === "object" && data.quota !== null) {
      applyQuotaGlobals(data.quota);
      return data.quota;
    }
    applyQuotaGlobals(null);
    return null;
  } catch (err) {
    if (window.DEBUG_LOGS)
      console.warn("Could not fetch quota status, proceeding.", err);
    return null;
  }
}

export { applyQuotaGlobals };
