//! The grid's model: what is loaded, how wide each column is, where the viewport
//! and the cursor are.
//!
//! Everything the element needs to draw a frame is a field on this struct, and
//! everything is a number rather than a widget. That is the whole trick behind
//! the frame budget: a scroll event changes one `Pixels` and the next frame
//! re-derives which thirty rows are on screen with two divisions. Nothing is
//! created or destroyed when you scroll a million rows.

use std::ops::Range;
use std::sync::Arc;

use db::{ResultSet, Value, ValueKind};
use editor::{Editor, EditorEvent, EditorMode};
use sqlgen::{PendingChanges, RowRef};

use crate::bench::{Bench, FrameMeter};
use gpui::{
    px, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Pixels, Point,
    Size, Subscription,
};
use ui::ActiveTheme;

/// Overlay scrollbar thickness, and the gap that keeps it off the panel edge.
pub(crate) const SCROLLBAR: f32 = 8.;
pub(crate) const SCROLLBAR_INSET: f32 = 2.;
/// What one of them covers of the axis it lies across.
pub(crate) const SCROLLBAR_FOOTPRINT: Pixels = px(SCROLLBAR + SCROLLBAR_INSET * 2.);

/// How the grid sizes its rows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Density {
    Compact,
    #[default]
    Default,
    Comfortable,
}

impl Density {
    pub fn row_height(self, cx: &App) -> Pixels {
        match self {
            Self::Compact => px(22.),
            Self::Default => cx.metrics().grid_row_height,
            Self::Comfortable => px(28.),
        }
    }
}

/// A rectangular block of cells. Selection is a list of these rather than a
/// per-cell flag set: a user who selects a whole million-row column should cost
/// four integers, not a million bools.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CellRect {
    pub rows: Range<usize>,
    pub cols: Range<usize>,
}

impl CellRect {
    /// Whole rows, from either end to the other.
    ///
    /// Every selection this grid makes is one of these. The columns are still
    /// carried because a null or a copy has to name cells, but nothing narrows
    /// them: what a person selects here is a record, not a corner of one.
    pub fn rows(a: usize, b: usize, cols: usize) -> Self {
        Self {
            rows: a.min(b)..a.max(b) + 1,
            cols: 0..cols,
        }
    }
}

/// Per-column display state, separate from the column's data so that resizing,
/// pinning, and hiding never touch the million values underneath.
#[derive(Clone, Debug)]
pub struct ColumnLayout {
    pub width: Pixels,
    /// Set once the user drags the edge. Auto-width stops recomputing for a
    /// column the user has sized: their number is a decision, ours is a guess.
    pub user_sized: bool,
    pub hidden: bool,
    /// Pinned columns render in the frozen left region and never scroll
    /// horizontally.
    pub pinned: bool,
}

/// Emitted when the user activates a cell, so the container can open the row
/// inspector or follow a foreign key without the grid knowing either exists.
#[derive(Clone, Copy, Debug)]
pub enum GridEvent {
    CursorMoved {
        row: usize,
        col: usize,
    },
    Activated {
        row: usize,
        col: usize,
    },
    /// A header was clicked. The grid records the new sort but does not apply
    /// it: whether the answer comes from re-asking the server or from
    /// reordering the rows already here is the container's decision, and only
    /// the container knows whether these rows are the whole table.
    SortChanged {
        col: usize,
        descending: bool,
    },
    /// Something was staged, reverted, or undone. The container redraws its
    /// commit bar; the grid does not know there is one.
    ChangesEdited,
    /// A right click landed on a cell. The grid has already moved the cursor
    /// there if it had to, so the container only has to put a menu at `at` —
    /// which is in window coordinates, because that is where a menu lives.
    ContextMenu {
        at: Point<Pixels>,
        row: usize,
        col: usize,
    },
}

/// Which column the grid is sorted by, and which way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sort {
    pub col: usize,
    pub descending: bool,
}

