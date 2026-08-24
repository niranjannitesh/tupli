//! What a column holds, when the column does not have to say.
//!
//! SQLite's types are advisory. A column declared `INTEGER` will store the
//! string `"n/a"` if something writes one, and a column declared nothing at all
//! — every expression in a `select` — has no declared type to read. So the kind
//! is decided from both ends: what the schema says the column is for, and what
//! the rows that actually came back turned out to be.
//!
//! The values win whenever the two disagree, and they have to. A [`db::Column`]
//! picks its storage from its kind, and a string arriving at an integer column
//! is stored as null — so guessing `Int` for a column that has text in it would
//! not mis-align the grid, it would blank the cells.

use db::ValueKind;
use rusqlite::types::Value as SqlValue;

/// What the values of one column turned out to be. Several flags rather than
/// one answer, because a SQLite column may hold more than one storage class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seen {
    int: bool,
    real: bool,
    text: bool,
    blob: bool,
    /// Every integer so far has been a 0 or a 1, which is how a `BOOLEAN`
    /// column looks when it is being used as one.
    boolean: bool,
}

impl Default for Seen {
    fn default() -> Self {
        Self {
            int: false,
            real: false,
            text: false,
            blob: false,
            boolean: true,
        }
    }
}

impl Seen {
    pub fn of(values: &[SqlValue]) -> Self {
        let mut seen = Self::default();
        for value in values {
            match value {
                SqlValue::Null => {}
                SqlValue::Integer(i) => {
                    seen.int = true;
                    seen.boolean &= *i == 0 || *i == 1;
                }
                SqlValue::Real(_) => seen.real = true,
                SqlValue::Text(_) => seen.text = true,
                SqlValue::Blob(_) => seen.blob = true,
            }
        }
        seen
    }

    /// The narrowest kind that holds everything seen without losing it.
    /// `None` when the column was entirely null and there is nothing to go on.
    fn storage(self) -> Option<ValueKind> {
        if self.blob {
            return Some(ValueKind::Bytes);
        }
        if self.text {
            return Some(ValueKind::Text);
        }
        if self.real {
            return Some(ValueKind::Float);
        }
        if self.int {
            return Some(ValueKind::Int);
        }
        None
    }

    /// What to call the type of a column that never declared one — the header
    /// of every computed column in a `select`.
    pub fn storage_name(self) -> &'static str {
        match self.storage() {
            Some(ValueKind::Bytes) => "blob",
            Some(ValueKind::Text) => "text",
            Some(ValueKind::Float) => "real",
            Some(ValueKind::Int) => "integer",
            _ => "null",
        }
    }
}

/// The declared type as a kind, by SQLite's own affinity rules plus the
/// spellings people use for the things the grid renders specially.
///
/// The order matters: `DATETIME` contains `DATE` and `TIMESTAMP` contains
/// `TIME`, so the longer names are asked about first. None of these change how
/// the value is stored — SQLite has five storage classes and that is all — they
/// change how it is drawn, which is why `TIMESTAMP` is worth telling apart from
/// `TEXT` even though SQLite does not.
pub fn declared_kind(declared: &str) -> ValueKind {
    let name = declared.to_ascii_uppercase();
    let has = |needle: &str| name.contains(needle);
    if has("BOOL") {
        ValueKind::Bool
    } else if has("JSON") {
        ValueKind::Json
    } else if has("UUID") || has("GUID") {
        ValueKind::Uuid
    } else if has("DATETIME") || has("TIMESTAMP") {
        ValueKind::Timestamp
    } else if has("DATE") {
        ValueKind::Date
    } else if has("TIME") {
        ValueKind::Time
    } else if has("INT") {
        ValueKind::Int
    } else if has("CHAR") || has("CLOB") || has("TEXT") {
        ValueKind::Text
    } else if has("BLOB") {
        ValueKind::Bytes
    } else if has("REAL") || has("FLOA") || has("DOUB") {
        ValueKind::Float
    } else if has("DEC") || has("NUMERIC") || has("MONEY") {
        ValueKind::Decimal
    } else {
        ValueKind::Unknown
    }
}

