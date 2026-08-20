//! Blocks: the unit ClickHouse answers in.
//!
//! A result is a stream of blocks, and a block is columnar — every value of
//! column one, then every value of column two. That is the whole reason this
//! driver exists in the shape it does: the app's own [`db::ResultSet`] is
//! columnar too, so a block goes into a column builder without ever being
//! turned into rows and back.
//!
//! The layout is: a small header of optional fields, a column count, a row
//! count, and then for each column its name, its type as text, and its values.
//! Nothing carries a length. If the type string says `Array(UInt8)` and the
//! reader believes something else, every byte after that is misread and the
//! connection is finished — so a block that cannot be read fully is an error,
//! never a partial result.

use db::DbResult;

use crate::protocol;
use crate::types::{self, Cellv, ChType};
use crate::wire::{self, malformed, Reader};

/// A limit on a row count read off the wire.
///
/// The count arrives before the data and decides how much is allocated, so a
/// desynchronised stream can ask for an arbitrary allocation here. ClickHouse
/// splits results at `max_block_size`, which is 65505 by default; ten million
/// is far past any real block and still recoverable.
const MAX_ROWS_PER_BLOCK: u64 = 10_000_000;

/// The same, for columns. A `select *` on the widest table anyone has is a few
/// thousand.
const MAX_COLUMNS: u64 = 100_000;

pub struct Block {
    pub columns: Vec<BlockColumn>,
    pub rows: usize,
}

pub struct BlockColumn {
    pub name: String,
    /// The type exactly as the server spelled it, which is what the grid header
    /// and the inspector show. Worth keeping separately from the parsed form:
    /// `LowCardinality(Nullable(String))` is information, and rebuilding that
    /// string from a [`ChType`] would only ever approximate it.
    pub type_name: String,
    pub ty: ChType,
    pub values: Vec<Cellv>,
}

impl Block {
    /// Whether this block is only telling us the shape of the answer.
    ///
    /// The server sends an empty block with the right columns before the first
    /// real one, and again at the end. It is how a query that matched nothing
    /// still produces a grid with headers.
    pub fn is_header(&self) -> bool {
        self.rows == 0
    }
}

/// Read a block body — everything after the packet tag.
pub async fn read_block(reader: Reader<'_>, revision: u64) -> DbResult<Block> {
    // The name of the temporary table the block belongs to. Always empty for
    // ordinary results; read regardless, because it is on the wire.
    if revision >= protocol::MIN_REVISION_WITH_TEMPORARY_TABLES {
        let _table = wire::read_string(reader).await?;
    }
    if revision >= protocol::MIN_REVISION_WITH_BLOCK_INFO {
        read_block_info(reader).await?;
    }

    let columns = wire::read_uvarint(reader).await?;
    let rows = wire::read_uvarint(reader).await?;
    if columns > MAX_COLUMNS || rows > MAX_ROWS_PER_BLOCK {
        return Err(malformed(format!(
            "a block of {columns} columns and {rows} rows"
        )));
    }
    let (columns, rows) = (columns as usize, rows as usize);

    let mut read = Vec::with_capacity(columns);
    for _ in 0..columns {
        let name = wire::read_string(reader).await?;
        let type_name = wire::read_string(reader).await?;
        let ty = types::parse(&type_name);
        // A block with no rows carries no bytes for its columns at all — not
        // even the `LowCardinality` version word, which is inside the part the
        // server skips. Reading a prefix here is the difference between a
        // working header block and a connection that never recovers.
        let values = match rows {
            0 => Vec::new(),
            _ => {
                types::read_prefix(reader, &ty).await?;
                types::read_values(reader, &ty, rows).await?
            }
        };
        read.push(BlockColumn {
            name,
            type_name,
            ty,
            values,
        });
    }
    Ok(Block {
        columns: read,
        rows,
    })
}

/// The block header: numbered optional fields, terminated by a zero.
///
/// Both fields describe a block on its way through a distributed aggregation
/// and mean nothing to a client reading a final result — but they are on the
/// wire and each has its own width, so they are read by number rather than
/// skipped by count.
async fn read_block_info(reader: Reader<'_>) -> DbResult<()> {
    loop {
        match wire::read_uvarint(reader).await? {
            0 => return Ok(()),
            // is_overflows
            1 => {
                wire::read_u8(reader).await?;
            }
            // bucket_num
            2 => {
                wire::read_i32(reader).await?;
            }
            other => {
                return Err(malformed(format!(
                    "field {other} in a block header, whose width is unknown"
                )))
            }
        }
    }
}

