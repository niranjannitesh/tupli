//! What a column's type string means, and how to read its bytes.
//!
//! ClickHouse names a column's type as source text — `Array(LowCardinality(
//! Nullable(String)))` — and that text is the *only* description of the bytes
//! that follow. So this is a real parser rather than a table of names: the
//! nesting is unbounded, the parameters matter (a `Decimal(38, 4)` is sixteen
//! bytes and a `Decimal(9, 4)` is four), and getting one wrong desynchronises
//! everything after it in the block.
//!
//! That last point is why [`ChType::Unsupported`] refuses to read rather than
//! rendering a placeholder. A text protocol can skip a value it does not
//! understand; this one cannot, because nothing says how long the value is
//! except knowing what it is. A column this client cannot decode ends the read
//! with the type named in the error, which is a thing somebody can act on —
//! unlike a grid quietly full of the wrong values.

use std::fmt::Write as _;

use db::{DbResult, ValueKind};
use futures::future::BoxFuture;

use crate::wire::{self, malformed, Reader};

/// A ClickHouse type, to the depth needed to read its bytes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ChType {
    /// Width in bits: 8, 16, 32, 64, 128, 256.
    UInt(u16),
    Int(u16),
    /// 32 or 64.
    Float(u16),
    Bool,
    String,
    FixedString(usize),
    Uuid,
    Date,
    Date32,
    /// The time zone is a display attribute; the bytes are a Unix timestamp
    /// either way. Kept so the type name can be reproduced, not used to shift
    /// anything — see [`write_timestamp`].
    DateTime(Option<String>),
    DateTime64 {
        precision: u32,
        timezone: Option<String>,
    },
    /// Width in bits (32, 64, 128, 256) and how many digits are after the
    /// point. The width comes from the declared precision, which is the only
    /// place it is written down.
    Decimal {
        bits: u16,
        scale: u32,
    },
    Enum {
        /// 8 or 16, which is the width on the wire.
        bits: u16,
        values: Vec<(i16, String)>,
    },
    Ipv4,
    Ipv6,
    /// The type of `NULL` and of an empty aggregate. Occupies no bytes.
    Nothing,
    LowCardinality(Box<ChType>),
    Nullable(Box<ChType>),
    Array(Box<ChType>),
    /// Element names, for the named-tuple form.
    Tuple(Vec<(Option<String>, ChType)>),
    Map(Box<ChType>, Box<ChType>),
    /// A type whose *layout* is unknown, which is not the same as one whose
    /// rendering is awkward. Carries the source text so the error can name it.
    Unsupported(String),
}

impl ChType {
    /// The type as the grid should treat it, which is much coarser than the
    /// type itself — see [`db::ValueKind`].
    pub fn kind(&self) -> ValueKind {
        match self {
            // Anything wider than an `i64` keeps the server's own digits. A
            // `UInt64` is the awkward one: half the values a `cityHash64` or a
            // `-1` sentinel produces do not fit in the signed integer the
            // grid stores, and a column that renders those as negative
            // numbers is showing data the server does not have. The cost is
            // that sorting such a column compares text, which orders `10`
            // before `9`; showing the wrong number is worse than showing the
            // right ones in the wrong order.
            Self::UInt(64) | Self::UInt(128) | Self::UInt(256) => ValueKind::Decimal,
            Self::Int(128) | Self::Int(256) => ValueKind::Decimal,
            Self::UInt(_) | Self::Int(_) => ValueKind::Int,
            Self::Float(_) => ValueKind::Float,
            Self::Bool => ValueKind::Bool,
            Self::Decimal { .. } => ValueKind::Decimal,
            Self::String | Self::FixedString(_) | Self::Enum { .. } => ValueKind::Text,
            Self::Ipv4 | Self::Ipv6 => ValueKind::Text,
            Self::Uuid => ValueKind::Uuid,
            Self::Date | Self::Date32 => ValueKind::Date,
            Self::DateTime(_) | Self::DateTime64 { .. } => ValueKind::Timestamp,
            Self::Array(_) => ValueKind::Array,
            // Rendered the way ClickHouse renders them — `(1,'a')`, `{'k':2}`
            // — which is neither JSON nor a scalar. `Json` is the closest the
            // grid has to "structured, show it in the inspector".
            Self::Tuple(_) | Self::Map(_, _) => ValueKind::Json,
            // Both of these are all-null columns as far as anything above
            // here is concerned.
            Self::Nothing => ValueKind::Unknown,
            Self::Unsupported(_) => ValueKind::Unknown,
            Self::Nullable(inner) | Self::LowCardinality(inner) => inner.kind(),
        }
    }

    /// Whether a value of this type may be missing.
    pub fn is_nullable(&self) -> bool {
        match self {
            Self::Nullable(_) => true,
            Self::LowCardinality(inner) => inner.is_nullable(),
            _ => false,
        }
    }

    /// This type with any `Nullable` peeled off. What a `LowCardinality`
    /// dictionary is made of: the null is carried by index zero, not by a
    /// mask, so the dictionary itself is never nullable.
    fn without_nullable(&self) -> &ChType {
        match self {
            Self::Nullable(inner) => inner,
            other => other,
        }
    }
}

