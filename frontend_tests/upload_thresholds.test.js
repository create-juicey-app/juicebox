import { shouldUseChunk, selectChunkSize } from "../public/js/upload.js";

const MB = 1024 * 1024;
const GB = MB * 1024;
const TB = GB * 1024;
const DEFAULT_THRESHOLD = 128 * MB;

beforeEach(() => {
  delete window.CHUNK_THRESHOLD_BYTES;
  delete window.MAX_FILE_BYTES;
  delete window.PREFERRED_CHUNK_SIZE_BYTES;
});

describe("shouldUseChunk matrix", () => {
  const CASES = [
    {
      label: "null file returns false",
      size: null,
      override: undefined,
      max: undefined,
      expected: false,
    },
    {
      label: "below default threshold stays single",
      size: DEFAULT_THRESHOLD - MB,
      expected: false,
    },
    {
      label: "exactly default threshold chunks",
      size: DEFAULT_THRESHOLD,
      expected: true,
    },
    {
      label: "above default threshold chunks",
      size: DEFAULT_THRESHOLD + MB,
      expected: true,
    },
    {
      label: "custom override higher than file",
      size: 70 * MB,
      override: 100 * MB,
      expected: false,
    },
    {
      label: "custom override triggers chunking",
      size: 90 * MB,
      override: 64 * MB,
      expected: true,
    },
    {
      label: "custom override still skips when below",
      size: 50 * MB,
      override: 64 * MB,
      expected: false,
    },
    {
      label: "zero override ignored",
      size: DEFAULT_THRESHOLD + 1,
      override: 0,
      expected: true,
    },
    {
      label: "negative override ignored",
      size: DEFAULT_THRESHOLD + 1,
      override: -1,
      expected: true,
    },
    {
      label: "max file limit forces chunk",
      size: 10 * MB,
      max: 5 * MB,
      expected: true,
    },
    {
      label: "max file limit not exceeded",
      size: 4 * MB,
      max: 5 * MB,
      expected: false,
    },
    { label: "tiny files never chunk", size: 256 * 1024, expected: false },
  ];

  test.each(CASES)("%s", ({ label, size, override, max, expected }) => {
    if (typeof override === "number") {
      window.CHUNK_THRESHOLD_BYTES = override;
    }
    if (typeof max === "number") {
      window.MAX_FILE_BYTES = max;
    }
    const file = size === null ? null : { size };
    expect(shouldUseChunk(file)).toBe(expected);
  });
});

describe("selectChunkSize respects overrides and limits", () => {
  const SELECT_CASES = [
    {
      label: "defaults to 8MiB for regular files",
      size: 10 * MB,
      override: undefined,
      expected: 8 * MB,
    },
    {
      label: "clamps override to minimum",
      size: 10 * MB,
      override: 1024,
      expected: 64 * 1024,
    },
    {
      label: "clamps override to maximum",
      size: 10 * MB,
      override: 64 * MB,
      expected: 32 * MB,
    },
    {
      label: "uses override when valid",
      size: 10 * MB,
      override: 12 * MB,
      expected: 12 * MB,
    },
    {
      label: "grows chunk for 200 GiB file",
      size: 200 * GB,
      override: undefined,
      expected: 10737419,
    },
    {
      label: "caps at max for multi-terabyte file",
      size: 2 * TB,
      override: undefined,
      expected: 32 * MB,
    },
    {
      label: "override cannot shrink required chunk",
      size: 400 * GB,
      override: 5 * MB,
      expected: 21474837,
    },
    {
      label: "uses min for explicit 64KiB override",
      size: 5 * MB,
      override: 64 * 1024,
      expected: 64 * 1024,
    },
  ];

  test.each(SELECT_CASES)("%s", ({ label, size, override, expected }) => {
    if (typeof override === "number") {
      window.PREFERRED_CHUNK_SIZE_BYTES = override;
    }
    const chunkSize = selectChunkSize(size);
    expect(chunkSize).toBe(expected);
  });
});

describe("selectChunkSize never exceeds chunk limit", () => {
  const LARGE_FILES = [
    50 * GB,
    150 * GB,
    500 * GB,
    1 * TB,
    3 * TB,
    5 * TB,
    8 * TB,
    10 * TB,
  ];

  const MAX_CHUNK_TOTAL = 32 * MB * 20000;

  test.each(LARGE_FILES)("keeps chunk handling sane for %d bytes", (size) => {
    const chunkSize = selectChunkSize(size);
    const chunkCount = Math.ceil(size / chunkSize);
    expect(chunkSize).toBeGreaterThanOrEqual(64 * 1024);
    expect(chunkSize).toBeLessThanOrEqual(32 * MB);
    if (size <= MAX_CHUNK_TOTAL) {
      expect(chunkCount).toBeLessThanOrEqual(20000);
    } else {
      expect(chunkSize).toBe(32 * MB);
    }
  });
});
