//! Tests that need a real server.
//!
//! Skipped unless `TUPLI_CLICKHOUSE_HOST` is set, so `cargo test` on a machine
//! without ClickHouse stays green. A protocol this positional cannot be
//! convincingly tested against a mock of itself — the unit tests check that the
//! reader matches the layout this crate believes in, and only a real server can
//! say whether that belief is right.
//!
//! Run against a throwaway container:
//!
//! ```text
//! docker run -d --rm --name tupli-ch-test -p 19000:9000 \
//!     clickhouse/clickhouse-server:25.1-alpine
//! TUPLI_CLICKHOUSE_HOST=127.0.0.1 TUPLI_CLICKHOUSE_PORT=19000 \
//!     cargo test -p db_clickhouse --test live -- --nocapture
//! ```

use db_clickhouse::{ClickHouseConfig, ClickHouseConnection};

fn config() -> Option<ClickHouseConfig> {
    let host = std::env::var("TUPLI_CLICKHOUSE_HOST").ok()?;
    Some(ClickHouseConfig {
        host,
        port: std::env::var("TUPLI_CLICKHOUSE_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
            .unwrap_or(db_clickhouse::DEFAULT_PORT),
        database: std::env::var("TUPLI_CLICKHOUSE_DB")
            .unwrap_or_else(|_| db_clickhouse::DEFAULT_DATABASE.into()),
        ..ClickHouseConfig::default()
    })
}

/// Every live test starts the same way; `None` means the environment did not
/// ask for these tests to run.
async fn connect() -> Option<ClickHouseConnection> {
    let config = config()?;
    Some(
        ClickHouseConnection::open(
            config,
            std::env::var("TUPLI_CLICKHOUSE_PASSWORD").ok().as_deref(),
        )
        .await
        .expect("could not connect"),
    )
}

#[tokio::test]
async fn a_handshake_gets_a_version_back() {
    let Some(connection) = connect().await else {
        return;
    };
    let server = connection.server();
    println!(
        "{} {} revision {}, timezone {:?}",
        server.name,
        connection.server_version(),
        server.revision,
        server.timezone
    );
    assert!(server.major > 0, "the server reported no version");
    assert!(!connection.is_closed());
    connection.ping().await.expect("ping");
}

#[tokio::test]
async fn a_select_comes_back_with_its_values_in_the_right_columns() {
    let Some(connection) = connect().await else {
        return;
    };
    let fetched = connection
        .fetch(
            "select number, number * 2 as doubled from system.numbers limit 5",
            100,
        )
        .await
        .expect("select");
    let rows = fetched.rows.expect("no columns came back");
    assert_eq!(rows.row_count(), 5);
    assert_eq!(rows.columns[0].meta.name, "number");
    assert_eq!(rows.columns[1].meta.name, "doubled");
    assert!(!fetched.truncated);
    let mut scratch = String::new();
    assert_eq!(text(&rows, 4, 1, &mut scratch), "8");
}

/// The whole point of `max_rows`: an unbounded source has to be stopped rather
/// than drained, and the connection has to survive the stopping.
#[tokio::test]
async fn a_row_limit_cancels_the_query_and_leaves_the_connection_usable() {
    let Some(connection) = connect().await else {
        return;
    };
    let fetched = connection
        .fetch("select number from system.numbers", 10)
        .await
        .expect("limited select");
    let rows = fetched.rows.expect("no columns came back");
    assert_eq!(rows.row_count(), 10);
    assert!(
        fetched.truncated,
        "an infinite source reported no truncation"
    );

    // If the cancel left anything on the wire, this reads it as its own.
    let after = connection
        .fetch("select 'still here' as state", 10)
        .await
        .expect("the connection did not survive the cancel");
    let rows = after.rows.expect("no columns came back");
    let mut scratch = String::new();
    assert_eq!(text(&rows, 0, 0, &mut scratch), "still here");
}

/// A query that matches nothing still has to produce a grid with headers, and
/// the header block is where a reader that assumes prefixes desynchronises.
#[tokio::test]
async fn an_empty_result_still_names_its_columns() {
    let Some(connection) = connect().await else {
        return;
    };
    let fetched = connection
        .fetch(
            "select name, toLowCardinality(engine) as engine from system.tables where 0",
            100,
        )
        .await
        .expect("empty select");
    let rows = fetched.rows.expect("no columns came back");
    assert_eq!(rows.row_count(), 0);
    assert_eq!(rows.columns.len(), 2);
    connection
        .ping()
        .await
        .expect("the connection desynchronised");
}

#[tokio::test]
async fn a_bad_statement_comes_back_as_a_syntax_error_and_not_a_dead_socket() {
    let Some(connection) = connect().await else {
        return;
    };
    let error = connection
        .fetch("select from where", 100)
        .await
        .expect_err("that should not have parsed");
    assert_eq!(error.class, db::ErrorClass::Syntax, "{error:?}");
    assert!(error.code.is_some(), "no error code came back");
    assert!(!connection.is_closed());
    connection
        .ping()
        .await
        .expect("the connection desynchronised");
}

/// One row covering every type this driver claims to read, checked value by
/// value. A type that decodes to the wrong width does not fail here — it
/// shifts every column after it, which is why this asserts on all of them.
#[tokio::test]
async fn the_type_matrix_decodes_to_the_text_the_grid_shows() {
    let Some(connection) = connect().await else {
        return;
    };
    let cases: &[(&str, &str, &str)] = &[
        ("u8", "toUInt8(255)", "255"),
        ("u16", "toUInt16(65535)", "65535"),
        ("u32", "toUInt32(4294967295)", "4294967295"),
        ("u64", "toUInt64(18446744073709551615)", "18446744073709551615"),
        // Through a string, because ClickHouse's own parser turns a literal
        // this wide into a `Float64` first and loses the low digits — the
        // server really does answer `toUInt128(2^128 - 1)` with a rounded
        // number, so a fixture written the obvious way would be testing the
        // rounding rather than the decoding.
        ("u128", "CAST('340282366920938463463374607431768211455', 'UInt128')", "340282366920938463463374607431768211455"),
        ("u256", "CAST('115792089237316195423570985008687907853269984665640564039457584007913129639935', 'UInt256')", "115792089237316195423570985008687907853269984665640564039457584007913129639935"),
        ("i8", "toInt8(-128)", "-128"),
        ("i16", "toInt16(-32768)", "-32768"),
        ("i32", "toInt32(-2147483648)", "-2147483648"),
        ("i64", "toInt64(-9223372036854775808)", "-9223372036854775808"),
        ("i128", "CAST('-170141183460469231731687303715884105728', 'Int128')", "-170141183460469231731687303715884105728"),
        ("i256", "CAST('-57896044618658097711785492504343953926634992332820282019728792003956564819968', 'Int256')", "-57896044618658097711785492504343953926634992332820282019728792003956564819968"),
        ("f32", "toFloat32(0.5)", "0.5"),
        ("f64", "toFloat64(-1.25)", "-1.25"),
        ("bool", "true", "true"),
        ("str", "'héllo'", "héllo"),
        ("fixed", "toFixedString('ab', 4)", "ab"),
        ("uuid", "toUUID('61f0c404-5cb3-11e7-907b-a6006ad3dba0')", "61f0c404-5cb3-11e7-907b-a6006ad3dba0"),
        ("date", "toDate('2026-08-20')", "2026-08-20"),
        ("date32", "toDate32('1925-01-01')", "1925-01-01"),
        ("datetime", "toDateTime('2026-08-20 12:34:56', 'UTC')", "2026-08-20 12:34:56"),
        ("datetime64", "toDateTime64('2026-08-20 12:34:56.789', 3, 'UTC')", "2026-08-20 12:34:56.789"),
        ("decimal32", "toDecimal32(12.34, 2)", "12.34"),
        ("decimal64", "toDecimal64(-0.000001, 6)", "-0.000001"),
        ("decimal128", "toDecimal128('1234567890123456789.0123456789', 10)", "1234567890123456789.0123456789"),
        ("decimal256", "toDecimal256('-0.5', 1)", "-0.5"),
        ("enum", "CAST('b', 'Enum8(\\'a\\' = 1, \\'b\\' = 2)')", "b"),
        ("ipv4", "toIPv4('192.168.0.1')", "192.168.0.1"),
        ("ipv6", "toIPv6('2001:db8::1')", "2001:db8::1"),
        ("nullable_null", "CAST(NULL, 'Nullable(Int32)')", ""),
        ("nullable_set", "CAST(7, 'Nullable(Int32)')", "7"),
        ("lowcard", "toLowCardinality('warm')", "warm"),
        ("lowcard_null", "CAST(NULL, 'LowCardinality(Nullable(String))')", ""),
        // Composites are compared against what ClickHouse itself prints, down
        // to the absent spaces: a value copied out of the grid should go back
        // into the editor unchanged.
        ("array", "[1, 2, 3]", "[1,2,3]"),
        ("array_nested", "[['a'], [], ['b', 'c']]", "[['a'],[],['b','c']]"),
        ("array_nullable", "CAST([1, NULL], 'Array(Nullable(Int8))')", "[1,NULL]"),
        ("tuple", "(1, 'two')", "(1,'two')"),
        ("tuple_named", "CAST((1, 'two'), 'Tuple(n Int8, s String)')", "(1,'two')"),
        ("map", "map('a', 1)", "{'a':1}"),
        ("nothing", "CAST(NULL, 'Nullable(Nothing)')", ""),
    ];

    let statement = format!(
        "select {}",
        cases
            .iter()
            .map(|(name, expression, _)| format!("{expression} as {name}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let fetched = connection.fetch(&statement, 10).await.expect("type matrix");
    let rows = fetched.rows.expect("no columns came back");
    assert_eq!(rows.row_count(), 1);
    assert_eq!(rows.columns.len(), cases.len());

    let mut scratch = String::new();
    let mut wrong = Vec::new();
    for (index, (name, _, expected)) in cases.iter().enumerate() {
        let got = text(&rows, 0, index, &mut scratch);
        println!(
            "{name:<16} {:<24} {got}",
            rows.columns[index].meta.type_name
        );
        if got != *expected {
            wrong.push(format!("{name}: expected {expected:?}, got {got:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The columns the grid actually draws, as text.
fn text(rows: &db::ResultSet, row: usize, column: usize, scratch: &mut String) -> String {
    match rows.columns[column].render(row, scratch) {
        db::CellText::Null => String::new(),
        db::CellText::Borrowed(text) => text.to_string(),
        db::CellText::Formatted => scratch.clone(),
    }
}

/// The sidebar's whole content, against a real server.
///
/// Four queries have to line up by database and table name for this to produce
/// anything; a column joined to the wrong relation is invisible in a `SELECT`
/// test and obvious here.
#[tokio::test]
async fn the_catalog_finds_the_system_tables_and_their_columns() {
    let Some(connection) = connect().await else {
        return;
    };
    let catalog = db::Driver::catalog(&connection).await.expect("catalog");
    let db::Catalog::Sql(snapshot) = catalog else {
        panic!("ClickHouse produced a catalog that is not SQL-shaped");
    };

    let system = snapshot
        .schema("system")
        .expect("no `system` database in the catalog");
    assert!(
        system.is_system,
        "`system` was not marked as a system schema"
    );

    // `system.numbers` exists on every server ever built, has exactly one
    // column, and that column is called `number`.
    let numbers = system
        .relation("numbers")
        .expect("no `system.numbers` in the catalog");
    assert_eq!(numbers.columns.len(), 1);
    assert_eq!(&*numbers.columns[0].name, "number");
    assert_eq!(&*numbers.columns[0].type_name, "UInt64");
    assert!(!numbers.columns[0].nullable);

    // The connected database is the one the session opened, and it is a schema
    // like any other rather than a level of its own.
    assert_eq!(snapshot.current_schema, snapshot.database);
    assert!(snapshot.in_search_path(&snapshot.database));
    assert!(
        snapshot.databases.is_empty(),
        "ClickHouse databases are schemas; nothing should be listed above them"
    );
    assert!(!snapshot.server_version.is_empty());

    // A sorting key becomes an index, and system tables that have one are the
    // only place to see that without writing a table.
    let indexed = snapshot
        .relations()
        .filter(|relation| !relation.indexes.is_empty())
        .count();
    println!(
        "{} schemas, {} relations, {} with a sorting key",
        snapshot.schemas.len(),
        snapshot.relations().count(),
        indexed
    );
    assert!(snapshot.schemas.len() >= 2, "only one database came back");
}

/// The write path, which needs a server it is allowed to break.
///
/// Gated on its own variable rather than on `TUPLI_CLICKHOUSE_HOST`, because
/// the host that is convenient to test reads against is usually somebody's
/// real one. Nothing here runs unless the environment says so twice:
///
/// ```text
/// docker run -d --rm --name tupli-ch-test -p 19000:9000 \
///     clickhouse/clickhouse-server:25.1-alpine
/// TUPLI_CLICKHOUSE_HOST=127.0.0.1 TUPLI_CLICKHOUSE_PORT=19000 \
///     TUPLI_CLICKHOUSE_WRITABLE=1 cargo test -p db_clickhouse --test live
/// ```
#[tokio::test]
async fn ddl_and_an_insert_report_what_they_wrote() {
    if std::env::var("TUPLI_CLICKHOUSE_WRITABLE").is_err() {
        return;
    }
    let Some(connection) = connect().await else {
        return;
    };

    // DDL names no columns, so it has to come back as an affected count and
    // not as an empty grid.
    let created = connection
        .fetch(
            "create table if not exists tupli_probe (id UInt32, label String) \
             engine = MergeTree order by id",
            100,
        )
        .await
        .expect("create table");
    assert!(created.rows.is_none(), "DDL came back with columns");

    // `insert … values` is the one statement where the server asks the client
    // for a block back before it will finish.
    let inserted = connection
        .fetch("insert into tupli_probe values (1, 'one'), (2, 'two')", 100)
        .await
        .expect("insert values");
    assert!(inserted.rows.is_none(), "an insert came back with columns");
    assert_eq!(inserted.written_rows, 2);

    let selected = connection
        .fetch("select label from tupli_probe order by id", 100)
        .await
        .expect("select back");
    let rows = selected.rows.expect("no columns came back");
    assert_eq!(rows.row_count(), 2);
    let mut scratch = String::new();
    assert_eq!(text(&rows, 1, 0, &mut scratch), "two");

    // And the table the catalog now has to see, sorting key and all.
    let db::Catalog::Sql(snapshot) = db::Driver::catalog(&connection).await.expect("catalog")
    else {
        panic!("ClickHouse produced a catalog that is not SQL-shaped");
    };
    let probe = snapshot
        .schema(&snapshot.database)
        .and_then(|schema| schema.relation("tupli_probe"))
        .expect("the table just created is not in the catalog");
    assert_eq!(probe.columns.len(), 2);
    assert_eq!(&*probe.columns[1].name, "label");
    assert_eq!(
        probe.indexes.len(),
        1,
        "the sorting key did not become an index"
    );
    assert!(probe.indexes[0].is_primary);

    connection
        .fetch("drop table tupli_probe", 100)
        .await
        .expect("drop table");
}
