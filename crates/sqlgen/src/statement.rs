//! One statement and its parameters.
//!
//! Values are always bound, never interpolated: a grid cell can contain a
//! quote, a backslash, or a whole `DROP TABLE`, and the only defence that
//! actually holds is that the value never becomes part of the SQL text. The
//! preview does interpolate — but the preview is a thing to read, not a thing
//! to run, and the two are produced by different functions on purpose.

use db::{Value, ValueKind};

/// What the statement is for, so a failure can say which change failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatementKind {
    Insert,
    Update,
    Delete,
    /// A schema change. It carries no parameters and expects no row count —
    /// `ALTER TABLE` reports nothing — but it commits in the same transaction
    /// as everything else, which is the whole reason it is a [`Statement`] and
    /// not a bare string.
    Ddl,
}

impl StatementKind {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Ddl => "schema change",
        }
    }
}

/// One statement of a commit.
#[derive(Clone, Debug)]
pub struct Statement {
    pub sql: String,
    pub params: Vec<Value>,
    pub kind: StatementKind,
    /// How many rows this is supposed to touch, when that is knowable.
    ///
    /// An `UPDATE` by primary key that reports zero rows did not fail — it
    /// found nothing to update, which means the row is gone or has changed
    /// underneath the grid. That is the whole point of checking: the database
    /// will not raise an error for it.
    pub expect_rows: Option<u64>,
}

impl Statement {
    /// The statement with its parameters written in, for the preview sheet.
    ///
    /// Never sent anywhere. `$10` is not `$1` followed by a zero, so the
    /// placeholder is read as a whole number rather than pattern-replaced one
    /// index at a time.
    pub fn preview(&self) -> String {
        let mut out = String::with_capacity(self.sql.len() + self.params.len() * 8);
        let mut chars = self.sql.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch != '$' {
                out.push(ch);
                continue;
            }
            let mut digits = String::new();
            while let Some(d) = chars.peek().copied().filter(char::is_ascii_digit) {
                digits.push(d);
                chars.next();
            }
            match digits.parse::<usize>().ok().and_then(|n| {
                // Placeholders are one-based; a `$0` is not one of ours.
                n.checked_sub(1).and_then(|n| self.params.get(n))
            }) {
                Some(value) => out.push_str(&literal(value)),
                None => {
                    out.push('$');
                    out.push_str(&digits);
                }
            }
        }
        out
    }
}

/// A value as SQL text. For reading only — see [`Statement::preview`].
pub fn literal(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => db::value::format_f64(*f),
        // Numbers that arrived as text stay unquoted: `total = '12.30'` is
        // legal but reads like a string comparison, and the preview is there
        // to be read.
        Value::Text {
            kind: ValueKind::Decimal,
            text,
        } if is_number(text) => text.to_string(),
        Value::Text { text, .. } => db::schema::quote_literal(text),
        Value::Bytes(bytes) => {
            let mut out = String::with_capacity(bytes.len() * 2 + 12);
            out.push_str("'\\x");
            for byte in bytes.iter() {
                use std::fmt::Write as _;
                let _ = write!(out, "{byte:02x}");
            }
            out.push_str("'::bytea");
            out
        }
    }
}

fn is_number(text: &str) -> bool {
    !text.is_empty()
        && text
            .strip_prefix('-')
            .unwrap_or(text)
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statement(sql: &str, params: Vec<Value>) -> Statement {
        Statement {
            sql: sql.to_string(),
            params,
            kind: StatementKind::Update,
            expect_rows: Some(1),
        }
    }

    #[test]
    fn a_preview_reads_the_placeholder_as_a_whole_number() {
        let params = (1..=11).map(Value::Int).collect();
        let s = statement("select $1, $10, $11", params);
        assert_eq!(s.preview(), "select 1, 10, 11");
    }

    #[test]
    fn a_quote_in_a_value_is_doubled_not_escaped_away() {
        let s = statement(
            "update t set name = $1",
            vec![Value::text(ValueKind::Text, "O'Hara")],
        );
        assert_eq!(s.preview(), "update t set name = 'O''Hara'");
    }

    #[test]
    fn a_placeholder_with_no_parameter_is_left_alone() {
        let s = statement("select $2", vec![Value::Int(1)]);
        assert_eq!(s.preview(), "select $2");
    }

    #[test]
    fn bytes_preview_as_a_hex_literal() {
        let s = statement(
            "update t set blob = $1",
            vec![Value::Bytes(vec![0x00, 0xff, 0x10].into())],
        );
        assert_eq!(s.preview(), "update t set blob = '\\x00ff10'::bytea");
    }
}
