//! ClickHouse over its own protocol.
//!
//! Not the HTTP interface on 8123. ClickHouse's native protocol on 9000 is a
//! stateful binary stream: varint-tagged packets with no lengths and no
//! framing, where the answer to a query is a run of columnar blocks that ends
//! when the server says it does. That is harder to speak than HTTP and worth
//! it, because a block is already laid out the way [`db::ResultSet`] is — every
//! value of one column together — so fifty thousand rows go from socket to grid
//! without ever being turned into rows and back.
//!
//! Nothing here is generated and nothing here wraps an existing client. The
//! modules are the protocol in layers: [`wire`] is varints and strings,
//! [`types`] turns a type name like `Array(Nullable(LowCardinality(String)))`
//! into a reader for the bytes it describes, [`block`] reads one columnar
//! block, and [`client`] is the packet loop that ties them together.
//!
//! The one thing worth knowing before reading any of it: every optional field
//! on the wire is gated on the revision the *client* announced, not the one the
//! server speaks. A server talking to an old client writes an old client's
//! fields. So [`protocol::CLIENT_REVISION`] is not a "how new are we" number —
//! it is the exact shape of every packet in this crate, and moving it means
//! auditing all of them.

mod block;
mod client;
mod compress;
mod config;
mod driver;
mod introspect;
mod protocol;
mod types;
mod wire;

pub use client::{ClickHouseConnection, Fetched, ServerInfo, CONNECT_TIMEOUT};
pub use config::{
    ClickHouseConfig, DEFAULT_DATABASE, DEFAULT_PORT, DEFAULT_TLS_PORT, DEFAULT_USER,
};
pub use protocol::CLIENT_REVISION;
