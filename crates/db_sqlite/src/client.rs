//! One open file.
//!
//! Two things here are unlike every other driver in this workspace. The first
//! is [`blocking`]: SQLite does not have an async interface and cannot be given
//! one, so each call is a closure handed to a thread that is allowed to block.
//! The second follows from it — the connection lives behind a mutex, taken
//! inside the closure and released before the future resolves, so the lock is
//! never held across an await.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use db::{ConnectionConfig, DbError, DbResult, Outcome, Value, Write};
use parking_lot::Mutex;
use rusqlite::types::{Null, ToSqlOutput, Value as SqlValue, ValueRef};
use rusqlite::{Connection, InterruptHandle, OpenFlags, ToSql};

use crate::error::{classify, connection_error};
use crate::rows::{self, Heading};

/// How long to wait for another writer before giving up.
///
/// A file is shared with whatever else has it open — a background job, a second
/// client, the app the database belongs to — and SQLite's default is to fail
/// the instant it finds the file locked. Five seconds turns the common case, a
/// commit that overlaps by milliseconds, from an error into a pause.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub struct SqliteConnection {
    conn: Arc<Mutex<Connection>>,
    /// Usable while the mutex is held by the thread running a statement, which
    /// is the entire point: this is the only way to reach a busy connection.
    interrupt: Arc<InterruptHandle>,
    version: Arc<str>,
    /// The file's own name, which is what stands in for a database name
    /// everywhere the app expects one.
    database: Arc<str>,
    /// Set when a statement is interrupted, so the next call can report a
    /// cancel rather than whatever SQLite says afterwards.
    canceled: Arc<AtomicBool>,
}

impl SqliteConnection {
    pub async fn connect(config: &ConnectionConfig, _password: Option<&str>) -> DbResult<Self> {
        let path = config.database.trim().to_string();
        if path.is_empty() {
            return Err(DbError::connection("No database file was given."));
        }
        blocking(move || open(&path)).await
    }

    pub fn server_version(&self) -> &Arc<str> {
        &self.version
    }

    pub fn database(&self) -> &Arc<str> {
        &self.database
    }

    /// Always false. There is no socket to drop: the file is open until this
    /// value is, and a file that is deleted out from under an open handle is
    /// still readable on macOS.
    pub fn is_closed(&self) -> bool {
        false
    }

    pub fn cancel(&self) -> futures::future::BoxFuture<'static, ()> {
        let interrupt = self.interrupt.clone();
        let canceled = self.canceled.clone();
        Box::pin(async move {
            canceled.store(true, Ordering::SeqCst);
            interrupt.interrupt();
        })
    }

    /// Run `f` against the connection on a thread that may block.
    pub(crate) async fn with_conn<T, F>(&self, f: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> DbResult<T> + Send + 'static,
    {
        let conn = self.conn.clone();
        let canceled = self.canceled.clone();
        blocking(move || {
            // Cleared here rather than after the interrupt, because SQLite
            // keeps the interrupt pending until something is running to
            // receive it: clearing it early would arm the flag against the
            // next statement.
            canceled.store(false, Ordering::SeqCst);
            let conn = conn.lock();
            let result = f(&conn);
            match canceled.swap(false, Ordering::SeqCst) && result.is_err() {
                true => Err(DbError::canceled()),
                false => result,
            }
        })
        .await
    }

    pub async fn query(&self, statement: &str, max_rows: usize) -> DbResult<Outcome> {
        let sql = statement.to_string();
        self.with_conn(move |conn| run(conn, &sql, max_rows)).await
    }

    pub async fn apply(&self, writes: &[Write<'_>]) -> DbResult<Vec<u64>> {
        // Owned, because the closure outlives this call: `Write` borrows the
        // change set and a blocking thread cannot be promised it stays put.
        let staged: Vec<(String, Vec<Value>, Option<u64>)> = writes
            .iter()
            .map(|w| (w.sql.to_string(), w.params.to_vec(), w.expect_rows))
            .collect();
        self.with_conn(move |conn| commit(conn, &staged)).await
    }

    /// Run a statement for its effect. Used by introspection, never by the
    /// console.
    pub async fn execute(&self, sql: &str) -> DbResult<u64> {
        match self.query(sql, 0).await? {
            Outcome::Affected(n) => Ok(n),
            Outcome::Rows { rows, .. } => Ok(rows.row_count() as u64),
        }
    }
}

/// Hand `f` to a thread that is allowed to block.
///
/// Falls back to running it inline when there is no runtime, which is what a
/// unit test is: the work is the same, and requiring an async test harness to
/// exercise a synchronous library would be a cost paid for nothing.
async fn blocking<T, F>(f: F) -> DbResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> DbResult<T> + Send + 'static,
{
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return f();
    };
    match handle.spawn_blocking(f).await {
        Ok(result) => result,
        // The closure panicked. Anything else has already been turned into a
        // `DbError` inside it.
        Err(error) => Err(DbError::internal(format!("sqlite call failed: {error}"))),
    }
}

