//! Tab strips.
//!
//! Every region in the app — sidebar, centre stack, bottom dock, inspector — has
//! its own strip, and they all have to look identical or the window stops
//! reading as one program. So there is exactly one implementation, parameterised
//! only by whether the tabs can be closed.
//!
//! The active tab is a *notch*, not a pill: it is the height of the strip and
//! painted in the colour of the plane below, so its bottom edge does not exist
//! and the tab and its content read as one continuous surface. That only works
//! because the strip itself is a different plane from the content: `chrome`
//! above, `panel` below, the same relationship the titlebar has with the
//! window. An earlier version floated a bordered pill in a strip the same
//! colour as everything else, and the result was a button that happened to be
//! near some content rather than a label attached to it.
//!
//! Square corners, all four. A radius on the top two would be the tab drawing
//! its own outline again — a rounded lid on a shape whose whole point is that
//! it has no edges of its own — and at this size it reads as a smudge rather
//! than as a curve.
//!
//! One hairline does the whole job. It runs along the top of the strip and
//! along the bottom, and where the active tab is it *detours*: down the tab's
//! left edge, along the bottom of the strip it does not reach, back up the
//! right. So the tab has no outline of its own — what you see around it is the
//! seam between chrome and content, bent around the hole the tab cuts in it.
//! The bottom run is painted first and the tabs are painted over it, which is
//! why the tab needs no bottom edge to erase: it simply covers that stretch.
//!
//! The strip is where that pairing is enforced, which is why [`TabBar`] paints
//! `chrome` itself rather than leaving it to whatever region it was dropped in.

use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, ElementId, IntoElement, MouseButton,
    ParentElement, RenderOnce, SharedString, StyleRefinement, Styled, Window,
};
use smallvec::SmallVec;

use crate::{
    h_flex, ActiveTheme, Icon, IconColor, IconName, IconSize, Label, LabelSize, StyledExt,
};

type Handler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
/// A right click carries its position, because the menu it opens has to appear
/// where the pointer is rather than where the tab is.
type MenuHandler = Box<dyn Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Tab {
    id: ElementId,
    label: SharedString,
    /// Secondary text after the label, e.g. the connection a table belongs to.
    detail: Option<SharedString>,
    icon: Option<IconName>,
    icon_color: IconColor,
    active: bool,
    /// Share the strip's width equally with its siblings instead of sizing to
    /// content. Used by the fixed strips (sidebar, dock) where the tab set is
    /// known and a segmented control reads better than ragged widths.
    fill: bool,
    /// Draw a dot instead of the close button — unsaved script, uncommitted rows.
    dirty: bool,
    closable: bool,
    /// Pinned: the × becomes a pin, and the strip keeps this tab at its head.
    /// The icon is not a button — unpinning is a menu item, because a control
    /// that closes the tab in one state and keeps it in the other is a control
    /// nobody can click without looking first.
    pinned: bool,
    /// Whether this tab draws its own left / right stretch of the seam. Off at
    /// the outer ends of a strip that is flush against a region divider: the
    /// divider is already that line, and drawing it twice is a two-pixel edge
    /// that reads as a mistake next to every other hairline in the window.
    /// [`TabBar`] decides this; a caller has no way to know it.
    seam_l: bool,
    seam_r: bool,
    on_click: Option<Handler>,
    on_close: Option<Handler>,
    on_secondary: Option<MenuHandler>,
}

impl Tab {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: None,
            icon: None,
            icon_color: IconColor::Muted,
            active: false,
            fill: false,
            dirty: false,
            closable: false,
            pinned: false,
            seam_l: true,
            seam_r: true,
            on_click: None,
            on_close: None,
            on_secondary: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
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

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Equal-width segment rather than content-width tab.
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self
    }

    pub fn dirty(mut self, dirty: bool) -> Self {
        self.dirty = dirty;
        self
    }

    pub fn closable(mut self, closable: bool) -> Self {
        self.closable = closable;
        self
    }

    pub fn pinned(mut self, pinned: bool) -> Self {
        self.pinned = pinned;
        self
    }

    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }

    pub fn on_close(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Box::new(f));
        self
    }

    /// Right click, for the strip that has a menu behind its tabs.
    pub fn on_secondary(
        mut self,
        f: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_secondary = Some(Box::new(f));
        self
    }
}

