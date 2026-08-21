//! Getting rows out of the app and into a file.
//!
//! The formats are the clipboard's — the same five in [`grid::Format`], for the
//! same reason and with the same rules — so what is here is only the part that
//! a file needs and a clipboard does not: which rows, what to call it, where to
//! put it, and what to say when it does not work.
//!
//! One honesty constraint runs through all of it. A browsed table is a *page*
//! of itself, and an export that quietly wrote the page while the sheet said
//! "all rows" would be a data-loss bug wearing a success message. So the count
//! is always the count in hand, the sheet says out loud when there is more
//! behind it, and the word "all" is never used for a truncated result.

use std::path::PathBuf;

use gpui::{
    px, AppContext as _, Context, Entity, EventEmitter, IntoElement, Render, SharedString, Window,
};
use grid::{Format, Rows};
use ui::{
    Button, ButtonSize, ButtonVariant, FormRow, Label, LabelSize, Notice, NoticeTone, Segmented,
    Sheet as SheetView,
};

use crate::workspace::{count_of, Workspace};

/// The formats offered, in the order the control lists them. CSV first because
/// it is what a spreadsheet wants and a spreadsheet is where most exported rows
/// are going.
const FORMATS: [(&str, Format); 5] = [
    ("CSV", Format::Csv),
    ("TSV", Format::Tsv { headers: true }),
    ("JSON", Format::Json),
    ("SQL", Format::Sql),
    ("Markdown", Format::Markdown),
];

pub enum ExportEvent {
    Dismissed,
    /// Write these rows in this format. Where is the platform's question, and
    /// it is asked after the sheet closes.
    Confirmed {
        format: Format,
        rows: Rows,
    },
}

pub struct ExportSheet {
    /// What is being exported: a qualified table name, or the query.
    subject: SharedString,
    format: usize,
    rows: Rows,
    /// How many rows the selection covers. Zero when the grid has none, which
    /// is when the choice is not offered at all.
    selected: usize,
    /// How many rows the pane is holding — never how many the table has.
    held: usize,
    /// Whether there are more rows behind the ones in hand.
    partial: bool,
}

impl EventEmitter<ExportEvent> for ExportSheet {}

impl ExportSheet {
    pub fn new(
        subject: impl Into<SharedString>,
        selected: usize,
        held: usize,
        partial: bool,
    ) -> Self {
        Self {
            subject: subject.into(),
            format: 0,
            // A selection is a deliberate act and an export right after one is
            // almost always about it; with nothing selected there is only the
            // one answer anyway.
            rows: match selected > 0 {
                true => Rows::Selected,
                false => Rows::All,
            },
            selected,
            held,
            partial,
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        cx.emit(ExportEvent::Confirmed {
            format: FORMATS[self.format].1,
            rows: self.rows,
        });
    }

    fn count(&self) -> usize {
        match self.rows {
            Rows::Selected => self.selected,
            Rows::All => self.held,
        }
    }
}

impl Render for ExportSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let held = format!("{} in hand", count_of(self.held, "row"));
        // A control with one button in it is not a choice, it is a label
        // pretending to be one — and pretending invites a click that does
        // nothing. With no selection the count is simply stated.
        let rows: gpui::AnyElement = match self.selected {
            0 => Label::new(held).size(LabelSize::Small).into_any_element(),
            n => Segmented::new(
                "export-rows",
                vec![
                    SharedString::from(format!("Selection · {}", count_of(n, "row"))),
                    SharedString::from(held),
                ],
            )
            .selected(match self.rows {
                Rows::Selected => 0,
                Rows::All => 1,
            })
            .on_select({
                let sheet = cx.entity();
                move |index, _, cx| {
                    sheet.update(cx, |this, cx| {
                        this.rows = match index {
                            0 => Rows::Selected,
                            _ => Rows::All,
                        };
                        cx.notify();
                    })
                }
            })
            .into_any_element(),
        };

