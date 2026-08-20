//! Window chrome: separators, toolbars, status bars, empty states.
//!
//! These are the pieces that frame content. They are deliberately dumb — no
//! state, no behaviour — so that the layout code in the app crate stays the
//! single place that decides what goes where.

use gpui::{
    div, prelude::*, px, AnyElement, App, CursorStyle, ElementId, IntoElement, MouseButton,
    MouseDownEvent, RenderOnce, SharedString, StyleRefinement, Styled, Window,
};
use smallvec::SmallVec;

use crate::{h_flex, v_flex, ActiveTheme, Icon, IconColor, IconName, IconSize, Label, LabelSize};

/// A layout region: the sidebar, the centre stack, the inspector, the dock.
///
/// The regions butt against one another and are told apart by a hairline on the
/// seam, which is why this has no corner radius and no margin of its own. An
/// earlier design floated each one as a card on a near-black plane; it read as
/// five objects arranged on a desk when what the window actually is is one
/// surface divided up, and every gutter spent eight pixels of the narrowest
/// part of the screen saying so.
///
/// The seam is the caller's to draw. Only the caller knows which of its edges
/// has a neighbour, and a border on all four would draw every line twice.
pub fn region(cx: &App) -> gpui::Div {
    let c = cx.colors();
    div()
        .flex()
        .flex_col()
        .bg(c.panel)
        // Nothing here is rounded, but a grid still has to stop at the region's
        // edge rather than paint under the one beside it.
        .overflow_hidden()
}

/// Which way a splitter or divider runs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// A one-pixel rule. Prefer a container's `border_*` where possible; this is for
/// separators that sit *between* siblings rather than around one.
#[derive(IntoElement)]
pub struct Divider {
    axis: Axis,
    /// Inset from both ends, so a menu separator does not touch the edges.
    inset: gpui::Pixels,
}

impl Divider {
    pub fn horizontal() -> Self {
        Self {
            axis: Axis::Horizontal,
            inset: px(0.),
        }
    }

    pub fn vertical() -> Self {
        Self {
            axis: Axis::Vertical,
            inset: px(0.),
        }
    }

    pub fn inset(mut self, inset: gpui::Pixels) -> Self {
        self.inset = inset;
        self
    }
}

impl RenderOnce for Divider {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = cx.colors().border;
        match self.axis {
            Axis::Horizontal => div().h(px(1.)).w_full().mx(self.inset).bg(color),
            Axis::Vertical => div().w(px(1.)).h_full().my(self.inset).bg(color),
        }
        .flex_none()
    }
}

/// The draggable line between two regions. Draws as a hairline but takes a wider
/// hit area, and swaps the cursor so the affordance is discoverable without any
/// visible handle.
#[derive(IntoElement)]
pub struct ResizeHandle {
    id: ElementId,
    axis: Axis,
    active: bool,
    /// Draw nothing until hovered or dragged. For a splitter that sits in the
    /// gutter between two cards, where the gap is already the separator and a
    /// permanent line would just be a third edge.
    quiet: bool,
    on_drag_start: Option<Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>>,
}

impl ResizeHandle {
    pub fn new(id: impl Into<ElementId>, axis: Axis) -> Self {
        Self {
            id: id.into(),
            axis,
            active: false,
            quiet: false,
            on_drag_start: None,
        }
    }

    /// Highlight while a drag is in progress.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Show the line only on hover or drag.
    pub fn invisible_line(mut self) -> Self {
        self.quiet = true;
        self
    }

    pub fn on_drag_start(
        mut self,
        f: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_drag_start = Some(Box::new(f));
        self
    }
}

impl RenderOnce for ResizeHandle {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors();
        let hit = cx.metrics().splitter_hit_width;
        let line = if self.active {
            c.accent
        } else if self.quiet {
            gpui::transparent_black()
        } else {
            c.border
        };
        let hover_line = c.accent;
        let vertical = self.axis == Axis::Vertical;

