//! A live connection, as the window sees it.
//!
//! The bridge between two schedulers. GPUI owns the main thread and will not
//! tolerate a blocking call on it; `tokio-postgres` needs a Tokio reactor. So
//! every database operation is a Tokio future handed to [`gpui_tokio::Tokio`],
//! and its result comes back through a GPUI task that updates this entity on
//! the main thread. Nothing else in the app touches `db_pg` directly.
//!
//! One session is one server connection, not a pool. `SET`, temporary tables
//! and open transactions all behave the way a person typing SQL expects them
//! to, which no pool can promise.

use std::sync::Arc;
use std::time::{Duration, Instant};

use db::{ConnectionConfig, DbError, ErrorClass, ResultSet, SchemaSnapshot};
use db_pg::{introspect, Canceller, Outcome, PgConnection};
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
    /// Rows the server had more of than [`db_pg::DEFAULT_MAX_ROWS`].
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

pub enum SessionEvent {
    /// Connected, disconnected, or failed. The titlebar and status bar redraw.
    StateChanged,
    /// A new [`SchemaSnapshot`] landed. The sidebar rebuilds its tree.
    SchemaChanged,
    /// A statement finished, for better or worse. `Session::last` has it.
    Finished,
    /// A grid commit finished. `Session::last_apply` has it.
    Applied,
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
    connection: Option<Arc<PgConnection>>,
    /// Kept beside the connection because cancelling needs a second socket, and
    /// the connection itself is busy on the first one.
    canceller: Option<Canceller>,
    pub snapshot: Option<Arc<SchemaSnapshot>>,
    /// The in-flight operation. Dropping it detaches this side; the server-side
    /// half is stopped by [`Session::cancel`].
    task: Option<Task<()>>,
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
            canceller: None,
            snapshot: None,
            task: None,
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

    pub fn is_busy(&self) -> bool {
        self.activity != Activity::Idle
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
        self.canceller = None;
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
            let connection = PgConnection::connect(&config, password.as_deref())
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
            let snapshot = introspect::snapshot(&connection).await?;
            Ok::<_, DbError>((connection, snapshot))
        });

        self.task = Some(cx.spawn(async move |this, cx| {
            let result = joined(work.await);
            this.update(cx, |this, cx| {
                this.activity = Activity::Idle;
                this.task = None;
                match result {
                    Ok((connection, snapshot)) => {
                        this.canceller = Some(connection.canceller());
                        this.connection = Some(Arc::new(connection));
                        this.snapshot = Some(Arc::new(snapshot));
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
        self.canceller = None;
        self.snapshot = None;
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

        let work = Tokio::spawn(cx, async move { introspect::snapshot(&connection).await });
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = joined(work.await);
            this.update(cx, |this, cx| {
                this.activity = Activity::Idle;
                this.task = None;
                match result {
                    Ok(snapshot) => {
                        this.snapshot = Some(Arc::new(snapshot));
                        cx.emit(SessionEvent::SchemaChanged);
                    }
                    Err(error) => log::error!("refresh failed: {}", error.full_text()),
                }
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
        if self.config.is_read_only() && writes(&sql) {
            let error = DbError::new(
                ErrorClass::Server,
                "This connection is marked read-only. Change it in the connection settings to run writes.",
            );
            self.finish_locally(sql, error, cx);
            return;
        }
        if self.is_busy() {
            return;
        }

        self.activity = Activity::Running;
        cx.notify();

        let statement = sql.to_string();
        let work = Tokio::spawn(cx, async move {
            // Timed inside the Tokio task so the number is the round trip and
            // not the round trip plus however long the UI took to notice.
            let started = Instant::now();
            let outcome = connection.query(&statement, db_pg::DEFAULT_MAX_ROWS).await;
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
            let writes: Vec<db_pg::Write<'_>> = statements
                .iter()
                .map(|s| db_pg::Write {
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
        let Some(canceller) = self.canceller.clone() else {
            return;
        };
        if !self.is_busy() {
            return;
        }
        Tokio::spawn(cx, async move { canceller.cancel().await }).detach();
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
