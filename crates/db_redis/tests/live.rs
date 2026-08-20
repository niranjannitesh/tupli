//! Tests that need a real Redis.
//!
//! Every test here skips unless `TUPLI_TEST_REDIS` names a server, so a
//! checkout with no Redis still runs `cargo test` clean. The variable holds
//! what the connection sheet would collect rather than a URL, because the
//! point is to exercise the path the app actually takes:
//!
//! ```text
//! TUPLI_TEST_REDIS='host=127.0.0.1 port=56379 db=0' \
//!     cargo test -p db_redis --test live -- --nocapture
//! ```
//!
//! There is no `seed.sql` equivalent: Redis has no schema to load, so each
//! test writes the keys it needs under its own prefix and deletes them
//! afterwards. Sharing one server between parallel tests is safe as long as
//! every test stays inside its prefix, which is what [`Fixture`] is for.

use db::{ConnectionConfig, SafetyLevel, SslMode};
use db_redis::client::{argv, RedisConnection};
use db_redis::RespValue;

fn config() -> Option<ConnectionConfig> {
    let spec = std::env::var("TUPLI_TEST_REDIS").ok()?;
    let mut config = ConnectionConfig::from_spec(&spec).expect("TUPLI_TEST_REDIS");
    // A scratch Redis on localhost speaks neither TLS nor ACLs, which is the
    // normal case for this test. `ConnectionConfig::default` fills in the
    // Postgres-shaped defaults — TLS required, the local account as the user —
    // and both have to be undone unless the spec asked for them.
    if !spec.contains("sslmode=") {
        config.ssl_mode = SslMode::Disable;
    }
    if !spec.contains("user=") {
        config.user = String::new();
    }
    Some(config)
}

/// Skip rather than fail when there is no server. A red suite on a machine
/// without Redis would train everyone to ignore it.
macro_rules! server {
    () => {
        match config() {
            Some(config) => config,
            None => {
                eprintln!("skipped: set TUPLI_TEST_REDIS to run the live tests");
                return;
            }
        }
    };
}

/// A connection and a key prefix nobody else is using.
struct Fixture {
    conn: RedisConnection,
    prefix: String,
}

impl Fixture {
    async fn open(config: ConnectionConfig, name: &str) -> Self {
        let conn = RedisConnection::connect(&config, None)
            .await
            .expect("connect");
        let fixture = Self {
            conn,
            prefix: format!("tupli:test:{name}:"),
        };
        fixture.clear().await;
        fixture
    }

    fn key(&self, name: &str) -> Vec<u8> {
        format!("{}{name}", self.prefix).into_bytes()
    }

    /// Delete everything under this test's prefix. `SCAN` and not `KEYS`, on
    /// the same principle the key browser follows.
    async fn clear(&self) {
        let pattern = format!("{}*", self.prefix).into_bytes();
        let mut cursor = b"0".to_vec();
        loop {
            let reply = self
                .conn
                .command(&[
                    b"SCAN".to_vec(),
                    cursor.clone(),
                    b"MATCH".to_vec(),
                    pattern.clone(),
                    b"COUNT".to_vec(),
                    b"1000".to_vec(),
                ])
                .await
                .expect("scan");
            let RespValue::Array(page) = reply else { panic!("scan reply: {reply:?}") };
            cursor = page[0].as_bytes().expect("cursor");
            if let RespValue::Array(keys) = &page[1] {
                for key in keys {
                    let key = key.as_bytes().expect("key");
                    self.conn
                        .command(&[b"UNLINK".to_vec(), key])
                        .await
                        .expect("unlink");
                }
            }
            if cursor == b"0" {
                break;
            }
        }
    }

    async fn send(&self, args: &[&[u8]]) -> RespValue {
        let args: Vec<Vec<u8>> = args.iter().map(|arg| arg.to_vec()).collect();
        self.conn.command(&args).await.expect("command")
    }
}

#[tokio::test]
async fn a_connection_knows_what_it_is_talking_to() {
    let config = server!();
    let fixture = Fixture::open(config, "connect").await;
    assert!(!fixture.conn.is_closed());
    assert!(!fixture.conn.server_version().is_empty());
    assert_ne!(fixture.conn.server_version().as_ref(), "unknown");
    fixture.conn.ping().await.expect("ping");
}

