//! The centre's pane tree: how the editors are arranged, and nothing else.
//!
//! `PaneGroup` is a plain tree of ids with a fraction per member. It knows how
//! to split, how to close, and how to move a seam; it knows nothing about what
//! a pane contains, which is what lets it be tested without a window and
//! rendered without a special case. The panes themselves live in
//! [`crate::workspace::Workspace`], keyed by the ids this tree holds.
//!
//! Modelled on Zed's `PaneGroup` (docs/PLAN.md §9.4), with one deliberate
//! difference: a member's flex is a fraction of its group and the fractions in
//! a group sum to one, so a group can be laid out with `flex_basis` percentages
//! and a drag is arithmetic on two numbers rather than on the whole row.

use std::sync::Arc;

use gpui::{Entity, SharedString};
use ui::Axis;

use editor::{Editor, Input};
use grid::Grid;

use crate::results::ResultsTab;

/// A pane's handle. Ids are never reused inside a window, so a stale id names
/// a pane that is gone rather than someone else's.
pub type PaneId = usize;

/// Which way a group lays its members out.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Side by side. A split-right makes one of these.
    Columns,
    /// Stacked. A split-down makes one of these.
    Rows,
}

impl Layout {
    /// The way a seam between two members of this group runs. A column layout
    /// is separated by a vertical seam, which is the axis a `ResizeHandle`
    /// wants.
    pub fn seam(self) -> Axis {
        match self {
            Layout::Columns => Axis::Vertical,
            Layout::Rows => Axis::Horizontal,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Member {
    Pane(PaneId),
    Group(PaneGroup),
}

/// The smallest fraction of its group a member can be dragged to. Small enough
/// to be a sliver, large enough that a pane can always be grabbed back.
const MIN_FLEX: f32 = 0.1;

#[derive(Clone, Debug, PartialEq)]
pub struct PaneGroup {
    pub layout: Layout,
    pub members: Vec<Member>,
    /// One fraction per member, summing to 1.
    pub flexes: Vec<f32>,
}

impl PaneGroup {
    /// A window's first tree: one pane, no seams.
    pub fn new(root: PaneId) -> Self {
        Self {
            layout: Layout::Columns,
            members: vec![Member::Pane(root)],
            flexes: vec![1.],
        }
    }

    pub fn len(&self) -> usize {
        self.members
            .iter()
            .map(|member| match member {
                Member::Pane(_) => 1,
                Member::Group(group) => group.len(),
            })
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Every pane, left to right and top to bottom — the order a reader would
    /// find them in, which is also the order ⌘⌥→ should walk.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        for member in &self.members {
            match member {
                Member::Pane(id) => out.push(*id),
                Member::Group(group) => group.collect(out),
            }
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        self.panes().contains(&id)
    }

    /// Put `new` beside `target`. Returns false if `target` is not in the tree,
    /// in which case nothing has changed.
    ///
    /// Splitting along the layout the pane already lives in adds a member to
    /// that group and takes the room out of the pane being split, so its
    /// neighbours keep the width they were given. Splitting the other way
    /// turns the pane into a group of two.
    pub fn split(&mut self, target: PaneId, new: PaneId, layout: Layout) -> bool {
        let only = self.members.len() == 1;
        for index in 0..self.members.len() {
            match &mut self.members[index] {
                Member::Pane(id) if *id == target => {
                    if self.layout == layout || only {
                        self.layout = layout;
                        let half = self.flexes[index] / 2.;
                        self.flexes[index] = half;
                        self.flexes.insert(index + 1, half);
                        self.members.insert(index + 1, Member::Pane(new));
                    } else {
                        self.members[index] = Member::Group(PaneGroup {
                            layout,
                            members: vec![Member::Pane(target), Member::Pane(new)],
                            flexes: vec![0.5, 0.5],
                        });
                    }
                    return true;
                }
                Member::Group(group) => {
                    if group.split(target, new, layout) {
                        return true;
                    }
                }
                Member::Pane(_) => {}
            }
        }
        false
    }

    /// Take a pane out of the tree, handing its room to its siblings in the
    /// proportions they already had. A group left holding one member is
    /// dissolved into its parent — otherwise closing panes would leave a tree
    /// of groups nesting nothing, and the next split would land in the wrong
    /// one.
    ///
    /// Returns false if the pane is not here. Removing the last pane of the
    /// root leaves an empty root; the caller is expected not to do that.
    pub fn remove(&mut self, target: PaneId) -> bool {
        for index in 0..self.members.len() {
            match &mut self.members[index] {
                Member::Pane(id) if *id == target => {
                    self.members.remove(index);
                    let freed = self.flexes.remove(index);
                    self.spread(freed);
                    return true;
                }
                Member::Group(group) => {
                    if group.remove(target) {
                        if group.members.len() == 1 {
                            let only = group.members.remove(0);
                            self.members[index] = only;
                        }
                        return true;
                    }
                }
                Member::Pane(_) => {}
            }
        }
        false
    }

    /// Hand `freed` to whatever is left, in proportion.
    fn spread(&mut self, freed: f32) {
        let total: f32 = self.flexes.iter().sum();
        if self.flexes.is_empty() || total <= 0. {
            return;
        }
        for flex in self.flexes.iter_mut() {
            *flex += freed * (*flex / total);
        }
    }

    /// The group at `path`, where each step is an index into `members`.
    pub fn group_at(&mut self, path: &[usize]) -> Option<&mut PaneGroup> {
        match path.split_first() {
            None => Some(self),
            Some((index, rest)) => match self.members.get_mut(*index)? {
                Member::Group(group) => group.group_at(rest),
                Member::Pane(_) => None,
            },
        }
    }

    /// Move the seam after `index` by `delta` fractions of the group, taking
    /// from one neighbour and giving to the other.
    pub fn resize(&mut self, path: &[usize], index: usize, delta: f32) {
        let from = self
            .group_at(path)
            .and_then(|group| group.flexes.get(index).copied());
        if let Some(from) = from {
            self.resize_to(path, index, from + delta);
        }
    }

    /// Put the seam after `index` at `flex` fractions of its group, taking the
    /// rest from its neighbour. Both keep at least [`MIN_FLEX`], so a drag
    /// past the end stops rather than collapsing a pane to nothing a mouse
    /// could never find again.
    ///
    /// Absolute rather than relative because that is what a drag needs: the
    /// mouse says where the seam is now, and adding up hundreds of small
    /// deltas would drift.
    pub fn resize_to(&mut self, path: &[usize], index: usize, flex: f32) {
        let Some(group) = self.group_at(path) else {
            return;
        };
        if index + 1 >= group.flexes.len() {
            return;
        }
        let room = group.flexes[index] + group.flexes[index + 1];
        let next = flex.clamp(MIN_FLEX, room - MIN_FLEX);
        group.flexes[index] = next;
        group.flexes[index + 1] = room - next;
    }
}

/// What a centre-stack tab contains.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CenterKind {
    /// A saved or scratch SQL script.
    Query,
    /// A table opened for browsing.
    Table,
    /// A table's structure editor.
    Structure,
    /// One key of a key-value server, opened for browsing.
    Key,
}

/// A chip in the middle of being written.
///
/// Adding and editing are the same screen: the `+` opens an empty one, clicking
/// a chip opens it filled in. `editing` is which chip goes back when it is
/// committed, and `None` means append.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Composer {
    pub editing: Option<usize>,
    pub chip: crate::filter::Chip,
}

pub struct CenterTab {
    pub kind: CenterKind,
    pub title: SharedString,
    pub detail: Option<SharedString>,
    pub dirty: bool,
    /// The relation this tab is browsing, for the tabs that are browsing one.
    /// The filter and the refresh button both need to re-issue the statement
    /// that filled the grid, and re-parsing the title to get there would be a
    /// guess.
    pub relation: Option<db::RelationRef>,
    /// The key this tab is browsing, and what it holds. The counterpart of
    /// `relation` for a server whose objects are keys: refresh and paging both
    /// need to re-read it, and the type comes along because it is what decides
    /// the reader — deriving it again from the name is a `TYPE` per refresh.
    pub key: Option<(std::sync::Arc<[u8]>, db::KeyType)>,
    /// The saved query this tab is editing, once it is editing one. ⌘S writes
    /// straight back to it; a tab with no id has to be named first.
    pub saved_query: Option<uuid::Uuid>,
    /// The tab's own script, held while some other tab is showing. There is one
    /// console per pane and one buffer per tab, so switching tabs swaps this
    /// with what the pane's editor is holding.
    pub sql: String,
    /// What is in the filter box above the rows, chips and all. On the tab and
    /// not on the pane, because a `where` written against `orders` is nonsense
    /// against `users` and switching tabs must not carry it across.
    pub filter: crate::filter::Filter,
    /// Which page of a browsed table this tab is showing, counted in pages of
    /// [`crate::settings::Settings::page_size`]. Browsing is the only thing
    /// that can page: the app wrote that `select`, so it may add an `offset`
    /// to it. A hand-written statement is the user's sentence and gets an
    /// `offset` appended to it over nobody's dead body.
    /// `None` until something turns one — that is the difference between a
    /// table that fits on one screen and one that is being paged, and it is
    /// what decides whether the statement carries an `order by`.
    pub page: Option<usize>,
    /// The structure editor, for the tabs that are one. Built when the tab is
    /// opened and not restored with it: a design half-typed when the app was
    /// quit is not something to put back and call the table's shape.
    pub(crate) structure: Option<Entity<crate::structure::StructureEditor>>,
    /// The connection this tab is talking to.
    ///
    /// On the tab and not on the window, which is the difference between an
    /// app you can keep two databases open in and one where choosing a
    /// database is a mode. Two tabs can hold two servers at once, each with
    /// its own catalog and its own rows; activating a tab is what makes its
    /// connection the one the sidebar and the titlebar are describing.
    /// `None` for a tab restored from the last run before anything has been
    /// connected, and for the demo window, which is talking to nobody.
    pub(crate) session: Option<Entity<crate::session::Session>>,
    /// Where this tab was connected when the app last quit, for a tab that has
    /// been restored and not yet looked at. Consumed the first time it is
    /// activated, which is when the connection behind it is actually opened —
    /// a window that comes back with tabs on four databases opens the one you
    /// are looking at, not four.
    pub(crate) reconnect: Option<(uuid::Uuid, String)>,
}

/// One statement's rows, kept so that a script's answers can be looked at one
/// at a time. Only statements that returned rows are here: an `update` reports
/// what it changed in the message log and has nothing for a grid to show.
pub(crate) struct StatementResult {
    pub sql: SharedString,
    pub rows: Arc<db::ResultSet>,
    pub elapsed: std::time::Duration,
    pub truncated: bool,
}

/// One editor and the result set it produced.
///
/// A window starts with one of these and gains another every time something is
/// split. Everything here is *per pane* — its tabs, its console, its grid, its
/// row count, its error — which is what makes two panes able to hold two
/// different queries against the same connection without either noticing the
/// other. The connection itself, the sidebar, the message log and the docks are
/// the window's, not the pane's, and stay on [`crate::workspace::Workspace`].
pub struct Pane {
    /// This pane's handle, as the tree knows it.
    pub id: PaneId,

