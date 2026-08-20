//! Postgres wire values → text and numbers the grid can hold.
//!
//! tokio-postgres asks for every result column in **binary** format, so this
//! module owns the decoding. That is a deliberate trade: binary avoids the
//! server formatting a million integers into decimal strings, but it means a
//! type nobody wrote a decoder for arrives as opaque bytes rather than as its
//! text form.
//!
//! The escape hatch is the type's *kind* rather than its OID. An enum arrives
//! as its label, a domain as its base type, an array as its elements, a
//! composite as its fields, a range as its bounds — so unmapped user-defined
//! types built out of mapped ones still render correctly, and only a genuinely
//! novel base type falls through to hex.
//!
//! Timestamps assume the session is `UTC`. [`crate::client`] sets it on
//! connect, precisely so that this module never has to guess at a time zone.

use std::fmt::Write as _;

use db::ValueKind;
use fallible_iterator::FallibleIterator as _;
use postgres_protocol::types;
use postgres_types::{Kind, Type};

/// What a decoded value turned out to be.
///
/// `Text` means "written into the `out` buffer you passed in" — the caller owns
/// that buffer and reuses it for the whole fetch, so a column of a million
/// timestamps costs one allocation, not a million.
#[derive(Debug)]
pub enum Decoded<'a> {
    Bool(bool),
    Int(i64),
    Float(f64),
    Bytes(&'a [u8]),
    Text,
}

/// The grid-level kind for a Postgres type.
///
/// Coarse on purpose: the grid needs to know how to align and face a column,
/// not what the type is. The real type name travels alongside in
/// [`db::ColumnMeta::type_name`].
pub fn kind_for(ty: &Type) -> ValueKind {
    match *ty {
        Type::BOOL => ValueKind::Bool,
        Type::INT2 | Type::INT4 | Type::INT8 | Type::OID | Type::XID | Type::CID | Type::TID => {
            ValueKind::Int
        }
        Type::FLOAT4 | Type::FLOAT8 => ValueKind::Float,
        Type::NUMERIC | Type::MONEY => ValueKind::Decimal,
        Type::TEXT
        | Type::VARCHAR
        | Type::BPCHAR
        | Type::NAME
        | Type::CHAR
        | Type::XML
        | Type::UNKNOWN => ValueKind::Text,
        Type::BYTEA => ValueKind::Bytes,
        Type::UUID => ValueKind::Uuid,
        Type::DATE => ValueKind::Date,
        Type::TIME | Type::TIMETZ => ValueKind::Time,
        Type::TIMESTAMP | Type::TIMESTAMPTZ => ValueKind::Timestamp,
        Type::JSON | Type::JSONB => ValueKind::Json,
        _ => match ty.kind() {
            Kind::Array(_) => ValueKind::Array,
            // A domain is its base type wearing a different name; treating it
            // as anything else would right-align some `positive_int` columns
            // and not others.
            Kind::Domain(inner) => kind_for(inner),
            // Enum labels are prose, and prose gets the UI face.
            Kind::Enum(_) => ValueKind::Text,
            _ => ValueKind::Unknown,
        },
    }
}

/// The kind for a type the catalog described but [`Type::from_oid`] does not
/// know — an enum, a domain, a composite, an extension's own type.
///
/// Introspection has the catalog row in hand, so it can answer from
/// `pg_type.typtype`/`typcategory` instead of guessing. `base_oid` is
/// `typbasetype`, non-zero only for domains, and it is tried first: a domain
/// over `int4` should behave in every way like `int4`.
pub fn kind_for_catalog(oid: u32, base_oid: u32, typtype: &str, category: &str) -> ValueKind {
    if let Some(ty) = Type::from_oid(oid) {
        return kind_for(&ty);
    }
    if base_oid != 0 {
        if let Some(base) = Type::from_oid(base_oid) {
            return kind_for(&base);
        }
    }
    match typtype {
        // An enum's values are labels, which read as prose.
        "e" => ValueKind::Text,
        _ => match category {
            "B" => ValueKind::Bool,
            "N" => ValueKind::Decimal,
            "S" => ValueKind::Text,
            "D" => ValueKind::Timestamp,
            "A" => ValueKind::Array,
            "U" if oid == 0 => ValueKind::Unknown,
            _ => ValueKind::Unknown,
        },
    }
}

