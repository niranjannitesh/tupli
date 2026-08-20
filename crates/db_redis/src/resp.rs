//! The reply of an arbitrary command, and the command line that asked for it.
//!
//! Postgres has one reply shape and this crate's readers each know theirs, but
//! the console has neither: whatever the user typed comes back as a tree of
//! RESP values that nothing in the app has a schema for. [`RespValue`] is that
//! tree, owned and detached from the driver's lifetime, and it knows two ways
//! to present itself — [`RespValue::to_text`] for the reply pane, which prints
//! what `redis-cli` prints because that is what people already know how to
//! read, and [`RespValue::to_result_set`] for when the reply happens to be
//! table-shaped and the grid can do better.

use db::{ColumnMeta, ResultSet, ValueKind};

use crate::rows;

/// A reply, owned.
///
/// A flattening of `redis::Value`: RESP2 and RESP3 spell the same reply
/// differently (`Okay` versus `SimpleString("OK")`, `Nil` versus an empty
/// array), and the difference is the driver's business, not the pane's.
#[derive(Clone, Debug, PartialEq)]
pub enum RespValue {
    Nil,
    Int(i64),
    Double(f64),
    Bool(bool),
    /// A simple string — `OK`, `PONG`, a type name. Unquoted when printed,
    /// because it is a keyword rather than data.
    Status(String),
    /// A bulk string: the actual data, which may not be text at all.
    Bulk(Vec<u8>),
    Array(Vec<RespValue>),
    Set(Vec<RespValue>),
    Map(Vec<(RespValue, RespValue)>),
    /// A server error carried *inside* a reply rather than as the reply. A
    /// pipeline or a `MULTI` can fail one command and succeed the rest.
    Error { code: String, message: String },
}

impl From<redis::Value> for RespValue {
    fn from(value: redis::Value) -> Self {
        use redis::Value as V;
        match value {
            V::Nil => Self::Nil,
            V::Int(n) => Self::Int(n),
            V::Double(n) => Self::Double(n),
            V::Boolean(b) => Self::Bool(b),
            V::Okay => Self::Status("OK".into()),
            V::SimpleString(s) => Self::Status(s),
            V::BulkString(bytes) => Self::Bulk(bytes),
            // A verbatim string carries a format hint (`txt`, `mkd`) that only
            // `LOLWUT` and `LATENCY DOCTOR` use. The text is the point.
            V::VerbatimString { text, .. } => Self::Bulk(text.into_bytes()),
            V::Array(items) => Self::Array(items.into_iter().map(Into::into).collect()),
            // A push message reaching here means a subscription reply arrived
            // on a command connection; show it rather than swallowing it.
            V::Push { data, .. } => Self::Array(data.into_iter().map(Into::into).collect()),
            V::Set(items) => Self::Set(items.into_iter().map(Into::into).collect()),
            V::Map(pairs) => Self::Map(
                pairs
                    .into_iter()
                    .map(|(k, v)| (k.into(), v.into()))
                    .collect(),
            ),
            // Attributes are out-of-band metadata about the reply. Nothing in
            // the UI has a place for them yet, so the reply itself wins.
            V::Attribute { data, .. } => (*data).into(),
            V::ServerError(error) => Self::Error {
                code: error.code().to_string(),
                message: error.details().unwrap_or_default().to_string(),
            },
            // `Value` is `#[non_exhaustive]`, and a big number is the one
            // variant whose Rust type depends on a feature flag. Debug is a
            // poor rendering but an honest one, and it cannot panic.
            other => Self::Bulk(format!("{other:?}").into_bytes()),
        }
    }
}

impl RespValue {
    /// Whether this is a single value rather than a container. Drives both the
    /// indenting in [`Self::to_text`] and the column-shape guess in
    /// [`Self::to_result_set`].
    pub fn is_scalar(&self) -> bool {
        !matches!(self, Self::Array(_) | Self::Set(_) | Self::Map(_))
    }

