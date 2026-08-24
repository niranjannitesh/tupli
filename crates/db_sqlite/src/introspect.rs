//! The catalog, out of `sqlite_master` and the pragmas.
//!
//! One query per schema for the object list, then four pragmas per table. That
//! is more round trips than the single join the Postgres driver uses, and it
//! does not matter: the round trip is a function call, and the whole thing runs
//! inside one lock on a blocking thread.
//!
//! Split up rather than joined for a reason that does matter. A pragma against
//! a broken virtual table — an FTS index whose module is not loaded, a view over
//! a table that has since been dropped — fails, and in one big join that
//! failure is the whole tree. Here it is one relation with no columns.

use std::collections::HashMap;
use std::sync::Arc;

use db::schema::quote_ident;
use db::{
    ColumnDef, DbResult, ForeignKey, IdentityKind, IndexDef, RefAction, Relation, RelationKind,
    RelationRef, Schema, SchemaSnapshot, TriggerDef, ValueKind,
};
use rusqlite::Connection;

use crate::error::classify;
use crate::types::declared_kind;

/// What `sqlite_master` said about one object, before anything is asked about
/// it.
struct MasterRow {
    kind: String,
    name: String,
    /// The table an index or a trigger belongs to; its own name otherwise.
    table: String,
    /// The statement that created it, verbatim. Null for the indexes SQLite
    /// makes itself to back a `UNIQUE` or `PRIMARY KEY` constraint.
    sql: Option<String>,
}

pub fn snapshot(
    conn: &Connection,
    database: Arc<str>,
    version: Arc<str>,
) -> DbResult<SchemaSnapshot> {
    let mut schemas = Vec::new();
    for name in attached(conn)? {
        schemas.push(schema(conn, &name)?);
    }
    Ok(SchemaSnapshot {
        database,
        // One connection is one file. There is nothing to switch to, so an
        // empty list is the honest answer and the database picker stays away.
        databases: Vec::new(),
        server_version: version,
        // Both are reachable unqualified, and a name in `temp` shadows the
        // same name in `main` — which is the same rule as a search path, with
        // the order fixed by SQLite rather than by a setting.
        search_path: vec!["temp".into(), "main".into()],
        current_schema: "main".into(),
        schemas,
    })
}

/// `main`, `temp`, and anything `ATTACH`ed, in the order SQLite resolves them.
fn attached(conn: &Connection) -> DbResult<Vec<String>> {
    let mut stmt = conn.prepare("PRAGMA database_list").map_err(classify)?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(classify)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(classify)?;
    Ok(names)
}

fn schema(conn: &Connection, name: &str) -> DbResult<Schema> {
    let objects = master(conn, name)?;
    let stats = row_counts(conn, name, &objects)?;
    let mut relations = Vec::new();
    for object in objects
        .iter()
        .filter(|o| is_relation(&o.kind) && !is_internal(&o.name))
    {
        relations.push(relation(conn, name, object, &objects, &stats)?);
    }
    Ok(Schema {
        name: name.into(),
        // A file has no owner but the filesystem's, and putting a unix user
        // here would suggest the database knew about it.
        owner: "".into(),
        // `temp` is where SQLite keeps the tables of this session. It is the
        // user's own, not the engine's, so it is not folded away.
        is_system: false,
        relations,
        routines: Vec::new(),
    })
}

fn is_relation(kind: &str) -> bool {
    kind == "table" || kind == "view"
}

/// `sqlite_` is a reserved prefix, so everything under it is the engine's own
/// bookkeeping: the `ANALYZE` statistics, the `AUTOINCREMENT` counter, the
/// indexes it builds for itself. Nothing in the tree distinguishes a system
/// relation from a real one, and a database whose first table is
/// `sqlite_stat1` is answering a question nobody asked. They stay queryable
/// from the editor, and this crate still reads them — the row counts come out
/// of `sqlite_stat1`.
fn is_internal(name: &str) -> bool {
    name.starts_with("sqlite_")
}

