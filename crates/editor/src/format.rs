//! Laying SQL out.
//!
//! What this does and does not do is the whole design. It changes *whitespace*:
//! where the lines break, how far each one is indented, which tokens sit tight
//! against each other. It does not change a single character of what you wrote
//! — not the case of a keyword, not a quote style, not an alias, not the order
//! of anything. A formatter that also rewrites is a formatter people stop
//! pressing, because once it has silently upper-cased a column name inside a
//! quoted identifier you never trust it again.
//!
//! The rules, in one place:
//!
//! * a clause at the top level of a statement starts a line at the left margin
//!   — `select`, `from`, `where`, `order`, and the rest of them;
//! * a comma at the top level ends its line, so a select list reads down;
//! * `and` / `or` at the top level start an indented line, so a predicate with
//!   four terms is four lines rather than one long one;
//! * a join gets its own indented line, with its `on` beside it;
//! * everything inside parentheses stays on one line, because the parenthesis
//!   is already saying "this is one thing" — the exception is the column list
//!   of a `create table`, which is a list of definitions and reads as one;
//! * a blank line the author left between two statements stays.
//!
//! Comments survive, in place: a `--` that trailed a line still trails it, and
//! one that had a line to itself keeps it. That is the other half of not
//! rewriting — losing a comment is worse than any layout.

use crate::sql::KEYWORDS;

/// Four spaces, the same as [`sqlgen`](../../sqlgen/src/ddl.rs)'s DDL. Two
/// indentation widths in one window would be two answers to a question the
/// reader is not even asking.
const INDENT: &str = "    ";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Word,
    /// A number, a string, a dollar-quoted body, a quoted identifier: anything
    /// whose insides this module must not look at.
    Atom,
    /// `--` to the end of the line.
    LineComment,
    /// `/* … */`, which may run over several lines and is emitted verbatim.
    BlockComment,
    Punct,
}

struct Token {
    kind: Kind,
    text: String,
    /// The word, lowercased, for a [`Kind::Word`]. Empty otherwise. Kept beside
    /// the text so the emitter never lowercases in a hot loop, and never
    /// confuses "what it says" with "what it is".
    lower: String,
    /// A newline stood between this token and the last one.
    broke: bool,
    /// Two or more did — an author's paragraph break, which is worth keeping.
    paragraph: bool,
}

/// Format `src`, or return it unchanged if it is not something this can take
/// apart — an unterminated string or comment, which is to say text in the
/// middle of being typed.
pub fn format(src: &str) -> String {
    match lex(src) {
        Some(tokens) if !tokens.is_empty() => emit(&tokens),
        _ => src.to_string(),
    }
}

// ---- lexing ---------------------------------------------------------------

