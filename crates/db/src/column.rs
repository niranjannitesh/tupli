//! Columnar storage for fetched rows.
//!
//! The grid's frame budget is 8ms for a million rows, and the layout of the
//! data decides whether that is reachable before a single line of rendering
//! code is written. `Vec<Vec<Value>>` — a row of boxes per row of data — costs
//! one pointer chase per cell and scatters a screenful of text across the heap.
//! Columnar with a shared byte buffer puts the thirty values the grid is about
//! to draw from one column into the same few cache lines.
//!
//! Nothing here allocates per cell at read time. A text cell hands back a
//! borrowed `&str` into the column's own buffer; only the numeric kinds format,
//! and they format into a caller-owned scratch string that is reused for the
//! whole frame.

use std::fmt::Write as _;

use crate::value::{format_f64, hex_prefix, Value, ValueKind};

/// A bitset of null positions. One bit per row rather than an `Option` per
/// value, which would double the size of an `i64` column for information that
/// is almost always all-zero.
#[derive(Clone, Debug, Default)]
pub struct NullMask {
    words: Vec<u64>,
    count: usize,
}

impl NullMask {
    pub fn with_capacity(rows: usize) -> Self {
        Self {
            words: Vec::with_capacity(rows.div_ceil(64)),
            count: 0,
        }
    }

    pub fn push(&mut self, is_null: bool, index: usize) {
        if index / 64 >= self.words.len() {
            self.words.push(0);
        }
        if is_null {
            self.words[index / 64] |= 1 << (index % 64);
            self.count += 1;
        }
    }

    #[inline]
    pub fn is_null(&self, index: usize) -> bool {
        // The fast path is a column with no nulls at all, which is most of
        // them: the mask is empty and this is a single length compare.
        match self.words.get(index / 64) {
            Some(word) => word & (1 << (index % 64)) != 0,
            None => false,
        }
    }

    pub fn null_count(&self) -> usize {
        self.count
    }

    pub fn any(&self) -> bool {
        self.count > 0
    }
}

/// Variable-length text stored as one contiguous buffer plus offsets.
///
/// This is the layout every columnar engine converges on, for the reason that
/// it makes a column of strings cost two allocations total instead of one per
/// value.
#[derive(Clone, Debug, Default)]
pub struct TextBuf {
    bytes: String,
    /// `len + 1` entries; value `i` is `bytes[offsets[i]..offsets[i + 1]]`.
    offsets: Vec<u32>,
}

impl TextBuf {
    pub fn with_capacity(values: usize, bytes: usize) -> Self {
        let mut offsets = Vec::with_capacity(values + 1);
        offsets.push(0);
        Self {
            bytes: String::with_capacity(bytes),
            offsets,
        }
    }

    pub fn push(&mut self, s: &str) {
        self.bytes.push_str(s);
        self.offsets.push(self.bytes.len() as u32);
    }

    pub fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn get(&self, index: usize) -> &str {
        let start = self.offsets[index] as usize;
        let end = self.offsets[index + 1] as usize;
        &self.bytes[start..end]
    }

    /// Total bytes held, for the memory readout in the status bar.
    pub fn heap_size(&self) -> usize {
        self.bytes.capacity() + self.offsets.capacity() * 4
    }
}

/// Variable-length binary, same layout as [`TextBuf`] without the UTF-8 promise.
#[derive(Clone, Debug, Default)]
pub struct BytesBuf {
    bytes: Vec<u8>,
    offsets: Vec<u32>,
}

impl BytesBuf {
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            offsets: vec![0],
        }
    }

    pub fn push(&mut self, b: &[u8]) {
        self.bytes.extend_from_slice(b);
        self.offsets.push(self.bytes.len() as u32);
    }

    pub fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, index: usize) -> &[u8] {
        let start = self.offsets[index] as usize;
        let end = self.offsets[index + 1] as usize;
        &self.bytes[start..end]
    }

    pub fn heap_size(&self) -> usize {
        self.bytes.capacity() + self.offsets.capacity() * 4
    }
}

