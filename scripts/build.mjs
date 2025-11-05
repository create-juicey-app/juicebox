#!/usr/bin/env node
import { build, context } from "esbuild";
import autoprefixer from "autoprefixer";
import cssnano from "cssnano";
import postcss from "postcss";
import * as sass from "sass";
import { mkdir, rm, readdir, readFile, writeFile } from "node:fs/promises";
import { watch } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import crypto from "node:crypto";
import { brotliCompress, gzip } from "node:zlib";
import { promisify } from "node:util";

const brotli = promisify(brotliCompress);
const gzipCompress = promisify(gzip);

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const projectRoot = path.resolve(__dirname, "..");
const distDir = path.join(projectRoot, "public", "dist");

// SCSS source: single app.scss
const cssSourcePath = path.join(projectRoot, "public", "css", "app.scss");
const cssDir = path.dirname(cssSourcePath);
const watchMode = process.argv.includes("--watch");

async function ensureCleanDist() {
  await rm(distDir, { recursive: true, force: true });
  await mkdir(distDir, { recursive: true });
}

async function precompressArtifacts(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  await Promise.all(
    entries.map(async (entry) => {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        await precompressArtifacts(full);
        return;
      }
      if (!entry.isFile()) return;
      if (!/\.(js|css|html)$/i.test(entry.name)) return;
      const source = await readFile(full);
      const [brBuf, gzBuf] = await Promise.all([
        brotli(source),
        gzipCompress(source),
      ]);
      await Promise.all([
        writeFile(`${full}.br`, brBuf),
        writeFile(`${full}.gz`, gzBuf),
      ]);
    }),
  );
}

async function generateManifest(dir, overrides = {}) {
  const entries = await readdir(dir);
  const manifest = {};
  const main = entries.find((name) => /^app-.*\.js$/.test(name)) || "app.js";
  manifest.app = `/dist/${main}`;

  const cssEntry =
    entries.find((name) => /^app-.*\.css$/.test(name)) ||
    entries.find((name) => name === "app.css");
  if (overrides.cssPath) {
    manifest.css = overrides.cssPath;
  } else if (cssEntry) {
    manifest.css = `/dist/${cssEntry}`;
  }

  await writeFile(
    path.join(dir, "manifest.json"),
    JSON.stringify(manifest, null, 2),
  );
}

// Compile SCSS with dart-sass, then run PostCSS plugins (autoprefixer + cssnano in prod)
async function buildCss({ production }) {
  // Compile SCSS -> CSS
  const sassResult = sass.compile(cssSourcePath, {
    style: production ? "expanded" : "expanded",
    sourceMap: !production,
    loadPaths: [path.join(projectRoot, "public", "css")],
  });

  const compiledCss = sassResult.css;

  let prevMap;
  if (!production && sassResult.sourceMap) {
    if (typeof sassResult.sourceMap === "string") {
      prevMap = sassResult.sourceMap;
    } else {
      try {
        prevMap = JSON.stringify(sassResult.sourceMap);
      } catch {
        prevMap = null;
      }
    }
  }

  const plugins = [autoprefixer()];
  if (production) plugins.push(cssnano({ preset: "default" }));

  const postcssResult = await postcss(plugins).process(compiledCss, {
    from: cssSourcePath,
    map: production ? false : { prev: prevMap, inline: false },
  });

  const baseName = "app";
  let fileName = `${baseName}.css`;
  if (production) {
    const hash = crypto
      .createHash("sha256")
      .update(postcssResult.css)
      .digest("hex")
      .slice(0, 8);
    fileName = `${baseName}-${hash}.css`;
  }

  const outPath = path.join(distDir, fileName);
  await writeFile(outPath, postcssResult.css, "utf8");
  if (!production && postcssResult.map) {
    await writeFile(`${outPath}.map`, postcssResult.map.toString(), "utf8");
  }

  return fileName;
}

function setupCssWatcher(rebuild) {
  const srcDir = cssDir;
  let timeout = null;
  const watcher = watch(srcDir, { persistent: true }, (eventType, filename) => {
    if (!filename) return;
    const changedPath = path.resolve(srcDir, filename.toString());
    if (!changedPath.endsWith(".scss")) return;
    if (timeout) clearTimeout(timeout);
    timeout = setTimeout(() => {
      timeout = null;
      rebuild().catch((err) =>
        console.error("[build] CSS rebuild failed", err),
      );
    }, 50);
  });
  watcher.on("error", (err) => console.error("[build] CSS watcher error", err));
  return async () => {
    if (timeout) {
      clearTimeout(timeout);
      timeout = null;
    }
    watcher.close();
  };
}

async function buildApp({ watch }) {
  const production = !watch;

  const common = {
    platform: "browser",
    target: "es2020",
    sourcemap: true,
    logLevel: "info",
    tsconfigRaw: {
      compilerOptions: {
        useDefineForClassFields: false,
      },
    },
    drop: production ? ["console", "debugger"] : [],
    legalComments: "none",
  };

  const appOptions = {
    ...common,
    entryPoints: [path.join(projectRoot, "public", "js", "app.js")],
    outdir: distDir,
    bundle: true,
    format: "esm",
    splitting: true,
    chunkNames: "chunks/[name]-[hash]",
    entryNames: production ? "[name]-[hash]" : "[name]",
    define: {
      "process.env.NODE_ENV": JSON.stringify(
        production ? "production" : "development",
      ),
      "window.DEBUG_LOGS": "false",
    },
    minify: production,
  };

  if (watch) {
    let cssFile = await buildCss({ production });

    function createRebuildPlugin(label, onSuccess) {
      return {
        name: `watch-${label}`,
        setup(build) {
          build.onEnd((result) => {
            if (
              result &&
              Array.isArray(result.errors) &&
              result.errors.length
            ) {
              console.error(
                "[build] " + label + " rebuild failed",
                result.errors,
              );
            } else {
              if (typeof onSuccess === "function") {
                Promise.resolve(onSuccess()).catch((err) =>
                  console.error(
                    "[build]",
                    label,
                    "post-rebuild hook failed",
                    err,
                  ),
                );
              }
              console.log("[build] " + label + " rebuilt");
            }
          });
        },
      };
    }

    const appCtx = await context({
      ...appOptions,
      plugins: [
        ...(appOptions.plugins || []),
        createRebuildPlugin("app", () =>
          generateManifest(distDir, { cssPath: `/dist/${cssFile}` }),
        ),
      ],
    });

    await mkdir(distDir, { recursive: true });
    await appCtx.rebuild();
    await generateManifest(distDir, { cssPath: `/dist/${cssFile}` });

    await appCtx.watch();

    const disposeCssWatcher = setupCssWatcher(async () => {
      cssFile = await buildCss({ production });
      await generateManifest(distDir, { cssPath: `/dist/${cssFile}` });
      console.log("[build] CSS rebuilt");
    });

    console.log("Watching for changes...");
    return () => Promise.all([appCtx.dispose(), disposeCssWatcher()]);
  }

  const cssFile = await buildCss({ production });
  await build(appOptions);
  await precompressArtifacts(distDir);
  await generateManifest(distDir, { cssPath: `/dist/${cssFile}` });
}

(async () => {
  try {
    await ensureCleanDist();
    await buildApp({ watch: watchMode });
  } catch (err) {
    console.error(err);
    process.exitCode = 1;
  }
})();