/// Decode one non-null value.
///
/// Never fails: a value this module cannot make sense of is rendered as hex
/// rather than aborting a fetch that is otherwise fine. A fetch that dies on
/// row 400,000 because of one unusual column is worse than a column of hex.
pub fn decode<'a>(ty: &Type, raw: &'a [u8], out: &mut String) -> Decoded<'a> {
    match try_decode(ty, raw, out) {
        Some(decoded) => decoded,
        None => {
            out.clear();
            write_hex(raw, out);
            Decoded::Text
        }
    }
}

fn try_decode<'a>(ty: &Type, raw: &'a [u8], out: &mut String) -> Option<Decoded<'a>> {
    match *ty {
        Type::BOOL => return Some(Decoded::Bool(types::bool_from_sql(raw).ok()?)),
        Type::INT2 => return Some(Decoded::Int(types::int2_from_sql(raw).ok()? as i64)),
        Type::INT4 => return Some(Decoded::Int(types::int4_from_sql(raw).ok()? as i64)),
        Type::INT8 => return Some(Decoded::Int(types::int8_from_sql(raw).ok()?)),
        Type::OID | Type::XID | Type::CID => {
            return Some(Decoded::Int(types::oid_from_sql(raw).ok()? as i64));
        }
        Type::FLOAT4 => {
            return Some(Decoded::Float(db::value::widen_f32(
                types::float4_from_sql(raw).ok()?,
            )))
        }
        Type::FLOAT8 => return Some(Decoded::Float(types::float8_from_sql(raw).ok()?)),
        Type::BYTEA => return Some(Decoded::Bytes(types::bytea_from_sql(raw))),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::XML | Type::UNKNOWN => {
            out.clear();
            out.push_str(types::text_from_sql(raw).ok()?);
            return Some(Decoded::Text);
        }
        _ => {}
    }

    out.clear();
    write_text(ty, raw, out)?;
    Some(Decoded::Text)
}

