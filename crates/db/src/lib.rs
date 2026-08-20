//! Driver-agnostic database model: values, columnar row storage, schema.
//!
//! Nothing in here knows about Postgres, and nothing in here knows about GPUI.
//! Drivers produce [`ResultSet`]s; the grid consumes them.

pub mod column;
pub mod connection;
pub mod error;
pub mod schema;
pub mod value;

pub use column::{
    BytesBuf, Cell, CellText, Column, ColumnBuilder, ColumnData, ColumnMeta, NullMask, ResultSet,
    TextBuf, TextColumnBuilder,
};
pub use connection::{ConnectionColor, ConnectionConfig, SafetyLevel, SslMode};
pub use error::{DbError, DbResult, ErrorClass, Notice};
pub use schema::{
    CheckConstraint, ColumnDef, ForeignKey, IdentityKind, IndexDef, RefAction, Relation,
    RelationKind, RelationRef, Routine, Schema, SchemaSnapshot, TriggerDef,
};
pub use value::{Value, ValueKind};
