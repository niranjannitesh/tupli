//! Looking for a string in some text.
//!
//! The same three questions everywhere they are asked — the console, and the
//! grid, which reaches across for this type rather than growing a second
//! spelling of "case insensitive". Nothing here has heard of a buffer or a
//! window: it takes a `&str` and gives back offsets, which is what makes it
//! testable and what lets a grid cell and a SQL script share it.

use std::ops::Range;

/// What to look for, and how strictly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Search {
    pub query: String,
    /// Off by default. Someone typing into a find field is looking for a word,
    /// not asserting its capitalisation, and a search that misses `Orders`
    /// because they typed `orders` reads as a search that is broken.
    pub case_sensitive: bool,
    /// `id` matching `id` and not `uuid`.
    pub whole_word: bool,
}

impl Search {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            case_sensitive: false,
            whole_word: false,
        }
    }

    /// An empty query matches nothing rather than everything. Every caller
    /// treats "no query" and "no matches" the same way, and the alternative —
    /// every cell in the table lighting up the moment the field is cleared —
    /// is nobody's idea of a search.
    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    /// Whether the text contains this search anywhere. For a grid cell, where
    /// the answer is a wash over the whole cell and the position within it is
    /// not wanted.
    pub fn matches(&self, text: &str) -> bool {
        !self.is_empty() && self.scan(text, 0).is_some()
    }

    /// Every match, as char ranges, left to right and non-overlapping.
    ///
    /// Char rather than byte offsets because that is the coordinate the editor
    /// works in throughout; the scan itself is over bytes, and the conversion
    /// is one walk of the text at the end rather than one per match.
    pub fn find_all(&self, text: &str) -> Vec<Range<usize>> {
        if self.is_empty() {
            return Vec::new();
        }
        let mut bytes = Vec::new();
        let mut at = 0;
        // The query is non-empty, so every match is at least one byte wide and
        // resuming at its end always moves forward.
        while let Some((start, end)) = self.scan(text, at) {
            bytes.push(start..end);
            at = end;
        }
        to_char_ranges(text, &bytes)
    }

    /// The first match at or after `from`, as a byte range.
    fn scan(&self, hay: &str, from: usize) -> Option<(usize, usize)> {
        // The overwhelmingly common shape — a plain substring — is what the
        // standard library is already good at, and it is a memchr rather than
        // the char-by-char walk below.
        if self.case_sensitive && !self.whole_word {
            let at = from + hay.get(from..)?.find(&self.query)?;
            return Some((at, at + self.query.len()));
        }
        for (offset, _) in hay.get(from..)?.char_indices() {
            let at = from + offset;
            if let Some(end) = self.match_at(hay, at) {
                if !self.whole_word || self.bounded(hay, at, end) {
                    return Some((at, end));
                }
            }
        }
        None
    }

    /// The byte the query ends at if it starts exactly here, else `None`.
    fn match_at(&self, hay: &str, at: usize) -> Option<usize> {
        let mut rest = hay.get(at..)?.chars();
        for wanted in self.query.chars() {
            if !self.same(rest.next()?, wanted) {
                return None;
            }
        }
        Some(hay.len() - rest.as_str().len())
    }

    fn same(&self, a: char, b: char) -> bool {
        if self.case_sensitive {
            a == b
        } else {
            // Not `to_ascii_lowercase`: a query typed in one script should find
            // the same word written in the other case of that script.
            a.to_lowercase().eq(b.to_lowercase())
        }
    }

    /// Whether a word character sits on neither side of `start..end`.
    fn bounded(&self, hay: &str, start: usize, end: usize) -> bool {
        let before = hay[..start].chars().next_back();
        let after = hay[end..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte ranges to char ranges, in one walk of the text.
///
/// The ranges must be sorted and disjoint, which is how [`Search::find_all`]
/// builds them: that is what lets every boundary be translated by the same
/// pass instead of one pass per match.
fn to_char_ranges(text: &str, bytes: &[Range<usize>]) -> Vec<Range<usize>> {
    let wanted: Vec<usize> = bytes.iter().flat_map(|r| [r.start, r.end]).collect();
    let mut found: Vec<usize> = Vec::with_capacity(wanted.len());
    let mut next = 0;
    // Chained with the end of the text, which is a char boundary that
    // `char_indices` does not yield and which a match on the last word ends at.
    let boundaries = text
        .char_indices()
        .map(|(byte, _)| byte)
        .chain(std::iter::once(text.len()))
        .enumerate();
    for (chars, byte) in boundaries {
        while next < wanted.len() && wanted[next] == byte {
            found.push(chars);
            next += 1;
        }
        if next == wanted.len() {
            break;
        }
    }
    found.chunks_exact(2).map(|pair| pair[0]..pair[1]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_is_case_insensitive_unless_asked_otherwise() {
        let search = Search::new("orders");
        assert_eq!(search.find_all("select * from ORDERS"), vec![14..20]);

        let strict = Search {
            case_sensitive: true,
            ..Search::new("orders")
        };
        assert!(strict.find_all("select * from ORDERS").is_empty());
    }

    #[test]
    fn every_occurrence_is_found_left_to_right() {
        let search = Search::new("id");
        assert_eq!(search.find_all("id, uuid, id"), vec![0..2, 6..8, 10..12]);
    }

    #[test]
    fn a_whole_word_search_skips_the_word_it_is_buried_in() {
        let search = Search {
            whole_word: true,
            ..Search::new("id")
        };
        assert_eq!(search.find_all("id, uuid, id"), vec![0..2, 10..12]);
    }

    #[test]
    fn offsets_are_counted_in_chars_and_not_bytes() {
        // The é is two bytes; a byte offset would put the match one past where
        // the editor would draw it.
        assert_eq!(Search::new("x").find_all("é x"), vec![2..3]);
    }

    #[test]
    fn a_match_at_the_very_end_is_still_a_match() {
        assert_eq!(Search::new("end").find_all("the end"), vec![4..7]);
    }

    #[test]
    fn an_empty_query_matches_nothing_rather_than_everything() {
        let search = Search::new("");
        assert!(search.find_all("anything at all").is_empty());
        assert!(!search.matches("anything at all"));
    }

    #[test]
    fn a_cell_only_has_to_say_whether_it_contains_the_text() {
        assert!(Search::new("ell").matches("hello"));
        assert!(!Search::new("ell").matches("world"));
    }
}
