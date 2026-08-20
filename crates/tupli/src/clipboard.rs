//! Getting rows out of the grid: ⌘C, and the menu that names the other formats.
//!
//! The formatting itself lives in the `grid` crate, next to the selection it
//! reads. What is here is the part that needs a workspace: which pane the click
//! landed in, whether these rows can be edited, and where on screen to put the
//! menu.

use gpui::{px, ClipboardItem, Context, IntoElement, Pixels, Point};
use grid::Format;
use ui::{ContextMenu, IconName, MenuItem};

use crate::workspace::Workspace;

/// An open grid context menu: where it was asked for, and about which cell.
pub(crate) struct RowMenu {
    /// Window coordinates of the click that opened it.
    pub at: Point<Pixels>,
    pub row: usize,
    pub col: usize,
}

impl Workspace {
    /// Right-click on a cell. The grid has already made sure the cell is in the
    /// selection, so everything the menu offers is about the selection.
    pub fn open_row_menu(
        &mut self,
        at: Point<Pixels>,
        row: usize,
        col: usize,
        cx: &mut Context<Self>,
    ) {
        // Two menus at once is never what was meant; the tree's is the only
        // other one that can still be up when a click reaches the grid.
        self.menu = None;
        self.row_menu = Some(RowMenu { at, row, col });
        cx.notify();
    }

    pub(crate) fn close_row_menu(&mut self, cx: &mut Context<Self>) {
        if self.row_menu.take().is_some() {
            cx.notify();
        }
    }

    /// Copy the selection, close the menu.
    ///
    /// Every format goes through here so that none of them can leave the menu
    /// up over the rows it just copied.
    fn copy_rows(&mut self, format: Format, cx: &mut Context<Self>) {
        let text = self.pane().grid.read(cx).copy_text(format);
        if let Some(text) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        self.close_row_menu(cx);
    }

    pub(crate) fn render_row_menu(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let menu = self.row_menu.as_ref()?;
        let (at, row, col) = (menu.at, menu.row, menu.col);
        let grid = self.pane().grid.read(cx);
        let editable = grid.is_editable();
        // "Copy 3 Rows" rather than "Copy Rows": the count is the one thing
        // about this gesture that is easy to be wrong about, and a right click
        // that landed outside the selection has just changed it.
        let count = grid.selected_rows().len();
        let rows = match count {
            1 => "Copy Row".to_string(),
            n => format!("Copy {n} Rows"),
        };

        Some(
            ContextMenu::new("row-menu")
                .at(at)
                .width(px(232.))
                .on_dismiss(cx.listener(|this, _, _, cx| this.close_row_menu(cx)))
                .item(
                    MenuItem::new("Copy Cell")
                        .icon(IconName::Copy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let text = this.pane().grid.read(cx).cell_text(row, col);
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                            this.close_row_menu(cx);
                        })),
                )
                .item(
                    MenuItem::new(rows)
                        .icon(IconName::Copy)
                        .shortcut("⌘C")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.copy_rows(Format::Tsv { headers: false }, cx);
                        })),
                )
                .item(
                    MenuItem::new("Copy with Column Names")
                        .icon(IconName::Columns)
                        .shortcut("⇧⌘C")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.copy_rows(Format::Tsv { headers: true }, cx);
                        })),
                )
                .separator()
                .item(
                    MenuItem::new("Copy as CSV")
                        .icon(IconName::Table)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.copy_rows(Format::Csv, cx);
                        })),
                )
                .item(
                    MenuItem::new("Copy as JSON")
                        .icon(IconName::BracketsCurly)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.copy_rows(Format::Json, cx);
                        })),
                )
                .item(
                    MenuItem::new("Copy as Markdown")
                        .icon(IconName::TextAlignLeft)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.copy_rows(Format::Markdown, cx);
                        })),
                )
                .separator()
                .item(
                    // The same formats again, but to a file rather than the
                    // clipboard, so the ellipsis: this one asks where.
                    MenuItem::new("Export Rows…")
                        .icon(IconName::DatabaseExport)
                        .shortcut("⇧⌘E")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_row_menu(cx);
                            this.open_export(cx);
                        })),
                )
                .separator()
                .item(
                    // The three row verbs, all of them staging rather than
                    // writing — which is why Delete is not marked as danger
                    // here the way a `DROP` is. Nothing leaves this window
                    // until Commit.
                    MenuItem::new("Add Row")
                        .icon(IconName::Plus)
                        .disabled(!editable)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.add_row(cx);
                            this.close_row_menu(cx);
                        })),
                )
                .item(
                    MenuItem::new("Delete Selected Rows")
                        .icon(IconName::Trash)
                        .disabled(!editable)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.delete_rows(cx);
                            this.close_row_menu(cx);
                        })),
                )
                .item(
                    MenuItem::new("Revert Selected Rows")
                        .icon(IconName::Undo)
                        .disabled(!editable)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.revert_rows(cx);
                            this.close_row_menu(cx);
                        })),
                ),
        )
    }
}
