//! Fetched values into a [`db::ResultSet`].
//!
//! Everything arrives owned. Other drivers decode a wire buffer and hand the
//! builder a borrowed slice out of it; here the rows are stepped one at a time
//! and each value must be copied off the statement before the next step
//! invalidates it. So the shape is: collect the whole column, then decide what
//! it is (see [`crate::types`]), then build. Deciding first is not an option
//! when the schema is only a hint.

use db::{Cell, Column, ColumnBuilder, ColumnMeta, ResultSet, ValueKind};
use rusqlite::types::Value as SqlValue;

use crate::types::{self, Seen};

/// One column's worth of what the statement said about it, before any rows.
pub struct Heading {
    pub name: String,
    /// The declared type of the table column this came from, when it came from
    /// one at all. An expression has none.
    pub declared: Option<String>,
}

pub fn result_set(headings: &[Heading], data: Vec<Vec<SqlValue>>) -> ResultSet {
    let columns = headings
        .iter()
        .zip(data)
        .map(|(heading, values)| column(heading, values))
        .collect();
    ResultSet::new(columns)
}

fn column(heading: &Heading, values: Vec<SqlValue>) -> Column {
    let seen = Seen::of(&values);
    let kind = types::kind(heading.declared.as_deref(), seen);
    let type_name = match &heading.declared {
        Some(declared) if !declared.is_empty() => declared.clone(),
        _ => seen.storage_name().to_string(),
    };
    let meta = ColumnMeta::new(&heading.name, kind, type_name);
    let mut builder = ColumnBuilder::with_capacity(meta, values.len());
    for value in &values {
        builder.push(cell(value, kind));
    }
    builder.finish()
}

fn cell(value: &SqlValue, kind: ValueKind) -> Option<Cell<'_>> {
    Some(match value {
        SqlValue::Null => return None,
        SqlValue::Integer(i) => match kind {
            ValueKind::Bool => Cell::Bool(*i != 0),
            _ => Cell::Int(*i),
        },
        SqlValue::Real(f) => Cell::Float(*f),
        // A column is binary as soon as one value in it is, and the text in the
        // other rows has to arrive as bytes too — the builder stores a value
        // that does not match its storage as null, which would silently empty
        // half the column.
        SqlValue::Text(s) => match kind {
            ValueKind::Bytes => Cell::Bytes(s.as_bytes()),
            _ => Cell::Str(s),
        },
        SqlValue::Blob(b) => Cell::Bytes(b),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heading(name: &str, declared: Option<&str>) -> Heading {
        Heading {
            name: name.to_string(),
            declared: declared.map(str::to_string),
        }
    }

    #[test]
    fn a_column_is_built_as_the_type_it_was_declared() {
        let rows = result_set(
            &[heading("id", Some("INTEGER"))],
            vec![vec![SqlValue::Integer(1), SqlValue::Null]],
        );
        assert_eq!(rows.row_count(), 2);
        assert_eq!(rows.columns[0].meta.kind, ValueKind::Int);
        assert_eq!(rows.columns[0].meta.type_name, "INTEGER");
        assert_eq!(rows.columns[0].value(0), db::Value::Int(1));
        assert!(rows.columns[0].value(1).is_null());
    }

    #[test]
    fn an_integer_in_a_text_column_is_still_shown() {
        // SQLite lets this happen and a client that showed an empty cell
        // instead would be hiding the row that needs looking at.
        let rows = result_set(
            &[heading("note", Some("TEXT"))],
            vec![vec![SqlValue::Integer(42)]],
        );
        assert_eq!(rows.columns[0].meta.kind, ValueKind::Text);
        assert_eq!(
            rows.columns[0].value(0),
            db::Value::Text {
                kind: ValueKind::Text,
                text: "42".into()
            }
        );
    }

    #[test]
    fn text_beside_a_blob_survives_as_bytes() {
        let rows = result_set(
            &[heading("payload", None)],
            vec![vec![
                SqlValue::Text("hi".into()),
                SqlValue::Blob(vec![0x00]),
            ]],
        );
        assert_eq!(rows.columns[0].meta.kind, ValueKind::Bytes);
        assert_eq!(rows.columns[0].value(0), db::Value::Bytes(b"hi"[..].into()));
    }

    #[test]
    fn a_column_with_no_declared_type_is_named_after_what_it_holds() {
        let rows = result_set(
            &[heading("count(*)", None)],
            vec![vec![SqlValue::Integer(3)]],
        );
        assert_eq!(rows.columns[0].meta.type_name, "integer");
    }
}