/// The text form of a value, appended to `out`.
///
/// Split out from [`decode`] because the container types recurse into it: an
/// array renders its elements with the same code that renders a bare one, which
/// is what keeps `timestamptz[]` from formatting differently to `timestamptz`.
fn write_text(ty: &Type, raw: &[u8], out: &mut String) -> Option<()> {
    match *ty {
        Type::BOOL => out.push_str(if types::bool_from_sql(raw).ok()? {
            "true"
        } else {
            "false"
        }),
        Type::INT2 => write!(out, "{}", types::int2_from_sql(raw).ok()?).ok()?,
        Type::INT4 => write!(out, "{}", types::int4_from_sql(raw).ok()?).ok()?,
        Type::INT8 => write!(out, "{}", types::int8_from_sql(raw).ok()?).ok()?,
        Type::OID | Type::XID | Type::CID => {
            write!(out, "{}", types::oid_from_sql(raw).ok()?).ok()?
        }
        Type::FLOAT4 => out.push_str(&db::value::format_f64(db::value::widen_f32(
            types::float4_from_sql(raw).ok()?,
        ))),
        Type::FLOAT8 => out.push_str(&db::value::format_f64(types::float8_from_sql(raw).ok()?)),
        Type::CHAR => write!(out, "{}", types::char_from_sql(raw).ok()?).ok()?,
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::XML | Type::UNKNOWN => {
            out.push_str(types::text_from_sql(raw).ok()?)
        }
        Type::BYTEA => {
            out.push_str("\\x");
            write_hex(types::bytea_from_sql(raw), out);
        }
        Type::NUMERIC => write_numeric(raw, out)?,
        // `money` is int64 scaled by the server's `lc_monetary` fractional
        // digits, which is 2 everywhere it matters. The currency symbol is
        // deliberately dropped: it is a display setting of the *server*, and
        // repeating it in a grid cell only makes the column harder to scan.
        Type::MONEY => {
            let cents = types::int8_from_sql(raw).ok()?;
            write!(out, "{}.{:02}", cents / 100, (cents % 100).abs()).ok()?
        }
        Type::UUID => write_uuid(types::uuid_from_sql(raw).ok()?, out),
        Type::DATE => write_date(types::date_from_sql(raw).ok()?, out)?,
        Type::TIME => write_time_of_day(types::time_from_sql(raw).ok()?, out),
        Type::TIMETZ => {
            // micros then a zone offset in seconds, sign-flipped from the way
            // people write offsets.
            let (time, zone) = raw.split_at_checked(8)?;
            write_time_of_day(types::time_from_sql(time).ok()?, out);
            write_zone(-i32::from_be_bytes(zone.try_into().ok()?), out);
        }
        Type::TIMESTAMP => write_timestamp(types::timestamp_from_sql(raw).ok()?, out)?,
        Type::TIMESTAMPTZ => {
            write_timestamp(types::timestamp_from_sql(raw).ok()?, out)?;
            // The session is UTC (see the module header), so the offset is
            // always zero and saying so is more honest than omitting it.
            out.push_str("+00");
        }
        Type::INTERVAL => write_interval(raw, out)?,
        Type::JSON => out.push_str(std::str::from_utf8(raw).ok()?),
        Type::JSONB => {
            // One version byte, then the same text json uses.
            let (version, body) = raw.split_first()?;
            if *version != 1 {
                return None;
            }
            out.push_str(std::str::from_utf8(body).ok()?);
        }
        // The geometric family. All of it is float8s in a fixed order, and
        // all of it is written the way `::text` writes it, so a cell can be
        // pasted back into a statement.
        Type::POINT => write_point(&mut { raw }, out)?,
        Type::LSEG | Type::BOX => {
            let buf = &mut { raw };
            // A `box` prints its corners bare, an `lseg` wraps them in
            // brackets. The server normalises a box's corners itself.
            let bracketed = *ty == Type::LSEG;
            if bracketed {
                out.push('[');
            }
            write_point(buf, out)?;
            out.push(',');
            write_point(buf, out)?;
            if bracketed {
                out.push(']');
            }
        }
        Type::LINE => {
            // Ax + By + C = 0.
            let buf = &mut { raw };
            out.push('{');
            for i in 0..3 {
                if i > 0 {
                    out.push(',');
                }
                write_f64(buf, out)?;
            }
            out.push('}');
        }
        Type::PATH => {
            // One byte saying whether the path is closed, then the point
            // count, then the points. Closed paths print in parentheses and
            // open ones in brackets, which is the only thing that tells them
            // apart on the page.
            let buf = &mut { raw };
            let (closed, rest) = buf.split_first()?;
            *buf = rest;
            let (open_bracket, close_bracket) = match closed {
                0 => ('[', ']'),
                _ => ('(', ')'),
            };
            out.push(open_bracket);
            write_points(buf, out)?;
            out.push(close_bracket);
        }
        Type::POLYGON => {
            let buf = &mut { raw };
            out.push('(');
            write_points(buf, out)?;
            out.push(')');
        }
        Type::CIRCLE => {
            let buf = &mut { raw };
            out.push('<');
            write_point(buf, out)?;
            out.push(',');
            write_f64(buf, out)?;
            out.push('>');
        }
        Type::INET | Type::CIDR => write_inet(raw, out)?,
        Type::MACADDR => {
            let mac = types::macaddr_from_sql(raw).ok()?;
            for (i, byte) in mac.iter().enumerate() {
                if i > 0 {
                    out.push(':');
                }
                write!(out, "{byte:02x}").ok()?;
            }
        }
        Type::BIT | Type::VARBIT => {
            let bits = types::varbit_from_sql(raw).ok()?;
            for i in 0..bits.len() {
                let byte = bits.bytes().get(i / 8)?;
                out.push(if byte & (0x80 >> (i % 8)) != 0 {
                    '1'
                } else {
                    '0'
                });
            }
        }
        _ => return write_by_kind(ty, raw, out),
    }
    Some(())
}

