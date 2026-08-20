//! Building the columns the grid draws.
//!
//! Every reader in this crate ends here, because the whole bet of adding a
//! second engine was that the grid would not have to change: a hash, a stream,
//! and a `select *` all arrive as a [`ResultSet`] and are drawn by the same
//! virtualised table. These are the shapes of column that Redis produces.

use db::{Cell, Column, ColumnBuilder, ColumnMeta, ResultSet, TextColumnBuilder, ValueKind};

/// A column of text — key names, hash fields, list elements, and every
/// other bulk string the server sends.
pub fn text_column<'a>(
    meta: ColumnMeta,
    values: impl IntoIterator<Item = Option<&'a str>>,
) -> Column {
    let mut builder = TextColumnBuilder::new();
    for value in values {
        builder.push(value);
    }
    builder.finish(meta)
}

/// A column of whole numbers — lengths, indices, TTLs, memory sizes.
pub fn int_column(meta: ColumnMeta, values: impl IntoIterator<Item = Option<i64>>) -> Column {
    let mut builder = ColumnBuilder::new(meta);
    for value in values {
        builder.push(value.map(Cell::Int));
    }
    builder.finish()
}

/// A column of floats. Sorted-set scores, and nothing else so far.
pub fn float_column(meta: ColumnMeta, values: impl IntoIterator<Item = Option<f64>>) -> Column {
    let mut builder = ColumnBuilder::new(meta);
    for value in values {
        builder.push(value.map(Cell::Float));
    }
    builder.finish()
}

/// A column of Redis values, which are bytes that are *usually* text.
///
/// The storage is decided by looking at the whole page rather than per value,
/// because a column has one kind and the grid aligns and fonts by it. One
/// non-UTF-8 value makes the column binary: showing `\xDEADBEEF…` for every
/// row in a column that has any binary in it is honest, whereas showing lossy
/// text for the binary rows and real text for the rest would let somebody edit
/// a cell whose displayed contents are not what is stored.
///
/// The full bytes survive either way — `Column::value` hands back
/// `Value::Bytes` for a binary column — which is what the inspector runs the
/// decoders over.
pub fn bytes_column(name: &str, values: &[Option<&[u8]>]) -> Column {
    let text = values
        .iter()
        .flatten()
        .all(|v| std::str::from_utf8(v).is_ok());
    if text {
        return text_column(
            ColumnMeta::new(name, ValueKind::Text, "string"),
            values
                .iter()
                .map(|v| v.map(|v| std::str::from_utf8(v).unwrap_or_default())),
        );
    }
    let mut builder = ColumnBuilder::new(ColumnMeta::new(name, ValueKind::Bytes, "bytes"));
    for value in values {
        builder.push(value.map(Cell::Bytes));
    }
    builder.finish()
}

/// [`bytes_column`] for the common case of values that are all present.
pub fn value_column(name: &str, values: &[Vec<u8>]) -> Column {
    let borrowed: Vec<Option<&[u8]>> = values.iter().map(|v| Some(v.as_slice())).collect();
    bytes_column(name, &borrowed)
}

/// [`bytes_column`] for readers whose values can be missing — an `mget` of a
/// key that expired between the scan and the read, a nil inside a reply array.
pub fn nullable_value_column(name: &str, values: &[Option<Vec<u8>>]) -> Column {
    let borrowed: Vec<Option<&[u8]>> = values.iter().map(|v| v.as_deref()).collect();
    bytes_column(name, &borrowed)
}

/// A result set of one column and one row, for the replies that are a single
/// value. Rendering those as a one-cell table rather than as a special case is
/// what lets the console reuse the grid.
pub fn single(name: &str, value: Option<&[u8]>) -> ResultSet {
    ResultSet::new(vec![bytes_column(name, &[value])])
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::{CellText, Value};

    #[test]
    fn a_column_of_text_stays_text() {
        let column = value_column("value", &[b"one".to_vec(), b"two".to_vec()]);
        assert_eq!(column.meta.kind, ValueKind::Text);
        let mut scratch = String::new();
        assert!(matches!(
            column.render(0, &mut scratch),
            CellText::Borrowed("one")
        ));
    }

    #[test]
    fn one_binary_value_makes_the_whole_column_binary() {
        let column = value_column("value", &[b"text".to_vec(), vec![0xff, 0x00]]);
        assert_eq!(column.meta.kind, ValueKind::Bytes);
        // And the bytes are all still there for the inspector to decode.
        assert_eq!(column.value(1), Value::Bytes(vec![0xff, 0x00].into()));
        let mut scratch = String::new();
        assert!(matches!(column.render(1, &mut scratch), CellText::Formatted));
        assert_eq!(scratch, "\\xFF00");
    }

    #[test]
    fn a_missing_value_is_null_and_not_an_empty_string() {
        let column = nullable_value_column("value", &[Some(b"here".to_vec()), None]);
        let mut scratch = String::new();
        assert!(matches!(column.render(1, &mut scratch), CellText::Null));
    }

    #[test]
    fn scores_and_indices_keep_their_numeric_alignment() {
        let scores = float_column(
            ColumnMeta::new("score", ValueKind::Float, "double"),
            [Some(1.5)],
        );
        assert!(scores.meta.kind.is_numeric());
        let indices = int_column(ColumnMeta::new("index", ValueKind::Int, "integer"), [Some(0)]);
        assert!(indices.meta.kind.is_numeric());
    }

    #[test]
    fn a_single_value_is_a_one_cell_table() {
        let rows = single("value", Some(b"hello"));
        assert_eq!(rows.row_count(), 1);
        assert_eq!(rows.column_count(), 1);
    }
}