pub struct Grid {
    pub(crate) data: Arc<ResultSet>,
    pub(crate) columns: Vec<ColumnLayout>,
    /// Prefix sums of visible column widths; `x_offsets[i]` is where column `i`
    /// starts. Recomputed on resize, not per frame — finding the first visible
    /// column is then a binary search instead of a scan.
    pub(crate) x_offsets: Vec<Pixels>,
    pub(crate) scroll: Point<Pixels>,
    pub(crate) cursor: (usize, usize),
    /// The far corner of a shift-selection. `None` means the cursor is the anchor.
    pub(crate) anchor: Option<(usize, usize)>,
    pub(crate) selection: Vec<CellRect>,
    pub(crate) density: Density,
    pub(crate) zebra: bool,
    /// Last painted viewport, so page-up and autoscroll know how far a page is.
    pub(crate) viewport: Size<Pixels>,
    /// Set when the cursor moves; the next prepaint scrolls it into view and
    /// clears it. Scrolling is a paint-time concern because it needs the
    /// viewport, which the model does not know until it has been laid out.
    pub(crate) autoscroll: bool,
    /// True once widths have been measured against a real window.
    pub(crate) measured: bool,
    /// An in-flight column resize: which column, where the pointer went down,
    /// and how wide the column was then. Storing the *start* rather than
    /// applying deltas keeps the width exact — accumulated deltas drift by a
    /// pixel per event as they round.
    pub(crate) dragging_column: Option<(usize, Pixels, Pixels)>,
    /// True between pressing in the body and letting go, so a move extends the
    /// selection instead of merely hovering. A press is a selection gesture
    /// until the button comes back up, wherever the pointer wanders in the
    /// meantime — leaving the grid, or running past the last row, is part of
    /// the gesture rather than the end of it.
    pub(crate) selecting: bool,
    /// Which column edge the pointer is currently within grabbing distance of.
    ///
    /// A cursor shape is per-frame state, so the frame has to be redrawn when
    /// the answer changes — nothing else about a bare hover does. Keeping the
    /// answer here rather than recomputing it in the move handler is what makes
    /// "has it changed?" cheap enough to ask on every mouse move.
    pub(crate) hover_edge: Option<usize>,
    /// Which header the pointer is over, for the sort hint. Same story as
    /// `hover_edge`: a per-frame decision that needs a frame to be asked for.
    pub(crate) hover_header: Option<usize>,
    /// The sort the header is advertising. Set by the container after it has
    /// actually reordered the rows, so the arrow never claims an order the
    /// grid is not in.
    pub(crate) sort: Option<Sort>,

    /// Frame timing for the M0 gate. `None` outside benchmark runs, so an
    /// ordinary session pays nothing for it.
    pub(crate) bench: Option<Bench>,
    focus: FocusHandle,

    // ---- editing ---------------------------------------------------------
    /// Whether this result set can be written back at all. The grid does not
    /// work that out — it is a catalog question, and the container answers it
    /// with [`Grid::set_editable`].
    pub(crate) editable: bool,
    /// Everything staged and not yet committed. Behind an `Arc` so a frame can
    /// hold the set it painted without copying it, and so a commit can hand the
    /// whole thing to a background task.
    pub(crate) changes: Arc<PendingChanges>,
    /// The cell currently under a text editor, if any.
    pub(crate) editing: Option<Editing>,
    /// Set when an edit ends, because focus has to go back to the grid and
    /// only the render pass has a window to give it to.
    pub(crate) refocus: bool,
    /// Width of the row-number gutter as of the last frame.
    ///
    /// The element measures it (it depends on the widest ordinal, so on the row
    /// count and the font) and writes it back here, because the cell editor is
    /// a sibling element that has to be positioned before the grid paints.
    pub(crate) gutter: Pixels,
    /// Set when an edit begins, for the same reason as `refocus` and in the
    /// other direction: the new editor has to take the keyboard.
    pub(crate) focus_edit: bool,
}

/// A cell being typed into.
pub(crate) struct Editing {
    pub(crate) row: usize,
    pub(crate) col: usize,
    pub(crate) editor: Entity<Editor>,
    /// Kept alive for as long as the edit is: it is what turns Enter into a
    /// commit and Escape into a cancel.
    pub(crate) _events: Subscription,
}

impl EventEmitter<GridEvent> for Grid {}

impl Focusable for Grid {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Grid {
    /// Auto-width bounds. A column narrower than the minimum is unreadable; one
    /// wider than the maximum pushes every other column off screen for the sake
    /// of one long text blob, which is what truncation is for.
    pub const MIN_COLUMN_WIDTH: f32 = 48.;
    pub const MAX_COLUMN_WIDTH: f32 = 420.;
    /// Horizontal padding inside a cell, each side.
    pub const CELL_PADDING: f32 = 8.;
    /// Rows sampled when guessing a column's width.
    ///
    /// Sampling rather than scanning is not an approximation we tolerate, it is
    /// the only option: measuring a million rows to decide a width would take
    /// longer than the query did. The cost is that a wide value further down
    /// truncates until the user widens the column or double-clicks the edge.
    pub const WIDTH_SAMPLE_ROWS: usize = 256;

    pub fn new(data: ResultSet, cx: &mut Context<Self>) -> Self {
        let columns = data
            .columns
            .iter()
            .map(|_| ColumnLayout {
                width: px(120.),
                user_sized: false,
                hidden: false,
                pinned: false,
            })
            .collect();
        Self {
            data: Arc::new(data),
            columns,
            x_offsets: Vec::new(),
            scroll: Point::default(),
            cursor: (0, 0),
            anchor: None,
            selection: Vec::new(),
            density: Density::default(),
            zebra: true,
            viewport: Size::default(),
            autoscroll: false,
            measured: false,
            dragging_column: None,
            selecting: false,
            hover_edge: None,
            hover_header: None,
            sort: None,
            bench: None,
            focus: cx.focus_handle(),
            editable: false,
            changes: Arc::new(PendingChanges::new()),
            editing: None,
            refocus: false,
            gutter: px(0.),
            focus_edit: false,
        }
    }

    pub fn data(&self) -> &Arc<ResultSet> {
        &self.data
    }

    pub fn sort(&self) -> Option<Sort> {
        self.sort
    }

    pub fn set_sort(&mut self, sort: Option<Sort>, cx: &mut Context<Self>) {
        self.sort = sort;
        cx.notify();
    }

