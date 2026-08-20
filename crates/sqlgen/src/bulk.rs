//! Many rows at once.
//!
//! [`crate::dml`] writes one statement per row because it is describing edits
//! somebody made by hand: five of them, each addressed by its own identity, and
//! a per-row `expect_rows` so a commit can say which one went wrong. A file is
//! not that. Ten thousand rows as ten thousand round trips is a progress bar
//! measured in minutes, and every one of them is the same statement.
//!
//! So: one `insert` per batch, values bound as always. The batch size is not a
//! taste — see [`fit`].

use db::{RelationRef, Value};

use crate::dml::qualified;
use crate::statement::{Statement, StatementKind};

/// Rows per statement, before the parameter limit has its say.
///
/// Large enough that the round trips stop mattering, small enough that a
/// failure names a hundred rows rather than the whole file, and that the
/// preview of one statement is something a person can still look at.
pub const BATCH: usize = 100;

/// How many rows of `columns` fit in one statement.
///
/// Postgres binds parameters as a 16-bit count, so 65,535 is a hard ceiling on
/// the whole statement rather than a tuning knob: a 40-column table cannot send
/// more than 1,638 rows at a time no matter what anybody would prefer, and a
/// 700-column one is down to 93. Getting this wrong is a `bind message has
/// N parameter formats but M parameters` from the server, which says nothing
/// about the file it came from.
pub fn fit(columns: usize, batch: usize) -> usize {
    const PARAMS: usize = u16::MAX as usize;
    match columns {
        0 => 1,
        columns => batch.min(PARAMS / columns).max(1),
    }
}

/// `rows` as `insert` statements, `batch` rows at a time.
///
/// Column names are the table's, already matched to the file's by the caller —
/// which order they are in is the import sheet's decision to make and to show,
/// not one to be inferred here.
pub fn inserts(
    relation: &RelationRef,
    columns: &[String],
    rows: &[Vec<Value>],
    batch: usize,
) -> Vec<Statement> {
    if columns.is_empty() || rows.is_empty() {
        return Vec::new();
    }
    let table = qualified(relation);
    let names: Vec<String> = columns
        .iter()
        .map(|name| db::schema::quote_ident(name))
        .collect();
    let names = names.join(", ");
    let per = fit(columns.len(), batch);

    rows.chunks(per)
        .map(|chunk| {
            let mut params = Vec::with_capacity(chunk.len() * columns.len());
            let mut tuples = Vec::with_capacity(chunk.len());
            for row in chunk {
                let mut placeholders = Vec::with_capacity(columns.len());
                // A row shorter than the header is a null in the columns it
                // does not reach. The reader refuses ragged files, so this is
                // only ever a column the user chose not to map.
                for index in 0..columns.len() {
                    params.push(row.get(index).cloned().unwrap_or(Value::Null));
                    placeholders.push(format!("${}", params.len()));
                }
                tuples.push(format!("({})", placeholders.join(", ")));
            }
            Statement {
                sql: format!("INSERT INTO {table} ({names}) VALUES {}", tuples.join(", ")),
                params,
                kind: StatementKind::Insert,
                expect_rows: Some(chunk.len() as u64),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn users() -> RelationRef {
        RelationRef::new("public", "users")
    }

    fn row(name: &str, age: i64) -> Vec<Value> {
        vec![Value::text(db::ValueKind::Text, name), Value::Int(age)]
    }

    #[test]
    fn one_statement_carries_every_row_it_was_given() {
        let columns = vec!["name".to_string(), "age".to_string()];
        let rows = vec![row("Ada", 36), row("Grace", 45)];
        let statements = inserts(&users(), &columns, &rows, BATCH);
        assert_eq!(statements.len(), 1);
        assert_eq!(
            statements[0].sql,
            "INSERT INTO public.users (name, age) VALUES ($1, $2), ($3, $4)"
        );
        assert_eq!(statements[0].params.len(), 4);
        assert_eq!(statements[0].expect_rows, Some(2));
    }

    #[test]
    fn a_file_longer_than_the_batch_becomes_more_than_one_statement() {
        let columns = vec!["name".to_string(), "age".to_string()];
        let rows: Vec<_> = (0..5).map(|n| row("Ada", n)).collect();
        let statements = inserts(&users(), &columns, &rows, 2);
        assert_eq!(statements.len(), 3);
        // The last one is short, and says so rather than claiming a full batch.
        assert_eq!(statements[2].expect_rows, Some(1));
    }

    #[test]
    fn the_batch_shrinks_to_stay_under_the_parameter_limit() {
        // 700 columns × 100 rows is 70,000 parameters, and the wire counts them
        // in sixteen bits.
        assert_eq!(fit(700, 100), 93);
        assert!(fit(700, 100) * 700 <= u16::MAX as usize);
        // A narrow table is not punished for it.
        assert_eq!(fit(2, 100), 100);
        // And a table wider than the limit still sends one row at a time rather
        // than none — the server refuses it, with its own words.
        assert_eq!(fit(70_000, 100), 1);
    }

    #[test]
    fn a_column_the_file_had_nothing_for_is_null_not_a_missing_placeholder() {
        let columns = vec!["name".to_string(), "age".to_string()];
        let rows = vec![vec![Value::text(db::ValueKind::Text, "Ada")]];
        let statements = inserts(&users(), &columns, &rows, BATCH);
        assert_eq!(statements[0].params.len(), 2);
        assert_eq!(statements[0].params[1], Value::Null);
    }

    #[test]
    fn nothing_to_insert_is_no_statements_rather_than_an_empty_one() {
        assert!(inserts(&users(), &["name".to_string()], &[], BATCH).is_empty());
        assert!(inserts(&users(), &[], &[row("Ada", 36)], BATCH).is_empty());
    }
}