#[tokio::test]
async fn a_command_comes_back_as_a_typed_reply() {
    let config = server!();
    let fixture = Fixture::open(config, "reply").await;
    let key = fixture.key("string");

    fixture.send(&[b"SET", &key, b"hello"]).await;
    assert_eq!(fixture.send(&[b"GET", &key]).await, RespValue::Bulk(b"hello".to_vec()));
    assert_eq!(fixture.send(&[b"STRLEN", &key]).await, RespValue::Int(5));
    assert_eq!(fixture.send(&[b"GET", b"tupli:test:nothing"]).await, RespValue::Nil);
    assert_eq!(fixture.send(&[b"TYPE", &key]).await.as_str(), Some("string"));

    // A value that is not text at all survives the round trip.
    let binary = fixture.key("binary");
    fixture.conn
        .command(&[b"SET".to_vec(), binary.clone(), vec![0xff, 0x00, 0xfe]])
        .await
        .expect("set");
    assert_eq!(
        fixture.send(&[b"GET", &binary]).await,
        RespValue::Bulk(vec![0xff, 0x00, 0xfe])
    );
    fixture.clear().await;
}

#[tokio::test]
async fn a_failing_command_says_what_the_server_said() {
    let config = server!();
    let fixture = Fixture::open(config, "errors").await;
    let key = fixture.key("list");
    fixture.send(&[b"RPUSH", &key, b"a"]).await;

    // The classic: the right command against the wrong kind of key.
    let error = fixture
        .conn
        .command(&argv([b"GET".as_slice(), &key]))
        .await
        .expect_err("wrongtype");
    assert_eq!(error.code.as_deref(), Some("WRONGTYPE"));
    assert!(error.hint.is_some(), "{error:?}");

    // A command that does not exist is the user's typing, not the server's
    // problem, and so is not offered a retry.
    let error = fixture
        .conn
        .command(&argv([b"NOTACOMMAND".as_slice()]))
        .await
        .expect_err("unknown command");
    assert_eq!(error.class, db::ErrorClass::Syntax);
    assert!(!fixture.conn.is_closed());
    fixture.clear().await;
}

#[tokio::test]
async fn a_read_only_connection_refuses_before_the_socket() {
    let mut config = server!();
    config.safety = SafetyLevel::ReadOnly;
    let writable = Fixture::open(config.clone(), "readonly").await;
    let key = writable.key("guarded");
    // Seeded through a connection that is allowed to write, so the test can
    // prove the value is still there afterwards.
    let mut plain = config.clone();
    plain.safety = SafetyLevel::Normal;
    let seed = Fixture::open(plain, "readonly").await;
    seed.send(&[b"SET", &key, b"original"]).await;

    let error = writable
        .conn
        .command(&argv([b"SET".as_slice(), &key, b"changed"]))
        .await
        .expect_err("refused");
    assert!(error.message.contains("read-only"), "{error:?}");
    assert_eq!(seed.send(&[b"GET", &key]).await, RespValue::Bulk(b"original".to_vec()));

    // Reads still work, and so does the metadata a read-only browser needs.
    assert_eq!(writable.send(&[b"GET", &key]).await, RespValue::Bulk(b"original".to_vec()));
    seed.clear().await;
}

#[tokio::test]
async fn a_blocking_command_is_refused_whatever_the_safety_level() {
    let config = server!();
    let fixture = Fixture::open(config, "blocking").await;
    // Would hold the multiplexed connection open forever; refused here rather
    // than discovered by a hung pane.
    let error = fixture
        .conn
        .command(&argv([b"BLPOP".as_slice(), b"nothing", b"0"]))
        .await
        .expect_err("refused");
    assert!(error.message.contains("waits"), "{error:?}");
    fixture.conn.ping().await.expect("still usable");
}

