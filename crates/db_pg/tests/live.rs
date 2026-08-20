//! Tests that need a real PostgreSQL.
//!
//! Every test here skips unless `TUPLI_TEST_PG` names a server, so a checkout
//! with no database still runs `cargo test` clean. The variable holds the
//! pieces the app itself would collect from the connection sheet, not a libpq
//! URL, because the point is to exercise `ConnectionConfig` — the path the app
//! actually takes:
//!
//! ```text
//! TUPLI_TEST_PG='host=127.0.0.1 port=55432 db=tupli_dev user=postgres' \
//!     cargo test -p db_pg --test live -- --nocapture
//! ```
//!
//! The schema they expect is `tests/seed.sql`, next to this file — organizations,
//! users, an enum, an array column, a jsonb column, a view, a materialized view
//! in another schema, a partial index, a function, and `structure_demo`, which
//! exists to carry one of everything the structure and DDL tabs render.

use db::{ConnectionConfig, SslMode, Value, ValueKind};
use db_pg::{Outcome, PgConnection, Write};

fn config() -> Option<ConnectionConfig> {
    let spec = std::env::var("TUPLI_TEST_PG").ok()?;
    let mut config = ConnectionConfig::from_spec(&spec).expect("TUPLI_TEST_PG");
    // Nothing here is encrypted and nothing here is anyone's data; a local
    // scratch server that refuses TLS is the normal case for this test. The
    // spec can still say `sslmode=` and win.
    if !spec.contains("sslmode=") {
        config.ssl_mode = SslMode::Disable;
    }
    Some(config)
}

/// Skip rather than fail when there is no server. A red suite on a machine
/// without Postgres would train everyone to ignore it.
macro_rules! server {
    () => {
        match config() {
            Some(config) => config,
            None => {
                eprintln!("skipped: set TUPLI_TEST_PG to run the live tests");
                return;
            }
        }
    };
}

/// Serialises the tests that change the catalog against the ones that read the
/// whole of it.
///
/// Introspection describes every relation it listed, and Postgres fails the
/// query outright — `could not open relation with OID …` — if one of them is
/// dropped while it is still working through the rest. These tests share one
/// server and run in parallel, so without this the suite is red about once in
/// every few runs for a reason that has nothing to do with the code.
async fn catalog_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

async fn open() -> PgConnection {
    PgConnection::connect(&config().unwrap(), None)
        .await
        .expect("connect")
}

#[tokio::test]
async fn it_connects_and_says_what_it_is() {
    let config = server!();
    let connection = PgConnection::connect(&config, None).await.expect("connect");
    let Outcome::Rows { rows, .. } = connection.query("select version()", 10).await.unwrap() else {
        panic!("version() returns rows");
    };
    let mut scratch = String::new();
    let text = match rows.columns[0].render(0, &mut scratch) {
        db::CellText::Borrowed(s) => s.to_string(),
        db::CellText::Formatted => scratch.clone(),
        db::CellText::Null => panic!("version() is never null"),
    };
    assert!(text.starts_with("PostgreSQL"), "got {text:?}");
}

