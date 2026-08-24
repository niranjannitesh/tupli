//! The driver against SQLite itself.
//!
//! Every other test in this crate is about a function in isolation. These open
//! a database, run statements through the [`db::Driver`] the app holds, and
//! check what comes back — which is the only way to find out whether the
//! pragmas were spelled right.

use std::sync::Arc;

use db::{ConnectionConfig, DbResult, Driver, Engine, Outcome, Value, ValueKind, Write};
use db_sqlite::SqliteConnection;
use futures::executor::block_on;

fn open(path: &str) -> DbResult<Arc<dyn Driver>> {
    let mut config = ConnectionConfig::default();
    config.engine = Engine::Sqlite;
    config.database = path.to_string();
    Ok(Arc::new(block_on(SqliteConnection::connect(
        &config, None,
    ))?))
}

/// A fresh in-memory database with `sql` already run against it.
fn with(sql: &str) -> Arc<dyn Driver> {
    let driver = open(db::MEMORY).expect("an in-memory database always opens");
    for statement in sql.split(";\n") {
        if !statement.trim().is_empty() {
            block_on(driver.query(statement, 0)).expect(statement);
        }
    }
    driver
}

fn rows(driver: &Arc<dyn Driver>, sql: &str) -> db::ResultSet {
    match block_on(driver.query(sql, 1000)).expect(sql) {
        Outcome::Rows { rows, .. } => rows,
        Outcome::Affected(n) => panic!("expected rows from {sql}, got {n} affected"),
    }
}

#[test]
fn a_path_with_nothing_at_it_is_an_error_and_not_a_new_database() {
    let path = std::env::temp_dir().join(format!("tupli-not-here-{}.db", std::process::id()));
    let error = match open(&path.to_string_lossy()) {
        Err(error) => error,
        Ok(_) => panic!("no file, no connection"),
    };
    assert_eq!(error.class, db::ErrorClass::Connection);
    assert!(!path.exists(), "a mistyped path must not create a database");
}

#[test]
fn a_statement_that_returns_no_columns_reports_what_it_changed() {
    let driver = with("CREATE TABLE t (a INTEGER)");
    let outcome = block_on(driver.query("INSERT INTO t VALUES (1), (2), (3)", 0)).expect("insert");
    assert!(matches!(outcome, Outcome::Affected(3)), "{outcome:?}");
    // DDL changes no rows, and must not inherit the count of the insert before
    // it.
    let outcome = block_on(driver.query("CREATE TABLE u (a)", 0)).expect("create");
    assert!(matches!(outcome, Outcome::Affected(0)), "{outcome:?}");
}

