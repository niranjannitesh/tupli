//! Making sense of bytes that arrived with no type attached.
//!
//! A Redis value is a byte string and nothing more. What is actually in it —
//! JSON, MessagePack, a gzipped blob of either, a PHP session, a JPEG — is a
//! convention between whoever wrote it and whoever reads it, and the server
//! does not know or care. A client that assumes UTF-8 shows people mojibake and
//! then corrupts the key when they save it back, so nothing here converts to
//! text without first deciding what the bytes are.
//!
//! Two halves, kept apart on purpose. [`sniff`] guesses, and is allowed to be
//! wrong; [`decode`] does exactly what it is told, and is the only thing the UI
//! runs once a person has picked a chain by hand. A guess that is wrong should
//! cost one dropdown change, never a re-fetch.
//!
//! Chains, not single decoders, because the real world nests: a gzipped
//! MessagePack document is two steps, and each step is worth naming separately
//! so the UI can show `gzip → MessagePack` and let either half be overridden.

use std::io::Read as _;

use base64::Engine as _;

/// One step of a chain.
///
/// The byte→byte steps ([`Decoder::Gzip`] through [`Decoder::Base64`]) can be
/// followed by more steps; the byte→text ones end a chain, because there is
/// nothing left to hand on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decoder {
    /// The bytes are text. Fails rather than substituting replacement
    /// characters: a value that is not UTF-8 is a fact worth showing.
    Utf8,
    /// Parse and re-print with indentation.
    Json,
    /// MessagePack, rendered as JSON so one viewer serves both.
    MsgPack,
    /// PHP's `serialize()`, rendered as JSON for the same reason. Common in
    /// Redis because it is what Laravel and Symfony put in a cache entry.
    PhpSerialized,
    /// Offsets, bytes, and an ASCII gutter. Never fails, which is what makes it
    /// the honest fallback for anything else.
    Hex,
    Gzip,
    /// zlib — a deflate stream with a two-byte header and a checksum.
    Zlib,
    /// Raw deflate, with neither.
    Deflate,
    Base64,
}

impl Decoder {
    /// Whether this step produces bytes for the next one, rather than text.
    pub fn is_transform(self) -> bool {
        matches!(self, Self::Gzip | Self::Zlib | Self::Deflate | Self::Base64)
    }

    /// The name to put in a dropdown.
    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "Text",
            Self::Json => "JSON",
            Self::MsgPack => "MessagePack",
            Self::PhpSerialized => "PHP serialize",
            Self::Hex => "Hex",
            Self::Gzip => "gzip",
            Self::Zlib => "zlib",
            Self::Deflate => "deflate",
            Self::Base64 => "base64",
        }
    }

    /// Every decoder, in the order a menu should list them: the views first,
    /// then the unwrappings.
    pub const ALL: [Decoder; 9] = [
        Self::Utf8,
        Self::Json,
        Self::MsgPack,
        Self::PhpSerialized,
        Self::Hex,
        Self::Gzip,
        Self::Zlib,
        Self::Deflate,
        Self::Base64,
    ];
}

/// What kind of thing the decoded text is, so the viewer can choose a font, a
/// highlighter, and whether folding makes sense.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Form {
    Text,
    Json,
    Hex,
}

/// The result of running a chain.
#[derive(Clone, Debug)]
pub struct Decoded {
    /// What to show.
    pub text: String,
    /// The bytes as they stood after the last byte→byte step — what a hex
    /// viewer should show and what a save would write back through the same
    /// chain in reverse. For a chain of pure views this is the input unchanged.
    pub bytes: Vec<u8>,
    pub form: Form,
    /// The chain that actually ran. Equal to the chain passed in unless it was
    /// empty, in which case this says what the fallback chose.
    pub applied: Vec<Decoder>,
}

/// Why a chain did not run. Carries which step failed, because "not valid
/// JSON" is useless when the chain was `gzip → JSON` and it was the gzip.
#[derive(Clone, Debug)]
pub struct DecodeError {
    pub decoder: Decoder,
    pub message: String,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.decoder.label(), self.message)
    }
}

impl std::error::Error for DecodeError {}

fn fail(decoder: Decoder, message: impl Into<String>) -> DecodeError {
    DecodeError {
        decoder,
        message: message.into(),
    }
}

