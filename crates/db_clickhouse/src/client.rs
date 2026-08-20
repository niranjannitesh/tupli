//! Opening a connection and running one statement down it.
//!
//! Plain `async fn`s, no GPUI and no runtime of its own — the same shape as
//! `db_pg`'s and `db_redis`'s clients, so this can be driven against a real
//! server from a `#[tokio::test]`.
//!
//! Two things are structural rather than incidental.
//!
//! The connection is **not** multiplexed, and cannot be. A ClickHouse session
//! is a state machine: a `Query` is followed by an unbounded run of packets
//! ending in `EndOfStream`, and nothing in between says which query it belongs
//! to. So a query holds the read half for its whole life, and two queries on
//! one connection serialise. The app already opens a connection per pane.
//!
//! And the socket is split rather than owned whole, because [`Driver::cancel`]
//! has to write while the read loop is blocked on the same socket. The two
//! halves get their own locks: the writer's is held for the length of one
//! packet, the reader's for the length of one query.
//!
//! [`Driver::cancel`]: db::Driver::cancel

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use db::{
    Cell, ColumnBuilder, ColumnMeta, ConnectionConfig, DbError, DbResult, ErrorClass, Notice,
    ResultSet,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use crate::block::{self, Block};
use crate::config::ClickHouseConfig;
use crate::protocol::{self, CLIENT_REVISION};
use crate::types::Cellv;
use crate::wire::{self, io_error, malformed, Reader};

/// How long to wait for the socket, the TLS handshake and the `Hello`.
///
/// The same fifteen seconds `db_pg` allows. A server that has not said hello by
/// then is either not there or not reachable, and both are better reported than
/// waited on.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

type Read = Box<dyn AsyncRead + Unpin + Send>;
type WriteHalf = Box<dyn AsyncWrite + Unpin + Send>;

/// What the server said about itself in the handshake.
#[derive(Clone, Debug)]
pub struct ServerInfo {
    pub name: String,
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// The revision the server *speaks*, which is normally far above the one
    /// this client asked for. Read but not acted on: the fields on the wire are
    /// gated by the client's number, not this one.
    pub revision: u64,
    /// The server's own time zone. Kept for the status bar; not used to shift
    /// any value — see `types::timestamp`.
    pub timezone: Option<String>,
    pub display_name: Option<String>,
}

/// One open session.
pub struct ClickHouseConnection {
    reader: Mutex<Read>,
    /// Behind an `Arc` because `cancel` returns a `'static` future and has to
    /// reach the socket without borrowing the connection.
    writer: Arc<Mutex<WriteHalf>>,
    config: ClickHouseConfig,
    server: ServerInfo,
    server_version: Arc<str>,
    /// Set the first time the socket fails, so the app can offer to reconnect
    /// rather than sending into a dead connection. Shared with `cancel`, which
    /// can be the one that discovers it.
    closed: Arc<AtomicBool>,
    /// Server-side log lines, when the session has asked for them. Drained by
    /// `take_notices`, which the trait defines as synchronous — hence a plain
    /// mutex rather than tokio's. It is only ever held for a push or a take,
    /// never across an await.
    notices: std::sync::Mutex<Vec<Notice>>,
}

/// What one statement produced, before it is turned into a [`db::Outcome`].
#[derive(Debug)]
pub struct Fetched {
    /// `None` when the server never named any columns, which is what DDL and a
    /// finished `insert` look like.
    pub rows: Option<ResultSet>,
    pub truncated: bool,
    /// From `Progress`, and only ever what the server reported. A statement
    /// that wrote nothing reports nothing rather than a guessed zero.
    pub written_rows: u64,
}

impl ClickHouseConnection {
    /// Connect using a saved connection record and a password from the
    /// keychain.
    ///
    /// The password is borrowed for the handshake and never stored: nothing
    /// after the `Hello` needs it.
    pub async fn connect(config: &ConnectionConfig, password: Option<&str>) -> DbResult<Self> {
        Self::open(ClickHouseConfig::from_config(config), password).await
    }

    pub async fn open(config: ClickHouseConfig, password: Option<&str>) -> DbResult<Self> {
        let connect = async {
            let stream = tokio::net::TcpStream::connect((config.host.as_str(), config.port))
                .await
                .map_err(|error| {
                    DbError::connection(format!(
                        "Could not reach {}:{}: {error}",
                        config.host, config.port
                    ))
                })?;
            // The protocol is a run of small writes followed by a wait. Nagle
            // would hold the `Query` packet back for a round trip that has
            // nothing to piggyback on.
            let _ = stream.set_nodelay(true);
            match config.tls {
                false => Ok(split(stream)),
                true => Ok(split(tls(&config, stream).await?)),
            }
        };
        let (reader, writer) = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
            Ok(halves) => halves?,
            Err(_) => {
                return Err(DbError::connection(format!(
                    "{}:{} did not answer within {} seconds.",
                    config.host,
                    config.port,
                    CONNECT_TIMEOUT.as_secs()
                )))
            }
        };

        let mut connection = Self {
            reader: Mutex::new(reader),
            writer: Arc::new(Mutex::new(writer)),
            config,
            server: ServerInfo {
                name: String::new(),
                major: 0,
                minor: 0,
                patch: 0,
                revision: 0,
                timezone: None,
                display_name: None,
            },
            server_version: "".into(),
            closed: Arc::new(AtomicBool::new(false)),
            notices: std::sync::Mutex::new(Vec::new()),
        };
        connection.server = match tokio::time::timeout(
            CONNECT_TIMEOUT,
            connection.handshake(password.unwrap_or_default()),
        )
        .await
        {
            Ok(server) => server?,
            Err(_) => return Err(DbError::connection("The server accepted the connection but never said hello. If this is port 8123, that is the HTTP interface — tupli speaks the native protocol on 9000.")),
        };
        connection.server_version = format!(
            "{}.{}.{}",
            connection.server.major, connection.server.minor, connection.server.patch
        )
        .into();
        Ok(connection)
    }

    pub fn config(&self) -> &ClickHouseConfig {
        &self.config
    }

    pub fn server(&self) -> &ServerInfo {
        &self.server
    }

    pub fn server_version(&self) -> &Arc<str> {
        &self.server_version
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    pub fn is_read_only(&self) -> bool {
        self.config.is_read_only()
    }

    pub fn take_notices(&self) -> Vec<Notice> {
        match self.notices.lock() {
            Ok(mut notices) => std::mem::take(&mut *notices),
            // A poisoned lock means a panic while a log block was being
            // converted. The notices are lost either way; propagating the
            // panic into the query that noticed would be worse.
            Err(_) => Vec::new(),
        }
    }

    /// Say hello and hear one back.
    ///
    /// The one exchange where the client writes before it knows anything about
    /// the other end. A server that is really an HTTP listener answers this
    /// with an HTTP error, which does not parse as a `Hello` — hence the
    /// message that names the likely mistake.
    async fn handshake(&self, password: &str) -> DbResult<ServerInfo> {
        let mut packet = Vec::new();
        wire::write_uvarint(&mut packet, protocol::client::HELLO);
        wire::write_string(&mut packet, protocol::CLIENT_NAME);
        wire::write_uvarint(&mut packet, protocol::CLIENT_VERSION_MAJOR);
        wire::write_uvarint(&mut packet, protocol::CLIENT_VERSION_MINOR);
        wire::write_uvarint(&mut packet, CLIENT_REVISION);
        wire::write_string(&mut packet, &self.config.database);
        wire::write_string(&mut packet, &self.config.user);
        wire::write_string(&mut packet, password);
        self.send(&packet).await?;

        let mut guard = self.reader.lock().await;
        let reader: Reader<'_> = &mut **guard;
        match wire::read_uvarint(reader).await? {
            protocol::server::HELLO => {}
            protocol::server::EXCEPTION => {
                // Bad password, unknown user, unknown database. Reported as a
                // connection error because that is what the user has to go and
                // fix.
                let error = read_exception(reader).await?;
                return Err(DbError {
                    class: ErrorClass::Connection,
                    ..error
                });
            }
            other => {
                return Err(malformed(format!(
                    "{} where a handshake should be",
                    protocol::server::name(other)
                )))
            }
        }

        let mut server = ServerInfo {
            name: wire::read_string(reader).await?,
            major: wire::read_uvarint(reader).await?,
            minor: wire::read_uvarint(reader).await?,
            revision: wire::read_uvarint(reader).await?,
            patch: 0,
            timezone: None,
            display_name: None,
        };
        // Gated on what *this* client asked for, not on what the server can
        // do: the server writes the fields the client's revision covers.
        if CLIENT_REVISION >= protocol::MIN_REVISION_WITH_SERVER_TIMEZONE {
            server.timezone = Some(wire::read_string(reader).await?);
        }
        if CLIENT_REVISION >= protocol::MIN_REVISION_WITH_SERVER_DISPLAY_NAME {
            server.display_name = Some(wire::read_string(reader).await?);
        }
        if CLIENT_REVISION >= protocol::MIN_REVISION_WITH_VERSION_PATCH {
            server.patch = wire::read_uvarint(reader).await?;
        }
        Ok(server)
    }

    /// Round-trip a `Ping`, which is the only way to ask a ClickHouse
    /// connection whether it is still there without running a query.
    pub async fn ping(&self) -> DbResult<()> {
        let mut packet = Vec::new();
        wire::write_uvarint(&mut packet, protocol::client::PING);
        self.send(&packet).await?;

        let mut guard = self.reader.lock().await;
        let reader: Reader<'_> = &mut **guard;
        loop {
            match self.classify(read_packet(reader).await)? {
                Packet::Pong => return Ok(()),
                // A `Progress` can still be in flight from a query that was
                // cancelled. Skipping it is right; anything else here means
                // the connection is out of step and cannot be recovered.
                Packet::Progress(_) => continue,
                other => {
                    return Err(malformed(format!(
                        "{} in answer to a ping",
                        other.describe()
                    )))
                }
            }
        }
    }

    /// Run one statement and bring back at most `max_rows` rows.
    pub async fn fetch(&self, statement: &str, max_rows: usize) -> DbResult<Fetched> {
        if self.is_closed() {
            return Err(DbError::connection(
                "This connection is closed. Reconnect and try again.",
            ));
        }
        if self.is_read_only() && !is_read_only_statement(statement) {
            return Err(DbError::new(
                ErrorClass::Server,
                format!(
                    "This connection is read-only, so tupli will not send {}.",
                    leading_keyword(statement).to_uppercase()
                ),
            ));
        }

        // Held for the whole query: everything from here to `EndOfStream`
        // belongs to this statement and there is nothing on the wire that
        // would let a second one tell its packets apart.
        let mut guard = self.reader.lock().await;

        let mut packet = Vec::new();
        self.write_query(&mut packet, statement);
        // The external-data terminator. Not optional — the server reads blocks
        // until an empty one before it starts, so a `Query` without this just
        // hangs.
        block::write_empty_block(&mut packet, CLIENT_REVISION);
        self.send(&packet).await?;

        let reader: Reader<'_> = &mut **guard;
        let mut state = Fetch::new(max_rows, expects_client_data(statement));
        loop {
            let packet = self.classify(read_packet(reader).await);
            let packet = match packet {
                Ok(packet) => packet,
                // A statement this client asked to stop is not a failure, and
                // the rows already read are still the rows the user asked for.
                Err(error) if error.is_canceled() && state.canceled => break,
                Err(error) => return Err(error),
            };
            match packet {
                Packet::EndOfStream => break,
                Packet::Data(data) => {
                    if state.wants_terminator && data.is_header() {
                        // The server has sent the shape of the table it is
                        // about to be given rows for, and is now waiting. This
                        // client never sends rows — an `insert … values` is
                        // carried inside the query text — so the answer is
                        // "that is all", which is an empty block.
                        state.wants_terminator = false;
                        let mut terminator = Vec::new();
                        block::write_empty_block(&mut terminator, CLIENT_REVISION);
                        self.send(&terminator).await?;
                    }
                    state.absorb(data)?;
                    if state.truncated && !state.canceled {
                        state.canceled = true;
                        self.send_cancel().await?;
                    }
                }
                // Totals and extremes are extra blocks with the same shape as
                // the result and a different meaning; folding them into the
                // rows would put a summary row in the middle of the data.
                Packet::Totals | Packet::Extremes => {}
                Packet::Progress(progress) => state.written_rows = progress.written_rows,
                Packet::Log(log) => self.absorb_log(&log),
                Packet::ProfileInfo | Packet::ProfileEvents => {}
                Packet::TableColumns | Packet::PartUuids => {}
                Packet::Pong => {}
            }
        }
        Ok(state.finish())
    }

    /// Ask the server to stop the statement in flight.
    ///
    /// Best effort by nature: the query may already have finished, in which
    /// case the packet is read as the start of the next one — which is why the
    /// read loop keeps going until `EndOfStream` either way rather than
    /// treating a cancel as an ending.
    pub fn cancel(&self) -> futures::future::BoxFuture<'static, ()> {
        let writer = self.writer.clone();
        let closed = self.closed.clone();
        Box::pin(async move {
            let mut packet = Vec::new();
            wire::write_uvarint(&mut packet, protocol::client::CANCEL);
            let mut guard = writer.lock().await;
            if guard.write_all(&packet).await.is_err() || guard.flush().await.is_err() {
                closed.store(true, Ordering::Relaxed);
            }
        })
    }

    async fn send_cancel(&self) -> DbResult<()> {
        let mut packet = Vec::new();
        wire::write_uvarint(&mut packet, protocol::client::CANCEL);
        self.send(&packet).await
    }

    /// Write one whole packet.
    ///
    /// Assembled by the caller and handed over in one piece: a packet that was
    /// half written when something failed would leave the connection
    /// unparseable, and there is no resynchronisation point to recover to.
    async fn send(&self, packet: &[u8]) -> DbResult<()> {
        let mut guard = self.writer.lock().await;
        let result = match guard.write_all(packet).await {
            Ok(()) => guard.flush().await,
            Err(error) => Err(error),
        };
        result.map_err(|error| {
            self.closed.store(true, Ordering::Relaxed);
            io_error("sending a request", error)
        })
    }

    /// The `Query` packet.
    ///
    /// Every field below the first is gated on [`CLIENT_REVISION`], including
    /// the ones that are always written at the revision this client asks for.
    /// Writing them unconditionally would work today and quietly become wrong
    /// the moment somebody lowers the constant.
    fn write_query(&self, out: &mut Vec<u8>, statement: &str) {
        wire::write_uvarint(out, protocol::client::QUERY);
        // An empty query id means "you name it". A client-chosen id would only
        // be worth the trouble if cancellation went over a second connection,
        // and it does not — `Cancel` goes down this one.
        wire::write_string(out, "");

        if CLIENT_REVISION >= protocol::MIN_REVISION_WITH_CLIENT_INFO {
            wire::write_u8(out, protocol::QUERY_KIND_INITIAL);
            // Who *started* the query, as opposed to who is asking. The same
            // thing here, and the fields exist for a node relaying another
            // node's work.
            wire::write_string(out, "");
            wire::write_string(out, "");
            wire::write_string(out, "0.0.0.0:0");
            if CLIENT_REVISION >= protocol::MIN_REVISION_WITH_INITIAL_QUERY_START_TIME {
                // Microseconds since the epoch, fixed width rather than a
                // varint. Zero: it dates the query that *started* the chain,
                // and for a query that starts here the server's own stamp is
                // the better clock.
                wire::write_i64(out, 0);
            }
            wire::write_u8(out, protocol::INTERFACE_TCP);
            // The OS user and hostname go in `system.query_log`. Left empty:
            // they identify the person rather than the query, and a database
            // client should not be the reason a login name ends up in a shared
            // log table.
            wire::write_string(out, "");
            wire::write_string(out, "");
            wire::write_string(out, protocol::CLIENT_NAME);
            wire::write_uvarint(out, protocol::CLIENT_VERSION_MAJOR);
            wire::write_uvarint(out, protocol::CLIENT_VERSION_MINOR);
            wire::write_uvarint(out, CLIENT_REVISION);
            if CLIENT_REVISION >= protocol::MIN_REVISION_WITH_QUOTA_KEY_IN_CLIENT_INFO {
                wire::write_string(out, "");
            }
            if CLIENT_REVISION >= protocol::MIN_REVISION_WITH_DISTRIBUTED_DEPTH {
                wire::write_uvarint(out, 0);
            }
            if CLIENT_REVISION >= protocol::MIN_REVISION_WITH_VERSION_PATCH {
                wire::write_uvarint(out, protocol::CLIENT_VERSION_PATCH);
            }
            if CLIENT_REVISION >= protocol::MIN_REVISION_WITH_OPENTELEMETRY {
                wire::write_u8(out, 0);
            }
            if CLIENT_REVISION >= protocol::MIN_REVISION_WITH_PARALLEL_REPLICAS {
                // Not collaborating with an initiator, so neither of the two
                // replica counts that follow means anything.
                wire::write_uvarint(out, 0);
                wire::write_uvarint(out, 0);
                wire::write_uvarint(out, 0);
            }
        }

        // Per-query settings, as name/value pairs ended by an empty name. None
        // are sent: every setting this client might want — a row limit, a
        // timeout — is something the app already enforces itself, and a
        // setting the server rejects would fail the query rather than the
        // setting.
        wire::write_string(out, "");
        if CLIENT_REVISION >= protocol::MIN_REVISION_WITH_INTERSERVER_SECRET {
            wire::write_string(out, "");
        }
        wire::write_uvarint(out, protocol::STAGE_COMPLETE);
        // No compression — see the `compress` module.
        wire::write_uvarint(out, 0);
        wire::write_string(out, statement);
    }

    fn absorb_log(&self, log: &Block) {
        if let Ok(mut notices) = self.notices.lock() {
            notices.extend(log_notices(log));
        }
    }

    /// Mark the connection dead when the failure was the socket rather than
    /// the statement.
    fn classify(&self, result: DbResult<Packet>) -> DbResult<Packet> {
        if let Err(error) = &result {
            if error.class == ErrorClass::Connection || error.class == ErrorClass::Internal {
                self.closed.store(true, Ordering::Relaxed);
            }
        }
        result
    }
}