impl RenderOnce for Tab {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors().clone();
        let active = self.active;
        let close_id = ElementId::Name(format!("{:?}-close", self.id).into());

        let trailing = if self.pinned {
            Some(
                Icon::new(IconName::Pin)
                    .size(IconSize::XSmall)
                    .color(match active {
                        true => IconColor::Muted,
                        false => IconColor::Subtle,
                    })
                    .into_any_element(),
            )
        } else if self.dirty {
            Some(
                div()
                    .flex_none()
                    .size(px(6.))
                    .rounded_full()
                    .bg(if active { c.accent } else { c.text_subtle })
                    .into_any_element(),
            )
        } else if self.closable {
            let handler = self.on_close;
            Some(
                div()
                    .id(close_id)
                    .flex_none()
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .hover(move |s| s.bg(c.active))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .when_some(handler, |el, handler| {
                        el.on_click(move |e, window, cx| {
                            cx.stop_propagation();
                            handler(e, window, cx)
                        })
                    })
                    .child(
                        Icon::new(IconName::XmarkSm)
                            .size(IconSize::XSmall)
                            .color(IconColor::Muted),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };

        h_flex()
            .id(self.id)
            // The full height of the strip, top hairline to bottom. A tab that
            // stopped short would have a bottom edge, and a bottom edge is the
            // one thing separating a tab from a button.
            .h_full()
            .when(self.fill, |el| {
                el.flex_1().min_w_0().justify_center().px(px(6.))
            })
            .when(!self.fill, |el| el.flex_none().px(px(9.)).max_w(px(240.)))
            .gap(px(6.))
            .cursor_pointer()
            // The two vertical stretches of the seam are painted rather than
            // bordered: a border would be one side of a box, and these two
            // sides are not a box — each is drawn or not drawn on its own, and
            // neither may change the tab's width when the tab is activated.
            .relative()
            .when(active, |el| el.bg(c.tab_active))
            .when(!active, |el| el.hover(move |s| s.bg(c.hover)))
            .children((active && self.seam_l).then(|| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(px(1.))
                    .bg(c.border)
            }))
            .children((active && self.seam_r).then(|| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .w(px(1.))
                    .bg(c.border)
            }))
            .when_some(self.on_click, |el, handler| {
                el.on_click(move |e, window, cx| handler(e, window, cx))
            })
            .when_some(self.on_secondary, |el, handler| {
                el.on_mouse_down(MouseButton::Right, move |e, window, cx| {
                    cx.stop_propagation();
                    handler(e, window, cx)
                })
            })
            .children(self.icon.map(|i| {
                // A colour the caller chose outranks the active state, because
                // the reason to colour a tab's icon is to be noticed from the
                // tab you are actually on: a red Messages icon that only turns
                // red once you have opened Messages has told you nothing.
                let color = match (self.icon_color == IconColor::Muted, active) {
                    (false, _) => self.icon_color,
                    (true, true) => IconColor::Muted,
                    (true, false) => IconColor::Subtle,
                };
                Icon::new(i).size(IconSize::Small).color(color)
            }))
            .child(
                Label::new(self.label)
                    .color(if active {
                        IconColor::Default
                    } else {
                        IconColor::Muted
                    })
                    .when_true(active, |l| l.medium())
                    // The name is what gives when the tab is too narrow for
                    // both halves. It is the longer of the two and the one the
                    // icon and the tab's position already hint at; the detail
                    // beside it is a schema or a database, and half of one of
                    // those says nothing at all.
                    .min_w_0(),
            )
            .children(self.detail.map(|d| {
                Label::new(d)
                    .size(LabelSize::Small)
                    .color(IconColor::Subtle)
                    .flex_none()
                    .max_w(px(80.))
            }))
            .children(trailing)
    }
}

