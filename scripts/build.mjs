#!/usr/bin/env node
import { build, context } from "esbuild";
import { mkdir, rm, readdir, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { brotliCompress, gzip } from "node:zlib";
import { promisify } from "node:util";

const brotli = promisify(brotliCompress);
const gzipCompress = promisify(gzip);

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const projectRoot = path.resolve(__dirname, "..");
const distDir = path.join(projectRoot, "public", "dist");
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

async function generateManifest(dir) {
  const entries = await readdir(dir);
  const manifest = {};
  const main = entries.find((name) => /^app-.*\.js$/.test(name)) || "app.js";
  manifest.app = `/dist/${main}`;
  await writeFile(
    path.join(dir, "manifest.json"),
    JSON.stringify(manifest, null, 2)
  );
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
    const appCtx = await context(appOptions);
    const thumbCtx = await context(thumbOptions);
    await Promise.all([appCtx.watch(), thumbCtx.watch()]);
    console.log("Watching for changes...");
    return () => Promise.all([appCtx.dispose(), thumbCtx.dispose()]);
  }

  await Promise.all([build(appOptions), build(thumbOptions)]);
  await precompressArtifacts(distDir);
  await generateManifest(distDir);
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