    // ---- centre ----------------------------------------------------------
    pub tabs: Vec<CenterTab>,
    pub active_tab: usize,
    /// Where the tab strip is scrolled to. Held here rather than in the strip
    /// because activating a tab has to be able to bring it into view, and the
    /// strip is rebuilt from scratch every frame.
    pub tab_scroll: gpui::ScrollHandle,
    /// How wide the tab strip was the last time it was drawn. A `ScrollHandle`
    /// is never told its container resized, so a strip that was scrolled to
    /// show the active tab keeps that offset when a split halves the pane and
    /// the tab it was revealing ends up off the right-hand edge. Comparing
    /// against this is how the reveal is done again, and only then.
    pub(crate) tab_strip_width: gpui::Pixels,
    pub editor: Entity<Editor>,

    // ---- results ---------------------------------------------------------
    pub grid: Entity<Grid>,
    pub row_count: usize,
    pub selected_row: Option<usize>,
    pub selected_column: usize,
    /// The hand-written `where` box, for whichever tab is showing and has
    /// asked for it. One per pane rather than one per tab for the same reason
    /// there is one console per pane: the text belongs to the tab and lives on
    /// it ([`CenterTab::filter`]), and this is the widget it is loaded into.
    pub filter: Entity<Input>,
    /// The value being typed into the chip composer. A second box because it
    /// is a second question — `where` in one, `pro` in the other — and sharing
    /// one would mean clearing the clause every time a chip was edited.
    pub chip_value: Entity<Input>,
    /// The half-built chip, while the composer is open. `None` is the resting
    /// state: a row of finished chips and a `+`.
    pub composer: Option<Composer>,
    /// Which of the result dock's tabs is showing.
    pub results_tab: ResultsTab,
    /// Every row-returning answer the last run produced, oldest first.
    ///
    /// One statement makes one, and the Data tab is named after whatever
    /// produced it. A script makes as many as it has selects, and the dock
    /// grows a tab per result — the last one is not the only one anybody
    /// wanted, or they would not have run the others.
    pub(crate) results: Vec<StatementResult>,
    /// Which of [`Pane::results`] the grid is showing.
    pub(crate) result_index: usize,