/// Above this many bytes, a decompressor is refused rather than run.
///
/// A gzip bomb is forty bytes on the wire and a gigabyte in memory, and the
/// value that arrives here came off a server this client does not control.
const MAX_INFLATED: u64 = 64 * 1024 * 1024;

/// Run a chain over some bytes.
///
/// An empty chain means "decide for me": [`sniff`] picks one, and if it has no
/// opinion the bytes are shown as text when they are text and as hex when they
/// are not. That fallback is why every call site can pass the user's chain
/// straight through without a special case for "they have not chosen yet".
pub fn decode(bytes: &[u8], chain: &[Decoder]) -> Result<Decoded, DecodeError> {
    let owned;
    let chain = match chain.is_empty() {
        false => chain,
        true => {
            owned = default_chain(bytes);
            &owned
        }
    };

    let mut current = bytes.to_vec();
    for (index, decoder) in chain.iter().enumerate() {
        let last = index + 1 == chain.len();
        match decoder {
            Decoder::Gzip => current = inflate(&current, Wrapper::Gzip)?,
            Decoder::Zlib => current = inflate(&current, Wrapper::Zlib)?,
            Decoder::Deflate => current = inflate(&current, Wrapper::Raw)?,
            Decoder::Base64 => {
                current = base64::engine::general_purpose::STANDARD
                    .decode(trim_ascii_whitespace(&current))
                    .map_err(|e| fail(Decoder::Base64, e.to_string()))?
            }
            // A view. Anything after it would have no bytes to work on, so a
            // chain that puts one in the middle is a caller bug worth naming.
            view => {
                if !last {
                    return Err(fail(*view, "nothing can follow a view in a chain"));
                }
                let (text, form) = render(&current, *view)?;
                return Ok(Decoded {
                    text,
                    bytes: current,
                    form,
                    applied: chain.to_vec(),
                });
            }
        }
    }

    // The chain was all unwrapping and said nothing about how to show the
    // result, so the same fallback applies to what came out.
    let view = match std::str::from_utf8(&current).is_ok() {
        true => Decoder::Utf8,
        false => Decoder::Hex,
    };
    let (text, form) = render(&current, view)?;
    let mut applied = chain.to_vec();
    applied.push(view);
    Ok(Decoded {
        text,
        bytes: current,
        form,
        applied,
    })
}

/// Turn bytes into text with one of the viewing decoders.
fn render(bytes: &[u8], view: Decoder) -> Result<(String, Form), DecodeError> {
    match view {
        Decoder::Utf8 => match std::str::from_utf8(bytes) {
            Ok(text) => Ok((text.to_owned(), Form::Text)),
            Err(error) => Err(fail(Decoder::Utf8, format!("not UTF-8: {error}"))),
        },
        Decoder::Json => {
            let value: serde_json::Value = serde_json::from_slice(bytes)
                .map_err(|e| fail(Decoder::Json, e.to_string()))?;
            Ok((pretty_json(&value), Form::Json))
        }
        Decoder::MsgPack => {
            let value = read_msgpack(bytes)?;
            Ok((pretty_json(&value), Form::Json))
        }
        Decoder::PhpSerialized => {
            let value = crate::php::parse(bytes)
                .map_err(|message| fail(Decoder::PhpSerialized, message))?;
            Ok((pretty_json(&value), Form::Json))
        }
        Decoder::Hex => Ok((hex_dump(bytes), Form::Hex)),
        transform => Err(fail(transform, "is not a view")),
    }
}

