//! A per-line SQL lexer, and the rule for where a statement begins and ends.
//!
//! [`crate::syntax`] does the real highlighting now — a parse of the whole
//! document, which is the only way to be right about a string or a comment that
//! runs over a newline. What is left here is the fallback it drops to when the
//! grammar will not load or the document is too large to reparse on every
//! keystroke, and the token categories both of them speak in.
//!
//! [`statement_at`] is not a fallback. ⌘⏎ runs the statement under the cursor,
//! and finding it by scanning for an unquoted `;` costs nothing and is right
//! for the SQL people write. §11 of the plan replaces it with `pg_query.rs`
//! when statement splitting has to also be *semantically* right.

use std::ops::Range;

use gpui::Hsla;
use ui::SyntaxTheme;

/// Also the completion source's keyword list: one place where the words this
/// app knows about are written down.
pub const KEYWORDS: &[&str] = &[
    "select",
    "from",
    "where",
    "join",
    "inner",
    "left",
    "right",
    "full",
    "outer",
    "on",
    "group",
    "by",
    "having",
    "order",
    "limit",
    "offset",
    "insert",
    "into",
    "values",
    "update",
    "set",
    "delete",
    "create",
    "table",
    "view",
    "index",
    "alter",
    "drop",
    "add",
    "column",
    "primary",
    "key",
    "foreign",
    "references",
    "not",
    "null",
    "default",
    "and",
    "or",
    "as",
    "distinct",
    "union",
    "all",
    "case",
    "when",
    "then",
    "else",
    "end",
    "with",
    "returning",
    "cascade",
    "interval",
    "asc",
    "desc",
    "exists",
    "in",
    "is",
    "like",
    "ilike",
    "between",
    "begin",
    "commit",
    "rollback",
    "explain",
    "analyze",
];

const TYPES: &[&str] = &[
    "int",
    "int2",
    "int4",
    "int8",
    "integer",
    "bigint",
    "smallint",
    "text",
    "varchar",
    "char",
    "bool",
    "boolean",
    "numeric",
    "decimal",
    "real",
    "double",
    "precision",
    "date",
    "time",
    "timestamp",
    "timestamptz",
    "uuid",
    "json",
    "jsonb",
    "bytea",
    "serial",
    "bigserial",
];

pub const FUNCTIONS: &[&str] = &[
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "coalesce",
    "now",
    "nullif",
    "greatest",
    "least",
    "length",
    "lower",
    "upper",
    "cast",
    "date_trunc",
    "extract",
    "row_number",
    "rank",
];

/// The categories the editor paints. Deliberately fewer than any grammar
/// names: a reader is looking for the shape of a statement, not a taxonomy.
///
/// The discriminants are load-bearing — [`crate::syntax`] indexes a byte map by
/// them — so add to the end rather than the middle.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TokenKind {
    Keyword,
    Type,
    Function,
    String,
    Number,
    Comment,
    Operator,
    Punctuation,
    Identifier,
    /// A name the query author chose: a table alias, a bound parameter.
    Variable,
}

impl TokenKind {
    pub fn color(self, syntax: &SyntaxTheme) -> Hsla {
        match self {
            TokenKind::Keyword => syntax.keyword,
            TokenKind::Type => syntax.type_name,
            TokenKind::Function => syntax.function,
            TokenKind::String => syntax.string,
            TokenKind::Number => syntax.number,
            TokenKind::Comment => syntax.comment,
            TokenKind::Operator => syntax.operator,
            TokenKind::Punctuation => syntax.punctuation,
            TokenKind::Identifier => syntax.identifier,
            TokenKind::Variable => syntax.variable,
        }
    }
}

/// Byte ranges and their category, in order, for one line.
pub fn tokenize(line: &str) -> Vec<(Range<usize>, TokenKind)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        let start = i;
        let b = bytes[i];

        if b == b'-' && bytes.get(i + 1) == Some(&b'-') {
            out.push((start..bytes.len(), TokenKind::Comment));
            break;
        }

        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        if b == b'\'' || b == b'"' {
            let quote = b;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push((
                start..i,
                if quote == b'"' {
                    TokenKind::Identifier
                } else {
                    TokenKind::String
                },
            ));
            continue;
        }

        if b.is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            out.push((start..i, TokenKind::Number));
            continue;
        }

        if b.is_ascii_alphabetic() || b == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = line[start..i].to_ascii_lowercase();
            // A word immediately followed by `(` is a call, whatever else it is.
            let called = bytes.get(i) == Some(&b'(');
            let kind = if called && FUNCTIONS.contains(&word.as_str()) {
                TokenKind::Function
            } else if KEYWORDS.contains(&word.as_str()) {
                TokenKind::Keyword
            } else if TYPES.contains(&word.as_str()) {
                TokenKind::Type
            } else if called {
                TokenKind::Function
            } else {
                TokenKind::Identifier
            };
            out.push((start..i, kind));
            continue;
        }

        i += 1;
        let kind = match b {
            b'(' | b')' | b',' | b';' | b'.' | b'[' | b']' => TokenKind::Punctuation,
            _ => TokenKind::Operator,
        };
        out.push((start..i, kind));
    }

    out
}

