// Copy the curated Nucleo subset out of the extraction scratch dir into assets/icons/.
// Duo icons keep their two layers (`name.svg` + `name.duo.svg`) so the Icon element
// can stack them; gpui tints an SVG with a single color, so duotone needs two draws.
import { readFileSync, writeFileSync, mkdirSync, rmSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const SRC = process.env.ICON_SRC;
const OUT = join(root, "assets/icons");
const map = JSON.parse(readFileSync(join(root, "tools/icons.json"), "utf8"));

rmSync(OUT, { recursive: true, force: true });
mkdirSync(OUT, { recursive: true });

const missing = [];
let n = 0;

// Semantic pack name -> directory the extractor wrote.
const DIRS = { outline: "outline", bold: "micro", fill: "fill", duo: "duo" };

for (const [pack, entries] of Object.entries({
  outline: map.outline,
  bold: map.bold,
  fill: map.fill,
  duo: map.duo,
})) {
  for (const [name, src] of Object.entries(entries)) {
    const from = `${SRC}/${DIRS[pack]}/${src}.svg`;
    if (!existsSync(from)) { missing.push(`${pack}/${src}`); continue; }
    writeFileSync(`${OUT}/${name}.svg`, readFileSync(from));
    n++;
    const duo = `${SRC}/${DIRS[pack]}/${src}.duo.svg`;
    if (pack === "duo" && existsSync(duo)) {
      writeFileSync(`${OUT}/${name}.duo.svg`, readFileSync(duo));
      n++;
    }
  }
}

console.log(`copied ${n} files`);
if (missing.length) console.log(`MISSING (${missing.length}): ${missing.join(", ")}`);
