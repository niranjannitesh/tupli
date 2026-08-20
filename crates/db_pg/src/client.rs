//! Opening a connection and running statements on it.
//!
//! Nothing here touches GPUI or a runtime: these are `async fn`s, and the app
//! decides which executor drives them. That is what keeps the driver testable
//! against a real server from a plain `#[tokio::test]`.
//!
//! One connection is one session, deliberately. Pooling would let two
//! statements from the same window land on different backends, which breaks
//! `SET`, temporary tables, and open transactions — all of which a database
//! client's users rely on without thinking about it.

use std::sync::Arc;

use db::{
    Cell, ColumnBuilder, ColumnMeta, ConnectionConfig, DbError, DbResult, ErrorClass, Notice,
    ResultSet, SslMode, Value,
};
use futures::StreamExt as _;
use postgres_types::{FromSql, Type};
use tokio_postgres::config::SslMode as PgSslMode;
use tokio_postgres::{Client, Config, Statement};

use crate::params::Param;
use crate::types::{self, Decoded};

/// Rows fetched in one go before the result is called truncated.
///
/// A person cannot read more than this, and the point of a limit is that a
/// mistyped `select * from events` should not pull 400 million rows into
/// memory before anyone can react.
pub const DEFAULT_MAX_ROWS: usize = 50_000;

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

/// The error for a statement that touched the wrong number of rows. Worded for
/// the person who pressed Commit, not for a log.
fn mismatch(write: &Write<'_>, actual: u64) -> DbError {
    let mut error = match actual {
        0 => DbError::new(
            ErrorClass::Server,
            "The row changed underneath you and was not updated.",
        ),
        _ => DbError::new(
            ErrorClass::Server,
            format!("Expected to change one row, but this would have changed {actual}."),
        ),
    };
    error.detail = Some(write.sql.into());
    error.hint = Some("Nothing was saved. Refresh and try again.".into());
    error
}

/// One open session.
pub struct PgConnection {
    client: Client,
    server_version: Arc<str>,
    /// Kept so a running statement can be cancelled from another task — it is
    /// a separate connection to the server, which is the only way Postgres
    /// accepts a cancel.
    cancel: tokio_postgres::CancelToken,
    /// Dropping this aborts the background task that drives the socket, which
    /// closes the connection.
    _driver: tokio::task::JoinHandle<()>,
    /// What the server has said out of band since anyone last asked.
    ///
    /// Notices arrive on the connection, not on the statement: the protocol
    /// interleaves them with the rows, and by the time `query` returns they
    /// have already gone past. So the driver task collects them here and the
    /// statement drains them afterwards, which attributes them correctly for
    /// as long as one connection runs one statement at a time — which is the
    /// arrangement this whole module is built on.
    notices: Arc<parking_lot::Mutex<Vec<Notice>>>,
}

impl PgConnection {
    /// Connect, authenticate, and put the session into a known state.
    pub async fn connect(config: &ConnectionConfig, password: Option<&str>) -> DbResult<Self> {
        let pg = pg_config(config, password);
        let tls = tls_connector(config)?;

        let (client, connection) = pg
            .connect(tls)
            .await
            .map_err(|e| classify(e, ErrorClass::Connection))?;

        // The connection future owns the socket; it has to be polled for
        // anything at all to happen on it. It ends when the client is dropped.
        //
        // Polled a message at a time rather than awaited whole, because
        // awaiting it throws away everything the server says that is not a
        // row: notices, warnings, and the `LISTEN` notifications a later
        // milestone will want.
        let notices: Arc<parking_lot::Mutex<Vec<Notice>>> = Arc::default();
        let collected = notices.clone();
        let mut connection = connection;
        let driver = tokio::spawn(async move {
            let mut stream =
                futures::stream::poll_fn(move |cx| connection.poll_message(cx)).boxed();
            while let Some(message) = stream.next().await {
                match message {
                    Ok(tokio_postgres::AsyncMessage::Notice(notice)) => {
                        collected.lock().push(notice_from(&notice));
                    }
                    // `LISTEN`/`NOTIFY` has no subscriber yet. Logged rather
                    // than dropped in silence, so that the day one exists it is
                    // clear the messages were arriving all along.
                    Ok(tokio_postgres::AsyncMessage::Notification(n)) => {
                        log::debug!("notify {}: {}", n.channel(), n.payload())
                    }
                    Ok(_) => {}
                    Err(error) => {
                        log::warn!("postgres connection closed: {error}");
                        break;
                    }
                }
            }
        });

        let cancel = client.cancel_token();
        let connection = Self {
            client,
            server_version: "".into(),
            cancel,
            _driver: driver,
            notices,
        };
        connection.prepare_session().await?;
        let server_version = connection.scalar("show server_version").await?;
        // Whatever the session setup said is not anybody's statement.
        connection.take_notices();
        Ok(Self {
            server_version: server_version.into(),
            ..connection
        })
    }