    /// The bytes this value stands for, for a grid cell. `None` is a null
    /// cell — which is `nil`, and not an empty string, those being different
    /// answers to `GET`.
    pub fn as_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Self::Nil => None,
            Self::Int(n) => Some(n.to_string().into_bytes()),
            Self::Double(n) => Some(db::value::format_f64(*n).into_bytes()),
            Self::Bool(b) => Some(if *b { b"true".to_vec() } else { b"false".to_vec() }),
            Self::Status(s) => Some(s.clone().into_bytes()),
            Self::Bulk(bytes) => Some(bytes.clone()),
            Self::Error { code, message } => Some(format!("{code} {message}").into_bytes()),
            // A container in a cell is a reply the grid could not straighten
            // out; one line of its text is better than an empty cell.
            other => Some(other.to_text().replace('\n', " ").into_bytes()),
        }
    }

    /// The value as a string, when it is one. Used by the readers that ask a
    /// question with a known answer — `TYPE`, `OBJECT ENCODING`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Status(s) => Some(s),
            Self::Bulk(bytes) => std::str::from_utf8(bytes).ok(),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            Self::Bulk(bytes) => std::str::from_utf8(bytes).ok()?.parse().ok(),
            _ => None,
        }
    }

    /// The elements, for the three variants that have some.
    fn items(&self) -> Option<&[RespValue]> {
        match self {
            Self::Array(items) | Self::Set(items) => Some(items),
            _ => None,
        }
    }

    /// The reply as `redis-cli` would print it.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out
    }

    fn write(&self, out: &mut String, indent: usize) {
        match self {
            Self::Array(items) | Self::Set(items) => {
                if items.is_empty() {
                    out.push_str("(empty array)");
                    return;
                }
                // Right-aligned indices, so a reply of a hundred rows keeps a
                // straight left edge for the values.
                let width = items.len().to_string().len();
                for (ix, item) in items.iter().enumerate() {
                    let label = format!("{:>width$}) ", ix + 1, width = width);
                    if ix > 0 {
                        out.push('\n');
                        out.extend(std::iter::repeat_n(' ', indent));
                    }
                    out.push_str(&label);
                    item.write(out, indent + label.len());
                }
            }
            Self::Map(pairs) => {
                if pairs.is_empty() {
                    out.push_str("(empty map)");
                    return;
                }
                let width = pairs.len().to_string().len();
                for (ix, (key, value)) in pairs.iter().enumerate() {
                    let label = format!("{:>width$}# ", ix + 1, width = width);
                    if ix > 0 {
                        out.push('\n');
                        out.extend(std::iter::repeat_n(' ', indent));
                    }
                    out.push_str(&label);
                    key.write(out, indent + label.len());
                    out.push_str(" => ");
                    value.write(out, indent + label.len());
                }
            }
            Self::Nil => out.push_str("(nil)"),
            Self::Int(n) => out.push_str(&format!("(integer) {n}")),
            Self::Double(n) => out.push_str(&format!("(double) {}", db::value::format_f64(*n))),
            Self::Bool(b) => out.push_str(if *b { "(true)" } else { "(false)" }),
            Self::Status(s) => out.push_str(s),
            Self::Bulk(bytes) => out.push_str(&quote(bytes)),
            Self::Error { code, message } => {
                out.push_str("(error) ");
                out.push_str(code);
                if !message.is_empty() {
                    out.push(' ');
                    out.push_str(message);
                }
            }
        }
    }

    /// The reply as a table, when it has a table's shape.
    ///
    /// Three shapes are worth straightening out, and guessing past them does
    /// more harm than good — a reply mis-split into columns is harder to read
    /// than the same reply left as a list:
    ///
    /// * a map (`CONFIG GET`, `XINFO STREAM`) — key and value;
    /// * a list of equally long lists (`SLOWLOG GET` after a fashion) — one
    ///   column per position;
    /// * a flat list — one column.
    ///
    /// Anything else, including any single value, returns `None` and is shown
    /// as text.
    pub fn to_result_set(&self) -> Option<ResultSet> {
        match self {
            Self::Map(pairs) => {
                let keys: Vec<_> = pairs.iter().map(|(k, _)| k.as_bytes()).collect();
                let values: Vec<_> = pairs.iter().map(|(_, v)| v.as_bytes()).collect();
                Some(ResultSet::new(vec![
                    rows::nullable_value_column("key", &keys),
                    rows::nullable_value_column("value", &values),
                ]))
            }
            _ => {
                let items = self.items()?;
                if items.is_empty() {
                    return None;
                }
                if items.iter().all(|item| item.is_scalar()) {
                    let values: Vec<_> = items.iter().map(RespValue::as_bytes).collect();
                    return Some(ResultSet::new(vec![rows::nullable_value_column(
                        "value", &values,
                    )]));
                }
                // Every row a list of scalars, all the same length: a table
                // whose header the server did not send.
                let width = items.first()?.items()?.len();
                if width == 0 {
                    return None;
                }
                let rectangular = items.iter().all(|row| {
                    row.items()
                        .is_some_and(|r| r.len() == width && r.iter().all(RespValue::is_scalar))
                });
                if !rectangular {
                    return None;
                }
                let columns = (0..width)
                    .map(|ix| {
                        let cells: Vec<_> = items
                            .iter()
                            .map(|row| row.items().and_then(|r| r[ix].as_bytes()))
                            .collect();
                        rows::nullable_value_column(&(ix + 1).to_string(), &cells)
                    })
                    .collect();
                Some(ResultSet::new(columns))
            }
        }
    }
}