        SheetView::new("export-sheet", "Export Rows")
            .subtitle(self.subject.clone())
            .width(px(460.))
            .on_dismiss(cx.listener(|_, _, _, cx| cx.emit(ExportEvent::Dismissed)))
            .child(
                FormRow::new("Format")
                    .child(
                        Segmented::new("export-format", FORMATS.map(|(label, _)| label))
                            .selected(self.format)
                            .on_select({
                                let sheet = cx.entity();
                                move |index, _, cx| {
                                    sheet.update(cx, |this, cx| {
                                        this.format = index;
                                        cx.notify();
                                    })
                                }
                            }),
                    )
                    .hint(match FORMATS[self.format].1 {
                        Format::Sql => "Written as insert statements naming this table.",
                        Format::Tsv { .. } | Format::Csv => {
                            "A null and an empty string both write an empty field."
                        }
                        _ => "Hidden columns are left out, as they are on screen.",
                    }),
            )
            .child(FormRow::new("Rows").child(rows))
            // Only when it is true *and* it bites: a selection is exactly the
            // rows somebody pointed at, so warning about the ones behind it
            // would be answering a question nobody asked. Worded as the limit
            // it is — finding out from a short file afterwards is the worst
            // way to learn the export could not reach them.
            .children((self.partial && self.rows == Rows::All).then(|| {
                Notice::new(
                    NoticeTone::Warning,
                    "This is the page in hand, not the whole table. Load more rows first \
                     to export them.",
                )
            }))
            .footer_end(
                Button::new("cancel", "Cancel")
                    .size(ButtonSize::Small)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ExportEvent::Dismissed))),
            )
            .footer_end(
                Button::new("export", "Export…")
                    .variant(ButtonVariant::Accent)
                    .size(ButtonSize::Small)
                    .disabled(self.count() == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
            )
    }
}

impl Workspace {
    /// Open the export sheet for the active pane, if it has anything to export.
    pub fn open_export(&mut self, cx: &mut Context<Self>) {
        let grid = self.pane().grid.read(cx);
        let held = grid.row_count();
        if held == 0 {
            return;
        }
        // `selected_rows` falls back to the cursor's row, and a grid selects
        // its first cell the moment rows land. One row is therefore not
        // evidence of a selection — and one row is a ⌘C job anyway.
        let selected = grid.selected_rows().len();
        let selected = match selected > 1 {
            true => selected,
            false => 0,
        };
        let subject = match self.pane().active().and_then(|tab| tab.relation.clone()) {
            Some(relation) => relation.to_string(),
            None => "Query result".to_string(),
        };
        let partial = self.rows_beyond_this_page();

        let sheet = cx.new(|_| ExportSheet::new(subject, selected, held, partial));
        cx.subscribe(&sheet, Self::on_export_event).detach();
        self.export_sheet = Some(sheet);
        cx.notify();
    }

    fn on_export_event(
        &mut self,
        _sheet: Entity<ExportSheet>,
        event: &ExportEvent,
        cx: &mut Context<Self>,
    ) {
        self.export_sheet = None;
        cx.notify();
        if let ExportEvent::Confirmed { format, rows } = event {
            self.export_rows(*format, *rows, cx);
        }
    }

