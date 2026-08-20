# tools

## Icons

The UI icon set is the paid **Nucleo** library, in two packs:

| pack | usage | box | style |
|---|---|---|---|
| `nucleo-micro-bold` | every UI affordance: toolbars, tree, buttons, status | 20×20 | 2px stroke |
| `nucleo-ui-fill-duo-18` | large decorative/empty-state art only | 18×18 | two-layer fill |

Regenerating `assets/icons/` is a two-step process.

### 1. Install the packs

They are private npm packages gated by a license key:

```sh
export NUCLEO_LICENSE_KEY=<your Nucleo licence key>
npm i nucleo-micro-bold@^1.2.0 nucleo-ui-fill-duo-18@^1.5.0
```

The key is a customer's Nucleo licence and is not in this repository. It belongs in the
env, not in a file and not in a request to anything other than the Nucleo registry. The
generated SVGs in `assets/icons/` are committed, so a clone does not need the key unless
it is regenerating the set.

### 2. Extract, then curate

```sh
NUCLEO_MODULES=./node_modules ICON_OUT=/tmp/nucleo-svg node tools/extract_icons.mjs
ICON_SRC=/tmp/nucleo-svg node tools/build_icons.mjs
```

`extract_icons.mjs` renders every React icon component to a plain SVG with
`react-dom/server`, normalises the root tag (drops `width`/`height`, keeps `viewBox`)
and — for the duo pack — splits the children into two files:

* `name.svg` — the primary paths
* `name.duo.svg` — the paths marked `data-color="color-2"`, which keep their
  `fill-opacity="0.4"`

gpui rasterises an SVG as a single-channel alpha mask tinted by `text_color`
(`paint_svg` in `crates/gpui/src/elements/svg.rs`), so real duotone means drawing
the two files as two stacked layers in different colours. The retained
`fill-opacity` means a duo icon still reads correctly if only one layer is drawn.

`tools/icons.json` is the curated map of semantic name → source file. Add an entry
there and re-run `build_icons.mjs`; nothing else references the 8k-file raw set.
