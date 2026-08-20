//! Re-indenting a JSON document for the value panel.
//!
//! Postgres hands back `jsonb` as one line — a 4KB object with no newline in
//! it — and one line is the one shape a person cannot read. So the panel lays
//! it out before showing it.
//!
//! This walks the text and inserts newlines rather than parsing into a
//! [`serde_json::Value`] and printing that back, for two reasons that are the
//! same reason: what comes out has to be the document that went in. A parse
//! round-trip sorts object keys into whatever order the map type keeps, and
//! rewrites every number through `f64` — `1e400`, a 30-digit id, and the
//! difference between `1.0` and `1` all change on the way through. Copying the
//! bytes across and only touching the whitespace between them cannot.

/// Lay a JSON object or array out over multiple lines, or return `None` if the
/// text is not one.
///
/// Scalars are `None` too: a bare string or number is already as readable as it
/// is going to get, and re-printing it would only mean the panel disagreeing
/// with the grid about what the cell holds.
pub fn pretty(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if !raw.starts_with(['{', '[']) {
        return None;
    }
    // Checked by a parser rather than by the walk below, which is deliberately
    // credulous about anything that is not a bracket or a quote. Postgres
    // writes an array as `{one,two}` and a composite as `(1,x)`, and a column
    // of those laid out as if it were an object would be this panel inventing a
    // shape the value does not have.
    serde_json::from_str::<serde::de::IgnoredAny>(raw).ok()?;
    let mut out = String::with_capacity(raw.len() + raw.len() / 4);
    let mut stack: Vec<char> = Vec::new();
    let mut chars = raw.char_indices().peekable();
    // Set when the outermost container closes. Anything after that point is
    // text that happens to begin with a document, not a document.
    let mut done = false;
    while let Some((ix, ch)) = chars.next() {
        if done && !ch.is_ascii_whitespace() {
            return None;
        }
        match ch {
            '"' => {
                // A string is copied whole and by index, so that whatever is
                // inside it — braces, commas, escaped quotes — is never read as
                // structure. An unterminated one is not JSON.
                let end = string_end(raw, ix)?;
                out.push_str(&raw[ix..end]);
                while chars.peek().is_some_and(|(next, _)| *next < end) {
                    chars.next();
                }
            }
            '{' | '[' => {
                stack.push(ch);
                out.push(ch);
                // `{}` and `[]` stay on one line. An empty container opened and
                // closed across three lines reads as something with contents
                // that failed to draw.
                match next_meaningful(raw, ix + 1) {
                    Some(close) if close == closing(ch) => {}
                    _ => newline(&mut out, stack.len()),
                }
            }
            '}' | ']' => {
                if stack.pop()? != opening(ch) {
                    return None;
                }
                if !out.ends_with(['{', '[']) {
                    newline(&mut out, stack.len());
                }
                out.push(ch);
                done = stack.is_empty();
            }
            ',' if !stack.is_empty() => {
                out.push(ch);
                newline(&mut out, stack.len());
            }
            ':' if stack.last() == Some(&'{') => out.push_str(": "),
            c if c.is_ascii_whitespace() => {}
            c => out.push(c),
        }
    }
    // Trailing junk after the document closed, or a container left open: either
    // way this is a string that starts with a brace, not JSON.
    match stack.is_empty() {
        true => Some(out),
        false => None,
    }
}

/// What a stretch of a JSON document is, for colouring it.
///
/// Coarser than a parser's idea of a token: everything a reader picks out of a
/// document at a glance is here, and nothing else is. A key and a string value
/// are different colours because telling them apart is most of what reading an
/// object is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Token {
    Key,
    String,
    Number,
    /// `true`, `false`, `null`.
    Literal,
    /// Braces, brackets, commas, colons.
    Punctuation,
}

