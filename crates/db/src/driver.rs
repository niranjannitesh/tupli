//! What the app is allowed to ask a database, and what it may assume back.
//!
//! The app talks to [`Driver`] and to nothing else. `db_pg` and `db_redis`
//! implement it; the registry that picks between them lives above both, in
//! `drivers`. Everything here is deliberately shaped by what the *window*
//! needs — a result set, a catalog, a cancel — rather than by what either
//! wire protocol happens to offer.
//!
//! The interesting half is [`Capabilities`]. A keyspace has no schemas, no
//! SQL and no transactions, so the honest way to support one is not to pretend
//! otherwise but to let the UI ask "can you?" before it offers something. A
//! `false` here is a menu item that is not drawn, not a call that fails.

use std::sync::Arc;

use futures::future::BoxFuture;

use crate::error::{DbError, DbResult, Notice};
use crate::keyspace::{Cursor, KeyFacts, KeyListing, KeyPage, KeyQuery, KeyType, Keyspace};
use crate::roles::{Grants, RoleSet};
use crate::schema::RelationRef;
use crate::schema::SchemaSnapshot;
use crate::value::Value;
use crate::ResultSet;

/// Rows fetched in one go before the result is called truncated.
///
/// A person cannot read more than this, and the point of a limit is that a
/// mistyped `select * from events` should not pull 400 million rows into
/// memory before anyone can react.
pub const DEFAULT_MAX_ROWS: usize = 50_000;

/// Which server is at the other end.
///
/// Saved with the connection, because it decides everything from the default
/// port to which crate opens the socket, and guessing it from the port is the
/// kind of cleverness that breaks the moment someone runs Redis on 5432.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Engine {
    #[default]
    Postgres,
    /// A file rather than a server. Everything else here is reached over a
    /// socket; this one is a path, and the difference shows up in every field
    /// of a connection.
    Sqlite,
    Redis,
    ClickHouse,
}

impl Engine {
    pub const ALL: [Engine; 4] = [Self::Postgres, Self::Sqlite, Self::Redis, Self::ClickHouse];

    /// Is the connection a file on this machine rather than a server
    /// somewhere?
    ///
    /// The one question about an engine that is not a capability, because it
    /// is not about what a connection can do — it is about what a connection
    /// *is*, and so about which fields the connection window draws. A host, a
    /// port, a user and a TLS mode are all answers to "which server", and for
    /// a file there is no such question to answer.
    pub fn is_file(self) -> bool {
        matches!(self, Self::Sqlite)
    }

    /// The stored spelling. Written to SQLite and to `TUPLI_CONNECT`, so it
    /// does not change once a version has shipped with it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Sqlite => "sqlite",
            Self::Redis => "redis",
            Self::ClickHouse => "clickhouse",
        }
    }

    /// What the connection window calls it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Postgres => "PostgreSQL",
            Self::Sqlite => "SQLite",
            Self::Redis => "Redis",
            Self::ClickHouse => "ClickHouse",
        }
    }

    /// The inverse of [`Engine::as_str`], generous about the names people
    /// actually type.
    pub fn from_str(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" | "pg" => Some(Self::Postgres),
            "sqlite" | "sqlite3" => Some(Self::Sqlite),
            "redis" => Some(Self::Redis),
            "clickhouse" | "click-house" | "ch" => Some(Self::ClickHouse),
            _ => None,
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Self::Postgres => 5432,
            // Nothing listens. Zero rather than an arbitrary number so that
            // anything printing an endpoint has a value it can recognise as
            // "there is no port here" instead of one it has to special-case.
            Self::Sqlite => 0,
            Self::Redis => 6379,
            // The native protocol. 8123 is the HTTP interface, which is a
            // different thing this app does not speak.
            Self::ClickHouse => 9000,
        }
    }

    /// What this engine's world normally does about TLS.
    ///
    /// Not a judgement about which is safer — it is what the server on the
    /// other end is overwhelmingly likely to be doing. Nearly all managed
    /// Postgres speaks TLS on 5432 and refuses anything else. Redis does the
    /// opposite: 6379 is plain, TLS is a separate port that has to be turned
    /// on, and a TLS client against a plain 6379 does not get an error — it
    /// gets a handshake that never completes and a ten-second wait for a
    /// timeout. Guessing wrong there is not a warning, it is a hang.
    pub fn default_ssl_mode(self) -> crate::SslMode {
        match self {
            Self::Postgres => crate::SslMode::Require,
            // There is no wire to encrypt.
            Self::Sqlite => crate::SslMode::Disable,
            Self::Redis => crate::SslMode::Disable,
            // ClickHouse is Redis' case exactly: 9000 is plain, TLS is 9440
            // and has to be turned on, and a TLS client against 9000 hangs
            // rather than fails.
            Self::ClickHouse => crate::SslMode::Disable,
        }
    }

    /// What a connection to this engine can be asked for. A property of the
    /// engine rather than of the session: it decides what the window draws
    /// before a socket is open, which is when the tabs and menus are built.
    pub fn capabilities(self) -> Capabilities {
        match self {
            Self::Postgres => Capabilities::POSTGRES,
            Self::Sqlite => Capabilities::SQLITE,
            Self::Redis => Capabilities::REDIS,
            Self::ClickHouse => Capabilities::CLICKHOUSE,
        }
    }
}