    /// A click on a column header. First click sorts ascending, the second
    /// flips it, the third clears it — three states, because "put it back the
    /// way the server sent it" is a thing people want and no other gesture
    /// offers it.
    pub fn cycle_sort(&mut self, col: usize, cx: &mut Context<Self>) {
        let next = match self.sort {
            Some(Sort {
                col: c,
                descending: false,
            }) if c == col => Some(Sort {
                col,
                descending: true,
            }),
            Some(Sort {
                col: c,
                descending: true,
            }) if c == col => None,
            _ => Some(Sort {
                col,
                descending: false,
            }),
        };
        self.sort = next;
        match next {
            Some(Sort { col, descending }) => cx.emit(GridEvent::SortChanged { col, descending }),
            // Clearing is still a sort change; the container re-asks for the
            // rows in their natural order.
            None => cx.emit(GridEvent::SortChanged {
                col,
                descending: false,
            }),
        }
        cx.notify();
    }

    pub fn set_data(&mut self, data: ResultSet, cx: &mut Context<Self>) {
        self.set_data_arc(Arc::new(data), cx);
    }

    /// The same, for a result set somebody else is also holding — a script's
    /// answers stay alive in the pane so its result tabs can switch between
    /// them, and copying a hundred thousand rows to show one of them again
    /// would be silly.
    pub fn set_data_arc(&mut self, data: Arc<ResultSet>, cx: &mut Context<Self>) {
        // The same columns coming back is the ordinary case — a sort, the next
        // page, a re-run of the statement being written — and everything the
        // reader arranged *about the columns* is still about these columns:
        // their widths, which are pinned, which are hidden, and how far right
        // the view was dragged. Only the rows are new, so only the row-shaped
        // state resets. Sorting a table forty columns wide by scrolling to
        // column thirty and clicking would otherwise throw the reader back to
        // column one every time.
        let same_columns = self.columns.len() == data.columns.len()
            && self
                .data
                .columns
                .iter()
                .zip(data.columns.iter())
                .all(|(a, b)| a.meta.name == b.meta.name && a.meta.kind == b.meta.kind);
        if !same_columns {
            self.columns = data
                .columns
                .iter()
                .map(|_| ColumnLayout {
                    width: px(120.),
                    user_sized: false,
                    hidden: false,
                    pinned: false,
                })
                .collect();
            self.scroll.x = px(0.);
            self.measured = false;
        }
        self.data = data;
        self.scroll.y = px(0.);
        self.cursor = (0, 0);
        self.anchor = None;
        // The first row, selected. The inspector beside the grid describes
        // whichever row is selected and defaults to the first one; leaving the
        // selection empty until the first click meant a panel describing row 1
        // next to a grid with nothing highlighted in it.
        self.selection = match self.data.row_count() > 0 && self.data.column_count() > 0 {
            true => vec![CellRect::rows(0, 0, self.data.column_count())],
            false => Vec::new(),
        };
        // Staged edits do not survive the rows they were staged against. A
        // change set is a list of row indexes, so carrying it over to another
        // result set does not merely look wrong in the next tab — it points an
        // uncommitted update at whatever row happens to be third in a table it
        // was never written for.
        self.editing = None;
        if !self.changes.is_empty() {
            self.changes = Arc::new(PendingChanges::new());
            cx.emit(GridEvent::ChangesEdited);
        }
        // A new result set is not the old one reordered, so the arrow goes
        // away. The container puts it back if this data *is* the reordering it
        // asked for.
        self.sort = None;
        cx.notify();
    }

    /// Deterministic viewport sweep for the criterion benchmark: down the rows
    /// and across the columns at different rates, so the pair does not repeat
    /// for a long time and the line-layout cache never gets a free frame.
    pub fn scroll_for_bench(&mut self, tick: u32, cx: &App) {
        let max = self.max_scroll(self.viewport, cx);
        let sweep = |distance: f32, max: Pixels| {
            let max = f32::from(max);
            if max <= 0. {
                px(0.)
            } else {
                px(distance % max)
            }
        };
        self.scroll.y = sweep(tick as f32 * 13., max.y);
        self.scroll.x = sweep(tick as f32 * 7., max.x);
    }

    /// Puts the grid into benchmark mode: it drives its own redraw loop and
    /// scrolls continuously, so the numbers describe frames that are actually
    /// doing work rather than frames repainting a still image.
    pub fn start_benchmark(&mut self) {
        self.bench = Some(Bench {
            frame: FrameMeter::new("grid frame", 900),
            paint: FrameMeter::new("grid paint", 900),
            last: None,
            direction: 1.,
        });
    }

