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
const args = process.argv.slice(2);
const watchMode = args.includes("--watch");
const profileMode = args.includes("--profile");

if (watchMode && profileMode) {
  console.error("[build] --watch and --profile cannot be used together");
  process.exit(1);
}

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

  return {
    fileName,
    bytes: Buffer.byteLength(postcssResult.css, "utf8"),
  };
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

async function buildApp({ watch, profile }) {
  const production = profile || !watch;

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
        production ? "production" : "development"
      ),
      "window.DEBUG_LOGS": "false",
    },
    minify: production,
  };

  if (watch) {
    let cssInfo = await buildCss({ production });

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
                result.errors
              );
            } else {
              if (typeof onSuccess === "function") {
                Promise.resolve(onSuccess()).catch((err) =>
                  console.error(
                    "[build]",
                    label,
                    "post-rebuild hook failed",
                    err
                  )
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
          generateManifest(distDir, { cssPath: `/dist/${cssInfo.fileName}` })
        ),
      ],
    });

    await mkdir(distDir, { recursive: true });
    await appCtx.rebuild();
    await generateManifest(distDir, { cssPath: `/dist/${cssInfo.fileName}` });

    await appCtx.watch();

    const disposeCssWatcher = setupCssWatcher(async () => {
      cssInfo = await buildCss({ production });
      await generateManifest(distDir, { cssPath: `/dist/${cssInfo.fileName}` });
      console.log("[build] CSS rebuilt");
    });

    console.log("Watching for changes...");
    return () => Promise.all([appCtx.dispose(), disposeCssWatcher()]);
  }

  const cssInfo = await buildCss({ production });
  const buildResult = await build({
    ...appOptions,
    metafile: profile,
    logLevel: profile ? "warning" : appOptions.logLevel,
  });
  if (profile && buildResult.metafile) {
    const summary = await writeBundleProfile(buildResult.metafile, cssInfo);
    console.log(
      `[profile] Bundle total ${summary.totalJsHumanSize} (JS) · CSS ${
        summary.css?.humanSize ?? "n/a"
      }`
    );
  }
  await precompressArtifacts(distDir);
  await generateManifest(distDir, { cssPath: `/dist/${cssInfo.fileName}` });
}

function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let idx = 0;
  while (value >= 1024 && idx < units.length - 1) {
    value /= 1024;
    idx += 1;
  }
  const precision = value >= 100 || idx === 0 ? 0 : value >= 10 ? 1 : 2;
  const formatted = value.toFixed(precision).replace(/\.0+$/, "");
  return `${formatted} ${units[idx]}`;
}

function summarizeMetafile(metafile, cssInfo) {
  const outputs = Object.entries(metafile.outputs || {});
  const bundles = outputs
    .filter(([, info]) => info.entryPoint || info.inputs)
    .map(([file, info]) => ({
      file,
      bytes: info.bytes || 0,
      humanSize: formatBytes(info.bytes || 0),
      entryPoint: info.entryPoint || null,
      imports: Array.isArray(info.imports) ? info.imports.length : 0,
    }))
    .sort((a, b) => b.bytes - a.bytes);

  const totalJsBytes = bundles.reduce((acc, item) => acc + item.bytes, 0);

  const moduleSizes = new Map();
  for (const [, info] of outputs) {
    if (!info.inputs) continue;
    for (const [modulePath, meta] of Object.entries(info.inputs)) {
      const previous = moduleSizes.get(modulePath) || 0;
      const moduleBytes = meta.bytesInOutput ?? meta.bytes ?? 0;
      moduleSizes.set(modulePath, previous + moduleBytes);
    }
  }

  const topModules = Array.from(moduleSizes.entries())
    .map(([path, bytes]) => ({
      path,
      bytes,
      humanSize: formatBytes(bytes),
    }))
    .sort((a, b) => b.bytes - a.bytes)
    .slice(0, 25);

  const cssSummary = cssInfo
    ? {
        file: cssInfo.fileName,
        bytes: cssInfo.bytes,
        humanSize: formatBytes(cssInfo.bytes),
      }
    : null;

  return {
    generatedAt: new Date().toISOString(),
    totalJsBytes,
    totalJsHumanSize: formatBytes(totalJsBytes),
    bundleCount: bundles.length,
    css: cssSummary,
    bundles,
    topModules,
  };
}

function formatProfileMarkdown(summary) {
  const lines = [];
  lines.push("# Frontend Bundle Profile");
  lines.push("");
  lines.push(`Generated at ${summary.generatedAt}`);
  lines.push("");
  lines.push(
    `- Total JS: **${summary.totalJsHumanSize}** (${summary.totalJsBytes} bytes)`
  );
  if (summary.css) {
    lines.push(
      `- CSS (${summary.css.file}): **${summary.css.humanSize}** (${summary.css.bytes} bytes)`
    );
  }
  lines.push(`- Bundles emitted: ${summary.bundleCount}`);
  lines.push("");

  if (summary.bundles.length) {
    lines.push("## Bundles");
    lines.push("");
    lines.push("| File | Size | Entry Point | Imports |");
    lines.push("| --- | --- | --- | --- |");
    for (const bundle of summary.bundles) {
      const entry = bundle.entryPoint ? `\`${bundle.entryPoint}\`` : "—";
      lines.push(
        `| \`${bundle.file}\` | ${bundle.humanSize} | ${entry} | ${bundle.imports} |`
      );
    }
    lines.push("");
  }

  if (summary.topModules.length) {
    lines.push("## Top modules by size");
    lines.push("");
    lines.push("| Module | Size |");
    lines.push("| --- | --- |");
    for (const module of summary.topModules) {
      lines.push(`| \`${module.path}\` | ${module.humanSize} |`);
    }
    lines.push("");
  }

  lines.push(
    "Use `npx esbuild-analyze public/dist/profile/meta.json` for an interactive view."
  );

  return `${lines.join("\n")}\n`;
}

async function writeBundleProfile(metafile, cssInfo) {
  const profileDir = path.join(distDir, "profile");
  await mkdir(profileDir, { recursive: true });
  await writeFile(
    path.join(profileDir, "meta.json"),
    JSON.stringify(metafile, null, 2),
    "utf8"
  );
  const summary = summarizeMetafile(metafile, cssInfo);
  await writeFile(
    path.join(profileDir, "summary.json"),
    JSON.stringify(summary, null, 2),
    "utf8"
  );
  await writeFile(
    path.join(profileDir, "summary.md"),
    formatProfileMarkdown(summary),
    "utf8"
  );
  return summary;
}

(async () => {
  try {
    await ensureCleanDist();
    await buildApp({ watch: watchMode, profile: profileMode });
  } catch (err) {
    console.error(err);
    process.exitCode = 1;
  }
})();
