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
    use super::pretty;

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
}
