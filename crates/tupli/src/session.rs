//! A live connection, as the window sees it.
//!
//! The bridge between two schedulers. GPUI owns the main thread and will not
//! tolerate a blocking call on it; the drivers need a Tokio reactor. So every
//! database operation is a Tokio future handed to [`gpui_tokio::Tokio`], and
//! its result comes back through a GPUI task that updates this entity on the
//! main thread.
//!
//! What is on the other end is an [`Arc<dyn Driver>`] and nothing more
//! specific: this crate does not depend on `db_pg` or `db_redis` at all, so
//! "the window does not know which engine it is talking to" is checked rather
//! than promised. What it may assume instead is
//! [`ConnectionConfig::capabilities`].
//!
//! One session is one server connection, not a pool. `SET`, temporary tables
//! and open transactions all behave the way a person typing SQL expects them
//! to, which no pool can promise.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use db::{
    Catalog, ConnectionConfig, Cursor, DbError, Driver, ErrorClass, Grants, KeyFacts, KeyQuery,
    KeyType, Outcome, RelationRef, ResultSet, RoleSet, SchemaSnapshot,
};
use gpui::{Context, EventEmitter, SharedString, Task};
use gpui_tokio::Tokio;

/// Where the connection is.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionState {
    /// Configured but not opened. Where every session starts.
    Offline,
    Connecting,
    Connected,
    /// The last attempt failed, with the server's own words.
    Failed(SharedString),
}

impl SessionState {
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    /// The one-line summary the status bar shows.
    pub fn label(&self) -> SharedString {
        match self {
            Self::Offline => "Not connected".into(),
            Self::Connecting => "Connecting…".into(),
            Self::Connected => "Connected".into(),
            Self::Failed(message) => message.clone(),
        }
    }
}

/// What the session is doing right now, so the UI can show a spinner on the
/// right thing and offer cancel only when there is something to cancel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Connecting,
    Introspecting,
    Running,
}

/// One finished statement.
pub struct Run {
    pub sql: SharedString,
    pub elapsed: Duration,
    /// The rows, if it produced any. Taken by the workspace and handed to the
    /// grid, which leaves `None` behind — a result set is large enough that
    /// keeping a second copy here would be a real cost, not a tidy one.
    pub rows: Option<ResultSet>,
    /// Rows the server had more of than [`db::DEFAULT_MAX_ROWS`].
    pub truncated: bool,
    /// `UPDATE 3` — the count for a statement that returned no rows.
    pub affected: Option<u64>,
    pub error: Option<DbError>,
    /// `NOTICE`/`WARNING` the server said while this ran. Usually empty; a
    /// migration or a chatty `plpgsql` function fills it.
    pub notices: Vec<db::Notice>,
}

impl Run {
    pub fn succeeded(&self) -> bool {
        self.error.is_none()
    }
}

/// How many keys one turn of the walk asks for.
///
/// Large enough that the tree looks populated after one round trip, small
/// enough that a `MATCH` which hits nothing does not hold the server inside a
/// single `SCAN` for long. `SCAN`'s `COUNT` is a hint about work done and not
/// a count of keys returned, so this is a budget, not a promise.
const KEY_PAGE: usize = 500;

/// How much of one key's contents is read at a time. A list can hold ten
/// million items; the grid is virtualised but the wire is not.
const KEY_ROWS: usize = 1000;

/// One opened key: what it is, and as much of it as was read.
///
/// The shape of [`Run`] and for the same reason — the rows are taken out by
/// whoever draws them rather than copied — but it is not a `Run`, because
/// nothing was run. There is no statement to put in the message log and no
/// elapsed time worth reporting for two round trips.
pub struct KeyView {
    pub key: Arc<[u8]>,
    pub kind: KeyType,
    /// TTL, encoding, memory, length — the facts bar above the rows. `None`
    /// when the key went away between being listed and being opened, which is
    /// ordinary in Redis rather than an error.
    pub facts: Option<KeyFacts>,
    pub rows: Option<ResultSet>,
    /// Where the next page of *this key's* contents starts.
    pub more: Option<Cursor>,
    pub error: Option<DbError>,
}

