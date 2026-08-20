//! Getting rows out of a file and into a table.
//!
//! The mirror of [`crate::export`], and deliberately not its symmetry. An
//! export cannot go wrong in a way that matters — the worst case is a file
//! nobody wanted — while an import writes to a server, and the failure it must
//! never have is silently putting the right values in the wrong columns. So
//! everything here is about being sure before the write: the file is read and
//! shown before anything is sent, the column matching is a decision on screen
//! rather than an inference, and a file that does not line up is refused by
//! line number instead of padded into place.
//!
//! It is one transaction. Ten thousand rows either all land or none do, which
//! is the only outcome anybody can act on — a half-loaded table is worse than
//! no table, because it looks like a loaded one.

use std::path::{Path, PathBuf};

use db::{ColumnMeta, Value};
use gpui::{
    px, AppContext as _, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window,
};
use grid::import::{read_delimited, sniff, Problem, Table};
use ui::{
    h_flex, v_flex, ActiveTheme, Button, ButtonSize, ButtonVariant, FormRow, Label, LabelSize,
    Notice, NoticeTone, Segmented, Sheet as SheetView, Switch,
};

use crate::workspace::{count_of, Workspace};

/// The separators offered, and the character each one is.
///
/// Three, because they are the three a spreadsheet exports: a European locale
/// writes `;` because its decimal point is a comma, and a file that came out of
/// a terminal is usually tabs.
const SEPARATORS: [(&str, char); 3] = [("Comma", ','), ("Tab", '\t'), ("Semicolon", ';')];

/// Rows of the file shown before it is sent. Enough to recognise the file and
/// to catch a column that landed one place over; not so many that the sheet
/// becomes the grid it is about to write into.
const PREVIEW_ROWS: usize = 5;

/// How a file's columns find the table's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Matching {
    /// Header text to column name, ignoring case and surrounding space.
    ByName,
    /// First column to first column. What a file with no header row has.
    ByOrder,
}

/// What the import would write, worked out from the parse and the matching.
#[derive(Debug)]
pub struct Plan {
    /// Table columns being written, in the order the values are in.
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// File headers with no column to go to. Not an error — a spreadsheet
    /// carries notes and totals that were never meant for the server — but
    /// said out loud, because the other reading of it is a typo.
    pub ignored: Vec<String>,
}

/// Match the file to the table and convert every cell.
///
/// The result is in the *file's* column order, not the table's — the statement
/// names its columns, so the server is indifferent, and the preview is read
/// against the file.
///
/// A free function so the whole decision can be tested without a window: this
/// is the part where getting it wrong writes the wrong data.
pub fn plan(columns: &[ColumnMeta], table: &Table, matching: Matching) -> Plan {
    let pairs: Vec<(usize, usize)> = match matching {
        Matching::ByName => table
            .headers
            .iter()
            .enumerate()
            .filter_map(|(from, header)| {
                let header = header.trim();
                columns
                    .iter()
                    .position(|column| column.name.eq_ignore_ascii_case(header))
                    .map(|to| (to, from))
            })
            .collect(),
        Matching::ByOrder => (0..table.headers.len().min(columns.len()))
            .map(|index| (index, index))
            .collect(),
    };
    let ignored = table
        .headers
        .iter()
        .enumerate()
        .filter(|(from, _)| !pairs.iter().any(|(_, taken)| taken == from))
        .map(|(_, header)| header.clone())
        .collect();

    let rows = table
        .rows
        .iter()
        .map(|row| {
            pairs
                .iter()
                .map(|(to, from)| match row.get(*from).and_then(Option::as_ref) {
                    // An empty unquoted field is a null, which is the one thing
                    // the file can say that the exporter could not. See
                    // `grid::import`.
                    None => Value::Null,
                    Some(text) => Value::parse(columns[*to].kind, text),
                })
                .collect()
        })
        .collect();

    Plan {
        columns: pairs
            .iter()
            .map(|(to, _)| columns[*to].name.clone())
            .collect(),
        rows,
        ignored,
    }
}

