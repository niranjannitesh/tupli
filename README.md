<div align="center">

<img src="assets/app-icon-ribbon/master.png" alt="Tupli" width="128" height="128">

# Tupli

**A native macOS client for Postgres, written in Rust on [GPUI](https://www.gpui.rs).**

Opens instantly. Scrolls a million rows without dropping a frame. Never blocks on the database.

![status: alpha](https://img.shields.io/badge/status-alpha-e8a33d?style=flat-square)
![platform: macOS](https://img.shields.io/badge/platform-macOS%2013%2B-6f7782?style=flat-square)
![built with: Rust](https://img.shields.io/badge/rust-1.90%2B-b7410e?style=flat-square)
![engine: PostgreSQL](https://img.shields.io/badge/postgres-9.6%20%E2%80%93%2017-336791?style=flat-square)
![licence: MIT](https://img.shields.io/badge/licence-MIT-4c9a5f?style=flat-square)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/media/tupli-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="docs/media/tupli-light.png">
  <img src="docs/media/tupli-dark.png" alt="Tupli browsing a table, with the row inspector open">
</picture>

</div>

---

> ### ⚠️ Alpha. In development.
>
> Tupli is being built in the open and is **not ready to be anyone's daily driver**. There
> is no release build, no signed download, no update channel and no upgrade path for the
> local store — it is `cargo build` or nothing. Things move, break and get renamed.
>
> It reads and writes real databases. Point it at something you can afford to be wrong
> about until it has more mileage on it.

## Why

TablePlus is fast and closed. pgAdmin is a web app in a wrapper. DataGrip is a JVM that
takes a minute to think about it. All three make you wait — on launch, on a wide table, on
a scroll to row 400,000.

GPUI is Zed's UI framework: GPU-composited, no DOM, no view hierarchy diffing. That buys a
result grid that is genuinely virtualized and a window that is on screen before you have
let go of the keys. Everything below the UI — connection pool, query execution,
introspection — is off the main thread by construction, so a slow query slows the query
and nothing else.

## What works today

**Connections** — saved connections in a local SQLite store, passwords in the macOS
Keychain (never in the database), `postgres://` URL paste-to-fill, SSL modes, connection
test with server version and latency, colour tag per connection that tints the whole
window.

**Browsing** — schema tree over multiple databases, schemas, tables, views, materialized
views; virtualized grid with frozen gutter, resizable columns, NULL and type-aware
rendering; keyset paging over large tables; per-column filters.

**Editing** — double-click a cell to edit it, type-checked against the column; inserts,
updates and deletes staged and applied in one transaction on **Commit**, with the exact
SQL shown before it runs. Tables without a primary or unique key are read-only, and the
inspector says so.

**Row and table inspector** — every column of the selected row with its type, expandable
for long values and pretty-printed JSON, one-click copy, Set NULL, and follow-the-foreign-key
to the referenced row. The Table tab carries the size, row estimate, owner, primary key
and whether the relation is writable at all.

**SQL** — tree-sitter-backed editor with syntax highlighting, completion for schemas,
tables and columns, statement-under-cursor detection, formatting, run (`⌘↵`) or run-all
(`⌘⇧↵`), cancellation (`⌘.`), messages and timing per statement.

**Structure & DDL** — column, index, constraint and trigger lists; generated `CREATE`
statements for any object.

**The rest of the shell** — split panes, tabs in the titlebar, command palette (`⌘K`),
object jump (`⌘P`), query history, saved queries, settings window, and full light/dark
theming that reloads live.

## Not there yet

SSH tunnelling · import and export (CSV, SQL, JSON) · structure *editing* (the design
sheet is scaffolded, not wired) · connection folders and a proper connection manager
window · Redis as a second engine ([the driver is written](crates/db_redis), the UI is
not) · anything that is not Postgres or macOS.

## Build it

Rust 1.90 or newer, Xcode command line tools, macOS 13+.

```sh
git clone git@github.com:anuvaya/tupli.git
cd tupli
cargo run -p tupli
```

The first build compiles GPUI from source and takes a while. Subsequent ones do not.

To get a real `.app` — with an icon, a Dock presence and a name Spotlight can find:

```sh
scripts/bundle.sh --channel development --open
```

Channels are separate applications, not one application wearing three icons: development,
preview and production each get their own name, bundle identifier and ribboned icon, so
you can run the one you are hacking on next to the one you rely on.

## Where it keeps things

| | |
|---|---|
| `~/Library/Application Support/tupli/tupli.db` | connections, history, saved queries, window state |
| `~/Library/Logs/tupli` | logs, where Console.app already looks |
| Keychain, service `tupli` | one generic password per connection, keyed by its UUID |

Deleting the first one resets the app. Passwords are never written to it.

## Themes

Themes are Zed theme JSON, so a theme written for Zed mostly works here. Bundled:
**Fleet** (Light, Dark, Dark Purple), **One**, **Ayu**, **Gruvbox** — see
[`assets/themes`](assets/themes). Both appearances are first-class; the whole chrome
recolours live, without a restart.

## Keys

| | | | |
|---|---|---|---|
| `⌘K` | command palette | `⌘↵` | run statement |
| `⌘P` | jump to object | `⌘⇧↵` | run everything |
| `⌘T` | new tab | `⌘.` | cancel |
| `⌘N` | new connection | `⌘R` | refresh results |
| `⌘1` `⌘2` `⌘3` | sidebar / results / inspector | `⌘⇧R` | refresh schema |
| `⌘D` `⌘⇧D` | split right / down | `⌥⇧F` | format SQL |
| `⌘,` | settings | `F6` | follow foreign key |

## Layout

```
crates/
  db          engine-agnostic types: connections, schemas, columns, values, errors
  db_pg       the Postgres driver — pooling, introspection, type decoding
  db_redis    the second engine, written to prove the boundary is real
  sqlgen      SQL generation: DML from grid edits, DDL from structure
  grid        the virtualized result grid, as a standalone element
  editor      the SQL editor: rope, tree-sitter, completion
  ui          design system — theme, buttons, tabs, menus, sheets, icons
  store       SQLite + Keychain: connections, history, saved queries
  tupli       the application: window, panes, sidebar, inspector, commands
```

`db` holds the shared vocabulary — rows, values, schemas — and everything above the
drivers is written against it rather than against Postgres. The last mile is not done:
`db_pg` still shows up by name in a couple of places in the app layer, and a `Driver`
trait to dispatch through is the next structural piece of work.

## Developing

```sh
cargo test --workspace
cargo build --workspace --examples
```

There is a headless renderer for reviewing UI changes without a window — it renders both
appearances offscreen to PNG at 2×:

```sh
TUPLI_CONNECT="host=127.0.0.1 dbname=example user=postgres" \
TUPLI_OPEN=public.users \
cargo run -p tupli --example screenshot -- /tmp/shot
```

Most of the interesting state is reachable through `TUPLI_*` environment variables for
exactly this reason — see `crates/tupli/examples/screenshot.rs`.

## Credits

[GPUI](https://github.com/zed-industries/zed) by Zed Industries. Themes adapted from their
upstream projects, each with its licence in [`assets/themes`](assets/themes). Icons are
the commercial [Nucleo](https://nucleoapp.com) set, used under licence — see
[`tools/README.md`](tools/README.md) before regenerating them.

## Licence

The code is [MIT](LICENSE).

The icons under `assets/icons/` are not: they are generated from the commercial
[Nucleo](https://nucleoapp.com) set and are used here under its licence, which the MIT
grant does not extend to. Reuse the code freely; bring your own icons. Bundled themes
carry their upstream licences alongside them in [`assets/themes`](assets/themes).