/// The rows of one statement as they accumulate.
struct Fetch {
    builders: Vec<ColumnBuilder>,
    named: bool,
    rows: usize,
    max_rows: usize,
    truncated: bool,
    canceled: bool,
    wants_terminator: bool,
    written_rows: u64,
}

impl Fetch {
    fn new(max_rows: usize, wants_terminator: bool) -> Self {
        Self {
            builders: Vec::new(),
            named: false,
            rows: 0,
            max_rows,
            truncated: false,
            canceled: false,
            wants_terminator,
            written_rows: 0,
        }
    }

    /// Take what fits of one block.
    ///
    /// The limit is applied per block rather than by stopping the stream,
    /// because stopping the stream is asynchronous: the cancel is sent after
    /// this returns and blocks already in flight still arrive. Anything past
    /// the limit is dropped here and reported as truncation.
    fn absorb(&mut self, block: Block) -> DbResult<()> {
        if !self.named && !block.columns.is_empty() {
            self.named = true;
            self.builders = block
                .columns
                .iter()
                .map(|column| {
                    ColumnBuilder::new(ColumnMeta {
                        name: column.name.clone(),
                        type_name: column.type_name.clone(),
                        kind: column.ty.kind(),
                        nullable: column.ty.is_nullable(),
                        // ClickHouse's primary key is a sorting key, not a
                        // uniqueness constraint, so marking a column with it
                        // would claim something false. See `introspect`.
                        is_pk: false,
                        is_fk: false,
                    })
                })
                .collect();
        }
        if block.rows == 0 {
            return Ok(());
        }
        if block.columns.len() != self.builders.len() {
            return Err(malformed(format!(
                "a block of {} columns in a result of {}",
                block.columns.len(),
                self.builders.len()
            )));
        }

        let room = self.max_rows.saturating_sub(self.rows);
        let take = block.rows.min(room);
        if block.rows > take {
            self.truncated = true;
        }
        for (builder, column) in self.builders.iter_mut().zip(&block.columns) {
            for value in column.values.iter().take(take) {
                builder.push(value.as_cell());
            }
        }
        self.rows += take;
        Ok(())
    }

