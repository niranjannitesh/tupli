//! Turning rows into text — for the clipboard, and for a file.
//!
//! Five formats, because the same rows leave this app for five different
//! places: a spreadsheet, a file, a document, a pull request, and another
//! server. Tab-separated is what a spreadsheet pastes cleanly, so it is what
//! ⌘C gives; the rest are asked for by name from the grid's context menu or
//! from the export sheet.
//!
//! Everything here works from the text the grid is already showing — the same
//! staged-value-wins rule the paint loop uses — so what lands on the clipboard
//! is what was on screen. The one deliberate exception is `bytea`, which the
//! grid abbreviates to sixteen bytes and an ellipsis because a cell is 200px
//! wide; the clipboard has no such excuse and gets all of it.
//!
//! Null is the awkward one. A spreadsheet has no null, so TSV and CSV write an
//! empty field and accept that an empty string and a null arrive looking the
//! same — that ambiguity is inherent to the format, and inventing `\N` or the
//! word NULL would only move it somewhere less expected. JSON, which does have
//! a null, writes one.

use std::fmt::Write as _;

use db::schema::{quote_ident, quote_literal};
use db::ValueKind;

/// What the clipboard gets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    /// Tab-separated. `headers` off is the ⌘C default: the rows are usually
    /// going under a header row that already exists.
    Tsv { headers: bool },
    /// RFC 4180, always with a header line. A CSV whose columns are unnamed is
    /// a file that has to be opened next to the query that made it.
    Csv,
    /// An array of objects, one per row, typed: numbers and booleans unquoted,
    /// nulls null, and a `json` column embedded as the document it is rather
    /// than as a string containing one.
    Json,
    /// A GitHub-flavoured table, for pasting into a review or an issue.
    Markdown,
    /// `insert` statements, for moving rows to another server.
    ///
    /// Which table they name is a fact about the rows and not about the
    /// format, so it is on the [`Sheet`] — see [`Sheet::of`].
    Sql,
}

impl Format {
    /// What a file of this should be called, which is also how the export
    /// sheet labels it.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Tsv { .. } => "tsv",
            Format::Csv => "csv",
            Format::Json => "json",
            Format::Markdown => "md",
            Format::Sql => "sql",
        }
    }
}

/// Rows resolved to their display text, which is all any of the formats need.
///
/// Built by the grid from its selection and thrown away immediately after; it
/// exists so that the formatting can be tested without a window, an entity, or
/// a result set.
pub struct Sheet {
    headers: Vec<String>,
    kinds: Vec<ValueKind>,
    /// Row-major, `None` for null. Always `headers.len()` per row.
    cells: Vec<Option<String>>,
    /// Where the rows came from, for the formats that have to name it. A
    /// placeholder rather than an `Option`, because the alternative is
    /// refusing to write `insert` statements for the result of a join — and
    /// an `insert into table` somebody edits is more useful than nothing.
    table: String,
    /// Whether an `insert` here may claim a generated key. Off by default,
    /// because it is a clause only some servers accept.
    overriding: bool,
}

impl Sheet {
    pub fn new(headers: Vec<String>, kinds: Vec<ValueKind>) -> Self {
        debug_assert_eq!(headers.len(), kinds.len());
        Self {
            headers,
            kinds,
            cells: Vec::new(),
            table: "table".to_string(),
            overriding: false,
        }
    }

    /// Name the table these rows came from, already quoted for the server.
    pub fn of(mut self, table: impl Into<String>) -> Self {
        self.table = table.into();
        self
    }

    /// Write `overriding system value` into the `insert`.
    ///
    /// A table's `id` is usually `generated always as identity`, and Postgres
    /// refuses a literal for one unless the statement says so. An export that
    /// left the clause out would produce a file that reads correctly and then
    /// fails on every batch — the exact failure this whole format exists to
    /// avoid. See [`db::Capabilities::identity_overrides`] for who accepts it.
    pub fn overriding(mut self, yes: bool) -> Self {
        self.overriding = yes;
        self
    }

