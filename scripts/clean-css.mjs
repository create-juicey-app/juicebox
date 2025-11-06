#!/usr/bin/env node
import fs from "fs/promises";
import path from "path";
import postcss from "postcss";
import mergeRules from "postcss-merge-rules";
import discardDuplicates from "postcss-discard-duplicates";
import autoprefixer from "autoprefixer";
import cssnano from "cssnano";

const cssDir = path.resolve("./public/css"); // folder with your CSS/SCSS
const outputFile = path.join(cssDir, "app.clean.css"); // cleaned source output

async function mergeCssFiles() {
  const files = await fs.readdir(cssDir);
  const cssFiles = files.filter(
    (f) => f.endsWith(".css") || f.endsWith(".scss"),
  );

  let combinedCss = "";
  for (const file of cssFiles) {
    const content = await fs.readFile(path.join(cssDir, file), "utf8");
    combinedCss += content + "\n";
  }

  return combinedCss;
}

async function cleanCss() {
  const combinedCss = await mergeCssFiles();

  const plugins = [
    autoprefixer(),
    mergeRules(),
    discardDuplicates(),
    cssnano({ preset: "default" }),
  ];

  const result = await postcss(plugins).process(combinedCss, {
    from: undefined,
    map: false,
  });

  await fs.writeFile(outputFile, result.css, "utf8");
  console.log("CSS cleaned and written to", outputFile);
}

cleanCss().catch((err) => {
  console.error(err);
  process.exit(1);
});
