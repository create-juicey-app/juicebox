#!/usr/bin/env node
import { build, context } from "esbuild";
import autoprefixer from "autoprefixer";
import cssnano from "cssnano";
import postcss from "postcss";
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
const cssSourcePath = path.join(projectRoot, "public", "css", "app.css");
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
    })
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
    JSON.stringify(manifest, null, 2)
  );
}

async function buildCss({ production }) {
  const source = await readFile(cssSourcePath, "utf8");
  const plugins = [autoprefixer()];
  if (production) {
    plugins.push(cssnano({ preset: "default" }));
  }
  const result = await postcss(plugins).process(source, {
    from: cssSourcePath,
    map: production ? false : { inline: false },
  });
  let fileName = "app.css";
  if (production) {
    const hash = crypto
      .createHash("sha256")
      .update(result.css)
      .digest("hex")
      .slice(0, 8);
    fileName = `app-${hash}.css`;
  }
  const outPath = path.join(distDir, fileName);
  await writeFile(outPath, result.css, "utf8");
  if (!production && result.map) {
    await writeFile(`${outPath}.map`, result.map.toString(), "utf8");
  }
  return fileName;
}

function setupCssWatcher(rebuild) {
  const srcDir = path.dirname(cssSourcePath);
  let timeout = null;
  const watcher = watch(srcDir, { persistent: true }, (eventType, filename) => {
    if (!filename) return;
    const changedPath = path.resolve(srcDir, filename.toString());
    if (changedPath !== cssSourcePath) return;
    if (timeout) clearTimeout(timeout);
    timeout = setTimeout(() => {
      timeout = null;
      rebuild().catch((err) =>
        console.error("[build] CSS rebuild failed", err)
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
      "process.env.NODE_ENV": JSON.stringify(production ? "production" : "development"),
      "window.DEBUG_LOGS": "false",
    },
    minify: production,
  };

  const thumbOptions = {
    ...common,
    entryPoints: [path.join(projectRoot, "public", "js", "thumbgen.js")],
    outfile: path.join(distDir, "thumbgen.js"),
    bundle: true,
    format: "iife",
    globalName: "Thumbgen",
    minify: production,
  };

  if (watch) {
    let cssFile = await buildCss({ production });
    const appCtx = await context(appOptions);
    const thumbCtx = await context(thumbOptions);
    await Promise.all([appCtx.rebuild(), thumbCtx.rebuild()]);
    await generateManifest(distDir, { cssPath: `/dist/${cssFile}` });
    await Promise.all([
      appCtx.watch({
        onRebuild(error) {
          if (error) {
            console.error("[build] App rebuild failed", error);
          } else {
            generateManifest(distDir, { cssPath: `/dist/${cssFile}` }).catch((err) =>
              console.error("[build] Manifest refresh failed", err)
            );
          }
        },
      }),
      thumbCtx.watch({
        onRebuild(error) {
          if (error) {
            console.error("[build] Thumbgen rebuild failed", error);
          }
        },
      }),
    ]);
    const disposeCssWatcher = setupCssWatcher(async () => {
      cssFile = await buildCss({ production });
      await generateManifest(distDir, { cssPath: `/dist/${cssFile}` });
      console.log("[build] CSS rebuilt");
    });
    console.log("Watching for changes...");
    return () =>
      Promise.all([appCtx.dispose(), thumbCtx.dispose(), disposeCssWatcher()]);
  }

  const cssFile = await buildCss({ production });
  await Promise.all([build(appOptions), build(thumbOptions)]);
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