    /// The focus handle, for the element's click-to-focus path.
    pub fn focus(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    /// Rows the cursor can reach: what came back, plus what is waiting to be
    /// inserted. New rows are real rows as far as the grid is concerned —
    /// that is the whole point of staging them here rather than in a form.
    pub fn row_count(&self) -> usize {
        self.data.row_count() + self.changes.new_row_count()
    }

    /// Rows that exist in the database.
    pub fn fetched_row_count(&self) -> usize {
        self.data.row_count()
    }

    pub fn set_density(&mut self, density: Density, cx: &mut Context<Self>) {
        self.density = density;
        cx.notify();
    }

    pub fn set_zebra(&mut self, on: bool, cx: &mut Context<Self>) {
        self.zebra = on;
        cx.notify();
    }

    // ---- editing ---------------------------------------------------------

    /// Whether the container has said this result set can be written back.
    pub fn is_editable(&self) -> bool {
        self.editable
    }

    /// Say whether rows here can be edited. Turning it off throws away
    /// anything staged: a grid that cannot be committed must not go on
    /// displaying changes as though it could.
    pub fn set_editable(&mut self, editable: bool, cx: &mut Context<Self>) {
        if self.editable == editable {
            return;
        }
        self.editable = editable;
        if !editable {
            self.editing = None;
            self.changes = Arc::new(PendingChanges::new());
        }
        cx.notify();
    }

    pub fn changes(&self) -> &Arc<PendingChanges> {
        &self.changes
    }

    pub fn has_changes(&self) -> bool {
        !self.changes.is_empty()
    }

    pub fn discard_changes(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self.changes = Arc::new(PendingChanges::new());
        cx.emit(GridEvent::ChangesEdited);
        cx.notify();
    }

    /// Which row of the change set a display row is. Rows past the fetched
    /// ones are the staged inserts, in the order they were added.
    pub fn row_ref(&self, row: usize) -> Option<RowRef> {
        let fetched = self.data.row_count();
        match row.checked_sub(fetched) {
            None => Some(RowRef::Existing(row)),
            Some(nth) => self.changes.new_rows().nth(nth).map(RowRef::New),
        }
    }

    /// What a cell shows: the staged value if there is one, otherwise what
    /// came back from the server. `None` is a null.
    ///
    /// This allocates, so it is for the handful of cells that leave the paint
    /// loop — the one being edited, the one the inspector is showing.
    pub fn cell_value(&self, row: usize, col: usize) -> Option<Value> {
        let staged = self
            .row_ref(row)
            .and_then(|r| self.changes.value(r, col))
            .cloned();
        match staged {
            Some(value) => Some(value),
            None if row < self.data.row_count() => Some(self.data.columns.get(col)?.value(row)),
            // A new row nobody has typed into yet: the column will take its
            // default, and there is nothing to show.
            None => None,
        }
    }

    /// Stage a value into a cell, or `None` for a null.
    pub fn set_cell(&mut self, row: usize, col: usize, value: Value, cx: &mut Context<Self>) {
        let Some(target) = self.row_ref(row) else {
            return;
        };
        Arc::make_mut(&mut self.changes).set(target, col, value);
        cx.emit(GridEvent::ChangesEdited);
        cx.notify();
    }

    /// Add a row below the last one and put the cursor in it.
    pub fn add_row(&mut self, cx: &mut Context<Self>) {
        if !self.editable {
            return;
        }
        Arc::make_mut(&mut self.changes).insert();
        let row = self.row_count() - 1;
        self.set_cursor(row, 0, false, cx);
        cx.emit(GridEvent::ChangesEdited);
        cx.notify();
    }

    /// Mark every selected row for deletion — or unmark them, if they are
    /// already marked. One key for both directions, because the mark is the
    /// only thing that happened and undoing it should not need a second one.
    pub fn delete_rows(&mut self, cx: &mut Context<Self>) {
        if !self.editable {
            return;
        }
        let rows = self.selected_rows();
        let all_marked = rows.iter().all(|row| match self.row_ref(*row) {
            Some(RowRef::Existing(row)) => self.changes.is_deleted(row),
            _ => false,
        });
        let changes = Arc::make_mut(&mut self.changes);
        for row in rows {
            match self.data.row_count() > row {
                true if all_marked => changes.undelete(row),
                _ => {
                    if let Some(target) = row_ref_in(&self.data, changes, row) {
                        changes.delete(target);
                    }
                }
            }
        }
        cx.emit(GridEvent::ChangesEdited);
        cx.notify();
    }

    /// Throw away the edits on the selected rows, leaving everything else
    /// staged.
    pub fn revert_rows(&mut self, cx: &mut Context<Self>) {
        let rows = self.selected_rows();
        let cols = self.data.column_count();
        let changes = Arc::make_mut(&mut self.changes);
        for row in rows {
            let Some(target) = row_ref_in(&self.data, changes, row) else {
                continue;
            };
            match target {
                RowRef::Existing(row) => {
                    changes.undelete(row);
                    for col in 0..cols {
                        changes.revert(RowRef::Existing(row), col);
                    }
                }
                // Reverting a row that does not exist yet is removing it.
                RowRef::New(_) => changes.delete(target),
            }
        }
        cx.emit(GridEvent::ChangesEdited);
        cx.notify();
    }

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        Arc::make_mut(&mut self.changes).undo();
        self.clamp_cursor();
        cx.emit(GridEvent::ChangesEdited);
        cx.notify();
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        Arc::make_mut(&mut self.changes).redo();
        self.clamp_cursor();
        cx.emit(GridEvent::ChangesEdited);
        cx.notify();
    }

