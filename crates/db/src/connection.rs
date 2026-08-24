//! What it takes to reach a server.
//!
//! A [`ConnectionConfig`] is everything except the password. That split is
//! deliberate and load-bearing: the config is what gets written to the local
//! SQLite store, logged, and shown in the connection list, and it must be safe
//! to do all three. Secrets live in the Keychain, keyed by [`ConnectionConfig::id`],
//! and are fetched only at the moment a connection is opened.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::driver::Engine;

/// How hard to insist on TLS.
///
/// The names and semantics are libpq's, because these end up in a connection
/// string and any deviation would be a trap for anyone who knows Postgres.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SslMode {
    Disable,
    Allow,
    /// Try TLS, fall back to plaintext. libpq's default, and the reason so many
    /// connections people believe are encrypted are not.
    Prefer,
    /// Encrypt, but do not check who you are talking to.
    #[default]
    Require,
    /// Encrypt and verify the certificate chain.
    VerifyCa,
    /// Encrypt, verify the chain, and verify the hostname.
    VerifyFull,
}

impl SslMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Allow => "allow",
            Self::Prefer => "prefer",
            Self::Require => "require",
            Self::VerifyCa => "verify-ca",
            Self::VerifyFull => "verify-full",
        }
    }

    /// The inverse of [`SslMode::as_str`].
    pub fn from_str(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_str() == text)
    }

    pub fn verifies_certificate(self) -> bool {
        matches!(self, Self::VerifyCa | Self::VerifyFull)
    }

    pub fn encrypts(self) -> bool {
        !matches!(self, Self::Disable)
    }

    pub const ALL: [SslMode; 6] = [
        Self::Disable,
        Self::Allow,
        Self::Prefer,
        Self::Require,
        Self::VerifyCa,
        Self::VerifyFull,
    ];
}

impl fmt::Display for SslMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The tint a connection carries through the whole window.
///
/// This is the single most effective guardrail in a database client: production
/// is red everywhere it appears, and no amount of care substitutes for the
/// window simply looking different.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionColor {
    #[default]
    None,
    Grey,
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Pink,
}

impl ConnectionColor {
    /// The name the settings file and the connection spec use. Also the serde
    /// spelling, which is what keeps a saved connection and a typed one
    /// meaning the same thing.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Grey => "grey",
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Pink => "pink",
        }
    }

    pub fn from_str(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|color| color.as_str() == text)
    }

    /// Every colour, in the order the picker offers them.
    pub const ALL: [Self; 9] = [
        Self::None,
        Self::Grey,
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Blue,
        Self::Purple,
        Self::Pink,
    ];
}

/// How careful to be with writes on this connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SafetyLevel {
    /// Edits commit the way any other client would.
    #[default]
    Normal,
    /// Every write is previewed and confirmed, and the confirmation names the
    /// connection. For anything a person would be sorry to have typed into.
    Confirm,
    /// No writes at all: the grid never enters edit mode and DDL is refused
    /// before it is sent.
    ReadOnly,
}

/// Everything needed to open a connection except the secret.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub id: Uuid,
    /// Which server is at the other end, and so which driver opens it. Saved
    /// rather than guessed: a port is a hint, not a fact.
    #[serde(default)]
    pub engine: Engine,
    pub name: String,
    /// Optional folder in the connection list.
    pub group: Option<String>,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub ssl_mode: SslMode,
    /// Path to a client certificate, key, and root CA, when the server wants
    /// them. Paths rather than contents: these are files the OS already knows
    /// how to protect.
    pub ssl_cert: Option<String>,
    pub ssl_key: Option<String>,
    pub ssl_root_cert: Option<String>,
    pub color: ConnectionColor,
    pub safety: SafetyLevel,
    /// Send a periodic no-op so an idle connection survives a NAT or a
    /// pgbouncer timeout. On by default: reconnecting mid-session loses
    /// temporary tables and open transactions.
    #[serde(default)]
    pub keep_alive: bool,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            engine: Engine::default(),
            name: String::new(),
            group: None,
            host: "localhost".into(),
            port: Engine::default().default_port(),
            database: String::new(),
            user: whoami(),
            ssl_mode: Engine::default().default_ssl_mode(),
            ssl_cert: None,
            ssl_key: None,
            ssl_root_cert: None,
            color: ConnectionColor::default(),
            safety: SafetyLevel::default(),
            keep_alive: true,
        }
    }
}