#[tokio::test]
async fn introspection_finds_the_whole_catalog() {
    let _ = server!();
    let _catalog = catalog_lock().await;
    let connection = open().await;
    let snapshot = db_pg::introspect::snapshot(&connection)
        .await
        .expect("snapshot");

    assert_eq!(snapshot.database.as_ref(), "tupli_dev");
    assert!(!snapshot.server_version.is_empty());
    assert!(
        snapshot.search_path.iter().any(|s| s.as_ref() == "public"),
        "search_path was {:?}",
        snapshot.search_path
    );
    // `search_path` carries the implicit `pg_catalog` first, which is right for
    // resolution and wrong for a breadcrumb; `current_schema` is the one that
    // answers "where am I".
    assert_eq!(
        snapshot.search_path.first().map(|s| s.as_ref()),
        Some("pg_catalog")
    );
    assert_eq!(snapshot.current_schema.as_ref(), "public");

    // Both schemas, and none of the system ones.
    let names: Vec<&str> = snapshot.schemas.iter().map(|s| s.name.as_ref()).collect();
    assert!(names.contains(&"public"), "{names:?}");
    assert!(names.contains(&"analytics"), "{names:?}");
    assert!(!names.contains(&"pg_catalog"), "{names:?}");
    assert!(!names.contains(&"information_schema"), "{names:?}");

    let public = snapshot
        .schemas
        .iter()
        .find(|s| s.name.as_ref() == "public")
        .unwrap();

    let users = public
        .relations
        .iter()
        .find(|r| r.reference.name.as_ref() == "users")
        .expect("users table");
    assert!(users.detail_loaded);
    assert_eq!(users.estimated_rows, 20_000, "analyze ran, so this is real");

    // Every column, in ordinal order, with the types the wire will send.
    let columns: Vec<&str> = users.columns.iter().map(|c| c.name.as_ref()).collect();
    assert_eq!(
        columns,
        [
            "id",
            "email",
            "full_name",
            "organization_id",
            "plan",
            "mrr_cents",
            "is_active",
            "tags",
            "settings",
            "score",
            "created_at"
        ]
    );

    let id = &users.columns[0];
    assert!(!id.nullable);
    assert_eq!(
        id.identity,
        Some(db::IdentityKind::Always),
        "id is GENERATED ALWAYS AS IDENTITY, and always is not by default"
    );
    assert_eq!(
        users.primary_key().map(|i| i.columns.as_slice()),
        Some(["id".into()].as_slice()),
        "the primary key is the index that says it is"
    );
    let full_name = &users.columns[2];
    assert!(full_name.nullable, "full_name has no not-null constraint");

    // The partial index keeps its predicate, which is the thing the standard
    // views cannot tell us.
    let partial = users
        .indexes
        .iter()
        .find(|i| i.name.as_ref() == "users_active")
        .expect("users_active index");
    assert!(partial.predicate.is_some(), "a partial index says so");
    assert!(!partial.is_primary);

    let fk = users
        .foreign_keys
        .iter()
        .find(|f| f.columns.iter().any(|c| c.as_ref() == "organization_id"))
        .expect("the organization foreign key");
    assert_eq!(fk.target.name.as_ref(), "organizations");

    // The view and the materialized view land in the right schemas as the
    // right kinds.
    assert!(public
        .relations
        .iter()
        .any(|r| r.reference.name.as_ref() == "active_users" && r.kind == db::RelationKind::View));
    let analytics = snapshot
        .schemas
        .iter()
        .find(|s| s.name.as_ref() == "analytics")
        .unwrap();
    assert!(analytics
        .relations
        .iter()
        .any(|r| r.reference.name.as_ref() == "plan_totals"
            && r.kind == db::RelationKind::MaterializedView));

    assert!(
        public.routines.iter().any(|r| r.name.as_ref() == "mrr_for"),
        "the function is in the catalog"
    );
    // And under the schema that actually owns it. `analytics` sorts first and
    // holds no functions at all, so a routine misfiled by position rather than
    // by name lands here.
    assert!(
        analytics.routines.is_empty(),
        "a schema with no functions of its own has none attached to it"
    );
    for schema in &snapshot.schemas {
        for routine in &schema.routines {
            assert_eq!(
                routine.schema, schema.name,
                "every routine sits in its own schema"
            );
        }
        for relation in &schema.relations {
            assert_eq!(
                relation.reference.schema, schema.name,
                "every relation sits in its own schema"
            );
        }
    }
}

