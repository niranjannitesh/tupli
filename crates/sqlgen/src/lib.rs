//! SQL generation: identifier quoting, DML/DDL synthesis.
//!
//! The write path of the app, minus anything that knows how to talk to a
//! server. Edits are staged in [`PendingChanges`], a row is addressed by an
//! [`Identity`] resolved from the catalog, and [`dml::statements`] turns the
//! two into a list of [`Statement`]s with their parameters bound. What runs
//! them is somebody else's problem — which is what makes all of this testable
//! without a database.
//!
//! Identifier quoting lives in `db::schema` because the introspector needs it
//! too; this crate is about statements.

pub mod change;
pub mod ddl;
pub mod dml;
pub mod identity;
pub mod statement;
pub mod table;

pub use change::{Counts, PendingChanges, RowRef};
pub use dml::{statements, Concurrency, Target};
pub use identity::{resolve, Identity, NotEditable};
pub use statement::{literal, Statement, StatementKind};
pub use table::{ColumnDraft, TableDraft};