#[tokio::test]
async fn a_pipeline_is_one_round_trip() {
    let config = server!();
    let fixture = Fixture::open(config, "pipeline").await;
    let keys: Vec<Vec<u8>> = (0..5).map(|n| fixture.key(&format!("k{n}"))).collect();
    let sets: Vec<Vec<Vec<u8>>> = keys
        .iter()
        .map(|key| vec![b"SET".to_vec(), key.clone(), b"v".to_vec()])
        .collect();
    let replies = fixture.conn.pipeline(&sets).await.expect("pipeline");
    assert_eq!(replies.len(), 5);

    let types: Vec<Vec<Vec<u8>>> = keys
        .iter()
        .map(|key| vec![b"TYPE".to_vec(), key.clone()])
        .collect();
    let replies = fixture.conn.pipeline(&types).await.expect("pipeline");
    assert!(replies.iter().all(|reply| reply.as_str() == Some("string")));
    fixture.clear().await;
}

/// The text a grid would draw for one cell, or `None` for a null.
fn cell(rows: &db::ResultSet, column: usize, row: usize) -> Option<String> {
    let mut scratch = String::new();
    match rows.columns[column].render(row, &mut scratch) {
        db::CellText::Null => None,
        db::CellText::Borrowed(text) => Some(text.to_string()),
        db::CellText::Formatted => Some(scratch),
    }
}

fn names(rows: &db::ResultSet) -> Vec<&str> {
    rows.columns.iter().map(|c| c.meta.name.as_str()).collect()
}

/// Every column of a page, top to bottom, as the grid would draw it.
fn column_text(rows: &db::ResultSet, column: usize) -> Vec<Option<String>> {
    (0..rows.row_count()).map(|row| cell(rows, column, row)).collect()
}

#[tokio::test]
async fn every_kind_of_key_reads_back_as_a_grid() {
    let config = server!();
    let fixture = Fixture::open(config, "kinds").await;
    let conn = &fixture.conn;
    let key = |name: &str| fixture.key(name);

    fixture.send(&[b"SET", &key("string"), b"hello"]).await;
    fixture.send(&[b"RPUSH", &key("list"), b"a", b"b", b"c"]).await;
    fixture.send(&[b"SADD", &key("set"), b"x", b"y"]).await;
    fixture.send(&[b"ZADD", &key("zset"), b"1.5", b"one", b"2", b"two"]).await;
    fixture.send(&[b"HSET", &key("hash"), b"f", b"1", b"g", b"2"]).await;
    fixture.send(&[b"XADD", &key("stream"), b"1-1", b"a", b"1"]).await;
    fixture.send(&[b"XADD", &key("stream"), b"2-1", b"b", b"2"]).await;

    // Each type is a different set of columns, and that is the whole point:
    // the pane does not have to know what it is looking at, the reader does.
    let page = |name: &'static str, kind: db_redis::KeyType| async move {
        let key = key(name);
        assert_eq!(
            db_redis::keys::type_of(conn, &key).await.expect("type"),
            Some(kind.clone()),
        );
        db_redis::keys::read(conn, &key, &kind, None, 100).await.expect("read")
    };

    let string = page("string", db_redis::KeyType::String).await;
    assert_eq!(names(&string.rows), ["value"]);
    assert_eq!(cell(&string.rows, 0, 0).as_deref(), Some("hello"));

    let list = page("list", db_redis::KeyType::List).await;
    assert_eq!(names(&list.rows), ["index", "value"]);
    assert_eq!(column_text(&list.rows, 0), vec![Some("0".into()), Some("1".into()), Some("2".into())]);
    assert_eq!(column_text(&list.rows, 1), vec![Some("a".into()), Some("b".into()), Some("c".into())]);
    assert_eq!(list.total, Some(3));
    assert!(list.more.is_none(), "the whole list fitted in one page");

    let set = page("set", db_redis::KeyType::Set).await;
    assert_eq!(names(&set.rows), ["member"]);
    let mut members = column_text(&set.rows, 0);
    members.sort();
    assert_eq!(members, vec![Some("x".into()), Some("y".into())]);

    let zset = page("zset", db_redis::KeyType::SortedSet).await;
    assert_eq!(names(&zset.rows), ["member", "score"]);
    assert_eq!(column_text(&zset.rows, 0), vec![Some("one".into()), Some("two".into())]);
    // The score is a float column, not the text the server sent: `1.5` and `2`
    // have to sort as numbers.
    assert_eq!(zset.rows.columns[1].value(0), db::Value::Float(1.5));

    let hash = page("hash", db_redis::KeyType::Hash).await;
    assert_eq!(names(&hash.rows), ["field", "value"]);
    assert_eq!(hash.rows.row_count(), 2);

    let stream = page("stream", db_redis::KeyType::Stream).await;
    // One column per field name seen in the page; the entry that lacks the
    // other's field gets a null rather than a shifted row.
    assert_eq!(names(&stream.rows), ["id", "a", "b"]);
    assert_eq!(column_text(&stream.rows, 0), vec![Some("1-1".into()), Some("2-1".into())]);
    assert_eq!(column_text(&stream.rows, 1), vec![Some("1".into()), None]);
    assert_eq!(column_text(&stream.rows, 2), vec![None, Some("2".into())]);

    // And the facts a header bar shows.
    let facts = db_redis::keys::describe(conn, &key("hash")).await.expect("describe").expect("exists");
    assert_eq!(facts.kind, db_redis::KeyType::Hash);
    assert_eq!(facts.length, Some(2));
    assert_eq!(facts.ttl, None, "no expiry set");
    assert!(facts.encoding.is_some(), "{facts:?}");
    assert!(db_redis::keys::describe(conn, b"tupli:test:missing").await.expect("describe").is_none());
    fixture.clear().await;
}

