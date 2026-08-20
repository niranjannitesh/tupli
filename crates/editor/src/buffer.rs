//! The text itself.
//!
//! A rope, a version counter, and the coordinate conversions everything else
//! needs. Deliberately dumb: it knows nothing about selections, undo, or SQL.
//!
//! **Offsets are char offsets.** Not bytes, not UTF-16 units. One unit of
//! measure through the whole editor is worth a lot — the alternative is a
//! codebase where every function name has to say which kind of index it takes,
//! and where the one place that forgot is a panic on the first accented
//! character someone types. The two edges that need other units convert at the
//! boundary: shaping wants byte offsets into a single line ([`Buffer::line`]
//! hands out `String`s to index into), and macOS input methods want UTF-16
//! offsets into the whole document ([`Buffer::char_to_utf16`] and its inverse).

use std::ops::Range;

use ropey::Rope;

/// A position as a human reads it: which line, how far along it.
///
/// `column` is in chars, and always within the line — it never addresses the
/// newline itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub row: usize,
    pub column: usize,
}

impl Point {
    pub fn new(row: usize, column: usize) -> Self {
        Self { row, column }
    }
}

#[derive(Clone, Debug)]
pub struct Buffer {
    text: Rope,
    /// Bumped on every edit. Anything cached against the text — shaped lines,
    /// syntax runs, statement splits — is keyed by this.
    version: usize,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new("")
    }
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        Self {
            text: Rope::from_str(text),
            version: 0,
        }
    }

    pub fn version(&self) -> usize {
        self.version
    }

    /// Replace everything, keeping the version counter moving.
    ///
    /// A fresh `Buffer` starts at version zero, so assigning one over another
    /// would wind the clock back — and anything caching against the version,
    /// the longest line, the statement under the cursor, the parse, would go
    /// on believing an answer about text that is no longer there.
    pub fn reset(&mut self, text: &str) {
        let version = self.version + 1;
        *self = Self::new(text);
        self.version = version;
    }

    /// Length in chars.
    pub fn len(&self) -> usize {
        self.text.len_chars()
    }

    pub fn is_empty(&self) -> bool {
        self.text.len_chars() == 0
    }

    /// Number of lines. A buffer ending in `\n` has an empty last line, which
    /// is what an editor should show and what `ropey` already reports.
    pub fn line_count(&self) -> usize {
        self.text.len_lines()
    }

    /// One line's text, without its newline.
    pub fn line(&self, row: usize) -> String {
        if row >= self.line_count() {
            return String::new();
        }
        let slice = self.text.line(row);
        let mut s = slice.to_string();
        while s.ends_with('\n') || s.ends_with('\r') {
            s.pop();
        }
        s
    }

    /// Length of a line in chars, excluding its newline.
    pub fn line_len(&self, row: usize) -> usize {
        if row >= self.line_count() {
            return 0;
        }
        let slice = self.text.line(row);
        let mut len = slice.len_chars();
        // Walk back over the line terminator rather than subtracting a
        // constant: a file can mix `\n` and `\r\n`, and the last line has
        // neither.
        let mut chars = slice.chars_at(slice.len_chars());
        while len > 0 {
            match chars.prev() {
                Some('\n') | Some('\r') => len -= 1,
                _ => break,
            }
        }
        len
    }

    pub fn point_to_offset(&self, point: Point) -> usize {
        let point = self.clip(point);
        self.text.line_to_char(point.row) + point.column
    }

    pub fn offset_to_point(&self, offset: usize) -> Point {
        let offset = offset.min(self.len());
        let row = self.text.char_to_line(offset);
        Point {
            row,
            column: offset - self.text.line_to_char(row),
        }
    }

    /// Pull a point back onto the text: rows past the end clamp to the last
    /// line, columns past the end of a line clamp to its end.
    pub fn clip(&self, point: Point) -> Point {
        let row = point.row.min(self.line_count().saturating_sub(1));
        Point {
            row,
            column: point.column.min(self.line_len(row)),
        }
    }

    pub fn clip_offset(&self, offset: usize) -> usize {
        offset.min(self.len())
    }

    pub fn text(&self) -> String {
        self.text.to_string()
    }

    pub fn slice(&self, range: Range<usize>) -> String {
        let start = range.start.min(self.len());
        let end = range.end.clamp(start, self.len());
        self.text.slice(start..end).to_string()
    }

    /// The char at `offset`, or `None` at the end of the buffer.
    pub fn char_at(&self, offset: usize) -> Option<char> {
        if offset >= self.len() {
            None
        } else {
            Some(self.text.char(offset))
        }
    }

    /// Replace a char range. Returns what was there, so the caller can build an
    /// undo entry without reading the buffer twice.
    pub fn replace(&mut self, range: Range<usize>, new_text: &str) -> String {
        let start = range.start.min(self.len());
        let end = range.end.clamp(start, self.len());
        let old = self.text.slice(start..end).to_string();
        if start != end {
            self.text.remove(start..end);
        }
        if !new_text.is_empty() {
            self.text.insert(start, new_text);
        }
        self.version += 1;
        old
    }

    // ---- UTF-16, for input methods only ----------------------------------

    pub fn char_to_utf16(&self, offset: usize) -> usize {
        self.text.char_to_utf16_cu(offset.min(self.len()))
    }

    pub fn utf16_to_char(&self, offset: usize) -> usize {
        let max = self.text.len_utf16_cu();
        self.text.utf16_cu_to_char(offset.min(max))
    }

    pub fn len_utf16(&self) -> usize {
        self.text.len_utf16_cu()
    }
}