pub enum SessionEvent {
    /// Connected, disconnected, or failed. The titlebar and status bar redraw.
    StateChanged,
    /// A new [`SchemaSnapshot`] landed. The sidebar rebuilds its tree.
    SchemaChanged,
    /// A statement finished, for better or worse. `Session::last` has it.
    Finished,
    /// A grid commit finished. `Session::last_apply` has it.
    Applied,
    /// A page of the keyspace arrived. The sidebar rebuilds its tree, which is
    /// the same job [`SessionEvent::SchemaChanged`] does — separate because a
    /// scan lands repeatedly while the walk continues and must not re-run
    /// everything else that a new catalog means.
    KeysChanged,
    /// A key was read. `Session::last_key` has it.
    KeyOpened,
    /// The roles arrived, or one relation's grants did. Separate from
    /// [`SessionEvent::SchemaChanged`] because it lands on its own schedule —
    /// grants are read when somebody opens the Privileges tab, and rebuilding
    /// the whole tree for that would be a redraw of everything to fill in one
    /// pane.
    PrivilegesChanged,
}

/// One finished commit of staged grid edits.
///
/// Separate from [`Run`] because it is not one statement and it produced no
/// rows: what the caller needs back is whether the transaction went through,
/// and if it did not, why — so it can put the staged changes back on screen
/// rather than pretending they were saved.
pub struct Applied {
    /// What was attempted, for the message log.
    pub counts: sqlgen::Counts,
    pub elapsed: Duration,
    pub error: Option<DbError>,
}

impl Applied {
    pub fn succeeded(&self) -> bool {
        self.error.is_none()
    }

    /// `2 inserts, 1 update` — the sentence the message log prints.
    pub fn summary(&self) -> String {
        let c = self.counts;
        let parts: Vec<String> = [
            (c.inserts, "insert"),
            (c.updates, "update"),
            (c.deletes, "delete"),
            (c.ddl, "schema change"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, word)| {
            if n == 1 {
                format!("1 {word}")
            } else {
                format!("{n} {word}s")
            }
        })
        .collect();
        if parts.is_empty() {
            "nothing".to_string()
        } else {
            parts.join(", ")
        }
    }
}