    fn finish(self) -> Fetched {
        Fetched {
            rows: match self.named {
                true => Some(ResultSet::new(
                    self.builders
                        .into_iter()
                        .map(ColumnBuilder::finish)
                        .collect(),
                )),
                false => None,
            },
            truncated: self.truncated,
            written_rows: self.written_rows,
        }
    }
}

/// What the server can say inside a query.
enum Packet {
    Data(Block),
    /// The extra rows `with totals` and `with extremes` produce, and the
    /// per-query event counters. Their blocks are read off the wire — nothing
    /// here is skippable — and then dropped, so the variants carry nothing.
    Totals,
    Extremes,
    ProfileEvents,
    Log(Block),
    Progress(Progress),
    ProfileInfo,
    TableColumns,
    PartUuids,
    Pong,
    EndOfStream,
}

impl Packet {
    fn describe(&self) -> &'static str {
        match self {
            Self::Data(_) => "a data block",
            Self::Totals => "a totals block",
            Self::Extremes => "an extremes block",
            Self::Log(_) => "a log block",
            Self::ProfileEvents => "a profile-events block",
            Self::Progress(_) => "a progress report",
            Self::ProfileInfo => "a profile report",
            Self::TableColumns => "a column description",
            Self::PartUuids => "a list of parts",
            Self::Pong => "a pong",
            Self::EndOfStream => "the end of the stream",
        }
    }
}