fn master(conn: &Connection, schema: &str) -> DbResult<Vec<MasterRow>> {
    // The schema cannot be a bound parameter — it is part of the name of the
    // table being read — so it is quoted. It came from `database_list`, which
    // is SQLite's own answer, not from anything typed.
    let sql = format!(
        "SELECT type, name, tbl_name, sql FROM {}.sqlite_master ORDER BY name",
        quote_ident(schema)
    );
    let mut stmt = conn.prepare(&sql).map_err(classify)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(MasterRow {
                kind: row.get(0)?,
                name: row.get(1)?,
                table: row.get(2)?,
                sql: row.get(3)?,
            })
        })
        .map_err(classify)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(classify)?;
    Ok(rows)
}

/// Row counts, if somebody has run `ANALYZE`.
///
/// The only number SQLite keeps. There is no equivalent of `pg_class.reltuples`
/// that is maintained as rows arrive, so on a database that has never been
/// analysed this map is empty and every relation reports its count as unknown —
/// which is better than a `count(*)` per table on a tree that may have hundreds.
fn row_counts(
    conn: &Connection,
    schema: &str,
    objects: &[MasterRow],
) -> DbResult<HashMap<String, i64>> {
    if !objects.iter().any(|o| o.name == "sqlite_stat1") {
        return Ok(HashMap::new());
    }
    let sql = format!("SELECT tbl, stat FROM {}.sqlite_stat1", quote_ident(schema));
    let mut stmt = conn.prepare(&sql).map_err(classify)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(classify)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(classify)?;
    let mut counts = HashMap::new();
    for (table, stat) in rows {
        // `stat` is the row count and then one number per indexed column;
        // every row for a table starts with the same first number.
        if let Some(count) = stat.split(' ').next().and_then(|n| n.parse().ok()) {
            counts.insert(table, count);
        }
    }
    Ok(counts)
}

fn relation(
    conn: &Connection,
    schema: &str,
    object: &MasterRow,
    objects: &[MasterRow],
    stats: &HashMap<String, i64>,
) -> DbResult<Relation> {
    let sql = object.sql.as_deref().unwrap_or_default();
    let kind = relation_kind(&object.kind, sql);
    let columns = columns(conn, schema, &object.name, sql)?;
    let indexes = indexes(conn, schema, &object.name, objects, &columns)?;
    let triggers: Vec<TriggerDef> = objects
        .iter()
        .filter(|o| o.kind == "trigger" && o.table == object.name)
        .map(trigger)
        .collect();
    Ok(Relation {
        reference: RelationRef::new(schema, object.name.as_str()),
        kind,
        foreign_keys: foreign_keys(conn, schema, &object.name)?,
        // Not exposed by any pragma. They are in the `CREATE TABLE` below,
        // which is where the DDL tab reads them from; reconstructing them out
        // of that text would be parsing SQL to display SQL.
        checks: Vec::new(),
        definition: match kind.is_view() {
            true => view_body(sql).map(Arc::from),
            false => None,
        },
        create_statement: create_statement(sql, &object.name, objects),
        estimated_rows: stats.get(&object.name).copied().unwrap_or(-1),
        // SQLite has `dbstat`, but only in a build compiled with it. Zero is
        // read as unknown and the inspector leaves the row out.
        size_bytes: 0,
        comment: None,
        columns,
        indexes,
        triggers,
        detail_loaded: true,
    })
}

fn relation_kind(kind: &str, sql: &str) -> RelationKind {
    if kind == "view" {
        return RelationKind::View;
    }
    // A virtual table is a table whose rows are produced by a module — an FTS
    // index, a CSV file, a `dbstat`. `Foreign` is the closest thing this app
    // has, and the useful half of the resemblance is that it is not editable:
    // writing to the shadow tables of an FTS index by hand corrupts it.
    match sql.trim_start().get(..14).map(str::to_ascii_uppercase) {
        Some(head) if head == "CREATE VIRTUAL" => RelationKind::Foreign,
        _ => RelationKind::Table,
    }
}

