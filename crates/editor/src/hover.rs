//! What a name in the text means, for the panel that appears over it.
//!
//! The same division as [`crate::completion`]: the editor finds the word under
//! the pointer and hands it to a [`HoverSource`], and everything that knows a
//! column from a schema lives on the other side of that trait. Completion
//! answers "what could go here?"; this answers "what is that?" — which is the
//! question someone reading a query they did not write is actually asking, and
//! the one that otherwise costs a trip to the sidebar and back.

use std::ops::Range;

use gpui::SharedString;

use crate::completion::CompletionKind;

/// The word under the pointer, and what qualifies it.
#[derive(Clone, Debug)]
pub struct HoverContext {
    /// The whole buffer, for a source that has to read the statement around
    /// the word to know which table it belongs to.
    pub text: String,
    /// Char offset of the start of the word.
    pub offset: usize,
    /// The word itself, whole — both halves of it, unlike completion, which
    /// only ever sees what is behind the caret.
    pub word: String,
    /// What stood before the dot: `o` in `o.amount`.
    pub qualifier: Option<String>,
}

/// What the panel says.
///
/// Deliberately four flat parts rather than a document: a hover panel is read
/// in the half-second before the pointer moves on, and anything that needs
/// scrolling or scanning has failed at the only job it has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoverInfo {
    /// The name, qualified as far as it needs to be to be unambiguous.
    pub title: SharedString,
    /// Decides the icon, and is written out beside the title.
    pub kind: CompletionKind,
    /// The one line that matters most: a column's type, a table's shape.
    pub subtitle: Option<SharedString>,
    /// Label and value, one per line.
    pub rows: Vec<(SharedString, SharedString)>,
    /// The object's comment, in the database's own words.
    pub doc: Option<SharedString>,
}

impl HoverInfo {
    pub fn new(title: impl Into<SharedString>, kind: CompletionKind) -> Self {
        Self {
            title: title.into(),
            kind,
            subtitle: None,
            rows: Vec::new(),
            doc: None,
        }
    }

    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn row(mut self, label: impl Into<SharedString>, value: impl Into<SharedString>) -> Self {
        self.rows.push((label.into(), value.into()));
        self
    }

    pub fn doc(mut self, doc: impl Into<SharedString>) -> Self {
        self.doc = Some(doc.into());
        self
    }
}

/// Anything that can answer "what is that?".
pub trait HoverSource: 'static {
    fn hover(&self, context: &HoverContext) -> Option<HoverInfo>;
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The whole word the pointer is over, and the dotted path in front of it.
///
/// Both directions, unlike [`crate::completion::word_at`]: a caret in the
/// middle of `customers` is completing `custo`, but a pointer there is over
/// `customers`, and a panel that described half a name would be describing
/// nothing.
///
/// A pointer between two characters counts as being over the one behind it, so
/// that the right-hand edge of the last letter still hits the word. Whitespace
/// on both sides is nothing at all.
pub fn word_around(text: &str, offset: usize) -> Option<(Range<usize>, Option<String>)> {
    let chars: Vec<char> = text.chars().collect();
    let offset = offset.min(chars.len());
    let over = match chars.get(offset) {
        Some(c) if is_word(*c) => offset,
        // The trailing edge: `orders|` is over `orders`, not over the space.
        _ if offset > 0 && is_word(chars[offset - 1]) => offset - 1,
        _ => return None,
    };

    let mut start = over;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = over + 1;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    // A number is not a name. `10` in `limit 10` has nothing to say about
    // itself, and `2024` in a date literal has less.
    if chars[start].is_ascii_digit() {
        return None;
    }

    // The qualifier is worked out by the completion side, which already knows
    // how to walk a dotted path backwards; asking it about the start of the
    // word gives the path in front of it and an empty word.
    let (_, qualifier) = crate::completion::word_at(text, start);
    Some((start..end, qualifier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pointer_mid_word_is_over_the_whole_word() {
        let (range, qualifier) = word_around("select customers", 10).expect("a word");
        assert_eq!(range, 7..16);
        assert_eq!(qualifier, None);
    }

    #[test]
    fn the_edge_of_a_word_still_counts_as_over_it() {
        // Just past the `s`, which is where the pointer sits when it is on the
        // right half of the last letter.
        let (range, _) = word_around("select users", 12).expect("a word");
        assert_eq!(range, 7..12);
    }

    #[test]
    fn whitespace_is_over_nothing() {
        // Between the two spaces: nothing behind it either.
        assert!(word_around("select  users", 7).is_none());
        assert!(word_around("", 0).is_none());
    }

    #[test]
    fn a_dotted_name_hovers_as_its_last_part() {
        let (range, qualifier) = word_around("select o.amount from orders o", 11).expect("a word");
        assert_eq!(range, 9..15);
        assert_eq!(qualifier.as_deref(), Some("o"));
    }

    #[test]
    fn a_number_is_not_a_name() {
        assert!(word_around("limit 100", 7).is_none());
    }
}
