//! Turning selected rows into text for the clipboard.
//!
//! Four formats, because the same rows leave this app for four different
//! places: a spreadsheet, a file, a script, and a pull request. Tab-separated
//! is what a spreadsheet pastes cleanly, so it is what ⌘C gives; the rest are
//! asked for by name from the grid's context menu.
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
}

impl Sheet {
    pub fn new(headers: Vec<String>, kinds: Vec<ValueKind>) -> Self {
        debug_assert_eq!(headers.len(), kinds.len());
        Self {
            headers,
            kinds,
            cells: Vec::new(),
        }
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
        }
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

impl crate::state::Grid {
    /// The selected rows as text, or `None` when nothing is selected.
    ///
    /// Hidden columns are left out: this copies what is on screen, and a column
    /// somebody hid is not. Column order is the result's own — pinning moves a
    /// column to the left of the window, not of the data.
    pub fn copy_text(&self, format: Format) -> Option<String> {
        let rows = self.selected_rows();
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
        (!sheet.is_empty()).then(|| sheet.render(format))
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
