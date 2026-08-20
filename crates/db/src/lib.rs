//! Driver-agnostic database model: values, columnar row storage, schema.
//!
//! Nothing in here knows about Postgres, and nothing in here knows about GPUI.
//! Drivers produce [`ResultSet`]s; the grid consumes them.

pub mod column;
pub mod connection;
pub mod decode;
pub mod driver;
pub mod error;
pub mod keyspace;
pub mod php;
pub mod roles;
pub mod schema;
pub mod value;

pub use column::{
    BytesBuf, Cell, CellText, Column, ColumnBuilder, ColumnData, ColumnMeta, NullMask, ResultSet,
    TextBuf, TextColumnBuilder,
};
pub use connection::{ConnectionColor, ConnectionConfig, SafetyLevel, SslMode};
pub use decode::{decode, hex_dump, sniff, DecodeError, Decoded, Decoder, Form};
pub use driver::{
    Capabilities, Catalog, Dialect, Driver, Engine, Outcome, Write, DEFAULT_MAX_ROWS,
};
pub use error::{DbError, DbResult, ErrorClass, Notice};
pub use keyspace::{
    format_ttl, key_text, Cursor, KeyFacts, KeyInfo, KeyListing, KeyPage, KeyQuery, KeyType,
    Keyspace, KeyspaceDatabase,
};
pub use roles::{Grant, Grants, Privilege, Role, RoleSet, PUBLIC};
pub use schema::{
    CheckConstraint, ColumnDef, ForeignKey, IdentityKind, IndexDef, RefAction, Relation,
    RelationKind, RelationRef, Routine, Schema, SchemaSnapshot, TriggerDef,
};
pub use value::{Value, ValueKind};