/// Types with no OID-specific decoder, handled by what they are made of.
fn write_by_kind(ty: &Type, raw: &[u8], out: &mut String) -> Option<()> {
    match ty.kind() {
        // The wire form of an enum is its label.
        Kind::Enum(_) => out.push_str(std::str::from_utf8(raw).ok()?),
        Kind::Domain(inner) => return write_text(inner, raw, out),
        Kind::Array(element) => {
            let array = types::array_from_sql(raw).ok()?;
            out.push('{');
            let mut values = array.values();
            let mut first = true;
            while let Some(value) = values.next().ok()? {
                if !first {
                    out.push(',');
                }
                first = false;
                match value {
                    None => out.push_str("NULL"),
                    Some(value) => write_element(element, value, out)?,
                }
            }
            out.push('}');
        }
        Kind::Range(element) => match types::range_from_sql(raw).ok()? {
            types::Range::Empty => out.push_str("empty"),
            types::Range::Nonempty(lower, upper) => {
                out.push(match lower {
                    types::RangeBound::Inclusive(_) => '[',
                    _ => '(',
                });
                write_bound(element, &lower, out)?;
                out.push(',');
                write_bound(element, &upper, out)?;
                out.push(match upper {
                    types::RangeBound::Inclusive(_) => ']',
                    _ => ')',
                });
            }
        },
        Kind::Composite(fields) => {
            // int32 field count, then per field: int32 oid, int32 len, bytes.
            let mut buf = raw;
            let count = read_i32(&mut buf)? as usize;
            if count != fields.len() {
                return None;
            }
            out.push('(');
            for (i, field) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let _oid = read_i32(&mut buf)?;
                let len = read_i32(&mut buf)?;
                if len < 0 {
                    continue; // NULL renders as nothing inside a record, as it does in psql.
                }
                let (value, rest) = buf.split_at_checked(len as usize)?;
                buf = rest;
                write_element(field.type_(), value, out)?;
            }
            out.push(')');
        }
        _ => return None,
    }
    Some(())
}

/// An element inside an array, range, or record, quoted the way Postgres quotes
/// it so the rendered value round-trips through `::text`.
fn write_element(ty: &Type, raw: &[u8], out: &mut String) -> Option<()> {
    let start = out.len();
    write_text(ty, raw, out)?;
    let needs_quotes = {
        let rendered = &out[start..];
        rendered.is_empty()
            || rendered.contains([',', '{', '}', '(', ')', '"', '\\', ' '])
            || rendered.eq_ignore_ascii_case("null")
    };
    if needs_quotes {
        let escaped = out[start..].replace('\\', "\\\\").replace('"', "\\\"");
        out.truncate(start);
        out.push('"');
        out.push_str(&escaped);
        out.push('"');
    }
    Some(())
}

fn write_bound(
    ty: &Type,
    bound: &types::RangeBound<Option<&[u8]>>,
    out: &mut String,
) -> Option<()> {
    match bound {
        types::RangeBound::Unbounded => Some(()),
        types::RangeBound::Inclusive(value) | types::RangeBound::Exclusive(value) => match value {
            None => Some(()),
            Some(value) => write_element(ty, value, out),
        },
    }
}

// ---- scalar formatting ---------------------------------------------------

/// Postgres' binary `numeric`: base-10,000 digits with an explicit scale.
///
/// Decoded to text rather than to `f64` because that is the entire point of the
/// type. A `numeric(20,4)` that arrives as a float has already lost the value
/// somebody chose `numeric` to protect.
fn write_numeric(raw: &[u8], out: &mut String) -> Option<()> {
    const SIGN_POSITIVE: u16 = 0x0000;
    const SIGN_NEGATIVE: u16 = 0x4000;
    const SIGN_NAN: u16 = 0xC000;
    const SIGN_PINF: u16 = 0xD000;
    const SIGN_NINF: u16 = 0xF000;

    let mut buf = raw;
    let ndigits = read_i16(&mut buf)?;
    let weight = read_i16(&mut buf)?;
    let sign = read_i16(&mut buf)? as u16;
    let dscale = read_i16(&mut buf)?;

    match sign {
        SIGN_NAN => {
            out.push_str("NaN");
            return Some(());
        }
        SIGN_PINF => {
            out.push_str("Infinity");
            return Some(());
        }
        SIGN_NINF => {
            out.push_str("-Infinity");
            return Some(());
        }
        SIGN_POSITIVE | SIGN_NEGATIVE => {}
        _ => return None,
    }
    if sign == SIGN_NEGATIVE {
        out.push('-');
    }

    let digits: Vec<i16> = (0..ndigits)
        .map(|_| read_i16(&mut buf))
        .collect::<Option<_>>()?;

    // Integer part: groups from weight down to 0. A weight below zero means the
    // value is smaller than one and the integer part is a bare `0`.
    if weight < 0 {
        out.push('0');
    } else {
        for i in 0..=weight {
            let digit = digits.get(i as usize).copied().unwrap_or(0);
            if i == 0 {
                write!(out, "{digit}").ok()?;
            } else {
                write!(out, "{digit:04}").ok()?;
            }
        }
    }

    if dscale > 0 {
        out.push('.');
        // Fractional groups start just past the integer ones and are always
        // four digits wide; the declared scale decides where to stop, which is
        // how `1.10` keeps its trailing zero.
        let mut written = 0;
        let mut group = weight + 1;
        while written < dscale {
            let digit = if group < 0 {
                0
            } else {
                digits.get(group as usize).copied().unwrap_or(0)
            };
            let text = format!("{digit:04}");
            let take = ((dscale - written) as usize).min(4);
            out.push_str(&text[..take]);
            written += take as i16;
            group += 1;
        }
    }
    Some(())
}