impl std::fmt::Display for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the console is typing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialect {
    /// SQL: highlighted, formatted, split into statements on `;`.
    Sql,
    /// A line of arguments: `hgetall user:1`. No formatter, and the splitter
    /// is the newline.
    Commands,
}

/// What a driver can be asked to do.
///
/// Everything the UI would otherwise have to know about an engine by name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities {
    pub dialect: Dialect,
    /// Relations live in named schemas, so the tree has that level and names
    /// are qualified.
    pub schemas: bool,
    /// One server holds several databases, and there is a switcher for them.
    pub databases: bool,
    /// A set of writes can be sent as one unit that either all lands or none
    /// does — which is what makes staging grid edits safe to offer.
    pub transactions: bool,
    /// A statement in flight can be stopped.
    pub cancel: bool,
    /// The server can be asked how it would run a statement.
    pub explain: bool,
    /// A row in the grid can be edited and written back by primary key.
    pub editable_rows: bool,
    /// Objects can be created and altered: the structure editor and the
    /// new-table sheet.
    pub ddl: bool,
    /// The catalog is too large to hand over at once, so the objects in it are
    /// listed a page at a time through [`Driver::list_keys`] instead of
    /// arriving inside [`Catalog`]. What makes the app draw a key browser
    /// rather than a schema tree.
    pub paged_catalog: bool,
    /// The server has named roles that objects are granted to, so there is a
    /// list of them to browse and a privileges view on every relation.
    pub roles: bool,
    /// An `insert` may say `overriding system value`, which is what lets a
    /// generated key be written back as the value it had. Without it a SQL
    /// export of any table with an identity column is a file that will not
    /// load, which is the only thing such a file is for.
    pub identity_overrides: bool,
}

impl Capabilities {
    pub const POSTGRES: Self = Self {
        dialect: Dialect::Sql,
        schemas: true,
        databases: true,
        transactions: true,
        cancel: true,
        explain: true,
        editable_rows: true,
        ddl: true,
        paged_catalog: false,
        roles: true,
        identity_overrides: true,
    };

    /// SQLite is a Postgres with the server taken out, and the `false`s below
    /// are what leaves with it.
    ///
    /// `schemas` is true and `databases` false for the same reason as
    /// ClickHouse, arrived at from the other direction: one connection is one
    /// file, so there is nothing to switch between — but `main`, `temp` and
    /// anything `ATTACH`ed are all visible at once and can be joined across,
    /// which is what a schema is.
    ///
    /// `explain` is false even though SQLite will explain a statement, because
    /// the statement is `EXPLAIN QUERY PLAN` and not `EXPLAIN`: bare `EXPLAIN`
    /// here prints virtual-machine bytecode, which is an answer to a different
    /// question than the one anybody clicking a button labelled Explain is
    /// asking.
    ///
    /// `ddl` is false because the DDL this app writes is Postgres', and
    /// SQLite's `ALTER TABLE` can rename and add and little else — an offer to
    /// change a column's type here would be an offer to produce a statement
    /// the engine refuses. Reading DDL is unaffected: every relation carries
    /// the `CREATE` that made it, because SQLite stores that text verbatim.
    ///
    /// `roles` is false because there is no server to have users on. A file's
    /// permissions are the filesystem's.
    pub const SQLITE: Self = Self {
        dialect: Dialect::Sql,
        schemas: true,
        databases: false,
        transactions: true,
        cancel: true,
        explain: false,
        editable_rows: true,
        ddl: false,
        paged_catalog: false,
        roles: false,
        identity_overrides: false,
    };

    /// Redis is the reason this struct exists. It has logical databases and
    /// nothing else on this list: `MULTI` is not a transaction the grid could
    /// stage into, a key is not a row with a primary key, and there is no
    /// statement to cancel because there is no long-running read to cancel.
    pub const REDIS: Self = Self {
        dialect: Dialect::Commands,
        schemas: false,
        databases: true,
        transactions: false,
        cancel: false,
        explain: false,
        editable_rows: false,
        ddl: false,
        paged_catalog: true,
        roles: false,
        identity_overrides: false,
    };