/// Parse a type as the server spelled it.
///
/// Never fails: text this does not recognise becomes [`ChType::Unsupported`]
/// carrying the original, which is refused later at the point where it would
/// have had to read bytes. Separating those two makes the catalog — which only
/// wants a name and a kind — survive a type the reader could not have handled.
pub fn parse(text: &str) -> ChType {
    let text = text.trim();
    let (name, args) = split_name(text);
    match (name, args) {
        ("UInt8", None) => ChType::UInt(8),
        ("UInt16", None) => ChType::UInt(16),
        ("UInt32", None) => ChType::UInt(32),
        ("UInt64", None) => ChType::UInt(64),
        ("UInt128", None) => ChType::UInt(128),
        ("UInt256", None) => ChType::UInt(256),
        ("Int8", None) => ChType::Int(8),
        ("Int16", None) => ChType::Int(16),
        ("Int32", None) => ChType::Int(32),
        ("Int64", None) => ChType::Int(64),
        ("Int128", None) => ChType::Int(128),
        ("Int256", None) => ChType::Int(256),
        ("Float32", None) => ChType::Float(32),
        ("Float64", None) => ChType::Float(64),
        ("BFloat16", None) => ChType::Unsupported(text.into()),
        ("Bool", None) => ChType::Bool,
        ("String", None) => ChType::String,
        ("UUID", None) => ChType::Uuid,
        ("Date", None) => ChType::Date,
        ("Date32", None) => ChType::Date32,
        ("IPv4", None) => ChType::Ipv4,
        ("IPv6", None) => ChType::Ipv6,
        ("Nothing", None) => ChType::Nothing,
        ("FixedString", Some(args)) => match parse_usize(args) {
            Some(len) => ChType::FixedString(len),
            None => ChType::Unsupported(text.into()),
        },
        ("DateTime", args) => ChType::DateTime(args.and_then(|a| quoted(a.trim()))),
        ("DateTime64", Some(args)) => {
            let parts = split_args(args);
            match parts.first().and_then(|p| parse_usize(p)) {
                Some(precision) => ChType::DateTime64 {
                    precision: precision as u32,
                    timezone: parts.get(1).and_then(|p| quoted(p.trim())),
                },
                None => ChType::Unsupported(text.into()),
            }
        }
        ("Decimal", Some(args)) => {
            let parts = split_args(args);
            match (
                parts.first().and_then(|p| parse_usize(p)),
                parts.get(1).and_then(|p| parse_usize(p)),
            ) {
                (Some(precision), Some(scale)) => ChType::Decimal {
                    bits: decimal_bits(precision),
                    scale: scale as u32,
                },
                _ => ChType::Unsupported(text.into()),
            }
        }
        ("Decimal32" | "Decimal64" | "Decimal128" | "Decimal256", Some(args)) => {
            let bits: u16 = name.trim_start_matches("Decimal").parse().unwrap_or(128);
            match parse_usize(args) {
                Some(scale) => ChType::Decimal {
                    bits,
                    scale: scale as u32,
                },
                None => ChType::Unsupported(text.into()),
            }
        }
        ("Enum8" | "Enum16", Some(args)) => ChType::Enum {
            bits: match name {
                "Enum8" => 8,
                _ => 16,
            },
            values: parse_enum(args),
        },
        ("Nullable", Some(args)) => ChType::Nullable(Box::new(parse(args))),
        ("LowCardinality", Some(args)) => ChType::LowCardinality(Box::new(parse(args))),
        ("Array", Some(args)) => ChType::Array(Box::new(parse(args))),
        ("Map", Some(args)) => {
            let parts = split_args(args);
            match parts.len() {
                2 => ChType::Map(Box::new(parse(&parts[0])), Box::new(parse(&parts[1]))),
                _ => ChType::Unsupported(text.into()),
            }
        }
        ("Tuple", Some(args)) => {
            ChType::Tuple(split_args(args).iter().map(parse_element).collect())
        }
        // `Nested` reaches a client only when `flatten_nested = 0`; otherwise
        // the server has already split it into `name.field` array columns.
        ("Nested", Some(args)) => ChType::Array(Box::new(ChType::Tuple(
            split_args(args).iter().map(parse_element).collect(),
        ))),
        // A `SimpleAggregateFunction` is stored as its argument type and
        // differs only in how a merge combines two of them, which is the
        // server's business and not a reader's.
        ("SimpleAggregateFunction", Some(args)) => match split_args(args).last() {
            Some(inner) => parse(inner),
            None => ChType::Unsupported(text.into()),
        },
        // The geo types are aliases the server still names by their alias.
        ("Point", None) => {
            ChType::Tuple(vec![(None, ChType::Float(64)), (None, ChType::Float(64))])
        }
        ("Ring" | "LineString", None) => ChType::Array(Box::new(parse("Point"))),
        ("Polygon" | "MultiLineString", None) => ChType::Array(Box::new(parse("Ring"))),
        ("MultiPolygon", None) => ChType::Array(Box::new(parse("Polygon"))),
        // Every interval is an `Int64` count of its own unit.
        (name, None) if name.starts_with("Interval") => ChType::Int(64),
        _ => ChType::Unsupported(text.into()),
    }
}

/// Split `Array(String)` into `("Array", Some("String"))`, and a bare name
/// into `(name, None)`.
fn split_name(text: &str) -> (&str, Option<&str>) {
    match text.find('(') {
        Some(open) if text.ends_with(')') => {
            (text[..open].trim(), Some(&text[open + 1..text.len() - 1]))
        }
        _ => (text, None),
    }
}

/// Split an argument list at the commas that are not inside a nested type or
/// a quoted enum name. `Enum8('a,b' = 1)` is one argument, and so is
/// `Tuple(Int8, Int8)` inside an `Array`.
fn split_args(args: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut current = String::new();
    let mut chars = args.chars();
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) => {
                current.push(ch);
                match ch {
                    '\\' => {
                        if let Some(escaped) = chars.next() {
                            current.push(escaped);
                        }
                    }
                    _ if ch == q => quote = None,
                    _ => {}
                }
            }
            None => match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    current.push(ch);
                }
                '(' | '[' => {
                    depth += 1;
                    current.push(ch);
                }
                ')' | ']' => {
                    depth = depth.saturating_sub(1);
                    current.push(ch);
                }
                ',' if depth == 0 => {
                    parts.push(current.trim().to_string());
                    current = String::new();
                }
                _ => current.push(ch),
            },
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    parts
}