/// Days since 2000-01-01 → `YYYY-MM-DD`.
fn write_date(days: i32, out: &mut String) -> Option<()> {
    // The sentinels Postgres uses for `'infinity'::date`.
    if days == i32::MAX {
        out.push_str("infinity");
        return Some(());
    }
    if days == i32::MIN {
        out.push_str("-infinity");
        return Some(());
    }
    let date = time::Date::from_julian_day(PG_EPOCH_JULIAN_DAY.checked_add(days)?).ok()?;
    write!(
        out,
        "{:04}-{:02}-{:02}",
        date.year(),
        date.month() as u8,
        date.day()
    )
    .ok()
}

/// Microseconds since 2000-01-01 00:00 → `YYYY-MM-DD HH:MM:SS[.ffffff]`.
fn write_timestamp(micros: i64, out: &mut String) -> Option<()> {
    if micros == i64::MAX {
        out.push_str("infinity");
        return Some(());
    }
    if micros == i64::MIN {
        out.push_str("-infinity");
        return Some(());
    }
    // Floor division, so times before the epoch land on the right day.
    let day = micros.div_euclid(MICROS_PER_DAY);
    let rest = micros.rem_euclid(MICROS_PER_DAY);
    write_date(i32::try_from(day).ok()?, out)?;
    out.push(' ');
    write_time_of_day(rest, out);
    Some(())
}

/// Microseconds since midnight → `HH:MM:SS[.ffffff]`.
fn write_time_of_day(micros: i64, out: &mut String) {
    let seconds = micros.div_euclid(1_000_000);
    let fraction = micros.rem_euclid(1_000_000);
    let _ = write!(
        out,
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    );
    write_fraction(fraction, out);
}

/// The fractional seconds, with trailing zeros trimmed the way Postgres trims
/// them — `.5`, not `.500000`.
fn write_fraction(micros: i64, out: &mut String) {
    if micros == 0 {
        return;
    }
    let text = format!("{micros:06}");
    out.push('.');
    out.push_str(text.trim_end_matches('0'));
}

fn write_zone(offset_seconds: i32, out: &mut String) {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let offset = offset_seconds.unsigned_abs();
    let (hours, minutes, seconds) = (offset / 3600, (offset / 60) % 60, offset % 60);
    let _ = write!(out, "{sign}{hours:02}");
    if minutes != 0 || seconds != 0 {
        let _ = write!(out, ":{minutes:02}");
    }
    if seconds != 0 {
        let _ = write!(out, ":{seconds:02}");
    }
}

/// `interval`: micros, days, months — three independent fields, because a month
/// is not a fixed number of days and Postgres refuses to pretend otherwise.
fn write_interval(raw: &[u8], out: &mut String) -> Option<()> {
    let mut buf = raw;
    let micros = read_i64(&mut buf)?;
    let days = read_i32(&mut buf)?;
    let months = read_i32(&mut buf)?;

    let mut wrote = false;
    let mut part = |value: i64, unit: &str, out: &mut String| {
        if value == 0 {
            return;
        }
        if wrote {
            out.push(' ');
        }
        wrote = true;
        let _ = write!(out, "{value} {unit}");
        if value.abs() != 1 {
            out.push('s');
        }
    };
    part((months / 12) as i64, "year", out);
    part((months % 12) as i64, "mon", out);
    part(days as i64, "day", out);

    if micros != 0 {
        if wrote {
            out.push(' ');
        }
        let sign = if micros < 0 { "-" } else { "" };
        let abs = micros.abs();
        let seconds = abs / 1_000_000;
        let _ = write!(
            out,
            "{sign}{:02}:{:02}:{:02}",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        );
        write_fraction(abs % 1_000_000, out);
        wrote = true;
    }
    if !wrote {
        out.push_str("00:00:00");
    }
    Some(())
}