fn lex(src: &str) -> Option<Vec<Token>> {
    let bytes = src.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut i = 0;
    let (mut broke, mut paragraph) = (false, false);

    while i < bytes.len() {
        let c = bytes[i] as char;

        if c.is_whitespace() {
            if c == '\n' {
                paragraph |= broke;
                broke = true;
            }
            i += 1;
            continue;
        }

        let start = i;
        let kind = if c == '-' && bytes.get(i + 1) == Some(&b'-') {
            i = src[i..].find('\n').map_or(bytes.len(), |ix| i + ix);
            Kind::LineComment
        } else if c == '/' && bytes.get(i + 1) == Some(&b'*') {
            // Postgres nests these, so a depth counter rather than a search
            // for the first `*/`.
            let mut depth = 0;
            loop {
                match (bytes.get(i), bytes.get(i + 1)) {
                    (Some(b'/'), Some(b'*')) => {
                        depth += 1;
                        i += 2;
                    }
                    (Some(b'*'), Some(b'/')) => {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    }
                    (Some(_), _) => i += 1,
                    // Ran off the end mid-comment: not something to reflow.
                    (None, _) => return None,
                }
            }
            Kind::BlockComment
        } else if c == '\'' || c == '"' {
            // A doubled quote is an escaped one, which falls out of just
            // looking for the next quote and carrying on.
            i += 1;
            loop {
                match bytes.get(i) {
                    None => return None,
                    Some(&b) if b == c as u8 => {
                        i += 1;
                        if bytes.get(i) == Some(&(c as u8)) {
                            i += 1;
                            continue;
                        }
                        break;
                    }
                    Some(_) => i += 1,
                }
            }
            Kind::Atom
        } else if c == '$' {
            match dollar_quote(src, i) {
                Some(end) => {
                    i = end;
                    Kind::Atom
                }
                // A lone `$` — a parameter placeholder, `$1`.
                None => {
                    i += 1;
                    while i < bytes.len() && (bytes[i] as char).is_ascii_alphanumeric() {
                        i += 1;
                    }
                    Kind::Atom
                }
            }
        } else if c.is_ascii_digit() {
            while i < bytes.len()
                && matches!(bytes[i] as char, c if c.is_ascii_alphanumeric() || c == '.')
            {
                i += 1;
            }
            Kind::Atom
        } else if c.is_alphabetic() || c == '_' {
            while i < bytes.len()
                && matches!(bytes[i] as char, c if c.is_alphanumeric() || c == '_' || c == '$')
            {
                i += 1;
            }
            Kind::Word
        } else {
            // The operators that are two or three characters. Longest first, or
            // `->>` lexes as `->` and a stray `>`.
            const OPERATORS: &[&str] = &[
                "->>", "#>>", "<<=", ">>=", "::", "<>", "!=", "<=", ">=", "||", "->", "#>", ":=",
                "=>", "<<", ">>", "@>", "<@", "?|", "?&", "||/",
            ];
            match OPERATORS.iter().find(|op| src[i..].starts_with(**op)) {
                Some(op) => i += op.len(),
                None => i += c.len_utf8(),
            }
            Kind::Punct
        };

        let text = src[start..i].to_string();
        let lower = match kind {
            Kind::Word => text.to_lowercase(),
            _ => String::new(),
        };
        tokens.push(Token {
            kind,
            text,
            lower,
            broke,
            paragraph,
        });
        broke = false;
        paragraph = false;
    }
    Some(tokens)
}

/// The end of the `$tag$ … $tag$` starting at `at`, if that is what it is.
fn dollar_quote(src: &str, at: usize) -> Option<usize> {
    let rest = &src[at + 1..];
    let tag_len = rest.find('$')?;
    let tag = &rest[..tag_len];
    if !tag.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let open = at + 1 + tag_len + 1;
    let close = format!("${tag}$");
    let end = src[open..].find(&close)? + open;
    Some(end + close.len())
}

// ---- emitting -------------------------------------------------------------

/// A line being built and the lines already behind it.
#[derive(Default)]
struct Out {
    lines: Vec<String>,
    line: String,
    /// Whether `line` holds anything past its indentation. Without this a
    /// break asked for twice in a row would leave an empty line behind, which
    /// is exactly what happens where two rules agree.
    filled: bool,
    /// How far the line being built is indented. Remembered rather than
    /// recomputed, because a comment interrupting a select list has to land
    /// where the list is, and depth alone cannot say where that was.
    indent: usize,
}

impl Out {
    fn newline(&mut self, indent: usize) {
        if self.filled {
            self.lines.push(std::mem::take(&mut self.line));
        }
        self.line.clear();
        self.line.push_str(&INDENT.repeat(indent));
        self.filled = false;
        self.indent = indent;
    }

    fn blank(&mut self) {
        if self.filled {
            self.lines.push(std::mem::take(&mut self.line));
            self.filled = false;
        }
        self.line.clear();
        self.indent = 0;
        if self.lines.last().is_some_and(|line| !line.is_empty()) {
            self.lines.push(String::new());
        }
    }

    fn push(&mut self, text: &str, tight: bool) {
        if self.filled && !tight {
            self.line.push(' ');
        }
        self.line.push_str(text);
        self.filled = true;
    }