    pub fn push_row(&mut self, row: impl IntoIterator<Item = Option<String>>) {
        let before = self.cells.len();
        self.cells.extend(row);
        // A short row would silently shift every cell after it into the wrong
        // column, which is the one bug in an exporter nobody notices until the
        // data is somewhere else.
        debug_assert_eq!(self.cells.len() - before, self.headers.len());
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty() || self.headers.is_empty()
    }

    /// How many rows it holds, which is what the caller reports afterwards.
    pub fn len(&self) -> usize {
        match self.headers.is_empty() {
            true => 0,
            false => self.cells.len() / self.headers.len(),
        }
    }

    fn width(&self) -> usize {
        self.headers.len()
    }

    fn rows(&self) -> impl Iterator<Item = &[Option<String>]> {
        self.cells.chunks(self.width().max(1))
    }

    pub fn render(&self, format: Format) -> String {
        match format {
            Format::Tsv { headers } => self.delimited('\t', headers),
            Format::Csv => self.delimited(',', true),
            Format::Json => self.json(),
            Format::Markdown => self.markdown(),
            Format::Sql => self.sql(),
        }
    }

    /// One `insert` per batch of rows rather than one per row: a file of ten
    /// thousand single-row inserts is ten thousand round trips when somebody
    /// pipes it into psql, and the batched form is the same statement to read.
    fn sql(&self) -> String {
        const BATCH: usize = 100;
        let columns: Vec<String> = self.headers.iter().map(|h| quote_ident(h)).collect();
        let mut out = String::with_capacity(self.cells.len() * 12);
        let rows: Vec<&[Option<String>]> = self.rows().collect();
        for batch in rows.chunks(BATCH) {
            let _ = writeln!(
                out,
                "insert into {} ({}){} values",
                self.table,
                columns.join(", "),
                match self.overriding {
                    true => " overriding system value",
                    false => "",
                }
            );
            for (ix, row) in batch.iter().enumerate() {
                out.push_str("  (");
                for (col, cell) in row.iter().enumerate() {
                    if col > 0 {
                        out.push_str(", ");
                    }
                    push_sql_literal(&mut out, cell.as_deref(), self.kinds[col]);
                }
                out.push(')');
                out.push_str(if ix + 1 == batch.len() { ";\n" } else { ",\n" });
            }
        }
        out
    }

    /// Tab- and comma-separated differ only in the delimiter and in whether the
    /// header line is optional, so they are one function.
    fn delimited(&self, sep: char, headers: bool) -> String {
        let mut out = String::with_capacity(self.cells.len() * 12);
        if headers {
            for (ix, name) in self.headers.iter().enumerate() {
                if ix > 0 {
                    out.push(sep);
                }
                push_field(&mut out, name, sep);
            }
            out.push('\n');
        }
        for row in self.rows() {
            for (ix, cell) in row.iter().enumerate() {
                if ix > 0 {
                    out.push(sep);
                }
                push_field(&mut out, cell.as_deref().unwrap_or(""), sep);
            }
            out.push('\n');
        }
        out
    }

