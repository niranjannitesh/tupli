//! PostgreSQL driver.
//!
//! Everything Postgres-specific lives here and nothing else knows about it: the
//! app talks to [`db`]'s types, and this crate is what turns wire bytes into
//! them. It has no GPUI dependency and no runtime of its own — it is a library
//! of `async fn`s that the app drives from a Tokio runtime it owns.

pub mod client;
pub mod driver;
pub mod introspect;
pub mod params;
pub mod roles;
pub mod types;

pub use client::{Canceller, PgConnection};
// Re-exported rather than redefined: these are the app's types, not this
// crate's, and a caller that has a `PgConnection` should not have to know
// which crate the `Outcome` it hands back came from.
pub use db::{Outcome, Write, DEFAULT_MAX_ROWS};
pub use params::Param;
pub use types::{kind_for, Decoded};