fn columns(conn: &Connection, schema: &str, table: &str, sql: &str) -> DbResult<Vec<ColumnDef>> {
    // `table_xinfo` rather than `table_info`: the latter hides generated
    // columns, and a column missing from the structure tab is a column
    // somebody will try to insert into.
    let mut stmt = conn
        .prepare(
            "SELECT cid, name, type, \"notnull\", dflt_value, pk, hidden \
             FROM pragma_table_xinfo(?1, ?2)",
        )
        .map_err(classify)?;
    let rows = stmt
        .query_map((table, schema), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                row.get::<_, i64>(3)? != 0,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(classify)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(classify)?;

    let without_rowid = sql.to_ascii_uppercase().contains("WITHOUT ROWID");
    let single_pk = rows.iter().filter(|r| r.5 > 0).count() == 1;
    Ok(rows
        .into_iter()
        .map(|(cid, name, type_name, notnull, default, pk, hidden)| {
            // `INTEGER PRIMARY KEY` is not a column with an index on it, it is
            // another name for the rowid: SQLite fills it in when the insert
            // leaves it out, which is what identity means to the grid. The
            // spelling has to be exactly `INTEGER` — `INT PRIMARY KEY` is an
            // ordinary indexed column.
            let rowid_alias =
                pk == 1 && single_pk && !without_rowid && type_name.eq_ignore_ascii_case("INTEGER");
            ColumnDef {
                position: (cid + 1) as i16,
                kind: column_kind(&type_name),
                // A rowid alias is never null. Every other primary key column
                // of a rowid table can be, which is a real quirk of SQLite and
                // not worth hiding from somebody deciding whether to add a
                // `NOT NULL`.
                nullable: !notnull && !rowid_alias && !(pk > 0 && without_rowid),
                default: default.map(Arc::from),
                identity: rowid_alias.then_some(IdentityKind::ByDefault),
                // 2 is `GENERATED ALWAYS AS (…) VIRTUAL`, 3 is `STORED`. 1 is
                // a hidden column of a virtual table, which is not generated —
                // it is an argument.
                is_generated: hidden == 2 || hidden == 3,
                comment: None,
                name: name.into(),
                type_name: type_name.into(),
            }
        })
        .collect())
}

/// A declared type as a kind, with no rows to check it against.
///
/// The other half of [`crate::types::kind`], which reconciles this guess with
/// what actually came back. Here there is nothing to reconcile: the structure
/// tab is describing the column, not its contents.
fn column_kind(type_name: &str) -> ValueKind {
    match type_name.is_empty() {
        true => ValueKind::Unknown,
        false => declared_kind(type_name),
    }
}

fn indexes(
    conn: &Connection,
    schema: &str,
    table: &str,
    objects: &[MasterRow],
    columns: &[ColumnDef],
) -> DbResult<Vec<IndexDef>> {
    let mut list = conn
        .prepare("SELECT name, \"unique\", origin, partial FROM pragma_index_list(?1, ?2)")
        .map_err(classify)?;
    let listed = list
        .query_map((table, schema), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? != 0,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
            ))
        })
        .map_err(classify)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(classify)?;

    let mut indexes = Vec::with_capacity(listed.len());
    let mut has_primary = false;
    for (name, is_unique, origin, partial) in listed {
        has_primary |= origin == "pk";
        indexes.push(IndexDef {
            columns: index_columns(conn, schema, &name)?,
            is_unique,
            is_primary: origin == "pk",
            // SQLite has one kind of index. Saying `btree` is not a guess: it
            // is what the file format stores.
            method: "btree".into(),
            predicate: match partial {
                true => predicate(objects.iter().find(|o| o.name == name)),
                false => None,
            },
            name: name.into(),
        });
    }

    // A table keyed on `INTEGER PRIMARY KEY` has no index at all — the rowid
    // *is* the key — so `index_list` returns nothing for it and the grid would
    // decide the rows have no identity and refuse to edit them. Synthesising
    // one here is what makes the commonest table in SQLite editable.
    if !has_primary {
        let key: Vec<Arc<str>> = columns
            .iter()
            .filter(|c| c.identity.is_some())
            .map(|c| c.name.clone())
            .collect();
        if !key.is_empty() {
            indexes.insert(
                0,
                IndexDef {
                    name: "rowid".into(),
                    columns: key,
                    is_unique: true,
                    is_primary: true,
                    method: "btree".into(),
                    predicate: None,
                },
            );
        }
    }
    Ok(indexes)
}

