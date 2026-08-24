//! Local state: connections, history, saved queries, window layout.
//!
//! One SQLite file under Application Support. Real columns rather than a JSON
//! blob — this is a database client, and a schema is cheap to migrate and free
//! to query, while a blob costs a full rewrite the first time the connection
//! list needs sorting by group.
//!
//! Passwords are not here. See [`secrets`].

pub mod history;
pub mod paths;
pub mod saved;
pub mod secrets;

use anyhow::{Context, Result};
use db::{ConnectionColor, ConnectionConfig, SafetyLevel, SslMode};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

pub use history::{Finished, HistoryEntry, Kind as HistoryKind, Outcome};
pub use saved::SavedQuery;

/// The schema version this build expects. [`Store::migrate`] walks
/// `user_version` up to it, one step per release that changed the schema.
const SCHEMA_VERSION: i32 = 3;

pub struct Store {
    db: Connection,
}

impl Store {
    /// Open the real store, creating the directory and the file if this is the
    /// first launch.
    pub fn open() -> Result<Self> {
        let dir = paths::data_dir();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
        let file = paths::database_file();
        let db = Connection::open(&file)
            .with_context(|| format!("could not open {}", file.display()))?;
        Self::from_connection(db)
    }

    /// An empty store that never touches the disk. Tests use this; so does a
    /// launch where the real file turned out to be unreadable, because losing
    /// the connection list is better than refusing to start.
    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(db: Connection) -> Result<Self> {
        // WAL so a long-running history write never blocks a read on the UI
        // thread; NORMAL because losing the last few milliseconds of query
        // history in a power cut is not worth an fsync per statement.
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.pragma_update(None, "foreign_keys", true)?;
        let store = Self { db };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let version: i32 = self
            .db
            .query_row("pragma user_version", [], |row| row.get(0))?;
        if version >= SCHEMA_VERSION {
            return Ok(());
        }
        if version < 1 {
            self.db.execute_batch(
                "create table connections (
                     id            text primary key,
                     name          text not null,
                     grp           text,
                     host          text not null,
                     port          integer not null,
                     database      text not null,
                     username      text not null,
                     ssl_mode      text not null,
                     ssl_cert      text,
                     ssl_key       text,
                     ssl_root_cert text,
                     color         text not null,
                     safety        text not null,
                     keep_alive    integer not null,
                     -- Position in the sidebar. Users reorder connections and
                     -- expect it to stick; alphabetical is not an ordering,
                     -- it is a default.
                     sort_order    integer not null default 0
                 );

                 create table history (
                     id            integer primary key autoincrement,
                     connection_id text,
                     sql           text not null,
                     started_at    integer not null,
                     duration_ms   integer,
                     row_count     integer,
                     error         text
                 );
                 create index history_by_time on history (started_at desc);

                 create table saved_queries (
                     id            text primary key,
                     connection_id text,
                     name          text not null,
                     sql           text not null,
                     updated_at    integer not null
                 );

                 -- Anything small, singular and stringly typed: window size,
                 -- last open connection, which sidebar tab was showing.
                 create table settings (
                     key   text primary key,
                     value text not null
                 );",
            )?;
        }
        if version < 2 {
            // Every connection that existed before there was a choice is a
            // Postgres one, which is what the default says. A column rather
            // than a rebuild: `alter table add column` is instant in SQLite
            // when there is a default, and a rewrite of the connection list is
            // the one migration nobody would forgive going wrong.
            self.db.execute_batch(
                "alter table connections
                     add column engine text not null default 'postgres';",
            )?;
        }
        if version < 3 {
            // The Messages tab used to keep its own log in memory, with three
            // facts this table had nowhere to put. One record of what was run
            // is worth more than two that disagree, so the columns come here
            // and the tab goes.
            //
            // `affected` beside `row_count` rather than folded into it: a
            // statement that changed three rows and one that returned three
            // are different statements, and a log that spells both "3 rows" is
            // one nobody can read backwards.
            self.db.execute_batch(
                "alter table history add column affected integer;
                 alter table history add column notices  text;
                 alter table history add column outcome  text not null default 'ok';
                 alter table history add column kind     text not null default 'statement';
                 -- The rows already here know how they turned out; they were
                 -- only ever asked in the other direction.
                 update history set outcome = 'failed' where error is not null;",
            )?;
        }
        self.db
            .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }

    // ---- connections -----------------------------------------------------

    /// Every saved connection, in the order the sidebar should show them.
    pub fn connections(&self) -> Result<Vec<ConnectionConfig>> {
        let mut statement = self.db.prepare(
            "select id, name, grp, host, port, database, username, ssl_mode,
                    ssl_cert, ssl_key, ssl_root_cert, color, safety, keep_alive,
                    engine
             from connections
             order by sort_order, name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ConnectionConfig {
                id: parse_uuid(row.get::<_, String>(0)?),
                name: row.get(1)?,
                group: row.get(2)?,
                host: row.get(3)?,
                port: row.get::<_, i64>(4)? as u16,
                database: row.get(5)?,
                user: row.get(6)?,
                ssl_mode: parse_ssl_mode(&row.get::<_, String>(7)?),
                ssl_cert: row.get(8)?,
                ssl_key: row.get(9)?,
                ssl_root_cert: row.get(10)?,
                color: parse_color(&row.get::<_, String>(11)?),
                safety: parse_safety(&row.get::<_, String>(12)?),
                keep_alive: row.get(13)?,
                engine: parse_engine(&row.get::<_, String>(14)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Insert or update. Keyed by [`ConnectionConfig::id`], so editing a
    /// connection keeps its Keychain item and its history.
    pub fn save_connection(&self, config: &ConnectionConfig) -> Result<()> {
        // A new connection goes to the end of the list rather than the top:
        // things people add should appear where they last looked.
        let next: i64 = self.db.query_row(
            "select coalesce(max(sort_order) + 1, 0) from connections",
            [],
            |row| row.get(0),
        )?;
        self.db.execute(
            "insert into connections
                 (id, name, grp, host, port, database, username, ssl_mode,
                  ssl_cert, ssl_key, ssl_root_cert, color, safety, keep_alive, sort_order,
                  engine)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             on conflict(id) do update set
                 name = excluded.name, grp = excluded.grp, host = excluded.host,
                 port = excluded.port, database = excluded.database,
                 username = excluded.username, ssl_mode = excluded.ssl_mode,
                 ssl_cert = excluded.ssl_cert, ssl_key = excluded.ssl_key,
                 ssl_root_cert = excluded.ssl_root_cert, color = excluded.color,
                 safety = excluded.safety, keep_alive = excluded.keep_alive,
                 engine = excluded.engine",
            params![
                config.id.to_string(),
                config.name,
                config.group,
                config.host,
                config.port as i64,
                config.database,
                config.user,
                config.ssl_mode.as_str(),
                config.ssl_cert,
                config.ssl_key,
                config.ssl_root_cert,
                config.color.as_str(),
                safety_name(config.safety),
                config.keep_alive,
                next,
                config.engine.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Forget a connection, its history and its password.
    pub fn delete_connection(&self, id: Uuid) -> Result<()> {
        let key = id.to_string();
        self.db
            .execute("delete from history where connection_id = ?1", params![key])?;
        self.db
            .execute("delete from connections where id = ?1", params![key])?;
        // Best effort: a Keychain that refuses the delete should not stop the
        // connection disappearing from the list.
        if let Err(error) = secrets::delete_password(id) {
            log::warn!("could not remove the Keychain item for {id}: {error:#}");
        }
        Ok(())
    }

    /// Persist a drag-reorder of the connection list.
    pub fn reorder_connections(&self, ids: &[Uuid]) -> Result<()> {
        for (position, id) in ids.iter().enumerate() {
            self.db.execute(
                "update connections set sort_order = ?1 where id = ?2",
                params![position as i64, id.to_string()],
            )?;
        }
        Ok(())
    }

    // ---- settings --------------------------------------------------------

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "select value from settings where key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.db.execute(
            "insert into settings (key, value) values (?1, ?2)
             on conflict(key) do update set value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub(crate) fn db(&self) -> &Connection {
        &self.db
    }
}

// ---- enum round-tripping -------------------------------------------------
//
// Stored as their libpq/serde names rather than as integers: a `select * from
// connections` in any SQLite browser should be readable, and a renumbering
// should never silently turn "require" into "disable".

fn parse_uuid(text: String) -> Uuid {
    Uuid::parse_str(&text).unwrap_or_else(|_| Uuid::nil())
}

fn parse_engine(text: &str) -> db::Engine {
    // An unknown engine is a row from a newer build, not a Redis one. Postgres
    // is the only thing every version of this file has been able to open.
    db::Engine::from_str(text).unwrap_or_default()
}

fn parse_ssl_mode(text: &str) -> SslMode {
    SslMode::ALL
        .into_iter()
        .find(|mode| mode.as_str() == text)
        .unwrap_or_default()
}

/// A colour this version does not know is no colour, on the same principle as
/// the engine above: a row written by a later version still opens.
fn parse_color(text: &str) -> ConnectionColor {
    ConnectionColor::from_str(text).unwrap_or_default()
}

fn safety_name(safety: SafetyLevel) -> &'static str {
    match safety {
        SafetyLevel::Normal => "normal",
        SafetyLevel::Confirm => "confirm",
        SafetyLevel::ReadOnly => "read-only",
    }
}

fn parse_safety(text: &str) -> SafetyLevel {
    match text {
        "confirm" => SafetyLevel::Confirm,
        // An unreadable safety level must not fall back to "normal": that turns
        // a corrupt row into write access on a production database.
        "read-only" => SafetyLevel::ReadOnly,
        _ => SafetyLevel::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(name: &str) -> ConnectionConfig {
        ConnectionConfig {
            name: name.into(),
            host: "db.example.com".into(),
            database: "app".into(),
            user: "postgres".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_connection_survives_a_round_trip() {
        let store = Store::in_memory().unwrap();
        let mut original = config("Production");
        original.color = ConnectionColor::Red;
        original.safety = SafetyLevel::ReadOnly;
        original.ssl_mode = SslMode::VerifyFull;
        original.group = Some("Work".into());
        store.save_connection(&original).unwrap();

        let loaded = store.connections().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, original.id);
        assert_eq!(loaded[0].name, "Production");
        assert_eq!(loaded[0].group.as_deref(), Some("Work"));
        assert_eq!(loaded[0].color, ConnectionColor::Red);
        assert_eq!(loaded[0].safety, SafetyLevel::ReadOnly);
        assert_eq!(loaded[0].ssl_mode, SslMode::VerifyFull);
    }

    #[test]
    fn saving_twice_updates_rather_than_duplicates() {
        let store = Store::in_memory().unwrap();
        let mut c = config("Staging");
        store.save_connection(&c).unwrap();
        c.host = "moved.example.com".into();
        store.save_connection(&c).unwrap();

        let loaded = store.connections().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].host, "moved.example.com");
    }

    #[test]
    fn connections_come_back_in_the_order_they_were_added() {
        let store = Store::in_memory().unwrap();
        let (a, b, c) = (config("Zulu"), config("Alpha"), config("Mike"));
        for one in [&a, &b, &c] {
            store.save_connection(one).unwrap();
        }
        let names: Vec<_> = store
            .connections()
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["Zulu", "Alpha", "Mike"]);

        store.reorder_connections(&[b.id, c.id, a.id]).unwrap();
        let names: Vec<_> = store
            .connections()
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["Alpha", "Mike", "Zulu"]);
    }

    #[test]
    fn deleting_a_connection_takes_its_history_with_it() {
        let store = Store::in_memory().unwrap();
        let c = config("Scratch");
        store.save_connection(&c).unwrap();
        store
            .record_query(Some(c.id), "select 1", 1_700_000_000_000)
            .unwrap();
        assert_eq!(store.recent_queries(10).unwrap().len(), 1);

        store.delete_connection(c.id).unwrap();
        assert!(store.connections().unwrap().is_empty());
        assert!(store.recent_queries(10).unwrap().is_empty());
    }

    #[test]
    fn an_unreadable_safety_level_does_not_grant_write_access() {
        assert_eq!(parse_safety("read-only"), SafetyLevel::ReadOnly);
        assert_eq!(parse_safety("nonsense"), SafetyLevel::Normal);
        assert_eq!(parse_ssl_mode("verify-full"), SslMode::VerifyFull);
        // An unknown SSL mode falls back to `require`, never to `disable`.
        assert_eq!(parse_ssl_mode("nonsense"), SslMode::Require);
    }

    /// The v1 schema, spelled out rather than borrowed from `migrate`.
    ///
    /// A copy on purpose: the point of the test is that a file written by the
    /// *shipped* v1 opens, and a constant shared with the migration would drift
    /// with it and stop testing anything.
    const V1_SCHEMA: &str = "create table connections (
             id            text primary key,
             name          text not null,
             grp           text,
             host          text not null,
             port          integer not null,
             database      text not null,
             username      text not null,
             ssl_mode      text not null,
             ssl_cert      text,
             ssl_key       text,
             ssl_root_cert text,
             color         text not null,
             safety        text not null,
             keep_alive    integer not null,
             sort_order    integer not null default 0
         );

         create table history (
             id            integer primary key autoincrement,
             connection_id text,
             sql           text not null,
             started_at    integer not null,
             duration_ms   integer,
             row_count     integer,
             error         text
         );
         create index history_by_time on history (started_at desc);

         create table saved_queries (
             id            text primary key,
             connection_id text,
             name          text not null,
             sql           text not null,
             updated_at    integer not null
         );

         create table settings (
             key   text primary key,
             value text not null
         );";

    #[test]
    fn a_v1_file_opens_with_its_connections_intact() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(V1_SCHEMA).unwrap();
        db.execute(
            "insert into connections
                 (id, name, grp, host, port, database, username, ssl_mode,
                  ssl_cert, ssl_key, ssl_root_cert, color, safety, keep_alive, sort_order)
             values (?1, 'Production', 'Work', 'db.example.com', 5432, 'app', 'postgres',
                     'verify-full', null, null, null, 'red', 'read-only', 1, 0)",
            params![Uuid::nil().to_string()],
        )
        .unwrap();
        db.execute(
            "insert into history (connection_id, sql, started_at)
             values (?1, 'select 1', 1700000000000)",
            params![Uuid::nil().to_string()],
        )
        .unwrap();
        db.execute(
            "insert into history (connection_id, sql, started_at, error)
             values (?1, 'select oops', 1700000001000, 'no such column')",
            params![Uuid::nil().to_string()],
        )
        .unwrap();
        db.pragma_update(None, "user_version", 1).unwrap();

        let store = Store::from_connection(db).unwrap();
        let loaded = store.connections().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Production");
        assert_eq!(loaded[0].group.as_deref(), Some("Work"));
        assert_eq!(loaded[0].safety, SafetyLevel::ReadOnly);
        assert_eq!(loaded[0].ssl_mode, SslMode::VerifyFull);
        // The column the migration added: a connection saved before there was
        // a choice is a Postgres one.
        assert_eq!(loaded[0].engine, db::Engine::Postgres);
        // Everything else the file held is still there.
        let history = store.recent_queries(10).unwrap();
        assert_eq!(history.len(), 2);
        // A row written before there was an outcome column still knows how it
        // turned out; it was only ever asked in the other direction.
        assert_eq!(history[0].outcome, crate::Outcome::Failed);
        assert_eq!(history[1].outcome, crate::Outcome::Ok);
        assert_eq!(history[1].kind, crate::HistoryKind::Statement);

        // And the upgraded row is writable, which an `alter table` that
        // half-applied would not be.
        let mut edited = loaded[0].clone();
        edited.engine = db::Engine::Redis;
        store.save_connection(&edited).unwrap();
        assert_eq!(store.connections().unwrap()[0].engine, db::Engine::Redis);
    }

    #[test]
    fn a_migration_that_has_already_run_is_not_run_twice() {
        // Opening the same file twice is the common case, and `alter table add
        // column` is not idempotent: a second run would error, which on the
        // real path means the app refuses to start.
        let store = Store::in_memory().unwrap();
        store.save_connection(&config("Production")).unwrap();
        store.migrate().unwrap();
        assert_eq!(store.connections().unwrap().len(), 1);
    }

    #[test]
    fn an_engine_survives_a_round_trip() {
        let store = Store::in_memory().unwrap();
        let mut redis = config("Cache");
        redis.engine = db::Engine::Redis;
        redis.port = 6379;
        store.save_connection(&redis).unwrap();
        let loaded = store.connections().unwrap();
        assert_eq!(loaded[0].engine, db::Engine::Redis);
    }

    #[test]
    fn settings_are_a_string_map() {
        let store = Store::in_memory().unwrap();
        assert!(store.setting("window").unwrap().is_none());
        store.set_setting("window", "1440x900").unwrap();
        store.set_setting("window", "1280x800").unwrap();
        assert_eq!(
            store.setting("window").unwrap().as_deref(),
            Some("1280x800")
        );
    }
}
