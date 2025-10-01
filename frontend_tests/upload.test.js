import { shouldUseChunk, selectChunkSize } from "../public/js/upload.js";

describe("shouldUseChunk", () => {
  afterEach(() => {
    delete window.CHUNK_THRESHOLD_BYTES;
    delete window.MAX_FILE_BYTES;
  });

  it("respects the default threshold", () => {
    const smallFile = { size: 10 * 1024 * 1024 }; // 10 MiB
    expect(shouldUseChunk(smallFile)).toBe(false);

    const largeFile = { size: 200 * 1024 * 1024 }; // 200 MiB
    expect(shouldUseChunk(largeFile)).toBe(true);
  });

  it("forces chunking when max file bytes is exceeded", () => {
    window.MAX_FILE_BYTES = 50 * 1024 * 1024;
    const file = { size: 60 * 1024 * 1024 };
    expect(shouldUseChunk(file)).toBe(true);
  });

  it("uses a custom threshold override when provided", () => {
    window.CHUNK_THRESHOLD_BYTES = 32 * 1024 * 1024;
    const file = { size: 40 * 1024 * 1024 };
    expect(shouldUseChunk(file)).toBe(true);
  });
});

describe("selectChunkSize", () => {
  afterEach(() => {
    delete window.PREFERRED_CHUNK_SIZE_BYTES;
  });

  it("clamps the preferred chunk size within allowed bounds", () => {
    window.PREFERRED_CHUNK_SIZE_BYTES = 2 * 1024; // lower than minimum
    const size = selectChunkSize(10 * 1024 * 1024);
    expect(size).toBeGreaterThanOrEqual(64 * 1024);
  });

  it("ensures total chunks does not exceed limit", () => {
    const hugeFile = { size: 1024 * 1024 * 1024 * 4 }; // 4 GiB
    const chunk = selectChunkSize(hugeFile.size);
    const totalChunks = Math.ceil(hugeFile.size / chunk);
    expect(totalChunks).toBeLessThanOrEqual(20000);
  });
});
