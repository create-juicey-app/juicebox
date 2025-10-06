import { ttlCodeSeconds, escapeHtml } from "../public/js/utils.js";

describe("ttlCodeSeconds comprehensive mapping", () => {
  const TTL_CASES = [
    { code: "1h", expected: 3600 },
    { code: "3h", expected: 10800 },
    { code: "12h", expected: 43200 },
    { code: "1d", expected: 86400 },
    { code: "3d", expected: 259200 },
    { code: "7d", expected: 604800 },
    { code: "14d", expected: 1209600 },
    { code: "", expected: 259200 },
    { code: undefined, expected: 259200 },
    { code: "999d", expected: 259200 },
  ];

  test.each(TTL_CASES)("maps %p to %d seconds", ({ code, expected }) => {
    expect(ttlCodeSeconds(code)).toBe(expected);
  });
});

describe("escapeHtml handles diverse strings", () => {
  const ESCAPE_CASES = [
    { input: "<", expected: "&lt;" },
    { input: ">", expected: "&gt;" },
    { input: "&", expected: "&amp;" },
    { input: '"', expected: "&quot;" },
    { input: "'", expected: "&#39;" },
    { input: "<script>", expected: "&lt;script&gt;" },
    {
      input: '<a href="test">link</a>',
      expected: "&lt;a href=&quot;test&quot;&gt;link&lt;/a&gt;",
    },
    {
      input: "I <3 & it's > none",
      expected: "I &lt;3 &amp; it&#39;s &gt; none",
    },
    { input: "", expected: "" },
    { input: "plain text", expected: "plain text" },
    { input: "&&&", expected: "&amp;&amp;&amp;" },
    {
      input: "\n<>&\"'",
      expected: "\n&lt;&gt;&amp;&quot;&#39;",
    },
  ];

  test.each(ESCAPE_CASES)("escapes %p correctly", ({ input, expected }) => {
    expect(escapeHtml(input)).toBe(expected);
  });
});