/// One element of a `Tuple`, which may be `Int8` or `name Int8`.
fn parse_element(text: &String) -> (Option<String>, ChType) {
    let text = text.trim();
    // The name is a bare identifier followed by a space; anything else is the
    // unnamed form, including a type whose own name has no arguments.
    if let Some(space) = text.find(char::is_whitespace) {
        let (head, tail) = text.split_at(space);
        let is_identifier = !head.is_empty()
            && head
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '`');
        if is_identifier && !tail.trim().is_empty() {
            let name = head.trim_matches('`').to_string();
            return (Some(name), parse(tail.trim()));
        }
    }
    (None, parse(text))
}

/// `'ready' = 1, 'gone' = 2` → the pairs. A name this cannot read is dropped
/// rather than failing the parse: the *width* is what decoding needs, and a
/// value with no name renders as its number, which is still true.
fn parse_enum(args: &str) -> Vec<(i16, String)> {
    split_args(args)
        .iter()
        .filter_map(|part| {
            let (name, value) = part.rsplit_once('=')?;
            let value: i16 = value.trim().parse().ok()?;
            Some((value, unescape(quoted(name.trim())?)))
        })
        .collect()
}

/// Strip the quotes ClickHouse puts around a name inside a type.
fn quoted(text: &str) -> Option<String> {
    let text = text.trim();
    let inner = text
        .strip_prefix('\'')
        .and_then(|t| t.strip_suffix('\''))
        .or_else(|| text.strip_prefix('"').and_then(|t| t.strip_suffix('"')))?;
    Some(inner.to_string())
}

fn unescape(text: String) -> String {
    if !text.contains('\\') {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            },
            other => out.push(other),
        }
    }
    out
}

fn parse_usize(text: &str) -> Option<usize> {
    text.trim().parse().ok()
}

/// How wide a `Decimal(P, S)` is. The precision is the only thing that says
/// so, and the boundaries are ClickHouse's.
fn decimal_bits(precision: usize) -> u16 {
    match precision {
        0..=9 => 32,
        10..=18 => 64,
        19..=38 => 128,
        _ => 256,
    }
}

/// One value on its way from the wire into a column.
///
/// Owned, unlike [`db::Cell`], because a value read out of a nested column has
/// to be rendered into its parent's text before anything can be pushed. The
/// cost is one allocation per text value per fetch, which the column builder
/// then copies into its shared buffer.
#[derive(Clone, PartialEq, Debug)]
pub enum Cellv {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    /// A `String` column whose bytes are not UTF-8, which ClickHouse allows
    /// and people use. Kept as bytes so the column builder writes `\x…` rather
    /// than replacement characters.
    Bytes(Vec<u8>),
    /// An array, tuple or map, already rendered.
    ///
    /// Text to the grid and to everything downstream, but distinct from
    /// [`Cellv::Text`] here because the difference decides whether nesting it
    /// one level deeper quotes it: `[['a']]` is an array of arrays, and
    /// `['[\'a\']']` is an array of one string that looks like one.
    Composite(String),
}