/// Where each token of a JSON document is, or `None` if the text is not one.
///
/// Byte ranges into the text as given, so that this can be run over either the
/// server's single line or the laid-out version of it and the answer means the
/// same thing. Whitespace is left out — it has no colour.
///
/// Checked by a parser first, for the same reason `pretty` checks: `text[]`
/// renders as `{one,two}` and a composite as `(1,x)`, and colouring those as an
/// object would be the panel claiming a shape the value does not have.
pub fn spans(raw: &str) -> Option<Vec<(std::ops::Range<usize>, Token)>> {
    if !raw.trim_start().starts_with(['{', '[']) {
        return None;
    }
    serde_json::from_str::<serde::de::IgnoredAny>(raw).ok()?;
    let mut out = Vec::new();
    let bytes = raw.as_bytes();
    let mut ix = 0;
    while ix < bytes.len() {
        let start = ix;
        match bytes[ix] {
            b'"' => {
                let end = string_end(raw, ix)?;
                // A string is a key when a colon follows it, which is the only
                // thing that distinguishes the two in the grammar either.
                let token = match next_meaningful(raw, end) {
                    Some(':') => Token::Key,
                    _ => Token::String,
                };
                out.push((start..end, token));
                ix = end;
            }
            b'{' | b'}' | b'[' | b']' | b',' | b':' => {
                ix += 1;
                out.push((start..ix, Token::Punctuation));
            }
            b'-' | b'0'..=b'9' => {
                while ix < bytes.len()
                    && matches!(bytes[ix], b'-' | b'+' | b'.' | b'0'..=b'9' | b'e' | b'E')
                {
                    ix += 1;
                }
                out.push((start..ix, Token::Number));
            }
            b'a'..=b'z' => {
                while ix < bytes.len() && bytes[ix].is_ascii_lowercase() {
                    ix += 1;
                }
                out.push((start..ix, Token::Literal));
            }
            // Whitespace, and anything the walk does not recognise: skipped
            // rather than coloured, one character at a time so that a byte in
            // the middle of a character can never end a range.
            _ => ix += raw[ix..].chars().next().map_or(1, char::len_utf8),
        }
    }
    Some(out)
}

/// One level of indent per open container. Two spaces, which is what everything
/// that prints JSON for people to read has settled on.
fn newline(out: &mut String, depth: usize) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// The byte just past the closing quote of the string starting at `start`.
fn string_end(raw: &str, start: usize) -> Option<usize> {
    let mut escaped = false;
    for (ix, ch) in raw[start + 1..].char_indices() {
        match ch {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some(start + 1 + ix + 1),
            _ => {}
        }
    }
    None
}

/// The next character that is not whitespace, used to spot an empty container.
fn next_meaningful(raw: &str, from: usize) -> Option<char> {
    raw[from..].chars().find(|c| !c.is_ascii_whitespace())
}

fn closing(open: char) -> char {
    match open {
        '{' => '}',
        _ => ']',
    }
}

fn opening(close: char) -> char {
    match close {
        '}' => '{',
        _ => '[',
    }
}

#[cfg(test)]
mod tests {
    use super::{pretty, spans, Token};

