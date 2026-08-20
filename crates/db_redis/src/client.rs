//! Opening a connection and sending commands down it.
//!
//! Same shape as `db_pg`'s client and for the same reason: plain `async fn`s,
//! no GPUI, no runtime of its own, so the driver can be tested against a real
//! server from a `#[tokio::test]`.
//!
//! Two things differ from Postgres, and both are Redis being Redis.
//!
//! The connection is *multiplexed* — commands from several panes share one
//! socket and the driver matches replies to senders. Redis has no per-command
//! cancel and no session state worth protecting (`SELECT` aside, which is why
//! [`RedisConnection::select`] exists and why the key browser opens one
//! connection per database rather than switching underneath itself). What
//! multiplexing cannot survive is a command that never replies, so [`command`]
//! refuses those outright.
//!
//! And there is no server-side read-only mode. A connection marked read-only
//! in tupli is enforced here, before the socket, by [`crate::command`].
//!
//! [`command`]: RedisConnection::command

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use db::{ConnectionConfig, DbError, DbResult, ErrorClass};
use redis::aio::MultiplexedConnection;
use redis::{
    AsyncConnectionConfig, Client, Cmd, ConnectionAddr, ErrorKind, IntoConnectionInfo,
    ProtocolVersion, RedisConnectionInfo, RedisError,
};

use crate::command::{self, Kind};
use crate::config::RedisConfig;
use crate::error;
use crate::resp::RespValue;

/// How long to wait for the socket and the handshake.
///
/// Short, because the thing on the other end is either a millisecond away or
/// not there: a Redis that takes ten seconds to accept a connection is a Redis
/// nobody should be browsing.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// One open connection.
pub struct RedisConnection {
    /// Cloning this is how a command gets a handle: clones share the one
    /// socket and its reply-matching, which is the whole point of a
    /// multiplexed connection. It is not another connection.
    conn: MultiplexedConnection,
    config: RedisConfig,
    server_version: Arc<str>,
    resp3: bool,
    /// Set the first time a command fails for a connection reason, so the app
    /// can offer to reconnect rather than sending into a dead socket.
    closed: AtomicBool,
    /// Whether this server has the two commands the browser would like but can
    /// live without. Facts about the server rather than about any one walk,
    /// which is why they are here: a hosted Redis with `MEMORY USAGE` disabled
    /// refuses it every time, and rediscovering that once per page would cost
    /// a failed pipeline per page for the life of the connection.
    memory_usage: AtomicBool,
    /// `SCAN … TYPE` arrived in 6.0. Where it is missing the filtering happens
    /// client-side, which costs bandwidth and not correctness.
    scan_type: AtomicBool,
}

impl RedisConnection {
    /// Connect using a saved connection and a password from the keychain.
    ///
    /// The password is borrowed for the length of the handshake and never
    /// stored on the connection: nothing here needs it again, and the shorter
    /// it lives the fewer places it can be read out of.
    pub async fn connect(config: &ConnectionConfig, password: Option<&str>) -> DbResult<Self> {
        Self::open(RedisConfig::from_config(config), password).await
    }

    /// Connect to an already-resolved endpoint.
    pub async fn open(config: RedisConfig, password: Option<&str>) -> DbResult<Self> {
        // RESP3 first. It is worth asking for: maps come back as maps instead
        // of as flattened pairs the client has to re-pair by position, and
        // doubles keep their type. A server older than 6.0 does not know
        // `HELLO`, and says so, which is the one error worth retrying.
        let (conn, resp3) = match dial(&config, password, ProtocolVersion::RESP3).await {
            Ok(conn) => (conn, true),
            Err(error) if is_protocol_refusal(&error) => (
                dial(&config, password, ProtocolVersion::RESP2)
                    .await
                    .map_err(|error| error::classify(&error, ErrorClass::Connection))?,
                false,
            ),
            Err(error) => return Err(error::classify(&error, ErrorClass::Connection)),
        };

        let connection = Self {
            conn,
            config,
            server_version: "".into(),
            resp3,
            closed: AtomicBool::new(false),
            memory_usage: AtomicBool::new(true),
            scan_type: AtomicBool::new(true),
        };
        let info = connection.text(&argv([b"INFO", b"server"])).await?;
        Ok(Self {
            server_version: version_in(&info).unwrap_or("unknown").into(),
            ..connection
        })
    }

    pub fn config(&self) -> &RedisConfig {
        &self.config
    }

    /// What the server calls itself — `7.2.4`, or `unknown` if `INFO` was
    /// answered by something that is not Redis-shaped.
    pub fn server_version(&self) -> &Arc<str> {
        &self.server_version
    }