        let el_group = self.id.clone();
        let mut el = div()
            .id(self.id)
            .absolute()
            .flex_none()
            .cursor(if vertical {
                CursorStyle::ResizeLeftRight
            } else {
                CursorStyle::ResizeUpDown
            })
            .when_some(self.on_drag_start, |el, f| {
                el.on_mouse_down(MouseButton::Left, move |e, window, cx| {
                    cx.stop_propagation();
                    f(e, window, cx)
                })
            });

        let group = SharedString::from(format!("{:?}-splitter", el_group));
        el = el.group(group.clone());
        el = if vertical {
            el.w(hit).h_full().top_0().child(
                div()
                    .absolute()
                    .left(hit / 2. - px(0.5))
                    .top_0()
                    .w(px(1.))
                    .h_full()
                    .bg(line)
                    .group_hover(group, move |s| s.bg(hover_line)),
            )
        } else {
            el.h(hit).w_full().left_0().child(
                div()
                    .absolute()
                    .top(hit / 2. - px(0.5))
                    .left_0()
                    .h(px(1.))
                    .w_full()
                    .bg(line)
                    .group_hover(group, move |s| s.bg(hover_line)),
            )
        };
        el
    }
}

/// A dense horizontal band of controls under a tab strip: the grid's filter row,
/// the editor's run controls, the sidebar's add/refresh pair.
#[derive(IntoElement)]
pub struct Toolbar {
    id: ElementId,
    start: SmallVec<[AnyElement; 4]>,
    center: SmallVec<[AnyElement; 2]>,
    end: SmallVec<[AnyElement; 4]>,
    /// Toolbars under a tab strip need no top border; standalone ones do.
    bordered: bool,
    transparent: bool,
    style: StyleRefinement,
}

impl Toolbar {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            start: SmallVec::new(),
            center: SmallVec::new(),
            end: SmallVec::new(),
            bordered: true,
            transparent: false,
            style: StyleRefinement::default(),
        }
    }

    pub fn start_child(mut self, child: impl IntoElement) -> Self {
        self.start.push(child.into_any_element());
        self
    }

    /// Grows to fill: the filter input, the breadcrumb.
    pub fn center_child(mut self, child: impl IntoElement) -> Self {
        self.center.push(child.into_any_element());
        self
    }

    pub fn end_child(mut self, child: impl IntoElement) -> Self {
        self.end.push(child.into_any_element());
        self
    }

    pub fn borderless(mut self) -> Self {
        self.bordered = false;
        self
    }

    /// Inherit the surface colour instead of painting chrome — for the bar that
    /// sits directly above an editor.
    pub fn transparent(mut self) -> Self {
        self.transparent = true;
        self
    }
}

impl Styled for Toolbar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Toolbar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors();
        let height = cx.metrics().toolbar_height;

        let mut el = h_flex()
            .id(self.id)
            .h(height)
            .w_full()
            .flex_none()
            .px(px(6.))
            .gap(px(4.))
            .when(!self.transparent, |el| el.bg(c.chrome))
            .when(self.bordered, |el| el.border_b_1().border_color(c.border));
        el.style().refine(&self.style);

        // The centre slot is the flexible one; when nothing claims it, an empty
        // spacer takes its place so the trailing controls stay right-aligned.
        let center = h_flex()
            .flex_1()
            .min_w_0()
            .gap(px(4.))
            .children(self.center);

        // The two ends are grouped rather than laid straight into the bar so
        // that a narrow toolbar — one half of a split, say — gives way on the
        // left instead of pushing the buttons off the right. `overflow_hidden`
        // is what lets the leading group shrink at all: a flex item that could
        // not clip refuses to go below its content.
        let start = h_flex()
            .flex_shrink_1()
            .min_w_0()
            .overflow_hidden()
            .gap(px(4.))
            .children(self.start);
        let end = h_flex().flex_none().gap(px(4.)).children(self.end);

        el.child(start).child(center).child(end)
    }
}