/// `serde_json::to_string_pretty` never fails on a value that came out of a
/// parser, so the error case is folded away rather than propagated to every
/// caller as an impossible branch.
fn pretty_json(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

enum Wrapper {
    Gzip,
    Zlib,
    Raw,
}

fn inflate(bytes: &[u8], wrapper: Wrapper) -> Result<Vec<u8>, DecodeError> {
    let decoder = match wrapper {
        Wrapper::Gzip => Decoder::Gzip,
        Wrapper::Zlib => Decoder::Zlib,
        Wrapper::Raw => Decoder::Deflate,
    };
    let mut out = Vec::new();
    let result = match wrapper {
        Wrapper::Gzip => flate2::read::GzDecoder::new(bytes)
            .take(MAX_INFLATED)
            .read_to_end(&mut out),
        Wrapper::Zlib => flate2::read::ZlibDecoder::new(bytes)
            .take(MAX_INFLATED)
            .read_to_end(&mut out),
        Wrapper::Raw => flate2::read::DeflateDecoder::new(bytes)
            .take(MAX_INFLATED)
            .read_to_end(&mut out),
    };
    result.map_err(|e| fail(decoder, e.to_string()))?;
    if out.len() as u64 >= MAX_INFLATED {
        return Err(fail(decoder, "expands to more than 64 MB"));
    }
    Ok(out)
}

/// MessagePack → the JSON model, so one viewer serves both.
///
/// The two formats do not line up exactly and the mismatches are handled the
/// way a person reading the value would want rather than the way a round trip
/// would: a binary blob becomes a base64 string, a non-string map key becomes
/// its own text form, and an extension type becomes an object naming its tag.
/// None of that survives being written back, which is why writing back goes
/// through the original bytes and not through this.
fn read_msgpack(bytes: &[u8]) -> Result<serde_json::Value, DecodeError> {
    let mut cursor = bytes;
    let value = rmpv::decode::read_value(&mut cursor)
        .map_err(|e| fail(Decoder::MsgPack, e.to_string()))?;
    if !cursor.is_empty() {
        return Err(fail(
            Decoder::MsgPack,
            format!("{} trailing bytes after the document", cursor.len()),
        ));
    }
    Ok(msgpack_to_json(&value))
}

fn msgpack_to_json(value: &rmpv::Value) -> serde_json::Value {
    use serde_json::Value as J;
    match value {
        rmpv::Value::Nil => J::Null,
        rmpv::Value::Boolean(b) => J::Bool(*b),
        rmpv::Value::Integer(i) => match (i.as_i64(), i.as_u64()) {
            (Some(i), _) => J::from(i),
            (_, Some(u)) => J::from(u),
            // A u64 too large for an i64 and somehow not a u64 either. Text
            // rather than a lossy float.
            _ => J::String(i.to_string()),
        },
        rmpv::Value::F32(x) => json_number(*x as f64),
        rmpv::Value::F64(x) => json_number(*x),
        rmpv::Value::String(s) => match s.as_str() {
            Some(s) => J::String(s.to_owned()),
            // MessagePack strings are only *supposed* to be UTF-8.
            None => J::String(String::from_utf8_lossy(s.as_bytes()).into_owned()),
        },
        rmpv::Value::Binary(b) => J::String(base64::engine::general_purpose::STANDARD.encode(b)),
        rmpv::Value::Array(items) => J::Array(items.iter().map(msgpack_to_json).collect()),
        rmpv::Value::Map(pairs) => J::Object(
            pairs
                .iter()
                .map(|(key, value)| (msgpack_key(key), msgpack_to_json(value)))
                .collect(),
        ),
        rmpv::Value::Ext(tag, data) => J::Object(
            [
                ("type".to_owned(), J::from(*tag)),
                (
                    "data".to_owned(),
                    J::String(base64::engine::general_purpose::STANDARD.encode(data)),
                ),
            ]
            .into_iter()
            .collect(),
        ),
    }
}

/// JSON object keys are strings and MessagePack map keys are anything, so a
/// numeric key becomes the text of the number — which is what it would have
/// been had the same document been written as JSON in the first place.
fn msgpack_key(value: &rmpv::Value) -> String {
    match value {
        rmpv::Value::String(s) => match s.as_str() {
            Some(s) => s.to_owned(),
            None => String::from_utf8_lossy(s.as_bytes()).into_owned(),
        },
        other => match msgpack_to_json(other) {
            serde_json::Value::String(s) => s,
            json => json.to_string(),
        },
    }
}

/// `NaN` and the infinities have no JSON spelling, so they become the text
/// everybody writes them as rather than becoming null.
fn json_number(x: f64) -> serde_json::Value {
    match serde_json::Number::from_f64(x) {
        Some(number) => serde_json::Value::Number(number),
        None => serde_json::Value::String(x.to_string()),
    }
}

/// Bytes per line of a hex dump. Sixteen, because that is what every hex viewer
/// since `od` has used and because the ASCII gutter then lines up with a
/// terminal.
const HEX_WIDTH: usize = 16;

/// `00000000  7b 22 61 22 …  |{"a": 1}|`
pub fn hex_dump(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    // Three characters per byte, plus the offset and the gutter, plus slack.
    let mut out = String::with_capacity(bytes.len() * 4 + 32);
    for (index, line) in bytes.chunks(HEX_WIDTH).enumerate() {
        let _ = write!(out, "{:08x}  ", index * HEX_WIDTH);
        for column in 0..HEX_WIDTH {
            match line.get(column) {
                Some(byte) => {
                    let _ = write!(out, "{byte:02x} ");
                }
                None => out.push_str("   "),
            }
            // A gap down the middle, so the eye can count to eight instead of
            // to sixteen.
            if column == HEX_WIDTH / 2 - 1 {
                out.push(' ');
            }
        }
        out.push_str(" |");
        for byte in line {
            out.push(match byte {
                0x20..=0x7e => *byte as char,
                _ => '.',
            });
        }
        out.push_str("|\n");
    }
    out
}

/// What to run when nobody has said. [`sniff`]'s answer if it has one, and
/// otherwise text-if-it-is-text, hex if it is not.
fn default_chain(bytes: &[u8]) -> Vec<Decoder> {
    let sniffed = sniff(bytes);
    if !sniffed.is_empty() {
        return sniffed;
    }
    match std::str::from_utf8(bytes).is_ok() {
        true => vec![Decoder::Utf8],
        false => vec![Decoder::Hex],
    }
}

/// Guess what these bytes are, as a chain ready to hand to [`decode`].
///
/// An empty answer means "no opinion", not "plain text" — the caller decides
/// what to do with a value it cannot recognise, and for the grid that is
/// usually text-or-hex rather than an error.
///
/// The rules are deliberately conservative, because a wrong guess is worse than
/// no guess: it makes the user distrust every other value on the screen. So a
/// container is required for MessagePack and JSON (a bare `1` is valid in both
/// and is not evidence of anything), the whole buffer must be consumed, and
/// base64 is only claimed when what it wraps is itself recognisable — otherwise
/// every lowercase word of the right length would be "base64".
pub fn sniff(bytes: &[u8]) -> Vec<Decoder> {
    sniff_depth(bytes, 0)
}

/// Nested wrappers are real (base64 of gzip of JSON) and a hostile value could
/// nest forever, so the recursion is bounded.
const MAX_NESTING: usize = 4;

fn sniff_depth(bytes: &[u8], depth: usize) -> Vec<Decoder> {
    if bytes.is_empty() || depth >= MAX_NESTING {
        return Vec::new();
    }

    // The compressed formats first: their magic numbers are the only evidence
    // in this function that is not statistical.
    for (looks, decoder) in [
        (is_gzip(bytes), Decoder::Gzip),
        (is_zlib(bytes), Decoder::Zlib),
    ] {
        if !looks {
            continue;
        }
        let wrapper = match decoder {
            Decoder::Gzip => Wrapper::Gzip,
            _ => Wrapper::Zlib,
        };
        if let Ok(inner) = inflate(bytes, wrapper) {
            let mut chain = vec![decoder];
            match sniff_depth(&inner, depth + 1) {
                // Nothing recognisable inside, so the chain says how to show
                // what came out rather than stopping at "it was compressed".
                inner_chain if inner_chain.is_empty() => {
                    chain.push(match std::str::from_utf8(&inner).is_ok() {
                        true => Decoder::Utf8,
                        false => Decoder::Hex,
                    })
                }
                inner_chain => chain.extend(inner_chain),
            }
            return chain;
        }
    }

    if is_msgpack_document(bytes) {
        return vec![Decoder::MsgPack];
    }
    if is_json_document(bytes) {
        return vec![Decoder::Json];
    }
    if is_php_document(bytes) {
        return vec![Decoder::PhpSerialized];
    }

    // Base64 last, and only when it wraps something recognisable. `dGVzdGluZw`
    // is a real base64 string and also a plausible cache key; without this rule
    // the grid would start "decoding" identifiers.
    if looks_like_base64(bytes) {
        if let Ok(inner) = base64::engine::general_purpose::STANDARD
            .decode(trim_ascii_whitespace(bytes))
        {
            let inner = sniff_depth(&inner, depth + 1);
            if !inner.is_empty() {
                let mut chain = vec![Decoder::Base64];
                chain.extend(inner);
                return chain;
            }
        }
    }

    Vec::new()
}

fn is_gzip(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1f, 0x8b])
}

