//! Reading a compressed block.
//!
//! When compression is on, ClickHouse wraps the *body* of a `Data` packet —
//! everything after the packet tag and the table name — in frames of its own:
//! a checksum, a method byte, two sizes, and the payload. Decompressing gives
//! back the exact bytes an uncompressed block would have had, which is why the
//! block reader never learns whether compression happened.
//!
//! Whether compression happens is the *client's* call — a byte in the `Query`
//! packet — and this client sends zero, because a browser's bottleneck is the
//! server scanning the table rather than the fifty thousand rows coming back.
//! So this module is the half of the work that can be finished on its own and
//! checked: framing and LZ4. What is missing before the flag can be flipped is
//! a reader that pulls the *next* frame when a block runs off the end of the
//! current one — a block is not one frame, and nothing in a frame says whether
//! the block it belongs to has ended.

// Nothing calls this yet, by the decision above. The tests do, which is the
// point of keeping it compiled rather than commented out.
#![allow(dead_code)]

use db::DbResult;

use crate::wire::{self, malformed, Reader};

/// LZ4 without a separate compression level in the method byte.
const METHOD_LZ4: u8 = 0x82;
const METHOD_ZSTD: u8 = 0x90;
const METHOD_NONE: u8 = 0x02;

/// The checksum, plus the method byte and the two sizes that the sizes count
/// themselves as part of.
const CHECKSUM_LEN: usize = 16;
const HEADER_LEN: usize = 9;

/// A ceiling on what one frame may claim to expand to.
///
/// The sizes arrive before the data, so a frame that says it decompresses to
/// four gigabytes gets a four-gigabyte allocation before anything notices it
/// was lying. ClickHouse's own frames are a megabyte by default; a hundred is
/// far above anything real and far below anything fatal.
const MAX_UNCOMPRESSED: usize = 100 * 1024 * 1024;

/// Read one compressed frame and return the bytes it stands for.
///
/// The 16-byte CityHash128 checksum is read and **not verified**. Verifying it
/// would mean carrying an implementation of a hash that exists nowhere else in
/// this app, to detect corruption on a TCP connection that already has a
/// checksum, under a TLS layer that already has a MAC. The bytes are consumed
/// because the frame is not parseable otherwise; the guarantee is dropped
/// knowingly rather than by accident.
pub async fn read_frame(reader: Reader<'_>) -> DbResult<Vec<u8>> {
    let _checksum = wire::read_exact(reader, CHECKSUM_LEN).await?;
    let method = wire::read_u8(reader).await?;
    let compressed = wire::read_u32(reader).await? as usize;
    let uncompressed = wire::read_u32(reader).await? as usize;

    if compressed < HEADER_LEN {
        return Err(malformed(format!(
            "a compressed frame of {compressed} bytes, which is shorter than its own header"
        )));
    }
    if uncompressed > MAX_UNCOMPRESSED {
        return Err(malformed(format!(
            "a compressed frame claiming to expand to {uncompressed} bytes"
        )));
    }

    let body = wire::read_exact(reader, compressed - HEADER_LEN).await?;
    match method {
        METHOD_LZ4 => {
            let mut out = vec![0u8; uncompressed];
            let written = lz4_flex::block::decompress_into(&body, &mut out).map_err(|error| {
                malformed(format!("a block that would not decompress: {error}"))
            })?;
            // A short read means the frame's own two sizes disagree, and the
            // block reader would then read past the end of what it was given.
            if written != uncompressed {
                return Err(malformed(format!(
                    "a block that decompressed to {written} bytes instead of {uncompressed}"
                )));
            }
            Ok(out)
        }
        METHOD_NONE => Ok(body),
        METHOD_ZSTD => Err(malformed(
            "a zstd-compressed block, which tupli does not read — \
             set network_compression_method to lz4 or none",
        )),
        other => Err(malformed(format!(
            "a block compressed with method {other:#04x}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the frame a server would send, so the reader is tested against
    /// the layout rather than against itself.
    fn frame(method: u8, body: &[u8], uncompressed: usize) -> Vec<u8> {
        let mut out = vec![0u8; CHECKSUM_LEN];
        out.push(method);
        out.extend_from_slice(&((body.len() + HEADER_LEN) as u32).to_le_bytes());
        out.extend_from_slice(&(uncompressed as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    #[tokio::test]
    async fn an_lz4_frame_gives_back_exactly_what_went_into_it() {
        // Long enough and repetitive enough that it really compresses, so the
        // test is not passing on a pathological zero-length case.
        let original: Vec<u8> = "system.tables\0".repeat(64).into_bytes();
        let body = lz4_flex::block::compress(&original);
        assert!(body.len() < original.len(), "the fixture did not compress");
        let wire = frame(METHOD_LZ4, &body, original.len());
        let mut slice = wire.as_slice();
        assert_eq!(read_frame(&mut slice).await.unwrap(), original);
        assert!(slice.is_empty(), "the frame left bytes behind");
    }

    #[tokio::test]
    async fn an_uncompressed_frame_is_still_a_frame() {
        let original = b"plain".to_vec();
        let wire = frame(METHOD_NONE, &original, original.len());
        let mut slice = wire.as_slice();
        assert_eq!(read_frame(&mut slice).await.unwrap(), original);
    }

    #[tokio::test]
    async fn a_frame_that_lies_about_its_size_fails_rather_than_allocating() {
        let wire = frame(METHOD_LZ4, b"nonsense", 1 << 30);
        let mut slice = wire.as_slice();
        let error = read_frame(&mut slice).await.unwrap_err();
        assert!(error.message.contains("expand to"), "{error}");
    }

    #[tokio::test]
    async fn zstd_is_refused_by_name_rather_than_misread() {
        let wire = frame(METHOD_ZSTD, b"\x28\xb5\x2f\xfd", 16);
        let mut slice = wire.as_slice();
        let error = read_frame(&mut slice).await.unwrap_err();
        assert!(error.message.contains("zstd"), "{error}");
    }
}