pub enum ImportEvent {
    Dismissed,
    Confirmed(Plan),
}

pub struct ImportSheet {
    path: PathBuf,
    /// The file, whole. Changing the delimiter re-reads it from here rather
    /// than from disk: the file may have been rewritten in the meantime, and
    /// the rows on screen have to be the rows that get sent.
    text: String,
    /// Where the rows are going, qualified.
    target: SharedString,
    /// The table's columns, which is what the file is being matched against.
    columns: Vec<ColumnMeta>,
    separator: usize,
    headers: bool,
    matching: Matching,
    parsed: Result<Table, Problem>,
}

impl EventEmitter<ImportEvent> for ImportSheet {}

impl ImportSheet {
    pub fn new(
        path: PathBuf,
        text: String,
        target: impl Into<SharedString>,
        columns: Vec<ColumnMeta>,
    ) -> Self {
        let separator = SEPARATORS
            .iter()
            .position(|(_, ch)| *ch == sniff(&text))
            .unwrap_or(0);
        let mut sheet = Self {
            path,
            text,
            target: target.into(),
            columns,
            separator,
            // Nearly every file has one, and a file that does not shows its
            // first row of data as the headers — which is obvious on sight and
            // one switch away from fixed.
            headers: true,
            matching: Matching::ByName,
            parsed: Err(Problem {
                line: 1,
                message: String::new(),
            }),
        };
        sheet.reread();
        // By name unless nothing matches by name. A file whose headers are all
        // strangers is either a file with no header row or one from somewhere
        // else entirely; either way position is the only thing left to go on,
        // and offering a matching that maps nothing is offering nothing.
        if sheet.plan().is_some_and(|plan| plan.columns.is_empty()) {
            sheet.matching = Matching::ByOrder;
        }
        sheet
    }

    fn reread(&mut self) {
        self.parsed = read_delimited(&self.text, SEPARATORS[self.separator].1, self.headers);
    }

    fn plan(&self) -> Option<Plan> {
        let table = self.parsed.as_ref().ok()?;
        Some(plan(&self.columns, table, self.matching))
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(plan) = self.plan() {
            cx.emit(ImportEvent::Confirmed(plan));
        }
    }

    fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    /// The first few rows as they will be sent, headed by the column each one
    /// is going into — not by the header the file had.
    ///
    /// That substitution is the whole point of the preview. A file's own header
    /// row tells you what the file thinks; what anybody actually needs to check
    /// is where those values are about to land.
    fn preview(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        let plan = self.plan()?;
        if plan.columns.is_empty() {
            return None;
        }
        let c = cx.colors().clone();
        let ty = cx.typography().clone();
        let width = px(120.);

        let head = h_flex().gap(px(8.)).children(
            plan.columns
                .iter()
                .map(|name| {
                    Label::new(name.clone())
                        .size(LabelSize::Small)
                        .color(ui::IconColor::Subtle)
                        .into_any_element()
                })
                .map(|label| {
                    gpui::div()
                        .w(width)
                        .flex_none()
                        .overflow_hidden()
                        .child(label)
                }),
        );

        let rows = plan.rows.iter().take(PREVIEW_ROWS).map(|row| {
            h_flex().gap(px(8.)).children(row.iter().map(|value| {
                let (text, muted) = match value {
                    Value::Null => ("NULL".to_string(), true),
                    other => (cell_text(other), false),
                };
                gpui::div()
                    .w(width)
                    .flex_none()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(match muted {
                        true => c.text_subtle,
                        false => c.text,
                    })
                    .child(text)
            }))
        });

        Some(
            v_flex()
                .id("import-preview")
                .overflow_x_scroll()
                .gap(px(2.))
                .p(px(8.))
                .rounded(px(6.))
                .bg(c.surface)
                .font_family(ty.mono_family.clone())
                .text_size(ty.ui_size_sm)
                .child(head)
                .children(rows),
        )
    }
}