#[tokio::test]
async fn the_decoder_survives_the_types_a_real_table_uses() {
    let _ = server!();
    let connection = open().await;
    let Outcome::Rows { rows, truncated } = connection
        .query(
            "select id, email, plan, tags, settings, score, is_active, created_at, \
             organization_id from users order by id limit 100",
            1000,
        )
        .await
        .expect("query")
    else {
        panic!("a select returns rows");
    };

    assert!(!truncated);
    assert_eq!(rows.row_count(), 100);
    assert_eq!(rows.column_count(), 9);

    // Every cell renders without panicking, which is the whole claim: the
    // binary wire decoder handles an enum, a text array, jsonb, a numeric, a
    // timestamptz and a uuid.
    let mut scratch = String::new();
    for row in 0..rows.row_count() {
        for column in &rows.columns {
            let _ = column.render(row, &mut scratch);
        }
    }

    let plan = &rows.columns[2];
    let mut seen = std::collections::HashSet::new();
    for row in 0..rows.row_count() {
        if let db::CellText::Borrowed(text) = plan.render(row, &mut scratch) {
            seen.insert(text.to_string());
        }
    }
    assert_eq!(
        seen,
        ["free", "team", "pro"]
            .map(String::from)
            .into_iter()
            .collect(),
        "the enum decodes to its labels"
    );

    // Every eleventh score is null, and the null mask has to know it.
    let score = &rows.columns[5];
    assert!(
        (0..rows.row_count()).any(|r| matches!(score.render(r, &mut scratch), db::CellText::Null)),
        "nulls come back as nulls, not as empty strings"
    );
}

#[tokio::test]
async fn the_row_cap_reports_that_it_stopped_early() {
    let _ = server!();
    let connection = open().await;
    let Outcome::Rows { rows, truncated } =
        connection.query("select * from users", 500).await.unwrap()
    else {
        panic!("a select returns rows");
    };
    assert_eq!(rows.row_count(), 500);
    assert!(truncated, "20000 rows do not fit in 500");
}

#[tokio::test]
async fn a_statement_with_no_columns_reports_what_it_changed() {
    let _ = server!();
    let connection = open().await;
    connection
        .execute("create temp table scratch (id int)")
        .await
        .unwrap();
    let affected = connection
        .execute("insert into scratch select generate_series(1, 7)")
        .await
        .unwrap();
    assert_eq!(affected, 7);
}

#[tokio::test]
async fn a_bad_statement_comes_back_with_the_servers_own_words() {
    let _ = server!();
    let connection = open().await;
    let error = connection
        .query("select nonexistent from users", 10)
        .await
        .expect_err("this cannot succeed");

    assert!(
        error.message.contains("nonexistent"),
        "message was {:?}",
        error.message
    );
    assert_eq!(error.code.as_deref(), Some("42703"), "undefined_column");
    assert!(
        error.position.is_some(),
        "the editor needs somewhere to put the caret"
    );
}

#[tokio::test]
async fn a_running_statement_can_be_cancelled() {
    let _ = server!();
    let connection = open().await;
    let canceller = connection.canceller();

    // Long enough that the cancel cannot be racing the statement's own end,
    // short enough that a broken cancel fails the test in seconds rather than
    // hanging the suite.
    let running = tokio::spawn(async move { connection.query("select pg_sleep(30)", 10).await });
    // The request has to reach a backend that is already running the sleep;
    // sent too early it finds an idle connection and does nothing.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    canceller.cancel().await;

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), running)
        .await
        .expect("the cancelled statement should return promptly")
        .expect("the query task should not panic");
    let error = outcome.expect_err("a cancelled statement does not return rows");
    assert!(
        error.is_canceled(),
        "expected a cancellation, got {:?} ({:?})",
        error.class,
        error.message
    );
    assert_eq!(error.code.as_deref(), Some("57014"), "query_canceled");
}

/// A scratch table of its own per test, dropped afterwards, so these can run
/// against a database somebody is also using by hand.
async fn scratch(connection: &PgConnection, name: &str) {
    connection
        .execute(&format!("drop table if exists {name}"))
        .await
        .expect("drop");
    connection
        .execute(&format!(
            "create table {name} (id serial primary key, email text unique, note text)"
        ))
        .await
        .expect("create");
}