pub struct Session {
    pub config: ConnectionConfig,
    /// The password as it was typed, when this session was opened by saving a
    /// connection. Held so that the first connect does not have to ask the
    /// Keychain for something it was handed a moment ago: the write and the
    /// read are two different subsystems, and a save that works followed by a
    /// read that does not is a connection that reports "password missing"
    /// about a password the user watched themselves type.
    ///
    /// Not a cache. A session reopened from the sidebar has none of this and
    /// goes to the Keychain, which is the store of record.
    password: Option<String>,
    state: SessionState,
    activity: Activity,
    connection: Option<Arc<dyn Driver>>,
    /// The catalog of a SQL server. `None` on a connection that has no schema
    /// to read — see [`Session::keyspace`].
    pub snapshot: Option<Arc<SchemaSnapshot>>,
    /// The catalog of a key-value server, which is the databases and how full
    /// they are. Exactly one of this and [`Session::snapshot`] is set on a
    /// connected session.
    pub keyspace: Option<Arc<db::Keyspace>>,
    /// The keys the browser has seen so far, in the order the server handed
    /// them over. A *sample* and not an inventory: on a keyspace of any size
    /// this is the first few thousand keys of a walk that has not finished,
    /// which is why [`Session::keys_complete`] exists and why nothing here
    /// reports a total.
    pub keys: Vec<db::KeyInfo>,
    /// Whether the walk that produced [`Session::keys`] reached the end.
    pub keys_complete: bool,
    /// Where the walk stopped, so "load more" has somewhere to resume from.
    key_cursor: Option<db::Cursor>,
    /// The glob the current listing was asked for, so a scan already running
    /// against the same pattern is not started again.
    key_pattern: String,
    /// What the last [`Session::open_key`] found out about the key it opened.
    pub last_key: Option<KeyView>,
    /// Every role on the server, read once when the connection opens. `None`
    /// on an engine that has no roles, and also while the read is in flight.
    pub roles: Option<Arc<RoleSet>>,
    /// The privileges on the relations somebody has looked at.
    ///
    /// Kept rather than re-read because the answer is stable until somebody
    /// runs `grant`, and thrown away wholesale on reconnect because a `set
    /// role` — or a different login — changes every one of them at once.
    pub grants: HashMap<RelationRef, Arc<Grants>>,
    /// The relation whose grants are being read, so two clicks on the same tab
    /// do not become two round trips.
    loading_grants: Option<RelationRef>,
    /// The in-flight operation. Dropping it detaches this side; the server-side
    /// half is stopped by [`Session::cancel`].
    task: Option<Task<()>>,
    /// The statement that arrived while [`Session::task`] was still in flight,
    /// and will be sent the moment it lands. One slot, not a queue: what
    /// overtakes a browse is another browse of somewhere else, and every one
    /// of them but the last is a question nobody is still asking.
    superseding: Option<SharedString>,
    pub last: Option<Run>,
    /// The last commit of grid edits, on the same terms as [`Session::last`].
    pub last_apply: Option<Applied>,
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    pub fn new(config: ConnectionConfig) -> Self {
        Self::with_password(config, None)
    }

