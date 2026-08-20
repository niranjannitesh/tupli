//! Reading the catalog out of `system`.
//!
//! Four queries for the whole server, joined here rather than by the server.
//! ClickHouse would join them happily; doing it in the client keeps each query
//! to one table and one order, which is what makes the four cheap enough to run
//! on every connect.
//!
//! The shape this produces needs one thing said about it. ClickHouse databases
//! are not Postgres databases: a session sees every one of them and can join
//! across them without reconnecting. That makes them schemas, and they are
//! mapped onto [`Schema`] rather than onto the snapshot's database list — which
//! is why [`Capabilities::CLICKHOUSE`] has `schemas` and not `databases`.
//!
//! [`Capabilities::CLICKHOUSE`]: db::Capabilities::CLICKHOUSE

use std::collections::HashMap;
use std::sync::Arc;

use db::{
    CellText, ColumnDef, DbResult, IndexDef, Relation, RelationKind, RelationRef, ResultSet,
    Schema, SchemaSnapshot,
};

use crate::client::ClickHouseConnection;
use crate::types;

/// A ceiling on each catalog query.
///
/// Well past any real server — a hundred thousand columns is a thousand
/// hundred-column tables — and low enough that a runaway `system.columns` on a
/// shared warehouse cannot take the window down with it. A snapshot that hits
/// this is silently short, which is why it is far enough out that hitting it
/// means something is wrong rather than something is large.
const CATALOG_LIMIT: usize = 500_000;

/// Databases whose contents are the server describing itself.
///
/// Shown on request and hidden by default, the same as `pg_catalog`. Spelled
/// both ways because ClickHouse really does have `INFORMATION_SCHEMA` and
/// `information_schema` as two databases with the same contents.
fn is_system_database(name: &str) -> bool {
    matches!(name, "system" | "INFORMATION_SCHEMA" | "information_schema")
}

