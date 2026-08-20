//! Which driver opens a connection.
//!
//! The one place that knows both engines by name. It exists so that nothing
//! above it does: the app depends on this crate and on [`db`], never on
//! `db_pg` or `db_redis`, which makes "the UI does not know what it is talking
//! to" a fact the compiler checks rather than a rule people remember.
//!
//! Adding an engine is a variant on [`db::Engine`] and an arm here.

use std::sync::Arc;

use db::{ConnectionConfig, DbResult, Driver, Engine};

/// Open the connection the config describes.
///
/// The password is borrowed rather than owned: it comes from the Keychain or
/// from a field on screen, and the fewer copies of it exist the fewer places
/// it can be read out of. Every driver takes it the same way, and none of them
/// keeps it past the handshake.
pub async fn connect(
    config: &ConnectionConfig,
    password: Option<&str>,
) -> DbResult<Arc<dyn Driver>> {
    Ok(match config.engine {
        Engine::Postgres => {
            Arc::new(db_pg::PgConnection::connect(config, password).await?) as Arc<dyn Driver>
        }
        Engine::Redis => {
            Arc::new(db_redis::RedisConnection::connect(config, password).await?) as Arc<dyn Driver>
        }
    })
}