impl Cellv {
    pub fn as_cell(&self) -> Option<db::Cell<'_>> {
        match self {
            Self::Null => None,
            Self::Bool(b) => Some(db::Cell::Bool(*b)),
            Self::Int(i) => Some(db::Cell::Int(*i)),
            Self::Float(f) => Some(db::Cell::Float(*f)),
            Self::Text(t) => Some(db::Cell::Str(t)),
            Self::Bytes(b) => Some(db::Cell::Bytes(b)),
            Self::Composite(t) => Some(db::Cell::Str(t)),
        }
    }

    /// This value as it would appear *inside* an array, a tuple or a map.
    ///
    /// Character for character what ClickHouse itself prints, which is the
    /// point: a person reading `['a','b']` in the grid and typing it back into
    /// the editor should get the same array back.
    fn nested(&self) -> String {
        match self {
            Self::Null => "NULL".into(),
            Self::Bool(b) => b.to_string(),
            Self::Int(i) => i.to_string(),
            Self::Float(f) => db::value::format_f64(*f),
            Self::Text(t) => quote(t),
            Self::Bytes(b) => quote(&String::from_utf8_lossy(b)),
            Self::Composite(t) => t.clone(),
        }
    }
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for ch in text.chars() {
        match ch {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

/// Read whatever a type writes before its values.
///
/// Only `LowCardinality` has anything — a version word — but every container
/// has to be walked, because the prefixes of a whole nested type are written
/// before any of its data. Reading them inline with each level's values would
/// work for `LowCardinality(String)` and desynchronise on
/// `Array(LowCardinality(String))`.
pub fn read_prefix<'a>(reader: Reader<'a>, ty: &'a ChType) -> BoxFuture<'a, DbResult<()>> {
    Box::pin(async move {
        match ty {
            ChType::LowCardinality(inner) => {
                let version = wire::read_u64(reader).await?;
                if version != 1 {
                    return Err(malformed(format!(
                        "a low-cardinality column encoded in a way this does not read (version {version})"
                    )));
                }
                read_prefix(reader, inner.without_nullable()).await
            }
            ChType::Nullable(inner) | ChType::Array(inner) => read_prefix(reader, inner).await,
            ChType::Map(key, value) => {
                read_prefix(reader, key).await?;
                read_prefix(reader, value).await
            }
            ChType::Tuple(elements) => {
                for (_, element) in elements {
                    read_prefix(reader, element).await?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    })
}

/// Read `rows` values of `ty`.
///
/// Boxed because the recursion is the point: a column's type can nest as deep
/// as somebody wrote it, and this is one call per column per block rather than
/// one per value.
pub fn read_values<'a>(
    reader: Reader<'a>,
    ty: &'a ChType,
    rows: usize,
) -> BoxFuture<'a, DbResult<Vec<Cellv>>> {
    Box::pin(async move {
        match ty {
            ChType::UInt(8) => read_fixed(reader, rows, 1, |b| Cellv::Int(b[0] as i64)).await,
            ChType::UInt(16) => read_fixed(reader, rows, 2, |b| Cellv::Int(le_u64(b) as i64)).await,
            ChType::UInt(32) => read_fixed(reader, rows, 4, |b| Cellv::Int(le_u64(b) as i64)).await,
            ChType::UInt(64) => {
                read_fixed(reader, rows, 8, |b| Cellv::Text(le_u64(b).to_string())).await
            }
            ChType::UInt(128) => {
                read_fixed(reader, rows, 16, |b| Cellv::Text(unsigned_decimal(b))).await
            }
            ChType::UInt(256) => {
                read_fixed(reader, rows, 32, |b| Cellv::Text(unsigned_decimal(b))).await
            }
            ChType::Int(8) => read_fixed(reader, rows, 1, |b| Cellv::Int(b[0] as i8 as i64)).await,
            ChType::Int(16) => read_fixed(reader, rows, 2, |b| Cellv::Int(le_i64(b))).await,
            ChType::Int(32) => read_fixed(reader, rows, 4, |b| Cellv::Int(le_i64(b))).await,
            ChType::Int(64) => read_fixed(reader, rows, 8, |b| Cellv::Int(le_i64(b))).await,
            ChType::Int(128) => {
                read_fixed(reader, rows, 16, |b| Cellv::Text(signed_decimal(b))).await
            }
            ChType::Int(256) => {
                read_fixed(reader, rows, 32, |b| Cellv::Text(signed_decimal(b))).await
            }
            // Every width ClickHouse has is above; anything else came from a
            // parse that should have produced `Unsupported`.
            ChType::UInt(bits) | ChType::Int(bits) => Err(malformed(format!(
                "an integer {bits} bits wide, which is not a width this reads"
            ))),
            ChType::Float(32) => {
                read_fixed(reader, rows, 4, |b| {
                    Cellv::Float(db::value::widen_f32(f32::from_le_bytes([
                        b[0], b[1], b[2], b[3],
                    ])))
                })
                .await
            }
            ChType::Float(_) => {
                read_fixed(reader, rows, 8, |b| {
                    Cellv::Float(f64::from_le_bytes(eight(b)))
                })
                .await
            }
            ChType::Bool => read_fixed(reader, rows, 1, |b| Cellv::Bool(b[0] != 0)).await,
            ChType::Uuid => read_fixed(reader, rows, 16, |b| Cellv::Text(uuid(b))).await,
            ChType::Date => {
                read_fixed(reader, rows, 2, |b| Cellv::Text(date(le_u64(b) as i64))).await
            }
            ChType::Date32 => read_fixed(reader, rows, 4, |b| Cellv::Text(date(le_i64(b)))).await,
            ChType::DateTime(_) => {
                read_fixed(reader, rows, 4, |b| {
                    Cellv::Text(timestamp(le_u64(b) as i64, 0, 0))
                })
                .await
            }
            ChType::DateTime64 { precision, .. } => {
                let precision = *precision;
                read_fixed(reader, rows, 8, move |b| {
                    let ticks = le_i64(b);
                    let divisor = 10i64.pow(precision.min(18));
                    Cellv::Text(timestamp(
                        ticks.div_euclid(divisor),
                        ticks.rem_euclid(divisor),
                        precision,
                    ))
                })
                .await
            }
            ChType::Decimal { bits, scale } => {
                let scale = *scale;
                read_fixed(reader, rows, (*bits / 8) as usize, move |b| {
                    Cellv::Text(scaled(signed_decimal(b), scale))
                })
                .await
            }
            ChType::Enum { bits, values } => {
                let width = (*bits / 8) as usize;
                read_fixed(reader, rows, width, |b| {
                    let value = le_i64(b) as i16;
                    match values.iter().find(|(v, _)| *v == value) {
                        Some((_, name)) => Cellv::Text(name.clone()),
                        // A value with no name is a server and a client that
                        // disagree about the enum; the number is still true.
                        None => Cellv::Text(value.to_string()),
                    }
                })
                .await
            }
            ChType::Ipv4 => {
                read_fixed(reader, rows, 4, |b| {
                    Cellv::Text(std::net::Ipv4Addr::from(le_u64(b) as u32).to_string())
                })
                .await
            }
            ChType::Ipv6 => {
                read_fixed(reader, rows, 16, |b| {
                    Cellv::Text(std::net::Ipv6Addr::from(sixteen(b)).to_string())
                })
                .await
            }
            ChType::FixedString(width) => {
                read_fixed(reader, rows, *width, |b| {
                    // ClickHouse pads a short value with zero bytes, and the
                    // wire cannot tell padding from a value that ends in one.
                    // Trimming is what makes `FixedString(16)` holding `ok`
                    // read as `ok` rather than as fourteen invisible
                    // characters.
                    let end = b.iter().rposition(|byte| *byte != 0).map_or(0, |i| i + 1);
                    text_or_bytes(&b[..end])
                })
                .await
            }
            ChType::String => {
                let mut values = Vec::with_capacity(rows.min(4096));
                for _ in 0..rows {
                    let bytes = wire::read_bytes(reader).await?;
                    values.push(text_or_bytes(&bytes));
                }
                Ok(values)
            }
            // The type of a bare `NULL`, and the one type whose values carry
            // no information — yet ClickHouse still writes a byte per row for
            // them, so a reader that skips it desynchronises on `select NULL`.
            ChType::Nothing => {
                wire::read_exact(reader, rows).await?;
                Ok(vec![Cellv::Null; rows])
            }
            ChType::Nullable(inner) => {
                let mask = wire::read_exact(reader, rows).await?;
                let mut values = read_values(reader, inner, rows).await?;
                for (value, is_null) in values.iter_mut().zip(mask) {
                    if is_null != 0 {
                        *value = Cellv::Null;
                    }
                }
                Ok(values)
            }
            ChType::LowCardinality(inner) => read_low_cardinality(reader, inner, rows).await,
            ChType::Array(inner) => {
                let offsets = read_offsets(reader, rows).await?;
                let total = offsets.last().copied().unwrap_or(0);
                let flat = read_values(reader, inner, total).await?;
                let mut values = Vec::with_capacity(rows);
                let mut start = 0usize;
                for end in offsets {
                    let items: Vec<String> = flat[start..end].iter().map(Cellv::nested).collect();
                    values.push(Cellv::Composite(format!("[{}]", items.join(","))));
                    start = end;
                }
                Ok(values)
            }
            ChType::Map(key, value) => {
                let offsets = read_offsets(reader, rows).await?;
                let total = offsets.last().copied().unwrap_or(0);
                let keys = read_values(reader, key, total).await?;
                let entries = read_values(reader, value, total).await?;
                let mut values = Vec::with_capacity(rows);
                let mut start = 0usize;
                for end in offsets {
                    let pairs: Vec<String> = (start..end)
                        .map(|i| format!("{}:{}", keys[i].nested(), entries[i].nested()))
                        .collect();
                    values.push(Cellv::Composite(format!("{{{}}}", pairs.join(","))));
                    start = end;
                }
                Ok(values)
            }
            ChType::Tuple(elements) => {
                // Each element is a full column of its own, side by side —
                // which is why a tuple costs nothing to store and has to be
                // reassembled here rather than read row by row.
                let mut columns = Vec::with_capacity(elements.len());
                for (_, element) in elements {
                    columns.push(read_values(reader, element, rows).await?);
                }
                let mut values = Vec::with_capacity(rows);
                for row in 0..rows {
                    // Element names are parsed and then not printed, because
                    // ClickHouse does not print them either — a named tuple
                    // and an anonymous one read the same in the grid, and the
                    // names are in the column's type, which is on the header.
                    let fields: Vec<String> =
                        columns.iter().map(|column| column[row].nested()).collect();
                    values.push(Cellv::Composite(format!("({})", fields.join(","))));
                }
                Ok(values)
            }
            ChType::Unsupported(name) => Err(db::DbError::new(
                db::ErrorClass::Server,
                format!("tupli cannot read a column of type {name} yet."),
            )),
        }
    })
}

/// The array lengths, which ClickHouse writes as cumulative ends.
async fn read_offsets(reader: Reader<'_>, rows: usize) -> DbResult<Vec<usize>> {
    let mut offsets = Vec::with_capacity(rows);
    let mut previous = 0usize;
    for _ in 0..rows {
        let offset = wire::read_u64(reader).await? as usize;
        if offset < previous {
            return Err(malformed("array offsets that go backwards"));
        }
        previous = offset;
        offsets.push(offset);
    }
    Ok(offsets)
}

/// The dictionary-encoded form, which is the one ClickHouse-specific layout
/// worth understanding: a column of a million statuses is a dictionary of four
/// strings and a million one-byte keys.
///
/// A null in a `LowCardinality(Nullable(T))` is key zero rather than a mask,
/// which is why the dictionary is read as `T` and index zero is thrown away.
async fn read_low_cardinality(
    reader: Reader<'_>,
    inner: &ChType,
    rows: usize,
) -> DbResult<Vec<Cellv>> {
    let flags = wire::read_u64(reader).await?;
    let key_width = match flags & 0xff {
        0 => 1usize,
        1 => 2,
        2 => 4,
        3 => 8,
        other => return Err(malformed(format!("a dictionary key {other} bytes wide"))),
    };

    let dictionary_size = wire::read_u64(reader).await? as usize;
    let nullable = inner.is_nullable();
    let dictionary = read_values(reader, inner.without_nullable(), dictionary_size).await?;

    let keys = wire::read_u64(reader).await? as usize;
    if keys != rows {
        return Err(malformed(format!(
            "a dictionary with {keys} keys for {rows} rows"
        )));
    }
    let raw = wire::read_exact(reader, keys * key_width).await?;
    let mut values = Vec::with_capacity(rows);
    for chunk in raw.chunks_exact(key_width) {
        let index = le_u64(chunk) as usize;
        values.push(match (nullable && index == 0, dictionary.get(index)) {
            (true, _) | (_, None) => Cellv::Null,
            (false, Some(value)) => value.clone(),
        });
    }
    Ok(values)
}

/// Read `rows` values of a fixed width, in one allocation rather than `rows`
/// of them. Every scalar type goes through here.
async fn read_fixed(
    reader: Reader<'_>,
    rows: usize,
    width: usize,
    decode: impl Fn(&[u8]) -> Cellv,
) -> DbResult<Vec<Cellv>> {
    if width == 0 {
        return Ok(vec![Cellv::Text(String::new()); rows]);
    }
    let raw = wire::read_exact(reader, rows * width).await?;
    Ok(raw.chunks_exact(width).map(decode).collect())
}

/// UTF-8 if it is, bytes if it is not. ClickHouse's `String` is a byte string;
/// treating one that is not text as text would mangle it, and treating one
/// that *is* text as bytes would hide it behind a hex dump.
fn text_or_bytes(bytes: &[u8]) -> Cellv {
    match std::str::from_utf8(bytes) {
        Ok(text) => Cellv::Text(text.to_string()),
        Err(_) => Cellv::Bytes(bytes.to_vec()),
    }
}

fn le_u64(bytes: &[u8]) -> u64 {
    let mut value = 0u64;
    for (index, byte) in bytes.iter().take(8).enumerate() {
        value |= u64::from(*byte) << (index * 8);
    }
    value
}

/// Sign-extended, so a two-byte `-1` is `-1` and not `65535`.
fn le_i64(bytes: &[u8]) -> i64 {
    let width = bytes.len().min(8);
    let value = le_u64(bytes);
    match width {
        8 => value as i64,
        _ => {
            let shift = 64 - width * 8;
            ((value << shift) as i64) >> shift
        }
    }
}

fn eight(bytes: &[u8]) -> [u8; 8] {
    let mut out = [0u8; 8];
    out.copy_from_slice(&bytes[..8]);
    out
}

fn sixteen(bytes: &[u8]) -> [u8; 16] {
    let mut out = [0u8; 16];
    out.copy_from_slice(&bytes[..16]);
    out
}

/// ClickHouse stores a UUID as two little-endian `UInt64`s, high half first.
/// Reading it as sixteen bytes in order — which is what the canonical text
/// form is — gives the right characters in the wrong places, and the result
/// looks enough like a UUID that nobody notices.
fn uuid(bytes: &[u8]) -> String {
    let high = le_u64(&bytes[..8]).to_be_bytes();
    let low = le_u64(&bytes[8..16]).to_be_bytes();
    let mut ordered = [0u8; 16];
    ordered[..8].copy_from_slice(&high);
    ordered[8..].copy_from_slice(&low);
    let mut out = String::with_capacity(36);
    for (index, byte) in ordered.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// 1970-01-01 in the Julian day numbering `time::Date` counts in.
const UNIX_EPOCH_JULIAN_DAY: i32 = 2_440_588;

fn date(days: i64) -> String {
    let mut out = String::new();
    if write_date(days, &mut out).is_none() {
        out.clear();
        // Out of the range a calendar has. Better the number than a wrong
        // date or an empty cell.
        let _ = write!(out, "{days} days");
    }
    out
}

fn write_date(days: i64, out: &mut String) -> Option<()> {
    let julian = i64::from(UNIX_EPOCH_JULIAN_DAY).checked_add(days)?;
    let date = time::Date::from_julian_day(i32::try_from(julian).ok()?).ok()?;
    write!(
        out,
        "{:04}-{:02}-{:02}",
        date.year(),
        date.month() as u8,
        date.day()
    )
    .ok()
}

/// A timestamp, rendered in UTC.
///
/// A `DateTime('Asia/Kolkata')` names a zone, but the eight bytes are a Unix
/// instant and the zone is a display preference the server applies for its own
/// text formats. Rendering in UTC is the same instant, and it is what the
/// Postgres side of this app already shows — one convention across engines
/// beats each engine's own. A tz database would be needed to do otherwise and
/// this app does not carry one.
fn timestamp(seconds: i64, fraction: i64, precision: u32) -> String {
    let mut out = String::new();
    let day = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    if write_date(day, &mut out).is_none() {
        out.clear();
        let _ = write!(out, "{seconds}");
        return out;
    }
    let _ = write!(
        out,
        " {:02}:{:02}:{:02}",
        rest / 3600,
        (rest / 60) % 60,
        rest % 60
    );
    if precision > 0 {
        let _ = write!(out, ".{fraction:0width$}", width = precision as usize);
    }
    out
}

/// Put the point back into a decimal the server sent as an integer.
fn scaled(digits: String, scale: u32) -> String {
    if scale == 0 {
        return digits;
    }
    let scale = scale as usize;
    let (sign, digits) = match digits.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", digits.as_str()),
    };
    let padded = match digits.len() > scale {
        true => digits.to_string(),
        false => format!("{}{digits}", "0".repeat(scale + 1 - digits.len())),
    };
    let split = padded.len() - scale;
    format!("{sign}{}.{}", &padded[..split], &padded[split..])
}

/// A little-endian unsigned integer of any width, in base ten.
///
/// Long division by a billion at a time over 32-bit limbs. Needed because
/// nothing narrower holds a `UInt256`, and a column of them that rendered as
/// hex — or as a truncated `f64` — would be unusable for the thing people
/// store in one, which is money.
fn unsigned_decimal(bytes: &[u8]) -> String {
    let mut limbs: Vec<u32> = bytes
        .chunks(4)
        .map(|chunk| {
            let mut value = 0u32;
            for (index, byte) in chunk.iter().enumerate() {
                value |= u32::from(*byte) << (index * 8);
            }
            value
        })
        .collect();
    while limbs.last() == Some(&0) {
        limbs.pop();
    }
    if limbs.is_empty() {
        return "0".into();
    }
    let mut groups: Vec<u32> = Vec::new();
    while !limbs.is_empty() {
        let mut remainder = 0u64;
        for limb in limbs.iter_mut().rev() {
            let value = (remainder << 32) | u64::from(*limb);
            *limb = (value / 1_000_000_000) as u32;
            remainder = value % 1_000_000_000;
        }
        groups.push(remainder as u32);
        while limbs.last() == Some(&0) {
            limbs.pop();
        }
    }
    let mut out = groups.pop().unwrap_or(0).to_string();
    while let Some(group) = groups.pop() {
        let _ = write!(out, "{group:09}");
    }
    out
}

/// The same, for two's complement.
fn signed_decimal(bytes: &[u8]) -> String {
    let negative = bytes.last().is_some_and(|byte| byte & 0x80 != 0);
    if !negative {
        return unsigned_decimal(bytes);
    }
    let mut magnitude: Vec<u8> = bytes.iter().map(|byte| !byte).collect();
    for byte in magnitude.iter_mut() {
        match *byte {
            0xff => *byte = 0,
            _ => {
                *byte += 1;
                break;
            }
        }
    }
    format!("-{}", unsigned_decimal(&magnitude))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nested_type_parses_all_the_way_down() {
        assert_eq!(
            parse("Array(LowCardinality(Nullable(String)))"),
            ChType::Array(Box::new(ChType::LowCardinality(Box::new(
                ChType::Nullable(Box::new(ChType::String))
            ))))
        );
    }

    #[test]
    fn a_decimals_width_comes_from_its_precision() {
        assert_eq!(
            parse("Decimal(9, 2)"),
            ChType::Decimal { bits: 32, scale: 2 }
        );
        assert_eq!(
            parse("Decimal(18, 4)"),
            ChType::Decimal { bits: 64, scale: 4 }
        );
        assert_eq!(
            parse("Decimal(38, 8)"),
            ChType::Decimal {
                bits: 128,
                scale: 8
            }
        );
        assert_eq!(
            parse("Decimal(76, 0)"),
            ChType::Decimal {
                bits: 256,
                scale: 0
            }
        );
        // And the aliases name the width themselves.
        assert_eq!(
            parse("Decimal64(3)"),
            ChType::Decimal { bits: 64, scale: 3 }
        );
    }

    #[test]
    fn an_enums_names_survive_the_commas_inside_them() {
        let ChType::Enum { bits, values } = parse("Enum8('a,b' = 1, 'c' = -2)") else {
            panic!("not an enum");
        };
        assert_eq!(bits, 8);
        assert_eq!(values, vec![(1, "a,b".into()), (-2, "c".into())]);
    }

    #[test]
    fn a_tuple_keeps_its_element_names() {
        let ChType::Tuple(elements) = parse("Tuple(id UInt32, name String)") else {
            panic!("not a tuple");
        };
        assert_eq!(elements[0].0.as_deref(), Some("id"));
        assert_eq!(elements[1].1, ChType::String);
        // And the unnamed form is still a tuple of the same shape.
        let ChType::Tuple(plain) = parse("Tuple(UInt32, String)") else {
            panic!("not a tuple");
        };
        assert_eq!(plain[0].0, None);
    }

    #[test]
    fn a_datetime_keeps_the_zone_it_names() {
        assert_eq!(
            parse("DateTime('Asia/Kolkata')"),
            ChType::DateTime(Some("Asia/Kolkata".into()))
        );
        assert_eq!(parse("DateTime"), ChType::DateTime(None));
        assert_eq!(
            parse("DateTime64(3, 'UTC')"),
            ChType::DateTime64 {
                precision: 3,
                timezone: Some("UTC".into())
            }
        );
    }

    #[test]
    fn a_type_this_cannot_read_says_so_rather_than_guessing() {
        // The layout of an aggregate state is the server's business, and
        // nothing in a block says how long one is — so it cannot be skipped,
        // only refused.
        assert_eq!(
            parse("AggregateFunction(sum, UInt64)"),
            ChType::Unsupported("AggregateFunction(sum, UInt64)".into())
        );
        assert_eq!(
            parse("AggregateFunction(sum, UInt64)").kind(),
            ValueKind::Unknown
        );
    }

    #[test]
    fn a_simple_aggregate_is_just_its_argument() {
        assert_eq!(
            parse("SimpleAggregateFunction(any, Nullable(String))"),
            ChType::Nullable(Box::new(ChType::String))
        );
    }

    #[test]
    fn nullability_is_visible_through_a_dictionary() {
        assert!(parse("LowCardinality(Nullable(String))").is_nullable());
        assert!(!parse("LowCardinality(String)").is_nullable());
        assert!(parse("Nullable(Int64)").is_nullable());
    }

    #[test]
    fn the_kinds_that_decide_alignment_are_the_ones_the_grid_asked_for() {
        assert_eq!(parse("Int32").kind(), ValueKind::Int);
        assert_eq!(parse("Nullable(Int32)").kind(), ValueKind::Int);
        // Wide enough that an i64 would show the wrong number.
        assert_eq!(parse("UInt64").kind(), ValueKind::Decimal);
        assert_eq!(parse("Int256").kind(), ValueKind::Decimal);
        assert_eq!(parse("LowCardinality(String)").kind(), ValueKind::Text);
        assert_eq!(parse("Array(UInt8)").kind(), ValueKind::Array);
        assert_eq!(parse("DateTime64(3)").kind(), ValueKind::Timestamp);
    }

    #[test]
    fn a_uuid_comes_off_the_wire_as_two_swapped_halves() {
        // `SELECT toUUID('00112233-4455-6677-8899-aabbccddeeff')`, as the
        // sixteen bytes ClickHouse actually sends.
        let mut wire = Vec::new();
        wire.extend_from_slice(&0x0011_2233_4455_6677u64.to_le_bytes());
        wire.extend_from_slice(&0x8899_aabb_ccdd_eeffu64.to_le_bytes());
        assert_eq!(uuid(&wire), "00112233-4455-6677-8899-aabbccddeeff");
    }

    #[test]
    fn a_wide_integer_keeps_every_digit() {
        assert_eq!(unsigned_decimal(&[0u8; 16]), "0");
        assert_eq!(
            unsigned_decimal(&u64::MAX.to_le_bytes()),
            "18446744073709551615"
        );
        // 2^128 - 1, which no primitive here holds.
        assert_eq!(
            unsigned_decimal(&[0xffu8; 16]),
            "340282366920938463463374607431768211455"
        );
        assert_eq!(signed_decimal(&[0xffu8; 16]), "-1");
        let mut minus_two = [0xffu8; 16];
        minus_two[0] = 0xfe;
        assert_eq!(signed_decimal(&minus_two), "-2");
    }

    #[test]
    fn a_decimal_gets_its_point_back() {
        assert_eq!(scaled("12345".into(), 2), "123.45");
        assert_eq!(scaled("-12345".into(), 2), "-123.45");
        // Fewer digits than the scale, which is every value below one.
        assert_eq!(scaled("5".into(), 4), "0.0005");
        assert_eq!(scaled("-5".into(), 4), "-0.0005");
        assert_eq!(scaled("42".into(), 0), "42");
    }

    #[test]
    fn dates_and_timestamps_read_the_way_the_rest_of_the_app_prints_them() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(20_000), "2024-10-04");
        // Date32 reaches before the epoch, where flooring matters.
        assert_eq!(date(-1), "1969-12-31");
        assert_eq!(timestamp(0, 0, 0), "1970-01-01 00:00:00");
        assert_eq!(timestamp(1_700_000_000, 0, 0), "2023-11-14 22:13:20");
        assert_eq!(timestamp(1_700_000_000, 5, 3), "2023-11-14 22:13:20.005");
    }

    #[test]
    fn a_narrow_signed_integer_keeps_its_sign() {
        assert_eq!(le_i64(&[0xff, 0xff]), -1);
        assert_eq!(le_i64(&[0xfe, 0xff, 0xff, 0xff]), -2);
        assert_eq!(le_i64(&[0x01, 0x00]), 1);
        assert_eq!(le_u64(&[0xff, 0xff]), 65535);
    }

    #[tokio::test]
    async fn a_nullable_column_reads_its_mask_before_its_values() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&[0, 1, 0]);
        wire.extend_from_slice(&1i32.to_le_bytes());
        wire.extend_from_slice(&0i32.to_le_bytes());
        wire.extend_from_slice(&3i32.to_le_bytes());
        let ty = parse("Nullable(Int32)");
        let mut slice = wire.as_slice();
        let values = read_values(&mut slice, &ty, 3).await.unwrap();
        assert_eq!(values, vec![Cellv::Int(1), Cellv::Null, Cellv::Int(3)]);
    }

    #[tokio::test]
    async fn an_array_column_reads_its_lengths_as_cumulative_ends() {
        let mut wire = Vec::new();
        for end in [2u64, 2, 3] {
            wire.extend_from_slice(&end.to_le_bytes());
        }
        wire.extend_from_slice(&[7u8, 8, 9]);
        let ty = parse("Array(UInt8)");
        let mut slice = wire.as_slice();
        let values = read_values(&mut slice, &ty, 3).await.unwrap();
        assert_eq!(
            values,
            vec![
                Cellv::Composite("[7,8]".into()),
                Cellv::Composite("[]".into()),
                Cellv::Composite("[9]".into()),
            ]
        );
    }

    /// An array of arrays and an array of strings that look like arrays are
    /// different values, and quoting the inner one would render them the same.
    #[tokio::test]
    async fn an_array_inside_an_array_is_not_quoted_like_a_string() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&1u64.to_le_bytes());
        wire.extend_from_slice(&1u64.to_le_bytes());
        wire.extend_from_slice(b"\x01a");
        let ty = parse("Array(Array(String))");
        let mut slice = wire.as_slice();
        let values = read_values(&mut slice, &ty, 1).await.unwrap();
        assert_eq!(values, vec![Cellv::Composite("[['a']]".into())]);
    }

    #[tokio::test]
    async fn a_dictionary_column_maps_its_keys_back_through_its_dictionary() {
        let mut wire = Vec::new();
        // One-byte keys, no flags this reader acts on.
        wire.extend_from_slice(&0u64.to_le_bytes());
        wire.extend_from_slice(&3u64.to_le_bytes());
        for word in ["", "ready", "gone"] {
            wire.push(word.len() as u8);
            wire.extend_from_slice(word.as_bytes());
        }
        wire.extend_from_slice(&4u64.to_le_bytes());
        wire.extend_from_slice(&[1u8, 2, 0, 1]);
        let ty = parse("LowCardinality(Nullable(String))");
        let mut slice = wire.as_slice();
        let values = read_values(&mut slice, &ty, 4).await.unwrap();
        assert_eq!(
            values,
            vec![
                Cellv::Text("ready".into()),
                Cellv::Text("gone".into()),
                // Index zero is the null placeholder, not the empty string.
                Cellv::Null,
                Cellv::Text("ready".into()),
            ]
        );
    }

    #[tokio::test]
    async fn a_string_that_is_not_text_stays_bytes() {
        let blob = [0x89u8, 0x50, 0xff];
        let mut wire = vec![blob.len() as u8];
        wire.extend_from_slice(&blob);
        let ty = parse("String");
        let mut slice = wire.as_slice();
        let values = read_values(&mut slice, &ty, 1).await.unwrap();
        assert_eq!(values, vec![Cellv::Bytes(blob.to_vec())]);
    }

    #[tokio::test]
    async fn an_unreadable_type_stops_the_read_instead_of_desyncing_it() {
        let ty = parse("AggregateFunction(sum, UInt64)");
        let mut slice: &[u8] = &[0u8; 64];
        let error = read_values(&mut slice, &ty, 1).await.unwrap_err();
        assert!(error.message.contains("AggregateFunction"), "{error}");
    }
}
