//! Where a Redis server is, and which of its sixteen databases we are looking at.
//!
//! [`db::ConnectionConfig`] is the app's connection record: it is what the
//! sidebar lists, what SQLite stores, and what the Keychain is keyed by. Redis
//! needs two things that record does not model — a numeric database index
//! instead of a database name, and a username that is optional because Redis
//! only grew ACLs in 6.0 — so rather than widen the shared struct for one
//! engine, this crate reads a [`RedisConfig`] out of it. `database` carries the
//! index as text, which is what the connection sheet already collects, and
//! `user` carries the ACL username, empty meaning the default user.
//!
//! Passwords never appear in either type. [`RedisConfig::from_url`] hands the
//! password back as a separate value precisely so that the thing which gets
//! serialised, logged, and shown cannot contain it.

use db::{ConnectionConfig, SafetyLevel, SslMode};

/// The port `redis-server` listens on when nobody has said otherwise.
pub const DEFAULT_PORT: u16 = 6379;

/// How many logical databases a stock server has. Not a protocol limit — it is
/// `databases 16` in the default config file — so it is used to phrase a
/// complaint, never to reject an index the server might well accept.
pub const CONVENTIONAL_DATABASES: u8 = 16;

/// Everything needed to open a Redis session except the secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedisConfig {
    pub host: String,
    pub port: u16,
    /// The logical database to `SELECT` on connect. Redis calls these numbers,
    /// not names, and a server with sixteen of them is sixteen keyspaces that
    /// share one set of credentials.
    pub db_index: u8,
    /// The ACL user. `None` means the implicit `default` user, which is what
    /// every server before 6.0 has and most servers after it still use.
    pub username: Option<String>,
    /// `rediss://` rather than `redis://`.
    pub tls: bool,
    /// Whether the certificate has to check out. Off for the `require`-shaped
    /// modes, for the same reason `db_pg` turns it off there: an internal
    /// server with a self-signed certificate is the normal case, and refusing
    /// it would only teach people to disable TLS altogether.
    pub verify_tls: bool,
    pub safety: SafetyLevel,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: DEFAULT_PORT,
            db_index: 0,
            username: None,
            tls: false,
            verify_tls: false,
            safety: SafetyLevel::default(),
        }
    }
}

impl RedisConfig {
    /// Read the Redis-shaped fields out of the app's connection record.
    ///
    /// A `database` that is not a number is taken as database 0 rather than
    /// refused: the field is free text in the connection sheet, and a typo
    /// there should land somebody on the default keyspace, not on an error
    /// they cannot act on.
    pub fn from_config(config: &ConnectionConfig) -> Self {
        Self {
            host: config.host.clone(),
            port: match config.port {
                0 => DEFAULT_PORT,
                port => port,
            },
            db_index: config.database.trim().parse().unwrap_or(0),
            username: Some(config.user.trim())
                .filter(|user| !user.is_empty() && *user != "default")
                .map(str::to_owned),
            // `Prefer` and `Allow` mean "try TLS, fall back to plaintext",
            // which Postgres can do because the upgrade is in-band. Redis
            // cannot: a TLS server and a plaintext one are two different
            // ports, and a TLS handshake against a plaintext port does not
            // fail, it hangs until the connect timeout. So only the modes
            // that *insist* on encryption turn it on, and the ambivalent ones
            // mean plaintext here.
            tls: config.ssl_mode.verifies_certificate() || config.ssl_mode == SslMode::Require,
            verify_tls: config.ssl_mode.verifies_certificate(),
            safety: config.safety,
        }
    }

    /// The inverse, for the connection sheet: fields the app already stores,
    /// filled in from a config parsed out of a URL.
    pub fn apply_to(&self, config: &mut ConnectionConfig) {
        config.host = self.host.clone();
        config.port = self.port;
        config.database = self.db_index.to_string();
        config.user = self.username.clone().unwrap_or_default();
        config.ssl_mode = match (self.tls, self.verify_tls) {
            (false, _) => SslMode::Disable,
            (true, false) => SslMode::Require,
            (true, true) => SslMode::VerifyFull,
        };
        config.safety = self.safety;
    }