    /// The statement the last run sent, and how long it took. `None` until
    /// something has actually been run in this pane.
    pub last_sql: Option<SharedString>,
    pub elapsed: Option<std::time::Duration>,
    /// The last failure, shown until the next run replaces it.
    pub error: Option<db::DbError>,
    /// Where in the buffer the statement that is running came from, as a char
    /// offset. Postgres reports a syntax error as a position within the text it
    /// was handed, and this is what turns that back into a place in the
    /// console. `None` for a statement nobody typed — a table opened from the
    /// tree, a query replayed from history.
    pub(crate) run_origin: Option<usize>,
    /// The statements ⌘⇧⏎ has not sent yet, each with its origin in the buffer.
    ///
    /// A script goes to the server one statement at a time — the extended
    /// protocol prepares exactly one — so the rest wait here and the next is
    /// sent when the last one lands. Emptied by a failure, because running the
    /// remaining nineteen statements of a script whose second one failed is
    /// almost never what anybody wanted.
    pub(crate) queue: std::collections::VecDeque<(String, usize)>,
    /// The rows exactly as the server sent them, kept so that clearing a
    /// client-side sort can put them back without asking again.
    pub(crate) unsorted: Option<Arc<db::ResultSet>>,
    /// A sort that has been asked for but whose rows have not arrived yet. The
    /// grid drops its arrow when new data lands, so this is what puts it back.
    pub(crate) pending_sort: Option<grid::Sort>,
    /// Whether the last result set hit the row cap.
    pub truncated: bool,
    /// `UPDATE 3` — what a statement that returned no rows did.
    pub affected: Option<u64>,