impl Render for ImportSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let plan = self.plan();
        let matched = plan.as_ref().map_or(0, |plan| plan.columns.len());
        let ignored = plan.as_ref().map_or(0, |plan| plan.ignored.len());
        let count = plan.as_ref().map_or(0, |plan| plan.rows.len());

        SheetView::new("import-sheet", "Import Rows")
            .subtitle(format!("{} → {}", self.file_name(), self.target))
            .width(px(640.))
            .on_dismiss(cx.listener(|_, _, _, cx| cx.emit(ImportEvent::Dismissed)))
            .child(
                FormRow::new("Delimiter").child(
                    Segmented::new("import-separator", SEPARATORS.map(|(label, _)| label))
                        .selected(self.separator)
                        .on_select({
                            let sheet = cx.entity();
                            move |index, _, cx| {
                                sheet.update(cx, |this, cx| {
                                    this.separator = index;
                                    this.reread();
                                    cx.notify();
                                })
                            }
                        }),
                ),
            )
            .child(
                FormRow::new("Header")
                    .hint("The first row names the columns rather than being one of them.")
                    .child(Switch::new("import-headers", self.headers).on_toggle({
                        let sheet = cx.entity();
                        move |on, _, cx| {
                            sheet.update(cx, |this, cx| {
                                this.headers = on;
                                this.reread();
                                cx.notify();
                            })
                        }
                    })),
            )
            .child(
                FormRow::new("Columns")
                    .child(
                        Segmented::new("import-matching", ["By name", "By order"])
                            .selected(match self.matching {
                                Matching::ByName => 0,
                                Matching::ByOrder => 1,
                            })
                            .on_select({
                                let sheet = cx.entity();
                                move |index, _, cx| {
                                    sheet.update(cx, |this, cx| {
                                        this.matching = match index {
                                            0 => Matching::ByName,
                                            _ => Matching::ByOrder,
                                        };
                                        cx.notify();
                                    })
                                }
                            }),
                    )
                    // Nothing while the file will not parse: there is no
                    // matching to report, and "nothing matches" under a notice
                    // naming line 4 is a second, wrong diagnosis of one fault.
                    .hint(match (self.parsed.is_ok(), matched, ignored) {
                        (false, _, _) => String::new(),
                        (_, 0, _) => "Nothing in this file matches a column of this table.".into(),
                        (_, matched, 0) => format!(
                            "{} into {}.",
                            count_of(matched, "column"),
                            self.target.clone()
                        ),
                        (_, matched, ignored) => format!(
                            "{} into {}; {} ignored.",
                            count_of(matched, "column"),
                            self.target.clone(),
                            count_of(ignored, "column"),
                        ),
                    }),
            )
            .children(self.preview(cx))
            // A parse failure is the file's own line number, which is the only
            // thing anybody can act on: the fix is in an editor, not here.
            .children(self.parsed.as_ref().err().map(|problem| {
                Notice::new(NoticeTone::Danger, problem.to_string()).detail(
                    "Every row has to have as many values as the header. Padding a short \
                     one would put its values in the wrong columns.",
                )
            }))
            // Not a warning about the write — it is one transaction and it
            // either lands or does not — but about what is *not* in the file.
            .children((matched > 0 && matched < self.columns.len()).then(|| {
                Notice::new(
                    NoticeTone::Info,
                    format!(
                        "{} of this table take their default.",
                        count_of(self.columns.len() - matched, "column")
                    ),
                )
            }))
            .footer_start(
                Label::new(match count {
                    0 => "Nothing to import".to_string(),
                    n => format!("{} to insert", count_of(n, "row")),
                })
                .size(LabelSize::Small)
                .color(ui::IconColor::Subtle),
            )
            .footer_end(
                Button::new("cancel", "Cancel")
                    .size(ButtonSize::Small)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ImportEvent::Dismissed))),
            )
            .footer_end(
                Button::new("import", "Import")
                    .variant(ButtonVariant::Accent)
                    .size(ButtonSize::Small)
                    .disabled(count == 0 || matched == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
            )
    }
}