#[tokio::test]
async fn a_commit_applies_every_write_or_none_of_them() {
    let _ = server!();
    let _catalog = catalog_lock().await;
    let connection = open().await;
    scratch(&connection, "tupli_apply").await;

    let one = Value::text(ValueKind::Text, "one@example.com");
    let two = Value::text(ValueKind::Text, "two@example.com");
    let inserted = connection
        .apply(&[
            Write {
                sql: "INSERT INTO tupli_apply (email) VALUES ($1)",
                params: std::slice::from_ref(&one),
                expect_rows: Some(1),
            },
            Write {
                sql: "INSERT INTO tupli_apply (email) VALUES ($1)",
                params: std::slice::from_ref(&two),
                expect_rows: Some(1),
            },
        ])
        .await
        .expect("two inserts");
    assert_eq!(inserted, vec![1, 1]);

    // The second write violates the unique index, so the first must not
    // survive either.
    let three = Value::text(ValueKind::Text, "three@example.com");
    let error = connection
        .apply(&[
            Write {
                sql: "INSERT INTO tupli_apply (email) VALUES ($1)",
                params: std::slice::from_ref(&three),
                expect_rows: Some(1),
            },
            Write {
                sql: "INSERT INTO tupli_apply (email) VALUES ($1)",
                params: std::slice::from_ref(&one),
                expect_rows: Some(1),
            },
        ])
        .await
        .expect_err("the unique index refuses the second");
    assert_eq!(error.code.as_deref(), Some("23505"));

    let count = connection
        .scalar("select count(*) from tupli_apply")
        .await
        .expect("count");
    assert_eq!(count, "2", "the rolled-back insert left nothing behind");

    connection
        .execute("drop table tupli_apply")
        .await
        .expect("drop");
}

#[tokio::test]
async fn a_row_that_moved_underneath_the_grid_stops_the_commit() {
    let _ = server!();
    let _catalog = catalog_lock().await;
    let connection = open().await;
    scratch(&connection, "tupli_stale").await;
    connection
        .execute("insert into tupli_stale (id, email) values (1, 'a@example.com')")
        .await
        .expect("seed");

    // The guard the grid generates when the value it is showing is stale.
    let params = vec![
        Value::text(ValueKind::Text, "b@example.com"),
        Value::Int(1),
        Value::text(ValueKind::Text, "stale@example.com"),
    ];
    let error = connection
        .apply(&[Write {
            sql: "UPDATE tupli_stale SET email = $1 WHERE id = $2 \
                  AND email IS NOT DISTINCT FROM $3",
            params: &params,
            expect_rows: Some(1),
        }])
        .await
        .expect_err("nothing matched the guard");
    assert!(
        error.message.contains("changed underneath"),
        "{}",
        error.message
    );

    let email = connection
        .scalar("select email from tupli_stale where id = 1")
        .await
        .expect("read back");
    assert_eq!(email, "a@example.com");

    connection
        .execute("drop table tupli_stale")
        .await
        .expect("drop");
}

#[tokio::test]
async fn a_null_parameter_is_not_the_string_null() {
    let _ = server!();
    let _catalog = catalog_lock().await;
    let connection = open().await;
    scratch(&connection, "tupli_nulls").await;

    let params = vec![Value::Null, Value::text(ValueKind::Text, "")];
    connection
        .apply(&[Write {
            sql: "INSERT INTO tupli_nulls (email, note) VALUES ($1, $2)",
            params: &params,
            expect_rows: Some(1),
        }])
        .await
        .expect("insert");

    let answer = connection
        .scalar("select (email is null) || \',\' || (note = \'\') from tupli_nulls")
        .await
        .expect("read back");
    assert_eq!(answer, "true,true");

    connection
        .execute("drop table tupli_nulls")
        .await
        .expect("drop");
}