    // ---- editing ---------------------------------------------------------
    /// How a row of the shown result set is addressed for writes, or why it
    /// cannot be. Recomputed whenever the grid is handed different rows —
    /// editability is a property of the *result*, not of the connection.
    pub(crate) identity: Result<sqlgen::Identity, sqlgen::NotEditable>,
    /// The relation the shown rows came out of, when they came out of one.
    /// Kept beside the identity because generating a statement needs both and
    /// the tab that knows it may have been switched away from by then.
    pub(crate) editing_relation: Option<db::RelationRef>,
    /// The generated SQL, while the confirmation sheet is up.
    pub(crate) preview: Option<Vec<sqlgen::Statement>>,
}

impl Pane {
    /// A pane around entities the caller has already made — they need
    /// subscriptions wired to the workspace, which only the workspace can do,
    /// so building them is its job and holding them is this one's.
    pub fn new(
        id: PaneId,
        editor: Entity<Editor>,
        grid: Entity<Grid>,
        filter: Entity<Input>,
        chip_value: Entity<Input>,
    ) -> Self {
        Self {
            id,
            tabs: Vec::new(),
            active_tab: 0,
            tab_scroll: gpui::ScrollHandle::new(),
            tab_strip_width: gpui::Pixels::ZERO,
            editor,
            grid,
            row_count: 0,
            selected_row: None,
            selected_column: 0,
            filter,
            chip_value,
            composer: None,
            results_tab: ResultsTab::Data,
            results: Vec::new(),
            result_index: 0,
            last_sql: None,
            elapsed: None,
            error: None,
            run_origin: None,
            queue: std::collections::VecDeque::new(),
            unsorted: None,
            pending_sort: None,
            truncated: false,
            affected: None,
            identity: Err(sqlgen::NotEditable::NotATable),
            editing_relation: None,
            preview: None,
        }
    }

    /// The tab that is showing, if there is one. A pane with no tabs is a
    /// legal state — closing the last tab leaves one — and every reader of the
    /// active tab has to survive it.
    pub fn active(&self) -> Option<&CenterTab> {
        self.tabs.get(self.active_tab)
    }

