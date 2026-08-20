//! The value model.
//!
//! Two representations, deliberately. [`Value`] is one owned value and is what
//! editing, the inspector, and parameter binding use — convenience matters and
//! there is only ever one of it on screen. Bulk data never uses it: a million
//! rows of `Value` is a million enum discriminants and a million heap pointers,
//! which is exactly the layout the grid cannot afford. That job belongs to
//! [`crate::column`], which stores the same data columnar.

use std::fmt;
use std::sync::Arc;

/// What a column holds, coarse enough to drive rendering decisions.
///
/// This is not the database's type. Postgres has hundreds of types and the
/// grid cares about roughly seven distinctions: is it right-aligned, is it
/// monospace, is it a blob, is it structured. Drivers map their own OIDs onto
/// this and keep the real type name alongside it in [`crate::ColumnMeta`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValueKind {
    Bool,
    Int,
    Float,
    /// Exact decimal — rendered as text so no precision is lost on the way in.
    Decimal,
    Text,
    Bytes,
    Uuid,
    Date,
    Time,
    Timestamp,
    Json,
    Array,
    /// A type no driver mapping covers. Rendered as its text representation,
    /// which is the rule that keeps type support from being an infinite backlog.
    Unknown,
}

impl ValueKind {
    /// Numbers are right-aligned so their digits line up down the column; that
    /// alignment is the only reason a column of numbers is readable at all.
    pub fn is_numeric(self) -> bool {
        matches!(self, Self::Int | Self::Float | Self::Decimal)
    }

    /// Kinds that are read as glyphs rather than as prose, and so want the
    /// mono face: fixed-width digits keep the columns of a uuid or a timestamp
    /// vertically aligned across rows.
    pub fn is_mono(self) -> bool {
        !matches!(self, Self::Text | Self::Unknown)
    }
}

/// A single owned value. Used for editing and inspection, never for bulk rows.
#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Decimals, timestamps, uuids, json, arrays and anything unmapped all live
    /// here as their text form plus the kind they came from. Keeping the text
    /// the server sent means a value the app does not understand still round
    /// trips unchanged.
    Text {
        kind: ValueKind,
        text: Arc<str>,
    },
    Bytes(Arc<[u8]>),
}