/// The typed body of a column.
#[derive(Clone, Debug)]
pub enum ColumnData {
    Bool(Vec<bool>),
    I64(Vec<i64>),
    F64(Vec<f64>),
    Text(TextBuf),
    /// Dictionary-encoded text: `codes` indexes into `dict`.
    ///
    /// Real tables are full of low-cardinality text — statuses, enums, country
    /// codes, tenant ids — where the same forty bytes repeat a million times.
    /// Storing four bytes per row instead turns a 40MB column into a 4MB one,
    /// and it makes equality filtering an integer compare.
    Dict {
        codes: Vec<u32>,
        dict: TextBuf,
    },
    Bytes(BytesBuf),
}

impl ColumnData {
    pub fn len(&self) -> usize {
        match self {
            Self::Bool(v) => v.len(),
            Self::I64(v) => v.len(),
            Self::F64(v) => v.len(),
            Self::Text(t) => t.len(),
            Self::Dict { codes, .. } => codes.len(),
            Self::Bytes(b) => b.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn heap_size(&self) -> usize {
        match self {
            Self::Bool(v) => v.capacity(),
            Self::I64(v) => v.capacity() * 8,
            Self::F64(v) => v.capacity() * 8,
            Self::Text(t) => t.heap_size(),
            Self::Dict { codes, dict } => codes.capacity() * 4 + dict.heap_size(),
            Self::Bytes(b) => b.heap_size(),
        }
    }
}

/// Where a rendered cell's text lives.
///
/// The distinction is the point of the whole module: `Borrowed` means the grid
/// draws straight out of the column with no work at all, and it is what the
/// majority of cells return.
#[derive(Clone, Copy, Debug)]
pub enum CellText<'a> {
    /// The value is null. The grid draws its own placeholder.
    Null,
    /// Text that already exists in the column.
    Borrowed(&'a str),
    /// Text that had to be formatted; it is in the scratch buffer the caller
    /// passed in.
    Formatted,
}

/// Column metadata: everything the grid needs about a column that is not its data.
#[derive(Clone, Debug)]
pub struct ColumnMeta {
    pub name: String,
    /// The engine's own type name, shown in the header and the inspector.
    pub type_name: String,
    pub kind: ValueKind,
    pub nullable: bool,
    pub is_pk: bool,
    pub is_fk: bool,
}

impl ColumnMeta {
    pub fn new(name: impl Into<String>, kind: ValueKind, type_name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            type_name: type_name.into(),
            kind,
            nullable: true,
            is_pk: false,
            is_fk: false,
        }
    }

    pub fn pk(mut self) -> Self {
        self.is_pk = true;
        self.nullable = false;
        self
    }

    pub fn fk(mut self) -> Self {
        self.is_fk = true;
        self
    }

    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }
}

/// One column: its metadata, its null mask, and its data.
#[derive(Clone, Debug)]
pub struct Column {
    pub meta: ColumnMeta,
    pub nulls: NullMask,
    pub data: ColumnData,
}

impl Column {
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// This column with its values reordered by `order`.
    pub fn permuted(&self, order: &[u32]) -> Self {
        let mut nulls = NullMask::with_capacity(order.len());
        for (i, &src) in order.iter().enumerate() {
            nulls.push(self.nulls.is_null(src as usize), i);
        }
        let data = match &self.data {
            ColumnData::Bool(v) => ColumnData::Bool(order.iter().map(|&i| v[i as usize]).collect()),
            ColumnData::I64(v) => ColumnData::I64(order.iter().map(|&i| v[i as usize]).collect()),
            ColumnData::F64(v) => ColumnData::F64(order.iter().map(|&i| v[i as usize]).collect()),
            ColumnData::Text(t) => {
                let mut out = TextBuf::with_capacity(order.len(), t.heap_size());
                for &i in order {
                    out.push(t.get(i as usize));
                }
                ColumnData::Text(out)
            }
            // The dictionary is unchanged — only which code sits in which row.
            ColumnData::Dict { codes, dict } => ColumnData::Dict {
                codes: order.iter().map(|&i| codes[i as usize]).collect(),
                dict: dict.clone(),
            },
            ColumnData::Bytes(b) => {
                let mut out = BytesBuf::new();
                for &i in order {
                    out.push(b.get(i as usize));
                }
                ColumnData::Bytes(out)
            }
        };
        Self {
            meta: self.meta.clone(),
            nulls,
            data,
        }
    }