/// Byte offset of a char offset *within a single line*.
///
/// Shaping and highlighting both work in bytes over one line's `String`; the
/// model works in chars over the document. This is the only conversion between
/// them, and it is deliberately a free function so nothing is tempted to cache
/// it across an edit.
pub fn char_to_byte(line: &str, column: usize) -> usize {
    line.char_indices()
        .nth(column)
        .map(|(i, _)| i)
        .unwrap_or(line.len())
}

/// Inverse of [`char_to_byte`].
pub fn byte_to_char(line: &str, byte: usize) -> usize {
    line.char_indices().take_while(|(i, _)| *i < byte).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_exclude_terminators() {
        let b = Buffer::new("one\ntwo\r\nthree");
        assert_eq!(b.line_count(), 3);
        assert_eq!(b.line(0), "one");
        assert_eq!(b.line(1), "two");
        assert_eq!(b.line(2), "three");
        assert_eq!(b.line_len(1), 3);
    }

    #[test]
    fn trailing_newline_makes_an_empty_last_line() {
        let b = Buffer::new("one\n");
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.line(1), "");
        assert_eq!(b.clip(Point::new(9, 9)), Point::new(1, 0));
    }

    #[test]
    fn points_round_trip_through_offsets() {
        let b = Buffer::new("héllo\nwörld");
        let p = Point::new(1, 3);
        assert_eq!(b.offset_to_point(b.point_to_offset(p)), p);
        // Chars, not bytes: "héllo" is 5 chars and 6 bytes.
        assert_eq!(b.point_to_offset(Point::new(1, 0)), 6);
    }

    #[test]
    fn a_reset_moves_the_version_forward() {
        let mut b = Buffer::new("hello");
        b.replace(5..5, " world");
        let before = b.version();
        b.reset("something else entirely");
        assert_eq!(b.text(), "something else entirely");
        assert!(b.version() > before);
    }

    #[test]
    fn replace_returns_the_old_text() {
        let mut b = Buffer::new("hello world");
        assert_eq!(b.replace(6..11, "there"), "world");
        assert_eq!(b.text(), "hello there");
        assert_eq!(b.version(), 1);
    }

    #[test]
    fn line_byte_conversion_handles_multibyte() {
        assert_eq!(char_to_byte("héllo", 3), 4);
        assert_eq!(byte_to_char("héllo", 4), 3);
        assert_eq!(char_to_byte("héllo", 99), 6);
    }
}