fn index_columns(conn: &Connection, schema: &str, index: &str) -> DbResult<Vec<Arc<str>>> {
    let mut stmt = conn
        .prepare("SELECT name FROM pragma_index_info(?1, ?2) ORDER BY seqno")
        .map_err(classify)?;
    let names = stmt
        .query_map((index, schema), |row| row.get::<_, Option<String>>(0))
        .map_err(classify)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(classify)?;
    // A null name is an indexed expression rather than a column. It is left
    // out, which makes the index look narrower than it is — but a name that is
    // not a column would break every generated statement that used it.
    Ok(names.into_iter().flatten().map(Arc::from).collect())
}

/// The `WHERE` of a partial index, out of the statement that created it.
fn predicate(object: Option<&MasterRow>) -> Option<Arc<str>> {
    let sql = object?.sql.as_deref()?;
    let at = sql.to_ascii_uppercase().rfind(" WHERE ")?;
    Some(sql[at + 7..].trim().into())
}

fn foreign_keys(conn: &Connection, schema: &str, table: &str) -> DbResult<Vec<ForeignKey>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, \"table\", \"from\", \"to\", on_update, on_delete \
             FROM pragma_foreign_key_list(?1, ?2) ORDER BY id, seq",
        )
        .map_err(classify)?;
    let rows = stmt
        .query_map((table, schema), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(classify)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(classify)?;

    // One key per `id`, one row per column of it.
    let mut keys: Vec<ForeignKey> = Vec::new();
    let mut ids: Vec<i64> = Vec::new();
    for (id, target, from, to, on_update, on_delete) in rows {
        if ids.last() != Some(&id) {
            ids.push(id);
            keys.push(ForeignKey {
                // SQLite does not name foreign keys, and the structure tab
                // needs something to show. The table and the ordinal are what
                // distinguish two keys to the same target.
                name: format!("{table}_fk_{id}").into(),
                columns: Vec::new(),
                target: RelationRef::new(schema, target),
                target_columns: Vec::new(),
                on_delete: action(&on_delete),
                on_update: action(&on_update),
            });
        }
        let key = keys.last_mut().expect("pushed above");
        key.columns.push(from.into());
        // A null `to` means the target's primary key, which the pragma does
        // not spell out. Left empty rather than guessed: the app follows the
        // key by name, and a wrong name is worse than a missing one.
        if let Some(to) = to {
            key.target_columns.push(to.into());
        }
    }
    Ok(keys)
}

fn action(text: &str) -> RefAction {
    match text {
        "RESTRICT" => RefAction::Restrict,
        "CASCADE" => RefAction::Cascade,
        "SET NULL" => RefAction::SetNull,
        "SET DEFAULT" => RefAction::SetDefault,
        _ => RefAction::NoAction,
    }
}

fn trigger(object: &MasterRow) -> TriggerDef {
    let sql = object.sql.as_deref().unwrap_or_default();
    TriggerDef {
        name: object.name.as_str().into(),
        timing: timing(sql).into(),
        // A SQLite trigger has a body, not a function to call. The body is in
        // the definition, which the DDL tab prints whole.
        function: "".into(),
        // Nothing can disable one. `ALTER TABLE … DISABLE TRIGGER` is Postgres.
        enabled: true,
        definition: sql.into(),
    }
}

/// `BEFORE INSERT`, for the column that is not two hundred characters wide.
fn timing(sql: &str) -> String {
    let upper = sql.to_ascii_uppercase();
    let when = ["BEFORE", "AFTER", "INSTEAD OF"]
        .into_iter()
        .filter_map(|word| upper.find(word).map(|at| (at, word)))
        .min_by_key(|(at, _)| *at);
    let event = ["DELETE", "INSERT", "UPDATE"]
        .into_iter()
        .filter_map(|word| upper.find(word).map(|at| (at, word)))
        .min_by_key(|(at, _)| *at);
    match (when, event) {
        (Some((_, when)), Some((_, event))) => format!("{when} {event}"),
        // A trigger with no `BEFORE` or `AFTER` is `AFTER`, which is SQLite's
        // default and not worth a blank column.
        (None, Some((_, event))) => format!("AFTER {event}"),
        _ => String::new(),
    }
}