impl ConnectionConfig {
    /// The name to show when the user has not given one — `user@host/db`, the
    /// same shorthand psql prints.
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.clone();
        }
        // A file has no user and no host to be named after. Its own name is
        // what somebody with ten of them recognises; the directory it is in
        // goes on the endpoint line underneath.
        if self.engine.is_file() {
            return file_label(&self.database);
        }
        let db = if self.database.is_empty() {
            &self.user
        } else {
            &self.database
        };
        format!("{}@{}/{}", self.user, self.host, db)
    }

    /// The connection in as few characters as tell it apart.
    ///
    /// Its name if it has one; otherwise the server it points at — the host,
    /// or for a file the directory the file is in, which is what distinguishes
    /// two copies of `app.db`. [`Self::display_name`] answers the same
    /// question at full length, and full length is not what a tab has.
    pub fn short_name(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.clone();
        }
        if self.engine.is_file() {
            let path = std::path::Path::new(self.database.trim());
            return path
                .parent()
                .and_then(|parent| parent.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.display_name());
        }
        self.host.clone()
    }

    /// What to call the database in a space too small for a path.
    ///
    /// On a server that is the name and there is nothing to shorten. A file
    /// engine's "database" is where the file is, and the last part of it is
    /// the part somebody recognises — the directory above is what
    /// [`Self::endpoint`] is for.
    pub fn database_label(&self) -> String {
        if self.engine.is_file() {
            return file_label(&self.database);
        }
        self.database.clone()
    }

    /// `host:port/database`, for the status bar and the tab subtitle. The port
    /// is left out when it is the one the engine would have used anyway.
    pub fn endpoint(&self) -> String {
        if self.engine.is_file() {
            return home_relative(&self.database);
        }
        if self.port == self.engine.default_port() {
            format!("{}/{}", self.host, self.database)
        } else {
            format!("{}:{}/{}", self.host, self.port, self.database)
        }
    }

    /// The libpq keyword/value connection string, **without** the password.
    ///
    /// Built in this crate rather than in `db_pg` so it can be shown in the UI
    /// and asserted against in tests with no driver present. The password is
    /// added by the driver at the last moment and never passes through here,
    /// which is what makes it safe to log this string.
    pub fn connection_string(&self) -> String {
        // Nothing else on the list applies to a path, and writing `host=` and
        // `sslmode=` beside one would suggest they did.
        if self.engine.is_file() {
            return format!(
                "{} {}",
                kv("engine", self.engine.as_str()),
                kv("dbname", &self.database)
            );
        }
        let mut parts = vec![
            kv("host", &self.host),
            kv("port", &self.port.to_string()),
            kv("user", &self.user),
        ];
        if !self.database.is_empty() {
            parts.push(kv("dbname", &self.database));
        }
        parts.push(kv("sslmode", self.ssl_mode.as_str()));
        for (key, value) in [
            ("sslcert", &self.ssl_cert),
            ("sslkey", &self.ssl_key),
            ("sslrootcert", &self.ssl_root_cert),
        ] {
            if let Some(value) = value {
                if !value.is_empty() {
                    parts.push(kv(key, value));
                }
            }
        }
        // Only when it is not the default: a Postgres string is the one
        // everybody pastes elsewhere, and an `engine=postgres` in it would be
        // a keyword `psql` does not know.
        if self.engine != Engine::default() {
            parts.insert(0, kv("engine", self.engine.as_str()));
        }
        parts.push(kv("application_name", "tupli"));
        parts.join(" ")
    }

    /// Reasons this config cannot be used, in the order the sheet shows its
    /// fields. Empty means it is ready to connect.
    pub fn problems(&self) -> Vec<&'static str> {
        let mut problems = Vec::new();
        // A file connection has no server, so none of the questions about one
        // are asked. What it has instead is a path, and the mistake it is
        // actually prone to is a path that is not there — which is worth
        // saying in the form rather than at the end of a failed connect,
        // because the driver deliberately will not create the file.
        if self.engine.is_file() {
            let path = self.database.trim();
            if path.is_empty() {
                problems.push("Choose a database file");
            } else if path != MEMORY && !std::path::Path::new(path).exists() {
                problems.push("There is no file at that path");
            }
            return problems;
        }
        if self.host.trim().is_empty() {
            problems.push("Host is required");
        }
        if self.port == 0 {
            problems.push("Port must be between 1 and 65535");
        }
        // Redis reaches most servers as the implicit `default` user, so an
        // empty name there is the normal case rather than a mistake.
        if self.engine == Engine::Postgres && self.user.trim().is_empty() {
            problems.push("User is required");
        }
        if self.engine == Engine::Redis && !self.database.trim().is_empty() {
            if self.database.trim().parse::<u8>().is_err() {
                problems.push("Redis databases are numbered, not named");
            }
        }
        if self.ssl_mode.verifies_certificate() && self.ssl_root_cert.is_none() {
            problems.push("Certificate verification needs a root certificate");
        }
        problems
    }

    pub fn is_read_only(&self) -> bool {
        self.safety == SafetyLevel::ReadOnly
    }

    /// What a connection to this server will be able to do. Answerable before
    /// anything is open, which is when the window decides what to draw.
    pub fn capabilities(&self) -> crate::driver::Capabilities {
        self.engine.capabilities()
    }

    /// Change engines, carrying the port and the TLS setting along when
    /// neither was ever chosen.
    ///
    /// Somebody who has typed a port meant it; somebody who left 5432 alone
    /// and then picked Redis did not mean 5432. The same argument covers
    /// `ssl_mode`, which is a default per engine rather than one setting with
    /// a safe value — see [`Engine::default_ssl_mode`]. A mode that was picked
    /// on purpose is never moved, in either direction.
    pub fn set_engine(&mut self, engine: Engine) {
        if self.port == self.engine.default_port() {
            self.port = engine.default_port();
        }
        if self.ssl_mode == self.engine.default_ssl_mode() {
            self.ssl_mode = engine.default_ssl_mode();
        }
        self.engine = engine;
    }

    /// Parse a libpq keyword/value string — the inverse of
    /// [`ConnectionConfig::connection_string`].
    ///
    /// This is how a connection is named on a command line: by the integration
    /// tests, and by the `TUPLI_CONNECT` switch that boots the app straight
    /// into a server. It is deliberately *not* a URL parser: URLs make the
    /// password a substring of a string that gets logged, and the keyword form
    /// is what `psql` and every Postgres tool already accept.
    ///
    /// Unknown keywords are an error rather than a shrug, because the whole
    /// point of writing one of these by hand is finding out you typed
    /// `database=` when you meant `dbname=`.
    pub fn from_spec(spec: &str) -> Result<Self, String> {
        let mut config = Self::default();
        // A port or an sslmode that was typed is a decision; one that was not
        // follows whatever engine turns up in the spec, whichever order the
        // two came in.
        let mut port_given = false;
        let mut ssl_given = false;
        for (key, value) in split_spec(spec)? {
            match key.as_str() {
                "host" => config.host = value,
                "port" => {
                    port_given = true;
                    config.port = value
                        .parse()
                        .map_err(|_| format!("port must be a number, not {value:?}"))?
                }
                "engine" => {
                    config.engine = Engine::from_str(&value)
                        .ok_or_else(|| format!("unknown engine {value:?}"))?
                }
                // `db` is not libpq's spelling; it is accepted because it is
                // what everybody types, and rejecting it would teach nothing.
                // `file` and `path` are the words for the SQLite case, where
                // "database" is a thing on disk rather than a name on a
                // server.
                "dbname" | "db" | "database" | "file" | "path" => config.database = value,
                "user" => config.user = value,
                "sslmode" => {
                    ssl_given = true;
                    config.ssl_mode = SslMode::from_str(&value)
                        .ok_or_else(|| format!("unknown sslmode {value:?}"))?
                }
                "sslcert" => config.ssl_cert = Some(value),
                "sslkey" => config.ssl_key = Some(value),
                "sslrootcert" => config.ssl_root_cert = Some(value),
                "name" => config.name = value,
                "color" => {
                    config.color = ConnectionColor::from_str(&value)
                        .ok_or_else(|| format!("unknown colour {value:?}"))?
                }
                // Written by `connection_string` and meaningless coming back.
                "application_name" => {}
                other => return Err(format!("unknown keyword {other:?}")),
            }
        }
        if !port_given {
            config.port = config.engine.default_port();
        }
        if !ssl_given {
            config.ssl_mode = config.engine.default_ssl_mode();
        }
        Ok(config)
    }
}