#[tokio::test]
async fn a_list_longer_than_a_page_says_where_it_stopped() {
    let config = server!();
    let fixture = Fixture::open(config, "paging").await;
    let key = fixture.key("long");
    let mut push = argv([b"RPUSH".as_slice(), &key]);
    for n in 0..25u32 {
        push.push(n.to_string().into_bytes());
    }
    fixture.conn.command(&push).await.expect("rpush");

    let first = db_redis::keys::read(&fixture.conn, &key, &db_redis::KeyType::List, None, 10)
        .await
        .expect("read");
    assert_eq!(first.rows.row_count(), 10);
    assert_eq!(first.total, Some(25));
    let more = first.more.expect("there is more");

    let second = db_redis::keys::read(&fixture.conn, &key, &db_redis::KeyType::List, Some(&more), 100)
        .await
        .expect("read");
    // Picking up where it left off, not starting again.
    assert_eq!(cell(&second.rows, 1, 0).as_deref(), Some("10"));
    assert_eq!(second.rows.row_count(), 15);
    assert!(second.more.is_none());
    fixture.clear().await;
}

#[tokio::test]
async fn the_key_browser_walks_the_keyspace_without_keys() {
    let config = server!();
    let fixture = Fixture::open(config, "scan").await;
    for n in 0..40u32 {
        fixture.send(&[b"SET", &fixture.key(&format!("s{n}")), b"v"]).await;
    }
    fixture.send(&[b"RPUSH", &fixture.key("list"), b"a"]).await;
    fixture.send(&[b"EXPIRE", &fixture.key("s0"), b"600"]).await;

    let pattern = format!("{}*", fixture.prefix);
    let mut scan = db_redis::Scan::new().matching(&pattern).count(7);
    let found = scan.take(&fixture.conn, 1000).await.expect("scan");
    // A small `COUNT` means many round trips, not fewer keys.
    assert_eq!(found.len(), 41, "{} keys", found.len());
    assert!(scan.is_done());
    assert_eq!(scan.seen(), 41);

    let listed = found.iter().find(|info| info.key == fixture.key("s0")).expect("s0");
    assert!(matches!(listed.ttl, Some(ttl) if ttl > 0 && ttl <= 600), "{:?}", listed.ttl);
    let list = found.iter().find(|info| info.key == fixture.key("list")).expect("list");
    assert_eq!(list.kind, db_redis::KeyType::List);

    // Filtering by type is the server's job where the server can do it.
    let mut lists = db_redis::Scan::new()
        .matching(&pattern)
        .of_type(&db_redis::KeyType::List);
    let only = lists.take(&fixture.conn, 1000).await.expect("scan");
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].key, fixture.key("list"));

    let rows = db_redis::scan::to_result_set(&found);
    assert_eq!(names(&rows), ["key", "type", "ttl", "size"]);
    assert_eq!(rows.row_count(), 41);
    fixture.clear().await;
}