    /// Parse a `redis://` or `rediss://` URL, returning the password separately.
    ///
    /// Splitting the password out is the whole point of the signature. A URL is
    /// the one place a Redis password routinely travels as a substring of
    /// something people paste into chat, and every part of this app downstream
    /// of here treats the config as safe to write down. The caller puts the
    /// second half of the pair in the Keychain and drops it.
    ///
    /// Accepted: `redis://host`, `redis://host:6380/3`, `rediss://user:pw@host`,
    /// `redis://:pw@host` (no username, the pre-ACL form), and a bare
    /// `host:port` with no scheme, because that is what people type.
    pub fn from_url(url: &str) -> Result<(Self, Option<String>), String> {
        let url = url.trim();
        if url.is_empty() {
            return Err("empty connection URL".into());
        }
        let mut config = Self::default();

        let rest = match url.split_once("://") {
            Some(("redis", rest)) => rest,
            Some(("rediss", rest)) => {
                config.tls = true;
                config.verify_tls = true;
                rest
            }
            Some((scheme, _)) => return Err(format!("unknown scheme {scheme:?}")),
            // No scheme at all. `redis-cli -u` refuses this and everybody types
            // it anyway, so it is read as plaintext on the default port.
            None => url,
        };

        // Split on the *last* `@`: a password may legitimately contain one, and
        // a host may not.
        let (credentials, authority) = match rest.rsplit_once('@') {
            Some((credentials, authority)) => (Some(credentials), authority),
            None => (None, rest),
        };
        let mut password = None;
        if let Some(credentials) = credentials {
            let (user, secret) = match credentials.split_once(':') {
                Some((user, secret)) => (user, Some(secret)),
                None => (credentials, None),
            };
            config.username = Some(percent_decode(user)).filter(|u| !u.is_empty());
            password = secret.map(percent_decode).filter(|p| !p.is_empty());
        }

        let (host_port, path) = match authority.split_once('/') {
            Some((host_port, path)) => (host_port, path),
            None => (authority, ""),
        };
        // A path may carry a query string the URL forms allow and this does not
        // read; dropping it is better than failing on a URL that works
        // elsewhere.
        let path = path.split(['?', '#']).next().unwrap_or("");
        if !path.is_empty() {
            config.db_index = path
                .parse()
                .map_err(|_| format!("database must be a number, not {path:?}"))?;
        }

        // Only the ports of an IPv6 literal are after a `]`; splitting on the
        // last colon everywhere else would turn `::1` into a host of `:` and a
        // port of `1`.
        let (host, port) = match host_port.strip_prefix('[') {
            Some(after) => match after.split_once(']') {
                Some((host, tail)) => (host, tail.strip_prefix(':')),
                None => return Err("unterminated IPv6 literal".into()),
            },
            None => match host_port.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (host_port, None),
            },
        };
        if host.is_empty() {
            return Err("no host in connection URL".into());
        }
        config.host = host.to_owned();
        if let Some(port) = port.filter(|p| !p.is_empty()) {
            config.port = port
                .parse()
                .map_err(|_| format!("port must be a number, not {port:?}"))?;
        }
        Ok((config, password))
    }

    /// The URL for this config **without** the password — for the connection
    /// list, a log line, or a copy button. There is deliberately no method that
    /// puts the password back in.
    pub fn url(&self) -> String {
        let scheme = match self.tls {
            true => "rediss",
            false => "redis",
        };
        let user = match &self.username {
            Some(user) => format!("{user}@"),
            None => String::new(),
        };
        let host = match self.host.contains(':') {
            true => format!("[{}]", self.host),
            false => self.host.clone(),
        };
        format!("{scheme}://{user}{host}:{}/{}", self.port, self.db_index)
    }

    /// `host:port/db`, for the status bar. Short enough to sit next to other
    /// facts, unlike the URL.
    pub fn endpoint(&self) -> String {
        match self.port {
            DEFAULT_PORT => format!("{}/{}", self.host, self.db_index),
            port => format!("{}:{port}/{}", self.host, self.db_index),
        }
    }

    pub fn is_read_only(&self) -> bool {
        self.safety == SafetyLevel::ReadOnly
    }

    /// Reasons this cannot be used, in the order the sheet shows its fields.
    pub fn problems(&self) -> Vec<&'static str> {
        let mut problems = Vec::new();
        if self.host.trim().is_empty() {
            problems.push("Host is required");
        }
        if self.port == 0 {
            problems.push("Port must be between 1 and 65535");
        }
        if self.db_index >= CONVENTIONAL_DATABASES {
            problems.push("Database is usually 0-15; this server may not have that many");
        }
        problems
    }
}