    /// ClickHouse is SQL with several of the assumptions underneath this app
    /// removed, and each `false` below is one of them.
    ///
    /// `databases` is false and `schemas` true because a ClickHouse session
    /// sees every database on the server and can join across them. They behave
    /// like schemas, so they are drawn as schemas — a database switcher would
    /// be a switch between things that were never apart.
    ///
    /// `transactions` is false because there are none. A multi-statement unit
    /// that either all lands or none does is not something the engine offers,
    /// so the grid must not offer to stage edits into one.
    ///
    /// `editable_rows` is false because a `MergeTree` primary key is a sort
    /// order, not an identity: two rows may share one, and an `update` is an
    /// asynchronous mutation that rewrites parts rather than a row. There is
    /// nothing to write a cell back *by*.
    ///
    /// `paged_catalog` is false because four queries against `system` bring
    /// back the whole catalog and the tree can be drawn from it at once.
    ///
    /// `roles` is false because ClickHouse's are optional and usually unused:
    /// a server configured from `users.xml` has none, and a list that is empty
    /// on most servers is a sidebar group that mostly says nothing.
    ///
    /// `identity_overrides` is false because there is no identity column to
    /// override — and the clause is Postgres syntax the parser would reject.
    pub const CLICKHOUSE: Self = Self {
        dialect: Dialect::Sql,
        schemas: true,
        databases: false,
        transactions: false,
        cancel: true,
        explain: true,
        editable_rows: false,
        ddl: true,
        paged_catalog: false,
        roles: false,
        identity_overrides: false,
    };

    pub fn is_sql(&self) -> bool {
        self.dialect == Dialect::Sql
    }
}

/// What running a statement produced.
#[derive(Debug)]
pub enum Outcome {
    /// A result set, and whether the fetch stopped early.
    Rows { rows: ResultSet, truncated: bool },
    /// A statement that returned no columns: `insert`, `update`, DDL.
    Affected(u64),
}

impl Outcome {
    pub fn row_count(&self) -> usize {
        match self {
            Self::Rows { rows, .. } => rows.row_count(),
            Self::Affected(n) => *n as usize,
        }
    }
}

/// One statement of a transaction, with what it is supposed to touch.
///
/// Borrowed rather than owned: these are built from a change set that outlives
/// the call, and a commit of ten thousand rows should not copy every parameter
/// to describe itself.
pub struct Write<'a> {
    pub sql: &'a str,
    pub params: &'a [Value],
    /// Rows this must affect, when that is known. `None` for anything whose
    /// count is not a claim — DDL, or a statement written by hand.
    pub expect_rows: Option<u64>,
}

/// What is on the server, as far as the sidebar is concerned.
///
/// Two variants rather than one shape with holes in it, because the difference
/// is not cosmetic: a SQL catalog is exhaustive and cheap, and a keyspace is
/// neither. Pretending a key is a relation would put a number in front of the
/// user that no `SCAN` can honestly produce.
pub enum Catalog {
    Sql(SchemaSnapshot),
    Keyspace(Keyspace),
}

impl Catalog {
    pub fn sql(&self) -> Option<&SchemaSnapshot> {
        match self {
            Self::Sql(snapshot) => Some(snapshot),
            Self::Keyspace(_) => None,
        }
    }

    pub fn keyspace(&self) -> Option<&Keyspace> {
        match self {
            Self::Keyspace(keyspace) => Some(keyspace),
            Self::Sql(_) => None,
        }
    }

    /// Turn a catalog that is not the expected kind into an error rather than
    /// a silent empty tree — it means a driver and a view disagree, which is a
    /// bug and should read like one.
    pub fn into_sql(self) -> DbResult<SchemaSnapshot> {
        match self {
            Self::Sql(snapshot) => Ok(snapshot),
            Self::Keyspace(_) => Err(DbError::internal(
                "this connection has a keyspace, not a schema",
            )),
        }
    }
}