fn open(path: &str) -> DbResult<SqliteConnection> {
    // No `SQLITE_OPEN_CREATE`: a mistyped path must be an error. Creating the
    // file instead would open an empty database that looks exactly like the
    // real one after something went very wrong.
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if path == db::MEMORY {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let conn = Connection::open_with_flags(path, flags).map_err(connection_error)?;
    conn.busy_timeout(BUSY_TIMEOUT).map_err(connection_error)?;
    // Off by default, for compatibility with databases written before SQLite
    // had them. The app draws the foreign keys and offers edits against them,
    // so it enforces them: an edit the schema says is impossible should fail
    // here rather than quietly leave a dangling row.
    conn.execute_batch("PRAGMA foreign_keys = ON")
        .map_err(connection_error)?;
    let version: String = conn
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .map_err(connection_error)?;
    Ok(SqliteConnection {
        interrupt: Arc::new(conn.get_interrupt_handle()),
        conn: Arc::new(Mutex::new(conn)),
        version: version.into(),
        database: file_name(path).into(),
        canceled: Arc::new(AtomicBool::new(false)),
    })
}

/// `orders.db` out of `/srv/data/orders.db`, for everywhere the app wants the
/// name of a database and there is a file instead.
fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn run(conn: &Connection, sql: &str, max_rows: usize) -> DbResult<Outcome> {
    let mut stmt = conn.prepare(sql).map_err(classify)?;
    if stmt.column_count() == 0 {
        // Counted as a difference rather than read from `changes()`, which
        // reports the last statement that touched a row — so a `CREATE TABLE`
        // would inherit the count of whatever ran before it.
        let before = conn.total_changes();
        stmt.raw_execute().map_err(classify)?;
        return Ok(Outcome::Affected(conn.total_changes() - before));
    }

    let headings: Vec<Heading> = stmt
        .columns()
        .iter()
        .map(|column| Heading {
            name: column.name().to_string(),
            declared: column.decl_type().map(str::to_string),
        })
        .collect();
    let mut data: Vec<Vec<SqlValue>> = vec![Vec::new(); headings.len()];
    let mut truncated = false;
    let mut fetched = 0;
    let mut rows = stmt.raw_query();
    // One row past the cap is fetched and dropped, which is the only way to
    // tell a result of exactly `max_rows` from one that was cut short.
    while let Some(row) = rows.next().map_err(classify)? {
        if fetched == max_rows {
            truncated = true;
            break;
        }
        for (index, column) in data.iter_mut().enumerate() {
            column.push(owned(row.get_ref(index).map_err(classify)?));
        }
        fetched += 1;
    }
    Ok(Outcome::Rows {
        rows: rows::result_set(&headings, data),
        truncated,
    })
}

/// A fetched value, copied off the statement before the next step frees it.
fn owned(value: ValueRef<'_>) -> SqlValue {
    match value {
        ValueRef::Null => SqlValue::Null,
        ValueRef::Integer(i) => SqlValue::Integer(i),
        ValueRef::Real(r) => SqlValue::Real(r),
        // SQLite stores whatever bytes were written, so a column it calls text
        // may not be UTF-8. Keeping those as bytes shows them as a blob, which
        // is at least true; the alternative is an error in the middle of a
        // fetch over one bad row.
        ValueRef::Text(s) => match std::str::from_utf8(s) {
            Ok(s) => SqlValue::Text(s.to_owned()),
            Err(_) => SqlValue::Blob(s.to_vec()),
        },
        ValueRef::Blob(b) => SqlValue::Blob(b.to_vec()),
    }
}

fn commit(conn: &Connection, writes: &[(String, Vec<Value>, Option<u64>)]) -> DbResult<Vec<u64>> {
    // `IMMEDIATE` rather than plain `BEGIN`: a deferred transaction takes its
    // write lock at the first write, which means a second writer can arrive in
    // between and turn the commit into a busy error after some of the work is
    // already done.
    conn.execute_batch("BEGIN IMMEDIATE").map_err(classify)?;
    let mut affected = Vec::with_capacity(writes.len());
    for (sql, params, expect) in writes {
        let result = write_one(conn, sql, params).and_then(|rows| match expect {
            Some(expected) if rows != *expected => Err(mismatch(sql, rows)),
            _ => Ok(rows),
        });
        match result {
            Ok(rows) => affected.push(rows),
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        }
    }
    conn.execute_batch("COMMIT").map_err(classify)?;
    Ok(affected)
}

fn write_one(conn: &Connection, sql: &str, params: &[Value]) -> DbResult<u64> {
    let mut stmt = conn.prepare(sql).map_err(classify)?;
    for (index, value) in params.iter().enumerate() {
        // The SQL is written with Postgres' `$1`, which SQLite reads as a named
        // parameter called `$1` — so the name is looked up first, and the
        // position is the fallback for anything written with a bare `?`.
        let name = format!("${}", index + 1);
        match stmt.parameter_index(&name).map_err(classify)? {
            Some(at) => stmt.raw_bind_parameter(at, Bound(value)),
            None => stmt.raw_bind_parameter(index + 1, Bound(value)),
        }
        .map_err(classify)?;
    }
    let before = conn.total_changes();
    // Not `raw_execute`, which refuses a statement that returns rows: a
    // `RETURNING` clause is legal here and its rows are simply not read.
    let mut rows = stmt.raw_query();
    while rows.next().map_err(classify)?.is_some() {}
    Ok(conn.total_changes() - before)
}

fn mismatch(sql: &str, actual: u64) -> DbError {
    let mut error = match actual {
        0 => DbError::new(
            db::ErrorClass::Server,
            "The row changed underneath you and was not updated.",
        ),
        _ => DbError::new(
            db::ErrorClass::Server,
            format!("Expected to change one row, but this would have changed {actual}."),
        ),
    };
    error.detail = Some(sql.into());
    error.hint = Some("Nothing was saved. Refresh and try again.".into());
    error
}

/// A [`db::Value`] as a bound parameter.
///
/// Borrowed all the way down: the text and byte kinds hand SQLite a pointer
/// into the value's own `Arc`, and `SQLITE_TRANSIENT` copies it before the
/// statement is stepped.
struct Bound<'a>(&'a Value);

impl ToSql for Bound<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match self.0 {
            Value::Null => ToSqlOutput::from(Null),
            Value::Bool(b) => ToSqlOutput::from(*b),
            Value::Int(i) => ToSqlOutput::from(*i),
            Value::Float(f) => ToSqlOutput::from(*f),
            // Everything textual keeps the text it arrived as. A timestamp
            // written back to SQLite is the string the user saw, which is the
            // only form the column is known to accept.
            Value::Text { text, .. } => ToSqlOutput::Borrowed(ValueRef::Text(text.as_bytes())),
            Value::Bytes(bytes) => ToSqlOutput::Borrowed(ValueRef::Blob(bytes)),
        })
    }
}