    /// Order two non-null values of this column against each other.
    ///
    /// Numbers compare as numbers and text as text, which is the whole reason
    /// the grid does not sort on the rendered strings: `10` before `9` is the
    /// bug every client that sorts its display text has.
    pub fn compare(&self, a: usize, b: usize) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match &self.data {
            ColumnData::Bool(v) => v[a].cmp(&v[b]),
            ColumnData::I64(v) => v[a].cmp(&v[b]),
            // NaN is not less or greater than anything, so it is treated as
            // equal rather than allowed to make the comparator inconsistent —
            // a comparator that lies can panic the sort.
            ColumnData::F64(v) => v[a].partial_cmp(&v[b]).unwrap_or(Ordering::Equal),
            ColumnData::Text(t) => t.get(a).cmp(t.get(b)),
            // Compare the strings, not the codes: the dictionary is in
            // first-seen order, which is not any order a reader would expect.
            ColumnData::Dict { codes, dict } => {
                dict.get(codes[a] as usize).cmp(dict.get(codes[b] as usize))
            }
            ColumnData::Bytes(b_) => b_.get(a).cmp(b_.get(b)),
        }
    }

    /// One owned value out of the column.
    ///
    /// The opposite trade to [`Column::render`]: this allocates, and is for the
    /// handful of values that leave the grid — a cell being edited, the key of
    /// a row being updated, a parameter about to be bound. Never call it per
    /// cell per frame.
    pub fn value(&self, row: usize) -> Value {
        if self.nulls.is_null(row) {
            return Value::Null;
        }
        let kind = self.meta.kind;
        match &self.data {
            ColumnData::Bool(v) => Value::Bool(v[row]),
            ColumnData::I64(v) => Value::Int(v[row]),
            ColumnData::F64(v) => Value::Float(v[row]),
            ColumnData::Text(t) => Value::text(kind, t.get(row)),
            ColumnData::Dict { codes, dict } => Value::text(kind, dict.get(codes[row] as usize)),
            ColumnData::Bytes(b) => Value::Bytes(b.get(row).into()),
        }
    }

    /// The display text for `row`.
    ///
    /// `scratch` is cleared and written into only when the value has to be
    /// formatted; text columns never touch it. Pass the same `String` for every
    /// cell in the frame and the grid allocates nothing to draw a screenful.
    pub fn render<'a>(&'a self, row: usize, scratch: &mut String) -> CellText<'a> {
        if self.nulls.is_null(row) {
            return CellText::Null;
        }
        match &self.data {
            ColumnData::Text(t) => CellText::Borrowed(t.get(row)),
            ColumnData::Dict { codes, dict } => CellText::Borrowed(dict.get(codes[row] as usize)),
            ColumnData::Bool(v) => CellText::Borrowed(if v[row] { "true" } else { "false" }),
            ColumnData::I64(v) => {
                scratch.clear();
                let _ = write!(scratch, "{}", v[row]);
                CellText::Formatted
            }
            ColumnData::F64(v) => {
                scratch.clear();
                scratch.push_str(&format_f64(v[row]));
                CellText::Formatted
            }
            ColumnData::Bytes(b) => {
                scratch.clear();
                let bytes = b.get(row);
                let _ = write!(scratch, "\\x{}", hex_prefix(bytes, 12));
                CellText::Formatted
            }
        }
    }

    pub fn heap_size(&self) -> usize {
        self.data.heap_size()
    }
}

/// A fetched result set: columns plus the row count they agree on.
#[derive(Clone, Debug, Default)]
pub struct ResultSet {
    pub columns: Vec<Column>,
    row_count: usize,
}

impl ResultSet {
    pub fn new(columns: Vec<Column>) -> Self {
        // Every column must be the same length or (row, col) indexing is a
        // panic waiting for a scroll event. Take the shortest rather than
        // trusting them: a driver bug should truncate the view, not crash it.
        let row_count = columns.iter().map(Column::len).min().unwrap_or(0);
        Self { columns, row_count }
    }

    /// Reorder every column by `order`, which lists source row indices in the
    /// order the result should read.
    ///
    /// A permutation rather than a sort of rows, because the storage is
    /// columnar: there is no row object to swap. Every column is rebuilt once,
    /// which is O(rows) per column and allocates one new buffer — the price of
    /// keeping the layout that makes drawing cheap. The dictionary of a
    /// dictionary-encoded column is shared, not rebuilt: only the codes move.
    pub fn permuted(&self, order: &[u32]) -> Self {
        Self::new(self.columns.iter().map(|c| c.permuted(order)).collect())
    }

