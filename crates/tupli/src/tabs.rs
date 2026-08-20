//! The tab strip's own menu: the close family, and the pin.
//!
//! A tab strip accumulates. You go looking for one table and open fourteen,
//! and the way out of that is not fourteen clicks on fourteen × buttons — it
//! is "close the others", which is a gesture about the strip rather than about
//! any one tab. That is what this file is: the verbs that take a whole strip
//! as their subject.
//!
//! The pin is what makes them safe to use. Without it "close the others" is a
//! gesture nobody reaches for, because the others include the scratch query
//! that has been open since morning. A pin says *this one survives whatever I
//! do to the rest*, so every bulk close here steps over pinned tabs — and the
//! single Close does not, because that one names its tab.

use gpui::{px, Context, IntoElement, Pixels, Point};
use ui::{ContextMenu, MenuItem};

use crate::pane::{CenterTab, PaneId};
use crate::workspace::Workspace;

/// An open tab menu: where it was asked for, and about which tab.
pub(crate) struct TabMenu {
    /// Window coordinates of the click that opened it.
    pub at: Point<Pixels>,
    pub pane: PaneId,
    pub index: usize,
}

impl Workspace {
    pub fn open_tab_menu(
        &mut self,
        at: Point<Pixels>,
        pane: PaneId,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        // Right-clicking a tab is also a way of pointing at it, which is what
        // makes every item below able to say "this tab" without a second click
        // having chosen it first.
        self.activate_pane(pane, cx);
        self.menu = None;
        self.row_menu = None;
        self.tab_menu = Some(TabMenu { at, pane, index });
        cx.notify();
    }

    pub(crate) fn close_tab_menu(&mut self, cx: &mut Context<Self>) {
        if self.tab_menu.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn render_tab_menu(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let menu = self.tab_menu.as_ref()?;
        let (at, pane, index) = (menu.at, menu.pane, menu.index);
        let tabs = &self.pane_by(pane)?.tabs;
        let subject = tabs.get(index)?;
        let pinned = subject.pinned;
        // Every count is of what the item would actually close, which is what
        // decides whether it is offered at all. A greyed "Close Others" over a
        // strip of one is the menu saying there is nothing else; a live one
        // over a strip whose others are all pinned would be a lie.
        let others = closable(tabs, |i, _| i != index).len();
        let left = closable(tabs, |i, _| i < index).len();
        let right = closable(tabs, |i, _| i > index).len();
        let unchanged = closable(tabs, |_, tab| !tab.dirty).len();
        let all = closable(tabs, |_, _| true).len();

        Some(
            ContextMenu::new("tab-menu")
                .at(at)
                .width(px(216.))
                .on_dismiss(cx.listener(|this, _, _, cx| this.close_tab_menu(cx)))
                .item(MenuItem::new("Close").shortcut("⌘W").on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.close_tab_menu(cx);
                        this.close_tab(index, cx);
                    },
                )))
                .item(
                    MenuItem::new("Close Others")
                        .shortcut("⌥⌘W")
                        .disabled(others == 0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_tab_menu(cx);
                            this.close_tabs_where(|i, _| i != index, cx);
                        })),
                )
                .separator()
                .item(
                    MenuItem::new("Close Left")
                        .disabled(left == 0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_tab_menu(cx);
                            this.close_tabs_where(|i, _| i < index, cx);
                        })),
                )
                .item(
                    MenuItem::new("Close Right")
                        .disabled(right == 0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_tab_menu(cx);
                            this.close_tabs_where(|i, _| i > index, cx);
                        })),
                )
                .separator()
                // "Unchanged" rather than Zed's "Clean", because the dot on a
                // tab here means an unsaved script or rows staged against a
                // table, and those are the two things this leaves standing.
                .item(
                    MenuItem::new("Close Unchanged")
                        .disabled(unchanged == 0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_tab_menu(cx);
                            this.close_tabs_where(|_, tab| !tab.dirty, cx);
                        })),
                )
                .item(
                    MenuItem::new("Close All")
                        .disabled(all == 0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_tab_menu(cx);
                            this.close_tabs_where(|_, _| true, cx);
                        })),
                )
                .separator()
                .item(
                    MenuItem::new(match pinned {
                        true => "Unpin Tab",
                        false => "Pin Tab",
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_tab_menu(cx);
                        this.toggle_pin(index, cx);
                    })),
                ),
        )
    }

    /// ⌥⌘W — Safari's and Chrome's binding, and the only one of these worth a
    /// key of its own: it is the one you reach for after going looking for
    /// something, which is when the strip is worst and the mouse is furthest
    /// from it.
    pub(crate) fn close_other_tabs(&mut self, cx: &mut Context<Self>) {
        let active = self.pane().active_tab;
        self.close_tabs_where(|i, _| i != active, cx);
    }

    /// Close every tab the predicate names — except a pinned one, which is
    /// what a pin is for.
    pub(crate) fn close_tabs_where(
        &mut self,
        doomed: impl Fn(usize, &CenterTab) -> bool,
        cx: &mut Context<Self>,
    ) {
        let doomed = closable(&self.pane().tabs, doomed);
        self.close_tabs(&doomed, cx);
    }

    /// Pin or unpin, and move the tab to where a pinned tab lives.
    ///
    /// The move is the point. A pin that left the tab in place would be a
    /// promise with no shape on screen, and the promise is "this one is at the
    /// front and it stays" — so pinning walks the tab to the end of the pinned
    /// run, and unpinning drops it at the start of what is left.
    pub fn toggle_pin(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.pane().tabs.len() {
            return;
        }
        let pinning = !self.pane().tabs[index].pinned;
        self.pane_mut().tabs[index].pinned = pinning;
        let pinned = self
            .pane()
            .tabs
            .iter()
            .enumerate()
            .filter(|(i, tab)| tab.pinned && *i != index)
            .count();
        self.move_tab(index, pinned, cx);
        self.save_session(cx);
        cx.notify();
    }

    /// Move one tab, keeping whichever tab was showing on screen.
    fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let len = self.pane().tabs.len();
        if from == to || from >= len || to >= len {
            return;
        }
        // The console's text belongs to the tab that is showing, and this is
        // the last moment the index it is filed under is still the old one.
        self.stash_editor(cx);
        let active = self.pane().active_tab;
        let pane = self.pane_mut();
        let tab = pane.tabs.remove(from);
        pane.tabs.insert(to, tab);
        pane.active_tab = index_after_move(active, from, to);
        pane.tab_scroll.scroll_to_item(pane.active_tab);
    }
}