/// Everything it takes to recreate the relation, in SQLite's own words.
///
/// The DDL tab prints this verbatim when it is there rather than rebuilding the
/// statement from the parts — which is right, because SQLite stores the text
/// the user typed. The indexes and triggers are separate objects with their own
/// stored text, so they are appended: a `CREATE TABLE` alone would be a
/// recreation missing half the table's behaviour.
fn create_statement(sql: &str, table: &str, objects: &[MasterRow]) -> Option<Arc<str>> {
    if sql.is_empty() {
        return None;
    }
    let mut out = String::from(sql.trim_end().trim_end_matches(';'));
    for object in objects {
        if object.table != table || !matches!(&*object.kind, "index" | "trigger") {
            continue;
        }
        // Null for an index SQLite created itself to back a constraint, which
        // is already written out inside the `CREATE TABLE` above.
        let Some(sql) = object.sql.as_deref() else {
            continue;
        };
        out.push_str(";\n\n");
        out.push_str(sql.trim_end().trim_end_matches(';'));
    }
    Some(out.into())
}

/// The `SELECT` a view is, without the `CREATE VIEW … AS` in front of it.
///
/// The inspector shows this on its own, and sqlgen writes the `CREATE` back
/// around it. Found by scanning for the first `AS` outside quotes and
/// parentheses, because a view's name or its column list may contain one.
fn view_body(sql: &str) -> Option<&str> {
    let bytes = sql.as_bytes();
    let mut depth = 0i32;
    let mut quote = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '"' | '`' => quote = Some(c),
                '[' => quote = Some(']'),
                '(' => depth += 1,
                ')' => depth -= 1,
                'a' | 'A' if depth == 0 => {
                    let word = sql.get(i..i + 2).unwrap_or_default();
                    let before = i == 0 || !is_word(bytes[i - 1] as char);
                    let after = bytes.get(i + 2).is_none_or(|c| !is_word(*c as char));
                    if word.eq_ignore_ascii_case("as") && before && after {
                        return Some(sql[i + 2..].trim());
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_view_keeps_only_its_query() {
        assert_eq!(
            view_body("CREATE VIEW active AS SELECT * FROM users WHERE ok"),
            Some("SELECT * FROM users WHERE ok")
        );
        // The `as` in the name is not the one that matters.
        assert_eq!(
            view_body("CREATE VIEW \"as it was\" AS\nSELECT 1"),
            Some("SELECT 1")
        );
        // Nor is one inside the column list.
        assert_eq!(
            view_body("CREATE VIEW v (as_of) AS SELECT 1"),
            Some("SELECT 1")
        );
        assert_eq!(view_body("CREATE TABLE t (a int)"), None);
    }

    #[test]
    fn a_trigger_says_when_it_runs() {
        assert_eq!(
            timing("CREATE TRIGGER t AFTER INSERT ON users BEGIN SELECT 1; END"),
            "AFTER INSERT"
        );
        assert_eq!(
            timing("CREATE TRIGGER t INSTEAD OF UPDATE ON v BEGIN SELECT 1; END"),
            "INSTEAD OF UPDATE"
        );
        assert_eq!(
            timing("CREATE TRIGGER t DELETE ON users BEGIN SELECT 1; END"),
            "AFTER DELETE"
        );
    }

    #[test]
    fn a_partial_index_keeps_its_condition() {
        let object = MasterRow {
            kind: "index".into(),
            name: "i".into(),
            table: "t".into(),
            sql: Some("CREATE INDEX i ON t (a) WHERE a > 0".into()),
        };
        assert_eq!(predicate(Some(&object)).as_deref(), Some("a > 0"));
    }
}
