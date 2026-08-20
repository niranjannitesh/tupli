//! Reading a delimited file back into rows.
//!
//! The other half of [`crate::export`], and here rather than anywhere else for
//! one reason: the quoting rule has to be the same rule. A file this app wrote
//! must read back as the rows it wrote, and the only way to be sure of that is
//! for the writer and the reader to sit next to each other and be tested
//! against each other.
//!
//! What the reader can tell that the writer cannot say is null. `a,,b` has an
//! empty field and `a,"",b` has an empty string, and every spreadsheet on the
//! planet agrees about which is which. The writer collapses both to nothing —
//! it has a grid of values and no way to mark one — so a round trip through
//! CSV turns an empty string into a null. That is a property of the format,
//! not a bug here, and the import sheet says so out loud.

/// A file, read.
#[derive(Debug, PartialEq)]
pub struct Table {
    /// Column names if the file had a header row, `col1`… if it did not — so
    /// there is always something to show and something to match on.
    pub headers: Vec<String>,
    /// Row-major, `None` for an empty unquoted field.
    pub rows: Vec<Vec<Option<String>>>,
}

/// Why a file could not be read, in the words its author would want.
///
/// Always with a line number: "some row is short" is not something anybody can
/// act on, and the whole value of refusing a ragged file is that it names the
/// line to go and look at.
#[derive(Debug, PartialEq)]
pub struct Problem {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Line {}: {}", self.line, self.message)
    }
}

/// Which character separates the fields.
///
/// A guess, from the first line only: whichever candidate appears most often
/// outside quotes. Comma when nothing appears at all, because a one-column file
/// has no separator to find and comma is the extension people typed.
pub fn sniff(text: &str) -> char {
    const CANDIDATES: [char; 3] = [',', '\t', ';'];
    let line = text.lines().next().unwrap_or_default();
    let mut best = (',', 0usize);
    for candidate in CANDIDATES {
        let mut count = 0;
        let mut quoted = false;
        for ch in line.chars() {
            match ch {
                '"' => quoted = !quoted,
                c if c == candidate && !quoted => count += 1,
                _ => {}
            }
        }
        if count > best.1 {
            best = (candidate, count);
        }
    }
    best.0
}

/// Read a delimited file.
///
/// Rows must all be the width of the header. Padding a short row would shift
/// every value after the gap into the wrong column and write it to the server
/// without anybody seeing — the one failure an importer must never have — so a
/// ragged file is refused and the line is named.
pub fn read_delimited(text: &str, sep: char, has_headers: bool) -> Result<Table, Problem> {
    let mut records = Vec::new();
    let mut record: Vec<Option<String>> = Vec::new();
    let mut field = String::new();
    // Whether this field has been quoted at any point, which is the whole
    // difference between an empty string and a null.
    let mut was_quoted = false;
    let mut quoted = false;
    let mut line = 1;
    // The line a record started on, which is the one to name: a broken record
    // holding a quoted newline ends somewhere far below where it went wrong.
    let mut record_line = 1;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if quoted {
            match ch {
                // A doubled quote inside a quoted field is one quote. Anything
                // else after a quote ends the field.
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                '\n' => {
                    line += 1;
                    field.push('\n');
                }
                _ => field.push(ch),
            }
            continue;
        }
        match ch {
            '"' => {
                quoted = true;
                was_quoted = true;
            }
            c if c == sep => {
                record.push(finish_field(&mut field, was_quoted));
                was_quoted = false;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                line += 1;
                record.push(finish_field(&mut field, was_quoted));
                was_quoted = false;
                // A blank line is nothing, not a row of one null. Files end
                // with one, and so does every editor that touches them.
                if record.len() > 1 || record[0].is_some() {
                    records.push(std::mem::take(&mut record));
                } else {
                    record.clear();
                }
                record_line = line;
            }
            _ => field.push(ch),
        }
    }
    if quoted {
        return Err(Problem {
            line: record_line,
            message: "a quoted value is never closed — the file ends inside it.".into(),
        });
    }
    // A file with no newline at the end still has a last row in hand.
    if !field.is_empty() || was_quoted || !record.is_empty() {
        record.push(finish_field(&mut field, was_quoted));
        if record.len() > 1 || record[0].is_some() {
            records.push(record);
        }
    }

    if records.is_empty() {
        return Err(Problem {
            line: 1,
            message: "there is nothing in this file.".into(),
        });
    }
    let width = records[0].len();
    let headers = match has_headers {
        true => records
            .remove(0)
            .into_iter()
            .enumerate()
            .map(|(ix, name)| match name {
                Some(name) if !name.trim().is_empty() => name.trim().to_string(),
                _ => format!("col{}", ix + 1),
            })
            .collect(),
        false => (1..=width).map(|n| format!("col{n}")).collect(),
    };
    for (ix, record) in records.iter().enumerate() {
        if record.len() != width {
            // Counted from the first record, so the number matches what an
            // editor shows whether or not the first line was a header.
            let at = ix + 1 + usize::from(has_headers);
            return Err(Problem {
                line: at,
                message: format!(
                    "{} value{} here, but {width} column{} above.",
                    record.len(),
                    match record.len() == 1 {
                        true => "",
                        false => "s",
                    },
                    match width == 1 {
                        true => "",
                        false => "s",
                    },
                ),
            });
        }
    }
    Ok(Table {
        headers,
        rows: records,
    })
}