/// What `structure_demo` is for: the four things introspection learned to read
/// for the DDL tab, none of which any other object in the seed has.
#[tokio::test]
async fn introspection_reads_checks_triggers_and_view_bodies() {
    let _ = server!();
    let _catalog = catalog_lock().await;
    let connection = open().await;
    let snapshot = db_pg::introspect::snapshot(&connection)
        .await
        .expect("snapshot");

    let demo = snapshot
        .relation(&db::RelationRef::new("public", "structure_demo"))
        .expect("structure_demo");

    let mut checks: Vec<&str> = demo.checks.iter().map(|c| c.name.as_ref()).collect();
    checks.sort();
    assert_eq!(
        checks,
        [
            "structure_demo_amount_positive",
            "structure_demo_label_present"
        ]
    );
    let positive = demo
        .checks
        .iter()
        .find(|c| c.name.as_ref() == "structure_demo_amount_positive")
        .unwrap();
    // The server's own printing, which is what the DDL tab sends back.
    assert!(
        positive.definition.starts_with("CHECK "),
        "got {:?}",
        positive.definition
    );

    // Two triggers, and neither of the two the foreign key created behind them.
    let mut triggers: Vec<&str> = demo.triggers.iter().map(|t| t.name.as_ref()).collect();
    triggers.sort();
    assert_eq!(triggers, ["structure_demo_audit", "structure_demo_touch"]);
    let touch = demo
        .triggers
        .iter()
        .find(|t| t.name.as_ref() == "structure_demo_touch")
        .unwrap();
    assert_eq!(touch.timing.as_ref(), "BEFORE UPDATE");
    assert_eq!(touch.function.as_ref(), "public.structure_demo_touch");
    assert!(touch.enabled);
    let audit = demo
        .triggers
        .iter()
        .find(|t| t.name.as_ref() == "structure_demo_audit")
        .unwrap();
    assert!(!audit.enabled, "it was disabled in the seed");

    // The generated column keeps its expression in the default slot, and is
    // told apart from a default by its flag rather than by guessing.
    let dollars = demo.column("amount_dollars").expect("amount_dollars");
    assert!(dollars.is_generated);
    assert!(dollars.default.is_some());
    assert!(!dollars.is_identity());
    assert_eq!(
        demo.column("id").unwrap().identity,
        Some(db::IdentityKind::Always)
    );

    assert_eq!(
        demo.comment.as_deref(),
        Some("every DDL feature the tab renders")
    );
    assert_eq!(
        demo.column("label").unwrap().comment.as_deref(),
        Some("not empty; see the check")
    );

    // A table has no body; a view is nothing but one.
    assert!(demo.definition.is_none());
    let view = snapshot
        .relation(&db::RelationRef::new("public", "active_users"))
        .expect("active_users");
    assert!(
        view.definition
            .as_deref()
            .unwrap_or_default()
            .contains("is_active"),
        "the view body came back: {:?}",
        view.definition
    );
}

/// The DDL tab's output, run.
///
/// A generator can be self-consistently wrong forever — the only test that
/// catches a missing `NOT NULL` or a mis-spelled `GENERATED` is the server's
/// parser. So: render `structure_demo`, run the render into an empty schema,
/// introspect the copy, and render that. The two texts differ only by the
/// schema name, or the generator lost something on the way through.
#[tokio::test]
async fn generated_ddl_recreates_the_table_it_came_from() {
    let _ = server!();
    let _catalog = catalog_lock().await;
    let connection = open().await;
    connection
        .execute("drop schema if exists tupli_ddl cascade")
        .await
        .expect("clean");
    connection
        .execute("create schema tupli_ddl")
        .await
        .expect("schema");

    let before = db_pg::introspect::snapshot(&connection)
        .await
        .expect("first");
    let source = before
        .relation(&db::RelationRef::new("public", "structure_demo"))
        .expect("structure_demo");
    let ddl = sqlgen::ddl::relation(source);
    let moved = ddl.replace("public.structure_demo", "tupli_ddl.structure_demo");

    // One statement at a time: the extended protocol prepares exactly one, and
    // nothing this generator emits contains a semicolon of its own.
    for statement in moved.split_inclusive(";\n") {
        if statement.trim().is_empty() {
            continue;
        }
        connection
            .execute(statement)
            .await
            .unwrap_or_else(|e| panic!("{statement}\n{}", e.message));
    }

    let after = db_pg::introspect::snapshot(&connection)
        .await
        .expect("second");
    let copy = after
        .relation(&db::RelationRef::new("tupli_ddl", "structure_demo"))
        .expect("the copy");
    assert_eq!(
        sqlgen::ddl::relation(copy),
        moved,
        "the copy renders back to what built it"
    );

    connection
        .execute("drop schema tupli_ddl cascade")
        .await
        .expect("drop");
}