fn write_inet(raw: &[u8], out: &mut String) -> Option<()> {
    // family, netmask bits, is_cidr, address length, address.
    let family = *raw.first()?;
    let bits = *raw.get(1)?;
    let len = *raw.get(3)? as usize;
    let addr = raw.get(4..4 + len)?;
    match family {
        2 if len == 4 => {
            let _ = write!(out, "{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3]);
            if bits != 32 {
                let _ = write!(out, "/{bits}");
            }
        }
        3 if len == 16 => {
            let groups: Vec<String> = addr
                .chunks_exact(2)
                .map(|c| format!("{:x}", u16::from_be_bytes([c[0], c[1]])))
                .collect();
            out.push_str(&groups.join(":"));
            if bits != 128 {
                let _ = write!(out, "/{bits}");
            }
        }
        _ => return None,
    }
    Some(())
}

fn write_uuid(bytes: [u8; 16], out: &mut String) {
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        let _ = write!(out, "{byte:02x}");
    }
}

/// `(x,y)` — the shape every geometric type is built out of.
fn write_point(buf: &mut &[u8], out: &mut String) -> Option<()> {
    out.push('(');
    write_f64(buf, out)?;
    out.push(',');
    write_f64(buf, out)?;
    out.push(')');
    Some(())
}

/// An int32 count followed by that many points, as `path` and `polygon` store
/// their vertices. The caller supplies the brackets.
fn write_points(buf: &mut &[u8], out: &mut String) -> Option<()> {
    let count = read_i32(buf)?;
    if count < 0 {
        return None;
    }
    for i in 0..count {
        if i > 0 {
            out.push(',');
        }
        write_point(buf, out)?;
    }
    Some(())
}

fn write_f64(buf: &mut &[u8], out: &mut String) -> Option<()> {
    let (head, rest) = buf.split_at_checked(8)?;
    *buf = rest;
    out.push_str(&db::value::format_f64(f64::from_be_bytes(
        head.try_into().ok()?,
    )));
    Some(())
}

fn write_hex(bytes: &[u8], out: &mut String) {
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
}

// ---- little readers ------------------------------------------------------

/// 2000-01-01, which is what every Postgres date and timestamp counts from.
const PG_EPOCH_JULIAN_DAY: i32 = 2_451_545;
const MICROS_PER_DAY: i64 = 86_400_000_000;

fn read_i16(buf: &mut &[u8]) -> Option<i16> {
    let (head, rest) = buf.split_at_checked(2)?;
    *buf = rest;
    Some(i16::from_be_bytes(head.try_into().ok()?))
}

fn read_i32(buf: &mut &[u8]) -> Option<i32> {
    let (head, rest) = buf.split_at_checked(4)?;
    *buf = rest;
    Some(i32::from_be_bytes(head.try_into().ok()?))
}

