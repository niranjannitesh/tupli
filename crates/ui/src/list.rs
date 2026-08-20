//! Rows.
//!
//! The schema tree, the Redis key tree, completion popups, menus and the
//! connection list are all the same object: a fixed-height row with an optional
//! disclosure affordance, an icon, a label, and a trailing slot. Sharing one
//! implementation is what keeps hover, selection and indentation identical
//! everywhere — the details a user notices only when they disagree.

use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, ElementId, IntoElement, MouseButton,
    RenderOnce, SharedString, StyleRefinement, Styled, Window,
};
use smallvec::SmallVec;

use crate::{
    h_flex, ActiveTheme, ElidedLabel, Icon, IconColor, IconName, IconSize, Label, LabelSize,
    StyledExt,
};

type Handler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// State of a row's disclosure triangle.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Disclosure {
    /// Has children, currently collapsed.
    Collapsed,
    /// Has children, currently expanded.
    Expanded,
    /// A leaf: reserve the space so labels still line up, but draw nothing.
    Leaf,
    /// Not a tree at all: reserve nothing.
    None,
}

#[derive(IntoElement)]
pub struct ListItem {
    id: ElementId,
    label: SharedString,
    /// Dim text after the label — row counts, key counts, types.
    meta: Option<SharedString>,
    icon: Option<IconName>,
    icon_color: IconColor,
    /// Extra element between the icon and the label, e.g. a Redis type badge.
    prefix: Option<AnyElement>,
    end: SmallVec<[AnyElement; 2]>,
    /// Tree depth. Multiplied by `Metrics::tree_indent`.
    indent: usize,
    disclosure: Disclosure,
    selected: bool,
    /// Selected, but the owning pane is not focused — muted highlight.
    inactive: bool,
    /// Render the label in the monospace family.
    mono: bool,
    /// Elide the label's middle rather than its end.
    elide_middle: bool,
    height: Option<gpui::Pixels>,
    on_click: Option<Handler>,
    on_toggle: Option<Handler>,
    on_secondary: Option<Handler>,
    style: StyleRefinement,
}

impl ListItem {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            meta: None,
            icon: None,
            icon_color: IconColor::Muted,
            prefix: None,
            end: SmallVec::new(),
            indent: 0,
            disclosure: Disclosure::None,
            selected: false,
            inactive: false,
            mono: false,
            elide_middle: false,
            height: None,
            on_click: None,
            on_toggle: None,
            on_secondary: None,
            style: StyleRefinement::default(),
        }
    }

    pub fn meta(mut self, meta: impl Into<SharedString>) -> Self {
        self.meta = Some(meta.into());
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn icon_color(mut self, color: IconColor) -> Self {
        self.icon_color = color;
        self
    }

    pub fn prefix(mut self, prefix: impl IntoElement) -> Self {
        self.prefix = Some(prefix.into_any_element());
        self
    }

    pub fn end_child(mut self, child: impl IntoElement) -> Self {
        self.end.push(child.into_any_element());
        self
    }

    pub fn indent(mut self, level: usize) -> Self {
        self.indent = level;
        self
    }

    pub fn disclosure(mut self, disclosure: Disclosure) -> Self {
        self.disclosure = disclosure;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn inactive(mut self, inactive: bool) -> Self {
        self.inactive = inactive;
        self
    }

    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    /// Elide the middle of the label rather than its end.
    ///
    /// For a schema tree, where the names in one list are written to a
    /// convention and share their beginnings: `conversation_analytics` and
    /// `conversation_interactions` elide to the same row, and a tree that
    /// shows the same row four times is not showing four tables.
    pub fn elide_middle(mut self) -> Self {
        self.elide_middle = true;
        self
    }

    pub fn height(mut self, height: gpui::Pixels) -> Self {
        self.height = Some(height);
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }

    /// Fires when the disclosure triangle itself is clicked, so that clicking the
    /// row selects without collapsing.
    pub fn on_toggle(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Box::new(f));
        self
    }

    pub fn on_secondary_click(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_secondary = Some(Box::new(f));
        self
    }
}

impl Styled for ListItem {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for ListItem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors().clone();
        let m = cx.metrics().clone();
        let height = self.height.unwrap_or(m.row_height);
        let selected = self.selected;

        let bg = if selected {
            Some(if self.inactive {
                c.selected_inactive
            } else {
                c.selected
            })
        } else {
            None
        };

        let toggle_id = ElementId::Name(format!("{:?}-toggle", self.id).into());
        let disclosure = match self.disclosure {
            Disclosure::None => None,
            Disclosure::Leaf => Some(div().flex_none().size(px(14.)).into_any_element()),
            state => {
                let icon = if state == Disclosure::Expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                };
                let handler = self.on_toggle;
                Some(
                    div()
                        .id(toggle_id)
                        .flex_none()
                        .size(px(14.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when_some(handler, |el, handler| {
                            el.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(move |e, window, cx| {
                                    cx.stop_propagation();
                                    handler(e, window, cx)
                                })
                        })
                        .child(
                            Icon::new(icon)
                                .size(IconSize::XSmall)
                                .color(IconColor::Subtle),
                        )
                        .into_any_element(),
                )
            }
        };

        let mut el = h_flex()
            .id(self.id)
            .h(height)
            .flex_none()
            // Inset from the panel edge and rounded, so the selection reads as a
            // chip on the row rather than as a stripe running out of the panel.
            .mx(px(4.))
            .rounded(m.radius)
            .pr(px(6.))
            .pl(px(6.) + m.tree_indent * self.indent)
            .gap(px(5.))
            .cursor_pointer()
            .when_some(bg, |el, bg| el.bg(bg))
            .when(!selected, |el| el.hover(move |s| s.bg(c.hover)));

        if let Some(handler) = self.on_click {
            el = el.on_click(move |e, window, cx| handler(e, window, cx));
        }
        if let Some(handler) = self.on_secondary {
            el = el.on_mouse_down(MouseButton::Right, move |e, window, cx| {
                let click = ClickEvent::Mouse(gpui::MouseClickEvent {
                    down: e.clone(),
                    up: gpui::MouseUpEvent {
                        button: e.button,
                        position: e.position,
                        modifiers: e.modifiers,
                        click_count: e.click_count,
                    },
                });
                handler(&click, window, cx);
            });
        }
        el.style().refine(&self.style);

        el.children(disclosure)
            .children(self.icon.map(|i| {
                Icon::new(i).size(IconSize::Small).color(if selected {
                    IconColor::Default
                } else {
                    self.icon_color
                })
            }))
            .children(self.prefix)
            .map(|el| match self.elide_middle {
                true => el.child(
                    ElidedLabel::new(self.label)
                        .when_mono(self.mono)
                        .color(IconColor::Default),
                ),
                false => el.child(
                    Label::new(self.label)
                        .when_true(self.mono, |l| l.mono())
                        .flex_1()
                        .min_w_0(),
                ),
            })
            .children(self.meta.map(|m| {
                Label::new(m)
                    .size(LabelSize::Small)
                    .color(IconColor::Subtle)
            }))
            .children(self.end)
    }
}
