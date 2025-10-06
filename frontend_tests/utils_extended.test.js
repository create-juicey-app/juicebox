import { fmtBytes } from "../public/js/utils.js";

describe("fmtBytes exact outputs", () => {
  const EXACT_CASES = [
    { value: 0, expected: "0 B" },
    { value: 1, expected: "1 B" },
    { value: 16, expected: "16 B" },
    { value: 512, expected: "512 B" },
    { value: 1023, expected: "1023 B" },
    { value: 1024, expected: "1.00 KB" },
    { value: 1536, expected: "1.50 KB" },
    { value: 4096, expected: "4.00 KB" },
    { value: 10240, expected: "10.00 KB" },
    { value: 65536, expected: "64.00 KB" },
    { value: 131072, expected: "128.00 KB" },
    { value: 524288, expected: "512.00 KB" },
    { value: 1048576, expected: "1.00 MB" },
    { value: 10485760, expected: "10.00 MB" },
    { value: 123456789, expected: "117.74 MB" },
    { value: 536870912, expected: "512.00 MB" },
    { value: 1073741824, expected: "1.00 GB" },
    { value: 5368709120, expected: "5.00 GB" },
    { value: 10737418240, expected: "10.00 GB" },
    { value: 17179869184, expected: "16.00 GB" },
    { value: 45097156608, expected: "42.00 GB" },
    { value: 137438953472, expected: "128.00 GB" },
    { value: 549755813888, expected: "512.00 GB" },
    { value: 1099511627776, expected: "1.00 TB" },
    { value: 5497558138880, expected: "5.00 TB" },
  ];

  test.each(EXACT_CASES)("formats %d bytes", ({ value, expected }) => {
    expect(fmtBytes(value)).toBe(expected);
  });
});

describe("fmtBytes rounding behaviour", () => {
  const ROUND_CASES = [
    { value: 1234, expected: "1.21 KB" },
    { value: 5678, expected: "5.54 KB" },
    { value: 891011, expected: "870.13 KB" },
    { value: 1234567, expected: "1.18 MB" },
    { value: 9876543, expected: "9.42 MB" },
    { value: 7654321, expected: "7.30 MB" },
    { value: 1357911, expected: "1.30 MB" },
    { value: 42424242, expected: "40.46 MB" },
    { value: 50505050, expected: "48.17 MB" },
    { value: 600000000, expected: "572.20 MB" },
    { value: 700000000, expected: "667.57 MB" },
    { value: 800000000, expected: "762.94 MB" },
    { value: 900000000, expected: "858.31 MB" },
    { value: 1000000001, expected: "953.67 MB" },
    { value: 1200000000, expected: "1.12 GB" },
  ];

  test.each(ROUND_CASES)(
    "keeps two decimals for %d bytes",
    ({ value, expected }) => {
      expect(fmtBytes(value)).toBe(expected);
    }
  );
});

describe("fmtBytes unit selection", () => {
  const UNIT_CASES = [
    { value: 500, unit: "B" },
    { value: 10 * 1024, unit: "KB" },
    { value: 5 * 1024 * 1024, unit: "MB" },
    { value: 2 * 1024 * 1024 * 1024, unit: "GB" },
    { value: 6 * 1024 * 1024 * 1024 * 1024, unit: "TB" },
    { value: 987654321, unit: "MB" },
    { value: 12 * 1024 * 1024, unit: "MB" },
    { value: 14 * 1024, unit: "KB" },
    { value: 4 * 1024 * 1024 * 1024 * 1024, unit: "TB" },
    { value: 42, unit: "B" },
  ];

  test.each(UNIT_CASES)("uses %s for %d bytes", ({ value, unit }) => {
    expect(fmtBytes(value)).toMatch(new RegExp(`\\b${unit}$`));
  });
});