    /// The rows the selection covers, or the cursor's row if there is no
    /// selection.
    pub fn selected_rows(&self) -> Vec<usize> {
        let mut rows: Vec<usize> = self
            .selection
            .iter()
            .flat_map(|rect| rect.rows.clone())
            .filter(|row| *row < self.row_count())
            .collect();
        if rows.is_empty() {
            rows.push(self.cursor.0);
        }
        rows.sort_unstable();
        rows.dedup();
        rows
    }

    /// An undo that removes a staged row can leave the cursor past the end.
    fn clamp_cursor(&mut self) {
        let last = self.row_count().saturating_sub(1);
        self.cursor.0 = self.cursor.0.min(last);
    }

    // ---- the cell editor -------------------------------------------------

    /// Open an editor over a cell. `seed` is the keystroke that started it,
    /// when typing is what opened the editor rather than Enter — the character
    /// replaces the value, the way it does in every spreadsheet.
    pub fn begin_edit(
        &mut self,
        row: usize,
        col: usize,
        seed: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if !self.editable || col >= self.data.column_count() || row >= self.row_count() {
            return;
        }
        let text = match seed {
            Some(seed) => seed.to_string(),
            None => match self.cell_value(row, col) {
                Some(Value::Null) | None => String::new(),
                Some(value) => value.to_string(),
            },
        };
        let seeded = seed.is_some();
        let editor = cx.new(|cx| {
            let mut editor = Editor::new(EditorMode::SingleLine, cx);
            editor.set_style(cell_style(cx));
            // `set_text` leaves the caret at the end, which is where a seeded
            // editor wants it: the keystroke that opened it has already been
            // typed. Enter opened it instead, so the old value is selected and
            // the next keystroke replaces it.
            editor.set_text(&text, cx);
            if !seeded {
                editor.select_all(cx);
            }
            editor
        });
        let events = cx.subscribe(&editor, |grid, _, event: &EditorEvent, cx| match event {
            EditorEvent::Submit => grid.stage_edit(cx),
            EditorEvent::Cancel => grid.cancel_edit(cx),
            _ => {}
        });
        self.editing = Some(Editing {
            row,
            col,
            editor,
            _events: events,
        });
        self.focus_edit = true;
        cx.notify();
    }

    /// Open an editor over the cursor's cell.
    pub fn edit_cursor(&mut self, seed: Option<&str>, cx: &mut Context<Self>) {
        let (row, col) = self.cursor;
        self.begin_edit(row, col, seed, cx);
    }

    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Close the editor, keeping what was typed.
    ///
    /// Staged, not written: the value joins [`Grid::changes`] and the cell
    /// shows as dirty. Getting it to the server is the Commit button's job,
    /// which is why this is not called `commit` — the two words would be the
    /// same word for two very different amounts of consequence.
    pub fn stage_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        let text = editing.editor.read(cx).text();
        let kind = self
            .data
            .columns
            .get(editing.col)
            .map(|c| c.meta.kind)
            .unwrap_or(ValueKind::Text);
        // An empty field is an empty string, not a null. Nulling a cell is its
        // own gesture (⌘⌫) precisely because the two are different values and
        // the difference is invisible in a text field.
        let value = Value::parse(kind, &text);
        self.set_cell(editing.row, editing.col, value, cx);
        self.refocus = true;
        cx.notify();
    }