    /// Render the rows, ask where to put them, and write them there.
    ///
    /// The text is built here and moved into the task rather than read from the
    /// grid after the panel closes: the save panel is modal for as long as
    /// somebody is looking at it, and rows that changed underneath in the
    /// meantime would be written without anybody having seen them.
    pub fn export_rows(&mut self, format: Format, rows: Rows, cx: &mut Context<Self>) {
        let Some((count, text)) = self.render_export(format, rows, cx) else {
            return;
        };
        let name = format!("{}.{}", self.export_stem(), format.extension());
        let directory = dirs_downloads();
        let prompt = cx.prompt_for_new_path(&directory, Some(&name));
        cx.spawn(async move |this, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(path))) => path,
                // Cancelled, or a picker that would not open. Neither is worth
                // a message: one was on purpose and the other already logged.
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    log::warn!("the save panel did not open: {error:#}");
                    return;
                }
                // The window went away while the panel was up.
                Err(_) => return,
            };
            let written = cx
                .background_spawn({
                    let path = path.clone();
                    async move { std::fs::write(&path, text) }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_export(path, count, written, true, cx)
            });
        })
        .detach();
    }

    /// The same write with the panel skipped, for a caller that already knows
    /// where the file goes.
    pub fn export_rows_to(
        &mut self,
        path: PathBuf,
        format: Format,
        rows: Rows,
        cx: &mut Context<Self>,
    ) {
        let Some((count, text)) = self.render_export(format, rows, cx) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let written = cx
                .background_spawn({
                    let path = path.clone();
                    async move { std::fs::write(&path, text) }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_export(path, count, written, false, cx)
            });
        })
        .detach();
    }

    /// How many rows, and the file's whole contents.
    fn render_export(
        &self,
        format: Format,
        rows: Rows,
        cx: &Context<Self>,
    ) -> Option<(usize, String)> {
        let sheet = self
            .pane()
            .grid
            .read(cx)
            .sheet(rows)?
            .overriding(self.capabilities(cx).identity_overrides);
        let sheet = match self.pane().active().and_then(|tab| tab.relation.clone()) {
            Some(reference) => sheet.of(reference.qualified()),
            None => sheet,
        };
        Some((sheet.len(), sheet.render(format)))
    }

    /// What to call the file, before the extension: the table's own name, so a
    /// folder of exports reads as a list of tables.
    fn export_stem(&self) -> String {
        match self.pane().active().and_then(|tab| tab.relation.clone()) {
            Some(reference) => reference.name.to_string(),
            None => "query".to_string(),
        }
    }

    /// Report what happened, in the one place this app keeps a record of what
    /// it did: the History tab. On success Finder is also opened on the file —
    /// an export is done for the sake of the file, and the fastest way to say
    /// "it is there" is to show it. Not when the caller named the path itself,
    /// though: it already knows where the file is and did not ask to be shown.
    fn finish_export(
        &mut self,
        path: PathBuf,
        count: usize,
        written: std::io::Result<()>,
        reveal: bool,
        cx: &mut Context<Self>,
    ) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        // The file is the point, so the file is what the row names — and an
        // export took no time worth reporting, because the rows were already
        // in hand before it started.
        self.log_event(
            store::HistoryKind::Export,
            format!("export {} → {name}", count_of(count, "row")),
            match &written {
                Ok(()) => store::Finished {
                    row_count: Some(count as i64),
                    ..store::Finished::ok(0)
                },
                Err(error) => store::Finished::failed(0, format!("{name}: {error}")),
            },
            cx,
        );
        match written {
            Ok(()) if reveal => cx.reveal_path(&path),
            Ok(()) => {}
            // A failed write is the one case worth interrupting for: the file
            // somebody asked for does not exist, and they are about to go
            // looking for it.
            Err(_) => self.show_sidebar_tab(crate::workspace::SidebarTab::History, cx),
        }
        cx.notify();
    }
}

impl Workspace {
    /// Whether the pane is holding part of something bigger: a result that hit
    /// the row cap, or any page of a table but a lone first one.
    ///
    /// Guessed from the same evidence the footer's arrows use, because it is
    /// the only evidence there is — no `count(*)` was run, and running one to
    /// word a sentence would be a sequential scan per export.
    fn rows_beyond_this_page(&self) -> bool {
        let pane = self.pane();
        pane.truncated
            || pane
                .active()
                .and_then(|tab| tab.page)
                .is_some_and(|p| p > 0)
            || (pane.active().is_some_and(|tab| tab.relation.is_some())
                && pane.row_count >= self.settings.page_size())
    }
}

/// Where a save panel should open. The Downloads folder, which is where a
/// browser would have put it and therefore where people look first.
fn dirs_downloads() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Downloads"))
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| std::env::temp_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selection_is_what_an_export_right_after_one_is_about() {
        let sheet = ExportSheet::new("public.users", 12, 500, false);
        assert_eq!(sheet.rows, Rows::Selected);
        assert_eq!(sheet.count(), 12);
    }

    #[test]
    fn with_nothing_selected_there_is_only_one_answer() {
        let sheet = ExportSheet::new("public.users", 0, 500, false);
        assert_eq!(sheet.rows, Rows::All);
        assert_eq!(sheet.count(), 500);
    }

    #[test]
    fn every_format_the_sheet_offers_writes_a_file_of_its_own_kind() {
        // Two formats sharing an extension would silently overwrite each
        // other's suggested filename in the save panel.
        let mut extensions: Vec<&str> = FORMATS.iter().map(|(_, f)| f.extension()).collect();
        extensions.sort_unstable();
        let count = extensions.len();
        extensions.dedup();
        assert_eq!(extensions.len(), count);
    }
}