#[test]
fn a_fetch_stops_at_the_cap_and_says_so() {
    let driver = with("CREATE TABLE t (a INTEGER);\nINSERT INTO t VALUES (1), (2), (3)");
    let outcome = block_on(driver.query("SELECT a FROM t", 2)).expect("select");
    match outcome {
        Outcome::Rows { rows, truncated } => {
            assert_eq!(rows.row_count(), 2);
            assert!(truncated);
        }
        other => panic!("{other:?}"),
    }
    // Exactly as many rows as the cap is not a truncation.
    match block_on(driver.query("SELECT a FROM t", 3)).expect("select") {
        Outcome::Rows { rows, truncated } => {
            assert_eq!(rows.row_count(), 3);
            assert!(!truncated);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_column_is_typed_by_its_declaration_and_by_what_is_in_it() {
    let driver = with(
        "CREATE TABLE t (id INTEGER, flag BOOLEAN, when_ TIMESTAMP, blob_ BLOB);\n\
         INSERT INTO t VALUES (1, 1, '2024-01-01 00:00:00', x'00ff')",
    );
    let rows = rows(&driver, "SELECT * FROM t");
    let kinds: Vec<ValueKind> = rows.columns.iter().map(|c| c.meta.kind).collect();
    assert_eq!(
        kinds,
        vec![
            ValueKind::Int,
            ValueKind::Bool,
            ValueKind::Timestamp,
            ValueKind::Bytes
        ]
    );
    assert_eq!(rows.columns[1].value(0), Value::Bool(true));
    assert_eq!(rows.columns[3].value(0), Value::Bytes(vec![0, 255].into()));
}

#[test]
fn a_syntax_error_lands_on_the_word_that_caused_it() {
    let driver = with("CREATE TABLE t (a)");
    let error = block_on(driver.query("SELECT nope FROM t", 10)).expect_err("no such column");
    assert_eq!(error.class, db::ErrorClass::Syntax);
    assert!(error.message.contains("nope"), "{}", error.message);
    // SQLite gives the byte offset of `nope`; the editor counts from one.
    assert_eq!(error.position, Some(8));

    // A missing table is the one resolution error SQLite reports without an
    // offset. It still belongs on the statement.
    let error = block_on(driver.query("SELECT * FROM nope", 10)).expect_err("no such table");
    assert_eq!(error.class, db::ErrorClass::Syntax);
    assert_eq!(error.position, None);
}

#[test]
fn the_catalog_finds_the_tables_and_their_columns() {
    let driver = with(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL, note TEXT);\n\
         CREATE UNIQUE INDEX users_email ON users (email);\n\
         CREATE VIEW active AS SELECT id FROM users WHERE note IS NULL",
    );
    let snapshot = block_on(driver.catalog())
        .expect("a catalog")
        .into_sql()
        .expect("a sql catalog");
    assert_eq!(&*snapshot.current_schema, "main");
    let main = snapshot.schema("main").expect("main");
    let names: Vec<&str> = main.relations.iter().map(|r| &*r.reference.name).collect();
    assert_eq!(names, vec!["active", "users"]);

    let users = main.relation("users").expect("users");
    let columns: Vec<&str> = users.columns.iter().map(|c| &*c.name).collect();
    assert_eq!(columns, vec!["id", "email", "note"]);
    assert!(!users.columns[0].nullable, "a rowid alias is never null");
    assert!(!users.columns[1].nullable, "declared NOT NULL");
    assert!(users.columns[2].nullable);
    // `INTEGER PRIMARY KEY` is filled in by the engine, so an insert may leave
    // it out.
    assert!(users.columns[0].has_server_value());

    // No `CREATE INDEX` backs the primary key of a rowid table, and without a
    // synthesised one the grid would refuse to edit the rows.
    let identity = users
        .row_identity()
        .expect("something to identify a row by");
    assert!(identity.is_primary);
    assert_eq!(
        identity.columns.iter().map(|c| &**c).collect::<Vec<_>>(),
        vec!["id"]
    );
    assert!(users.indexes.iter().any(|i| &*i.name == "users_email"));

    let active = main.relation("active").expect("active");
    assert_eq!(active.kind, db::RelationKind::View);
    assert_eq!(
        active.definition.as_deref(),
        Some("SELECT id FROM users WHERE note IS NULL")
    );
}

#[test]
fn the_engines_own_tables_stay_out_of_the_tree_but_still_count_the_rows() {
    let driver = with(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT);\n         INSERT INTO users (email) VALUES ('a'), ('b'), ('c');\n         ANALYZE",
    );
    let snapshot = block_on(driver.catalog())
        .expect("a catalog")
        .into_sql()
        .expect("a sql catalog");
    let main = snapshot.schema("main").expect("main");
    let names: Vec<&str> = main.relations.iter().map(|r| &*r.reference.name).collect();
    assert_eq!(names, vec!["users"], "sqlite_stat1 is not the user's table");
    // Which is where the count came from, so hiding it must not lose it.
    assert_eq!(main.relation("users").expect("users").estimated_rows, 3);
}

#[test]
fn the_ddl_of_a_table_carries_its_indexes_and_triggers() {
    let driver = with(
        "CREATE TABLE t (a INTEGER);\n\
         CREATE INDEX t_a ON t (a);\n\
         CREATE TRIGGER t_ins AFTER INSERT ON t BEGIN SELECT 1; END",
    );
    let snapshot = block_on(driver.catalog()).unwrap().into_sql().unwrap();
    let t = snapshot.schema("main").unwrap().relation("t").unwrap();
    let ddl = sqlgen::ddl::relation(t);
    assert!(ddl.contains("CREATE TABLE t (a INTEGER)"), "{ddl}");
    assert!(ddl.contains("CREATE INDEX t_a ON t (a)"), "{ddl}");
    assert!(ddl.contains("CREATE TRIGGER t_ins"), "{ddl}");
    assert_eq!(&*t.triggers[0].timing, "AFTER INSERT");
}

#[test]
fn a_foreign_key_knows_both_ends_of_itself() {
    let driver = with(
        "CREATE TABLE parents (id INTEGER PRIMARY KEY);\n\
         CREATE TABLE kids (id INTEGER PRIMARY KEY, \
         parent INTEGER REFERENCES parents (id) ON DELETE CASCADE)",
    );
    let snapshot = block_on(driver.catalog()).unwrap().into_sql().unwrap();
    let kids = snapshot.schema("main").unwrap().relation("kids").unwrap();
    let key = &kids.foreign_keys[0];
    assert_eq!(
        key.columns.iter().map(|c| &**c).collect::<Vec<_>>(),
        vec!["parent"]
    );
    assert_eq!(&*key.target.name, "parents");
    assert_eq!(
        key.target_columns.iter().map(|c| &**c).collect::<Vec<_>>(),
        vec!["id"]
    );
    assert_eq!(key.on_delete, db::RefAction::Cascade);
}

#[test]
fn a_transaction_commits_together() {
    let driver = with("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)");
    let params = [
        vec![Value::Int(1), Value::text(ValueKind::Text, "one")],
        vec![Value::Int(2), Value::text(ValueKind::Text, "two")],
    ];
    let writes: Vec<Write<'_>> = params
        .iter()
        .map(|params| Write {
            sql: "INSERT INTO t (id, name) VALUES ($1, $2)",
            params,
            expect_rows: Some(1),
        })
        .collect();
    assert_eq!(
        block_on(driver.apply(&writes)).expect("two inserts"),
        vec![1, 1]
    );
    let rows = rows(&driver, "SELECT name FROM t ORDER BY id");
    assert_eq!(rows.row_count(), 2);
    assert_eq!(
        rows.columns[0].value(1),
        Value::text(ValueKind::Text, "two")
    );
}

#[test]
fn a_write_that_would_change_the_wrong_rows_takes_the_others_with_it() {
    let driver = with(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT);\n\
         INSERT INTO t VALUES (1, 'a'), (2, 'a')",
    );
    let first = [Value::text(ValueKind::Text, "b")];
    let writes = [
        Write {
            sql: "UPDATE t SET name = $1 WHERE id = 1",
            params: &first,
            expect_rows: Some(1),
        },
        // Two rows match, one was promised: the whole thing is rolled back.
        Write {
            sql: "DELETE FROM t",
            params: &[],
            expect_rows: Some(1),
        },
    ];
    let error = block_on(driver.apply(&writes)).expect_err("a mismatch");
    assert!(
        error.message.contains("would have changed"),
        "{}",
        error.message
    );
    let rows = rows(&driver, "SELECT name FROM t ORDER BY id");
    assert_eq!(rows.row_count(), 2);
    assert_eq!(rows.columns[0].value(0), Value::text(ValueKind::Text, "a"));
}
