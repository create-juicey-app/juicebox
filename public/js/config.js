// Default values
window.MAX_FILE_BYTES = 500 * 1024 * 1024;
window.MAX_FILE_SIZE_STR = "500MB";
window.ENABLE_STREAMING_UPLOADS = false;

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
    return cfg;
  } catch (err) {
    if (window.DEBUG_LOGS)
      console.warn("Could not fetch dynamic config, using defaults.", err);
    return null;
  }
}