    /// Session settings the rest of the crate assumes.
    ///
    /// `UTC` is the important one: [`crate::types`] formats timestamps from the
    /// wire without asking anybody what zone they are in, and this is what
    /// makes that true rather than merely likely.
    async fn prepare_session(&self) -> DbResult<()> {
        self.client
            .batch_execute(
                "set time zone 'UTC';\n\
                 set client_encoding to 'UTF8';\n\
                 set bytea_output to 'hex';\n\
                 set extra_float_digits to 3;",
            )
            .await
            .map_err(|e| classify(e, ErrorClass::Connection))
    }

    pub fn server_version(&self) -> &Arc<str> {
        &self.server_version
    }

    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }

    /// Everything the server has said out of band since this was last called.
    ///
    /// Draining rather than reading: a notice belongs to the statement that
    /// provoked it, and leaving it in the buffer would attribute it to the next
    /// one as well.
    pub fn take_notices(&self) -> Vec<Notice> {
        std::mem::take(&mut *self.notices.lock())
    }

    /// A handle that cancels whatever this connection is currently running.
    ///
    /// Cheap to clone and safe to hold across tasks; cancelling an idle
    /// connection is a no-op, which is why it does not need to know whether a
    /// statement is in flight.
    pub fn canceller(&self) -> Canceller {
        Canceller(self.cancel.clone())
    }

    /// Run one statement.
    ///
    /// One statement, not a script: the extended protocol prepares what it is
    /// given, and a semicolon-separated batch is a syntax error. The editor
    /// sends the statement under the cursor, and a script runner will split
    /// first and call this per statement.
    pub async fn query(&self, sql: &str, max_rows: usize) -> DbResult<Outcome> {
        let statement = self
            .client
            .prepare(sql)
            .await
            .map_err(|e| classify(e, ErrorClass::Syntax))?;

        if statement.columns().is_empty() {
            let affected = self
                .client
                .execute(&statement, &[])
                .await
                .map_err(|e| classify(e, ErrorClass::Server))?;
            return Ok(Outcome::Affected(affected));
        }

        self.fetch(&statement, max_rows).await
    }

    async fn fetch(&self, statement: &Statement, max_rows: usize) -> DbResult<Outcome> {
        let mut builders: Vec<ColumnBuilder> = statement
            .columns()
            .iter()
            .map(|column| {
                let ty = column.type_();
                ColumnBuilder::new(ColumnMeta::new(
                    column.name(),
                    types::kind_for(ty),
                    ty.name(),
                ))
            })
            .collect();
        let column_types: Vec<Type> = statement
            .columns()
            .iter()
            .map(|c| c.type_().clone())
            .collect();

        let stream = self
            .client
            .query_raw(
                statement,
                std::iter::empty::<&(dyn tokio_postgres::types::ToSql + Sync)>(),
            )
            .await
            .map_err(|e| classify(e, ErrorClass::Server))?;
        futures::pin_mut!(stream);

        // One scratch buffer for the whole fetch. Every formatted value —
        // timestamps, numerics, uuids — is written here and copied into the
        // column, so a million rows cost no allocations beyond the columns
        // themselves.
        let mut scratch = String::new();
        let mut truncated = false;
        let mut count = 0usize;

        while let Some(row) = stream.next().await {
            let row = row.map_err(|e| classify(e, ErrorClass::Server))?;
            for (index, builder) in builders.iter_mut().enumerate() {
                let raw: Option<Raw> = row.get(index);
                match raw {
                    None => builder.push(None),
                    Some(Raw(bytes)) => {
                        match types::decode(&column_types[index], bytes, &mut scratch) {
                            Decoded::Bool(b) => builder.push(Some(Cell::Bool(b))),
                            Decoded::Int(i) => builder.push(Some(Cell::Int(i))),
                            Decoded::Float(f) => builder.push(Some(Cell::Float(f))),
                            Decoded::Bytes(b) => builder.push(Some(Cell::Bytes(b))),
                            Decoded::Text => builder.push(Some(Cell::Str(&scratch))),
                        }
                    }
                }
            }
            count += 1;
            if count >= max_rows {
                truncated = true;
                break;
            }
        }
        // Dropping the stream here closes the portal, which tells the server to
        // stop producing rows we are never going to read.
        drop(stream);

        let rows = ResultSet::new(builders.into_iter().map(ColumnBuilder::finish).collect());
        Ok(Outcome::Rows { rows, truncated })
    }

    /// The first column of the first row, as text. For `show`, `select
    /// version()`, and the handful of one-value catalog questions.
    pub async fn scalar(&self, sql: &str) -> DbResult<String> {
        match self.query(sql, 1).await? {
            Outcome::Rows { rows, .. } => {
                let mut scratch = String::new();
                let column = rows
                    .columns
                    .first()
                    .ok_or_else(|| DbError::internal("expected one column"))?;
                Ok(match column.render(0, &mut scratch) {
                    db::CellText::Borrowed(s) => s.to_string(),
                    db::CellText::Formatted => scratch,
                    db::CellText::Null => String::new(),
                })
            }
            Outcome::Affected(_) => Err(DbError::internal("expected a result set")),
        }
    }

    /// Run one statement with bound parameters, for its effect.
    ///
    /// Separate from [`Self::query`] because the write path is the only place
    /// with parameters, and the read path's hot loop should not pay a slice
    /// walk per call to discover it has none.
    pub async fn execute_params(&self, sql: &str, params: &[Value]) -> DbResult<u64> {
        let bound: Vec<Param<'_>> = params.iter().map(Param).collect();
        let slots: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = bound
            .iter()
            .map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        self.client
            .execute(sql, &slots)
            .await
            .map_err(|e| classify(e, ErrorClass::Server))
    }

    /// Apply a set of writes as one transaction.
    ///
    /// All of it or none of it: a commit that half-applied would leave the
    /// grid showing rows that are partly saved, with no way to say which. A
    /// statement that touches the wrong number of rows is treated as a failure
    /// for the same reason — an `UPDATE` by primary key that matched nothing
    /// means the row moved underneath the person editing it, and the database
    /// will not raise an error for that.
    pub async fn apply(&self, writes: &[Write<'_>]) -> DbResult<Vec<u64>> {
        self.client
            .batch_execute("BEGIN")
            .await
            .map_err(|e| classify(e, ErrorClass::Server))?;
        let mut affected = Vec::with_capacity(writes.len());
        for write in writes {
            let result = match self.execute_params(write.sql, write.params).await {
                Ok(rows) => match write.expect_rows {
                    Some(expected) if rows != expected => Err(mismatch(write, rows)),
                    _ => Ok(rows),
                },
                Err(error) => Err(error),
            };
            match result {
                Ok(rows) => affected.push(rows),
                Err(error) => {
                    // Best effort: the transaction is already aborted on the
                    // server, and a rollback that fails changes nothing about
                    // what to report.
                    let _ = self.client.batch_execute("ROLLBACK").await;
                    return Err(error);
                }
            }
        }
        self.client
            .batch_execute("COMMIT")
            .await
            .map_err(|e| classify(e, ErrorClass::Server))?;
        Ok(affected)
    }

    /// Run a statement for its effect, ignoring anything it returns.
    pub async fn execute(&self, sql: &str) -> DbResult<u64> {
        match self.query(sql, 0).await? {
            Outcome::Affected(n) => Ok(n),
            Outcome::Rows { rows, .. } => Ok(rows.row_count() as u64),
        }
    }

    pub(crate) fn client(&self) -> &Client {
        &self.client
    }
}

