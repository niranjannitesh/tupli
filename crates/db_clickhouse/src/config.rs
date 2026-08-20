//! Where a ClickHouse server is.
//!
//! Same shape as `db_redis`'s config and for the same reason: the app's shared
//! [`db::ConnectionConfig`] is what the sidebar lists and SQLite stores, and
//! widening it for one engine's quirks would make every engine carry them.
//! ClickHouse's quirks are two — a default database that is `default` rather
//! than the user's name, and a TLS port that is a different port — so they are
//! resolved here rather than in the shared record.
//!
//! No password field, deliberately: this is the type that gets cloned, logged
//! and shown, and the secret is passed alongside it for the length of a
//! handshake instead.

use db::{ConnectionConfig, SafetyLevel, SslMode};

/// The native protocol's port. 8123 is the HTTP interface, which this driver
/// exists not to speak.
pub const DEFAULT_PORT: u16 = 9000;

/// The same protocol wrapped in TLS. A separate listener, not an upgrade.
pub const DEFAULT_TLS_PORT: u16 = 9440;

/// The database every ClickHouse has and every client lands in.
pub const DEFAULT_DATABASE: &str = "default";

/// The user a stock server ships with, password-less.
pub const DEFAULT_USER: &str = "default";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClickHouseConfig {
    pub host: String,
    pub port: u16,
    /// The database a bare table name resolves in. Unlike Postgres this is not
    /// a boundary — one session can read every database on the server — so it
    /// is a starting point rather than a scope.
    pub database: String,
    pub user: String,
    pub tls: bool,
    pub verify_tls: bool,
    /// A path to a PEM the server's certificate must chain to, when the
    /// connection record names one.
    pub root_cert: Option<String>,
    pub safety: SafetyLevel,
}

impl Default for ClickHouseConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: DEFAULT_PORT,
            database: DEFAULT_DATABASE.into(),
            user: DEFAULT_USER.into(),
            tls: false,
            verify_tls: false,
            root_cert: None,
            safety: SafetyLevel::default(),
        }
    }
}

impl ClickHouseConfig {
    pub fn from_config(config: &ConnectionConfig) -> Self {
        // `Prefer` and `Allow` mean "try TLS, fall back to plaintext", which
        // Postgres can do because the upgrade is in-band. ClickHouse cannot:
        // 9440 and 9000 are two listeners, and a TLS handshake against 9000
        // does not fail — the server waits for a `Hello` that never comes and
        // the client waits for a certificate that never comes. So only the
        // modes that insist on encryption turn it on. Same reasoning as
        // `db_redis`, same failure mode if it is got wrong.
        let tls = config.ssl_mode.verifies_certificate() || config.ssl_mode == SslMode::Require;
        Self {
            host: config.host.clone(),
            port: match config.port {
                0 if tls => DEFAULT_TLS_PORT,
                0 => DEFAULT_PORT,
                port => port,
            },
            database: match config.database.trim() {
                "" => DEFAULT_DATABASE.into(),
                database => database.into(),
            },
            user: match config.user.trim() {
                "" => DEFAULT_USER.into(),
                user => user.into(),
            },
            tls,
            verify_tls: config.ssl_mode.verifies_certificate(),
            root_cert: config
                .ssl_root_cert
                .clone()
                .filter(|path| !path.trim().is_empty()),
            safety: config.safety,
        }
    }

    /// `host:port/database`, for the status bar.
    pub fn endpoint(&self) -> String {
        match self.port {
            DEFAULT_PORT => format!("{}/{}", self.host, self.database),
            port => format!("{}:{port}/{}", self.host, self.database),
        }
    }

    pub fn is_read_only(&self) -> bool {
        self.safety == SafetyLevel::ReadOnly
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_blanks_in_a_connection_record_become_clickhouses_own_defaults() {
        let config = ClickHouseConfig::from_config(&ConnectionConfig {
            host: "warehouse.internal".into(),
            port: 0,
            user: String::new(),
            database: String::new(),
            ssl_mode: SslMode::Disable,
            ..ConnectionConfig::default()
        });
        assert_eq!(config.port, DEFAULT_PORT);
        assert_eq!(config.database, DEFAULT_DATABASE);
        // Not the operating-system user, which is what the shared record falls
        // back to and what a Postgres server would accept.
        assert_eq!(config.user, DEFAULT_USER);
    }

    #[test]
    fn only_the_modes_that_insist_on_encryption_use_the_tls_port() {
        let record = |mode| ConnectionConfig {
            port: 0,
            ssl_mode: mode,
            ..ConnectionConfig::default()
        };
        // `Prefer` has no in-band upgrade to fall back from, so it is
        // plaintext rather than a ten-second hang against 9000.
        for mode in [SslMode::Disable, SslMode::Allow, SslMode::Prefer] {
            let config = ClickHouseConfig::from_config(&record(mode));
            assert!(!config.tls, "{mode:?}");
            assert_eq!(config.port, DEFAULT_PORT, "{mode:?}");
        }
        for mode in [SslMode::Require, SslMode::VerifyCa, SslMode::VerifyFull] {
            let config = ClickHouseConfig::from_config(&record(mode));
            assert!(config.tls, "{mode:?}");
            assert_eq!(config.port, DEFAULT_TLS_PORT, "{mode:?}");
        }
        // `Require` encrypts without checking who answered — the same meaning
        // it has for the Postgres driver.
        assert!(!ClickHouseConfig::from_config(&record(SslMode::Require)).verify_tls);
        assert!(ClickHouseConfig::from_config(&record(SslMode::VerifyFull)).verify_tls);
    }

    #[test]
    fn a_port_that_was_chosen_is_never_overridden_by_the_ssl_mode() {
        let config = ClickHouseConfig::from_config(&ConnectionConfig {
            port: 19000,
            ssl_mode: SslMode::VerifyFull,
            ..ConnectionConfig::default()
        });
        assert_eq!(config.port, 19000);
    }
}
