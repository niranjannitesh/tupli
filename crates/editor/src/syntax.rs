//! Tree-sitter SQL highlighting.
//!
//! The per-line lexer in [`crate::sql`] can only see one line at a time, which
//! is enough for `select 1` and wrong for everything an editor actually holds:
//! a string that runs over a newline, a `/* … */` that spans a page, a `$$`
//! body. This module parses the whole document instead and hands the element
//! the spans for a row.
//!
//! Two things make that affordable. The parse is whole-document but only runs
//! when the buffer version changes, and a SQL console is a few kilobytes, not a
//! source tree. And the result is folded once, into a `Vec` of per-row spans,
//! so a frame that redraws forty visible lines does forty slice lookups rather
//! than forty queries.
//!
//! The token *categories* are [`crate::sql::TokenKind`], unchanged, so the
//! theme work carries over from the lexer this replaces.

use std::ops::Range;

use gpui::Hsla;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};
use ui::SyntaxTheme;

use crate::editor::Highlight;
use crate::sql::TokenKind;

/// Documents past this size are left to the line lexer.
///
/// Not because tree-sitter cannot parse them — it can, quickly — but because
/// this module reparses from scratch on every keystroke, and the point at which
/// that stops being free is somewhere around here. A console holding half a
/// megabyte of SQL is a pasted dump, and a pasted dump is read, not typed.
const MAX_BYTES: usize = 512 * 1024;

/// A parser, its query, and the spans they last produced.
pub struct Syntax {
    parser: Parser,
    query: Query,
    /// One entry per row of the last text seen, in row order. The byte ranges
    /// are relative to the start of their own row, which is what the element
    /// wants: it shapes one line at a time.
    rows: Vec<Vec<(Range<usize>, TokenKind)>>,
}

impl Syntax {
    /// The SQL parser, or `None` if the grammar and its query disagree — which
    /// would be a build-time mistake, and is covered by a test, but is not
    /// worth taking the whole editor down for at runtime.
    pub fn sql() -> Option<Self> {
        let language: tree_sitter::Language = tree_sitter_sequel::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language).ok()?;
        let query = Query::new(&language, tree_sitter_sequel::HIGHLIGHTS_QUERY).ok()?;
        Some(Self {
            parser,
            query,
            rows: Vec::new(),
        })
    }

    /// Reparse and refold. Called once per buffer version, never per frame.
    pub fn refresh(&mut self, text: &str) {
        self.rows.clear();
        if text.len() > MAX_BYTES {
            return;
        }
        // No old tree: a stale one may only be reused after `Tree::edit` has
        // been told where the text moved, and the editor does not track edits
        // in tree-sitter's terms. A full parse of a console-sized document is
        // well under a millisecond, so the bookkeeping would buy nothing.
        let Some(tree) = self.parser.parse(text, None) else {
            return;
        };

        // Paint into a byte-per-byte map rather than trying to reconcile spans
        // as they arrive: captures nest — a string inside a term inside a
        // statement — and the inner one has to win. Sorting by start ascending
        // and end *descending* puts every parent before its children, so
        // painting in that order leaves the innermost colour on top.
        const NONE: u8 = u8::MAX;
        let mut kinds = vec![NONE; text.len()];
        let names = self.query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut captures = Vec::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), text.as_bytes());
        while let Some(m) = matches.next() {
            for capture in m.captures {
                let range = capture.node.byte_range();
                if range.end > text.len() {
                    continue;
                }
                let name = names[capture.index as usize];
                let Some(kind) = kind_for(name, &text[range.clone()]) else {
                    continue;
                };
                captures.push((range.start, range.end, kind));
            }
        }
        // Start ascending, end descending, then specificity — so a parent is
        // always painted before its children, and two captures over the very
        // same node are settled by which one says more. `count` in `count(id)`
        // is both a reference to an object and a call, and it is the call that
        // a reader wants to see.
        captures.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(b.1.cmp(&a.1))
                .then(specificity(a.2).cmp(&specificity(b.2)))
        });
        for (start, end, kind) in captures {
            kinds[start..end].fill(kind as u8);
        }

        // Fold the map into per-row runs. A run stops at a change of kind, but
        // never inside a character: the element turns these lengths into
        // `TextRun`s, and a run that ends mid-codepoint would break shaping.
        let mut start = 0;
        loop {
            let end = text[start..]
                .find('\n')
                .map(|i| start + i)
                .unwrap_or(text.len());
            let mut spans: Vec<(Range<usize>, TokenKind)> = Vec::new();
            let mut i = start;
            while i < end {
                let kind = kinds[i];
                if kind == NONE {
                    i += 1;
                    continue;
                }
                let from = i;
                i += 1;
                while i < end && (kinds[i] == kind || !text.is_char_boundary(i)) {
                    i += 1;
                }
                spans.push((from - start..i - start, KINDS[kind as usize]));
            }
            self.rows.push(spans);
            if end == text.len() {
                break;
            }
            start = end + 1;
        }
    }

    /// The spans for one row, or `None` if the row is outside what was last
    /// parsed — an oversized document, or a parse that failed.
    pub fn row(&self, row: usize) -> Option<&[(Range<usize>, TokenKind)]> {
        self.rows.get(row).map(Vec::as_slice)
    }
}

