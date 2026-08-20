//! Which columns name a row.
//!
//! Every write this app generates is addressed to exactly one row, and the
//! address has to come from somewhere the database agrees is unique. Primary
//! key, then a unique index over non-null columns, then `ctid` — and if none
//! of those are available the grid is read-only and says which one was
//! missing, because "read-only" with no reason is indistinguishable from a bug.

use std::sync::Arc;

use db::Relation;

/// How a row gets addressed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Identity {
    /// A key made of real columns, in index order. Stable across sessions:
    /// the same row is the same row tomorrow.
    Columns(Vec<Arc<str>>),
    /// Postgres' physical row address. Unique at this instant and not one
    /// moment longer — an `UPDATE` or a `VACUUM FULL` moves a row and its
    /// ctid changes with it, so anything addressed this way carries a warning.
    Ctid,
}

impl Identity {
    /// The column names to put in a `WHERE`.
    pub fn columns(&self) -> Vec<Arc<str>> {
        match self {
            Self::Columns(names) => names.clone(),
            Self::Ctid => vec![Arc::from("ctid")],
        }
    }

    /// Whether editing through this identity is safe enough to do quietly.
    pub fn is_stable(&self) -> bool {
        matches!(self, Self::Columns(_))
    }
}

/// Why a result set cannot be edited.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NotEditable {
    /// The rows came from a query, not from a table — a join, an aggregate, a
    /// function. There is no single table to write back to.
    NotATable,
    /// A view without a rule or trigger to write through.
    View,
    /// The table has no primary key and no unique index over non-null columns.
    NoKey,
    /// The key exists but this query did not select all of it, which is the
    /// usual case for someone who typed `select name from users`.
    KeyNotSelected(Vec<Arc<str>>),
}

impl NotEditable {
    /// One sentence for the read-only banner.
    pub fn message(&self) -> String {
        match self {
            Self::NotATable => "These rows came from a query, not a table.".into(),
            Self::View => "This view cannot be written to directly.".into(),
            Self::NoKey => "This table has no primary key or unique index.".into(),
            Self::KeyNotSelected(missing) => {
                let names: Vec<_> = missing.iter().map(|n| n.to_string()).collect();
                format!("The query did not select {}.", names.join(", "))
            }
        }
    }
}

/// Work out how to address a row of `fetched` in `relation`.
///
/// `fetched` is the result set's own column names, not the table's: a key
/// column the query left out is a key this grid cannot use, however real it is
/// in the catalog.
pub fn resolve(relation: &Relation, fetched: &[&str]) -> Result<Identity, NotEditable> {
    if relation.kind.is_view() {
        return Err(NotEditable::View);
    }
    if !relation.kind.is_editable() {
        return Err(NotEditable::NotATable);
    }
    let Some(index) = relation.row_identity() else {
        // No stable key, but a physical address will still do for a table.
        return match fetched.contains(&"ctid") {
            true => Ok(Identity::Ctid),
            false => Err(NotEditable::NoKey),
        };
    };
    let missing: Vec<Arc<str>> = index
        .columns
        .iter()
        .filter(|name| !fetched.contains(&&***name))
        .cloned()
        .collect();
    match missing.is_empty() {
        true => Ok(Identity::Columns(index.columns.clone())),
        false if fetched.contains(&"ctid") => Ok(Identity::Ctid),
        false => Err(NotEditable::KeyNotSelected(missing)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::{ColumnDef, IndexDef, RelationKind, RelationRef, ValueKind};

    fn column(name: &str, nullable: bool) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            position: 1,
            type_name: "int8".into(),
            kind: ValueKind::Int,
            nullable,
            default: None,
            identity: None,
            is_generated: false,
            comment: None,
        }
    }

    fn index(name: &str, columns: &[&str], primary: bool) -> IndexDef {
        IndexDef {
            name: name.into(),
            columns: columns.iter().map(|c| Arc::from(*c)).collect(),
            is_unique: true,
            is_primary: primary,
            method: "btree".into(),
            predicate: None,
        }
    }

    fn table(kind: RelationKind, columns: Vec<ColumnDef>, indexes: Vec<IndexDef>) -> Relation {
        Relation {
            reference: RelationRef::new("public", "users"),
            kind,
            columns,
            indexes,
            foreign_keys: vec![],
            checks: Vec::new(),
            triggers: Vec::new(),
            definition: None,
            estimated_rows: 0,
            size_bytes: 0,
            comment: None,
            detail_loaded: true,
        }
    }

    #[test]
    fn a_primary_key_that_was_selected_is_the_identity() {
        let t = table(
            RelationKind::Table,
            vec![column("id", false), column("email", true)],
            vec![index("users_pkey", &["id"], true)],
        );
        assert_eq!(
            resolve(&t, &["id", "email"]),
            Ok(Identity::Columns(vec![Arc::from("id")]))
        );
    }

    #[test]
    fn a_key_the_query_left_out_names_what_is_missing() {
        let t = table(
            RelationKind::Table,
            vec![column("id", false), column("email", true)],
            vec![index("users_pkey", &["id"], true)],
        );
        assert_eq!(
            resolve(&t, &["email"]),
            Err(NotEditable::KeyNotSelected(vec![Arc::from("id")]))
        );
    }

    #[test]
    fn ctid_stands_in_for_a_table_with_no_key_at_all() {
        let t = table(RelationKind::Table, vec![column("note", true)], vec![]);
        assert_eq!(resolve(&t, &["note", "ctid"]), Ok(Identity::Ctid));
        assert!(!Identity::Ctid.is_stable());
        assert_eq!(resolve(&t, &["note"]), Err(NotEditable::NoKey));
    }

    #[test]
    fn a_view_is_read_only() {
        let t = table(RelationKind::View, vec![column("id", false)], vec![]);
        assert_eq!(resolve(&t, &["id"]), Err(NotEditable::View));
    }
}
