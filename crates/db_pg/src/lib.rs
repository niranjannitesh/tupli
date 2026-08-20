//! PostgreSQL driver.
//!
//! Everything Postgres-specific lives here and nothing else knows about it: the
//! app talks to [`db`]'s types, and this crate is what turns wire bytes into
//! them. It has no GPUI dependency and no runtime of its own — it is a library
//! of `async fn`s that the app drives from a Tokio runtime it owns.

pub mod client;
pub mod introspect;
pub mod params;
pub mod types;

pub use client::{Canceller, Outcome, PgConnection, Write, DEFAULT_MAX_ROWS};
pub use params::Param;
pub use types::{kind_for, Decoded};