#[tokio::test]
async fn an_edit_goes_to_the_server_and_comes_back() {
    let config = server!();
    let fixture = Fixture::open(config, "writes").await;
    let conn = &fixture.conn;
    use db_redis::write;

    let string = fixture.key("string");
    write::set(conn, &string, b"first", None).await.expect("set");
    assert_eq!(fixture.send(&[b"GET", &string]).await, RespValue::Bulk(b"first".to_vec()));
    write::set(conn, &string, b"second", Some(600)).await.expect("set");
    assert!(write::expire(conn, &string, 300).await.expect("expire"));
    assert!(write::persist(conn, &string).await.expect("persist"));
    assert_eq!(fixture.send(&[b"TTL", &string]).await, RespValue::Int(-1));
    // A `SET` with no TTL keeps whatever expiry was there, which is what makes
    // editing a cell safe on a key somebody else set an expiry on.
    fixture.send(&[b"EXPIRE", &string, b"600"]).await;
    write::set(conn, &string, b"third", None).await.expect("set");
    assert!(matches!(fixture.send(&[b"TTL", &string]).await, RespValue::Int(ttl) if ttl > 0));

    let hash = fixture.key("hash");
    assert!(write::set_field(conn, &hash, b"f", b"1").await.expect("hset"), "new field");
    assert!(!write::set_field(conn, &hash, b"f", b"2").await.expect("hset"), "existing field");
    assert!(write::remove_field(conn, &hash, b"f").await.expect("hdel"));

    let list = fixture.key("list");
    assert_eq!(write::push(conn, &list, b"b", false).await.expect("rpush"), 1);
    assert_eq!(write::push(conn, &list, b"a", true).await.expect("lpush"), 2);
    write::set_index(conn, &list, 1, b"B").await.expect("lset");
    write::remove_index(conn, &list, 0).await.expect("lrem");
    assert_eq!(fixture.send(&[b"LRANGE", &list, b"0", b"-1"]).await,
               RespValue::Array(vec![RespValue::Bulk(b"B".to_vec())]));

    let set = fixture.key("set");
    assert!(write::add_member(conn, &set, b"old").await.expect("sadd"));
    write::replace_member(conn, &set, b"old", b"new").await.expect("replace");
    let members = fixture.send(&[b"SMEMBERS", &set]).await.to_text();
    assert!(members.contains("\"new\"") && !members.contains("\"old\""), "{members}");
    assert!(write::remove_member(conn, &set, b"new", false).await.expect("srem"));

    let zset = fixture.key("zset");
    write::set_score(conn, &zset, b"m", 1.0).await.expect("zadd");
    write::rename_member(conn, &zset, b"m", b"n", 2.5).await.expect("rename member");
    // RESP3 answers `ZSCORE` with a double and RESP2 with a bulk string, which
    // is exactly the divergence `as_bytes` exists to absorb.
    let score = fixture.send(&[b"ZSCORE", &zset, b"n"]).await.as_bytes().expect("score");
    assert_eq!(String::from_utf8(score).expect("utf8"), "2.5");
    assert!(write::remove_member(conn, &zset, b"n", true).await.expect("zrem"));

    let stream = fixture.key("stream");
    let id = write::append_entry(conn, &stream, None, &[(b"a".to_vec(), b"1".to_vec())])
        .await
        .expect("xadd");
    assert!(id.contains('-'), "{id}");
    assert!(write::remove_entry(conn, &stream, &id).await.expect("xdel"));

    // Renaming refuses to clobber unless told to.
    let from = fixture.key("from");
    let onto = fixture.key("onto");
    write::set(conn, &from, b"a", None).await.expect("set");
    write::set(conn, &onto, b"b", None).await.expect("set");
    assert!(!write::rename(conn, &from, &onto, false).await.expect("renamenx"));
    assert!(write::rename(conn, &from, &onto, true).await.expect("rename"));
    assert_eq!(fixture.send(&[b"GET", &onto]).await, RespValue::Bulk(b"a".to_vec()));

    let removed = write::delete(conn, &[string, hash, list, set, zset, stream, onto])
        .await
        .expect("delete");
    // Four, not seven: a hash, a set and a sorted set that lose their last
    // element stop existing, so they were already gone. A stream does not —
    // an empty stream is still a key, and the browser has to keep showing it.
    assert_eq!(removed, 4, "the emptied collections deleted themselves");
    fixture.clear().await;
}