    /// Abandon the edit. Nothing is staged.
    pub fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            self.refocus = true;
            cx.notify();
        }
    }

    /// Set one cell to null.
    ///
    /// One cell rather than the selection: the selection is a set of rows, and
    /// nulling every column of a row is not a gesture anyone means to make.
    /// The row inspector names the field, which is where this is asked from.
    pub fn null_cell(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        if !self.editable {
            return;
        }
        let changes = Arc::make_mut(&mut self.changes);
        let Some(target) = row_ref_in(&self.data, changes, row) else {
            return;
        };
        changes.set(target, col, Value::Null);
        cx.emit(GridEvent::ChangesEdited);
        cx.notify();
    }

    /// Recompute the prefix sums after any change to widths, order, or visibility.
    pub(crate) fn rebuild_offsets(&mut self) {
        self.x_offsets.clear();
        self.x_offsets.reserve(self.columns.len() + 1);
        let mut x = px(0.);
        for col in &self.columns {
            self.x_offsets.push(x);
            if !col.hidden {
                x += col.width;
            }
        }
        self.x_offsets.push(x);
    }

    /// Total width of all unpinned columns.
    pub(crate) fn content_width(&self) -> Pixels {
        self.x_offsets.last().copied().unwrap_or_default()
    }

    pub(crate) fn content_height(&self, cx: &App) -> Pixels {
        self.density.row_height(cx) * self.row_count() as f32
    }

    /// The first column whose right edge is past `x`, found by binary search on
    /// the prefix sums. Linear scanning here is fine at 20 columns and is a
    /// visible cost at 500, which real tables have.
    pub(crate) fn column_at(&self, x: Pixels) -> usize {
        match self
            .x_offsets
            .binary_search_by(|off| off.partial_cmp(&x).unwrap())
        {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
        .min(self.columns.len().saturating_sub(1))
    }

    pub(crate) fn column_bounds(&self, col: usize) -> (Pixels, Pixels) {
        (self.x_offsets[col], self.columns[col].width)
    }

    /// Which rows fall inside a vertical window. Clamped to the data, so an
    /// over-scrolled or empty grid yields an empty range instead of panicking
    /// in the paint loop.
    pub(crate) fn visible_rows(&self, top: Pixels, height: Pixels, cx: &App) -> Range<usize> {
        let rh = self.density.row_height(cx);
        if rh <= px(0.) || self.row_count() == 0 {
            return 0..0;
        }
        let first = (f32::from(top) / f32::from(rh)).floor().max(0.) as usize;
        let count = (f32::from(height) / f32::from(rh)).ceil() as usize + 2;
        let last = (first + count).min(self.row_count());
        first.min(last)..last
    }

    // ---- movement --------------------------------------------------------

    pub fn move_cursor(&mut self, drow: isize, dcol: isize, extend: bool, cx: &mut Context<Self>) {
        let rows = self.row_count();
        let cols = self.data.column_count();
        if rows == 0 || cols == 0 {
            return;
        }
        let row = (self.cursor.0 as isize + drow).clamp(0, rows as isize - 1) as usize;
        let col = (self.cursor.1 as isize + dcol).clamp(0, cols as isize - 1) as usize;
        self.set_cursor(row, col, extend, cx);
    }

    /// Put the cursor on a cell, which selects the row it is in.
    ///
    /// The selection is by row. A single highlighted cell is a spreadsheet's
    /// idea of what you are pointing at, and this is a table of records: the
    /// thing you clicked is the row, and the column only decides which value a
    /// double click would open.
    pub fn set_cursor(&mut self, row: usize, col: usize, extend: bool, cx: &mut Context<Self>) {
        let rows = self.row_count();
        let cols = self.data.column_count();
        if rows == 0 || cols == 0 {
            return;
        }
        let row = row.min(rows - 1);
        let col = col.min(cols - 1);

        if extend {
            // The anchor is set on the *first* extension, not on every one, or
            // shift-arrow would collapse the selection to one row each press.
            let anchor = self.anchor.get_or_insert(self.cursor).0;
            self.selection = vec![CellRect::rows(anchor, row, cols)];
        } else {
            self.anchor = None;
            self.selection = vec![CellRect::rows(row, row, cols)];
        }
        self.cursor = (row, col);
        self.autoscroll = true;
        cx.emit(GridEvent::CursorMoved { row, col });
        cx.notify();
    }

    pub fn page(&mut self, direction: isize, extend: bool, cx: &mut Context<Self>) {
        let rh = self.density.row_height(cx);
        let rows = (f32::from(self.viewport.height) / f32::from(rh))
            .floor()
            .max(1.) as isize;
        self.move_cursor(direction * rows, 0, extend, cx);
    }

    pub fn go_to_row(&mut self, row: usize, extend: bool, cx: &mut Context<Self>) {
        let col = self.cursor.1;
        self.set_cursor(row, col, extend, cx);
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        let rows = self.row_count();
        let cols = self.data.column_count();
        if rows == 0 || cols == 0 {
            return;
        }
        self.selection = vec![CellRect {
            rows: 0..rows,
            cols: 0..cols,
        }];
        cx.notify();
    }

    pub(crate) fn selection(&self) -> &[CellRect] {
        &self.selection
    }

    pub fn row_selected(&self, row: usize) -> bool {
        self.selection.iter().any(|r| r.rows.contains(&row))
    }

    // ---- scrolling -------------------------------------------------------

    /// `body` is the area the rows are drawn in — not the whole element. The
    /// header does not scroll, so counting its height here would hold the last
    /// row a header below the bottom edge, out of reach.
    pub(crate) fn max_scroll(&self, body: Size<Pixels>, cx: &App) -> Point<Pixels> {
        let content = Size {
            width: self.content_width(),
            height: self.content_height(cx),
        };
        max_scroll_of(content, body, self.density.row_height(cx))
    }

    pub(crate) fn clamp_scroll(&mut self, body: Size<Pixels>, cx: &App) {
        let max = self.max_scroll(body, cx);
        self.scroll.x = self.scroll.x.clamp(px(0.), max.x);
        self.scroll.y = self.scroll.y.clamp(px(0.), max.y);
    }

    pub fn scroll_by(&mut self, delta: Point<Pixels>, cx: &mut Context<Self>) {
        // Trackpad deltas are positive when the content should move down, which
        // is the opposite sign from the scroll offset.
        self.scroll.x -= delta.x;
        self.scroll.y -= delta.y;
        cx.notify();
    }

    /// Bring the cursor into view, moving as little as possible: a cursor that
    /// is already visible must not cause a scroll, or every keypress jitters
    /// the viewport.
    pub(crate) fn scroll_cursor_into_view(&mut self, body: Size<Pixels>, cx: &App) {
        let rh = self.density.row_height(cx);
        let top = rh * self.cursor.0 as f32;
        if top < self.scroll.y {
            self.scroll.y = top;
        } else if top + rh > self.scroll.y + body.height {
            self.scroll.y = top + rh - body.height;
        }

        if self.cursor.1 < self.x_offsets.len().saturating_sub(1) {
            let (x, w) = self.column_bounds(self.cursor.1);
            if x < self.scroll.x {
                self.scroll.x = x;
            } else if x + w > self.scroll.x + body.width {
                self.scroll.x = x + w - body.width;
            }
        }
    }

    // ---- sizing ----------------------------------------------------------

    /// Set a column's width from a drag, and remember that it was a decision.
    pub fn resize_column(&mut self, col: usize, width: Pixels, cx: &mut Context<Self>) {
        if let Some(c) = self.columns.get_mut(col) {
            c.width = px(f32::from(width).clamp(Self::MIN_COLUMN_WIDTH, 2000.));
            c.user_sized = true;
            self.rebuild_offsets();
            cx.notify();
        }
    }

    /// Width in units of "advance of a digit" for a value, used by auto-sizing.
    ///
    /// The grid is entirely monospaced — data grids always are, because a column
    /// of numbers whose digits do not line up is a column you cannot read down.
    /// That makes width exactly proportional to character count, so a column can
    /// be sized by counting characters instead of shaping text, which is the
    /// difference between sampling 256 rows in microseconds and in milliseconds.
    /// Wide (CJK) characters count double, which is what a monospaced face
    /// actually renders them at.
    pub(crate) fn display_cells(s: &str) -> usize {
        s.chars().map(|c| if c >= '\u{1100}' { 2 } else { 1 }).sum()
    }

    /// Longest sampled value in each column, in character cells.
    pub(crate) fn sample_widths(&self) -> Vec<usize> {
        let rows = self.data.row_count();
        let sample = Self::WIDTH_SAMPLE_ROWS.min(rows);
        let mut scratch = String::new();
        let mut out = Vec::with_capacity(self.data.columns.len());
        for column in &self.data.columns {
            let mut widest = 0usize;
            for row in 0..sample {
                let cells = match column.render(row, &mut scratch) {
                    db::CellText::Null => 4, // the NULL placeholder
                    db::CellText::Borrowed(s) => Self::display_cells(s),
                    db::CellText::Formatted => Self::display_cells(&scratch),
                };
                widest = widest.max(cells);
            }
            out.push(widest);
        }
        out
    }
}