    fn finish(mut self) -> String {
        if self.filled {
            self.lines.push(self.line);
        }
        self.lines.join("\n").trim_end().to_string()
    }
}

/// Where a `(` puts its contents.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Paren {
    /// On the same line, however long it gets. The default: a parenthesis is
    /// one expression and breaking it up hides that.
    Inline,
    /// One per line — the column list of a `create table`, which is a list of
    /// definitions and is unreadable as a paragraph.
    Expanded,
}

fn emit(tokens: &[Token]) -> String {
    let mut out = Out::default();
    let mut parens: Vec<Paren> = Vec::new();
    // The first word of the statement being written, which is the only thing
    // that says whether a `(` is a definition list or an expression.
    let mut statement: Option<&str> = None;
    let mut creating_table = false;
    // The last word seen, for the handful of two-word clause heads.
    let mut previous: Option<&str> = None;
    // Where the select list starts, when it is worth putting on lines of its
    // own. See `select_list`.
    let mut list_at: Option<usize> = None;
    // A line break owed to the token just written — the comma at the end of a
    // select item, the semicolon at the end of a statement. Owed rather than
    // taken, because a `--` comment that trailed that same source line has to
    // land before the break rather than after it, and only the next token
    // knows whether there is one.
    let mut owed: Option<usize> = None;
    let mut owed_blank = false;

    for (ix, token) in tokens.iter().enumerate() {
        let depth = parens.len();
        let expanded = parens.last() == Some(&Paren::Expanded);
        let trailing_comment = token.kind == Kind::LineComment && !token.broke;

        if !trailing_comment {
            match (owed_blank, owed.take()) {
                (true, _) => out.blank(),
                (false, Some(indent)) => out.newline(indent),
                (false, None) => {}
            }
            owed_blank = false;
        }

        // An author's blank line, kept — but only between statements, where it
        // is a paragraph break rather than an accident of typing.
        if token.paragraph && depth == 0 && statement.is_none() {
            out.blank();
        }

        // The first column of a multi-column select list, which starts the
        // column-per-line block the `select` above it announced.
        if list_at == Some(ix) {
            out.newline(1);
            list_at = None;
        }

        match token.kind {
            Kind::LineComment => {
                // On its own line if that is where it was, and always owing a
                // break: everything after `--` is comment, so a token that
                // followed it on one line would be swallowed.
                if token.broke {
                    out.newline(out.indent);
                }
                out.push(&token.text, false);
                owed = Some(owed.unwrap_or(out.indent));
            }
            Kind::BlockComment => out.push(&token.text, false),
            Kind::Word => {
                let word = token.lower.as_str();
                if depth == 0 {
                    if clause_head(word, previous) || on_conflict(tokens, ix) {
                        out.newline(0);
                    } else if joins(word, previous, tokens, ix) {
                        out.newline(1);
                    } else if matches!(word, "and" | "or") {
                        out.newline(1);
                    }
                }
                out.push(&token.text, tight_before(tokens, ix));
                if word == "select" && depth == 0 {
                    list_at = select_list(tokens, ix);
                }
                if statement.is_none() {
                    statement = Some(word);
                }
                creating_table |= statement == Some("create") && word == "table";
                previous = Some(word);
            }
            Kind::Atom => out.push(&token.text, tight_before(tokens, ix)),
            Kind::Punct => match token.text.as_str() {
                "(" => {
                    // A definition list, and only when it is *the* one: the
                    // `(id)` of a `references` clause inside it is still an
                    // expression.
                    let kind = match creating_table && depth == 0 {
                        true => Paren::Expanded,
                        false => Paren::Inline,
                    };
                    // `count(*)` is tight and `create table t (…)` is not: one
                    // is a call and the other is the table's body.
                    out.push("(", kind == Paren::Inline && tight_before(tokens, ix));
                    parens.push(kind);
                    if kind == Paren::Expanded {
                        out.newline(1);
                    }
                }
                ")" => {
                    let kind = parens.pop().unwrap_or(Paren::Inline);
                    if kind == Paren::Expanded {
                        out.newline(0);
                    }
                    out.push(")", true);
                    previous = None;
                }
                "," => {
                    out.push(",", true);
                    if depth == 0 || expanded {
                        owed = Some(1);
                    }
                }
                ";" => {
                    out.push(";", true);
                    // Statements are separated by a blank line rather than
                    // packed: a script is a sequence of things that each
                    // succeed or fail on their own, and it should look like it.
                    if tokens.get(ix + 1).is_some() {
                        owed_blank = true;
                    }
                    parens.clear();
                    statement = None;
                    creating_table = false;
                    previous = None;
                }
                _ => {
                    out.push(&token.text, tight_before(tokens, ix));
                    previous = None;
                }
            },
        }
    }
    out.finish()
}

