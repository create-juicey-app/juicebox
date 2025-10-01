import { fmtBytes, ttlCodeSeconds, escapeHtml } from "../public/js/utils.js";

describe("fmtBytes", () => {
  it("formats bytes into human readable strings", () => {
    expect(fmtBytes(0)).toBe("0 B");
    expect(fmtBytes(1024)).toBe("1.00 KB");
    expect(fmtBytes(5 * 1024 * 1024)).toBe("5.00 MB");
  });

  it("handles invalid numbers gracefully", () => {
    expect(fmtBytes(NaN)).toBe("–");
    expect(fmtBytes(-1)).toBe("–");
  });
});

describe("ttlCodeSeconds", () => {
  it("returns seconds for known codes", () => {
    expect(ttlCodeSeconds("1h")).toBe(3600);
    expect(ttlCodeSeconds("3d")).toBe(259200);
  });

  it("falls back to default when unknown", () => {
    expect(ttlCodeSeconds("unknown")).toBe(259200);
  });
});

describe("escapeHtml", () => {
  it("escapes special characters", () => {
    expect(escapeHtml('<script>alert("x")</script>')).toBe("&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;");
  });
});