/// Split a keyword/value string into pairs, honouring the single quoting
/// [`kv`] writes. Anything that is not `key=value` is an error rather than a
/// silently skipped token.
fn split_spec(spec: &str) -> Result<Vec<(String, String)>, String> {
    let mut pairs = Vec::new();
    let mut chars = spec.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            return Ok(pairs);
        }
        let mut key = String::new();
        let mut assigned = false;
        while let Some(&ch) = chars.peek() {
            if ch == '=' {
                chars.next();
                assigned = true;
                break;
            }
            // A keyword cannot contain a space. Stopping here rather than
            // running on to the next `=` turns `host=/tmp/pg socket` into a
            // clear complaint about `socket` instead of a bewildering one
            // about a keyword nobody typed.
            if ch.is_whitespace() {
                break;
            }
            key.push(ch);
            chars.next();
        }
        if key.is_empty() || !assigned {
            return Err(format!("expected keyword=value, got {key:?}"));
        }
        let mut value = String::new();
        if chars.peek() == Some(&'\'') {
            chars.next();
            let mut closed = false;
            while let Some(ch) = chars.next() {
                match ch {
                    '\\' => value.push(chars.next().unwrap_or('\\')),
                    '\'' => {
                        closed = true;
                        break;
                    }
                    other => value.push(other),
                }
            }
            if !closed {
                return Err(format!("unterminated quote after {key}="));
            }
        } else {
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() {
                    break;
                }
                value.push(ch);
                chars.next();
            }
        }
        pairs.push((key.to_ascii_lowercase(), value));
    }
}