/// The object statements, against a server that has an opinion about them.
///
/// Generating `TRUNCATE … RESTART IDENTITY` correctly is not the same as it
/// meaning what the sheet said it means, and the only way to know the sequence
/// really went back to 1 is to ask for the next id afterwards.
#[tokio::test]
async fn rename_truncate_and_drop_do_what_the_sheet_said() {
    let _ = server!();
    let _catalog = catalog_lock().await;
    let connection = open().await;
    scratch(&connection, "public.tupli_objects").await;
    connection
        .execute("drop table if exists public.tupli_objects_renamed")
        .await
        .expect("clean");
    connection
        .execute(
            "insert into public.tupli_objects (email) values ('a@example.com'), ('b@example.com')",
        )
        .await
        .expect("rows");

    let before = db::RelationRef::new("public", "tupli_objects");
    let after = db::RelationRef::new("public", "tupli_objects_renamed");

    connection
        .execute(&sqlgen::ddl::rename(
            &before,
            db::RelationKind::Table,
            "tupli_objects_renamed",
        ))
        .await
        .expect("rename");
    let snapshot = db_pg::introspect::snapshot(&connection)
        .await
        .expect("after the rename");
    assert!(snapshot.relation(&before).is_none(), "the old name is gone");
    assert!(
        snapshot.relation(&after).is_some(),
        "the new name is in the catalog"
    );

    // Truncate, restarting identity: the rows go, and so does the sequence's
    // memory of them.
    connection
        .execute(&sqlgen::ddl::truncate(&after, true, false))
        .await
        .expect("truncate");
    let Outcome::Rows { rows, .. } = connection
        .query(
            "select count(*)::bigint from public.tupli_objects_renamed",
            1000,
        )
        .await
        .expect("count")
    else {
        panic!("a select returns rows");
    };
    assert_eq!(
        rows.columns[0].value(0).to_string(),
        "0",
        "every row is gone"
    );
    connection
        .execute("insert into public.tupli_objects_renamed (email) values ('c@example.com')")
        .await
        .expect("one more");
    let Outcome::Rows { rows, .. } = connection
        .query("select id from public.tupli_objects_renamed", 1000)
        .await
        .expect("id")
    else {
        panic!("a select returns rows");
    };
    assert_eq!(
        rows.columns[0].value(0).to_string(),
        "1",
        "the sequence started again"
    );

    connection
        .execute(&sqlgen::ddl::drop_object(
            &after,
            db::RelationKind::Table,
            false,
        ))
        .await
        .expect("drop");
    let snapshot = db_pg::introspect::snapshot(&connection)
        .await
        .expect("after the drop");
    assert!(
        snapshot.relation(&after).is_none(),
        "the table is gone from the catalog"
    );
}