/// zlib has no magic number, only a header whose two bytes are constrained:
/// the low nibble of the first is the compression method (8 for deflate) and
/// the pair is a multiple of 31.
fn is_zlib(bytes: &[u8]) -> bool {
    match bytes {
        [cmf, flg, ..] => {
            cmf & 0x0f == 8 && (u16::from(*cmf) * 256 + u16::from(*flg)) % 31 == 0
        }
        _ => false,
    }
}

/// A MessagePack *document*, meaning a map or an array that accounts for every
/// byte in the buffer. Anything looser matches almost any binary blob: `0x01`
/// alone is a valid MessagePack integer, and so is the first byte of a PNG.
fn is_msgpack_document(bytes: &[u8]) -> bool {
    let container = matches!(
        bytes.first(),
        Some(0x80..=0x9f) | Some(0xdc..=0xdf)
    );
    if !container {
        return false;
    }
    let mut cursor = bytes;
    match rmpv::decode::read_value(&mut cursor) {
        Ok(value) => {
            cursor.is_empty() && matches!(value, rmpv::Value::Map(_) | rmpv::Value::Array(_))
        }
        Err(_) => false,
    }
}

/// Likewise for JSON: an object or an array, parsed whole. A bare `"hello"` or
/// `42` is valid JSON and is much more likely to be a plain value.
fn is_json_document(bytes: &[u8]) -> bool {
    let first = bytes.iter().find(|b| !b.is_ascii_whitespace());
    if !matches!(first, Some(b'{') | Some(b'[')) {
        return false;
    }
    serde_json::from_slice::<serde_json::Value>(bytes).is_ok()
}