/// Where the select list starting after `ix` begins, if it has more than one
/// item.
///
/// One column stays where it is: `select 1` and `select *` are complete
/// thoughts and a line break in the middle of one is a line break in the middle
/// of a sentence. Two or more read as a list, and a list reads down.
///
/// The answer is an index rather than a yes, because what follows `select` may
/// be `distinct`, or `distinct on (tenant_id)`, and the break belongs after all
/// of that — before the first thing that is actually a column.
fn select_list(tokens: &[Token], ix: usize) -> Option<usize> {
    let mut start = ix + 1;
    if tokens.get(start).map(|t| t.lower.as_str()) == Some("distinct") {
        start += 1;
        if tokens.get(start).map(|t| t.lower.as_str()) == Some("on") {
            start += 1;
            let mut depth = 0;
            while let Some(token) = tokens.get(start) {
                match token.text.as_str() {
                    "(" => depth += 1,
                    ")" => {
                        depth -= 1;
                        start += 1;
                        if depth == 0 {
                            break;
                        }
                        continue;
                    }
                    _ => {}
                }
                start += 1;
            }
        }
    } else if tokens.get(start).map(|t| t.lower.as_str()) == Some("all") {
        start += 1;
    }

    let mut depth = 0usize;
    for token in &tokens[start..] {
        match token.text.as_str() {
            "(" => depth += 1,
            ")" => depth = depth.saturating_sub(1),
            "," if depth == 0 => return Some(start),
            ";" if depth == 0 => break,
            _ => {
                // `from` ends the list; so does anything else that would have
                // started a line of its own.
                if depth == 0 && token.kind == Kind::Word && clause_head(&token.lower, None) {
                    break;
                }
            }
        }
    }
    None
}

/// Whether `word` begins a clause and therefore a line.
///
/// The pairs are the two-word heads: `delete from` and `do update` are one
/// clause each, and breaking between their words would put `from` at the
/// margin under a `delete` that says nothing on its own.
fn clause_head(word: &str, previous: Option<&str>) -> bool {
    let head = matches!(
        word,
        "select"
            | "from"
            | "where"
            | "having"
            | "window"
            | "limit"
            | "offset"
            | "fetch"
            | "union"
            | "intersect"
            | "except"
            | "values"
            | "returning"
            | "update"
            | "set"
            | "delete"
            | "insert"
            | "order"
            | "group"
            | "with"
    );
    if !head {
        return false;
    }
    !matches!(
        (word, previous),
        ("from", Some("delete"))
            | ("update", Some("do"))
            | ("set", Some("on"))
            | ("select", Some("as"))
            | ("insert", Some("do"))
    )
}

/// Whether the `on` at `ix` is the `on conflict` of an upsert.
///
/// Every other `on` in SQL belongs to the join above it and stays on that
/// line. This one is a clause in its own right — the part of an `insert` that
/// says what happens when the row is already there — and burying it at the end
/// of a `values` line hides the half of the statement people actually reread.
fn on_conflict(tokens: &[Token], ix: usize) -> bool {
    tokens[ix].lower == "on"
        && tokens
            .get(ix + 1)
            .is_some_and(|next| next.lower == "conflict")
}