/// How far along the server says it is. Only `written_rows` is used, and only
/// because it is the one honest answer to "how many rows did that write".
struct Progress {
    written_rows: u64,
}

/// Read one packet, or turn the server's exception into an error.
async fn read_packet(reader: Reader<'_>) -> DbResult<Packet> {
    let tag = wire::read_uvarint(reader).await?;
    match tag {
        protocol::server::DATA => Ok(Packet::Data(
            block::read_block(reader, CLIENT_REVISION).await?,
        )),
        protocol::server::EXCEPTION => Err(read_exception(reader).await?),
        protocol::server::PROGRESS => Ok(Packet::Progress(read_progress(reader).await?)),
        protocol::server::PONG => Ok(Packet::Pong),
        protocol::server::END_OF_STREAM => Ok(Packet::EndOfStream),
        protocol::server::PROFILE_INFO => {
            read_profile_info(reader).await?;
            Ok(Packet::ProfileInfo)
        }
        protocol::server::TOTALS => {
            block::read_block(reader, CLIENT_REVISION).await?;
            Ok(Packet::Totals)
        }
        protocol::server::EXTREMES => {
            block::read_block(reader, CLIENT_REVISION).await?;
            Ok(Packet::Extremes)
        }
        // Log and profile-event blocks are never compressed, whatever the
        // query asked for — which costs this client nothing, since it asks for
        // no compression anyway.
        protocol::server::LOG => Ok(Packet::Log(
            block::read_block(reader, CLIENT_REVISION).await?,
        )),
        protocol::server::PROFILE_EVENTS => {
            block::read_block(reader, CLIENT_REVISION).await?;
            Ok(Packet::ProfileEvents)
        }
        protocol::server::TABLE_COLUMNS => {
            let _external_table = wire::read_string(reader).await?;
            let _description = wire::read_string(reader).await?;
            Ok(Packet::TableColumns)
        }
        protocol::server::PART_UUIDS => {
            let count = wire::read_uvarint(reader).await?;
            if count > 1_000_000 {
                return Err(malformed(format!("a list of {count} part uuids")));
            }
            wire::read_exact(reader, count as usize * 16).await?;
            Ok(Packet::PartUuids)
        }
        // The server is asking this client to hand out work in a distributed
        // read. It only asks a client that opted in, and there is no way to
        // answer without desynchronising, so it ends the read by name.
        other => Err(malformed(format!(
            "{}, which tupli has no answer for",
            protocol::server::name(other)
        ))),
    }
}