/// [`Grid::row_ref`] without borrowing the whole grid — the mutating paths need
/// the data and the change set at the same time, and one of them is `&mut`.
fn row_ref_in(data: &ResultSet, changes: &PendingChanges, row: usize) -> Option<RowRef> {
    match row.checked_sub(data.row_count()) {
        None => Some(RowRef::Existing(row)),
        Some(nth) => changes.new_rows().nth(nth).map(RowRef::New),
    }
}

/// The cell editor's type. Same face and size as the cell underneath it, so
/// the text does not jump when the editor opens over it.
fn cell_style(cx: &App) -> editor::EditorStyle {
    let ty = cx.typography();
    editor::EditorStyle {
        font: ty.mono_font(),
        font_size: ty.mono_size,
        line_height: ty.mono_line_height,
        padding_x: px(Grid::CELL_PADDING),
        padding_y: px(0.),
    }
}

/// Whether a column's values want to hug the right edge.
pub(crate) fn is_right_aligned(kind: ValueKind) -> bool {
    kind.is_numeric()
}

/// How far `content` can be scrolled inside `view`, given how much blank the
/// rows may be dragged past their own end.
///
/// Two things are added to the content before the view is taken off it.
///
/// The overlay scrollbars float on top of the content rather than taking
/// layout width, so where one lies the row or column under it is read
/// *through* it. Each axis that has a bar therefore gives the other that much
/// more travel, which is what lets the last row be scrolled out from under the
/// horizontal bar and the last column out from behind the vertical one.
/// Reserving the space instead would narrow the grid permanently to serve a
/// control macOS hides by default.
///
/// Then `overscroll`, which is slack rather than compensation: a table whose
/// last row stops exactly at the bottom edge has that row wedged between the
/// scrollbar floating over it and the status bar beginning immediately below,
/// and the only way to read it is to know it is the last one. Being able to
/// pull it up off the edge is what makes the end of a table feel like an end
/// rather than a cut. Only downwards — blank to the right of the last column
/// reads as a column that failed to load, so sideways still stops dead.
///
/// Neither applies to content that already fits: a four-row table must not
/// scroll at all.
fn max_scroll_of(content: Size<Pixels>, view: Size<Pixels>, overscroll: Pixels) -> Point<Pixels> {
    let (mut width, mut height) = (content.width, content.height);
    if width > view.width {
        height += SCROLLBAR_FOOTPRINT;
    }
    // Asked of the content rather than of the running total, so that a table
    // which overflows only because a bar lies across it gets exactly the travel
    // that moves it out from under the bar and not a row of blank on top.
    if content.height > view.height {
        height += overscroll;
    }
    if height > view.height {
        width += SCROLLBAR_FOOTPRINT;
    }
    Point {
        x: (width - view.width).max(px(0.)),
        y: (height - view.height).max(px(0.)),
    }
}

#[cfg(test)]
mod tests {
    use super::{max_scroll_of, SCROLLBAR_FOOTPRINT};
    use gpui::{px, size, Pixels};