/// The bar along the bottom of the window. Left side is context (what you are
/// looking at), right side is state (row counts, timings, cursor position).
///
/// Painted directly on the window ground with no fill and no rule: it is the
/// same plane as the gutters between the cards, so giving it a surface would
/// turn the bottom margin into a fourth panel.
#[derive(IntoElement)]
pub struct StatusBar {
    start: SmallVec<[AnyElement; 4]>,
    end: SmallVec<[AnyElement; 4]>,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            start: SmallVec::new(),
            end: SmallVec::new(),
        }
    }

    pub fn start_child(mut self, child: impl IntoElement) -> Self {
        self.start.push(child.into_any_element());
        self
    }

    pub fn end_child(mut self, child: impl IntoElement) -> Self {
        self.end.push(child.into_any_element());
        self
    }
}

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for StatusBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let height = cx.metrics().status_bar_height;

        h_flex()
            .h(height)
            .w_full()
            .flex_none()
            // The frame plane, the same one the titlebar is on: the status
            // bar is the bottom edge of the window rather than the bottom of
            // the region above it.
            .bg(cx.colors().background)
            .border_t_1()
            .border_color(cx.colors().border)
            // The same indent every region gives its own content, so the first
            // label lines up with the tree above it rather than with the edge.
            .px(px(10.))
            .gap(px(10.))
            .child(h_flex().gap(px(10.)).children(self.start))
            .child(div().flex_1())
            .child(h_flex().gap(px(10.)).children(self.end))
    }
}

/// What a pane shows when it has nothing to show. Every empty state names the
/// thing that is missing and offers the action that would fill it — a bare
/// "No data" is a dead end.
#[derive(IntoElement)]
pub struct EmptyState {
    icon: IconName,
    title: SharedString,
    description: Option<SharedString>,
    action: Option<AnyElement>,
}

impl EmptyState {
    pub fn new(icon: IconName, title: impl Into<SharedString>) -> Self {
        Self {
            icon,
            title: title.into(),
            description: None,
            action: None,
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors();
        // No fill. Every region of the window is one plane painted `panel`, so
        // an empty state with a colour of its own reads as a grey card dropped
        // into the sidebar rather than as the region being empty.
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .child(
                Icon::new(self.icon)
                    .size(IconSize::XLarge)
                    .color(IconColor::Custom(c.text_disabled)),
            )
            .child(Label::new(self.title).medium().color(IconColor::Muted))
            .children(self.description.map(|d| {
                Label::new(d)
                    .size(LabelSize::Small)
                    .color(IconColor::Subtle)
                    .wrap()
                    .max_w(px(320.))
                    .text_center()
            }))
            .children(self.action.map(|a| div().mt(px(4.)).child(a)))
    }
}

/// A small uppercase heading above a group of rows — `FAVORITES (1)`,
/// `KEYS (8409 SCANNED)`, `TABLES`.
#[derive(IntoElement)]
pub struct SectionHeader {
    label: SharedString,
    end: Option<AnyElement>,
    inset: bool,
}

impl SectionHeader {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            end: None,
            inset: true,
        }
    }

    /// Drop the inset. The default matches a list, whose rows are inset by the
    /// same 8px; a form's labels start at the edge, and a heading that does not
    /// start where they do reads as belonging to something else.
    pub fn flush(mut self) -> Self {
        self.inset = false;
        self
    }

    pub fn end_child(mut self, child: impl IntoElement) -> Self {
        self.end = Some(child.into_any_element());
        self
    }
}

impl RenderOnce for SectionHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let _ = cx;
        h_flex()
            .h(px(22.))
            .w_full()
            .flex_none()
            .when(self.inset, |el| el.px(px(8.)))
            .child(
                Label::new(SharedString::from(self.label.to_uppercase()))
                    .size(LabelSize::Small)
                    .color(IconColor::Subtle)
                    .medium()
                    .flex_1(),
            )
            .children(self.end)
    }
}