    #[test]
    fn an_object_goes_one_field_to_a_line() {
        assert_eq!(
            pretty(r#"{"a":1,"b":[2,3]}"#).unwrap(),
            "{\n  \"a\": 1,\n  \"b\": [\n    2,\n    3\n  ]\n}"
        );
    }

    #[test]
    fn punctuation_inside_a_string_is_not_structure() {
        // The commas and the brace are text, and a `"` that was escaped did not
        // end anything.
        assert_eq!(
            pretty(r#"{"s":"a,b{c\"d"}"#).unwrap(),
            "{\n  \"s\": \"a,b{c\\\"d\"\n}"
        );
    }

    #[test]
    fn empty_containers_stay_on_one_line() {
        assert_eq!(
            pretty(r#"{"a":{},"b":[]}"#).unwrap(),
            "{\n  \"a\": {},\n  \"b\": []\n}"
        );
    }

    #[test]
    fn numbers_are_copied_rather_than_re_printed() {
        // Through a `f64` the integer loses its last digits and `1.0` loses its
        // point; `jsonb` keeps both, and so does this.
        let out = pretty(r#"{"big":10000000000000000000000000001,"one":1.0}"#).unwrap();
        assert!(out.contains("10000000000000000000000000001"), "{out}");
        assert!(out.contains("1.0"), "{out}");
    }

    #[test]
    fn a_postgres_array_is_not_an_object() {
        // `text[]` renders as `{one,two}`, which starts with a brace and is
        // nothing else about JSON.
        assert_eq!(pretty("{one,two}"), None);
        assert_eq!(pretty(r#"{"one","two"}"#), None);
    }

    #[test]
    fn keys_keep_the_order_the_row_had_them_in() {
        let out = pretty(r#"{"z":1,"a":2}"#).unwrap();
        assert!(out.find("\"z\"") < out.find("\"a\""), "{out}");
    }

    #[test]
    fn a_scalar_or_a_word_that_merely_starts_with_a_brace_is_not_json() {
        assert_eq!(pretty("42"), None);
        assert_eq!(pretty("hello"), None);
        assert_eq!(pretty("{not json"), None);
        assert_eq!(pretty(r#"{"a":1} and then some"#), None);
        assert_eq!(pretty(r#"{"a":1]"#), None);
    }

    #[test]
    fn an_already_indented_document_is_laid_out_the_same_way() {
        let once = pretty("{\n  \"a\":   1\n}").unwrap();
        assert_eq!(once, "{\n  \"a\": 1\n}");
        assert_eq!(pretty(&once).unwrap(), once);
    }

    /// The two strings in `{"a":"b"}` are different things, and the only thing
    /// that says so is the colon after the first one.
    #[test]
    fn a_string_before_a_colon_is_a_key() {
        let raw = r#"{"a":"b"}"#;
        let spans = spans(raw).unwrap();
        let kinds: Vec<Token> = spans.iter().map(|(_, token)| *token).collect();
        assert_eq!(
            kinds,
            vec![
                Token::Punctuation,
                Token::Key,
                Token::Punctuation,
                Token::String,
                Token::Punctuation,
            ]
        );
        assert_eq!(&raw[spans[1].0.clone()], "\"a\"");
        assert_eq!(&raw[spans[3].0.clone()], "\"b\"");
    }

    #[test]
    fn the_three_word_literals_are_not_strings() {
        let raw = r#"[true,false,null]"#;
        let words: Vec<&str> = spans(raw)
            .unwrap()
            .iter()
            .filter(|(_, token)| *token == Token::Literal)
            .map(|(range, _)| &raw[range.clone()])
            .collect();
        assert_eq!(words, vec!["true", "false", "null"]);
    }

    /// Colouring runs over the laid-out text as often as over the one-liner,
    /// and the indent between the tokens belongs to neither of them.
    #[test]
    fn whitespace_is_left_uncoloured() {
        let laid_out = pretty(r#"{"a":1}"#).unwrap();
        let spans = spans(&laid_out).unwrap();
        for (range, _) in &spans {
            assert!(!laid_out[range.clone()].trim().is_empty());
        }
    }

    /// A number is copied out whole, exponent and all, so that the colour ends
    /// where the value does.
    #[test]
    fn a_number_is_one_span_however_it_is_written() {
        let raw = r#"[-1.5e-9,10000000000000000000000000001]"#;
        let numbers: Vec<&str> = spans(raw)
            .unwrap()
            .iter()
            .filter(|(_, token)| *token == Token::Number)
            .map(|(range, _)| &raw[range.clone()])
            .collect();
        assert_eq!(numbers, vec!["-1.5e-9", "10000000000000000000000000001"]);
    }

    #[test]
    fn what_is_not_a_document_has_no_spans() {
        assert_eq!(spans("42"), None);
        assert_eq!(spans("hello"), None);
        // A `text[]` column, which is the reason this is checked by a parser
        // and not by its first character.
        assert_eq!(spans("{one,two}"), None);
        assert_eq!(spans("{}").unwrap().len(), 2);
    }
}