    pub fn active_mut(&mut self) -> Option<&mut CenterTab> {
        self.tabs.get_mut(self.active_tab)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(group: &PaneGroup) -> Vec<PaneId> {
        group.panes()
    }

    #[test]
    fn a_new_tree_is_one_pane() {
        let group = PaneGroup::new(0);
        assert_eq!(ids(&group), vec![0]);
        assert_eq!(group.len(), 1);
    }

    #[test]
    fn splitting_right_puts_the_new_pane_next_to_the_old_one() {
        let mut group = PaneGroup::new(0);
        assert!(group.split(0, 1, Layout::Columns));
        assert_eq!(ids(&group), vec![0, 1]);
        assert_eq!(group.flexes, vec![0.5, 0.5]);
        assert_eq!(group.layout, Layout::Columns);
    }

    #[test]
    fn a_second_split_takes_its_room_from_the_pane_it_split() {
        let mut group = PaneGroup::new(0);
        group.split(0, 1, Layout::Columns);
        group.split(1, 2, Layout::Columns);
        assert_eq!(ids(&group), vec![0, 1, 2]);
        assert_eq!(group.flexes, vec![0.5, 0.25, 0.25]);
    }

    #[test]
    fn splitting_across_the_layout_nests_a_group() {
        let mut group = PaneGroup::new(0);
        group.split(0, 1, Layout::Columns);
        group.split(1, 2, Layout::Rows);
        assert_eq!(ids(&group), vec![0, 1, 2]);
        assert_eq!(group.members.len(), 2);
        assert_eq!(group.flexes, vec![0.5, 0.5]);
        match &group.members[1] {
            Member::Group(inner) => {
                assert_eq!(inner.layout, Layout::Rows);
                assert_eq!(inner.panes(), vec![1, 2]);
            }
            other => panic!("expected a nested group, got {other:?}"),
        }
    }

    #[test]
    fn splitting_a_pane_that_is_not_here_changes_nothing() {
        let mut group = PaneGroup::new(0);
        let before = group.clone();
        assert!(!group.split(9, 1, Layout::Rows));
        assert_eq!(group, before);
    }

    #[test]
    fn closing_a_pane_hands_its_room_to_the_others() {
        let mut group = PaneGroup::new(0);
        group.split(0, 1, Layout::Columns);
        group.split(1, 2, Layout::Columns);
        assert!(group.remove(1));
        assert_eq!(ids(&group), vec![0, 2]);
        let total: f32 = group.flexes.iter().sum();
        assert!((total - 1.).abs() < 1e-5, "flexes should still sum to one");
        // 0 had twice 2's room and should still have it.
        assert!(group.flexes[0] > group.flexes[1]);
    }

    #[test]
    fn a_group_left_holding_one_pane_dissolves() {
        let mut group = PaneGroup::new(0);
        group.split(0, 1, Layout::Columns);
        group.split(1, 2, Layout::Rows);
        assert!(group.remove(2));
        assert_eq!(ids(&group), vec![0, 1]);
        assert!(
            group.members.iter().all(|m| matches!(m, Member::Pane(_))),
            "the nested group should be gone: {:?}",
            group.members
        );
    }

    #[test]
    fn a_seam_moves_room_between_its_two_neighbours() {
        let mut group = PaneGroup::new(0);
        group.split(0, 1, Layout::Columns);
        group.resize(&[], 0, 0.2);
        assert!((group.flexes[0] - 0.7).abs() < 1e-5);
        assert!((group.flexes[1] - 0.3).abs() < 1e-5);
    }

    #[test]
    fn a_seam_dragged_past_the_end_stops() {
        let mut group = PaneGroup::new(0);
        group.split(0, 1, Layout::Columns);
        group.resize(&[], 0, 5.);
        assert!((group.flexes[0] - 0.9).abs() < 1e-5);
        assert!((group.flexes[1] - 0.1).abs() < 1e-5);
        assert!(group.flexes[1] >= MIN_FLEX);
    }

    #[test]
    fn a_seam_inside_a_nested_group_is_reachable_by_path() {
        let mut group = PaneGroup::new(0);
        group.split(0, 1, Layout::Columns);
        group.split(1, 2, Layout::Rows);
        group.resize(&[1], 0, 0.25);
        match &group.members[1] {
            Member::Group(inner) => {
                assert!((inner.flexes[0] - 0.75).abs() < 1e-5);
                assert!((inner.flexes[1] - 0.25).abs() < 1e-5);
            }
            other => panic!("expected a nested group, got {other:?}"),
        }
        // The outer seam is untouched.
        assert_eq!(group.flexes, vec![0.5, 0.5]);
    }

    #[test]
    fn a_column_group_is_separated_by_a_vertical_seam() {
        assert_eq!(Layout::Columns.seam(), Axis::Vertical);
        assert_eq!(Layout::Rows.seam(), Axis::Horizontal);
    }
}