    /// The row order that sorts by `col`.
    ///
    /// Nulls sort last in both directions, which is what a person scanning a
    /// column means by "sort": they are looking for the largest value or the
    /// smallest, and in neither case were they looking for the empty ones. It
    /// is also what Postgres does by default for `desc`, and the opposite of
    /// what it does for `asc` — being consistent with the other direction beats
    /// being consistent with the server for something the client is deciding
    /// on its own anyway.
    pub fn sort_order(&self, col: usize, descending: bool) -> Vec<u32> {
        let mut order: Vec<u32> = (0..self.row_count as u32).collect();
        let Some(column) = self.columns.get(col) else {
            return order;
        };

        // A stable sort so that sorting by one column and then another leaves
        // the first column's order intact inside each group — the cheap way to
        // get a two-key sort without any two-key machinery.
        order.sort_by(|&a, &b| {
            let (a, b) = (a as usize, b as usize);
            match (column.nulls.is_null(a), column.nulls.is_null(b)) {
                (true, true) => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                (false, false) => {
                    let ord = column.compare(a, b);
                    if descending {
                        ord.reverse()
                    } else {
                        ord
                    }
                }
            }
        });
        order
    }

    pub fn row_count(&self) -> usize {
        self.row_count
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.row_count == 0
    }

    /// Bytes held by the row data, excluding metadata.
    pub fn heap_size(&self) -> usize {
        self.columns.iter().map(Column::heap_size).sum()
    }
}

/// One decoded value on its way into a column.
///
/// Borrowed, and never stored: a driver decodes a row into these and the
/// builder copies what it keeps. Nothing here survives the call, which is what
/// lets a million-row fetch run without a million allocations.
#[derive(Clone, Copy, Debug)]
pub enum Cell<'a> {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(&'a str),
    Bytes(&'a [u8]),
}

/// Accumulates one column, choosing its storage from the column's kind.
///
/// The kind is decided once, from the type the server reported, rather than
/// inferred from the values — a column of integers that happens to be all null
/// in the first chunk is still an integer column, and must still right-align.
pub struct ColumnBuilder {
    meta: ColumnMeta,
    inner: BuilderData,
    nulls: NullMask,
    len: usize,
    /// Reused when a value has to be formatted on the way in.
    scratch: String,
}

enum BuilderData {
    Bool(Vec<bool>),
    I64(Vec<i64>),
    F64(Vec<f64>),
    Text(TextColumnBuilder),
    Bytes(BytesBuf),
}

impl ColumnBuilder {
    pub fn new(meta: ColumnMeta) -> Self {
        let inner = match meta.kind {
            ValueKind::Bool => BuilderData::Bool(Vec::new()),
            ValueKind::Int => BuilderData::I64(Vec::new()),
            ValueKind::Float => BuilderData::F64(Vec::new()),
            ValueKind::Bytes => BuilderData::Bytes(BytesBuf::new()),
            // Decimals, timestamps, uuids, json and everything unmapped keep
            // the server's own text. Reformatting them would be a chance to
            // lose precision or a time zone for no gain.
            _ => BuilderData::Text(TextColumnBuilder::new()),
        };
        Self {
            meta,
            inner,
            nulls: NullMask::default(),
            len: 0,
            scratch: String::new(),
        }
    }