/// The strip a [`Tab`] lives in. Holds leading and trailing action slots so that
/// e.g. the sidebar's `+` / refresh pair and the centre stack's split control sit
/// at the same baseline as the tabs themselves.
#[derive(IntoElement)]
pub struct TabBar {
    id: ElementId,
    tabs: SmallVec<[Tab; 8]>,
    start: SmallVec<[AnyElement; 2]>,
    end: SmallVec<[AnyElement; 4]>,
    /// Scroll the tabs sideways rather than clipping them, and the handle the
    /// owner reaches the scroll position through. Off by default: a strip whose
    /// tab set is fixed — the sidebar's three, the dock's four — cannot overflow
    /// and does not need a scroll region to prove it.
    scroll: Option<gpui::ScrollHandle>,
    /// Whether another strip is stacked directly beneath this one — the pane's
    /// tabs over the dock's, while the centre has collapsed to nothing but
    /// strips. The seam is still this strip's to draw, which is what keeps it
    /// the height of every other strip, but it is painted over the tabs rather
    /// than under them and the active tab gets no notch: a notch is a tab
    /// reaching down into its content, and what is below here is not that
    /// tab's content but another strip.
    stacked: bool,
    /// Whether another strip is stacked directly above. That one drew the line
    /// between them, so this one has no top hairline of its own — and is a
    /// pixel shorter for the pixel it did not have to spend on one, which is
    /// what puts its tabs at the height of everybody else's.
    nested: bool,
    style: StyleRefinement,
}

impl TabBar {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            tabs: SmallVec::new(),
            start: SmallVec::new(),
            end: SmallVec::new(),
            scroll: None,
            stacked: false,
            nested: false,
            style: StyleRefinement::default(),
        }
    }

    /// Another strip follows directly below, so the line between them is this
    /// one's to draw — the same line, in the same row, that this strip would
    /// draw against a panel. See [`TabBar::nested`] for the other half.
    pub fn stacked(mut self) -> Self {
        self.stacked = true;
        self
    }

    /// Another strip sits directly above, and has drawn the line between them.
    pub fn nested(mut self) -> Self {
        self.nested = true;
        self
    }

    /// Let the tabs scroll sideways when there are more of them than there is
    /// room for.
    ///
    /// Without this a fourth tab in a split pane is simply cut off, and the one
    /// in front can be the one that is cut off — a window showing a table
    /// nothing on screen names. The handle is the owner's, so that opening or
    /// activating a tab can also bring it into view; a mouse or a trackpad
    /// reaches it on its own.
    pub fn track_scroll(mut self, scroll: gpui::ScrollHandle) -> Self {
        self.scroll = Some(scroll);
        self
    }

    pub fn tab(mut self, tab: Tab) -> Self {
        self.tabs.push(tab);
        self
    }

    pub fn tabs(mut self, tabs: impl IntoIterator<Item = Tab>) -> Self {
        self.tabs.extend(tabs);
        self
    }

    /// Controls pinned before the first tab.
    pub fn start_child(mut self, child: impl IntoElement) -> Self {
        self.start.push(child.into_any_element());
        self
    }

    /// Controls pinned after the last tab, right-aligned.
    pub fn end_child(mut self, child: impl IntoElement) -> Self {
        self.end.push(child.into_any_element());
        self
    }
}

