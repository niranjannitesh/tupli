// Render Nucleo React icon components to plain SVG files.
// Duotone icons are split into two layers so gpui (which tints an SVG with a
// single color) can stack them and produce real duotone.
import { readdirSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { pathToFileURL } from "node:url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";

// Where the packs were installed, and where the SVGs should land. Both are
// outside the repository: the input is licensed and the output is 8k files, of
// which `build_icons.mjs` keeps the ~150 named in `icons.json`.
const NM = process.env.NUCLEO_MODULES ?? "./node_modules";
const OUT = process.env.ICON_OUT ?? "./svg";

const PACKS = [
  { pkg: "nucleo-micro-bold", out: "micro", duo: false, strip: /^Icon/ },
  {
    pkg: "nucleo-ui-fill-duo-18",
    out: "duo",
    duo: true,
    strip: /^Icon|FillDuo18$/g,
  },
];

// kebab-case from PascalCase, keeping digit groups attached.
function kebab(name) {
  return name
    .replace(/([a-z0-9])([A-Z])/g, "$1-$2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1-$2")
    .toLowerCase();
}

// Split rendered markup into the <svg ...> open tag, the child elements, and close.
function parse(markup) {
  const open = markup.match(/^<svg[^>]*>/)[0];
  const inner = markup.slice(open.length, markup.lastIndexOf("</svg>"));
  // Children are self-closing or paired top-level elements.
  const nodes = [];
  let i = 0;
  while (i < inner.length) {
    if (inner[i] !== "<") {
      i++;
      continue;
    }
    const tag = inner.slice(i).match(/^<([a-zA-Z]+)/);
    if (!tag) break;
    const selfClose = inner.indexOf("/>", i);
    const pairClose = inner.indexOf(`</${tag[1]}>`, i);
    const gt = inner.indexOf(">", i);
    let end;
    if (selfClose !== -1 && selfClose + 1 === gt) {
      end = gt + 1;
    } else if (pairClose !== -1) {
      end = pairClose + tag[1].length + 3;
    } else {
      end = gt + 1;
    }
    nodes.push(inner.slice(i, end));
    i = end;
  }
  return { open, nodes };
}

const written = { total: 0, duoPairs: 0 };

for (const pack of PACKS) {
  const dir = `${NM}/${pack.pkg}/dist/components`;
  const outDir = `${OUT}/${pack.out}`;
  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(outDir, { recursive: true });

  const files = readdirSync(dir).filter(
    (f) => f.endsWith(".js") && f !== "Icon.js"
  );

  for (const file of files) {
    const compName = file.replace(/\.js$/, "");
    let mod;
    try {
      mod = await import(pathToFileURL(`${dir}/${file}`).href);
    } catch {
      continue;
    }
    const Comp = mod[compName];
    if (typeof Comp !== "function") continue;

    let markup;
    try {
      markup = renderToStaticMarkup(React.createElement(Comp));
    } catch {
      continue;
    }

    const { open, nodes } = parse(markup);
    // Normalize the root tag: drop width/height so the consumer sizes it.
    const viewBox = (open.match(/viewBox="([^"]*)"/) || [, "0 0 20 20"])[1];
    const root = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${viewBox}">`;

    const base = kebab(compName.replace(pack.strip, ""));

    if (pack.duo) {
      const secondary = nodes.filter((n) => n.includes('data-color="color-2"'));
      const primary = nodes.filter((n) => !n.includes('data-color="color-2"'));
      writeFileSync(
        `${outDir}/${base}.svg`,
        `${root}${primary.join("")}</svg>\n`
      );
      written.total++;
      if (secondary.length) {
        writeFileSync(
          `${outDir}/${base}.duo.svg`,
          `${root}${secondary.join("")}</svg>\n`
        );
        written.total++;
        written.duoPairs++;
      }
    } else {
      writeFileSync(`${outDir}/${base}.svg`, `${root}${nodes.join("")}</svg>\n`);
      written.total++;
    }
  }
  console.log(`${pack.pkg}: ${files.length} components -> ${outDir}`);
}

console.log(
  `wrote ${written.total} svg files (${written.duoPairs} with a duo layer)`
);