/// What the structure editor sends, run against a server that has to accept it.
///
/// The generator's own tests say what the text looks like; only Postgres can
/// say whether a rename, a retype, a new `NOT NULL` column and a moved primary
/// key can all be sent one after another against a table with a row in it. The
/// table is built by the same code path the editor uses — `TableDraft::blank`,
/// then `create` — so a `CREATE TABLE` the server would reject fails here and
/// not in front of somebody's schema.
#[tokio::test]
async fn a_created_table_can_be_altered_into_a_different_shape() {
    let _ = server!();
    let _catalog = catalog_lock().await;
    let connection = open().await;
    for name in ["public.tupli_draft", "public.tupli_draft_renamed"] {
        connection
            .execute(&format!("drop table if exists {name}"))
            .await
            .expect("clean");
    }

    let mut draft = sqlgen::TableDraft::blank("public");
    draft.name = "tupli_draft".into();
    draft.comment = "What the editor made.".into();
    draft.columns.push(sqlgen::ColumnDraft {
        name: "email".into(),
        type_name: "text".into(),
        nullable: false,
        comment: "Where to write.".into(),
        ..sqlgen::ColumnDraft::new()
    });
    draft.columns.push(sqlgen::ColumnDraft {
        name: "note".into(),
        type_name: "text".into(),
        ..sqlgen::ColumnDraft::new()
    });
    draft.columns.push(sqlgen::ColumnDraft {
        name: "seen_at".into(),
        type_name: "timestamp with time zone".into(),
        default: "now()".into(),
        ..sqlgen::ColumnDraft::new()
    });
    assert!(
        sqlgen::table::problems(&draft).is_empty(),
        "the draft is complete"
    );

    for statement in sqlgen::table::create(&draft) {
        connection
            .execute(&statement)
            .await
            .unwrap_or_else(|e| panic!("{statement}\n{}", e.message));
    }

    let reference = db::RelationRef::new("public", "tupli_draft");
    let snapshot = db_pg::introspect::snapshot(&connection)
        .await
        .expect("after the create");
    let relation = snapshot.relation(&reference).expect("the new table");
    assert_eq!(
        relation
            .columns
            .iter()
            .map(|c| &*c.name)
            .collect::<Vec<_>>(),
        ["id", "email", "note", "seen_at"],
        "the columns are in the order they were typed"
    );
    assert_eq!(relation.comment.as_deref(), Some("What the editor made."));
    assert!(
        relation.column("id").expect("id").is_identity(),
        "the key column the blank draft comes with is an identity"
    );
    assert_eq!(
        relation.primary_key().expect("a key").columns,
        vec!["id".into()]
    );
    let email = relation.column("email").expect("email");
    assert!(!email.nullable, "NOT NULL survived the round trip");
    assert_eq!(email.comment.as_deref(), Some("Where to write."));
    assert!(
        relation
            .column("seen_at")
            .expect("seen_at")
            .default
            .as_deref()
            .unwrap_or("")
            .contains("now()"),
        "the default is the expression that was typed"
    );

    // A row, so that every ALTER below runs against data rather than against an
    // empty shape — that is where `SET NOT NULL` and a type change get their
    // opinions.
    connection
        .execute("insert into public.tupli_draft (email, note) values ('a@example.com', 'hello')")
        .await
        .expect("a row");

    let before = sqlgen::TableDraft::of(relation);
    let mut after = before.clone();
    // One of everything the editor can do: a rename, a retype, a nullability
    // change, a new default, a dropped column, an added one, a moved key, and
    // the table's own name and comment.
    let note = after
        .columns
        .iter_mut()
        .find(|c| c.name == "note")
        .expect("note");
    note.name = "memo".into();
    note.type_name = "character varying(64)".into();
    note.nullable = false;
    note.default = "''".into();
    note.comment = "Anything else.".into();
    after.columns.retain(|c| c.name != "seen_at");
    after.columns.push(sqlgen::ColumnDraft {
        name: "qty".into(),
        type_name: "integer".into(),
        nullable: false,
        default: "0".into(),
        ..sqlgen::ColumnDraft::new()
    });
    for column in &mut after.columns {
        column.is_pk = column.name == "email";
    }
    after.comment = "Altered.".into();
    after.name = "tupli_draft_renamed".into();

    // Through `apply`, which is the door the editor's Save uses: one
    // transaction for the whole batch, DDL and all. Postgres commits schema
    // changes transactionally, and this is the test that says so.
    let statements = sqlgen::table::alter(&before, &after);
    let writes: Vec<Write> = statements
        .iter()
        .map(|sql| Write {
            sql,
            params: &[],
            expect_rows: None,
        })
        .collect();
    connection
        .apply(&writes)
        .await
        .unwrap_or_else(|e| panic!("{}\n{}", statements.join(";\n"), e.message));

    let renamed = db::RelationRef::new("public", "tupli_draft_renamed");
    let snapshot = db_pg::introspect::snapshot(&connection)
        .await
        .expect("after the alter");
    assert!(
        snapshot.relation(&reference).is_none(),
        "the old name is gone"
    );
    let relation = snapshot.relation(&renamed).expect("the renamed table");
    assert_eq!(
        relation
            .columns
            .iter()
            .map(|c| &*c.name)
            .collect::<Vec<_>>(),
        ["id", "email", "memo", "qty"],
        "renamed, dropped and added, and the rest where they were"
    );
    assert_eq!(relation.comment.as_deref(), Some("Altered."));
    let memo = relation.column("memo").expect("memo");
    assert!(
        memo.type_name.starts_with("character varying"),
        "the type changed: {}",
        memo.type_name
    );
    assert!(!memo.nullable, "and so did the nullability");
    assert!(
        memo.default.as_deref().unwrap_or("").starts_with("''"),
        "the default is the one that was typed: {:?}",
        memo.default
    );
    assert_eq!(memo.comment.as_deref(), Some("Anything else."));
    assert_eq!(
        relation.primary_key().expect("a key").columns,
        vec!["email".into()],
        "the key moved"
    );
    // The row that was here the whole time still is, and the column added with
    // a default filled itself in for it.
    let Outcome::Rows { rows, .. } = connection
        .query("select qty, memo from public.tupli_draft_renamed", 1000)
        .await
        .expect("the row")
    else {
        panic!("a select returns rows");
    };
    assert_eq!(
        rows.row_count(),
        1,
        "nothing was rewritten out of existence"
    );
    assert_eq!(rows.columns[0].value(0).to_string(), "0");
    assert_eq!(rows.columns[1].value(0).to_string(), "hello");

    // A second save with nothing changed says nothing.
    let now = sqlgen::TableDraft::of(relation);
    assert!(
        sqlgen::table::alter(&now, &now).is_empty(),
        "the table the server describes is the table the draft asked for"
    );

    connection
        .execute("drop table public.tupli_draft_renamed")
        .await
        .expect("drop");
}