/// The highlighter the SQL console installs.
///
/// Tree-sitter when the grammar loads, the line lexer above when it does not.
/// One place to make that choice, so no call site has to know which one it got.
pub fn highlighter() -> crate::editor::Highlighter {
    match crate::syntax::Syntax::sql() {
        Some(syntax) => Box::new(syntax),
        None => Box::new(|line: &str, syntax: &SyntaxTheme| {
            tokenize(line)
                .into_iter()
                .map(|(range, kind)| (range, kind.color(syntax)))
                .collect::<Vec<_>>()
        }),
    }
}

/// Every real statement terminator in `text`, as char indices of the `;`.
///
/// "Real" is the whole job: a `;` inside `'a; b'`, inside `-- a; b`, inside a
/// block comment, or inside a `$$ … $$` function body ends nothing. Postgres
/// nests block comments, so the depth is counted rather than searched for.
///
/// This is the cheap scanner, not a parser. §11 of the plan keeps `pg_query`
/// in reserve for the day splitting has to be exactly right; until then, the
/// cases below are the ones people actually type.
fn terminators(chars: &[char]) -> Vec<usize> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\'' | '"' => {
                let quote = chars[i];
                // `E'…'` takes C-style escapes, so a backslash there hides the
                // character after it — including the closing quote.
                let escapes = quote == '\'' && i > 0 && matches!(chars[i - 1], 'e' | 'E');
                i += 1;
                while i < chars.len() {
                    if escapes && chars[i] == '\\' {
                        i += 2;
                        continue;
                    }
                    if chars[i] == quote {
                        // A doubled quote is an escaped quote, not a close.
                        if chars.get(i + 1) == Some(&quote) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            '-' if chars.get(i + 1) == Some(&'-') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                let mut depth = 1;
                i += 2;
                while i + 1 < chars.len() && depth > 0 {
                    match (chars[i], chars[i + 1]) {
                        ('/', '*') => {
                            depth += 1;
                            i += 2;
                        }
                        ('*', '/') => {
                            depth -= 1;
                            i += 2;
                        }
                        _ => i += 1,
                    }
                }
                continue;
            }
            '$' => match dollar_quote(chars, i) {
                Some(close) => i = close,
                None => {}
            },
            ';' => found.push(i),
            _ => {}
        }
        i += 1;
    }
    found
}

/// If a `$tag$` opens at `at`, where the matching close ends. The tag is
/// `$` `[A-Za-z_][A-Za-z0-9_]*`? `$`, and only an identical tag closes it —
/// which is the entire point of the syntax, since the body is usually SQL with
/// semicolons in it.
fn dollar_quote(chars: &[char], at: usize) -> Option<usize> {
    let mut end = at + 1;
    while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
        // A tag cannot start with a digit; `$1` is a parameter, not a quote.
        if end == at + 1 && chars[end].is_ascii_digit() {
            return None;
        }
        end += 1;
    }
    if chars.get(end) != Some(&'$') {
        return None;
    }
    let tag = &chars[at..=end];
    let mut i = end + 1;
    while i + tag.len() <= chars.len() {
        if &chars[i..i + tag.len()] == tag {
            return Some(i + tag.len() - 1);
        }
        i += 1;
    }
    // An unclosed tag runs to the end of the document, which is what it looks
    // like while somebody is still typing the body.
    Some(chars.len().saturating_sub(1))
}