    const ROW: Pixels = px(24.);

    #[test]
    fn content_that_fits_cannot_be_scrolled() {
        // Neither the scrollbar allowance nor the overscroll may turn a table
        // that fits into one that drifts under the pointer.
        let max = max_scroll_of(size(px(300.), px(200.)), size(px(400.), px(300.)), ROW);
        assert_eq!(max.x, px(0.));
        assert_eq!(max.y, px(0.));
    }

    #[test]
    fn the_rows_can_be_dragged_a_row_past_their_own_end() {
        // Ten 24px rows in a 100px body: the six rows' worth that do not fit,
        // and then one row of blank so the last one can be pulled off the edge.
        let max = max_scroll_of(size(px(300.), px(240.)), size(px(400.), px(100.)), ROW);
        assert_eq!(max.y, px(140.) + ROW);
    }

    #[test]
    fn there_is_no_blank_past_the_last_column() {
        let max = max_scroll_of(size(px(900.), px(240.)), size(px(400.), px(100.)), ROW);
        assert_eq!(max.x, px(500.) + SCROLLBAR_FOOTPRINT);
    }

    #[test]
    fn a_scrollbar_on_one_axis_buys_travel_on_the_other() {
        // Wide enough to need a horizontal bar, which floats over the last row,
        // so the vertical axis gets the bar's thickness back as travel.
        let max = max_scroll_of(size(px(900.), px(240.)), size(px(400.), px(100.)), ROW);
        assert_eq!(max.y, px(140.) + SCROLLBAR_FOOTPRINT + ROW);
    }

    /// The bar is the reason rows that *almost* fit still have to scroll: it
    /// covers the bottom of the last one without making the content any taller.
    #[test]
    fn rows_that_fit_but_lie_under_the_horizontal_bar_can_still_be_moved() {
        let max = max_scroll_of(size(px(900.), px(240.)), size(px(400.), px(244.)), ROW);
        assert_eq!(max.y, SCROLLBAR_FOOTPRINT - px(4.));
    }
}

#[cfg(test)]
mod reload_tests {
    use db::{Column, ColumnData, ColumnMeta, NullMask, ResultSet, ValueKind};
    use gpui::{px, AppContext as _, Entity, TestAppContext};

    use super::Grid;

    fn column(name: &str, rows: usize) -> Column {
        let mut nulls = NullMask::with_capacity(rows);
        for row in 0..rows {
            nulls.push(false, row);
        }
        Column {
            meta: ColumnMeta::new(name.to_string(), ValueKind::Int, "int8"),
            nulls,
            data: ColumnData::I64((0..rows as i64).collect()),
        }
    }

    fn users(rows: usize) -> ResultSet {
        ResultSet::new(vec![column("id", rows), column("age", rows)])
    }

    fn grid(cx: &mut TestAppContext) -> Entity<Grid> {
        cx.update(|cx| ui::Theme::set_global(ui::Theme::of(ui::Appearance::Dark), cx));
        cx.update(|cx| cx.new(|cx| Grid::new(users(4), cx)))
    }

    /// What a sort, a page turn and a re-run all look like from here: the same
    /// columns, different rows.
    #[gpui::test]
    fn the_same_columns_coming_back_keep_their_width_and_the_view_on_them(cx: &mut TestAppContext) {
        let grid = grid(cx);
        grid.update(cx, |grid, cx| {
            grid.columns[1].width = px(300.);
            grid.columns[1].user_sized = true;
            grid.columns[1].pinned = true;
            grid.scroll.x = px(120.);
            grid.scroll.y = px(80.);

            grid.set_data(users(4), cx);

            assert_eq!(grid.columns[1].width, px(300.));
            assert!(grid.columns[1].pinned);
            assert_eq!(grid.scroll.x, px(120.));
            // The rows are new, so where you were down them is not.
            assert_eq!(grid.scroll.y, px(0.));
        });
    }

    /// What a click on another row does, in the order the press handler does
    /// it. The edit has to be closed out *before* the cursor moves, or it
    /// stages what was typed onto whichever cell the pointer landed on.
    #[gpui::test]
    fn ending_an_edit_stages_it_on_the_cell_it_was_opened_over(cx: &mut TestAppContext) {
        let grid = grid(cx);
        grid.update(cx, |grid, cx| {
            grid.set_editable(true, cx);
            grid.begin_edit(0, 1, Some("42"), cx);

            grid.stage_edit(cx);
            grid.set_cursor(3, 0, false, cx);

            assert!(!grid.is_editing());
            assert_eq!(grid.cell_value(0, 1), Some(db::Value::Int(42)));
            assert_eq!(grid.cell_value(3, 1), Some(db::Value::Int(3)));
        });
    }

    #[gpui::test]
    fn a_different_answer_starts_the_columns_over(cx: &mut TestAppContext) {
        let grid = grid(cx);
        grid.update(cx, |grid, cx| {
            grid.columns[1].width = px(300.);
            grid.scroll.x = px(120.);

            grid.set_data(ResultSet::new(vec![column("total", 4)]), cx);

            assert_eq!(grid.columns.len(), 1);
            assert_eq!(grid.scroll.x, px(0.));
        });
    }
}