/// The empty block the client sends where the protocol requires one: after a
/// `Query` to say there is no external data, and after an `insert` header to
/// say there are no more rows.
pub fn write_empty_block(out: &mut Vec<u8>, revision: u64) {
    wire::write_uvarint(out, protocol::client::DATA);
    if revision >= protocol::MIN_REVISION_WITH_TEMPORARY_TABLES {
        wire::write_string(out, "");
    }
    if revision >= protocol::MIN_REVISION_WITH_BLOCK_INFO {
        wire::write_uvarint(out, 1);
        wire::write_u8(out, 0);
        wire::write_uvarint(out, 2);
        // -1 is "not part of a two-level aggregation", which is what a client
        // block always is.
        wire::write_i32(out, -1);
        wire::write_uvarint(out, 0);
    }
    wire::write_uvarint(out, 0);
    wire::write_uvarint(out, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CLIENT_REVISION;

    /// Build a block the way a server would, so the reader is tested against
    /// the layout and not against a mirror of itself.
    struct Builder {
        columns: Vec<u8>,
        count: u64,
        rows: u64,
    }

    impl Builder {
        fn new(rows: u64) -> Self {
            Self {
                columns: Vec::new(),
                count: 0,
                rows,
            }
        }

        fn column(mut self, name: &str, ty: &str, values: &[u8]) -> Self {
            wire::write_string(&mut self.columns, name);
            wire::write_string(&mut self.columns, ty);
            self.columns.extend_from_slice(values);
            self.count += 1;
            self
        }

        fn finish(self) -> Vec<u8> {
            let mut out = Vec::new();
            wire::write_string(&mut out, "");
            wire::write_uvarint(&mut out, 1);
            wire::write_u8(&mut out, 0);
            wire::write_uvarint(&mut out, 2);
            wire::write_i32(&mut out, -1);
            wire::write_uvarint(&mut out, 0);
            wire::write_uvarint(&mut out, self.count);
            wire::write_uvarint(&mut out, self.rows);
            out.extend_from_slice(&self.columns);
            out
        }
    }

    #[tokio::test]
    async fn a_block_reads_its_columns_side_by_side() {
        let mut ids = Vec::new();
        for id in [1u32, 2] {
            ids.extend_from_slice(&id.to_le_bytes());
        }
        let mut names = Vec::new();
        for name in ["ok", "gone"] {
            names.push(name.len() as u8);
            names.extend_from_slice(name.as_bytes());
        }
        let wire_bytes = Builder::new(2)
            .column("id", "UInt32", &ids)
            .column("name", "String", &names)
            .finish();

        let mut slice = wire_bytes.as_slice();
        let block = read_block(&mut slice, CLIENT_REVISION).await.unwrap();
        assert!(slice.is_empty(), "the block left bytes behind");
        assert_eq!(block.rows, 2);
        assert_eq!(block.columns[0].values, vec![Cellv::Int(1), Cellv::Int(2)]);
        assert_eq!(
            block.columns[1].values,
            vec![Cellv::Text("ok".into()), Cellv::Text("gone".into())]
        );
        assert_eq!(block.columns[1].type_name, "String");
    }

    #[tokio::test]
    async fn a_header_block_carries_names_and_no_bytes() {
        // Including a `LowCardinality`, whose version word is inside the part
        // the server omits when there are no rows — the case that desyncs a
        // reader which assumes prefixes are always present.
        let wire_bytes = Builder::new(0)
            .column("id", "UInt32", &[])
            .column("status", "LowCardinality(String)", &[])
            .finish();

        let mut slice = wire_bytes.as_slice();
        let block = read_block(&mut slice, CLIENT_REVISION).await.unwrap();
        assert!(slice.is_empty(), "the block left bytes behind");
        assert!(block.is_header());
        assert_eq!(block.columns.len(), 2);
        assert_eq!(block.columns[1].type_name, "LowCardinality(String)");
        assert!(block.columns[1].values.is_empty());
    }

    #[tokio::test]
    async fn a_dictionary_column_reads_its_version_word_before_its_data() {
        let mut column = Vec::new();
        // The prefix, which is written once per column and not per block.
        column.extend_from_slice(&1u64.to_le_bytes());
        column.extend_from_slice(&0u64.to_le_bytes());
        column.extend_from_slice(&2u64.to_le_bytes());
        for word in ["ready", "gone"] {
            column.push(word.len() as u8);
            column.extend_from_slice(word.as_bytes());
        }
        column.extend_from_slice(&3u64.to_le_bytes());
        column.extend_from_slice(&[0u8, 1, 0]);

        let wire_bytes = Builder::new(3)
            .column("status", "LowCardinality(String)", &column)
            .finish();
        let mut slice = wire_bytes.as_slice();
        let block = read_block(&mut slice, CLIENT_REVISION).await.unwrap();
        assert!(slice.is_empty(), "the block left bytes behind");
        assert_eq!(
            block.columns[0].values,
            vec![
                Cellv::Text("ready".into()),
                Cellv::Text("gone".into()),
                Cellv::Text("ready".into()),
            ]
        );
    }

    #[tokio::test]
    async fn a_count_that_could_not_be_real_fails_before_it_allocates() {
        let mut wire_bytes = Vec::new();
        wire::write_string(&mut wire_bytes, "");
        wire::write_uvarint(&mut wire_bytes, 0);
        wire::write_uvarint(&mut wire_bytes, 1);
        wire::write_uvarint(&mut wire_bytes, u64::MAX / 2);
        let mut slice = wire_bytes.as_slice();
        assert!(read_block(&mut slice, CLIENT_REVISION).await.is_err());
    }

    #[tokio::test]
    async fn the_empty_block_this_sends_is_one_this_can_read() {
        let mut out = Vec::new();
        write_empty_block(&mut out, CLIENT_REVISION);
        let mut slice = out.as_slice();
        // Past the packet tag, which the block reader is not given.
        assert_eq!(wire::read_uvarint(&mut slice).await.unwrap(), 2);
        let block = read_block(&mut slice, CLIENT_REVISION).await.unwrap();
        assert!(slice.is_empty());
        assert_eq!(block.rows, 0);
        assert!(block.columns.is_empty());
    }
}
