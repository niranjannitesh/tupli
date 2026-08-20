//! Context menus.
//!
//! The one floating layer this app has. It exists because the destructive
//! object actions — truncate, drop, rename — have nowhere else to live: they
//! are per-object, they are rare, and putting them on a toolbar would leave a
//! Drop button permanently one slip away from the pointer.
//!
//! State lives in the host, like [`crate::Tab`] and [`crate::ListItem`]: the
//! host decides a menu is open, at which point, with which entries, and gets
//! told when it should stop being open. That keeps this a `RenderOnce` element
//! with no entity, no focus handle and no subscription, and it keeps "which
//! object is this menu about" in the one place that can answer it.
//!
//! The menu paints itself over the whole window rather than inside its opener:
//! a menu clipped by the sidebar it was opened from would be a menu with three
//! of its items missing. The host is expected to render it last, so it lands on
//! top of everything else.

use std::rc::Rc;

use gpui::{
    div, prelude::*, px, App, ClickEvent, ElementId, IntoElement, MouseButton, Pixels, Point,
    RenderOnce, SharedString, Window,
};

use crate::{h_flex, v_flex, ActiveTheme, Icon, IconColor, IconName, IconSize, Label, LabelSize};

// The same shape [`crate::ListItem`] uses, so a handler written for a row can
// be moved onto a menu item without being rewritten — and so `cx.listener`
// takes it as-is.
type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

pub struct MenuItem {
    label: SharedString,
    icon: Option<IconName>,
    /// The keystroke that does the same thing, right-aligned. Only ever a
    /// reminder: the binding itself belongs to the window, not to this row.
    shortcut: Option<SharedString>,
    /// Red text. For the items that destroy something — and only for those, or
    /// the colour stops meaning anything.
    danger: bool,
    disabled: bool,
    on_click: Option<Handler>,
}

impl MenuItem {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            shortcut: None,
            danger: false,
            disabled: false,
            on_click: None,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }
}

enum Entry {
    Item(MenuItem),
    Separator,
}

#[derive(IntoElement)]
pub struct ContextMenu {
    id: ElementId,
    /// Where the pointer was, in window coordinates.
    at: Point<Pixels>,
    entries: Vec<Entry>,
    width: Pixels,
    on_dismiss: Option<Handler>,
}

/// Tall enough to hit without aiming, short enough that eight items still read
/// as one list. Deliberately below [`crate::Metrics::row_height`]: a menu is
/// denser than a tree.
const ITEM_HEIGHT: Pixels = px(24.);
const PADDING_Y: Pixels = px(4.);
const SEPARATOR_HEIGHT: Pixels = px(7.);

impl ContextMenu {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            at: Point::default(),
            entries: Vec::new(),
            width: px(200.),
            on_dismiss: None,
        }
    }

    /// Where the pointer was when the menu was asked for.
    pub fn at(mut self, at: Point<Pixels>) -> Self {
        self.at = at;
        self
    }

    pub fn item(mut self, item: MenuItem) -> Self {
        self.entries.push(Entry::Item(item));
        self
    }

    /// Zero or more, which is what an `Option` is: a verb that only some rows
    /// have is `.items(condition.then(|| …))` rather than a menu built in
    /// pieces around an `if`.
    pub fn items(mut self, items: impl IntoIterator<Item = MenuItem>) -> Self {
        self.entries.extend(items.into_iter().map(Entry::Item));
        self
    }

    /// A hairline. Two in a row, or one at either end, are dropped when the
    /// menu is built, so a caller can add one after a group that turned out to
    /// be empty without having to think about it.
    pub fn separator(mut self) -> Self {
        self.entries.push(Entry::Separator);
        self
    }

    pub fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }

    /// Called when the menu should close: a click outside it, or a click on one
    /// of its items after that item has done its work.
    pub fn on_dismiss(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(f));
        self
    }

    /// How tall the menu will be. Computed rather than measured because the
    /// position has to be decided before layout runs, and every row here is a
    /// fixed height by construction.
    fn height(&self) -> Pixels {
        let content: Pixels = self
            .entries
            .iter()
            .map(|entry| match entry {
                Entry::Item(_) => ITEM_HEIGHT,
                Entry::Separator => SEPARATOR_HEIGHT,
            })
            .fold(px(0.), |total, height| total + height);
        content + PADDING_Y * 2.
    }
}

/// A [`ClickEvent`] from the release half of one. The handler shape is shared
/// with [`crate::ListItem`], which is worth a synthesised event here: every
/// caller of both would otherwise need two versions of the same closure.
fn click_of(up: &gpui::MouseUpEvent) -> ClickEvent {
    ClickEvent::Mouse(gpui::MouseClickEvent {
        down: gpui::MouseDownEvent {
            button: up.button,
            position: up.position,
            modifiers: up.modifiers,
            click_count: up.click_count,
            first_mouse: false,
        },
        up: up.clone(),
    })
}

fn click_of_down(down: &gpui::MouseDownEvent) -> ClickEvent {
    ClickEvent::Mouse(gpui::MouseClickEvent {
        down: down.clone(),
        up: gpui::MouseUpEvent {
            button: down.button,
            position: down.position,
            modifiers: down.modifiers,
            click_count: down.click_count,
        },
    })
}

/// Drop leading, trailing and doubled separators.
fn tidy(entries: Vec<Entry>) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::with_capacity(entries.len());
    for entry in entries {
        match entry {
            Entry::Separator if out.is_empty() => continue,
            Entry::Separator if matches!(out.last(), Some(Entry::Separator)) => continue,
            entry => out.push(entry),
        }
    }
    if matches!(out.last(), Some(Entry::Separator)) {
        out.pop();
    }
    out
}

