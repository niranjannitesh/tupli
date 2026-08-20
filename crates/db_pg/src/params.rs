//! Binding a [`Value`] to a parameter of a type we may know nothing about.
//!
//! A grid can be showing a column of `inet`, `tsvector`, `hstore`, a domain
//! over a composite, or a type an extension invented this morning. Writing a
//! binary encoder per type is an infinite backlog, and it is also unnecessary:
//! Postgres will parse any type from its own text representation, which is
//! exactly the representation the value arrived in.
//!
//! So every parameter goes out in text format, whatever the server says the
//! type is. `accepts` returns true for everything for the same reason —
//! refusing a type here would mean refusing to write to a column the grid can
//! already display.

use bytes::BytesMut;
use db::{Value, ValueKind};
use postgres_types::{Format, IsNull, ToSql, Type};

/// A value on its way to the server.
#[derive(Debug)]
pub struct Param<'a>(pub &'a Value);

impl ToSql for Param<'_> {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        use bytes::BufMut as _;
        match self.0 {
            Value::Null => return Ok(IsNull::Yes),
            Value::Bool(b) => out.put_slice(if *b { b"true" } else { b"false" }),
            Value::Int(i) => out.put_slice(i.to_string().as_bytes()),
            Value::Float(f) => out.put_slice(db::value::format_f64(*f).as_bytes()),
            Value::Text { text, .. } => out.put_slice(text.as_bytes()),
            // `bytea`'s text input format. Not the same as the preview's
            // literal, which is quoted; a parameter is already framed by the
            // protocol and must not be.
            Value::Bytes(bytes) => out.put_slice(hex(bytes).as_bytes()),
        }
        Ok(IsNull::No)
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }

    fn encode_format(&self, _ty: &Type) -> Format {
        Format::Text
    }

    postgres_types::to_sql_checked!();
}

/// What a value will look like to the server, for logging and for tests.
pub fn text_form(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Bool(b) => Some(b.to_string()),
        Value::Int(i) => Some(i.to_string()),
        Value::Float(f) => Some(db::value::format_f64(*f)),
        Value::Text { text, .. } => Some(text.to_string()),
        Value::Bytes(bytes) => Some(hex(bytes)),
    }
}

/// `bytea` in the text input format Postgres has used since 9.0.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2 + 2);
    out.push_str("\\x");
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// An empty string is not a null, and the difference matters more here than
/// anywhere else in the app: one is a value the column holds, the other is the
/// absence of one. The editors are what decide which the user meant; this is
/// the helper they share.
pub fn parse(kind: ValueKind, text: &str) -> Value {
    match kind {
        ValueKind::Bool => match text.trim().to_ascii_lowercase().as_str() {
            "t" | "true" | "yes" | "on" | "1" => Value::Bool(true),
            _ => Value::Bool(false),
        },
        ValueKind::Int => match text.trim().parse::<i64>() {
            Ok(i) => Value::Int(i),
            // Not a number the app can hold — a bigint beyond i64, say. Send
            // the text and let the server judge it.
            Err(_) => Value::text(kind, text),
        },
        ValueKind::Float => match text.trim().parse::<f64>() {
            Ok(f) => Value::Float(f),
            Err(_) => Value::text(kind, text),
        },
        _ => Value::text(kind, text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_go_out_in_postgres_hex_input_format() {
        let value = Value::Bytes(vec![0x00, 0x0f, 0xff].into());
        assert_eq!(text_form(&value).as_deref(), Some("\\x000fff"));
    }

    #[test]
    fn an_empty_string_is_a_value_and_null_is_not() {
        assert_eq!(
            text_form(&Value::text(ValueKind::Text, "")),
            Some(String::new())
        );
        assert_eq!(text_form(&Value::Null), None);
    }

    #[test]
    fn a_bigint_too_big_for_the_app_still_reaches_the_server() {
        let huge = "170141183460469231731687303715884105727";
        assert_eq!(
            text_form(&parse(ValueKind::Int, huge)).as_deref(),
            Some(huge)
        );
    }
}