/// The kind to build the column with, given both halves of the story.
pub fn kind(declared: Option<&str>, seen: Seen) -> ValueKind {
    let storage = seen.storage();
    // Anything binary makes the whole column binary, whatever it was declared
    // as. The alternative is a column that shows real text for some rows and
    // nothing for the rest, which invites editing a cell whose contents are
    // not what is displayed.
    if storage == Some(ValueKind::Bytes) {
        return ValueKind::Bytes;
    }
    let Some(declared) = declared.map(declared_kind) else {
        return storage.unwrap_or(ValueKind::Unknown);
    };
    match declared {
        // The three kinds with their own storage in a column: each is used
        // only if every value that arrived fits in it.
        ValueKind::Bool => match storage {
            Some(ValueKind::Int) if seen.boolean => ValueKind::Bool,
            None => ValueKind::Bool,
            // A `BOOLEAN` column holding 2, or 'yes', is a column of whatever
            // it is holding. Showing that is the point.
            _ => storage.unwrap_or(ValueKind::Unknown),
        },
        ValueKind::Int => match storage {
            Some(ValueKind::Int) | None => ValueKind::Int,
            _ => storage.unwrap_or(ValueKind::Unknown),
        },
        ValueKind::Float => match storage {
            Some(ValueKind::Int) | Some(ValueKind::Float) | None => ValueKind::Float,
            _ => storage.unwrap_or(ValueKind::Unknown),
        },
        // Everything else is held as the text the value renders to, which
        // anything at all fits into.
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(values: &[SqlValue]) -> Seen {
        Seen::of(values)
    }

    #[test]
    fn a_declared_type_decides_a_column_that_agrees_with_it() {
        let ints = seen(&[SqlValue::Integer(7), SqlValue::Null]);
        assert_eq!(kind(Some("INTEGER"), ints), ValueKind::Int);
        assert_eq!(kind(Some("bigint"), ints), ValueKind::Int);
        assert_eq!(
            kind(Some("TIMESTAMP"), seen(&[SqlValue::Text("now".into())])),
            ValueKind::Timestamp
        );
    }

    #[test]
    fn the_values_win_when_they_do_not_fit_what_was_declared() {
        // SQLite will store this and the grid has to draw it.
        let mixed = seen(&[SqlValue::Integer(1), SqlValue::Text("n/a".into())]);
        assert_eq!(kind(Some("INTEGER"), mixed), ValueKind::Text);
        // And one blob makes the column binary however it was declared.
        let binary = seen(&[SqlValue::Text("hi".into()), SqlValue::Blob(vec![0xff])]);
        assert_eq!(kind(Some("TEXT"), binary), ValueKind::Bytes);
    }

    #[test]
    fn a_boolean_column_is_a_boolean_only_while_it_holds_booleans() {
        let flags = seen(&[SqlValue::Integer(0), SqlValue::Integer(1)]);
        assert_eq!(kind(Some("BOOLEAN"), flags), ValueKind::Bool);
        let counts = seen(&[SqlValue::Integer(0), SqlValue::Integer(7)]);
        assert_eq!(kind(Some("BOOLEAN"), counts), ValueKind::Int);
    }

    #[test]
    fn a_column_with_no_declared_type_is_whatever_came_back() {
        assert_eq!(kind(None, seen(&[SqlValue::Integer(1)])), ValueKind::Int);
        assert_eq!(kind(None, seen(&[SqlValue::Real(1.5)])), ValueKind::Float);
        assert_eq!(
            kind(None, seen(&[SqlValue::Text("x".into())])),
            ValueKind::Text
        );
        // Nothing but nulls says nothing, and pretending otherwise would
        // right-align a column that may turn out to be prose.
        assert_eq!(kind(None, seen(&[SqlValue::Null])), ValueKind::Unknown);
    }

    #[test]
    fn the_longer_type_names_are_read_before_the_ones_inside_them() {
        assert_eq!(declared_kind("DATETIME"), ValueKind::Timestamp);
        assert_eq!(declared_kind("DATE"), ValueKind::Date);
        assert_eq!(declared_kind("TIME"), ValueKind::Time);
        assert_eq!(declared_kind("TIMESTAMP"), ValueKind::Timestamp);
    }
}
