//! Right-click a column header: what that offers, and the filtering it starts.
//!
//! The row menu next door is about the selection; this one is about a column,
//! which is why it opens without disturbing the selection at all. Its first
//! item is the short way to the thing the filter row is for — a condition on
//! the column under the pointer, rather than on whichever column the composer
//! would otherwise have guessed at.

use gpui::{px, ClipboardItem, Context, IntoElement, Pixels, Point};
use grid::Sort;
use ui::{ContextMenu, IconName, MenuItem};

use crate::workspace::Workspace;

/// An open header menu: where it was asked for, and about which column.
pub(crate) struct ColumnMenu {
    pub at: Point<Pixels>,
    pub col: usize,
}

impl Workspace {
    pub fn open_column_menu(&mut self, at: Point<Pixels>, col: usize, cx: &mut Context<Self>) {
        self.menu = None;
        self.row_menu = None;
        self.column_menu = Some(ColumnMenu { at, col });
        cx.notify();
    }

    pub(crate) fn close_column_menu(&mut self, cx: &mut Context<Self>) {
        if self.column_menu.take().is_some() {
            cx.notify();
        }
    }

    /// The name of a column as the result set gives it, or nothing if the grid
    /// has moved on since the menu opened.
    fn column_name(&self, col: usize, cx: &gpui::App) -> Option<String> {
        let data = self.pane().grid.read(cx).data().clone();
        data.columns.get(col).map(|c| c.meta.name.to_string())
    }

    /// Open the band on a new row for this column. The `+` would have started
    /// one on the first column of the table; this is the same gesture with the
    /// answer to "which column" already given, which is the whole reason to
    /// reach for it from a header rather than from the band.
    fn filter_on_column(&mut self, col: usize, cx: &mut Context<Self>) {
        let Some(column) = self.column_name(col, cx) else {
            return;
        };
        self.open_chip(None, cx);
        self.set_chip_column(column, cx);
    }

    /// Sort by this column, in this direction, regardless of what the header
    /// arrow says now. A menu is not the three-state click on the header: it
    /// names the order it will produce, so it has to produce it.
    fn sort_column(&mut self, sort: Option<Sort>, cx: &mut Context<Self>) {
        self.pane()
            .grid
            .update(cx, |grid, cx| grid.set_sort(sort, cx));
        self.apply_sort(cx);
    }

    pub(crate) fn render_column_menu(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let menu = self.column_menu.as_ref()?;
        let (at, col) = (menu.at, menu.col);
        let name = self.column_name(col, cx)?;
        let sort = self.pane().grid.read(cx).sort();
        // A filter row becomes a `where` sent to the server, so it needs a
        // table to send it about.
        let filterable = self
            .pane()
            .active()
            .is_some_and(|tab| tab.relation.is_some());

        Some(
            ContextMenu::new("column-menu")
                .at(at)
                .width(px(232.))
                .on_dismiss(cx.listener(|this, _, _, cx| this.close_column_menu(cx)))
                .item(
                    MenuItem::new(format!("Filter on {name}…"))
                        .icon(IconName::Filter)
                        .disabled(!filterable)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_column_menu(cx);
                            this.filter_on_column(col, cx);
                        })),
                )
                .separator()
                .item(
                    MenuItem::new("Sort Ascending")
                        .icon(IconName::SortAsc)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_column_menu(cx);
                            this.sort_column(
                                Some(Sort {
                                    col,
                                    descending: false,
                                }),
                                cx,
                            );
                        })),
                )
                .item(
                    MenuItem::new("Sort Descending")
                        .icon(IconName::SortDesc)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_column_menu(cx);
                            this.sort_column(
                                Some(Sort {
                                    col,
                                    descending: true,
                                }),
                                cx,
                            );
                        })),
                )
                .item(
                    // "Put it back the way the server sent it" — the third
                    // click on a header, which nothing else names.
                    MenuItem::new("Reset Sort")
                        .icon(IconName::Sort)
                        .disabled(sort.is_none())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_column_menu(cx);
                            this.sort_column(None, cx);
                        })),
                )
                .separator()
                .item(
                    MenuItem::new("Copy Column Name")
                        .icon(IconName::Copy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(name) = this.column_name(col, cx) {
                                cx.write_to_clipboard(ClipboardItem::new_string(name));
                            }
                            this.close_column_menu(cx);
                        })),
                ),
        )
    }
}