/// PHP's `serialize()` starts with a one-letter type tag and a colon, and the
/// interesting ones are the containers.
fn is_php_document(bytes: &[u8]) -> bool {
    let looks = bytes.starts_with(b"a:") || bytes.starts_with(b"O:");
    looks && crate::php::parse(bytes).is_ok()
}

/// Whether every byte could be part of a base64 string of the right length.
/// Necessary, nowhere near sufficient — see the caller.
fn looks_like_base64(bytes: &[u8]) -> bool {
    let trimmed = trim_ascii_whitespace(bytes);
    // Four characters carry three bytes, so anything shorter than a dozen is
    // too small for its content to be recognisable anyway.
    if trimmed.len() < 12 || !trimmed.len().is_multiple_of(4) {
        return false;
    }
    let body = trimmed.trim_ascii_end_matches_padding();
    !body.is_empty()
        && body
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
}

/// `[u8]::trim_ascii` exists but not a padding-aware one, and base64's `=` is
/// only ever a suffix.
trait TrimPadding {
    fn trim_ascii_end_matches_padding(&self) -> &Self;
}

impl TrimPadding for [u8] {
    fn trim_ascii_end_matches_padding(&self) -> &Self {
        let mut end = self.len();
        while end > 0 && self[end - 1] == b'=' {
            end -= 1;
        }
        &self[..end]
    }
}