/// Whether `word` starts a join clause, which is the one thing that gets an
/// indented line of its own rather than the margin: a join is subordinate to
/// the `from` above it and reads that way.
fn joins(word: &str, previous: Option<&str>, tokens: &[Token], ix: usize) -> bool {
    if word == "join" {
        // Only if it is not already on a line of its own — `left join` broke
        // at `left`.
        return !matches!(
            previous,
            Some("left" | "right" | "full" | "inner" | "outer" | "cross" | "natural" | "lateral")
        );
    }
    if !matches!(
        word,
        "left" | "right" | "full" | "inner" | "cross" | "natural"
    ) {
        return false;
    }
    // `left` is also a function, and `full` is a plausible column name. What
    // makes it a join is a `join` a word or two later.
    tokens[ix + 1..]
        .iter()
        .filter(|t| t.kind == Kind::Word)
        .take(2)
        .any(|t| t.lower == "join")
}

/// Whether `token` sits against what came before it with no space.
fn tight_before(tokens: &[Token], ix: usize) -> bool {
    let token = &tokens[ix];
    let Some(prior) = ix.checked_sub(1).map(|ix| &tokens[ix]) else {
        return true;
    };
    if matches!(prior.text.as_str(), "(" | "." | "::") {
        return true;
    }
    if matches!(token.text.as_str(), "," | ";" | ")" | "." | "::") {
        return true;
    }
    // `count(*)`, not `count (*)`. A keyword before a parenthesis is the other
    // thing — `in (…)`, `values (…)`, `exists (…)` — and those read as two
    // words because they are.
    if token.text == "(" {
        return match prior.kind {
            Kind::Word => {
                !KEYWORDS.contains(&prior.lower.as_str())
                    && !relation_name(tokens, ix - 1)
                    && prior.lower != "conflict"
            }
            _ => prior.text == ")",
        };
    }
    false
}

