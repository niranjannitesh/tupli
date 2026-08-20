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
            name: String::new(),
            group: None,
            host: "localhost".into(),
            port: 5432,
            database: String::new(),
            user: whoami(),
            ssl_mode: SslMode::default(),
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
        let db = if self.database.is_empty() {
            &self.user
        } else {
            &self.database
        };
        format!("{}@{}/{}", self.user, self.host, db)
    }

    /// `host:port/database`, for the status bar and the tab subtitle.
    pub fn endpoint(&self) -> String {
        if self.port == 5432 {
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
        parts.push(kv("application_name", "tupli"));
        parts.join(" ")
    }

    /// Reasons this config cannot be used, in the order the sheet shows its
    /// fields. Empty means it is ready to connect.
    pub fn problems(&self) -> Vec<&'static str> {
        let mut problems = Vec::new();
        if self.host.trim().is_empty() {
            problems.push("Host is required");
        }
        if self.port == 0 {
            problems.push("Port must be between 1 and 65535");
        }
        if self.user.trim().is_empty() {
            problems.push("User is required");
        }
        if self.ssl_mode.verifies_certificate() && self.ssl_root_cert.is_none() {
            problems.push("Certificate verification needs a root certificate");
        }
        problems
    }

    pub fn is_read_only(&self) -> bool {
        self.safety == SafetyLevel::ReadOnly
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
        for (key, value) in split_spec(spec)? {
            match key.as_str() {
                "host" => config.host = value,
                "port" => {
                    config.port = value
                        .parse()
                        .map_err(|_| format!("port must be a number, not {value:?}"))?
                }
                // `db` is not libpq's spelling; it is accepted because it is
                // what everybody types, and rejecting it would teach nothing.
                "dbname" | "db" | "database" => config.database = value,
                "user" => config.user = value,
                "sslmode" => {
                    config.ssl_mode = SslMode::from_str(&value)
                        .ok_or_else(|| format!("unknown sslmode {value:?}"))?
                }
                "sslcert" => config.ssl_cert = Some(value),
                "sslkey" => config.ssl_key = Some(value),
                "sslrootcert" => config.ssl_root_cert = Some(value),
                "name" => config.name = value,
                // Written by `connection_string` and meaningless coming back.
                "application_name" => {}
                other => return Err(format!("unknown keyword {other:?}")),
            }
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
}