/// An empty result set with one named column, for a reply that has no rows.
pub fn empty(name: &str) -> ResultSet {
    ResultSet::new(vec![rows::text_column(
        ColumnMeta::new(name, ValueKind::Text, "string"),
        std::iter::empty(),
    )])
}

/// A bulk string as `redis-cli` writes it: always quoted, non-printables as
/// `\xhh`, so that a trailing space or a stray newline in a value is visible
/// rather than something the user finds out about later.
fn quote(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 2);
    out.push('"');
    for &byte in bytes {
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x20..=0x7e => out.push(byte as char),
            other => out.push_str(&format!("\\x{other:02x}")),
        }
    }
    out.push('"');
    out
}

/// Split a typed command line into arguments.
///
/// The rules are `redis-cli`'s, deliberately: somebody who has a command in
/// their shell history has to be able to paste it into the console and get the
/// same thing. Double quotes take `\xhh` and the usual escapes, single quotes
/// take only `\'`, and a quote has to be followed by whitespace or the end of
/// the line — `"a"b` is a mistake worth reporting rather than a value worth
/// guessing at.
///
/// Arguments are bytes, not strings, because `\xff` is a legitimate thing to
/// send and Redis keys are binary-safe.
pub fn split_args(line: &str) -> Result<Vec<Vec<u8>>, String> {
    let bytes = line.as_bytes();
    let mut args = Vec::new();
    let mut at = 0;
    loop {
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        if at >= bytes.len() {
            return Ok(args);
        }
        let mut current = Vec::new();
        let quote = matches!(bytes[at], b'"' | b'\'').then(|| bytes[at]);
        if quote.is_some() {
            at += 1;
        }
        loop {
            match (quote, bytes.get(at)) {
                (Some(q), None) => {
                    return Err(format!(
                        "unbalanced {} in the command",
                        if q == b'"' { "quotes" } else { "single quotes" }
                    ))
                }
                (Some(q), Some(&byte)) if byte == q => {
                    at += 1;
                    match bytes.get(at) {
                        None => break,
                        Some(next) if next.is_ascii_whitespace() => break,
                        Some(_) => return Err("a closing quote must end the argument".into()),
                    }
                }
                (Some(b'"'), Some(b'\\')) => {
                    at += 1;
                    let escape = *bytes.get(at).ok_or("the command ends in a backslash")?;
                    at += 1;
                    match escape {
                        b'x' => {
                            let hi = bytes.get(at).copied().and_then(hex);
                            let lo = bytes.get(at + 1).copied().and_then(hex);
                            match (hi, lo) {
                                (Some(hi), Some(lo)) => {
                                    current.push(hi * 16 + lo);
                                    at += 2;
                                }
                                // `\x` without two hex digits is a literal x,
                                // which is what `redis-cli` does with it.
                                _ => current.push(b'x'),
                            }
                        }
                        b'n' => current.push(b'\n'),
                        b'r' => current.push(b'\r'),
                        b't' => current.push(b'\t'),
                        b'a' => current.push(0x07),
                        b'b' => current.push(0x08),
                        other => current.push(other),
                    }
                }
                (Some(b'\''), Some(b'\\')) if bytes.get(at + 1) == Some(&b'\'') => {
                    current.push(b'\'');
                    at += 2;
                }
                (None, None) => break,
                (None, Some(&byte)) if byte.is_ascii_whitespace() => break,
                (None, Some(&byte)) if byte == b'"' || byte == b'\'' => {
                    return Err("a quote must start the argument".into())
                }
                (_, Some(&byte)) => {
                    current.push(byte);
                    at += 1;
                }
            }
        }
        args.push(current);
    }
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        split_args(line)
            .unwrap()
            .into_iter()
            .map(|arg| String::from_utf8_lossy(&arg).into_owned())
            .collect()
    }

    #[test]
    fn a_command_line_splits_on_whitespace() {
        assert_eq!(args("  GET   some:key  "), ["GET", "some:key"]);
        assert!(args("").is_empty());
    }

    #[test]
    fn quotes_keep_an_argument_together() {
        assert_eq!(args(r#"SET key "hello world""#), ["SET", "key", "hello world"]);
        assert_eq!(args(r#"SET key 'it''s'"#.replace("''", "\\'").as_str()), ["SET", "key", "it's"]);
        // An empty argument is a real argument.
        assert_eq!(args(r#"SET key """#), ["SET", "key", ""]);
    }

    #[test]
    fn escapes_inside_double_quotes_follow_redis_cli() {
        assert_eq!(args(r#"SET k "a\x41\nb""#), ["SET", "k", "aA\nb"]);
        // Single quotes are literal apart from the quote itself.
        assert_eq!(args(r#"SET k '\x41'"#), ["SET", "k", "\\x41"]);
    }

    #[test]
    fn binary_arguments_survive_being_split() {
        let args = split_args(r#"SET k "\xff\x00""#).unwrap();
        assert_eq!(args[2], vec![0xff, 0x00]);
    }

    #[test]
    fn a_line_that_cannot_be_split_says_so() {
        assert!(split_args(r#"SET k "unfinished"#).is_err());
        assert!(split_args(r#"SET k "a"b"#).is_err());
        assert!(split_args(r#"SET k a"b""#).is_err());
        assert!(split_args(r#"SET k "a\"#).is_err());
    }

    #[test]
    fn a_scalar_reply_reads_like_redis_cli() {
        assert_eq!(RespValue::Nil.to_text(), "(nil)");
        assert_eq!(RespValue::Int(7).to_text(), "(integer) 7");
        assert_eq!(RespValue::Status("OK".into()).to_text(), "OK");
        assert_eq!(RespValue::Bulk(b"hi".to_vec()).to_text(), "\"hi\"");
    }

    #[test]
    fn a_value_shows_what_is_hiding_in_it() {
        // A trailing space and a newline are the kind of thing that costs an
        // afternoon when the display hides them.
        assert_eq!(RespValue::Bulk(b"a \n".to_vec()).to_text(), r#""a \n""#);
        assert_eq!(RespValue::Bulk(vec![0xff]).to_text(), r#""\xff""#);
    }

    #[test]
    fn a_nested_reply_indents_under_its_index() {
        let reply = RespValue::Array(vec![
            RespValue::Bulk(b"a".to_vec()),
            RespValue::Array(vec![
                RespValue::Bulk(b"b".to_vec()),
                RespValue::Bulk(b"c".to_vec()),
            ]),
        ]);
        assert_eq!(reply.to_text(), "1) \"a\"\n2) 1) \"b\"\n   2) \"c\"");
        assert_eq!(RespValue::Array(vec![]).to_text(), "(empty array)");
    }

    #[test]
    fn an_error_inside_a_reply_is_still_shown() {
        let reply = RespValue::Array(vec![
            RespValue::Status("OK".into()),
            RespValue::Error {
                code: "WRONGTYPE".into(),
                message: "against a key".into(),
            },
        ]);
        assert_eq!(reply.to_text(), "1) OK\n2) (error) WRONGTYPE against a key");
    }

    #[test]
    fn a_map_reply_becomes_two_columns() {
        let reply = RespValue::Map(vec![(
            RespValue::Bulk(b"maxmemory".to_vec()),
            RespValue::Bulk(b"0".to_vec()),
        )]);
        let rows = reply.to_result_set().unwrap();
        assert_eq!(rows.column_count(), 2);
        assert_eq!(rows.row_count(), 1);
    }

    #[test]
    fn a_flat_reply_becomes_one_column() {
        let reply = RespValue::Array(vec![RespValue::Int(1), RespValue::Nil]);
        let rows = reply.to_result_set().unwrap();
        assert_eq!(rows.column_count(), 1);
        assert_eq!(rows.row_count(), 2);
    }

    #[test]
    fn a_reply_of_equal_rows_becomes_a_table() {
        let row = |a: &str, b: &str| {
            RespValue::Array(vec![
                RespValue::Bulk(a.into()),
                RespValue::Bulk(b.into()),
            ])
        };
        let reply = RespValue::Array(vec![row("a", "1"), row("b", "2")]);
        let rows = reply.to_result_set().unwrap();
        assert_eq!(rows.column_count(), 2);
        assert_eq!(rows.row_count(), 2);
    }

    #[test]
    fn a_reply_of_no_particular_shape_stays_text() {
        // Ragged rows: splitting these into columns would line up values that
        // have nothing to do with each other.
        let reply = RespValue::Array(vec![
            RespValue::Array(vec![RespValue::Int(1)]),
            RespValue::Array(vec![RespValue::Int(1), RespValue::Int(2)]),
        ]);
        assert!(reply.to_result_set().is_none());
        assert!(RespValue::Int(1).to_result_set().is_none());
        assert!(RespValue::Array(vec![]).to_result_set().is_none());
    }
}