#[tokio::test]
async fn a_wrapped_value_survives_the_round_trip_to_the_inspector() {
    use std::io::Write as _;

    let config = server!();
    let fixture = Fixture::open(config, "decode").await;
    let conn = &fixture.conn;

    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(br#"{"name":"tupli","rows":10}"#).expect("gzip");
    let gzipped = gzip.finish().expect("gzip");
    // MessagePack for `{"a": 1}`, written out rather than generated so the test
    // fails if the decoder changes rather than if the encoder does.
    let msgpack = vec![0x81u8, 0xa1, b'a', 0x01];

    let blobs = fixture.key("blobs");
    db_redis::write::set_field(conn, &blobs, b"gz", &gzipped).await.expect("hset");
    db_redis::write::set_field(conn, &blobs, b"mp", &msgpack).await.expect("hset");

    let page = db_redis::keys::read(conn, &blobs, &db_redis::KeyType::Hash, None, 100)
        .await
        .expect("read");
    // One non-text value makes the whole value column binary, so the grid shows
    // `\xff…` rather than lossy text — and the inspector still gets real bytes.
    assert_eq!(page.rows.columns[1].meta.kind, db::ValueKind::Bytes);
    let bytes = |field: &str| -> Vec<u8> {
        let row = (0..page.rows.row_count())
            .find(|row| cell(&page.rows, 0, *row).as_deref() == Some(field))
            .expect("field");
        match page.rows.columns[1].value(row) {
            db::Value::Bytes(bytes) => bytes.to_vec(),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(bytes("gz"), gzipped);

    // What the inspector does with them: sniff, then run the chain.
    let chain = db_redis::sniff(&bytes("gz"));
    assert_eq!(chain.first(), Some(&db_redis::Decoder::Gzip));
    let decoded = db_redis::decode(&bytes("gz"), &chain).expect("decode");
    assert_eq!(decoded.form, db_redis::Form::Json);
    assert!(decoded.text.contains("\"tupli\""), "{}", decoded.text);

    let decoded = db_redis::decode(&bytes("mp"), &[db_redis::Decoder::MsgPack]).expect("decode");
    assert!(decoded.text.contains("\"a\""), "{}", decoded.text);
    // And the honest fallback for something that is none of the above.
    let decoded = db_redis::decode(&[0xff, 0x00], &[]).expect("decode");
    assert_eq!(decoded.form, db_redis::Form::Hex);
    fixture.clear().await;
}

#[tokio::test]
async fn the_server_describes_itself() {
    let config = server!();
    let fixture = Fixture::open(config, "info").await;
    let conn = &fixture.conn;

    let sections = db_redis::info::info(conn).await.expect("info");
    let server = sections
        .iter()
        .find(|section| section.name.eq_ignore_ascii_case("server"))
        .expect("a Server section");
    assert!(server.get("redis_version").is_some() || server.get("valkey_version").is_some());
    assert!(sections.len() > 3, "{} sections", sections.len());
    assert!(db_redis::info::to_result_set(&sections).row_count() > 20);

    // The sidebar's question: which of the sixteen databases has anything in
    // it. This test just put a key in one of them, so at least one says yes.
    fixture.send(&[b"SET", &fixture.key("k"), b"v"]).await;
    let databases = db_redis::info::databases(conn).await.expect("keyspace");
    assert!(!databases.is_empty(), "the database this test wrote to is empty");
    assert!(databases.iter().all(|database| database.keys > 0));

    let clients = db_redis::info::clients(conn).await.expect("client list");
    assert!(clients.row_count() >= 1);
    assert!(names(&clients).contains(&"addr"), "{:?}", names(&clients));

    // `SLOWLOG` is usually empty on a scratch server, which is a table with no
    // rows rather than an error.
    let slow = db_redis::info::slowlog(conn, 10).await.expect("slowlog");
    assert_eq!(names(&slow), ["id", "at", "microseconds", "command", "client", "name"]);

    let config = db_redis::info::config(conn, "maxmemory*").await.expect("config get");
    assert!(config.row_count() >= 1, "{} parameters", config.row_count());
    assert_eq!(names(&config), ["parameter", "value"]);
    fixture.clear().await;
}