    pub fn with_capacity(meta: ColumnMeta, rows: usize) -> Self {
        let mut builder = Self::new(meta);
        match &mut builder.inner {
            BuilderData::Bool(v) => v.reserve(rows),
            BuilderData::I64(v) => v.reserve(rows),
            BuilderData::F64(v) => v.reserve(rows),
            BuilderData::Text(_) | BuilderData::Bytes(_) => {}
        }
        builder
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn meta(&self) -> &ColumnMeta {
        &self.meta
    }

    /// Append one value. `None` is SQL NULL.
    ///
    /// A value that does not fit the column's storage — a string arriving at an
    /// integer column, which means the driver's type mapping and its decoder
    /// disagree — is stored as null rather than panicking. The alternative is a
    /// crash in the middle of a fetch over a type nobody anticipated.
    pub fn push(&mut self, value: Option<Cell<'_>>) {
        let index = self.len;
        self.len += 1;

        let Some(value) = value else {
            self.push_null_at(index);
            return;
        };

        match &mut self.inner {
            BuilderData::Bool(v) => match value {
                Cell::Bool(b) => {
                    self.nulls.push(false, index);
                    v.push(b);
                }
                _ => self.push_null_at(index),
            },
            BuilderData::I64(v) => match value {
                Cell::Int(i) => {
                    self.nulls.push(false, index);
                    v.push(i);
                }
                _ => self.push_null_at(index),
            },
            BuilderData::F64(v) => match value {
                Cell::Float(x) => {
                    self.nulls.push(false, index);
                    v.push(x);
                }
                Cell::Int(i) => {
                    self.nulls.push(false, index);
                    v.push(i as f64);
                }
                _ => self.push_null_at(index),
            },
            BuilderData::Bytes(b) => match value {
                Cell::Bytes(bytes) => {
                    self.nulls.push(false, index);
                    b.push(bytes);
                }
                _ => self.push_null_at(index),
            },
            BuilderData::Text(t) => {
                // The text builder keeps its own mask, so this arm must not
                // also push to `self.nulls`.
                match value {
                    Cell::Str(s) => t.push(Some(s)),
                    Cell::Bool(b) => t.push(Some(if b { "true" } else { "false" })),
                    Cell::Int(i) => {
                        self.scratch.clear();
                        let _ = write!(self.scratch, "{i}");
                        t.push(Some(&self.scratch));
                    }
                    Cell::Float(x) => {
                        self.scratch.clear();
                        self.scratch.push_str(&format_f64(x));
                        t.push(Some(&self.scratch));
                    }
                    Cell::Bytes(bytes) => {
                        self.scratch.clear();
                        let _ = write!(self.scratch, "\\x{}", hex_prefix(bytes, bytes.len()));
                        t.push(Some(&self.scratch));
                    }
                }
            }
        }
    }

    fn push_null_at(&mut self, index: usize) {
        match &mut self.inner {
            BuilderData::Bool(v) => {
                self.nulls.push(true, index);
                v.push(false);
            }
            BuilderData::I64(v) => {
                self.nulls.push(true, index);
                v.push(0);
            }
            BuilderData::F64(v) => {
                self.nulls.push(true, index);
                v.push(0.);
            }
            BuilderData::Bytes(b) => {
                self.nulls.push(true, index);
                b.push(&[]);
            }
            BuilderData::Text(t) => t.push(None),
        }
    }

    pub fn finish(self) -> Column {
        match self.inner {
            BuilderData::Bool(v) => Column {
                meta: self.meta,
                nulls: self.nulls,
                data: ColumnData::Bool(v),
            },
            BuilderData::I64(v) => Column {
                meta: self.meta,
                nulls: self.nulls,
                data: ColumnData::I64(v),
            },
            BuilderData::F64(v) => Column {
                meta: self.meta,
                nulls: self.nulls,
                data: ColumnData::F64(v),
            },
            BuilderData::Bytes(b) => Column {
                meta: self.meta,
                nulls: self.nulls,
                data: ColumnData::Bytes(b),
            },
            BuilderData::Text(t) => t.finish(self.meta),
        }
    }
}

/// Builds a text column, dictionary-encoding it if the values repeat enough to
/// be worth it.
///
/// The decision is made once, after `SAMPLE` values: if the sample has few
/// enough distinct values it commits to a dictionary, otherwise it commits to
/// plain text and drops the hash map. Deciding per value would mean carrying
/// the map for high-cardinality columns, which is the case where it is pure
/// overhead — a column of unique ids would pay for a map that never hits.
pub struct TextColumnBuilder {
    values: TextBuf,
    codes: Vec<u32>,
    dict: TextBuf,
    index: std::collections::HashMap<String, u32>,
    nulls: NullMask,
    len: usize,
    encoding: Encoding,
}

#[derive(PartialEq)]
enum Encoding {
    Deciding,
    Dict,
    Plain,
}

impl TextColumnBuilder {
    /// Values inspected before committing to an encoding.
    const SAMPLE: usize = 4096;
    /// Distinct values allowed in the sample for a dictionary to pay off. Above
    /// this the codes cost as much as the strings would.
    const MAX_DISTINCT: usize = 512;

    pub fn new() -> Self {
        Self {
            values: TextBuf::with_capacity(0, 0),
            codes: Vec::new(),
            dict: TextBuf::with_capacity(0, 0),
            index: std::collections::HashMap::new(),
            nulls: NullMask::default(),
            len: 0,
            encoding: Encoding::Deciding,
        }
    }