impl Value {
    pub fn kind(&self) -> ValueKind {
        match self {
            Self::Null => ValueKind::Unknown,
            Self::Bool(_) => ValueKind::Bool,
            Self::Int(_) => ValueKind::Int,
            Self::Float(_) => ValueKind::Float,
            Self::Text { kind, .. } => *kind,
            Self::Bytes(_) => ValueKind::Bytes,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    pub fn text(kind: ValueKind, text: impl Into<Arc<str>>) -> Self {
        Self::Text {
            kind: kind,
            text: text.into(),
        }
    }

    /// Text as a value of a column's kind.
    ///
    /// Typed into a cell or read out of a file — the same rule either way, so
    /// that a column takes a value from an import exactly as it would from the
    /// keyboard.
    ///
    /// Only the three kinds the app holds natively are parsed. Everything else
    /// travels as text and is judged by the server, which is what keeps type
    /// support from being an infinite backlog: a `numeric` of forty digits, a
    /// timestamp in a format this app has never seen and a `bigint` past `i64`
    /// all go out unchanged rather than being refused by a client that knows
    /// less about the type than the server does.
    pub fn parse(kind: ValueKind, text: &str) -> Self {
        match kind {
            ValueKind::Bool => match text.trim().to_ascii_lowercase().as_str() {
                "t" | "true" | "yes" | "on" | "1" => Self::Bool(true),
                _ => Self::Bool(false),
            },
            ValueKind::Int => match text.trim().parse::<i64>() {
                Ok(i) => Self::Int(i),
                Err(_) => Self::text(kind, text),
            },
            ValueKind::Float => match text.trim().parse::<f64>() {
                Ok(f) => Self::Float(f),
                Err(_) => Self::text(kind, text),
            },
            _ => Self::text(kind, text),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Deliberately empty rather than the word "NULL": the grid draws
            // null as a styled placeholder, and a value that renders as the
            // literal text `NULL` is indistinguishable from a string that
            // happens to say NULL.
            Self::Null => Ok(()),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(i) => write!(f, "{i}"),
            Self::Float(x) => write!(f, "{}", format_f64(*x)),
            Self::Text { text, .. } => f.write_str(text),
            Self::Bytes(b) => write!(f, "\\x{}", hex_prefix(b, 16)),
        }
    }
}

/// Floats without the `1.0000000000000002` noise of `{}` on a computed value,
/// and without the trailing `.0` of `{:?}` on a whole number.
///
/// Also the one place a float becomes SQL — a parameter, or a value written
/// into a generated `where` clause — so everything this returns has to be a
/// literal the server would read back as the same number.
pub fn format_f64(x: f64) -> String {
    // `inf` and `NaN` are how Rust spells them and `Infinity` and `NaN` are how
    // Postgres does. A float column really can hold both, and a grid that
    // showed `inf` would be naming a value the server has never heard of.
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return match x.is_sign_negative() {
            true => "-Infinity".into(),
            false => "Infinity".into(),
        };
    }
    let abs = x.abs();
    if x == x.trunc() && abs < 1e15 {
        return format!("{}", x as i64);
    }
    // Rust's `{}` never uses an exponent, so `3.4e38::float4` comes out as
    // thirty-nine digits — a number nobody can read and, worse, one that claims
    // to know its last twenty digits. Past the range where the digits are real,
    // and below the point where the leading zeros outnumber them, the exponent
    // is both shorter and more honest. Postgres prints these the same way.
    if abs != 0. && !(1e-4..1e16).contains(&abs) {
        return format!("{x:e}");
    }
    let mut s = format!("{x}");
    if s.len() > 17 {
        s = format!("{x:.15}");
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

/// The first `n` bytes as uppercase hex, with an ellipsis if there are more.
pub fn hex_prefix(bytes: &[u8], n: usize) -> String {
    let mut s = String::with_capacity(n * 2 + 1);
    for b in bytes.iter().take(n) {
        use fmt::Write as _;
        let _ = write!(s, "{b:02X}");
    }
    if bytes.len() > n {
        s.push('…');
    }
    s
}

/// `4.2 KB`, in whichever unit keeps it to a couple of digits.
pub fn byte_size(len: usize) -> String {
    match len {
        len if len < 1024 => format!("{len} B"),
        len if len < 1024 * 1024 => format!("{:.1} KB", len as f32 / 1024.),
        len => format!("{:.1} MB", len as f32 / (1024. * 1024.)),
    }
}

/// How much of a `bytea` is worth showing as hex in a cell one line tall.
///
/// Eight bytes is a hash prefix, a flag word, an id — things that are read as
/// hex and fit. Past that, the hex is a truncated dump of something the cell
/// cannot show anyway: `\x89504E470D0A1A0A…` is the same sixteen characters for
/// every PNG in the column, so it distinguishes nothing and costs the width of
/// the column. The size does distinguish them, and the inspector still has the
/// bytes.
pub const HEX_IN_CELL: usize = 8;

/// A `float4` as the `f64` that prints the way the server prints it.
///
/// `3.4e38::float4` is stored as the nearest `f32`, and widening that to `f64`
/// exposes every bit of the gap: `3.3999999521443642e38`. Those digits are not
/// data — they are an artefact of the conversion — and a grid that shows them
/// is disagreeing with `psql` about what is in the column. Going through the
/// shortest text that reads back as the same `f32` puts the value where the
/// server would put it, and writing it back gives the same four bytes.
pub fn widen_f32(x: f32) -> f64 {
    // `inf` and `NaN` do not round-trip through `parse`, and do not need to:
    // widening them is exact.
    format!("{x}").parse().unwrap_or(x as f64)
}

#[cfg(test)]
mod tests {
    use super::{format_f64, widen_f32};

    #[test]
    fn a_float4_prints_the_number_that_was_stored() {
        assert_eq!(format_f64(widen_f32(3.4e38)), "3.4e38");
        assert_eq!(format_f64(widen_f32(0.1)), "0.1");
        assert_eq!(format_f64(widen_f32(1.0)), "1");
        // And it is still that `f32` on the way back to the server.
        assert_eq!(widen_f32(0.1) as f32, 0.1f32);
        assert!(widen_f32(f32::INFINITY).is_infinite());
        assert!(widen_f32(f32::NAN).is_nan());
    }

    #[test]
    fn whole_numbers_lose_the_point() {
        assert_eq!(format_f64(1.0), "1");
        assert_eq!(format_f64(-42.0), "-42");
    }

    #[test]
    fn arithmetic_noise_is_rounded_away() {
        assert_eq!(format_f64(0.1 + 0.2), "0.3");
    }

    #[test]
    fn very_large_and_very_small_use_an_exponent() {
        // `3.4e38::float4`, which used to render as thirty-nine digits.
        assert_eq!(format_f64(3.4e38), "3.4e38");
        assert_eq!(format_f64(1e-20), "1e-20");
        assert_eq!(format_f64(-2.5e100), "-2.5e100");
        // And the ordinary range is left alone.
        assert_eq!(format_f64(1234.5), "1234.5");
        assert_eq!(format_f64(0.0001), "0.0001");
        assert_eq!(format_f64(0.0), "0");
    }

    #[test]
    fn the_special_values_are_spelled_the_way_the_server_spells_them() {
        assert_eq!(format_f64(f64::NAN), "NaN");
        assert_eq!(format_f64(f64::INFINITY), "Infinity");
        assert_eq!(format_f64(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn what_comes_out_reads_back_as_the_same_number() {
        for x in [3.4e38, 1e-20, 0.1 + 0.2, 1234.5, -2.5e100, 6.02e23] {
            let text = format_f64(x);
            let back: f64 = text.parse().expect(&text);
            assert!((back - x).abs() <= x.abs() * 1e-15, "{text} is not {x}");
        }
    }
}