    fn json(&self) -> String {
        let names = unique(&self.headers);
        let mut out = String::from("[\n");
        let mut rows = self.rows().peekable();
        while let Some(row) = rows.next() {
            out.push_str("  {\n");
            for (ix, cell) in row.iter().enumerate() {
                out.push_str("    ");
                push_json_string(&mut out, &names[ix]);
                out.push_str(": ");
                push_json_value(&mut out, cell.as_deref(), self.kinds[ix]);
                if ix + 1 < row.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str("  }");
            if rows.peek().is_some() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("]\n");
        out
    }

    fn markdown(&self) -> String {
        let mut out = String::new();
        out.push('|');
        for name in &self.headers {
            let _ = write!(out, " {} |", md_cell(name));
        }
        out.push_str("\n|");
        for kind in &self.kinds {
            // Numbers right, everything else left — the same call the grid
            // makes about alignment, so a pasted table reads like the one it
            // was copied from.
            out.push_str(match kind.is_numeric() {
                true => " ---: |",
                false => " --- |",
            });
        }
        out.push('\n');
        for row in self.rows() {
            out.push('|');
            for cell in row {
                let _ = write!(out, " {} |", md_cell(cell.as_deref().unwrap_or("")));
            }
            out.push('\n');
        }
        out
    }
}

/// One delimited field, quoted only when it has to be.
///
/// The rule is the spreadsheet one, and it is the same for both delimiters: a
/// field holding the delimiter, a quote, or a line break is wrapped in quotes
/// and its own quotes are doubled. Anything else is written as it stands, so an
/// ordinary column of text stays readable in the clipboard.
fn push_field(out: &mut String, text: &str, sep: char) {
    let needs = text.contains([sep, '"', '\n', '\r']);
    if !needs {
        out.push_str(text);
        return;
    }
    out.push('"');
    for ch in text.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
}

/// A JSON string, escaped to the letter of the grammar.
///
/// Hand-written rather than reached for through a serialiser because the only
/// input is a `&str` and the output has to be embeddable in the middle of a
/// document this module is assembling by hand anyway.
fn push_json_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Everything below a space has to be escaped and only these four
            // have a short form; the rest go out as `\u00XX`.
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A cell as JSON, typed by the column it came from.
fn push_json_value(out: &mut String, text: Option<&str>, kind: ValueKind) {
    let Some(text) = text else {
        out.push_str("null");
        return;
    };
    match kind {
        ValueKind::Bool if text == "true" || text == "false" => out.push_str(text),
        // The server's own digits, not a round trip through `f64`: a numeric
        // with thirty significant digits is a number JSON can carry and Rust's
        // float cannot, and re-printing it would quietly change the value.
        // `NaN` and `Infinity` are real in a float column and are not JSON, so
        // they fall through to a string.
        k if k.is_numeric() && is_json_number(text) => out.push_str(text),
        // A `json` column already holds a document. Embedding it keeps the
        // shape; a column that somehow holds something else — a half-typed
        // staged edit — is written as the text it is instead.
        ValueKind::Json if is_json_document(text) => out.push_str(text),
        _ => push_json_string(out, text),
    }
}

/// A cell as a SQL literal, typed by the column it came from.
///
/// Quoting is the safe default and not a fallback: Postgres coerces an
/// unadorned string literal to whatever the column is, so `'1e10'` into a
/// `numeric` is the same row as `1e10`. Numbers and booleans are written bare
/// only because a file somebody reads is worth the trouble.
fn push_sql_literal(out: &mut String, text: Option<&str>, kind: ValueKind) {
    let Some(text) = text else {
        out.push_str("NULL");
        return;
    };
    match kind {
        ValueKind::Bool if text == "true" || text == "false" => out.push_str(text),
        k if k.is_numeric() && is_plain_number(text) => out.push_str(text),
        // Already `\x…` — see `Grid::export_cell`, which is where a `bytea`
        // gets its full value back.
        ValueKind::Bytes if text.starts_with("\\x") => {
            let _ = write!(out, "'{text}'::bytea");
        }
        _ => out.push_str(&quote_literal(text)),
    }
}

/// Digits, one optional point, one optional sign. Deliberately narrower than
/// what Postgres accepts: everything it turns down is merely quoted, and
/// `NaN`, `Infinity` and `1e10` all have to be.
fn is_plain_number(text: &str) -> bool {
    let body = text.strip_prefix('-').unwrap_or(text);
    !body.is_empty()
        && body.chars().all(|c| c.is_ascii_digit() || c == '.')
        && body.chars().filter(|c| *c == '.').count() <= 1
}

/// Whether `text` is a JSON number, by the grammar rather than by parsing: what
/// gets written is `text` itself, so the only question is whether it is legal.
fn is_json_number(text: &str) -> bool {
    let mut chars = text.chars().peekable();
    if chars.peek() == Some(&'-') {
        chars.next();
    }
    // An integer part is required, and a leading zero cannot be followed by
    // more digits.
    let mut digits = 0;
    let leading_zero = chars.peek() == Some(&'0');
    while chars.peek().is_some_and(char::is_ascii_digit) {
        chars.next();
        digits += 1;
    }
    if digits == 0 || (leading_zero && digits > 1) {
        return false;
    }
    if chars.peek() == Some(&'.') {
        chars.next();
        let mut fraction = 0;
        while chars.peek().is_some_and(char::is_ascii_digit) {
            chars.next();
            fraction += 1;
        }
        if fraction == 0 {
            return false;
        }
    }
    if matches!(chars.peek(), Some('e' | 'E')) {
        chars.next();
        if matches!(chars.peek(), Some('+' | '-')) {
            chars.next();
        }
        let mut exponent = 0;
        while chars.peek().is_some_and(char::is_ascii_digit) {
            chars.next();
            exponent += 1;
        }
        if exponent == 0 {
            return false;
        }
    }
    chars.next().is_none()
}

/// Whether `text` can be dropped into a document as-is.
///
/// Only objects and arrays qualify. A bare `"a"` or `12` out of a json column
/// is a valid document too, but embedding those unquoted gains nothing and
/// risks writing something unbalanced from a cell that was never checked.
fn is_json_document(text: &str) -> bool {
    let text = text.trim();
    if !text.starts_with(['{', '[']) {
        return false;
    }
    balanced(text)
}

/// A bracket walk that skips strings, which is enough to know that embedding
/// `text` cannot break the document around it.
fn balanced(text: &str) -> bool {
    let mut stack = Vec::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => loop {
                match chars.next() {
                    Some('\\') => {
                        chars.next();
                    }
                    Some('"') => break,
                    Some(_) => {}
                    None => return false,
                }
            },
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' if stack.pop() != Some(ch) => return false,
            _ => {}
        }
    }
    stack.is_empty()
}

