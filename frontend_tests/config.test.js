/**
 * Tests for public/js/config.js behavior:
 * - Sets defaults on import
 * - Applies valid server-provided values
 * - Ignores invalid types and preserves existing values
 * - Handles non-OK responses and network failures gracefully
 */

describe("config.js fetchConfig", () => {
  const DEFAULT_BYTES = 500 * 1024 * 1024;
  const DEFAULT_SIZE_STR = "500MB";
  const DEFAULT_STREAMING = false;

  async function loadConfigModule() {
    jest.resetModules();
    // Ensure a clean slate so module default assignments run
    delete window.MAX_FILE_BYTES;
    delete window.MAX_FILE_SIZE_STR;
    delete window.ENABLE_STREAMING_UPLOADS;
    return await import("../public/js/config.js");
  }

  function mockFetchOnce(result) {
    global.fetch = jest.fn().mockResolvedValue(result);
  }

  function mockFetchOk(jsonValue) {
    mockFetchOnce({
      ok: true,
      status: 200,
      json: async () => jsonValue,
    });
  }

  function mockFetchNotOk(status = 500, jsonValue = { message: "err" }) {
    mockFetchOnce({
      ok: false,
      status,
      json: async () => jsonValue,
    });
  }

  beforeEach(() => {
    jest.resetModules();
    global.fetch = jest.fn();
  });

  afterEach(() => {
    jest.restoreAllMocks();
    delete global.fetch;
  });

  it("initializes default globals on import", async () => {
    await loadConfigModule();
    expect(window.MAX_FILE_BYTES).toBe(DEFAULT_BYTES);
    expect(window.MAX_FILE_SIZE_STR).toBe(DEFAULT_SIZE_STR);
    expect(window.ENABLE_STREAMING_UPLOADS).toBe(DEFAULT_STREAMING);
    expect(window.JB_UPLOADS_DISABLED).toBe(false);
    expect(window.JB_QUOTA_INFO).toBeNull();
  });

  it("updates globals from a successful server config and returns the cfg", async () => {
    const { fetchConfig } = await loadConfigModule();

    const cfg = {
      max_file_bytes: 123456789,
      max_file_size_str: "117.74 MB",
      enable_streaming_uploads: true,
      sentry: { enabled: false }, // unrelated but should be preserved in return value
      quota: {
        max_bytes: 50,
        used_bytes: 50,
        remaining_bytes: 0,
        uploads_blocked: true,
        max_bytes_str: "50B",
        used_bytes_str: "50B",
        remaining_bytes_str: "0B",
      },
    };
    mockFetchOk(cfg);

    const returned = await fetchConfig();
    // fetch called with correct endpoint and options
    expect(global.fetch).toHaveBeenCalledWith("/api/config", {
      cache: "no-store",
    });

    // Returns parsed config
    expect(returned).toEqual(cfg);

    // Updates globals
    expect(window.MAX_FILE_BYTES).toBe(123456789);
    expect(window.MAX_FILE_SIZE_STR).toBe("117.74 MB");
    expect(window.ENABLE_STREAMING_UPLOADS).toBe(true);
    expect(window.JB_UPLOADS_DISABLED).toBe(true);
    expect(window.JB_QUOTA_INFO).toEqual(cfg.quota);
  });

  it("ignores invalid types and preserves existing values", async () => {
    const { fetchConfig } = await loadConfigModule();

    // Set some non-defaults prior to fetch
    window.MAX_FILE_BYTES = 42;
    window.MAX_FILE_SIZE_STR = "42B";
    window.ENABLE_STREAMING_UPLOADS = true;
    window.JB_UPLOADS_DISABLED = true;
    window.JB_QUOTA_INFO = { uploads_blocked: true };

    // Provide mixed-type payload (only one valid field)
    mockFetchOk({
      max_file_bytes: "oops", // invalid
      max_file_size_str: "1TB", // valid
      enable_streaming_uploads: "nope", // invalid
      quota: null,
    });

    const returned = await fetchConfig();
    expect(returned).toEqual({
      max_file_bytes: "oops",
      max_file_size_str: "1TB",
      enable_streaming_uploads: "nope",
      quota: null,
    });

    // Only the valid field should be applied
    expect(window.MAX_FILE_BYTES).toBe(42); // unchanged
    expect(window.MAX_FILE_SIZE_STR).toBe("1TB"); // updated
    expect(window.ENABLE_STREAMING_UPLOADS).toBe(true); // unchanged
    expect(window.JB_UPLOADS_DISABLED).toBe(false);
    expect(window.JB_QUOTA_INFO).toBeNull();
  });

  it("returns null and does not change globals when response is not ok", async () => {
    const mod = await loadConfigModule();
    const { fetchConfig } = mod;

    // Set sentinel values
    window.MAX_FILE_BYTES = 777;
    window.MAX_FILE_SIZE_STR = "777B";
    window.ENABLE_STREAMING_UPLOADS = true;
    window.JB_UPLOADS_DISABLED = true;
    window.JB_QUOTA_INFO = { uploads_blocked: true };

    mockFetchNotOk(503, { message: "Service Unavailable" });

    const result = await fetchConfig();
    expect(result).toBeNull();

    // Verify globals preserved
    expect(window.MAX_FILE_BYTES).toBe(777);
    expect(window.MAX_FILE_SIZE_STR).toBe("777B");
    expect(window.ENABLE_STREAMING_UPLOADS).toBe(true);
    expect(window.JB_UPLOADS_DISABLED).toBe(true);
    expect(window.JB_QUOTA_INFO).toEqual({ uploads_blocked: true });
  });

  it("returns null and preserves globals on network error", async () => {
    const { fetchConfig } = await loadConfigModule();

    // Sentinel values
    window.MAX_FILE_BYTES = 888;
    window.MAX_FILE_SIZE_STR = "888B";
    window.ENABLE_STREAMING_UPLOADS = false;
    window.JB_UPLOADS_DISABLED = false;
    window.JB_QUOTA_INFO = { uploads_blocked: false };

    global.fetch = jest.fn().mockRejectedValue(new Error("network down"));

    const result = await fetchConfig();
    expect(result).toBeNull();

    expect(window.MAX_FILE_BYTES).toBe(888);
    expect(window.MAX_FILE_SIZE_STR).toBe("888B");
    expect(window.ENABLE_STREAMING_UPLOADS).toBe(false);
    expect(window.JB_UPLOADS_DISABLED).toBe(false);
    expect(window.JB_QUOTA_INFO).toEqual({ uploads_blocked: false });
  });
});
