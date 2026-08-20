//! Turning staged changes into statements.
//!
//! One statement per row, addressed by its identity, in an order chosen so
//! that a commit which frees a unique value and then reuses it works: deletes,
//! then updates, then inserts.

use std::collections::BTreeMap;

use db::{Column, RelationRef, ResultSet, Value};

use crate::change::PendingChanges;
use crate::identity::Identity;
use crate::statement::{Statement, StatementKind};

/// Whether an update also checks that the row still holds what the grid was
/// showing. Off means last-write-wins; on means a row someone else changed
/// first reports zero rows affected and the whole commit rolls back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Concurrency {
    LastWriteWins,
    CheckUnchanged,
}

/// Everything the generator needs that is not the changes themselves.
pub struct Target<'a> {
    pub relation: &'a RelationRef,
    pub identity: &'a Identity,
    pub rows: &'a ResultSet,
    pub concurrency: Concurrency,
}

/// The statements that would apply `changes`, in commit order.
pub fn statements(changes: &PendingChanges, target: &Target<'_>) -> Vec<Statement> {
    let mut out = Vec::with_capacity(changes.counts().total());
    for row in changes.deleted_rows() {
        out.push(delete(row, target));
    }
    for row in changes.edited_rows() {
        if let Some(edits) = changes.row_edits(row) {
            out.push(update(row, edits, target));
        }
    }
    for values in changes.insert_rows() {
        out.push(insert(values, target));
    }
    out
}

fn insert(values: &BTreeMap<usize, Value>, target: &Target<'_>) -> Statement {
    let table = qualified(target.relation);
    // A row where nothing was typed is still a row: every column takes its
    // default, which is exactly what `DEFAULT VALUES` says.
    if values.is_empty() {
        return Statement {
            sql: format!("INSERT INTO {table} DEFAULT VALUES"),
            params: vec![],
            kind: StatementKind::Insert,
            expect_rows: Some(1),
        };
    }
    let mut params = Vec::with_capacity(values.len());
    let mut names = Vec::with_capacity(values.len());
    let mut placeholders = Vec::with_capacity(values.len());
    for (column, value) in values {
        names.push(db::schema::quote_ident(
            &target.rows.columns[*column].meta.name,
        ));
        params.push(value.clone());
        placeholders.push(format!("${}", params.len()));
    }
    Statement {
        sql: format!(
            "INSERT INTO {table} ({}) VALUES ({})",
            names.join(", "),
            placeholders.join(", ")
        ),
        params,
        kind: StatementKind::Insert,
        expect_rows: Some(1),
    }
}

fn update(row: usize, edits: &BTreeMap<usize, Value>, target: &Target<'_>) -> Statement {
    let mut params = Vec::with_capacity(edits.len() + 2);
    let mut sets = Vec::with_capacity(edits.len());
    for (column, value) in edits {
        params.push(value.clone());
        sets.push(format!(
            "{} = ${}",
            db::schema::quote_ident(&target.rows.columns[*column].meta.name),
            params.len()
        ));
    }
    let mut wheres = key_predicates(row, target, &mut params);
    if target.concurrency == Concurrency::CheckUnchanged {
        // Only the columns being written are checked. Guarding on the whole
        // row would refuse an edit because someone else touched an unrelated
        // column, which is a conflict nobody has.
        for column in edits.keys() {
            wheres.push(predicate(
                &target.rows.columns[*column],
                row,
                &mut params,
                true,
            ));
        }
    }
    Statement {
        sql: format!(
            "UPDATE {} SET {} WHERE {}",
            qualified(target.relation),
            sets.join(", "),
            wheres.join(" AND ")
        ),
        params,
        kind: StatementKind::Update,
        expect_rows: Some(1),
    }
}

fn delete(row: usize, target: &Target<'_>) -> Statement {
    let mut params = Vec::new();
    let wheres = key_predicates(row, target, &mut params);
    Statement {
        sql: format!(
            "DELETE FROM {} WHERE {}",
            qualified(target.relation),
            wheres.join(" AND ")
        ),
        params,
        kind: StatementKind::Delete,
        expect_rows: Some(1),
    }
}

/// `pk = $1`, one per identity column, binding the row's current values.
fn key_predicates(row: usize, target: &Target<'_>, params: &mut Vec<Value>) -> Vec<String> {
    target
        .identity
        .columns()
        .iter()
        .filter_map(|name| column_index(target.rows, name).map(|index| (index, name)))
        .map(|(index, _)| predicate(&target.rows.columns[index], row, params, false))
        .collect()
}

/// One comparison. A key is never null, so it compares with `=`; a guard on an
/// ordinary column has to survive one, which is what `IS NOT DISTINCT FROM` is
/// for — `col = NULL` is null, and a null predicate matches nothing.
fn predicate(column: &Column, row: usize, params: &mut Vec<Value>, guard: bool) -> String {
    let name = db::schema::quote_ident(&column.meta.name);
    let value = column.value(row);
    // `ctid` arrives as text and there is no implicit cast to `tid`.
    let cast = match &*column.meta.name {
        "ctid" => "::tid",
        _ => "",
    };
    params.push(value);
    let placeholder = format!("${}{cast}", params.len());
    match guard {
        true => format!("{name} IS NOT DISTINCT FROM {placeholder}"),
        false => format!("{name} = {placeholder}"),
    }
}