/// The indices the predicate names that are actually closable — pinned tabs
/// are not.
fn closable(tabs: &[CenterTab], doomed: impl Fn(usize, &CenterTab) -> bool) -> Vec<usize> {
    tabs.iter()
        .enumerate()
        .filter(|(i, tab)| !tab.pinned && doomed(*i, tab))
        .map(|(i, _)| i)
        .collect()
}

/// Which tab should be showing once `doomed` are gone, in the numbering it had
/// before they went. `None` when the strip empties.
///
/// The active tab if it survives; otherwise its nearest surviving neighbour to
/// the right, and only failing that to the left. Rightwards first because the
/// tab that slides into the vacated slot is the one under the pointer, which is
/// what every editor with tabs does.
pub(crate) fn tab_after_closing(active: usize, doomed: &[usize], len: usize) -> Option<usize> {
    (0..len)
        .filter(|index| !doomed.contains(index))
        .min_by_key(|index| match *index >= active {
            true => (0, *index - active),
            false => (1, active - *index),
        })
}

/// Where the tab at `active` ends up once the tab at `from` has been taken out
/// and put back at `to`.
fn index_after_move(active: usize, from: usize, to: usize) -> usize {
    if active == from {
        return to;
    }
    // Everything between the two slots shifts one place towards the hole.
    match (
        from < to,
        active > from && active <= to,
        active >= to && active < from,
    ) {
        (true, true, _) => active - 1,
        (false, _, true) => active + 1,
        _ => active,
    }
}

#[cfg(test)]
mod tests {
    use super::{index_after_move, tab_after_closing};

    #[test]
    fn closing_a_tab_to_the_left_shifts_the_active_one_down() {
        // Old numbering: tab 2 survives, and it is the caller that renumbers.
        assert_eq!(tab_after_closing(2, &[0], 3), Some(2));
    }

    #[test]
    fn closing_a_tab_to_the_right_leaves_the_active_one_alone() {
        assert_eq!(tab_after_closing(0, &[2], 3), Some(0));
    }

    #[test]
    fn closing_the_active_last_tab_falls_back_onto_its_neighbour() {
        assert_eq!(tab_after_closing(2, &[2], 3), Some(1));
    }

    #[test]
    fn closing_the_active_tab_in_the_middle_takes_the_one_to_its_right() {
        assert_eq!(tab_after_closing(1, &[1], 3), Some(2));
    }

    #[test]
    fn closing_every_other_tab_leaves_the_one_that_was_showing() {
        assert_eq!(tab_after_closing(3, &[0, 1, 2, 4], 5), Some(3));
    }

    #[test]
    fn closing_a_run_that_swallows_the_active_tab_lands_past_it() {
        assert_eq!(tab_after_closing(1, &[0, 1, 2], 5), Some(3));
    }

    #[test]
    fn closing_a_run_at_the_end_falls_back_to_the_left() {
        assert_eq!(tab_after_closing(3, &[2, 3, 4], 5), Some(1));
    }

    #[test]
    fn closing_the_lot_leaves_nothing_to_show() {
        assert_eq!(tab_after_closing(0, &[0, 1], 2), None);
    }

    #[test]
    fn pinning_a_tab_carries_the_active_one_with_it() {
        // The third tab pinned to the front: it is the one showing, so it is
        // the one that has to still be showing afterwards.
        assert_eq!(index_after_move(2, 2, 0), 0);
    }

    #[test]
    fn a_tab_moved_past_the_active_one_shifts_it_a_place() {
        // Tab 0 goes to the end: everything it passed slides down one.
        assert_eq!(index_after_move(1, 0, 3), 0);
        // Tab 3 goes to the front: everything it passed slides up one.
        assert_eq!(index_after_move(1, 3, 0), 2);
    }

    #[test]
    fn a_move_that_misses_the_active_tab_leaves_it_where_it_was() {
        assert_eq!(index_after_move(0, 2, 3), 0);
        assert_eq!(index_after_move(3, 0, 2), 3);
    }
}