/// Whether the word at `ix` is the name of a table rather than of a function.
///
/// Lexically they are the same thing — an identifier with a `(` after it — and
/// the difference only shows in what came before. `insert into users (a, b)`
/// names a table and lists its columns; `count(a)` calls something. Getting
/// this wrong is visible, because a table jammed against its column list reads
/// as a call to a table.
fn relation_name(tokens: &[Token], ix: usize) -> bool {
    let mut at = ix;
    // Past a qualifier: `into public.users (…)` is still a table.
    while at >= 2 && tokens[at - 1].text == "." {
        at -= 2;
    }
    let Some(before) = at.checked_sub(1).map(|ix| &tokens[ix]) else {
        return false;
    };
    matches!(
        before.lower.as_str(),
        "into" | "table" | "update" | "join" | "from" | "only" | "using" | "exists"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_select_reads_down() {
        let out = format("select a, b, c from users where x = 1 and y = 2 order by a limit 10");
        assert_eq!(
            out,
            "select\n    a,\n    b,\n    c\nfrom users\nwhere x = 1\n    and y = 2\norder by a\nlimit 10"
        );
    }

    #[test]
    fn one_column_does_not_earn_a_block() {
        // The column-per-line block exists so a list can be read down. A list
        // of one has nothing to read down, and breaking it just costs a line.
        assert_eq!(format("select * from t"), "select *\nfrom t");
        assert_eq!(format("select a, b from t"), "select\n    a,\n    b\nfrom t");
    }

    #[test]
    fn nothing_but_whitespace_changes() {
        // Case, quoting and aliases are the author's. The only difference
        // between input and output is where the spaces are.
        let out = format("SELECT \"Id\" AS Ident FROM Public.\"Users\"");
        assert_eq!(out, "SELECT \"Id\" AS Ident\nFROM Public.\"Users\"");
    }

    #[test]
    fn a_call_keeps_its_parenthesis() {
        let out = format("select count(*), coalesce(a, b) from t where id in (1, 2)");
        assert_eq!(
            out,
            "select\n    count(*),\n    coalesce(a, b)\nfrom t\nwhere id in (1, 2)"
        );
    }

    #[test]
    fn a_join_is_subordinate_to_its_from() {
        let out = format(
            "select * from orders o left join users u on u.id = o.user_id join items i on i.id = o.item_id",
        );
        assert_eq!(
            out,
            "select *\nfrom orders o\n    left join users u on u.id = o.user_id\n    join items i on i.id = o.item_id"
        );
    }

    #[test]
    fn a_subquery_stays_where_it_is() {
        // Inside parentheses nothing breaks: the parenthesis already said this
        // was one thing.
        let out = format("select * from t where id in (select id from u where x = 1)");
        assert_eq!(
            out,
            "select *\nfrom t\nwhere id in (select id from u where x = 1)"
        );
    }

    #[test]
    fn a_create_table_is_a_list_of_definitions() {
        let out = format("create table t (id bigserial primary key, name text not null)");
        assert_eq!(
            out,
            "create table t (\n    id bigserial primary key,\n    name text not null\n)"
        );
    }

    #[test]
    fn statements_are_separated_by_a_blank_line() {
        let out = format("select 1; select 2;");
        assert_eq!(out, "select 1;\n\nselect 2;");
    }

    #[test]
    fn a_delete_keeps_its_from() {
        assert_eq!(format("delete from t where id = 1"), "delete from t\nwhere id = 1");
    }

    #[test]
    fn comments_stay_where_they_were() {
        let out = format("select a, -- the one that matters\nb from t");
        assert_eq!(
            out,
            "select\n    a, -- the one that matters\n    b\nfrom t"
        );
        let out = format("-- why\nselect a from t");
        assert_eq!(out, "-- why\nselect a\nfrom t");
    }

    #[test]
    fn a_string_is_never_looked_inside() {
        let out = format("select 'a, b -- not a comment' from t");
        assert_eq!(out, "select 'a, b -- not a comment'\nfrom t");
    }

    #[test]
    fn a_dollar_quoted_body_survives_whole() {
        let src = "create function f() returns int as $$ select 1, 2 from t $$ language sql";
        assert!(format(src).contains("$$ select 1, 2 from t $$"));
    }

    #[test]
    fn text_in_the_middle_of_being_typed_is_left_alone() {
        // An unterminated string is not a document to reflow; it is a document
        // one keystroke from being one.
        assert_eq!(format("select 'abc"), "select 'abc");
        assert_eq!(format("select /* abc"), "select /* abc");
    }

    #[test]
    fn formatting_is_stable() {
        // Twice is the same as once, which is the property that makes a format
        // button safe to lean on.
        let src = "select a, b from t left join u on u.id = t.id where a = 1 and b = 2;\ncreate table x (a int, b text);";
        let once = format(src);
        assert_eq!(format(&once), once);
    }

    #[test]
    fn a_table_is_not_a_function_call() {
        // `users (email, name)` names a table and lists its columns; only a
        // call gets to sit against its parenthesis.
        assert_eq!(
            format("insert into users (email, name) values ('a', 'b')"),
            "insert into users (email, name)\nvalues ('a', 'b')"
        );
        assert_eq!(
            format("insert into public.users(a) values (1)"),
            "insert into public.users (a)\nvalues (1)"
        );
    }

    #[test]
    fn an_upsert_shows_both_halves() {
        // `on conflict` is the half of an insert people reread. It does not
        // hide at the end of the `values` line the way a join's `on` does.
        let out = format(
            "insert into t (a) values (1) on conflict (a) do update set b = excluded.b returning id",
        );
        assert_eq!(
            out,
            "insert into t (a)\nvalues (1)\non conflict (a) do update\nset b = excluded.b\nreturning id"
        );
    }

    #[test]
    fn an_empty_document_stays_empty() {
        assert_eq!(format(""), "");
        assert_eq!(format("   \n  "), "   \n  ");
    }
}