impl Highlight for Syntax {
    fn refresh(&mut self, text: &str) {
        Syntax::refresh(self, text);
    }

    fn row(&self, row: usize, line: &str, syntax: &SyntaxTheme) -> Vec<(Range<usize>, Hsla)> {
        match self.row(row) {
            Some(spans) => spans
                .iter()
                .map(|(range, kind)| (range.clone(), kind.color(syntax)))
                .collect(),
            // Nothing parsed for this row. One line lexed badly beats a line
            // of undifferentiated grey.
            None => crate::sql::tokenize(line)
                .into_iter()
                .map(|(range, kind)| (range, kind.color(syntax)))
                .collect(),
        }
    }
}

/// The kinds, indexed by their `as u8`. Kept beside the enum's discriminants so
/// the byte map can store one byte per position instead of two.
const KINDS: [TokenKind; 10] = [
    TokenKind::Keyword,
    TokenKind::Type,
    TokenKind::Function,
    TokenKind::String,
    TokenKind::Number,
    TokenKind::Comment,
    TokenKind::Operator,
    TokenKind::Punctuation,
    TokenKind::Identifier,
    TokenKind::Variable,
];

/// How much a category claims to know, for when two of them describe the same
/// text. Only ever compared between captures over one identical node.
fn specificity(kind: TokenKind) -> u8 {
    match kind {
        TokenKind::Identifier => 0,
        TokenKind::Variable | TokenKind::Type => 1,
        TokenKind::Function => 2,
        _ => 3,
    }
}