fn read_i64(buf: &mut &[u8]) -> Option<i64> {
    let (head, rest) = buf.split_at_checked(8)?;
    *buf = rest;
    Some(i64::from_be_bytes(head.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(ty: &Type, raw: &[u8]) -> String {
        let mut out = String::new();
        write_text(ty, raw, &mut out).expect("decodes");
        out
    }

    /// The binary form of a `numeric`, as the server sends it.
    fn numeric(weight: i16, sign: u16, dscale: i16, digits: &[i16]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend((digits.len() as i16).to_be_bytes());
        buf.extend(weight.to_be_bytes());
        buf.extend(sign.to_be_bytes());
        buf.extend(dscale.to_be_bytes());
        for digit in digits {
            buf.extend(digit.to_be_bytes());
        }
        buf
    }

    /// Points as the server sends them: pairs of big-endian float8s.
    fn coords(values: &[f64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    #[test]
    fn the_geometric_family_prints_the_way_postgres_writes_it() {
        assert_eq!(text(&Type::POINT, &coords(&[1., 2.])), "(1,2)");
        assert_eq!(
            text(&Type::LSEG, &coords(&[1., 2., 3., 4.5])),
            "[(1,2),(3,4.5)]"
        );
        assert_eq!(text(&Type::BOX, &coords(&[3., 4., 1., 2.])), "(3,4),(1,2)");
        assert_eq!(text(&Type::LINE, &coords(&[0., -1., 5.])), "{0,-1,5}");
        assert_eq!(
            text(&Type::CIRCLE, &coords(&[1., 2., 3.])),
            "<(1,2),3>"
        );

        let mut polygon = 2i32.to_be_bytes().to_vec();
        polygon.extend(coords(&[0., 0., 1., 1.]));
        assert_eq!(text(&Type::POLYGON, &polygon), "((0,0),(1,1))");

        // A path carries the flag that says whether it is closed, and that is
        // the only thing the brackets are reporting.
        let mut open = vec![0u8];
        open.extend(polygon.clone());
        assert_eq!(text(&Type::PATH, &open), "[(0,0),(1,1)]");
        let mut closed = vec![1u8];
        closed.extend(polygon);
        assert_eq!(text(&Type::PATH, &closed), "((0,0),(1,1))");
    }

    #[test]
    fn a_truncated_point_is_no_decode_rather_than_a_panic() {
        let mut out = String::new();
        assert!(write_text(&Type::POINT, &coords(&[1.])[..4], &mut out).is_none());
    }

    #[test]
    fn numerics_keep_every_digit_and_their_scale() {
        // 1234.5678 → digits [1234, 5678], weight 0, scale 4
        assert_eq!(
            text(&Type::NUMERIC, &numeric(0, 0, 4, &[1234, 5678])),
            "1234.5678"
        );
        // 1.10 → the declared scale keeps the trailing zero.
        assert_eq!(text(&Type::NUMERIC, &numeric(0, 0, 2, &[1, 1000])), "1.10");
        // -0.5
        assert_eq!(
            text(&Type::NUMERIC, &numeric(-1, 0x4000, 1, &[5000])),
            "-0.5"
        );
        // A whole number with no scale at all.
        assert_eq!(
            text(&Type::NUMERIC, &numeric(1, 0, 0, &[12, 3456])),
            "123456"
        );
        assert_eq!(text(&Type::NUMERIC, &numeric(0, 0xC000, 0, &[])), "NaN");
    }

    #[test]
    fn timestamps_render_the_way_postgres_prints_them() {
        assert_eq!(
            text(&Type::TIMESTAMP, &0i64.to_be_bytes()),
            "2000-01-01 00:00:00"
        );
        // 2026-08-19 04:12:00.123456 UTC
        let micros = 840_427_920_123_456i64;
        assert_eq!(
            text(&Type::TIMESTAMP, &micros.to_be_bytes()),
            "2026-08-19 04:12:00.123456"
        );
        assert_eq!(
            text(&Type::TIMESTAMPTZ, &micros.to_be_bytes()),
            "2026-08-19 04:12:00.123456+00"
        );
        // Before the epoch: floor division has to carry the day back.
        assert_eq!(
            text(&Type::TIMESTAMP, &(-1i64).to_be_bytes()),
            "1999-12-31 23:59:59.999999"
        );
        assert_eq!(text(&Type::TIMESTAMP, &i64::MAX.to_be_bytes()), "infinity");
    }

    #[test]
    fn dates_and_times() {
        assert_eq!(text(&Type::DATE, &0i32.to_be_bytes()), "2000-01-01");
        assert_eq!(text(&Type::DATE, &(-1i32).to_be_bytes()), "1999-12-31");
        assert_eq!(
            text(&Type::TIME, &(3_661_000_000i64).to_be_bytes()),
            "01:01:01"
        );
        assert_eq!(text(&Type::TIME, &(500_000i64).to_be_bytes()), "00:00:00.5");
    }

    #[test]
    fn intervals_keep_months_days_and_time_apart() {
        let mut raw = Vec::new();
        raw.extend(3_600_000_000i64.to_be_bytes()); // 1 hour
        raw.extend(2i32.to_be_bytes()); // 2 days
        raw.extend(14i32.to_be_bytes()); // 1 year 2 mons
        assert_eq!(text(&Type::INTERVAL, &raw), "1 year 2 mons 2 days 01:00:00");

        let mut zero = Vec::new();
        zero.extend(0i64.to_be_bytes());
        zero.extend(0i32.to_be_bytes());
        zero.extend(0i32.to_be_bytes());
        assert_eq!(text(&Type::INTERVAL, &zero), "00:00:00");
    }

    #[test]
    fn uuids_get_their_hyphens_back() {
        let bytes: Vec<u8> = (0..16).collect();
        assert_eq!(
            text(&Type::UUID, &bytes),
            "00010203-0405-0607-0809-0a0b0c0d0e0f"
        );
    }

    #[test]
    fn an_undecodable_value_becomes_hex_rather_than_an_error() {
        let mut out = String::new();
        // A `point`, which has no decoder here.
        let decoded = decode(&Type::POINT, &[1, 2, 3], &mut out);
        assert!(matches!(decoded, Decoded::Text));
        assert_eq!(out, "010203");
    }

    #[test]
    fn kinds_follow_the_type_not_the_oid() {
        assert_eq!(kind_for(&Type::INT8), ValueKind::Int);
        assert_eq!(kind_for(&Type::NUMERIC), ValueKind::Decimal);
        assert_eq!(kind_for(&Type::TIMESTAMPTZ), ValueKind::Timestamp);
        assert_eq!(kind_for(&Type::TEXT_ARRAY), ValueKind::Array);
        // A domain over an integer is still a number, and still right-aligns.
        let domain = Type::new(
            "positive_int".into(),
            16_400,
            Kind::Domain(Type::INT4),
            "public".into(),
        );
        assert_eq!(kind_for(&domain), ValueKind::Int);
    }

    #[test]
    fn catalog_kinds_fall_back_through_the_base_type_then_the_category() {
        // A builtin still answers from its OID.
        assert_eq!(kind_for_catalog(20, 0, "b", "N"), ValueKind::Int);
        // A domain over int4.
        assert_eq!(kind_for_catalog(16_400, 23, "d", "N"), ValueKind::Int);
        // An enum nobody has a decoder for.
        assert_eq!(kind_for_catalog(16_401, 0, "e", "E"), ValueKind::Text);
        // An extension type: all the catalog says is that it is a string.
        assert_eq!(kind_for_catalog(16_402, 0, "b", "S"), ValueKind::Text);
    }

    #[test]
    fn an_enum_arrives_as_its_label() {
        let ty = Type::new(
            "plan".into(),
            16_401,
            Kind::Enum(vec!["free".into(), "team".into()]),
            "public".into(),
        );
        assert_eq!(text(&ty, b"team"), "team");
        assert_eq!(kind_for(&ty), ValueKind::Text);
    }

    #[test]
    fn arrays_render_their_elements_with_the_same_code() {
        // One dimension, no nulls, int4 elements, 2 values.
        let mut raw = Vec::new();
        raw.extend(1i32.to_be_bytes());
        raw.extend(0i32.to_be_bytes());
        raw.extend(23u32.to_be_bytes()); // int4 oid
        raw.extend(2i32.to_be_bytes()); // length
        raw.extend(1i32.to_be_bytes()); // lower bound
        for value in [7i32, 8] {
            raw.extend(4i32.to_be_bytes());
            raw.extend(value.to_be_bytes());
        }
        assert_eq!(text(&Type::INT4_ARRAY, &raw), "{7,8}");
    }

    #[test]
    fn array_elements_that_would_be_ambiguous_are_quoted() {
        let mut raw = Vec::new();
        raw.extend(1i32.to_be_bytes());
        raw.extend(1i32.to_be_bytes()); // has nulls
        raw.extend(25u32.to_be_bytes()); // text oid
        raw.extend(3i32.to_be_bytes());
        raw.extend(1i32.to_be_bytes());
        for value in ["a,b", "plain"] {
            raw.extend((value.len() as i32).to_be_bytes());
            raw.extend(value.as_bytes());
        }
        raw.extend((-1i32).to_be_bytes()); // NULL
        assert_eq!(text(&Type::TEXT_ARRAY, &raw), "{\"a,b\",plain,NULL}");
    }
}