/// Cancels whatever a connection is running, from anywhere.
#[derive(Clone)]
pub struct Canceller(tokio_postgres::CancelToken);

impl Canceller {
    /// Ask the server to cancel. Best-effort by design — Postgres' cancel
    /// protocol is advisory, and a statement that has already finished simply
    /// ignores it.
    pub async fn cancel(&self) {
        if let Err(error) = self.0.cancel_query(native_tls_connector_permissive()).await {
            log::debug!("cancel request failed: {error}");
        }
    }
}

/// The cancel request opens a *new* connection, and it carries no credentials
/// worth protecting — only the backend pid and a secret key the server itself
/// issued. Verifying a certificate here would mean threading the whole TLS
/// configuration through every cancel path for no gain in what an attacker
/// could learn.
fn native_tls_connector_permissive() -> postgres_native_tls::MakeTlsConnector {
    let connector = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()
        .expect("a permissive TLS connector always builds");
    postgres_native_tls::MakeTlsConnector::new(connector)
}

fn pg_config(config: &ConnectionConfig, password: Option<&str>) -> Config {
    let mut pg = Config::new();
    pg.host(&config.host)
        .port(config.port)
        .user(&config.user)
        .application_name("tupli");
    if !config.database.is_empty() {
        pg.dbname(&config.database);
    }
    if let Some(password) = password {
        pg.password(password);
    }
    // tokio-postgres only knows three modes; the two verifying ones are
    // `require` on the wire plus stricter checks in the TLS connector.
    pg.ssl_mode(match config.ssl_mode {
        SslMode::Disable => PgSslMode::Disable,
        SslMode::Allow | SslMode::Prefer => PgSslMode::Prefer,
        SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull => PgSslMode::Require,
    });
    pg.keepalives(config.keep_alive);
    pg.connect_timeout(std::time::Duration::from_secs(15));
    pg
}