/// The statement the cursor is inside, as a char range.
///
/// ⌘⏎ runs *a* statement, not the file, so the editor has to know where the one
/// under the cursor begins and ends.
///
/// The range is trimmed of leading and trailing whitespace so the marker in the
/// gutter covers the statement and not the blank lines around it.
pub fn statement_at(text: &str, offset: usize) -> Range<usize> {
    let chars: Vec<char> = text.chars().collect();
    let ends = terminators(&chars);
    // The first terminator at or after the cursor closes the statement the
    // cursor is in; the one before it — if there is one — is where that
    // statement began.
    let which = ends.iter().position(|&at| offset <= at);
    let end = which.map_or(chars.len(), |i| ends[i] + 1);
    let start = match which {
        Some(0) | None if ends.is_empty() => 0,
        Some(0) => 0,
        Some(i) => ends[i - 1] + 1,
        None => ends.last().map_or(0, |at| at + 1),
    };
    trimmed(&chars, start..end)
}

/// Every statement in `text`, in order, as char ranges.
///
/// ⌘⇧⏎ runs the lot, one at a time — the extended protocol prepares a single
/// statement, so a semicolon-separated script has to be taken apart before any
/// of it is sent. Whitespace and comment-only fragments are dropped: they are
/// what a trailing `;` and a commented-out line leave behind, and sending them
/// would put an empty row in the message log for every one.
pub fn statements(text: &str) -> Vec<Range<usize>> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    for at in terminators(&chars).into_iter().chain([chars.len()]) {
        let end = (at + 1).min(chars.len());
        let range = trimmed(&chars, start..end);
        if !is_blank(&chars[range.clone()]) {
            out.push(range);
        }
        start = end;
    }
    out
}

/// The same range with the whitespace at either end left out.
fn trimmed(chars: &[char], range: Range<usize>) -> Range<usize> {
    let mut start = range.start;
    let mut end = range.end.min(chars.len());
    while start < end && chars[start].is_whitespace() {
        start += 1;
    }
    while end > start && chars[end - 1].is_whitespace() {
        end -= 1;
    }
    start..end
}

/// Nothing a server would do anything with: whitespace, comments, and the
/// semicolon that ended the statement before.
fn is_blank(chars: &[char]) -> bool {
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            c if c.is_whitespace() || c == ';' => i += 1,
            '-' if chars.get(i + 1) == Some(&'-') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                let mut depth = 1;
                i += 2;
                while i + 1 < chars.len() && depth > 0 {
                    match (chars[i], chars[i + 1]) {
                        ('/', '*') => {
                            depth += 1;
                            i += 2;
                        }
                        ('*', '/') => {
                            depth -= 1;
                            i += 2;
                        }
                        _ => i += 1,
                    }
                }
            }
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_statement_under_the_cursor_is_found() {
        let sql = "select 1;\nselect 2;\n";
        assert_eq!(statement_at(sql, 0), 0..9);
        assert_eq!(statement_at(sql, 12), 10..19);
    }

    #[test]
    fn a_semicolon_in_a_string_does_not_split() {
        let sql = "select 'a; b' from t;";
        assert_eq!(statement_at(sql, 0), 0..21);
    }

    #[test]
    fn a_semicolon_in_a_comment_does_not_split() {
        let sql = "-- drop it;\nselect 1;";
        assert_eq!(statement_at(sql, 15), 0..21);
    }

    #[test]
    fn the_last_statement_needs_no_terminator() {
        let sql = "select 1;\nselect 2";
        assert_eq!(statement_at(sql, 18), 10..18);
    }

    #[test]
    fn a_dollar_quoted_body_is_one_statement() {
        let sql = "create function f() returns int as $$ begin; return 1; end; $$ language plpgsql;\nselect 2;";
        let found = statements(sql);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(sql[found[0].clone()].ends_with("language plpgsql;"));
        assert!(sql[found[1].clone()].starts_with("select 2"));
    }

    #[test]
    fn a_script_splits_into_its_statements() {
        let sql = "select 1;\n\n-- a note\nselect 'a; b';\n;\nselect 3\n";
        let found = statements(sql);
        let text: Vec<&str> = found.iter().map(|r| &sql[r.clone()]).collect();
        assert_eq!(text, ["select 1;", "-- a note\nselect 'a; b';", "select 3"]);
    }

    #[test]
    fn nothing_but_comments_is_not_a_statement() {
        assert!(statements("-- nothing\n/* at */ /* all */\n;;\n").is_empty());
        assert!(statements("   ").is_empty());
    }

    #[test]
    fn an_escaped_quote_does_not_open_a_string() {
        // `E'\''` is one string containing a quote; a scanner that missed the
        // backslash would see the rest of the file as string body.
        let sql = "select E'\\'' as q;\nselect 2;";
        assert_eq!(statements(sql).len(), 2);
    }
}