pub async fn snapshot(
    connection: &ClickHouseConnection,
    server_version: Arc<str>,
) -> DbResult<SchemaSnapshot> {
    let databases = connection
        .fetch(
            "select name from system.databases order by name",
            CATALOG_LIMIT,
        )
        .await?;
    let tables = connection
        .fetch(
            // `total_rows` and `total_bytes` come back nullable and are left
            // that way: the null means "this engine does not keep a count",
            // and turning it into a zero here would put a confident 0 rows in
            // front of a `Distributed` table holding a billion.
            "select database, name, engine, as_select, create_table_query, primary_key, \
             total_rows, total_bytes, comment \
             from system.tables where not is_temporary order by database, name",
            CATALOG_LIMIT,
        )
        .await?;
    let columns = connection
        .fetch(
            "select database, table, name, position, type, default_kind, default_expression, \
             comment from system.columns order by database, table, position",
            CATALOG_LIMIT,
        )
        .await?;
    // Skipping indexes are not indexes in the Postgres sense — they prune
    // granules rather than locate rows — but they are the only per-table index
    // objects ClickHouse has, and the structure tab is where someone goes to
    // find out why a query is not being pruned.
    let indexes = connection
        .fetch(
            "select database, table, name, type_full, expr from system.data_skipping_indices \
             order by database, table, name",
            CATALOG_LIMIT,
        )
        .await?;

    let mut columns_by_table = group(&columns.rows, |set, row| {
        (
            ColumnDef {
                name: text(set, row, 2).into(),
                position: number(set, row, 3).unwrap_or(0) as i16,
                kind: types::parse(&text(set, row, 4)).kind(),
                type_name: text(set, row, 4).into(),
                // Nullability is in the type and nowhere else: ClickHouse has
                // no `NOT NULL`, it has `Nullable(T)`.
                nullable: types::parse(&text(set, row, 4)).is_nullable(),
                default: match text(set, row, 6) {
                    expression if expression.is_empty() => None,
                    // `DEFAULT`, `MATERIALIZED`, `ALIAS` and `EPHEMERAL` are
                    // four different things that all live in this one column,
                    // and the keyword is the only thing that tells them apart.
                    expression => Some(match text(set, row, 5).as_str() {
                        "DEFAULT" | "" => expression.into(),
                        kind => format!("{kind} {expression}").into(),
                    }),
                },
                // Neither concept exists here: there is no sequence to draw
                // from and no stored generated column — a `MATERIALIZED`
                // column is recorded above as the default it is.
                identity: None,
                is_generated: false,
                comment: optional(set, row, 7),
            },
            key(set, row, 0, 1),
        )
    });
    let mut indexes_by_table = group(&indexes.rows, |set, row| {
        (
            IndexDef {
                name: text(set, row, 2).into(),
                // The expression whole rather than split on commas: a skipping
                // index is over an expression, and `cityHash64(a, b)` chopped
                // at its comma is two things that are not columns.
                columns: vec![text(set, row, 4).into()],
                is_unique: false,
                is_primary: false,
                method: text(set, row, 3).into(),
                predicate: None,
            },
            key(set, row, 0, 1),
        )
    });

    let mut relations_by_database: HashMap<String, Vec<Relation>> = HashMap::new();
    for row in 0..tables.rows.as_ref().map_or(0, ResultSet::row_count) {
        let set = tables.rows.as_ref().expect("checked by the row count");
        let database = text(set, row, 0);
        let name = text(set, row, 1);
        let engine = text(set, row, 2);
        let table = key_of(&database, &name);

        let mut indexes = indexes_by_table.remove(&table).unwrap_or_default();
        // The sorting key at the front, as a primary key, because that is what
        // ClickHouse calls it and what decides whether a `where` is cheap. It
        // is *not* unique and nothing is checking it — safe to present this way
        // only because `editable_rows` is false, so nothing will try to
        // identify a row by it.
        if let Some(primary) = primary_key(&text(set, row, 5)) {
            indexes.insert(0, primary);
        }

        let kind = relation_kind(&engine);
        // The statement the table was made with, which is the DDL tab's answer
        // for every relation here — a `MergeTree` is its engine, its sorting
        // key and its settings far more than it is its column list.
        let create_statement = optional(set, row, 4);
        let definition = match kind.is_view() {
            // `as_select` is empty for a view created before ClickHouse
            // recorded it separately; the whole `create table` statement is
            // worse to read but always there.
            true => match text(set, row, 3) {
                select if select.is_empty() => optional(set, row, 4),
                select => Some(select.into()),
            },
            false => None,
        };

        relations_by_database
            .entry(database.clone())
            .or_default()
            .push(Relation {
                reference: RelationRef::new(database, name),
                kind,
                columns: columns_by_table.remove(&table).unwrap_or_default(),
                indexes,
                // ClickHouse has none of the three, at all, by design.
                foreign_keys: Vec::new(),
                checks: Vec::new(),
                triggers: Vec::new(),
                definition,
                create_statement,
                // `total_rows` is null for every engine that does not keep a
                // count — `Merge`, `Distributed`, most table functions — and
                // the query above turns that into the -1 that means unknown.
                estimated_rows: number(set, row, 6).unwrap_or(-1),
                size_bytes: number(set, row, 7).unwrap_or(0),
                comment: optional(set, row, 8),
                // Nothing is loaded lazily: the four queries above are the
                // whole catalog, which is why `paged_catalog` is false.
                detail_loaded: true,
            });
    }

    let mut schemas = Vec::new();
    for row in 0..databases.rows.as_ref().map_or(0, ResultSet::row_count) {
        let set = databases.rows.as_ref().expect("checked by the row count");
        let name = text(set, row, 0);
        schemas.push(Schema {
            is_system: is_system_database(&name),
            relations: relations_by_database.remove(&name).unwrap_or_default(),
            // ClickHouse databases have no owner and no stored routines. A
            // user-defined function is server-wide rather than per-database, so
            // putting one under a schema would be inventing a relationship.
            owner: "".into(),
            routines: Vec::new(),
            name: name.into(),
        });
    }

    let database: Arc<str> = connection.config().database.as_str().into();
    Ok(SchemaSnapshot {
        // Left empty deliberately: on this engine the databases *are* the
        // schemas above, and listing them here as well would draw each one
        // twice.
        databases: Vec::new(),
        server_version,
        search_path: vec![database.clone()],
        current_schema: database.clone(),
        database,
        schemas,
    })
}

/// The sorting key as an index, or `None` when the table has no order —
/// `Memory`, `Log`, and every view.
fn primary_key(expression: &str) -> Option<IndexDef> {
    let columns: Vec<Arc<str>> = split_top_level(expression)
        .into_iter()
        .map(|part| unquote(&part))
        .collect();
    if columns.is_empty() {
        return None;
    }
    Some(IndexDef {
        name: "primary key".into(),
        columns,
        // Emphatically not unique. ClickHouse's primary key is a sort order
        // with a sparse index over it; two rows with the same key are two rows.
        is_unique: false,
        is_primary: true,
        method: "sparse".into(),
        predicate: None,
    })
}

/// A backtick-quoted identifier, as itself.
///
/// `system.tables.primary_key` prints the key the way ClickHouse would parse
/// it back, so anything needing quotes arrives wearing backticks — and a column
/// literally called `$user_id` needs them. The quotes have to come off before
/// the name can be matched against a result set's own column names, which is
/// what decides whether a grid knows how to address a row.
///
/// Only a part that is *entirely* one quoted identifier: `toStartOfDay(t)` is
/// an expression and stays whole.
fn unquote(part: &str) -> Arc<str> {
    let inner = match part.strip_prefix('`').and_then(|p| p.strip_suffix('`')) {
        Some(inner) => inner,
        None => return part.into(),
    };
    match inner.contains('`') {
        // A doubled or escaped backtick inside means the outer pair was not
        // the whole of it; leaving it alone is worse than guessing wrong.
        true => part.into(),
        false => inner.into(),
    }
}