/// One open connection to one server.
///
/// Async without `async fn`, because the app holds this as `Arc<dyn Driver>`
/// and a trait with `async fn` is not object-safe. The futures are boxed once
/// per call, which is nothing next to a round trip.
pub trait Driver: Send + Sync + 'static {
    fn engine(&self) -> Engine;

    /// What this connection can be asked for. Defaults to the engine's own
    /// answer; a driver overrides it when the server it reached turned out to
    /// be less than the engine promises — a replica that refuses writes, a
    /// Postgres too old to `explain`.
    fn capabilities(&self) -> Capabilities {
        self.engine().capabilities()
    }

    /// The version banner, already shortened to something a status bar can
    /// show.
    fn server_version(&self) -> Arc<str>;

    /// Whether the socket has gone. A connection that closed under us is not
    /// an error until someone asks it for something.
    fn is_closed(&self) -> bool;

    /// Anything the server said out of band since this was last called —
    /// Postgres `NOTICE`s and the like. Drained rather than accumulated, so a
    /// notice is attributed to the statement it arrived during.
    fn take_notices(&self) -> Vec<Notice> {
        Vec::new()
    }

    /// Read what is on the server: schemas and relations, or databases and key
    /// counts.
    fn catalog<'a>(&'a self) -> BoxFuture<'a, DbResult<Catalog>>;

    /// One page of the object list, for a driver whose catalog is paged.
    ///
    /// The three methods below are the other half of [`Catalog::Keyspace`]:
    /// the catalog says which databases exist and how many keys are in them,
    /// and these say what the keys are. They are on the trait rather than
    /// downcast out of it so that the browser can be written against
    /// [`Capabilities::paged_catalog`] and never against an engine name — the
    /// moment the UI asks "is this Redis?", every engine after Redis is a new
    /// branch in the same place.
    ///
    /// The default is an error and not an empty page, because an empty page is
    /// indistinguishable from an empty keyspace, and a browser that silently
    /// shows nothing is worse than one that says it asked the wrong driver.
    fn list_keys<'a>(&'a self, _query: &'a KeyQuery) -> BoxFuture<'a, DbResult<KeyListing>> {
        Box::pin(async { Err(unpaged()) })
    }

    /// What one key is: its type, size, encoding and time left. Separate from
    /// reading it because the header is drawn before the rows arrive, and a
    /// key that is a hundred-megabyte list should be described before anyone
    /// decides to open it.
    ///
    /// `Ok(None)` means the key is not there — which is an ordinary answer for
    /// something that may have expired between being listed and being clicked,
    /// not a failure.
    fn describe_key<'a>(&'a self, _key: &'a [u8]) -> BoxFuture<'a, DbResult<Option<KeyFacts>>> {
        Box::pin(async { Err(unpaged()) })
    }

    /// One page of a key's contents, as rows.
    ///
    /// `kind` is passed in rather than looked up because the caller already
    /// has it from the listing, and a round trip per page to re-ask a question
    /// already answered is a round trip for nothing.
    fn read_key<'a>(
        &'a self,
        _key: &'a [u8],
        _kind: &'a KeyType,
        _from: Option<&'a Cursor>,
        _limit: usize,
    ) -> BoxFuture<'a, DbResult<KeyPage>> {
        Box::pin(async { Err(unpaged()) })
    }

    /// Every role on the server, and which one this connection is.
    ///
    /// Read once per connection and not per tab: the list changes when
    /// somebody runs `create role`, which is not something the app should poll
    /// for. `None` from a driver whose [`Capabilities::roles`] is false, so
    /// that a caller which forgot to check gets an empty view rather than an
    /// error about a question that simply does not apply.
    fn roles<'a>(&'a self) -> BoxFuture<'a, DbResult<Option<RoleSet>>> {
        Box::pin(async { Ok(None) })
    }

    /// Who may do what to one relation, including the connected role itself.
    ///
    /// Per relation and on demand, because it is a different answer for every
    /// table and most tables are never asked about. The expensive half is
    /// [`Grants::mine`], which is a question about the caller and cannot be
    /// cached across a `set role`.
    fn grants<'a>(&'a self, _relation: &'a RelationRef) -> BoxFuture<'a, DbResult<Option<Grants>>> {
        Box::pin(async { Ok(None) })
    }

    /// Run one statement and bring back at most `max_rows` rows.
    fn query<'a>(&'a self, statement: &'a str, max_rows: usize)
        -> BoxFuture<'a, DbResult<Outcome>>;

    /// Send a set of writes as one unit. Drivers whose
    /// [`Capabilities::transactions`] is false may still implement this for a
    /// single write; the grid does not offer staged edits to them.
    fn apply<'a>(&'a self, writes: &'a [Write<'a>]) -> BoxFuture<'a, DbResult<Vec<u64>>>;

    /// Ask the server to stop whatever is running. A no-op where
    /// [`Capabilities::cancel`] is false, and best-effort everywhere: the
    /// statement may already have finished.
    fn cancel(&self) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}

/// What a driver whose catalog is not paged says when asked to page it. A
/// caller bug — the capability said so before the call was made.
fn unpaged() -> DbError {
    DbError::internal("this connection lists its objects in the catalog, not a page at a time")
}