/// A markdown cell: the pipe is the format's only structural character, and a
/// line break inside a cell ends the row.
fn md_cell(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '|' => out.push_str("\\|"),
            '\n' => out.push_str("<br>"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Column names made unique, because `select id, id` is legal SQL and an object
/// with the same key twice loses one of them.
fn unique(headers: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(headers.len());
    for name in headers {
        let mut candidate = name.clone();
        let mut n = 1;
        while out.contains(&candidate) {
            n += 1;
            candidate = format!("{name}_{n}");
        }
        out.push(candidate);
    }
    out
}

/// Which rows an export is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rows {
    /// What the selection covers, or the cursor's row when nothing is
    /// selected. The ⌘C rule.
    Selected,
    /// Everything the pane has. Everything it *has*: a browsed table is one
    /// page of itself, and this cannot reach past what was fetched — which is
    /// why the export sheet says the count out loud.
    All,
}

impl crate::state::Grid {
    /// The selected rows as text, or `None` when nothing is selected.
    pub fn copy_text(&self, format: Format) -> Option<String> {
        let sheet = self.sheet(Rows::Selected)?;
        (!sheet.is_empty()).then(|| sheet.render(format))
    }

    /// The rows resolved to their display text, ready to be rendered in any
    /// format. `None` when there are none to take.
    ///
    /// Hidden columns are left out: this exports what is on screen, and a
    /// column somebody hid is not. Column order is the result's own — pinning
    /// moves a column to the left of the window, not of the data.
    pub fn sheet(&self, which: Rows) -> Option<Sheet> {
        let rows: Vec<usize> = match which {
            Rows::Selected => self.selected_rows(),
            Rows::All => (0..self.row_count()).collect(),
        };
        if rows.is_empty() {
            return None;
        }
        let visible: Vec<usize> = (0..self.data.columns.len())
            .filter(|ix| !self.columns.get(*ix).is_some_and(|c| c.hidden))
            .collect();
        let mut sheet = Sheet::new(
            visible
                .iter()
                .map(|ix| self.data.columns[*ix].meta.name.clone())
                .collect(),
            visible
                .iter()
                .map(|ix| self.data.columns[*ix].meta.kind)
                .collect(),
        );
        for row in rows {
            sheet.push_row(visible.iter().map(|col| self.export_cell(row, *col)));
        }
        Some(sheet)
    }

    /// One cell as the clipboard should have it.
    ///
    /// The same value the grid paints, with one exception: `bytea` is drawn
    /// truncated because the column is narrow, and truncated bytes on a
    /// clipboard are worse than useless — they look like data.
    fn export_cell(&self, row: usize, col: usize) -> Option<String> {
        match self.cell_value(row, col)? {
            db::Value::Null => None,
            db::Value::Bytes(bytes) => {
                Some(format!("\\x{}", db::value::hex_prefix(&bytes, bytes.len())))
            }
            value => Some(value.to_string()),
        }
    }

    /// Put the selection on the clipboard. The ⌘C path and the menu's, both.
    pub fn copy_selection(&self, format: Format, cx: &mut gpui::App) {
        if let Some(text) = self.copy_text(format) {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        }
    }

    /// One cell, unformatted — no header, no quoting, no trailing newline.
    ///
    /// The value on its own, because the reason to copy a single cell is
    /// almost always to paste it into a `where` clause or a search box, and
    /// anything the formats add would have to be deleted again.
    pub fn cell_text(&self, row: usize, col: usize) -> String {
        self.export_cell(row, col).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> Sheet {
        let mut sheet = Sheet::new(
            vec!["id".into(), "name".into(), "settings".into()],
            vec![ValueKind::Int, ValueKind::Text, ValueKind::Json],
        );
        sheet.push_row([
            Some("1".into()),
            Some("Ada".into()),
            Some(r#"{"theme":"dark"}"#.into()),
        ]);
        sheet.push_row([Some("2".into()), None, None]);
        sheet
    }

    #[test]
    fn a_sql_export_names_the_table_and_batches_the_rows() {
        let sql = sheet().of("public.users").render(Format::Sql);
        assert_eq!(
            sql,
            concat!(
                "insert into public.users (id, name, settings) values\n",
                "  (1, 'Ada', '{\"theme\":\"dark\"}'),\n",
                "  (2, NULL, NULL);\n",
            )
        );
    }

    #[test]
    fn a_quote_in_a_value_cannot_end_the_literal_it_is_in() {
        let mut sheet = Sheet::new(vec!["name".into()], vec![ValueKind::Text]);
        sheet.push_row([Some("O'Hara".into())]);
        assert!(sheet.render(Format::Sql).contains("('O''Hara')"));
    }

    #[test]
    fn a_number_that_is_not_plainly_one_is_quoted_rather_than_guessed_at() {
        let mut sheet = Sheet::new(
            vec!["a".into(), "b".into(), "c".into()],
            vec![ValueKind::Decimal, ValueKind::Float, ValueKind::Float],
        );
        let row = ["12.30", "NaN", "1e10"];
        sheet.push_row(row.map(|v| Some(v.into())));
        assert!(sheet.render(Format::Sql).contains("(12.30, 'NaN', '1e10')"));
    }

    #[test]
    fn a_generated_key_is_claimed_rather_than_left_to_the_server() {
        // Without the clause every batch of this file is rejected by the one
        // server the format was written for.
        let mut sheet = Sheet::new(vec!["id".into()], vec![ValueKind::Int]).overriding(true);
        sheet.push_row([Some("1".into())]);
        assert!(sheet
            .render(Format::Sql)
            .starts_with("insert into table (id) overriding system value values"));
    }

    #[test]
    fn a_server_that_would_not_understand_the_clause_is_not_sent_it() {
        let mut sheet = Sheet::new(vec!["id".into()], vec![ValueKind::Int]);
        sheet.push_row([Some("1".into())]);
        assert!(!sheet.render(Format::Sql).contains("overriding"));
    }

    #[test]
    fn every_format_has_a_file_extension_of_its_own() {
        let all = [
            Format::Tsv { headers: true },
            Format::Csv,
            Format::Json,
            Format::Markdown,
            Format::Sql,
        ];
        let mut seen: Vec<&str> = all.iter().map(|f| f.extension()).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), all.len());
    }

    #[test]
    fn tab_separated_is_what_a_spreadsheet_wants() {
        assert_eq!(
            sheet().render(Format::Tsv { headers: false }),
            // The quotes inside the document are what force the field to be
            // quoted; a spreadsheet unwraps it back to the text it was.
            "1\tAda\t\"{\"\"theme\"\":\"\"dark\"\"}\"\n2\t\t\n"
        );
    }

    #[test]
    fn the_header_line_is_optional_for_tsv_and_not_for_csv() {
        let tsv = sheet().render(Format::Tsv { headers: true });
        assert!(tsv.starts_with("id\tname\tsettings\n"));
        let csv = sheet().render(Format::Csv);
        assert!(csv.starts_with("id,name,settings\n"));
    }

    #[test]
    fn a_field_is_quoted_only_when_the_format_forces_it() {
        let mut out = String::new();
        push_field(&mut out, "plain", ',');
        push_field(&mut out, "a,b", ',');
        push_field(&mut out, "say \"hi\"", ',');
        push_field(&mut out, "two\nlines", ',');
        assert_eq!(
            out,
            r#"plain"a,b""say ""hi""""two
lines""#
        );
        // And a comma is nothing to a tab-separated field.
        let mut tsv = String::new();
        push_field(&mut tsv, "a,b", '\t');
        assert_eq!(tsv, "a,b");
    }

    #[test]
    fn json_types_its_values_instead_of_quoting_everything() {
        let out = sheet().render(Format::Json);
        assert!(out.contains(r#""id": 1"#), "{out}");
        assert!(out.contains(r#""name": "Ada""#), "{out}");
        // The document is embedded, not stringified.
        assert!(out.contains(r#""settings": {"theme":"dark"}"#), "{out}");
        assert!(out.contains(r#""name": null"#), "{out}");
    }

    #[test]
    fn a_numeric_keeps_every_digit_the_server_sent() {
        let mut out = String::new();
        // Thirty digits: no float could carry this, and it is a legal JSON
        // number, so it goes out exactly as it came in.
        push_json_value(
            &mut out,
            Some("100000000000000000000000000001"),
            ValueKind::Decimal,
        );
        out.push(' ');
        // Trailing zeros are part of a numeric's scale, not noise to be tidied.
        push_json_value(&mut out, Some("7065.0000"), ValueKind::Decimal);
        assert_eq!(out, "100000000000000000000000000001 7065.0000");
    }

    #[test]
    fn the_values_a_float_column_holds_that_json_does_not_become_strings() {
        for text in ["NaN", "Infinity", "-Infinity"] {
            let mut out = String::new();
            push_json_value(&mut out, Some(text), ValueKind::Float);
            assert_eq!(out, format!("\"{text}\""));
        }
    }

    #[test]
    fn what_is_not_a_json_number_is_not_written_as_one() {
        for good in ["0", "-1", "1.5", "1e10", "1.5E-3", "7065.0000"] {
            assert!(is_json_number(good), "{good}");
        }
        for bad in ["", "-", "01", "1.", ".5", "1e", "0x1", "1 ", "NaN", "+1"] {
            assert!(!is_json_number(bad), "{bad}");
        }
    }

    #[test]
    fn an_unbalanced_document_is_quoted_rather_than_embedded() {
        assert!(is_json_document(r#"{"a":[1,2]}"#));
        // A brace inside a string is not structure.
        assert!(is_json_document(r#"{"a":"}"}"#));
        assert!(!is_json_document(r#"{"a":"#));
        assert!(!is_json_document("{]"));
        // Scalars are documents but are not embedded.
        assert!(!is_json_document("12"));
    }

    #[test]
    fn a_string_is_escaped_to_the_letter_of_the_grammar() {
        let mut out = String::new();
        push_json_string(&mut out, "a\"b\\c\nd\te\u{1}");
        assert_eq!(out, r#""a\"b\\c\nd\te\u0001""#);
    }

    #[test]
    fn markdown_aligns_numbers_right_and_escapes_the_only_character_it_has() {
        let mut sheet = Sheet::new(
            vec!["n".into(), "text".into()],
            vec![ValueKind::Int, ValueKind::Text],
        );
        sheet.push_row([Some("1".into()), Some("a|b".into())]);
        let out = sheet.render(Format::Markdown);
        assert_eq!(out, "| n | text |\n| ---: | --- |\n| 1 | a\\|b |\n");
    }

    #[test]
    fn two_columns_of_the_same_name_still_make_an_object_with_two_keys() {
        let mut sheet = Sheet::new(
            vec!["id".into(), "id".into()],
            vec![ValueKind::Int, ValueKind::Int],
        );
        sheet.push_row([Some("1".into()), Some("2".into())]);
        let out = sheet.render(Format::Json);
        assert!(out.contains(r#""id": 1"#), "{out}");
        assert!(out.contains(r#""id_2": 2"#), "{out}");
    }
}

/// The copy path with a real grid under it: selection, hidden columns, and the
/// staging rule. What [`Sheet`] cannot be asked, because it is handed its cells
/// already resolved.
#[cfg(test)]
mod grid_tests {
    use db::{Column, ColumnData, ColumnMeta, NullMask, ResultSet, TextColumnBuilder, ValueKind};
    use gpui::{AppContext as _, Entity, TestAppContext};

    use super::Format;
    use crate::state::Grid;

    fn users() -> ResultSet {
        let mut ids = NullMask::with_capacity(4);
        let mut values = Vec::new();
        for row in 0..4 {
            ids.push(false, row);
            values.push(row as i64 + 1);
        }
        let mut emails = TextColumnBuilder::new();
        for row in 0..4 {
            emails.push(Some(&format!("user{}@example.com", row + 1)));
        }
        ResultSet::new(vec![
            Column {
                meta: ColumnMeta::new("id".to_string(), ValueKind::Int, "int8")
                    .pk()
                    .not_null(),
                nulls: ids,
                data: ColumnData::I64(values),
            },
            emails.finish(ColumnMeta::new(
                "email".to_string(),
                ValueKind::Text,
                "text",
            )),
        ])
    }

    fn grid(cx: &mut TestAppContext) -> Entity<Grid> {
        cx.update(|cx| ui::Theme::set_global(ui::Theme::of(ui::Appearance::Dark), cx));
        cx.update(|cx| cx.new(|cx| Grid::new(users(), cx)))
    }

    /// ⌘C with no selection copies the row the cursor is on. There is always a
    /// cursor, so ⌘C never does nothing.
    #[gpui::test]
    fn the_cursor_is_a_selection_of_one(cx: &mut TestAppContext) {
        let grid = grid(cx);
        grid.update(cx, |grid, cx| {
            grid.set_cursor(2, 0, false, cx);
            assert_eq!(
                grid.copy_text(Format::Tsv { headers: false }),
                Some("3\tuser3@example.com\n".to_string())
            );
        });
    }

    #[gpui::test]
    fn a_shift_selection_copies_every_row_it_covers(cx: &mut TestAppContext) {
        let grid = grid(cx);
        grid.update(cx, |grid, cx| {
            grid.set_cursor(1, 0, false, cx);
            grid.set_cursor(3, 0, true, cx);
            assert_eq!(
                grid.copy_text(Format::Tsv { headers: true }),
                Some(
                    "id\temail\n2\tuser2@example.com\n3\tuser3@example.com\n4\tuser4@example.com\n"
                        .to_string()
                )
            );
        });
    }

    /// What is copied is what is on screen, so a hidden column is not in it.
    #[gpui::test]
    fn a_hidden_column_is_not_copied(cx: &mut TestAppContext) {
        let grid = grid(cx);
        grid.update(cx, |grid, cx| {
            grid.columns[1].hidden = true;
            grid.rebuild_offsets();
            grid.set_cursor(0, 0, false, cx);
            assert_eq!(grid.copy_text(Format::Csv), Some("id\n1\n".to_string()));
        });
    }

    /// An edit that has been staged but not sent is still what the cell shows,
    /// and the clipboard follows the screen rather than the result set.
    #[gpui::test]
    fn a_staged_edit_is_what_gets_copied(cx: &mut TestAppContext) {
        let grid = grid(cx);
        grid.update(cx, |grid, cx| {
            grid.set_editable(true, cx);
            grid.set_cell(
                0,
                1,
                db::Value::text(ValueKind::Text, "moved@example.com"),
                cx,
            );
            grid.set_cursor(0, 0, false, cx);
            assert_eq!(
                grid.copy_text(Format::Tsv { headers: false }),
                Some("1\tmoved@example.com\n".to_string())
            );
        });
    }
}