fn trim_ascii_whitespace(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map(|i| i + 1)
        .unwrap_or(start);
    &bytes[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn zlib(bytes: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn msgpack(json: &str) -> Vec<u8> {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        let value = to_rmpv(&value);
        let mut out = Vec::new();
        rmpv::encode::write_value(&mut out, &value).unwrap();
        out
    }

    fn to_rmpv(value: &serde_json::Value) -> rmpv::Value {
        match value {
            serde_json::Value::Null => rmpv::Value::Nil,
            serde_json::Value::Bool(b) => rmpv::Value::Boolean(*b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => rmpv::Value::Integer(i.into()),
                None => rmpv::Value::F64(n.as_f64().unwrap()),
            },
            serde_json::Value::String(s) => rmpv::Value::String(s.as_str().into()),
            serde_json::Value::Array(items) => {
                rmpv::Value::Array(items.iter().map(to_rmpv).collect())
            }
            serde_json::Value::Object(fields) => rmpv::Value::Map(
                fields
                    .iter()
                    .map(|(k, v)| (rmpv::Value::String(k.as_str().into()), to_rmpv(v)))
                    .collect(),
            ),
        }
    }

    #[test]
    fn plain_text_is_left_alone() {
        let decoded = decode(b"hello", &[]).unwrap();
        assert_eq!(decoded.text, "hello");
        assert_eq!(decoded.form, Form::Text);
        assert_eq!(decoded.applied, vec![Decoder::Utf8]);
        // And it is not mistaken for anything.
        assert!(sniff(b"hello").is_empty());
    }

    #[test]
    fn bytes_that_are_not_text_fall_back_to_hex_rather_than_to_mojibake() {
        let decoded = decode(&[0xff, 0xfe, 0x00, 0x41], &[]).unwrap();
        assert_eq!(decoded.form, Form::Hex);
        assert!(decoded.text.starts_with("00000000  ff fe 00 41"));
        assert!(decoded.text.contains("|...A|"));
        // Asking for text explicitly is an error, not a lossy conversion.
        assert!(decode(&[0xff], &[Decoder::Utf8]).is_err());
    }

    #[test]
    fn a_json_document_is_recognised_and_reprinted() {
        let raw = br#"{"b":1,"a":[2,3]}"#;
        assert_eq!(sniff(raw), vec![Decoder::Json]);
        let decoded = decode(raw, &[]).unwrap();
        assert_eq!(decoded.form, Form::Json);
        assert!(decoded.text.contains("\n  \"b\": 1"), "{}", decoded.text);
    }

    #[test]
    fn a_bare_json_scalar_is_not_evidence_of_json() {
        // Valid JSON, all three of them, and all three far likelier to be a
        // counter or a name than a document.
        for raw in [&b"42"[..], b"\"hello\"", b"true"] {
            assert!(sniff(raw).is_empty(), "{:?}", String::from_utf8_lossy(raw));
        }
    }

    #[test]
    fn messagepack_is_recognised_only_when_it_accounts_for_every_byte() {
        let packed = msgpack(r#"{"id":7,"tags":["a","b"]}"#);
        assert_eq!(sniff(&packed), vec![Decoder::MsgPack]);
        let decoded = decode(&packed, &[]).unwrap();
        assert_eq!(decoded.form, Form::Json);
        assert!(decoded.text.contains("\"id\": 7"), "{}", decoded.text);
        assert!(decoded.text.contains("\"tags\""), "{}", decoded.text);

        // One byte of something else on the end and it is no longer a document.
        let mut trailing = packed.clone();
        trailing.push(0x00);
        assert!(sniff(&trailing).is_empty());
        assert!(decode(&trailing, &[Decoder::MsgPack]).is_err());
    }

    #[test]
    fn a_single_messagepack_integer_is_not_a_document() {
        // `0x01` is a valid MessagePack positive fixint, and also the first
        // byte of a great many binary formats.
        assert!(sniff(&[0x01]).is_empty());
        assert!(sniff(&[0xc3]).is_empty());
    }

    #[test]
    fn messagepack_binary_and_extension_types_survive_as_something_readable() {
        let mut out = Vec::new();
        let value = rmpv::Value::Map(vec![
            (
                rmpv::Value::String("blob".into()),
                rmpv::Value::Binary(vec![0xde, 0xad]),
            ),
            (
                rmpv::Value::Integer(7.into()),
                rmpv::Value::Ext(3, vec![0x01]),
            ),
        ]);
        rmpv::encode::write_value(&mut out, &value).unwrap();
        let decoded = decode(&out, &[Decoder::MsgPack]).unwrap();
        // Base64 of `de ad`, and a numeric key that became its own text.
        assert!(decoded.text.contains("3q0="), "{}", decoded.text);
        assert!(decoded.text.contains("\"7\""), "{}", decoded.text);
        assert!(decoded.text.contains("\"type\": 3"), "{}", decoded.text);
    }

    #[test]
    fn a_gzipped_json_document_is_two_steps() {
        let packed = gzip(br#"{"ok":true}"#);
        assert_eq!(sniff(&packed), vec![Decoder::Gzip, Decoder::Json]);
        let decoded = decode(&packed, &[]).unwrap();
        assert_eq!(decoded.form, Form::Json);
        assert!(decoded.text.contains("\"ok\": true"));
        // `bytes` is what came out of the gzip, not what went in.
        assert_eq!(decoded.bytes, br#"{"ok":true}"#);
    }

    #[test]
    fn a_gzipped_messagepack_document_is_the_chain_the_user_would_have_picked() {
        let packed = gzip(&msgpack(r#"{"a":1}"#));
        assert_eq!(sniff(&packed), vec![Decoder::Gzip, Decoder::MsgPack]);
        assert!(decode(&packed, &[]).unwrap().text.contains("\"a\": 1"));
    }

    #[test]
    fn zlib_is_told_apart_from_gzip_by_its_header() {
        let packed = zlib(br#"["x"]"#);
        assert_eq!(sniff(&packed), vec![Decoder::Zlib, Decoder::Json]);
        assert!(decode(&packed, &[]).unwrap().text.contains("\"x\""));
    }

    #[test]
    fn compressed_text_that_is_not_structured_still_gets_unwrapped() {
        let packed = gzip(b"just some words");
        assert_eq!(sniff(&packed), vec![Decoder::Gzip, Decoder::Utf8]);
        assert_eq!(decode(&packed, &[]).unwrap().text, "just some words");
    }

    #[test]
    fn base64_is_claimed_only_when_what_it_wraps_is_recognisable() {
        let inner = br#"{"wrapped":true}"#;
        let wrapped = base64::engine::general_purpose::STANDARD.encode(inner);
        assert_eq!(
            sniff(wrapped.as_bytes()),
            vec![Decoder::Base64, Decoder::Json]
        );
        // A base64-shaped identifier is not decoded, because there is nothing
        // inside it to confirm the guess.
        assert!(sniff(b"YWJjZGVmZ2hpamts").is_empty());
    }

    #[test]
    fn a_chain_the_caller_names_is_run_exactly_as_given() {
        let packed = gzip(b"plain");
        // No sniffing: this is what was asked for.
        let decoded = decode(&packed, &[Decoder::Gzip, Decoder::Hex]).unwrap();
        assert_eq!(decoded.form, Form::Hex);
        assert!(decoded.text.contains("|plain|"));
        // And a chain that puts a view in the middle is refused rather than
        // silently truncated.
        let error = decode(&packed, &[Decoder::Hex, Decoder::Gzip]).unwrap_err();
        assert_eq!(error.decoder, Decoder::Hex);
    }

    #[test]
    fn a_failed_step_says_which_step_failed() {
        let error = decode(b"not gzip at all", &[Decoder::Gzip]).unwrap_err();
        assert_eq!(error.decoder, Decoder::Gzip);
        let error = decode(br#"{"a":"#, &[Decoder::Json]).unwrap_err();
        assert_eq!(error.decoder, Decoder::Json);
        assert!(error.to_string().starts_with("JSON: "));
    }

    #[test]
    fn a_chain_of_pure_unwrapping_still_ends_in_a_view() {
        let packed = gzip(b"text");
        let decoded = decode(&packed, &[Decoder::Gzip]).unwrap();
        assert_eq!(decoded.applied, vec![Decoder::Gzip, Decoder::Utf8]);
    }

    #[test]
    fn the_hex_dump_is_sixteen_bytes_to_a_line_with_an_ascii_gutter() {
        let dump = hex_dump(b"0123456789abcdefX");
        let lines: Vec<_> = dump.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("00000000  30 31"));
        assert!(lines[0].ends_with("|0123456789abcdef|"));
        assert!(lines[1].starts_with("00000010  58 "));
        assert!(lines[1].ends_with("|X|"));
        assert_eq!(hex_dump(b""), "");
    }

    #[test]
    fn nesting_stops_before_a_hostile_value_can_recurse_forever() {
        // Six layers of gzip, which a bounded sniffer must not follow to the
        // bottom. It unwraps as far as it is allowed and then calls the rest
        // binary, which is the answer that costs one dropdown to correct.
        let mut packed = msgpack(r#"{"a":1}"#);
        for _ in 0..6 {
            packed = gzip(&packed);
        }
        let chain = sniff(&packed);
        let unwrapped = chain.iter().filter(|d| **d == Decoder::Gzip).count();
        assert!((1..=MAX_NESTING).contains(&unwrapped), "{chain:?}");
        assert_eq!(chain.last(), Some(&Decoder::Hex), "{chain:?}");
    }

    #[test]
    fn a_view_ends_a_chain_and_a_transform_does_not() {
        let views = [
            Decoder::Utf8,
            Decoder::Json,
            Decoder::MsgPack,
            Decoder::PhpSerialized,
            Decoder::Hex,
        ];
        for decoder in Decoder::ALL {
            assert_eq!(
                views.contains(&decoder),
                !decoder.is_transform(),
                "{decoder:?}"
            );
        }
        // And every view really does refuse to be followed.
        for view in views {
            let error = decode(b"whatever", &[view, Decoder::Gzip]).unwrap_err();
            assert_eq!(error.decoder, view);
        }
    }
}