    pub fn push(&mut self, value: Option<&str>) {
        let s = value.unwrap_or("");
        self.nulls.push(value.is_none(), self.len);
        self.len += 1;

        match self.encoding {
            Encoding::Plain => self.values.push(s),
            Encoding::Dict => {
                let code = self.intern(s);
                self.codes.push(code);
            }
            Encoding::Deciding => {
                // Record both ways until the decision is made, then throw one
                // away. The sample is small enough that the duplicated work is
                // irrelevant next to getting the encoding right for the rest.
                self.values.push(s);
                let code = self.intern(s);
                self.codes.push(code);
                if self.len >= Self::SAMPLE {
                    self.commit();
                }
            }
        }
    }

    fn intern(&mut self, s: &str) -> u32 {
        if let Some(code) = self.index.get(s) {
            return *code;
        }
        let code = self.dict.len() as u32;
        self.dict.push(s);
        self.index.insert(s.to_owned(), code);
        code
    }

    fn commit(&mut self) {
        if self.index.len() <= Self::MAX_DISTINCT {
            self.encoding = Encoding::Dict;
            self.values = TextBuf::default();
        } else {
            self.encoding = Encoding::Plain;
            self.codes = Vec::new();
            self.dict = TextBuf::default();
            self.index = std::collections::HashMap::new();
        }
    }

    pub fn finish(mut self, meta: ColumnMeta) -> Column {
        if self.encoding == Encoding::Deciding {
            self.commit();
        }
        let data = if self.encoding == Encoding::Dict {
            ColumnData::Dict {
                codes: self.codes,
                dict: self.dict,
            }
        } else {
            ColumnData::Text(self.values)
        };
        Column {
            meta,
            nulls: self.nulls,
            data,
        }
    }
}

impl Default for TextColumnBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod sort_tests {
    use super::*;

    fn numbers(values: &[Option<i64>]) -> ResultSet {
        let mut nulls = NullMask::with_capacity(values.len());
        let mut data = Vec::with_capacity(values.len());
        for (i, v) in values.iter().enumerate() {
            nulls.push(v.is_none(), i);
            data.push(v.unwrap_or(0));
        }
        ResultSet::new(vec![Column {
            meta: ColumnMeta::new("n", ValueKind::Int, "int8"),
            nulls,
            data: ColumnData::I64(data),
        }])
    }

    fn read(set: &ResultSet) -> Vec<Option<i64>> {
        let column = &set.columns[0];
        let ColumnData::I64(v) = &column.data else {
            unreachable!()
        };
        (0..set.row_count())
            .map(|i| (!column.nulls.is_null(i)).then(|| v[i]))
            .collect()
    }

    #[test]
    fn numbers_sort_as_numbers_not_as_text() {
        let set = numbers(&[Some(9), Some(10), Some(1)]);
        let sorted = set.permuted(&set.sort_order(0, false));
        assert_eq!(read(&sorted), [Some(1), Some(9), Some(10)]);
    }

    #[test]
    fn nulls_go_last_in_both_directions() {
        let set = numbers(&[Some(2), None, Some(1)]);
        let up = set.permuted(&set.sort_order(0, false));
        let down = set.permuted(&set.sort_order(0, true));
        assert_eq!(read(&up), [Some(1), Some(2), None]);
        assert_eq!(read(&down), [Some(2), Some(1), None]);
    }

    #[test]
    fn a_dictionary_column_sorts_by_its_text_not_its_codes() {
        // First-seen order is zebra, apple — sorting must not follow it.
        let mut builder = TextColumnBuilder::new();
        for value in ["zebra", "apple", "zebra"] {
            builder.push(Some(value));
        }
        let set = ResultSet::new(vec![builder.finish(ColumnMeta::new(
            "s",
            ValueKind::Text,
            "text",
        ))]);
        let sorted = set.permuted(&set.sort_order(0, false));
        let mut scratch = String::new();
        let read: Vec<String> = (0..sorted.row_count())
            .map(|i| match sorted.columns[0].render(i, &mut scratch) {
                CellText::Borrowed(s) => s.to_string(),
                CellText::Formatted => scratch.clone(),
                CellText::Null => "null".into(),
            })
            .collect();
        assert_eq!(read, ["apple", "zebra", "zebra"]);
    }

