# Tupli

A native macOS database client. Rust + GPUI.

## Running it

`scripts/run.sh` — not `cargo run`, and not `target/debug/tupli`.

macOS reads an application's icon, its name in the menu bar and its Keychain
identity from the bundle, not from the executable, so a bare binary is a
different, nameless application with the generic icon. `scripts/run.sh` builds,
bundles, replaces any running instance and opens the result.

`scripts/dev-identity.sh` is a one-time step that creates a local code-signing
certificate. Without it every build is signed ad hoc, which means a new code
identity every time, which means the Keychain asks for permission again.

## Layout

    crates/db             the data model, the `Driver` trait, `Capabilities`
    crates/db_pg          Postgres, over the wire protocol
    crates/db_redis       Redis, over RESP
    crates/db_clickhouse  ClickHouse, over its native protocol
    crates/drivers        the registry; the only crate that knows the engines by name
    crates/sqlgen         SQL the app writes rather than the user
    crates/grid           the virtualised table
    crates/ui             buttons, menus, theme
    crates/editor         the SQL editor
    crates/store          SQLite for settings and history, Keychain for passwords
    crates/tupli          the application

The app depends on `drivers` and `db`, never on an engine crate. Anything the
UI needs to branch on is a flag on `Capabilities`, so the question is never
"is this Redis?" but "can these rows be edited?".

## House rules

- Rust 1.97.1, edition 2021. No let-chains, no `async fn` in traits.
- Never `cargo fmt --all`: the tree is not rustfmt-clean and it would bury the
  diff. Format the files you touched.
- Comments say *why*. A comment that restates the code is worse than none.
- Tests are named as sentences and assert on behaviour.