impl RenderOnce for ContextMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors().clone();
        let m = cx.metrics().clone();
        let viewport = window.viewport_size();
        let (width, height) = (self.width, self.height());
        let dismiss = self.on_dismiss.clone();

        // Flip rather than clamp when there is not room: a menu that has been
        // slid up the screen covers the thing it is about, where one that opens
        // upwards from the pointer still points at it.
        let margin = px(6.);
        let left = match self.at.x + width + margin > viewport.width {
            true => (self.at.x - width).max(margin),
            false => self.at.x,
        };
        let top = match self.at.y + height + margin > viewport.height {
            true => (self.at.y - height).max(margin),
            false => self.at.y,
        };

        let entries = tidy(self.entries);
        // Collected rather than lazy: every row clones the dismiss handle, and
        // a lazy iterator would still be holding a borrow of it when the
        // backdrop below needs to move it.
        let rows: Vec<_> = entries
            .into_iter()
            .map(|entry| match entry {
                Entry::Separator => div()
                    .h(SEPARATOR_HEIGHT)
                    .my(px(3.))
                    .mx(px(6.))
                    .border_b_1()
                    .border_color(c.border)
                    .into_any_element(),
                Entry::Item(item) => {
                    let colour = match (item.disabled, item.danger) {
                        (true, _) => c.text_disabled,
                        (false, true) => c.danger,
                        (false, false) => c.text,
                    };
                    let mut row = h_flex()
                        .h(ITEM_HEIGHT)
                        .px(px(8.))
                        .gap(px(6.))
                        .rounded(m.radius_sm)
                        .text_color(colour)
                        .children(item.icon.map(|icon| {
                            Icon::new(icon).size(IconSize::XSmall).color(
                                match (item.disabled, item.danger) {
                                    (true, _) => IconColor::Disabled,
                                    (false, true) => IconColor::Danger,
                                    (false, false) => IconColor::Muted,
                                },
                            )
                        }))
                        .child(
                            Label::new(item.label)
                                .flex_1()
                                .min_w_0()
                                // The row already carries the colour; a Label that
                                // set its own would fight it.
                                .color(IconColor::Inherit),
                        )
                        .children(item.shortcut.map(|keys| {
                            Label::new(keys)
                                .size(LabelSize::Small)
                                .color(IconColor::Subtle)
                        }));
                    if !item.disabled {
                        let dismiss = dismiss.clone();
                        let handler = item.on_click.clone();
                        row = row
                            .hover(|s| s.bg(c.hover))
                            .cursor_pointer()
                            // Mouse *up*, not down: the press that opened a menu
                            // and the release that chose from it are one gesture,
                            // and acting on the press would fire an item the moment
                            // the menu appeared under the pointer.
                            .on_mouse_up(MouseButton::Left, move |e, window, cx| {
                                let click = click_of(e);
                                if let Some(handler) = handler.as_ref() {
                                    handler(&click, window, cx);
                                }
                                if let Some(dismiss) = dismiss.as_ref() {
                                    dismiss(&click, window, cx);
                                }
                                cx.stop_propagation();
                            });
                    }
                    row.into_any_element()
                }
            })
            .collect();

        // The backdrop is the whole window and catches the click that closes
        // the menu. No scrim: a menu is a way to act on something, and dimming
        // what you are about to act on helps nobody.
        let backdrop_dismiss = dismiss.clone();
        div()
            .id(self.id)
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(MouseButton::Left, move |e, window, cx| {
                if let Some(dismiss) = backdrop_dismiss.as_ref() {
                    dismiss(&click_of_down(e), window, cx);
                }
            })
            .on_mouse_down(MouseButton::Right, move |e, window, cx| {
                if let Some(dismiss) = dismiss.as_ref() {
                    dismiss(&click_of_down(e), window, cx);
                }
            })
            .child(
                v_flex()
                    .absolute()
                    .left(left)
                    .top(top)
                    .w(width)
                    .py(PADDING_Y)
                    .bg(c.overlay)
                    .rounded(m.radius_lg)
                    .border_1()
                    .border_color(c.border_strong)
                    .shadow_lg()
                    // Keeps the press that lands on the menu itself from
                    // reaching the backdrop and closing it before the release.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .children(rows),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(entries: &[Entry]) -> Vec<&'static str> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Item(_) => "item",
                Entry::Separator => "sep",
            })
            .collect()
    }

    #[test]
    fn separators_that_separate_nothing_are_dropped() {
        let entries = vec![
            Entry::Separator,
            Entry::Item(MenuItem::new("Open")),
            Entry::Separator,
            Entry::Separator,
            Entry::Item(MenuItem::new("Drop")),
            Entry::Separator,
        ];
        assert_eq!(kinds(&tidy(entries)), vec!["item", "sep", "item"]);
    }

    #[test]
    fn a_menu_of_nothing_but_separators_is_empty() {
        assert!(tidy(vec![Entry::Separator, Entry::Separator]).is_empty());
    }

    #[test]
    fn height_counts_every_row_and_the_padding() {
        let menu = ContextMenu::new("m")
            .item(MenuItem::new("a"))
            .separator()
            .item(MenuItem::new("b"));
        assert_eq!(
            menu.height(),
            ITEM_HEIGHT * 2. + SEPARATOR_HEIGHT + PADDING_Y * 2.
        );
    }
}