impl Styled for TabBar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for TabBar {
    fn render(mut self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors();
        // The metric is the band between one hairline and the next, and a
        // nested strip inherits the first of those from the strip above. So it
        // spends nothing on a top border and stands a pixel shorter, which
        // leaves its tabs exactly as tall as the tabs in a strip that drew its
        // own. Measured line to line the two bands are the same.
        let height = match self.nested {
            false => cx.metrics().tab_strip_height,
            true => cx.metrics().tab_strip_height - px(1.),
        };

        // Where the tab row reaches the end of the strip, the strip's end is a
        // region divider that some neighbour already draws — the sidebar's
        // right edge, the inspector's left one, or the window itself. The tab
        // there gives up that stretch of seam rather than laying a second
        // hairline beside the first.
        if self.start.is_empty() {
            if let Some(first) = self.tabs.first_mut() {
                first.seam_l = false;
            }
        }
        if self.end.is_empty() {
            if let Some(last) = self.tabs.last_mut() {
                last.seam_r = false;
            }
        }
        // The seam runs unbroken under a stacked strip, so there is no detour
        // for a vertical to leave and rejoin it by. Drawn anyway they would be
        // a pair of ticks hanging off a straight line.
        if self.stacked {
            for tab in self.tabs.iter_mut() {
                tab.seam_l = false;
                tab.seam_r = false;
            }
        }

        let strip = self.id.clone();
        let border = c.border;
        // The tabs fill the strip's height and the controls beside them stay
        // centred in it, so the two slots align differently and the strip
        // itself takes no position at all.
        let mut el = div()
            .flex()
            .flex_row()
            .items_stretch()
            .id(self.id)
            .relative()
            .h(height)
            .flex_none()
            .w_full()
            .bg(c.chrome)
            // The strip's own edge against the band above it — titlebar,
            // editor, whatever it happens to be. Runs the full width, active
            // tab included: the notch is cut out of the bottom of the strip,
            // not out of both ends of it.
            .when(!self.nested, |el| el.border_t_1().border_color(c.border))
            .overflow_hidden();
        el.style().refine(&self.style);

        // The bottom run of the seam. Painted before the tabs, so that the
        // active one covers its own stretch of it and the notch has no bottom
        // edge — except under a stacked strip, where the line is between two
        // strips rather than between a tab and its content and has to survive
        // the tab that happens to be lit. A `border_b` would do neither: it is
        // painted after the children either way.
        let seam = || {
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .h(px(1.))
                .bg(border)
        };
        let stacked = self.stacked;

        el.children((!stacked).then(seam))
            .when(!self.start.is_empty(), |el| {
                el.child(
                    h_flex()
                        .flex_none()
                        .gap(px(2.))
                        .px(px(4.))
                        .children(self.start),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    // Bottom-aligned: whatever the tabs' own height turns out to
                    // be, the edge they share with the content is the one that has
                    // to line up.
                    .items_end()
                    .flex_1()
                    .min_w_0()
                    // No gap at all: tabs are adjacent faces of one surface, not
                    // separate objects, and the only thing that should ever come
                    // between two of them is the seam's detour down the side of the
                    // active one. An earlier version left a pixel of chrome
                    // showing through, which is invisible between two idle tabs and
                    // a mess next to a lit one — hover a tab beside the active one
                    // and that pixel became a third tone wedged between the two,
                    // reading as a gap rather than as an edge.
                    .gap_0()
                    .children(self.tabs)
                    .map(|el| match self.scroll {
                        // A scroll region needs an id of its own, and it has to be
                        // this strip's — two panes side by side are two strips, and
                        // one shared id would give them one shared scroll position.
                        Some(scroll) => el
                            .id(ElementId::Name(format!("{strip:?}-scroll").into()))
                            .overflow_x_scroll()
                            .track_scroll(&scroll)
                            .into_any_element(),
                        None => el.overflow_hidden().into_any_element(),
                    }),
            )
            .when(!self.end.is_empty(), |el| {
                el.child(h_flex().flex_none().gap(px(2.)).px(px(4.)).children(self.end))
            })
            // Last, so that it runs under the lit tab as well as the idle ones.
            .children(stacked.then(seam))
    }
}