impl Workspace {
    /// Ask for a file, read it, and open the sheet on it.
    ///
    /// Only from a tab that is browsing a table. A query result is not a place
    /// rows can be put — `select a, count(*) …` has no table behind it and no
    /// answer to "into what" — so the command is off rather than guessing.
    pub fn open_import(&mut self, cx: &mut Context<Self>) {
        if self.import_target(cx).is_none() {
            return;
        }
        let prompt = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn(async move |this, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                    Some(path) => path,
                    None => return,
                },
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    log::warn!("the open panel did not open: {error:#}");
                    return;
                }
                Err(_) => return,
            };
            let _ = this.update(cx, |this, cx| this.import_file(path, cx));
        })
        .detach();
    }

    /// The same sheet with the panel skipped, for a caller that already has the
    /// file in hand.
    pub fn import_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some((target, columns)) = self.import_target(cx) else {
            return;
        };
        let text = match std::fs::read(&path) {
            // Lossy rather than refused: a stray byte in a note column is not a
            // reason to reject a file, and the alternative is an error message
            // about encodings for a file that opens fine everywhere else.
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => {
                self.report_import(format!("{}: {error}", file_name(&path)), false, cx);
                return;
            }
        };
        let sheet = cx.new(|_| ImportSheet::new(path, text, target, columns));
        cx.subscribe(&sheet, Self::on_import_event).detach();
        self.import_sheet = Some(sheet);
        cx.notify();
    }

    /// Press the sheet's Import button from outside it — a headless render,
    /// which has no pointer to press it with.
    pub fn confirm_import(&mut self, cx: &mut Context<Self>) {
        if let Some(sheet) = self.import_sheet.clone() {
            sheet.update(cx, |sheet, cx| sheet.confirm(cx));
        }
    }

    fn on_import_event(
        &mut self,
        sheet: Entity<ImportSheet>,
        event: &ImportEvent,
        cx: &mut Context<Self>,
    ) {
        // Off the sheet before it goes: the log entry names the file, and by
        // the time the transaction answers there is no sheet left to ask.
        let from = sheet.read(cx).file_name();
        self.import_sheet = None;
        cx.notify();
        if let ImportEvent::Confirmed(plan) = event {
            self.run_import(plan, from, cx);
        }
    }

    /// Send the rows, as one transaction.
    ///
    /// Through `Session::apply`, which is the same door the grid's Commit uses:
    /// the read-only guard, the rollback, and the message the Messages tab ends
    /// up with are all already right there, and an import that had its own copy
    /// of them would be an import that could disagree with them.
    fn run_import(&mut self, plan: &Plan, from: String, cx: &mut Context<Self>) {
        let Some(relation) = self.pane().active().and_then(|tab| tab.relation.clone()) else {
            return;
        };
        let statements =
            sqlgen::bulk::inserts(&relation, &plan.columns, &plan.rows, sqlgen::bulk::BATCH);
        if statements.is_empty() {
            return;
        }
        let counts = sqlgen::Counts {
            inserts: plan.rows.len(),
            ..Default::default()
        };
        self.import_note = Some(format!("{} from {from}", count_of(plan.rows.len(), "row")).into());
        if let Some(session) = self.session.clone() {
            session.update(cx, |session, cx| session.apply(statements, counts, cx));
        }
        cx.notify();
    }

    /// Where an import would go: the table the tab is browsing, and its
    /// columns.
    fn import_target(&self, cx: &gpui::App) -> Option<(String, Vec<ColumnMeta>)> {
        if !self.capabilities(cx).editable_rows {
            return None;
        }
        let relation = self.pane().active().and_then(|tab| tab.relation.clone())?;
        let columns: Vec<ColumnMeta> = self
            .pane()
            .grid
            .read(cx)
            .data()
            .columns
            .iter()
            .map(|column| column.meta.clone())
            .collect();
        match columns.is_empty() {
            true => None,
            false => Some((relation.qualified(), columns)),
        }
    }

    /// Whether the Import command is worth offering at all.
    pub fn can_import(&self, cx: &gpui::App) -> bool {
        self.import_target(cx).is_some()
    }

    fn report_import(&mut self, text: String, ok: bool, cx: &mut Context<Self>) {
        use crate::results::{MessageTone, RunMessage};
        self.messages.push(RunMessage {
            at_ms: crate::workspace::now_ms(),
            sql: "import".into(),
            elapsed: std::time::Duration::ZERO,
            tone: match ok {
                true => MessageTone::Ok,
                false => MessageTone::Failed,
            },
            text: text.into(),
            notices: Vec::new(),
        });
        if !ok {
            self.select_results_tab(crate::results::ResultsTab::Messages, cx);
        }
        cx.notify();
    }
}