    #[test]
    fn sorting_an_empty_result_is_not_a_panic() {
        let set = numbers(&[]);
        assert_eq!(set.permuted(&set.sort_order(0, true)).row_count(), 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_mask_tracks_positions_past_a_word_boundary() {
        let mut m = NullMask::with_capacity(200);
        for i in 0..200 {
            m.push(i % 7 == 0, i);
        }
        assert!(m.is_null(0));
        assert!(m.is_null(63));
        assert!(!m.is_null(64));
        assert!(m.is_null(70));
        assert!(m.is_null(133));
        assert_eq!(m.null_count(), 29);
    }

    #[test]
    fn a_column_of_all_nulls_keeps_its_declared_kind() {
        let mut b = ColumnBuilder::new(ColumnMeta::new("n", ValueKind::Int, "int8"));
        for _ in 0..3 {
            b.push(None);
        }
        let col = b.finish();
        assert!(matches!(col.data, ColumnData::I64(_)));
        assert_eq!(col.nulls.null_count(), 3);
        assert!(col.meta.kind.is_numeric());
    }

    #[test]
    fn a_value_that_does_not_fit_becomes_null_rather_than_a_panic() {
        let mut b = ColumnBuilder::new(ColumnMeta::new("n", ValueKind::Int, "int8"));
        b.push(Some(Cell::Int(7)));
        b.push(Some(Cell::Str("not a number")));
        let col = b.finish();
        assert!(!col.nulls.is_null(0));
        assert!(col.nulls.is_null(1));
    }

    #[test]
    fn timestamps_keep_the_servers_own_text() {
        let mut b = ColumnBuilder::new(ColumnMeta::new("t", ValueKind::Timestamp, "timestamptz"));
        b.push(Some(Cell::Str("2026-08-19 04:12:00.123456+00")));
        let col = b.finish();
        let mut scratch = String::new();
        match col.render(0, &mut scratch) {
            CellText::Borrowed(s) => assert_eq!(s, "2026-08-19 04:12:00.123456+00"),
            other => panic!("expected borrowed text, got {other:?}"),
        }
    }

    #[test]
    fn low_cardinality_text_becomes_a_dictionary() {
        let mut b = TextColumnBuilder::new();
        for i in 0..10_000 {
            b.push(Some(["active", "pending", "failed"][i % 3]));
        }
        let col = b.finish(ColumnMeta::new("status", ValueKind::Text, "text"));
        match &col.data {
            ColumnData::Dict { codes, dict } => {
                assert_eq!(codes.len(), 10_000);
                assert_eq!(dict.len(), 3);
            }
            other => panic!("expected a dictionary, got {other:?}"),
        }
        let mut s = String::new();
        assert!(matches!(
            col.render(9_999, &mut s),
            CellText::Borrowed("active")
        ));
    }

    #[test]
    fn high_cardinality_text_stays_plain() {
        let mut b = TextColumnBuilder::new();
        for i in 0..10_000 {
            b.push(Some(&format!("user-{i}")));
        }
        let col = b.finish(ColumnMeta::new("email", ValueKind::Text, "text"));
        assert!(matches!(col.data, ColumnData::Text(_)));
        let mut s = String::new();
        assert!(matches!(
            col.render(4_242, &mut s),
            CellText::Borrowed("user-4242")
        ));
    }

    #[test]
    fn nulls_survive_the_encoding_decision() {
        let mut b = TextColumnBuilder::new();
        for i in 0..5_000 {
            b.push(if i % 3 == 0 { None } else { Some("x") });
        }
        let col = b.finish(ColumnMeta::new("note", ValueKind::Text, "text"));
        let mut s = String::new();
        assert!(matches!(col.render(0, &mut s), CellText::Null));
        assert!(matches!(col.render(1, &mut s), CellText::Borrowed("x")));
        assert!(matches!(col.render(4_998, &mut s), CellText::Null));
    }

    #[test]
    fn result_set_row_count_is_the_shortest_column() {
        let short = Column {
            meta: ColumnMeta::new("a", ValueKind::Int, "int8"),
            nulls: NullMask::default(),
            data: ColumnData::I64(vec![1, 2]),
        };
        let long = Column {
            meta: ColumnMeta::new("b", ValueKind::Int, "int8"),
            nulls: NullMask::default(),
            data: ColumnData::I64(vec![1, 2, 3, 4]),
        };
        assert_eq!(ResultSet::new(vec![short, long]).row_count(), 2);
    }
}