fn tls_connector(config: &ConnectionConfig) -> DbResult<postgres_native_tls::MakeTlsConnector> {
    let mut builder = native_tls::TlsConnector::builder();

    // `require` encrypts but does not verify — that is what the mode means, and
    // pretending otherwise would break every connection to a server with a
    // self-signed certificate, which is most internal ones.
    if !config.ssl_mode.verifies_certificate() {
        builder.danger_accept_invalid_certs(true);
        builder.danger_accept_invalid_hostnames(true);
    } else if config.ssl_mode == SslMode::VerifyCa {
        // verify-ca checks the chain but not the name.
        builder.danger_accept_invalid_hostnames(true);
    }

    if let Some(path) = config.ssl_root_cert.as_deref().filter(|p| !p.is_empty()) {
        let pem = std::fs::read(path)
            .map_err(|e| DbError::connection(format!("cannot read root certificate: {e}")))?;
        let certificate = native_tls::Certificate::from_pem(&pem)
            .map_err(|e| DbError::connection(format!("invalid root certificate: {e}")))?;
        builder.add_root_certificate(certificate);
    }

    if let (Some(cert), Some(key)) = (
        config.ssl_cert.as_deref().filter(|p| !p.is_empty()),
        config.ssl_key.as_deref().filter(|p| !p.is_empty()),
    ) {
        let cert = std::fs::read(cert)
            .map_err(|e| DbError::connection(format!("cannot read client certificate: {e}")))?;
        let key = std::fs::read(key)
            .map_err(|e| DbError::connection(format!("cannot read client key: {e}")))?;
        let identity = native_tls::Identity::from_pkcs8(&cert, &key)
            .map_err(|e| DbError::connection(format!("invalid client certificate: {e}")))?;
        builder.identity(identity);
    }

    let connector = builder
        .build()
        .map_err(|e| DbError::connection(format!("TLS setup failed: {e}")))?;
    Ok(postgres_native_tls::MakeTlsConnector::new(connector))
}

/// A column's bytes, straight off the wire.
///
/// tokio-postgres has no way to hand out a raw value, so this is a `FromSql`
/// that accepts every type and decodes nothing. All the decoding this crate
/// does is in [`crate::types`], which needs the bytes and the type, not a
/// pre-chosen Rust type.
struct Raw<'a>(&'a [u8]);

impl<'a> FromSql<'a> for Raw<'a> {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Raw(raw))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

/// A `NoticeResponse` off the wire → [`Notice`].
///
/// The same shape as an error, because on the wire it is one: same fields, same
/// codes, a severity that says it is not fatal.
fn notice_from(notice: &tokio_postgres::error::DbError) -> Notice {
    Notice {
        severity: notice.severity().into(),
        message: notice.message().into(),
        detail: notice.detail().map(Into::into),
        hint: notice.hint().map(Into::into),
    }
}

/// tokio-postgres error → [`DbError`], keeping everything the server said.
///
/// `fallback` is the class to use when the error came from the client rather
/// than the server and so has no SQLSTATE to classify it by.
pub(crate) fn classify(error: tokio_postgres::Error, fallback: ErrorClass) -> DbError {
    let Some(server) = error.as_db_error() else {
        let message = match error.source() {
            // The interesting part of a client-side error is almost always its
            // cause: "connection error" alone tells nobody anything.
            Some(source) => format!("{error}: {source}"),
            None => error.to_string(),
        };
        return DbError::new(fallback, message);
    };

    let code = server.code().code().to_string();
    let mut db_error = DbError::new(db::error::class_for_sqlstate(&code), server.message());
    db_error.code = Some(code.into());
    db_error.detail = server.detail().map(Into::into);
    db_error.hint = server.hint().map(Into::into);
    db_error.table = server.table().map(Into::into);
    db_error.column = server.column().map(Into::into);
    db_error.constraint = server.constraint().map(Into::into);
    db_error.position = match server.position() {
        Some(tokio_postgres::error::ErrorPosition::Original(p)) => Some(*p),
        // An error inside a function body points into SQL the user never
        // typed, so there is nothing in the editor to point at.
        _ => None,
    };
    db_error
}

use std::error::Error as _;