/// An empty field is null unless it was written with quotes around it.
fn finish_field(field: &mut String, was_quoted: bool) -> Option<String> {
    let text = std::mem::take(field);
    match text.is_empty() && !was_quoted {
        true => None,
        false => Some(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str) -> Vec<Vec<Option<String>>> {
        read_delimited(text, ',', true)
            .expect("a readable file")
            .rows
    }

    #[test]
    fn a_header_row_names_the_columns() {
        let table = read_delimited("id,name\n1,Ada\n", ',', true).expect("readable");
        assert_eq!(table.headers, vec!["id", "name"]);
        assert_eq!(table.rows.len(), 1);
    }

    #[test]
    fn a_file_without_a_header_still_has_columns_to_talk_about() {
        let table = read_delimited("1,Ada\n2,Grace\n", ',', false).expect("readable");
        assert_eq!(table.headers, vec!["col1", "col2"]);
        assert_eq!(table.rows.len(), 2);
    }

    #[test]
    fn an_empty_field_is_null_and_an_empty_quoted_one_is_a_string() {
        // The one distinction the format can carry and the writer cannot.
        assert_eq!(rows("a,b\n,\"\"\n"), vec![vec![None, Some(String::new())]]);
    }

    #[test]
    fn a_quoted_field_may_hold_the_separator_a_quote_and_a_newline() {
        let table = read_delimited("a\n\"x,\"\"y\"\"\nz\"\n", ',', true).expect("readable");
        assert_eq!(table.rows, vec![vec![Some("x,\"y\"\nz".to_string())]]);
    }

    #[test]
    fn a_short_row_is_refused_rather_than_padded_into_the_wrong_columns() {
        // Padding would write every value after the gap to the wrong column.
        let problem = read_delimited("a,b,c\n1,2,3\n4,5\n", ',', true).expect_err("ragged");
        assert_eq!(problem.line, 3);
        assert!(problem.message.contains("2 values"), "{problem}");
    }

    #[test]
    fn a_file_that_ends_inside_a_quote_says_where_the_row_began() {
        let problem = read_delimited("a\n\"unterminated\n", ',', true).expect_err("unclosed");
        assert_eq!(problem.line, 2);
    }

    #[test]
    fn windows_line_endings_do_not_become_part_of_the_last_value() {
        assert_eq!(
            rows("a,b\r\n1,2\r\n"),
            vec![vec![Some("1".into()), Some("2".into())]]
        );
    }

    #[test]
    fn a_last_row_with_no_newline_after_it_is_still_a_row() {
        assert_eq!(
            rows("a,b\n1,2"),
            vec![vec![Some("1".into()), Some("2".into())]]
        );
    }

    #[test]
    fn a_blank_line_is_not_a_row_of_one_null() {
        assert_eq!(
            rows("a,b\n1,2\n\n"),
            vec![vec![Some("1".into()), Some("2".into())]]
        );
    }

    #[test]
    fn the_separator_is_guessed_from_the_first_line() {
        assert_eq!(sniff("a\tb\tc\n1\t2\t3\n"), '\t');
        assert_eq!(sniff("a;b;c\n"), ';');
        assert_eq!(sniff("a,b,c\n"), ',');
        // A single column has no separator in it at all.
        assert_eq!(sniff("name\nAda\n"), ',');
    }

    #[test]
    fn a_separator_inside_quotes_is_not_evidence_of_anything() {
        assert_eq!(sniff("\"a;b;c;d\",e\n"), ',');
    }

    #[test]
    fn everything_the_exporter_writes_reads_back_as_what_it_wrote() {
        use crate::export::{Format, Sheet};
        use db::ValueKind;

        let mut sheet = Sheet::new(
            vec!["name".into(), "note".into()],
            vec![ValueKind::Text, ValueKind::Text],
        );
        sheet.push_row([
            Some("O'Hara, Ada".into()),
            Some("said \"hi\"\nthen left".into()),
        ]);
        let table = read_delimited(&sheet.render(Format::Csv), ',', true).expect("readable");
        assert_eq!(
            table.rows,
            vec![vec![
                Some("O'Hara, Ada".to_string()),
                Some("said \"hi\"\nthen left".to_string())
            ]]
        );
    }
}