/// What a capture from the grammar's `highlights.scm` means here.
///
/// The grammar names far more categories than this app paints — it separates
/// storage classes from qualifiers from attributes, all of which are keywords
/// to a reader — so most of this is deliberate flattening. `None` is for the
/// captures with nothing to say: `@spell`, which rides along on comments, and
/// anything a future grammar version adds.
fn kind_for(capture: &str, text: &str) -> Option<TokenKind> {
    Some(match capture {
        "keyword" | "keyword.operator" | "conditional" | "attribute" | "storageclass"
        | "type.qualifier" | "boolean" => TokenKind::Keyword,
        "type" | "type.builtin" => TokenKind::Type,
        "function.call" => TokenKind::Function,
        // The grammar has one `literal` node for quoted text and for numbers,
        // and tells them apart with a `#match?` predicate written in Lua
        // patterns, which the Rust bindings compile as a regex and which
        // therefore never fires. Ask the text instead.
        "string" | "number" | "float" => match text.starts_with(|c: char| c.is_ascii_digit())
            || (text.len() > 1 && text.starts_with(['+', '-', '.']))
        {
            true => TokenKind::Number,
            false => TokenKind::String,
        },
        "comment" => TokenKind::Comment,
        "operator" => TokenKind::Operator,
        "punctuation.bracket" | "punctuation.delimiter" => TokenKind::Punctuation,
        // An alias is a name the query author invented, and reads better in the
        // colour reserved for names over the one used for columns.
        "variable" | "parameter" => TokenKind::Variable,
        "field" => TokenKind::Identifier,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every span of `line`, as `(text, kind)`.
    fn spans(sql: &str) -> Vec<Vec<(String, TokenKind)>> {
        let mut syntax = Syntax::sql().expect("the SQL grammar loads");
        syntax.refresh(sql);
        sql.split('\n')
            .enumerate()
            .map(|(row, line)| {
                syntax
                    .row(row)
                    .expect("a row for every line")
                    .iter()
                    .map(|(range, kind)| (line[range.clone()].to_string(), *kind))
                    .collect()
            })
            .collect()
    }

    fn kinds_of(sql: &str, text: &str) -> Vec<TokenKind> {
        spans(sql)
            .into_iter()
            .flatten()
            .filter(|(s, _)| s == text)
            .map(|(_, kind)| kind)
            .collect()
    }

    #[test]
    fn the_grammar_and_its_query_agree() {
        assert!(Syntax::sql().is_some());
    }

    #[test]
    fn a_select_is_coloured_by_category() {
        assert_eq!(
            kinds_of("select 1 from users", "select"),
            [TokenKind::Keyword]
        );
        assert_eq!(
            kinds_of("select 1 from users", "from"),
            [TokenKind::Keyword]
        );
        assert_eq!(kinds_of("select 1 from users", "users"), [TokenKind::Type]);
    }

    #[test]
    fn a_number_is_not_a_string() {
        assert_eq!(kinds_of("select 42 from t", "42"), [TokenKind::Number]);
        assert_eq!(kinds_of("select 'x' from t", "'x'"), [TokenKind::String]);
    }

    #[test]
    fn a_call_is_a_function() {
        assert_eq!(
            kinds_of("select count(id) from t", "count"),
            [TokenKind::Function]
        );
    }

    /// The whole reason for this module: the line lexer sees `'`, decides the
    /// rest of the line is a string, and starts the next line clean.
    #[test]
    fn a_string_that_runs_over_a_newline_stays_a_string() {
        let rows = spans("select 'one\ntwo' from t");
        assert_eq!(
            rows[0],
            [
                ("select".into(), TokenKind::Keyword),
                ("'one".into(), TokenKind::String)
            ]
        );
        assert_eq!(rows[1][0], ("two'".to_string(), TokenKind::String));
    }

    #[test]
    fn a_block_comment_covers_every_line_it_touches() {
        let rows = spans("/* a\n b */\nselect 1");
        assert_eq!(rows[0], [("/* a".into(), TokenKind::Comment)]);
        assert_eq!(rows[1], [(" b */".into(), TokenKind::Comment)]);
        assert_eq!(rows[2][0], ("select".to_string(), TokenKind::Keyword));
    }

    /// Half-typed text is what an editor holds most of the time; a parser that
    /// gave up on it would be worse than the lexer.
    #[test]
    fn incomplete_sql_still_gets_its_keywords() {
        assert_eq!(
            kinds_of("select id, name fr", "select"),
            [TokenKind::Keyword]
        );
    }

    #[test]
    fn a_row_past_the_end_falls_back_rather_than_panicking() {
        let mut syntax = Syntax::sql().unwrap();
        syntax.refresh("select 1");
        assert!(syntax.row(9).is_none());
    }

    #[test]
    fn an_oversized_document_is_left_to_the_lexer() {
        let mut syntax = Syntax::sql().unwrap();
        syntax.refresh(&"select 1;\n".repeat(MAX_BYTES / 5));
        assert!(syntax.row(0).is_none());
    }

    #[test]
    fn multibyte_text_does_not_split_a_character() {
        let mut syntax = Syntax::sql().unwrap();
        let sql = "select 'héllo — wörld' from t";
        syntax.refresh(sql);
        for (range, _) in syntax.row(0).unwrap() {
            assert!(sql.is_char_boundary(range.start));
            assert!(sql.is_char_boundary(range.end));
        }
    }
}