/// The server's exception, including everything it was wrapped in.
///
/// The nested chain is where the real cause usually is: ClickHouse routinely
/// reports `Cannot execute query` wrapping the sentence that says why. The
/// outermost is the message and the chain becomes the detail, which is where
/// the Messages tab shows it.
fn read_exception(reader: Reader<'_>) -> futures::future::BoxFuture<'_, DbResult<DbError>> {
    Box::pin(async move {
        let code = wire::read_i32(reader).await?;
        let name = wire::read_string(reader).await?;
        let message = wire::read_string(reader).await?;
        let _stack_trace = wire::read_string(reader).await?;
        let has_nested = wire::read_u8(reader).await? != 0;
        let nested = match has_nested {
            true => Some(read_exception(reader).await?),
            false => None,
        };

        // The server prefixes its own message with the code and name; showing
        // that again in front of it would read as a stutter.
        let message = message
            .strip_prefix(&format!("{name}: "))
            .unwrap_or(&message)
            .to_string();
        Ok(DbError {
            code: Some(format!("{code} {name}").into()),
            detail: nested.map(|nested| nested.full_text().into()),
            ..DbError::new(class_for(code), message)
        })
    })
}

/// A ClickHouse error code → what the app should do about it.
///
/// The codes are ClickHouse's own numbers rather than SQLSTATE, so this is a
/// list and not a prefix rule. Only the ones that change behaviour are named:
/// [`ErrorClass::Syntax`] puts a caret in the editor and offers no retry,
/// [`ErrorClass::Connection`] offers to edit the connection, and
/// [`ErrorClass::Canceled`] unwinds without reporting anything at all.
fn class_for(code: i32) -> ErrorClass {
    match code {
        // SYNTAX_ERROR, UNKNOWN_IDENTIFIER, UNKNOWN_TABLE, UNKNOWN_FUNCTION,
        // UNKNOWN_DATABASE, NUMBER_OF_ARGUMENTS_DOESNT_MATCH, TYPE_MISMATCH,
        // ILLEGAL_TYPE_OF_ARGUMENT, NOT_AN_AGGREGATE — every one of them is a
        // statement the user has to change.
        62 | 47 | 60 | 46 | 81 | 42 | 53 | 43 | 215 => ErrorClass::Syntax,
        // AUTHENTICATION_FAILED, REQUIRED_PASSWORD, UNKNOWN_USER,
        // IP_ADDRESS_NOT_ALLOWED, NETWORK_ERROR, SOCKET_TIMEOUT.
        516 | 517 | 192 | 193 | 210 | 209 => ErrorClass::Connection,
        // QUERY_WAS_CANCELLED.
        394 => ErrorClass::Canceled,
        _ => ErrorClass::Server,
    }
}

async fn read_progress(reader: Reader<'_>) -> DbResult<Progress> {
    let _read_rows = wire::read_uvarint(reader).await?;
    let _read_bytes = wire::read_uvarint(reader).await?;
    let _total_rows = wire::read_uvarint(reader).await?;
    let written_rows = match CLIENT_REVISION >= protocol::MIN_REVISION_WITH_CLIENT_WRITE_INFO {
        true => {
            let written_rows = wire::read_uvarint(reader).await?;
            let _written_bytes = wire::read_uvarint(reader).await?;
            written_rows
        }
        false => 0,
    };
    Ok(Progress { written_rows })
}

/// Counters about the query that has just run. Read and dropped: the app shows
/// its own row count and timing, and a second set that disagrees by a block
/// would raise more questions than it answers.
///
/// Read it must be, though. This packet is the reason a working driver and a
/// broken one look identical until the last few bytes of a result: it arrives
/// after all the data, and getting its width wrong leaves exactly enough
/// rubbish on the wire to be misread as the start of the next query.
async fn read_profile_info(reader: Reader<'_>) -> DbResult<()> {
    // rows, blocks, bytes.
    for _ in 0..3 {
        wire::read_uvarint(reader).await?;
    }
    // applied_limit, rows_before_limit, calculated_rows_before_limit.
    wire::read_u8(reader).await?;
    wire::read_uvarint(reader).await?;
    wire::read_u8(reader).await?;
    if CLIENT_REVISION >= MIN_REVISION_WITH_ROWS_BEFORE_AGGREGATION {
        wire::read_u8(reader).await?;
        wire::read_uvarint(reader).await?;
    }
    Ok(())
}

/// `ProfileInfo` grew an "applied aggregation" pair, written only for a client
/// that asked for at least this revision — like everything else on this wire,
/// and unlike what the server itself can do.
const MIN_REVISION_WITH_ROWS_BEFORE_AGGREGATION: u64 = 54470;

/// A server log block → the notices the Messages tab shows.
///
/// Only arrives when the session asked for logs, which this one does not yet;
/// the block is read regardless because a packet that cannot be read is a
/// connection that cannot continue.
fn log_notices(log: &Block) -> Vec<Notice> {
    let column = |name: &str| {
        log.columns
            .iter()
            .find(|column| column.name == name)
            .map(|column| &column.values)
    };
    let (Some(text), Some(priority)) = (column("text"), column("priority")) else {
        return Vec::new();
    };
    (0..log.rows)
        .filter_map(|row| {
            let message = match text.get(row) {
                Some(Cellv::Text(message)) => message.clone(),
                _ => return None,
            };
            Some(Notice {
                severity: priority
                    .get(row)
                    .and_then(|value| match value.as_cell() {
                        Some(Cell::Str(level)) => Some(Arc::from(level)),
                        _ => None,
                    })
                    .unwrap_or_else(|| Arc::from("LOG")),
                message: message.into(),
                detail: None,
                hint: None,
            })
        })
        .collect()
}

/// Whether the server will stop and wait for rows after this statement.
///
/// An `insert … values` is answered with a header block and a pause, because
/// over the native protocol the rows normally arrive as blocks rather than as
/// text. An `insert … select` is not: the server has everything it needs and
/// runs it like any other statement. Getting this wrong in the second
/// direction leaves a stray block on the wire that the *next* query reads as
/// its own, so it is keyed on the statement's shape rather than on guessing
/// from what arrives.
fn expects_client_data(statement: &str) -> bool {
    if !leading_keyword(statement).eq_ignore_ascii_case("insert") {
        return false;
    }
    let lowered = statement.to_ascii_lowercase();
    // `insert into t select …` and `insert into t values (…)` differ only in
    // this word, and a `select` that appears later — inside a subquery in the
    // values — does not change which one it is.
    match (lowered.find(" values"), lowered.find(" select")) {
        (Some(values), Some(select)) => values < select,
        (Some(_), None) => true,
        _ => false,
    }
}

/// The first word of a statement, past any leading comment.
///
/// Comments matter: a console pane routinely holds `-- why this is slow` above
/// the `select`, and reading the comment as the statement would refuse it on a
/// read-only connection.
pub fn leading_keyword(statement: &str) -> &str {
    let mut rest = statement.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("--") {
            rest = after
                .split_once('\n')
                .map_or("", |(_, tail)| tail)
                .trim_start();
            continue;
        }
        if let Some(after) = rest.strip_prefix("/*") {
            rest = after
                .split_once("*/")
                .map_or("", |(_, tail)| tail)
                .trim_start();
            continue;
        }
        break;
    }
    let end = rest
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    &rest[..end]
}

