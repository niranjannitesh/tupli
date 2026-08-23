//! Completion: what can be offered, how the word under the cursor is found,
//! and how the offers are ranked.
//!
//! The editor knows nothing about databases. It knows there is a word under the
//! cursor, that the word may be qualified by something before a dot, and that
//! it can hand both to a [`CompletionSource`] and get a list back. Everything
//! about schemas, tables and search paths lives on the other side of that
//! trait, in the app — which is what keeps this crate usable for the password
//! field and the filter box as well as for the SQL console.

use std::ops::Range;

use gpui::SharedString;
use ui::{IconColor, IconName};

/// What an offer is, which decides its icon and how it sorts against its peers.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum CompletionKind {
    /// A column of a table in play. First, always: in a query being typed
    /// against a known table, the column is nearly always what is meant.
    Column,
    Table,
    View,
    Function,
    Schema,
    Keyword,
}

impl CompletionKind {
    pub fn icon(self) -> IconName {
        match self {
            Self::Column => IconName::Columns,
            Self::Table => IconName::Table,
            Self::View => IconName::Eye,
            Self::Function => IconName::BracketsCurly,
            Self::Schema => IconName::Layers,
            Self::Keyword => IconName::Code,
        }
    }

    /// What to call it in prose. The hover panel writes it out beside the
    /// name, because an icon alone does not distinguish a view from a table to
    /// anyone who has not learned the icons yet.
    pub fn label(self) -> &'static str {
        match self {
            Self::Column => "column",
            Self::Table => "table",
            Self::View => "view",
            Self::Function => "function",
            Self::Schema => "schema",
            Self::Keyword => "keyword",
        }
    }

    pub fn color(self) -> IconColor {
        match self {
            Self::Column => IconColor::Accent,
            Self::Table | Self::View => IconColor::Muted,
            _ => IconColor::Subtle,
        }
    }
}

/// One offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub label: SharedString,
    /// What is typed in, when that is not the label — a quoted identifier, or a
    /// function with its brackets already closed.
    pub insert: Option<SharedString>,
    /// The dim text on the right: a column's type, a table's schema.
    pub detail: Option<SharedString>,
    pub kind: CompletionKind,
}

impl Completion {
    pub fn new(label: impl Into<SharedString>, kind: CompletionKind) -> Self {
        Self {
            label: label.into(),
            insert: None,
            detail: None,
            kind,
        }
    }

    pub fn insert(mut self, insert: impl Into<SharedString>) -> Self {
        self.insert = Some(insert.into());
        self
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn text(&self) -> &str {
        self.insert.as_deref().unwrap_or(&self.label)
    }
}

/// Where the cursor is and what it is sitting in, worked out once by the editor
/// so that every source does not have to re-derive it.
#[derive(Clone, Debug)]
pub struct CompletionContext {
    /// The whole buffer. Sources that care about `from` clauses read it.
    pub text: String,
    /// Cursor position, in chars.
    pub offset: usize,
    /// The word being completed, which may be empty.
    pub prefix: String,
    /// What stood before the dot, if the word is qualified: `orders` in
    /// `orders.cust`, and `public.orders` in `public.orders.cust`.
    pub qualifier: Option<String>,
    /// True when the user asked for the list rather than the list appearing on
    /// its own. An explicit request offers everything; typing offers only what
    /// matches.
    pub explicit: bool,
}

/// Anything that can answer "what could go here?".
pub trait CompletionSource: 'static {
    fn completions(&self, context: &CompletionContext) -> Vec<Completion>;
}