    /// A session that already knows its password — the one the connection sheet
    /// just saved.
    pub fn with_password(config: ConnectionConfig, password: Option<String>) -> Self {
        Self {
            config,
            password,
            state: SessionState::Offline,
            activity: Activity::Idle,
            connection: None,
            snapshot: None,
            keyspace: None,
            keys: Vec::new(),
            keys_complete: false,
            key_cursor: None,
            key_pattern: String::new(),
            last_key: None,
            roles: None,
            grants: HashMap::new(),
            loading_grants: None,
            task: None,
            superseding: None,
            last: None,
            last_apply: None,
        }
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    pub fn activity(&self) -> Activity {
        self.activity
    }

    /// What the server said it is, empty until there is one. A `SchemaSnapshot`
    /// carries this for a SQL server; a keyspace has no snapshot to carry it,
    /// so the tree asks here.
    pub fn server_version(&self) -> Arc<str> {
        match &self.connection {
            Some(connection) => connection.server_version(),
            None => Arc::from(""),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.activity != Activity::Idle
    }

    /// Connect again, taking the connection's details afresh.
    ///
    /// The config and the password are replaced rather than kept, because the
    /// usual reason a failed connection is retried is that something about it
    /// was wrong and has just been corrected. A `None` password means "ask the
    /// Keychain", which is what a password field left alone means as well.
    pub fn retry(
        &mut self,
        config: ConnectionConfig,
        password: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.config = config;
        self.password = password;
        self.connect(cx);
    }

    /// Open the connection and read the catalog, in one operation.
    ///
    /// They are one operation because a connection without a schema is not
    /// something the UI can do anything with: the tree would be empty and
    /// autocomplete would have nothing to offer. Failing halfway leaves the
    /// session offline rather than connected-but-blind.
    pub fn connect(&mut self, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        self.state = SessionState::Connecting;
        self.activity = Activity::Connecting;
        self.connection = None;
        cx.emit(SessionEvent::StateChanged);
        cx.notify();

        let config = self.config.clone();
        let typed = self.password.clone();
        let work = Tokio::spawn(cx, async move {
            // The Keychain call is synchronous and can put up a system prompt
            // the first time. On a Tokio worker that is a blocked thread; on
            // the main thread it would be a frozen window.
            //
            // `TUPLI_PASSWORD` short-circuits it for the headless renderer and
            // for `TUPLI_CONNECT`: a connection named on the command line was
            // never saved, so there is no Keychain item under its id to find,
            // and prompting for one out of a screenshot run is not an option.
            let password = match (typed, std::env::var("TUPLI_PASSWORD")) {
                // What was typed into the sheet, if this session came from one.
                (Some(password), _) => Some(password),
                (None, Ok(password)) => Some(password),
                (None, Err(_)) => store::secrets::password(config.id)
                    .map_err(|error| DbError::internal(format!("Keychain: {error:#}")))?,
            };
            let connection = drivers::connect(&config, password.as_deref())
                .await
                // The driver's way of saying the server asked for a password
                // and was not given one. On its own it reads like the app lost
                // the password, and the difference between "there is none
                // saved" and "the one saved is wrong" is the difference between
                // knowing what to do next and not.
                .map_err(|error| match password.is_none() && error.message.contains("password") {
                    true => DbError::connection(
                        "this connection has no password saved, and the server asks for one \u{2014}                          edit the connection and enter it",
                    ),
                    false => error,
                })?;
            let catalog = connection.catalog().await?;
            // On the same trip as the catalog, and tolerant of failing: a
            // server that will not say who its roles are is still a server
            // worth browsing, and losing the connection over a list nobody
            // asked for yet would be absurd.
            let roles = connection.roles().await.unwrap_or_else(|error| {
                log::warn!("could not read the roles: {}", error.full_text());
                None
            });
            Ok::<_, DbError>((connection, catalog, roles))
        });

        self.task = Some(cx.spawn(async move |this, cx| {
            let result = joined(work.await);
            this.update(cx, |this, cx| {
                this.activity = Activity::Idle;
                this.task = None;
                match result {
                    Ok((connection, catalog, roles)) => {
                        this.connection = Some(connection);
                        this.set_catalog(catalog);
                        this.roles = roles.map(Arc::new);
                        this.state = SessionState::Connected;
                        cx.emit(SessionEvent::SchemaChanged);
                    }
                    Err(error) => {
                        log::error!("connect failed: {}", error.full_text());
                        this.state = SessionState::Failed(error.message.to_string().into());
                    }
                }
                cx.emit(SessionEvent::StateChanged);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Drop the connection. The driver task ends with it, which is what closes
    /// the socket — there is no goodbye to send.
    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.task = None;
        self.connection = None;
        self.snapshot = None;
        self.keyspace = None;
        self.roles = None;
        self.grants.clear();
        self.loading_grants = None;
        self.keys.clear();
        self.keys_complete = false;
        self.key_cursor = None;
        self.activity = Activity::Idle;
        self.state = SessionState::Offline;
        cx.emit(SessionEvent::SchemaChanged);
        cx.emit(SessionEvent::StateChanged);
        cx.notify();
    }

    /// Re-read the catalog. Called after DDL, and by the sidebar's refresh
    /// button; a snapshot is a fact about a moment and DDL ends the moment.
    pub fn refresh_schema(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = self.connection.clone() else {
            return;
        };
        if self.is_busy() {
            return;
        }
        self.activity = Activity::Introspecting;
        cx.notify();

        let work = Tokio::spawn(cx, async move { connection.catalog().await });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = joined(work.await);
            this.update(cx, |this, cx| {
                this.activity = Activity::Idle;
                this.task = None;
                match result {
                    Ok(catalog) => {
                        this.set_catalog(catalog);
                        cx.emit(SessionEvent::SchemaChanged);
                    }
                    Err(error) => log::error!("refresh failed: {}", error.full_text()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Read who may do what to one relation, unless it is already known or
    /// already on its way.
    ///
    /// Deliberately outside [`Session::task`] and outside [`Activity`]: this is
    /// three small catalog reads for a pane that is being looked at, and making
    /// it take the one busy slot would mean opening the Privileges tab could
    /// stop somebody's query from starting.
    pub fn load_grants(&mut self, relation: RelationRef, cx: &mut Context<Self>) {
        let Some(connection) = self.connection.clone() else {
            return;
        };
        if !connection.capabilities().roles
            || self.grants.contains_key(&relation)
            || self.loading_grants.as_ref() == Some(&relation)
        {
            return;
        }
        self.loading_grants = Some(relation.clone());
        let wanted = relation.clone();
        let work = Tokio::spawn(cx, async move { connection.grants(&wanted).await });
        cx.spawn(async move |this, cx| {
            let result = joined(work.await);
            this.update(cx, |this, cx| {
                if this.loading_grants.as_ref() == Some(&relation) {
                    this.loading_grants = None;
                }
                match result {
                    Ok(Some(grants)) => {
                        this.grants.insert(relation, Arc::new(grants));
                    }
                    Ok(None) => {}
                    Err(error) => {
                        log::warn!("could not read the privileges: {}", error.full_text())
                    }
                }
                cx.emit(SessionEvent::PrivilegesChanged);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Forget what was read, so the next pane that wants it asks again. Called
    /// after DDL and after a `grant`, both of which can change every answer.
    pub fn forget_grants(&mut self) {
        self.grants.clear();
        self.loading_grants = None;
    }

    /// Walk the keyspace and put what it finds in [`Session::keys`].
    ///
    /// A *sample*, not a listing: `SCAN` gives no total and no order, and the
    /// only command that would give both is the one that stalls the server. So
    /// this is called again — with the same `pattern` — for each further page,
    /// and the tree says how many keys it has *seen* rather than how many
    /// there are.
    ///
    /// Sizes are left out (`memory: false`): `MEMORY USAGE` is a command per
    /// key, and the browser only shows a size for the key that is open, which
    /// [`Session::open_key`] fetches on its own.
    pub fn scan_keys(&mut self, pattern: impl Into<String>, cx: &mut Context<Self>) {
        let pattern = pattern.into();
        let Some(connection) = self.connection.clone() else {
            return;
        };
        if !connection.capabilities().paged_catalog || self.is_busy() {
            return;
        }
        // A new pattern is a new walk, and the keys from the old one are about
        // a different question.
        let restart = pattern != self.key_pattern;
        if restart {
            self.keys.clear();
            self.keys_complete = false;
            self.key_cursor = None;
            self.key_pattern = pattern.clone();
        } else if self.keys_complete {
            return;
        }

        let query = KeyQuery {
            pattern,
            kind: None,
            from: self.key_cursor.clone(),
            limit: KEY_PAGE,
            memory: false,
        };
        self.activity = Activity::Introspecting;
        cx.notify();

        let work = Tokio::spawn(cx, async move { connection.list_keys(&query).await });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = joined(work.await);
            this.update(cx, |this, cx| {
                this.activity = Activity::Idle;
                this.task = None;
                match result {
                    Ok(listing) => {
                        this.keys.extend(listing.keys);
                        this.keys_complete = listing.more.is_none();
                        this.key_cursor = listing.more;
                        cx.emit(SessionEvent::KeysChanged);
                    }
                    Err(error) => log::error!("scan failed: {}", error.full_text()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Read one key. The result arrives as [`SessionEvent::KeyOpened`].
    ///
    /// Two round trips rather than one, because what a key *is* and what a key
    /// *holds* are different questions and the second cannot be asked without
    /// the answer to the first — `LRANGE` on a hash is an error. The `kind` the
    /// listing already knew is passed in anyway: it is what decides the reader,
    /// and re-deriving it here would make an open cost a `TYPE` it does not
    /// need.
    pub fn open_key(&mut self, key: Arc<[u8]>, kind: KeyType, cx: &mut Context<Self>) {
        let Some(connection) = self.connection.clone() else {
            return;
        };
        if self.is_busy() {
            return;
        }
        self.activity = Activity::Running;
        cx.notify();

        let opened = key.clone();
        let reading = kind.clone();
        let work = Tokio::spawn(cx, async move {
            let facts = connection.describe_key(&opened).await;
            let page = connection.read_key(&opened, &reading, None, KEY_ROWS).await;
            (facts, page)
        });

        self.task = Some(cx.spawn(async move |this, cx| {
            let joined = work.await;
            this.update(cx, |this, cx| {
                this.activity = Activity::Idle;
                this.task = None;
                let view = match joined {
                    Ok((facts, page)) => {
                        // A key that expired between the listing and the click
                        // is not a failure — it is what a TTL is for — so the
                        // tab opens empty rather than red.
                        let (rows, more, error) = match page {
                            Ok(page) => (Some(page.rows), page.more, None),
                            Err(error) => (None, None, Some(error)),
                        };
                        KeyView {
                            key,
                            kind,
                            facts: facts.ok().flatten(),
                            rows,
                            more,
                            error,
                        }
                    }
                    Err(error) => KeyView {
                        key,
                        kind,
                        facts: None,
                        rows: None,
                        more: None,
                        error: Some(DbError::internal(error.to_string())),
                    },
                };
                this.last_key = Some(view);
                cx.emit(SessionEvent::KeyOpened);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Send a statement. The result arrives as [`SessionEvent::Finished`].
    pub fn run(&mut self, sql: impl Into<SharedString>, cx: &mut Context<Self>) {
        let sql = sql.into();
        if sql.trim().is_empty() {
            return;
        }
        let Some(connection) = self.connection.clone() else {
            self.finish_locally(sql, DbError::connection("Not connected"), cx);
            return;
        };
        // The keyword sniffer is SQL's. A command engine enforces its own
        // read-only rule at the socket, where the command's name is a fact
        // rather than a guess.
        if self.config.is_read_only() && self.config.capabilities().is_sql() && writes(&sql) {
            let error = DbError::new(
                ErrorClass::Server,
                "This connection is marked read-only. Change it in the connection settings to run writes.",
            );
            self.finish_locally(sql, error, cx);
            return;
        }
        // Held rather than dropped. Switching tabs faster than the server
        // answers used to throw every switch after the first away, so the
        // grid filled with the rows of the tab you started from and each
        // further switch did nothing at all — which reads as the app
        // getting slower the more you ask of it.
        if self.is_busy() {
            self.superseding = Some(sql);
            return;
        }

        self.activity = Activity::Running;
        cx.notify();

        let statement = sql.to_string();
        let work = Tokio::spawn(cx, async move {
            // Timed inside the Tokio task so the number is the round trip and
            // not the round trip plus however long the UI took to notice.
            let started = Instant::now();
            let outcome = connection.query(&statement, db::DEFAULT_MAX_ROWS).await;
            // Drained here rather than on the UI thread: by the time the result
            // gets there another statement could have been sent, and the
            // notices would follow the wrong one.
            (outcome, started.elapsed(), connection.take_notices())
        });

        self.task = Some(cx.spawn(async move |this, cx| {
            let joined = work.await;
            this.update(cx, |this, cx| {
                this.activity = Activity::Idle;
                this.task = None;
                let run = match joined {
                    Ok((Ok(Outcome::Rows { rows, truncated }), elapsed, notices)) => Run {
                        sql,
                        elapsed,
                        rows: Some(rows),
                        truncated,
                        affected: None,
                        error: None,
                        notices,
                    },
                    Ok((Ok(Outcome::Affected(count)), elapsed, notices)) => Run {
                        sql,
                        elapsed,
                        rows: None,
                        truncated: false,
                        affected: Some(count),
                        error: None,
                        notices,
                    },
                    Ok((Err(error), elapsed, notices)) => Run {
                        sql,
                        elapsed,
                        rows: None,
                        truncated: false,
                        affected: None,
                        error: Some(error),
                        notices,
                    },
                    // The Tokio task itself died — a panic in the driver, or an
                    // abort. Neither is the server's fault, so it is internal.
                    Err(error) => Run {
                        sql,
                        elapsed: Duration::ZERO,
                        rows: None,
                        truncated: false,
                        affected: None,
                        error: Some(DbError::internal(error.to_string())),
                        notices: Vec::new(),
                    },
                };
                // Superseded while it was in flight: this answer is about
                // a table nobody is looking at any more, and publishing it
                // would put those rows under the current tab's name for as
                // long as the next round trip takes.
                if let Some(next) = this.superseding.take() {
                    this.run(next, cx);
                    return;
                }
                this.last = Some(run);
                cx.emit(SessionEvent::Finished);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Send a grid commit: one transaction, every statement or none of them.
    ///
    /// Unlike [`Session::run`] this does not go through the message log's
    /// notion of a statement — a commit is one unit of work however many
    /// `UPDATE`s it is made of, and reporting it as nine separate runs would
    /// bury the one thing that matters, which is whether it committed.
    pub fn apply(
        &mut self,
        statements: Vec<sqlgen::Statement>,
        counts: sqlgen::Counts,
        cx: &mut Context<Self>,
    ) {
        if statements.is_empty() {
            return;
        }
        let Some(connection) = self.connection.clone() else {
            self.finish_apply(counts, Some(DbError::connection("Not connected")), cx);
            return;
        };
        // The same guard `run` applies to typed SQL. A read-only connection is
        // a promise the app made to the user, and the grid is not an exception
        // to it just because the writes were generated rather than typed.
        if self.config.is_read_only() {
            let error = DbError::new(
                ErrorClass::Server,
                "This connection is marked read-only. Change it in the connection settings to save edits.",
            );
            self.finish_apply(counts, Some(error), cx);
            return;
        }
        if self.is_busy() {
            return;
        }

        self.activity = Activity::Running;
        cx.notify();

        let work = Tokio::spawn(cx, async move {
            let started = Instant::now();
            // Borrowed here rather than in the caller: `Write` holds slices,
            // and the statements have to outlive the await.
            let writes: Vec<db::Write<'_>> = statements
                .iter()
                .map(|s| db::Write {
                    sql: &s.sql,
                    params: &s.params,
                    expect_rows: s.expect_rows,
                })
                .collect();
            let outcome = connection.apply(&writes).await;
            (outcome.err(), started.elapsed())
        });

        self.task = Some(cx.spawn(async move |this, cx| {
            let joined = work.await;
            this.update(cx, |this, cx| {
                this.activity = Activity::Idle;
                this.task = None;
                let (error, elapsed) = match joined {
                    Ok((error, elapsed)) => (error, elapsed),
                    Err(error) => (Some(DbError::internal(error.to_string())), Duration::ZERO),
                };
                this.last_apply = Some(Applied {
                    counts,
                    elapsed,
                    error,
                });
                cx.emit(SessionEvent::Applied);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Report a commit that never left the app — not connected, read-only —
    /// through the same field a real one lands in.
    fn finish_apply(
        &mut self,
        counts: sqlgen::Counts,
        error: Option<DbError>,
        cx: &mut Context<Self>,
    ) {
        self.last_apply = Some(Applied {
            counts,
            elapsed: Duration::ZERO,
            error,
        });
        cx.emit(SessionEvent::Applied);
        cx.notify();
    }

    /// Ask the server to stop.
    ///
    /// A cancel request is a separate connection carrying the backend's key, so
    /// it works even though the session's own socket is busy waiting for rows.
    /// The server may already have finished, in which case this does nothing
    /// and the result arrives normally.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(connection) = self.connection.clone() else {
            return;
        };
        // Whatever was waiting behind this one goes too: cancel means stop,
        // not stop and then start the next thing.
        self.superseding = None;
        if !self.is_busy() || !self.config.capabilities().cancel {
            return;
        }
        Tokio::spawn(cx, async move { connection.cancel().await }).detach();
    }

    /// Put a freshly read catalog where the sidebar looks for it.
    ///
    /// Both fields are set every time, not just the one that matched: a
    /// session that switches to a connection of the other kind must not keep
    /// showing the tree it had before.
    fn set_catalog(&mut self, catalog: Catalog) {
        match catalog {
            Catalog::Sql(snapshot) => {
                self.snapshot = Some(Arc::new(snapshot));
                self.keyspace = None;
            }
            Catalog::Keyspace(keyspace) => {
                self.snapshot = None;
                self.keyspace = Some(Arc::new(keyspace));
            }
        }
        // The keys under the old catalog are not the keys under the new one,
        // and a tree holding both would be showing a database's keys under
        // another database's name.
        self.keys.clear();
        self.keys_complete = false;
        self.key_cursor = None;
    }

    /// Take the rows off the last run, leaving the timing and the statement
    /// behind for the status bar.
    pub fn take_rows(&mut self) -> Option<ResultSet> {
        self.last.as_mut()?.rows.take()
    }

    /// Report a failure the app decided on without asking the server, through
    /// exactly the same path a server error takes — so there is one place the
    /// UI has to handle errors, not two.
    fn finish_locally(&mut self, sql: SharedString, error: DbError, cx: &mut Context<Self>) {
        self.last = Some(Run {
            sql,
            elapsed: Duration::ZERO,
            rows: None,
            truncated: false,
            affected: None,
            error: Some(error),
            notices: Vec::new(),
        });
        cx.emit(SessionEvent::Finished);
        cx.notify();
    }
}

/// Collapse "the Tokio task died" and "the query failed" into one error, since
/// nothing upstream treats them differently.
fn joined<T>(result: Result<Result<T, DbError>, gpui_tokio::JoinError>) -> Result<T, DbError> {
    match result {
        Ok(inner) => inner,
        Err(error) => Err(DbError::internal(error.to_string())),
    }
}

/// A cheap guess at whether a statement writes, used only to refuse early on a
/// read-only connection.
///
/// Deliberately conservative and deliberately not a parser: it looks at the
/// first keyword, and anything it does not recognise as a read counts as a
/// write. Being wrong in that direction costs a person one setting change;
/// being wrong the other way costs them a table.
fn writes(sql: &str) -> bool {
    let first = sql
        .split(|c: char| c.is_whitespace() || c == '(' || c == ';')
        .find(|word| !word.is_empty() && !word.starts_with("--"))
        .unwrap_or("")
        .to_ascii_lowercase();
    !matches!(
        first.as_str(),
        "select" | "with" | "show" | "explain" | "table" | "values" | "fetch"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_recognised_and_everything_else_is_a_write() {
        assert!(!writes("select * from users"));
        assert!(!writes("  SELECT 1"));
        assert!(!writes("with recent as (select 1) select * from recent"));
        assert!(!writes("explain analyze select 1"));
        assert!(writes("update users set plan = 'pro'"));
        assert!(writes("delete from users"));
        assert!(writes("drop table users"));
        assert!(writes("truncate users"));
        // `explain analyze delete …` really does write, but a read-only
        // connection refusing it is the safe direction to be wrong in, and it
        // is the reason this is a guard rather than a parser.
        assert!(writes(""));
    }

    #[test]
    fn a_state_always_has_something_to_show() {
        assert_eq!(SessionState::Offline.label(), "Not connected");
        assert_eq!(
            SessionState::Failed("password authentication failed".into()).label(),
            "password authentication failed"
        );
        assert!(SessionState::Connected.is_connected());
    }
}
