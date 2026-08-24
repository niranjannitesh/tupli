//! SQLite, through `libsqlite3` rather than over a wire.
//!
//! The odd one out. Every other driver here speaks a protocol to a server that
//! is somebody else's process; this one calls a library in ours, and the two
//! consequences of that run through the whole crate.
//!
//! The first is that there is nothing to wait for. A socket read is a future
//! that yields; a page read is a function that returns, eventually. So every
//! call here is a blocking call moved onto a blocking thread
//! ([`client::SqliteConnection::with_conn`]) rather than a future that suspends
//! — the asynchrony is the executor's, not SQLite's.
//!
//! The second is that a "connection" is a file, and a file can be absent, or be
//! a JPEG. The driver deliberately opens without `SQLITE_OPEN_CREATE`: a typo
//! in a path should be an error, not a brand new empty database that looks like
//! the old one lost all its tables.
//!
//! What this does not do yet: a table with no primary key stays read-only. Its
//! rows have a `rowid` that would identify them perfectly well, but only if the
//! `select` asked for it, and the app writes that `select` a layer above here.
mod client;
mod driver;
mod error;
mod introspect;
mod rows;
mod types;

pub use client::SqliteConnection;