/// `%2F` → `/`. Enough of a decoder for the credential half of a URL, which is
/// the only part of one that is routinely escaped. A stray `%` that is not
/// followed by two hex digits is kept as a `%`, because it is far more likely
/// to be a character in the password than a truncated escape.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let hex = |b: u8| (b as char).to_digit(16);
        match (byte, bytes.get(index + 1), bytes.get(index + 2)) {
            (b'%', Some(&hi), Some(&lo)) => match (hex(hi), hex(lo)) {
                (Some(hi), Some(lo)) => {
                    out.push((hi * 16 + lo) as u8);
                    index += 3;
                }
                _ => {
                    out.push(byte);
                    index += 1;
                }
            },
            _ => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_host_is_a_plaintext_connection_on_the_default_port() {
        let (config, password) = RedisConfig::from_url("127.0.0.1").unwrap();
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, DEFAULT_PORT);
        assert_eq!(config.db_index, 0);
        assert!(!config.tls);
        assert_eq!(password, None);
    }

    #[test]
    fn the_scheme_decides_whether_the_socket_is_encrypted() {
        let (plain, _) = RedisConfig::from_url("redis://example.test").unwrap();
        assert!(!plain.tls);
        let (tls, _) = RedisConfig::from_url("rediss://example.test").unwrap();
        assert!(tls.tls && tls.verify_tls);
        assert!(RedisConfig::from_url("http://example.test").is_err());
    }

    #[test]
    fn the_path_is_the_database_index() {
        let (config, _) = RedisConfig::from_url("redis://example.test:6380/7").unwrap();
        assert_eq!(config.port, 6380);
        assert_eq!(config.db_index, 7);
        assert!(RedisConfig::from_url("redis://example.test/nope").is_err());
    }

    #[test]
    fn credentials_come_out_of_the_url_and_the_password_comes_out_separately() {
        let (config, password) = RedisConfig::from_url("redis://alice:s3cret@host/2").unwrap();
        assert_eq!(config.username.as_deref(), Some("alice"));
        assert_eq!(password.as_deref(), Some("s3cret"));
        // And it is not in anything the app would write down.
        assert!(!config.url().contains("s3cret"));
        assert!(!format!("{config:?}").contains("s3cret"));
    }

    #[test]
    fn the_pre_acl_form_has_a_password_and_no_user() {
        let (config, password) = RedisConfig::from_url("redis://:hunter2@host").unwrap();
        assert_eq!(config.username, None);
        assert_eq!(password.as_deref(), Some("hunter2"));
    }

    #[test]
    fn a_password_may_contain_the_characters_that_split_the_url() {
        // The split is on the last `@`, so an `@` in the secret survives.
        let (_, password) = RedisConfig::from_url("redis://u:a@b@host:6379").unwrap();
        assert_eq!(password.as_deref(), Some("a@b"));
        // And `%2F` is a slash, not the start of the database path.
        let (config, password) = RedisConfig::from_url("redis://u:a%2Fb@host/3").unwrap();
        assert_eq!(password.as_deref(), Some("a/b"));
        assert_eq!(config.db_index, 3);
    }

    #[test]
    fn an_ipv6_literal_keeps_its_colons() {
        let (config, _) = RedisConfig::from_url("redis://[::1]:6380/1").unwrap();
        assert_eq!(config.host, "::1");
        assert_eq!(config.port, 6380);
        assert_eq!(config.db_index, 1);
        // And it goes back out bracketed, so the round trip parses.
        assert_eq!(RedisConfig::from_url(&config.url()).unwrap().0, config);
    }

    #[test]
    fn a_url_without_a_password_round_trips_through_its_own_rendering() {
        for text in [
            "redis://host:6379/0",
            "rediss://alice@host:6380/15",
            "redis://[fe80::1]:6379/2",
        ] {
            let (config, _) = RedisConfig::from_url(text).unwrap();
            let (again, _) = RedisConfig::from_url(&config.url()).unwrap();
            assert_eq!(config, again, "{text}");
        }
    }

    #[test]
    fn the_apps_connection_record_carries_the_index_as_text() {
        let mut record = ConnectionConfig {
            host: "cache.internal".into(),
            port: 6380,
            database: "9".into(),
            user: "default".into(),
            ssl_mode: SslMode::Require,
            ..ConnectionConfig::default()
        };
        let config = RedisConfig::from_config(&record);
        assert_eq!(config.db_index, 9);
        // `default` is the implicit user, so it is not an ACL username.
        assert_eq!(config.username, None);
        assert!(config.tls && !config.verify_tls);

        // `Prefer` is Postgres's "upgrade if you can", which Redis has no way
        // to do. Taking it as plaintext is what keeps a default connection
        // record from hanging against an ordinary Redis.
        record.ssl_mode = SslMode::Prefer;
        assert!(!RedisConfig::from_config(&record).tls);
        record.ssl_mode = SslMode::VerifyFull;
        let strict = RedisConfig::from_config(&record);
        assert!(strict.tls && strict.verify_tls);

        config.apply_to(&mut record);
        assert_eq!(record.database, "9");
        assert_eq!(RedisConfig::from_config(&record), config);
    }

    #[test]
    fn a_database_that_is_not_a_number_is_database_zero() {
        let record = ConnectionConfig {
            database: "oracle".into(),
            ..ConnectionConfig::default()
        };
        assert_eq!(RedisConfig::from_config(&record).db_index, 0);
    }

    #[test]
    fn an_index_past_the_conventional_sixteen_is_a_warning_not_a_refusal() {
        let config = RedisConfig {
            db_index: 20,
            ..RedisConfig::default()
        };
        assert_eq!(config.problems().len(), 1);
        assert!(RedisConfig::default().problems().is_empty());
    }
}