/// Split a key expression on the commas that separate its parts.
///
/// Depth-aware, because `(a, b)` and `toStartOfDay(t), id` are both ordinary
/// sorting keys and chopping either at every comma produces fragments that are
/// not expressions.
fn split_top_level(expression: &str) -> Vec<Arc<str>> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, ch) in expression.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                push_trimmed(&mut parts, &expression[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    push_trimmed(&mut parts, &expression[start..]);
    parts
}

fn push_trimmed(parts: &mut Vec<Arc<str>>, part: &str) {
    let part = part.trim();
    if !part.is_empty() {
        parts.push(part.into());
    }
}

fn relation_kind(engine: &str) -> RelationKind {
    match engine {
        "MaterializedView" => RelationKind::MaterializedView,
        // A live view and a window view are both views that keep themselves
        // up to date, which is the distinction this app draws for a
        // materialised one — but neither stores rows, so they read as views.
        "View" | "LiveView" | "WindowView" => RelationKind::View,
        _ => RelationKind::Table,
    }
}

/// Bucket rows by the `(database, table)` they belong to.
fn group<T>(
    rows: &Option<ResultSet>,
    build: impl Fn(&ResultSet, usize) -> (T, String),
) -> HashMap<String, Vec<T>> {
    let mut grouped: HashMap<String, Vec<T>> = HashMap::new();
    let Some(set) = rows else {
        return grouped;
    };
    for row in 0..set.row_count() {
        let (value, key) = build(set, row);
        grouped.entry(key).or_default().push(value);
    }
    grouped
}

fn key(set: &ResultSet, row: usize, database: usize, table: usize) -> String {
    key_of(&text(set, row, database), &text(set, row, table))
}

/// A key that cannot collide, because a ClickHouse identifier can contain
/// anything including a dot but not a NUL.
fn key_of(database: &str, table: &str) -> String {
    format!("{database}\0{table}")
}

fn text(set: &ResultSet, row: usize, column: usize) -> String {
    let mut scratch = String::new();
    match set.columns[column].render(row, &mut scratch) {
        CellText::Null => String::new(),
        CellText::Borrowed(text) => text.to_string(),
        CellText::Formatted => scratch,
    }
}

fn optional(set: &ResultSet, row: usize, column: usize) -> Option<Arc<str>> {
    match text(set, row, column) {
        value if value.is_empty() => None,
        value => Some(value.into()),
    }
}

fn number(set: &ResultSet, row: usize, column: usize) -> Option<i64> {
    text(set, row, column).parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sorting_key_splits_on_the_commas_that_separate_its_parts() {
        assert_eq!(
            split_top_level("toStartOfDay(t), id"),
            vec![Arc::<str>::from("toStartOfDay(t)"), "id".into()]
        );
        // The commas inside the call are the call's, not the key's.
        assert_eq!(
            split_top_level("cityHash64(a, b)"),
            vec![Arc::<str>::from("cityHash64(a, b)")]
        );
        assert!(split_top_level("").is_empty());
        assert!(split_top_level("   ").is_empty());
    }

    #[test]
    fn a_table_with_no_order_has_no_primary_key_rather_than_an_empty_one() {
        assert!(primary_key("").is_none());
        let key = primary_key("id, ts").expect("a sorting key is a primary key here");
        assert!(key.is_primary);
        // The one thing a reader must not conclude from `is_primary`.
        assert!(!key.is_unique);
        assert_eq!(key.columns.len(), 2);
    }

    #[test]
    fn a_key_column_that_needed_quoting_is_named_without_them() {
        // What a column called `$user_id` looks like coming out of `system`.
        let key = primary_key("`$user_id`, `$timestamp`").expect("a sorting key");
        assert_eq!(
            key.columns,
            vec![Arc::<str>::from("$user_id"), "$timestamp".into()]
        );
        // An expression is not an identifier and keeps every character it has.
        let key = primary_key("toStartOfDay(`t`)").expect("a sorting key");
        assert_eq!(key.columns, vec![Arc::<str>::from("toStartOfDay(`t`)")]);
    }

    #[test]
    fn the_engine_name_says_whether_a_relation_holds_rows() {
        assert_eq!(relation_kind("MergeTree"), RelationKind::Table);
        assert_eq!(relation_kind("View"), RelationKind::View);
        assert_eq!(
            relation_kind("MaterializedView"),
            RelationKind::MaterializedView
        );
        // A materialised view's storage is an ordinary table, and it is listed
        // as one.
        assert_eq!(relation_kind("ReplicatedMergeTree"), RelationKind::Table);
    }
}
