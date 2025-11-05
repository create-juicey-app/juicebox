/**
 * Tiny SCSS sanity tests:
 * - Verifies we can compile the main SCSS entry (app.scss)
 * - Verifies token mixins emit CSS custom properties into a scoped selector
 *
 * These tests exercise the Dart Sass API directly, not the esbuild pipeline.
 */

import path from "node:path";

import sass from "sass";

const repoRoot = path.resolve(__dirname, ".."); // points to juicebox/
const cssDir = path.resolve(repoRoot, "public", "css");
const appScss = path.join(cssDir, "app.scss");

describe("SCSS build sanity", () => {
  test("compiles app.scss and outputs root CSS variables", () => {
    const result = sass.compile(appScss, {
      loadPaths: [cssDir],
      sourceMap: false,
      style: "expanded",
    });
    const css = result.css || "";

    // Basic assertions that compilation succeeded and tokens got emitted.
    expect(css.length).toBeGreaterThan(1000);
    expect(css).toContain(":root");
    expect(css).toMatch(/--accent:\s*#?[0-9a-fA-F]{3,8}|--accent:\s*[a-zA-Z]+/);
    expect(css).toMatch(/--bg:\s*#?[0-9a-fA-F]{3,8}|--bg:\s*[a-zA-Z]+/);

    // Ensure some component styles also made it through (e.g., drop-zone)
    expect(css).toContain(".drop-zone");
  });

  test("tokens mixin can emit themed scope variables", () => {
    const result = sass.compileString(
      `
      @use "tokens" as tokens;

      // Emit default tokens into a custom scope
      @include tokens.css-tokens("[data-theme=\\"light\\"]");

      // Emit with a small override map
      @include tokens.css-tokens-override("[data-theme=\\"light\\"]", (
        bg: #ffffff,
        text: #0b1621
      ));
    `,
      {
        loadPaths: [cssDir],
        sourceMap: false,
        style: "expanded",
      },
    );

    const css = result.css || "";
    expect(css).toMatch(/\[data-theme=(?:"light"|light)\]/);
    expect(css).toMatch(/\[data-theme=(?:"light"|light)\]\s*{\s*--accent:/);
    expect(css).toMatch(
      /\[data-theme=(?:"light"|light)\]\s*{[^}]*--bg:\s*#ffffff/i,
    );
    expect(css).toMatch(
      /\[data-theme=(?:"light"|light)\]\s*{[^}]*--text:\s*#0b1621/i,
    );
  });
});