    /// Whether the handshake settled on RESP3. Worth showing: it changes what
    /// some replies look like, and a Medis-style console user will notice.
    pub fn is_resp3(&self) -> bool {
        self.resp3
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Whether `MEMORY USAGE` is worth sending. Cleared for good the first
    /// time the server refuses it.
    pub fn has_memory_usage(&self) -> bool {
        self.memory_usage.load(Ordering::Relaxed)
    }

    pub fn without_memory_usage(&self) {
        self.memory_usage.store(false, Ordering::Relaxed);
    }

    /// Whether `SCAN … TYPE` is understood here.
    pub fn has_scan_type(&self) -> bool {
        self.scan_type.load(Ordering::Relaxed)
    }

    pub fn without_scan_type(&self) {
        self.scan_type.store(false, Ordering::Relaxed);
    }

    pub fn is_read_only(&self) -> bool {
        self.config.is_read_only()
    }

    /// Whether this connection will send that command line, and why not.
    ///
    /// Exposed so the console can grey out a refused command as it is typed
    /// rather than only after it is sent, and so the UI can ask before a
    /// [`Kind::Dangerous`] one — asking is the UI's job, since this layer has
    /// nobody to ask.
    // `DbError` is a fat struct and this is the one place it is returned from
    // a synchronous function, which is not worth boxing for: this runs once
    // per keystroke in the console, not once per row.
    #[allow(clippy::result_large_err)]
    pub fn permits(&self, args: &[Vec<u8>]) -> Result<Kind, DbError> {
        let kind = command::classify(args);
        match kind {
            Kind::Blocking => Err(error::blocked(&command::describe(args))),
            _ if self.is_read_only() && !kind.is_read() => {
                Err(error::refused(&command::describe(args)))
            }
            _ => Ok(kind),
        }
    }

    /// Send one command and get its reply.
    pub async fn command(&self, args: &[Vec<u8>]) -> DbResult<RespValue> {
        self.permits(args)?;
        let mut cmd = Cmd::new();
        for arg in args {
            cmd.arg(arg.as_slice());
        }
        let mut conn = self.conn.clone();
        let value = cmd
            .query_async::<redis::Value>(&mut conn)
            .await
            .map_err(|error| self.note(error))?;
        Ok(value.into())
    }

    /// Send several commands as one round trip.
    ///
    /// One failing command fails the batch — the protocol has no way to say
    /// "these eight worked and the ninth did not" for a pipeline, and pretending
    /// otherwise would mean guessing which reply belongs to which command. A
    /// caller that expects a command to be missing (`MEMORY USAGE` on a server
    /// that predates it) should send it once on its own and remember the answer.
    pub async fn pipeline(&self, commands: &[Vec<Vec<u8>>]) -> DbResult<Vec<RespValue>> {
        for args in commands {
            self.permits(args)?;
        }
        if commands.is_empty() {
            return Ok(Vec::new());
        }
        let mut pipe = redis::Pipeline::with_capacity(commands.len());
        for args in commands {
            let mut cmd = Cmd::new();
            for arg in args {
                cmd.arg(arg.as_slice());
            }
            pipe.add_command(cmd);
        }
        let mut conn = self.conn.clone();
        let replies = pipe
            .query_async::<Vec<redis::Value>>(&mut conn)
            .await
            .map_err(|error| self.note(error))?;
        Ok(replies.into_iter().map(Into::into).collect())
    }

    /// Send several commands as one transaction.
    ///
    /// `MULTI`/`EXEC`, so the server runs them with nothing in between. The
    /// grid needs this for the edits that are two commands pretending to be
    /// one — changing a set member is a remove and an add, and a pane that
    /// showed the set without the old member and without the new one for a
    /// moment would be showing something that never happened.
    pub async fn transaction(&self, commands: &[Vec<Vec<u8>>]) -> DbResult<Vec<RespValue>> {
        for args in commands {
            self.permits(args)?;
        }
        if commands.is_empty() {
            return Ok(Vec::new());
        }
        let mut pipe = redis::Pipeline::with_capacity(commands.len());
        pipe.atomic();
        for args in commands {
            let mut cmd = Cmd::new();
            for arg in args {
                cmd.arg(arg.as_slice());
            }
            pipe.add_command(cmd);
        }
        let mut conn = self.conn.clone();
        let replies = pipe
            .query_async::<Vec<redis::Value>>(&mut conn)
            .await
            .map_err(|error| self.note(error))?;
        Ok(replies.into_iter().map(Into::into).collect())
    }

    /// A reply that is expected to be one string. The readers use this for the
    /// questions with one answer — `TYPE`, `INFO`, `OBJECT ENCODING`.
    pub async fn text(&self, args: &[Vec<u8>]) -> DbResult<String> {
        let reply = self.command(args).await?;
        match reply.as_str() {
            Some(text) => Ok(text.to_string()),
            // Not an error worth a class of its own: it means the server
            // answered something the caller did not expect, which is the same
            // shape of problem as a command that failed.
            None => Err(unexpected(args, &reply)),
        }
    }

    /// A reply that is expected to be a number — a length, a count, a TTL.
    pub async fn number(&self, args: &[Vec<u8>]) -> DbResult<i64> {
        let reply = self.command(args).await?;
        reply.as_i64().ok_or_else(|| unexpected(args, &reply))
    }

    /// Round trip, to see whether the connection is still there.
    pub async fn ping(&self) -> DbResult<()> {
        self.command(&argv([b"PING"])).await.map(|_| ())
    }

    /// Point this connection at another database.
    ///
    /// Every pane sharing the connection moves with it, which is why the key
    /// browser opens a connection per database instead of calling this between
    /// reads. It is here for the console, where the user typing `SELECT 2`
    /// means it.
    pub async fn select(&mut self, index: u8) -> DbResult<()> {
        self.command(&argv([b"SELECT", index.to_string().as_bytes()]))
            .await?;
        self.config.db_index = index;
        Ok(())
    }

    /// Remember a connection-class failure, and convert it.
    fn note(&self, error: RedisError) -> DbError {
        let error = error::classify(&error, ErrorClass::Server);
        if error.class == ErrorClass::Connection {
            self.closed.store(true, Ordering::Relaxed);
        }
        error
    }
}

/// Build a command line out of byte strings.
///
/// Redis keys and values are binary, so commands are built from `&[u8]` rather
/// than from `&str` — a key that is a packed integer is a normal thing to find
/// in a real keyspace and has to survive being read back.
pub fn argv<const N: usize>(parts: [&[u8]; N]) -> Vec<Vec<u8>> {
    parts.iter().map(|part| part.to_vec()).collect()
}

/// Open the socket and do the handshake, with no interpretation of failure —
/// that is the caller's, which needs the [`RedisError`] to decide whether to
/// try again in RESP2.
async fn dial(
    config: &RedisConfig,
    password: Option<&str>,
    protocol: ProtocolVersion,
) -> Result<MultiplexedConnection, RedisError> {
    let addr = match config.tls {
        true => ConnectionAddr::TcpTls {
            host: config.host.clone(),
            port: config.port,
            insecure: !config.verify_tls,
            tls_params: None,
        },
        false => ConnectionAddr::Tcp(config.host.clone(), config.port),
    };

    // Built as a struct rather than as a URL on purpose: a URL means the
    // password is a substring of a string that anything might log, and this
    // way it goes straight into the handshake.
    let mut settings = RedisConnectionInfo::default()
        .set_db(config.db_index as i64)
        .set_protocol(protocol);
    if let Some(username) = &config.username {
        settings = settings.set_username(username);
    }
    if let Some(password) = password {
        settings = settings.set_password(password);
    }

    let info = addr.into_connection_info()?.set_redis_settings(settings);
    let client = Client::open(info)?;
    let options = AsyncConnectionConfig::new().set_connection_timeout(Some(CONNECT_TIMEOUT));
    client.get_multiplexed_async_connection_with_config(&options).await
}

/// Whether a failed handshake failed because the server is too old for RESP3.
///
/// Deliberately narrow. Retrying an authentication failure or a refused
/// connection in RESP2 would only produce the same error twice and blame the
/// wrong thing in the message the user finally sees.
fn is_protocol_refusal(error: &RedisError) -> bool {
    if error.kind() == ErrorKind::RESP3NotSupported {
        return true;
    }
    let message = error.to_string();
    message.contains("NOPROTO") || message.contains("unknown command")
}

/// The server's version out of `INFO server`.
fn version_in(info: &str) -> Option<&str> {
    info.lines()
        .filter_map(|line| line.split_once(':'))
        // Valkey and the other forks answer `INFO` too, and report their own
        // version under their own name as well as Redis's.
        .find(|(key, _)| matches!(*key, "redis_version" | "valkey_version" | "server_version"))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
}

/// A reply that was not the shape the caller asked for. Names the command,
/// never its arguments.
fn unexpected(args: &[Vec<u8>], reply: &RespValue) -> DbError {
    let mut error = DbError::new(
        ErrorClass::Internal,
        format!("{} answered something unexpected.", command::describe(args)),
    );
    error.detail = Some(reply.to_text().into());
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_comes_out_of_the_info_block() {
        let info = "# Server\r\nredis_version:7.2.4\r\nredis_mode:standalone\r\n";
        assert_eq!(version_in(info), Some("7.2.4"));
        // A fork that answers under its own name.
        assert_eq!(version_in("valkey_version:8.0.1\n"), Some("8.0.1"));
        // Something that is not Redis, or an empty field.
        assert_eq!(version_in("# Server\nredis_version:\n"), None);
        assert_eq!(version_in("hello"), None);
    }

    #[test]
    fn a_command_line_is_bytes_all_the_way_down() {
        let args = argv([b"GET".as_slice(), &[0xff, 0x00]]);
        assert_eq!(args[1], vec![0xff, 0x00]);
    }

    #[test]
    fn only_the_protocol_error_is_worth_a_second_attempt() {
        // A stand-in for the server's own answer, which is what reaches here.
        let old = RedisError::from((
            ErrorKind::Server(redis::ServerErrorKind::ResponseError),
            "unknown command 'HELLO'",
        ));
        assert!(is_protocol_refusal(&old));
        let wrong_password = RedisError::from((ErrorKind::AuthenticationFailed, "WRONGPASS"));
        assert!(!is_protocol_refusal(&wrong_password));
    }
}