/// libpq quoting: a value with a space or a quote in it goes in single quotes.
fn kv(key: &str, value: &str) -> String {
    if value.contains([' ', '\'', '\\']) {
        format!(
            "{key}='{}'",
            value.replace('\\', "\\\\").replace('\'', "\\'")
        )
    } else {
        format!("{key}={value}")
    }
}

/// The path SQLite reads as "do not touch the disk at all".
pub const MEMORY: &str = ":memory:";

/// A path as the last thing on it — `tupli.db` out of `/srv/data/tupli.db`.
fn file_label(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() {
        return "No file".to_string();
    }
    if path == MEMORY {
        return "In memory".to_string();
    }
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// `~/db/tupli.db` rather than `/Users/somebody/db/tupli.db`.
///
/// A status bar has one line for this and a home directory is a third of it
/// spent saying something the reader already knows.
fn home_relative(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() || path == home {
        return path.to_string();
    }
    match path.strip_prefix(&format!("{home}/")) {
        Some(rest) => format!("~/{rest}"),
        None => path.to_string(),
    }
}

/// The OS user, which is the default Postgres user for a local socket and so
/// the only default worth pre-filling.
fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_connection_string_never_carries_a_secret() {
        let config = ConnectionConfig {
            host: "db.internal".into(),
            port: 6432,
            database: "app".into(),
            user: "reader".into(),
            ..Default::default()
        };
        let s = config.connection_string();
        assert!(s.contains("host=db.internal"));
        assert!(s.contains("port=6432"));
        assert!(s.contains("sslmode=require"));
        assert!(s.contains("application_name=tupli"));
        assert!(!s.contains("password"));
    }

    #[test]
    fn values_with_spaces_are_quoted() {
        let config = ConnectionConfig {
            database: "my db".into(),
            ..Default::default()
        };
        assert!(config.connection_string().contains("dbname='my db'"));
    }

    #[test]
    fn verification_without_a_root_certificate_is_a_problem() {
        let config = ConnectionConfig {
            database: "app".into(),
            user: "me".into(),
            ssl_mode: SslMode::VerifyFull,
            ..Default::default()
        };
        assert!(config
            .problems()
            .iter()
            .any(|p| p.contains("root certificate")));
    }

    #[test]
    fn an_unnamed_connection_describes_itself() {
        let config = ConnectionConfig {
            user: "reader".into(),
            host: "db.internal".into(),
            database: "app".into(),
            ..Default::default()
        };
        assert_eq!(config.display_name(), "reader@db.internal/app");
    }

    #[test]
    fn a_connection_short_of_room_is_its_name_or_its_server() {
        let mut config = ConnectionConfig {
            user: "reader".into(),
            host: "db.internal".into(),
            database: "app".into(),
            ..Default::default()
        };
        assert_eq!(config.short_name(), "db.internal");
        config.name = "staging".into();
        assert_eq!(config.short_name(), "staging");
    }

    #[test]
    fn an_unnamed_file_is_short_for_the_directory_it_is_in() {
        // Which is the whole of the difference between two backups of the same
        // database, and the only part of the path worth a tab's room.
        let config = ConnectionConfig {
            engine: Engine::Sqlite,
            database: "/srv/nightly/app.db".into(),
            ..Default::default()
        };
        assert_eq!(config.short_name(), "nightly");
        assert_eq!(config.database_label(), "app.db");
    }

    #[test]
    fn a_spec_round_trips_through_the_connection_string() {
        let config = ConnectionConfig {
            host: "db.internal".into(),
            port: 6432,
            database: "app".into(),
            user: "reader".into(),
            ssl_mode: SslMode::VerifyFull,
            ssl_root_cert: Some("/etc/ssl/root.crt".into()),
            ..Default::default()
        };
        let back = ConnectionConfig::from_spec(&config.connection_string()).unwrap();
        assert_eq!(back.host, config.host);
        assert_eq!(back.port, config.port);
        assert_eq!(back.database, config.database);
        assert_eq!(back.user, config.user);
        assert_eq!(back.ssl_mode, config.ssl_mode);
        assert_eq!(back.ssl_root_cert, config.ssl_root_cert);
    }

    #[test]
    fn a_quoted_value_keeps_its_spaces() {
        let config =
            ConnectionConfig::from_spec("host='/tmp/pg socket' user=me name='my laptop'").unwrap();
        assert_eq!(config.host, "/tmp/pg socket");
        assert_eq!(config.name, "my laptop");
    }

    #[test]
    fn a_typo_is_reported_rather_than_ignored() {
        // The whole reason to hand-write one of these is to be told when it is
        // wrong; a skipped keyword would connect to the wrong database instead.
        let error = ConnectionConfig::from_spec("host=localhost dbnmae=app").unwrap_err();
        assert!(error.contains("dbnmae"), "{error}");
        assert!(ConnectionConfig::from_spec("port=not-a-number").is_err());
        assert!(ConnectionConfig::from_spec("sslmode=maybe").is_err());
        assert!(ConnectionConfig::from_spec("name='unclosed").is_err());
        assert!(ConnectionConfig::from_spec("host=localhost stray").is_err());
    }

    #[test]
    fn the_shorthands_people_actually_type_are_accepted() {
        let config =
            ConnectionConfig::from_spec("host=127.0.0.1 port=55432 db=tupli_dev user=postgres")
                .unwrap();
        assert_eq!(config.database, "tupli_dev");
        assert_eq!(config.port, 55432);
    }

    #[test]
    fn an_engine_brings_its_own_port_unless_one_was_typed() {
        let redis = ConnectionConfig::from_spec("engine=redis host=cache").unwrap();
        assert_eq!(redis.engine, Engine::Redis);
        assert_eq!(redis.port, 6379);
        // Order does not matter: the port is settled after the whole spec is
        // read, not as each keyword arrives.
        let early = ConnectionConfig::from_spec("port=6380 engine=redis").unwrap();
        assert_eq!(early.port, 6380);
        assert_eq!(ConnectionConfig::from_spec("host=db").unwrap().port, 5432);
        assert!(ConnectionConfig::from_spec("engine=mysql").is_err());
    }

    #[test]
    fn switching_engines_moves_a_port_nobody_chose() {
        let mut config = ConnectionConfig::default();
        config.set_engine(Engine::Redis);
        assert_eq!(config.port, 6379);
        // A typed port survives the switch, because typing it was the decision.
        let mut chosen = ConnectionConfig {
            port: 5433,
            ..Default::default()
        };
        chosen.set_engine(Engine::Redis);
        assert_eq!(chosen.port, 5433);
    }

    #[test]
    fn tls_follows_the_engine_until_somebody_picks_one() {
        // A plain Redis on 6379 answers a TLS handshake with silence, so the
        // Postgres default would cost a ten-second wait on every first attempt.
        let mut config = ConnectionConfig::default();
        assert_eq!(config.ssl_mode, SslMode::Require);
        config.set_engine(Engine::Redis);
        assert_eq!(config.ssl_mode, SslMode::Disable);

        // But a mode somebody chose stays chosen, and so does the way back.
        let mut chosen = ConnectionConfig::default();
        chosen.ssl_mode = SslMode::VerifyFull;
        chosen.set_engine(Engine::Redis);
        assert_eq!(chosen.ssl_mode, SslMode::VerifyFull);

        let spec = ConnectionConfig::from_spec("engine=redis host=127.0.0.1").unwrap();
        assert_eq!(spec.ssl_mode, SslMode::Disable);
        let typed = ConnectionConfig::from_spec("engine=redis sslmode=verify-full").unwrap();
        assert_eq!(typed.ssl_mode, SslMode::VerifyFull);
    }

    #[test]
    fn a_file_connection_names_itself_after_its_file() {
        let config = ConnectionConfig {
            engine: Engine::Sqlite,
            database: "/srv/data/orders.db".into(),
            ..Default::default()
        };
        assert_eq!(config.display_name(), "orders.db");
        assert_eq!(config.endpoint(), "/srv/data/orders.db");
        // The host, the port and the user are all still sitting there with
        // their Postgres defaults, and none of them is a reason to complain.
        assert!(
            config.problems().is_empty() || config.problems() == ["There is no file at that path"]
        );
    }

    #[test]
    fn a_file_connection_is_asked_only_for_a_file() {
        let missing = ConnectionConfig {
            engine: Engine::Sqlite,
            host: String::new(),
            user: String::new(),
            ..Default::default()
        };
        assert_eq!(missing.problems(), ["Choose a database file"]);

        let nowhere = ConnectionConfig {
            engine: Engine::Sqlite,
            database: "/no/such/place/tupli.db".into(),
            ..Default::default()
        };
        assert_eq!(nowhere.problems(), ["There is no file at that path"]);

        // An in-memory database is a real answer and not a path to check.
        let memory = ConnectionConfig {
            engine: Engine::Sqlite,
            database: MEMORY.into(),
            ..Default::default()
        };
        assert!(memory.problems().is_empty());
        assert_eq!(memory.display_name(), "In memory");
    }

    #[test]
    fn a_file_spec_round_trips_and_says_which_engine() {
        let config = ConnectionConfig {
            engine: Engine::Sqlite,
            database: "/srv/data/orders.db".into(),
            ..Default::default()
        };
        let spec = config.connection_string();
        assert_eq!(spec, "engine=sqlite dbname=/srv/data/orders.db");
        let back = ConnectionConfig::from_spec(&spec).unwrap();
        assert_eq!(back.engine, Engine::Sqlite);
        assert_eq!(back.database, config.database);
        // And the words somebody would actually type for a file.
        let typed = ConnectionConfig::from_spec("engine=sqlite file=/tmp/a.db").unwrap();
        assert_eq!(typed.database, "/tmp/a.db");
    }

    #[test]
    fn each_engine_is_asked_for_what_it_can_actually_be_asked_for() {
        let redis = ConnectionConfig {
            engine: Engine::Redis,
            database: "not-a-number".into(),
            user: String::new(),
            ..Default::default()
        };
        // No user is the normal case on Redis, but a named database is not.
        let problems = redis.problems();
        assert!(!problems.iter().any(|p| p.contains("User")), "{problems:?}");
        assert!(
            problems.iter().any(|p| p.contains("numbered")),
            "{problems:?}"
        );
        assert!(!redis.capabilities().schemas);
        assert!(ConnectionConfig::default().capabilities().schemas);
    }
}