/// One cell of the preview.
///
/// The value as it will be sent, not as SQL would spell it: a preview full of
/// quoted strings would be a preview of `sqlgen`'s output rather than of the
/// file, and the question being asked is whether the file lined up.
fn cell_text(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => db::value::format_f64(*f),
        Value::Text { text, .. } => text.to_string(),
        Value::Bytes(bytes) => format!("{} bytes", bytes.len()),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::ValueKind;

    fn columns() -> Vec<ColumnMeta> {
        vec![
            ColumnMeta::new("id", ValueKind::Int, "int8"),
            ColumnMeta::new("email", ValueKind::Text, "text"),
            ColumnMeta::new("is_active", ValueKind::Bool, "bool"),
        ]
    }

    fn table(text: &str) -> Table {
        read_delimited(text, ',', true).expect("a readable file")
    }

    #[test]
    fn a_header_finds_its_column_whatever_case_it_is_in() {
        let plan = plan(
            &columns(),
            &table("Email,ID\na@b.com,7\n"),
            Matching::ByName,
        );
        // In the file's order, which is what the preview is read against: the
        // insert names its columns, so the server does not care either way.
        assert_eq!(plan.columns, vec!["email", "id"]);
        assert_eq!(plan.rows[0][0], Value::text(ValueKind::Text, "a@b.com"));
        assert_eq!(plan.rows[0][1], Value::Int(7));
    }

    #[test]
    fn a_header_with_nowhere_to_go_is_named_rather_than_dropped_quietly() {
        let plan = plan(
            &columns(),
            &table("email,nickname\na@b.com,ada\n"),
            Matching::ByName,
        );
        assert_eq!(plan.columns, vec!["email"]);
        assert_eq!(plan.ignored, vec!["nickname"]);
    }

    #[test]
    fn matching_by_order_takes_the_columns_as_they_come() {
        let plan = plan(&columns(), &table("a,b\n7,x\n"), Matching::ByOrder);
        assert_eq!(plan.columns, vec!["id", "email"]);
        assert_eq!(plan.rows[0][0], Value::Int(7));
    }

    #[test]
    fn a_file_wider_than_the_table_stops_at_the_last_column() {
        let plan = plan(&columns(), &table("a,b,c,d\n1,2,3,4\n"), Matching::ByOrder);
        assert_eq!(plan.columns.len(), 3);
        assert_eq!(plan.ignored, vec!["d"]);
    }

    #[test]
    fn an_empty_field_arrives_as_a_null_and_not_as_an_empty_string() {
        let plan = plan(&columns(), &table("email\n\n"), Matching::ByName);
        assert_eq!(plan.rows.len(), 0, "a blank line is not a row");
        let plan = plan_of("email,id\n,3\n");
        assert_eq!(plan.rows[0][0], Value::Null);
        assert_eq!(plan.rows[0][1], Value::Int(3));
    }

    fn plan_of(text: &str) -> Plan {
        plan(&columns(), &table(text), Matching::ByName)
    }

    #[test]
    fn every_cell_is_read_as_the_kind_of_the_column_it_lands_in() {
        let plan = plan_of("id,is_active\n12,true\n");
        assert_eq!(plan.rows[0][0], Value::Int(12));
        assert_eq!(plan.rows[0][1], Value::Bool(true));
    }
}