/// Whether a statement only reads.
///
/// Enforced here rather than by the server, for the same reason `db_redis`
/// enforces its own: a read-only *connection* is a promise tupli makes, and
/// the alternative is asking every server to have a mode that means the same
/// thing. The list is what ClickHouse can start a statement with; anything not
/// on it — including anything unrecognised — is refused, so a keyword this
/// misses fails safe.
fn is_read_only_statement(statement: &str) -> bool {
    matches!(
        leading_keyword(statement).to_ascii_lowercase().as_str(),
        "select" | "with" | "show" | "describe" | "desc" | "explain" | "exists" | "check"
    )
}

/// Split a socket into halves that can be locked separately.
///
/// Buffered on the read side because the protocol reads a byte at a time — a
/// varint is read byte by byte by definition — and an unbuffered `read_u8` per
/// length prefix would be one syscall per field.
fn split<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(stream: S) -> (Read, WriteHalf) {
    let (reader, writer) = tokio::io::split(stream);
    (Box::new(BufReader::new(reader)), Box::new(writer))
}

async fn tls(
    config: &ClickHouseConfig,
    stream: tokio::net::TcpStream,
) -> DbResult<tokio_native_tls::TlsStream<tokio::net::TcpStream>> {
    let mut builder = native_tls::TlsConnector::builder();
    // Same meaning as in `db_pg`: `require` encrypts without checking who
    // answered, which is what makes an internal server with a self-signed
    // certificate usable at all.
    if !config.verify_tls {
        builder.danger_accept_invalid_certs(true);
        builder.danger_accept_invalid_hostnames(true);
    }
    if let Some(path) = &config.root_cert {
        let pem = std::fs::read(path).map_err(|error| {
            DbError::connection(format!("cannot read root certificate: {error}"))
        })?;
        let certificate = native_tls::Certificate::from_pem(&pem)
            .map_err(|error| DbError::connection(format!("invalid root certificate: {error}")))?;
        builder.add_root_certificate(certificate);
    }
    let connector = builder
        .build()
        .map_err(|error| DbError::connection(format!("TLS setup failed: {error}")))?;
    tokio_native_tls::TlsConnector::from(connector)
        .connect(&config.host, stream)
        .await
        .map_err(|error| {
            DbError::connection(format!(
                "TLS to {}:{} failed: {error}",
                config.host, config.port
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_leading_keyword_is_found_past_whatever_is_written_above_it() {
        assert_eq!(leading_keyword("  select 1"), "select");
        assert_eq!(leading_keyword("-- a note\nselect 1"), "select");
        assert_eq!(leading_keyword("/* a note */ INSERT into t"), "INSERT");
        assert_eq!(
            leading_keyword("--one\n  /* two */\n\tdrop table t"),
            "drop"
        );
        // An unterminated comment leaves nothing, which is refused rather than
        // read as the next word.
        assert_eq!(leading_keyword("/* forever"), "");
        assert_eq!(leading_keyword(""), "");
    }

    #[test]
    fn a_read_only_connection_refuses_anything_it_does_not_recognise() {
        for statement in [
            "select 1",
            "WITH x as (select 1) select * from x",
            "show tables",
            "explain select 1",
        ] {
            assert!(is_read_only_statement(statement), "{statement}");
        }
        for statement in [
            "insert into t values (1)",
            "drop table t",
            "alter table t delete where 1",
            "optimize table t",
            "system flush logs",
            "",
        ] {
            assert!(!is_read_only_statement(statement), "{statement}");
        }
    }

    #[test]
    fn only_an_insert_that_carries_its_own_rows_makes_the_server_wait() {
        assert!(expects_client_data("insert into t values (1), (2)"));
        assert!(expects_client_data("INSERT INTO t (a) VALUES (1)"));
        // The server runs this one itself and sends no header, so the empty
        // block would be left on the wire for the next query to trip over.
        assert!(!expects_client_data("insert into t select * from u"));
        assert!(!expects_client_data("select 1"));
        // A `select` inside the values is still the values form.
        assert!(expects_client_data(
            "insert into t values ((select max(a) from u))"
        ));
    }

    #[test]
    fn an_error_code_decides_whether_the_editor_gets_a_caret() {
        assert_eq!(class_for(62), ErrorClass::Syntax);
        assert_eq!(class_for(60), ErrorClass::Syntax);
        assert_eq!(class_for(516), ErrorClass::Connection);
        assert_eq!(class_for(394), ErrorClass::Canceled);
        // TOO_MANY_ROWS, a real refusal by a server that is working fine.
        assert_eq!(class_for(396), ErrorClass::Server);
    }

    #[tokio::test]
    async fn an_exception_keeps_the_cause_it_was_wrapped_around() {
        let mut wire_bytes = Vec::new();
        let mut exception = |code: i32, name: &str, message: &str, nested: bool| {
            wire_bytes.extend_from_slice(&code.to_le_bytes());
            wire::write_string(&mut wire_bytes, name);
            wire::write_string(&mut wire_bytes, message);
            wire::write_string(&mut wire_bytes, "a stack trace");
            wire::write_u8(&mut wire_bytes, u8::from(nested));
        };
        exception(
            159,
            "DB::Exception",
            "DB::Exception: Timeout exceeded",
            true,
        );
        exception(62, "DB::Exception", "DB::Exception: Syntax error", false);

        let mut slice = wire_bytes.as_slice();
        let error = read_exception(&mut slice).await.unwrap();
        assert!(slice.is_empty(), "the exception left bytes behind");
        // The server repeats the name in front of its own message; showing it
        // twice would read as a stutter.
        assert_eq!(&*error.message, "Timeout exceeded");
        assert_eq!(error.code.as_deref(), Some("159 DB::Exception"));
        assert!(error.detail.as_deref().unwrap().contains("Syntax error"));
    }
}
