//! The primitives every ClickHouse packet is built out of.
//!
//! The native protocol has no framing above the field level: there is no
//! packet length, no type tag on a value, no delimiter. A reader that gets one
//! field's width wrong does not get a parse error, it gets plausible garbage
//! for the rest of the connection. That is why everything here is a named
//! function with a test rather than an inline `read_exact` at each call site —
//! the width of a field is the whole contract.
//!
//! Reads take `&mut (dyn AsyncRead + Unpin + Send)` rather than a generic
//! reader. The block decoder recurses through nested types and needs boxed
//! futures to do it; a generic reader would make each recursion a fresh
//! instantiation and the compiler would never finish. One vtable per field on
//! an already-buffered socket costs nothing worth measuring.
//!
//! Writes go into a `Vec<u8>` and the whole packet is handed to the socket at
//! once. Partly for the syscall, mostly because a packet that is half written
//! when something fails would leave the connection unparseable.

use db::{DbError, DbResult, ErrorClass};
use tokio::io::{AsyncRead, AsyncReadExt};

/// What every read in this module is handed.
pub type Reader<'a> = &'a mut (dyn AsyncRead + Unpin + Send);

/// A read that failed because the connection did, not because the data was
/// wrong — worth separating so the UI can offer to reconnect.
pub fn io_error(what: &str, error: std::io::Error) -> DbError {
    DbError::new(
        ErrorClass::Connection,
        format!("The connection failed while {what}: {error}"),
    )
}

/// A read that failed because what arrived could not be what it claimed to be.
///
/// Almost always a protocol bug on this side rather than a broken server, so
/// it is reported as internal: the server is not going to behave differently
/// if the user tries again.
pub fn malformed(what: impl std::fmt::Display) -> DbError {
    DbError::internal(format!("The server sent something unreadable: {what}"))
}

pub async fn read_u8(reader: Reader<'_>) -> DbResult<u8> {
    reader
        .read_u8()
        .await
        .map_err(|error| io_error("reading a byte", error))
}

pub async fn read_u32(reader: Reader<'_>) -> DbResult<u32> {
    reader
        .read_u32_le()
        .await
        .map_err(|error| io_error("reading a number", error))
}

pub async fn read_i32(reader: Reader<'_>) -> DbResult<i32> {
    reader
        .read_i32_le()
        .await
        .map_err(|error| io_error("reading a number", error))
}

pub async fn read_u64(reader: Reader<'_>) -> DbResult<u64> {
    reader
        .read_u64_le()
        .await
        .map_err(|error| io_error("reading a number", error))
}

pub async fn read_exact(reader: Reader<'_>, len: usize) -> DbResult<Vec<u8>> {
    let mut buffer = vec![0u8; len];
    reader
        .read_exact(&mut buffer)
        .await
        .map_err(|error| io_error("reading a value", error))?;
    Ok(buffer)
}

/// A LEB128 unsigned integer, which is how ClickHouse writes every length,
/// every packet tag, and every count.
///
/// Capped at ten bytes because that is how many a `u64` can need. Without the
/// cap a stream that has gone out of step — every byte with its high bit set —
/// spins here forever instead of failing.
pub async fn read_uvarint(reader: Reader<'_>) -> DbResult<u64> {
    let mut value = 0u64;
    for shift in 0..10 {
        let byte = read_u8(reader).await?;
        value |= u64::from(byte & 0x7f) << (shift * 7);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(malformed("a varint that never ended"))
}

/// A length-prefixed string. ClickHouse's `String` is arbitrary bytes and is
/// routinely used for them, so nothing here insists on UTF-8.
pub async fn read_bytes(reader: Reader<'_>) -> DbResult<Vec<u8>> {
    let len = read_uvarint(reader).await? as usize;
    read_exact(reader, len).await
}

/// The same, for the places the protocol itself defines as text: names, type
/// names, error messages. Invalid UTF-8 there is a broken server rather than a
/// user's binary blob, and is replaced rather than refused — an unreadable
/// character in an error message should not hide the error.
pub async fn read_string(reader: Reader<'_>) -> DbResult<String> {
    let bytes = read_bytes(reader).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn write_uvarint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

pub fn write_string(out: &mut Vec<u8>, value: &str) {
    write_uvarint(out, value.len() as u64);
    out.extend_from_slice(value.as_bytes());
}

pub fn write_u8(out: &mut Vec<u8>, value: u8) {
    out.push(value);
}

pub fn write_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_le_bytes());
}

pub fn write_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn round_trip(value: u64) -> u64 {
        let mut out = Vec::new();
        write_uvarint(&mut out, value);
        let mut slice = out.as_slice();
        read_uvarint(&mut slice).await.unwrap()
    }

    #[tokio::test]
    async fn a_varint_survives_the_round_trip_at_every_width() {
        for value in [0, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            assert_eq!(round_trip(value).await, value, "{value}");
        }
    }

    #[tokio::test]
    async fn a_varint_is_the_shortest_encoding_that_fits() {
        // Not cosmetic: the server reads these back, and a padded varint from
        // a client is a length it will disagree with.
        let mut out = Vec::new();
        write_uvarint(&mut out, 0);
        assert_eq!(out, vec![0]);
        out.clear();
        write_uvarint(&mut out, 54440);
        assert_eq!(out.len(), 3);
    }

    #[tokio::test]
    async fn a_varint_that_never_ends_fails_instead_of_spinning() {
        let runaway = vec![0xffu8; 32];
        let mut slice = runaway.as_slice();
        assert!(read_uvarint(&mut slice).await.is_err());
    }

    #[tokio::test]
    async fn a_string_carries_its_own_length() {
        let mut out = Vec::new();
        write_string(&mut out, "system.tables");
        let mut slice = out.as_slice();
        assert_eq!(read_string(&mut slice).await.unwrap(), "system.tables");
        assert!(slice.is_empty());
    }

    #[tokio::test]
    async fn a_string_may_hold_bytes_that_are_not_text() {
        let blob = [0x89u8, 0x50, 0x4e, 0x47, 0xff, 0x00];
        let mut out = Vec::new();
        write_uvarint(&mut out, blob.len() as u64);
        out.extend_from_slice(&blob);
        let mut slice = out.as_slice();
        assert_eq!(read_bytes(&mut slice).await.unwrap(), blob);
    }
}