#[tokio::test]
async fn what_the_server_says_on_the_side_comes_back_with_the_statement() {
    let _ = server!();
    let connection = open().await;

    // A `DO` block that only raises: nothing is written, nothing is created,
    // and the two messages arrive interleaved with the command completion the
    // way a migration's would.
    connection
        .query(
            "do $$ begin \
               raise notice 'first, at notice level'; \
               raise warning 'second, at warning level'; \
             end $$",
            1,
        )
        .await
        .expect("a do block that only raises always succeeds");

    let notices = connection.take_notices();
    assert_eq!(notices.len(), 2, "got {notices:?}");
    assert_eq!(&*notices[0].severity, "NOTICE");
    assert_eq!(&*notices[0].message, "first, at notice level");
    assert!(!notices[0].is_warning());
    assert_eq!(&*notices[1].severity, "WARNING");
    assert!(notices[1].is_warning());

    // Drained, not read: the next statement must not inherit them.
    assert!(connection.take_notices().is_empty());
}

#[tokio::test]
async fn a_quiet_statement_brings_back_nothing() {
    let _ = server!();
    let connection = open().await;
    connection.query("select 1", 1).await.expect("select 1");
    // Including the session setup `connect` does, which is its own business.
    assert!(connection.take_notices().is_empty());
}