/// A source made of a fixed list, which is all the tests and the keyword-only
/// case need.
impl CompletionSource for Vec<Completion> {
    fn completions(&self, _context: &CompletionContext) -> Vec<Completion> {
        self.clone()
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The word under the cursor, in chars, and the dotted path in front of it.
///
/// Only what is *behind* the cursor counts. Completing on the text after it as
/// well would mean that putting the cursor in the middle of `customers` and
/// typing offers to replace the whole word, which is never what a cursor placed
/// mid-word is for.
pub fn word_at(text: &str, offset: usize) -> (Range<usize>, Option<String>) {
    let chars: Vec<char> = text.chars().collect();
    let offset = offset.min(chars.len());
    let mut start = offset;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }

    // The qualifier is every dotted segment immediately before the word, so
    // `public.orders.` qualifies with both halves and the source decides how
    // much of it it can use.
    let mut at = start;
    let mut qualifier: Option<String> = None;
    while at > 0 && chars[at - 1] == '.' {
        let mut seg = at - 1;
        while seg > 0 && is_word(chars[seg - 1]) {
            seg -= 1;
        }
        if seg == at - 1 {
            // A dot with nothing in front of it: `.foo`, or the tail of a
            // number. Nothing to qualify with.
            break;
        }
        let text: String = chars[seg..at - 1].iter().collect();
        qualifier = Some(match qualifier {
            Some(rest) => format!("{text}.{rest}"),
            None => text,
        });
        at = seg;
    }

    (start..offset, qualifier)
}

/// How well `label` answers `prefix`. Lower is better; `None` does not match.
///
/// Three tiers rather than a fuzzy score, because a completion list is read
/// top-down and small differences in the middle of it are noise: what matters
/// is that everything starting with what was typed comes before everything that
/// merely contains it.
fn score(label: &str, prefix: &str) -> Option<u8> {
    if prefix.is_empty() {
        return Some(0);
    }
    let (label, prefix) = (label.to_ascii_lowercase(), prefix.to_ascii_lowercase());
    if label.starts_with(&prefix) {
        return Some(0);
    }
    // `created_at` for `at`: a word boundary inside the name is nearly as good
    // as the start of it, and snake_case makes those boundaries explicit.
    if label.split('_').any(|part| part.starts_with(&prefix)) {
        return Some(1);
    }
    if label.contains(&prefix) {
        return Some(2);
    }
    // Subsequence, last: `cat` finds `created_at`.
    let mut chars = label.chars();
    prefix
        .chars()
        .all(|c| chars.any(|l| l == c))
        .then_some(3)
}

/// Longest list worth showing. Past this the popup is a table listing rather
/// than a suggestion, and the answer is to type another character.
pub const MAX_COMPLETIONS: usize = 40;

/// Filter and order the offers for `prefix`.
pub fn rank(items: Vec<Completion>, prefix: &str) -> Vec<Completion> {
    let mut scored: Vec<(u8, Completion)> = items
        .into_iter()
        .filter_map(|item| score(&item.label, prefix).map(|s| (s, item)))
        .collect();
    // Sort is stable, so a source that already has an order of its own — the
    // column order of a table, say — keeps it within each tier. With nothing
    // typed there are no tiers, and that source order is the whole answer:
    // sorting a table's columns by name length would scramble the one order
    // the person asking already knows the table by.
    let by_length = !prefix.is_empty();
    scored.sort_by(|a, b| {
        a.0.cmp(&b.0).then(a.1.kind.cmp(&b.1.kind)).then_with(|| {
            match by_length {
                // `id` before `identifier`: a shorter word that matched the
                // same way matched more of itself.
                true => a.1.label.len().cmp(&b.1.label.len()),
                false => std::cmp::Ordering::Equal,
            }
        })
    });
    scored.truncate(MAX_COMPLETIONS);
    scored.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_word_under_the_cursor_ends_at_the_cursor() {
        let (range, qualifier) = word_at("select cust", 11);
        assert_eq!(range, 7..11);
        assert_eq!(qualifier, None);
        // Mid-word: only the half behind the cursor is being completed.
        let (range, _) = word_at("select customers", 11);
        assert_eq!(range, 7..11);
    }

    #[test]
    fn a_dot_qualifies_the_word_after_it() {
        let (range, qualifier) = word_at("select o.na", 11);
        assert_eq!(range, 9..11);
        assert_eq!(qualifier.as_deref(), Some("o"));

        let (range, qualifier) = word_at("select public.orders.", 21);
        assert_eq!(range, 21..21);
        assert_eq!(qualifier.as_deref(), Some("public.orders"));
    }

    #[test]
    fn a_dot_with_nothing_in_front_qualifies_nothing() {
        let (_, qualifier) = word_at("select .na", 10);
        assert_eq!(qualifier, None);
    }

    #[test]
    fn matches_are_ordered_by_how_well_they_match() {
        let items = vec![
            Completion::new("created_at", CompletionKind::Column),
            Completion::new("cat_id", CompletionKind::Column),
            Completion::new("category", CompletionKind::Column),
        ];
        let ranked = rank(items, "cat");
        let labels: Vec<_> = ranked.iter().map(|c| c.label.as_ref()).collect();
        // Both prefix matches first, shorter one before longer; the word-start
        // match after them; the subsequence match last.
        assert_eq!(labels, ["cat_id", "category", "created_at"]);
    }

    #[test]
    fn a_column_outranks_a_keyword_that_matches_as_well() {
        let items = vec![
            Completion::new("select", CompletionKind::Keyword),
            Completion::new("selected", CompletionKind::Column),
        ];
        let ranked = rank(items, "sel");
        assert_eq!(ranked[0].label.as_ref(), "selected");
    }

    #[test]
    fn nothing_typed_offers_everything_it_was_given() {
        let items = vec![
            Completion::new("a", CompletionKind::Column),
            Completion::new("b", CompletionKind::Column),
        ];
        assert_eq!(rank(items, "").len(), 2);
    }

    /// `orders.` should read down the table, not shortest name first.
    #[test]
    fn nothing_typed_keeps_the_order_it_was_given() {
        let items = ["id", "user_id", "provider", "amount"]
            .map(|name| Completion::new(name, CompletionKind::Column))
            .to_vec();
        let ranked = rank(items, "");
        let labels: Vec<_> = ranked.iter().map(|c| c.label.as_ref()).collect();
        assert_eq!(labels, ["id", "user_id", "provider", "amount"]);
    }
}