fn column_index(rows: &ResultSet, name: &str) -> Option<usize> {
    rows.columns.iter().position(|c| c.meta.name == name)
}

fn qualified(relation: &RelationRef) -> String {
    format!(
        "{}.{}",
        db::schema::quote_ident(&relation.schema),
        db::schema::quote_ident(&relation.name)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::RowRef;
    use db::{ColumnBuilder, ColumnMeta, ValueKind};

    fn rows() -> ResultSet {
        let mut id = ColumnBuilder::new(ColumnMeta::new("id", ValueKind::Int, "int8").pk());
        let mut email = ColumnBuilder::new(ColumnMeta::new("email", ValueKind::Text, "text"));
        for i in 0..3 {
            id.push(Some(db::Cell::Int(i + 1)));
            email.push(Some(db::Cell::Str(&format!("user{i}@example.com"))));
        }
        ResultSet::new(vec![id.finish(), email.finish()])
    }

    fn users() -> RelationRef {
        RelationRef::new("public", "users")
    }

    fn target<'a>(
        relation: &'a RelationRef,
        rows: &'a ResultSet,
        identity: &'a Identity,
        c: Concurrency,
    ) -> Target<'a> {
        Target {
            relation,
            identity,
            rows,
            concurrency: c,
        }
    }

    fn text(s: &str) -> Value {
        Value::text(ValueKind::Text, s)
    }

    #[test]
    fn an_update_is_addressed_by_its_key() {
        let rows = rows();
        let identity = Identity::Columns(vec!["id".into()]);
        let users = users();
        let t = target(&users, &rows, &identity, Concurrency::LastWriteWins);
        let mut changes = PendingChanges::new();
        changes.set(RowRef::Existing(1), 1, text("new@example.com"));
        let statements = statements(&changes, &t);
        assert_eq!(statements.len(), 1);
        assert_eq!(
            statements[0].sql,
            "UPDATE public.users SET email = $1 WHERE id = $2"
        );
        assert_eq!(
            statements[0].preview(),
            "UPDATE public.users SET email = 'new@example.com' WHERE id = 2"
        );
    }

    #[test]
    fn the_optimistic_guard_checks_only_what_is_being_written() {
        let rows = rows();
        let identity = Identity::Columns(vec!["id".into()]);
        let users = users();
        let t = target(&users, &rows, &identity, Concurrency::CheckUnchanged);
        let mut changes = PendingChanges::new();
        changes.set(RowRef::Existing(0), 1, text("new@example.com"));
        let statements = statements(&changes, &t);
        assert_eq!(
            statements[0].sql,
            "UPDATE public.users SET email = $1 WHERE id = $2 \
             AND email IS NOT DISTINCT FROM $3"
        );
        assert_eq!(statements[0].params[2], text("user0@example.com"));
    }

    #[test]
    fn an_untouched_new_row_takes_every_default() {
        let rows = rows();
        let identity = Identity::Columns(vec!["id".into()]);
        let users = users();
        let t = target(&users, &rows, &identity, Concurrency::LastWriteWins);
        let mut changes = PendingChanges::new();
        changes.insert();
        let statements = statements(&changes, &t);
        assert_eq!(statements[0].sql, "INSERT INTO public.users DEFAULT VALUES");
    }

    #[test]
    fn a_new_row_only_names_the_columns_that_were_typed_in() {
        let rows = rows();
        let identity = Identity::Columns(vec!["id".into()]);
        let users = users();
        let t = target(&users, &rows, &identity, Concurrency::LastWriteWins);
        let mut changes = PendingChanges::new();
        let row = changes.insert();
        changes.set(row, 1, text("fresh@example.com"));
        let statements = statements(&changes, &t);
        assert_eq!(
            statements[0].sql,
            "INSERT INTO public.users (email) VALUES ($1)"
        );
    }

    #[test]
    fn deletes_go_first_so_a_unique_value_can_be_reused() {
        let rows = rows();
        let identity = Identity::Columns(vec!["id".into()]);
        let users = users();
        let t = target(&users, &rows, &identity, Concurrency::LastWriteWins);
        let mut changes = PendingChanges::new();
        let new = changes.insert();
        changes.set(new, 1, text("user0@example.com"));
        changes.delete(RowRef::Existing(0));
        let statements = statements(&changes, &t);
        let kinds: Vec<_> = statements.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![StatementKind::Delete, StatementKind::Insert]);
        assert_eq!(
            statements[0].preview(),
            "DELETE FROM public.users WHERE id = 1"
        );
    }

    #[test]
    fn a_ctid_key_is_cast_because_nothing_else_will_cast_it() {
        let mut ctid = ColumnBuilder::new(ColumnMeta::new("ctid", ValueKind::Unknown, "tid"));
        ctid.push(Some(db::Cell::Str("(0,1)")));
        let mut note = ColumnBuilder::new(ColumnMeta::new("note", ValueKind::Text, "text"));
        note.push(Some(db::Cell::Str("hello")));
        let rows = ResultSet::new(vec![ctid.finish(), note.finish()]);
        let identity = Identity::Ctid;
        let users = users();
        let t = target(&users, &rows, &identity, Concurrency::LastWriteWins);
        let mut changes = PendingChanges::new();
        changes.delete(RowRef::Existing(0));
        let statements = statements(&changes, &t);
        assert_eq!(
            statements[0].sql,
            "DELETE FROM public.users WHERE ctid = $1::tid"
        );
    }
}
