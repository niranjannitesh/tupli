//! A Redis backend for tupli.
//!
//! Redis is the second engine, and it is here as much to test the driver
//! boundary as to be useful: a keyspace has no tables, no rows, no columns and
//! no SQL, so every place the app quietly assumed "a query returns a result
//! set" has to be named. What survives that is the grid — every reader in
//! [`keys`] hands back a [`db::ResultSet`], so the same virtualised table draws
//! a hash, a stream, and a `select`.
//!
//! The one thing that does not generalise is completeness. Postgres can be
//! introspected exhaustively and cheaply; Redis cannot, because `KEYS *` on a
//! production server is an outage. The keyspace is therefore *sampled* — see
//! [`scan`] — and everything that reports on it says how much it looked at
//! rather than pretending to a total it has no honest way to know.

pub mod client;
pub mod command;
pub mod config;
pub mod decode;
pub mod error;
pub mod info;
pub mod keys;
pub mod php;
pub mod resp;
pub mod rows;
pub mod scan;
pub mod write;

pub use client::{argv, RedisConnection};
pub use command::Kind;
pub use config::RedisConfig;
pub use keys::{KeyFacts, KeyPage, KeyType, Position};
pub use decode::{decode, sniff, Decoded, Decoder, Form};
pub use resp::{split_args, RespValue};
pub use info::{Database, Section};
pub use scan::{KeyInfo, Scan};
