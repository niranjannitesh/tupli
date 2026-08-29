//! Choosing one of a few things.
//!
//! A segmented control rather than a pop-up menu, and deliberately so: the app
//! has no popover layer yet, and a menu that has to escape a scrolling sheet
//! needs one. Every choice in the app so far has six options or fewer, which is
//! the range where laying them all out beats hiding them behind a menu anyway —
//! the reader can see what the alternatives are without clicking.
//!
//! State lives in the host, like [`crate::Tab`]: this renders `selected` and
//! reports clicks.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::Refineable as _;
use gpui::{
    canvas, div, point, px, size, App, Bounds, ElementId, InteractiveElement as _, IntoElement,
    ParentElement, Pixels, RenderOnce, SharedString, StatefulInteractiveElement as _,
    StyleRefinement, Styled, Window,
};

use crate::icon::{Icon, IconColor, IconSize};
use crate::icon_name::IconName;
use crate::label::{Label, LabelSize};
use crate::styled_ext::h_flex;
use crate::theme::ActiveTheme;

type SelectHandler = Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Segmented {
    id: ElementId,
    options: Vec<SharedString>,
    selected: usize,
    on_select: Option<SelectHandler>,
    /// Let the options run onto a second line instead of squeezing. On by
    /// default: a truncated option label is worse than a two-row control.
    wrap: bool,
    /// Take the whole width of the column. On by default, because in a form
    /// the control column is the field's width and a segmented control that
    /// stopped short of it would break the edge every other row keeps.
    fill: bool,
}

impl Segmented {
    pub fn new(
        id: impl Into<ElementId>,
        options: impl IntoIterator<Item = impl Into<SharedString>>,
    ) -> Self {
        Self {
            id: id.into(),
            options: options.into_iter().map(Into::into).collect(),
            selected: 0,
            on_select: None,
            wrap: true,
            fill: true,
        }
    }

    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Shrink to the options rather than filling the column. For settings,
    /// where a three-word control stretched across 400px reads as a table
    /// rather than a choice.
    pub fn hug(mut self) -> Self {
        self.fill = false;
        self.wrap = false;
        self
    }

    /// Keep every option on one row, whatever it costs in width.
    pub fn no_wrap(mut self) -> Self {
        self.wrap = false;
        self
    }

    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Segmented {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors().clone();
        let m = cx.metrics().clone();
        let selected = self.selected;
        let handler = self.on_select;

        let mut row = h_flex()
            .id(self.id)
            .when(self.fill, |el| el.w_full())
            .gap(px(2.))
            .p(px(2.))
            .rounded(m.radius_sm)
            .bg(c.field)
            .border_1()
            .border_color(c.border);
        if self.wrap {
            row = row.flex_wrap();
        }

        let fill = self.fill;
        let row = row.children(self.options.into_iter().enumerate().map(|(index, option)| {
            let active = index == selected;
            let handler = handler.clone();
            div()
                .id(("segment", index))
                .px(px(8.))
                .py(px(2.))
                .rounded(m.radius_sm)
                .when(active, |el| el.bg(c.selected))
                .when(!active, |el| el.hover(|el| el.bg(c.hover)))
                .child(Label::new(option).size(LabelSize::Small).color(if active {
                    IconColor::Default
                } else {
                    IconColor::Muted
                }))
                .on_click(move |_, window, cx| {
                    if let Some(handler) = &handler {
                        handler(index, window, cx);
                    }
                })
        }));

        match fill {
            true => row.into_any_element(),
            // A block-level child of a block container takes the whole width
            // whatever its own sizing says, so hugging means putting the
            // control inside a row that fills instead.
            false => h_flex().child(row).into_any_element(),
        }
    }
}

/// A pop-up button, in the macOS sense: the current choice in a box, with a
/// tinted well at the trailing edge and the two chevrons in it.
///
/// The well is the whole difference between this and a button that happens to
/// have a caret on the end of it. On this platform it is what says a list will
/// open *over* the control and land with the current choice under the pointer;
/// a plain caret is what a toolbar button wears when it is going to drop
/// something below itself. Menus are the host's business, as everywhere else
/// here — this reports the click and draws the answer.
#[derive(IntoElement)]
pub struct Popup {
    id: ElementId,
    label: SharedString,
    disabled: bool,
    on_open: Option<Rc<dyn Fn(&Bounds<Pixels>, &mut Window, &mut App) + 'static>>,
    style: StyleRefinement,
}

impl Popup {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            disabled: false,
            on_open: None,
            style: StyleRefinement::default(),
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Handed the control's own frame rather than the pointer: the list this
    /// opens belongs to the box, and a menu that lands wherever the click did
    /// is a context menu wearing a pop-up's clothes.
    pub fn on_open(mut self, f: impl Fn(&Bounds<Pixels>, &mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(f));
        self
    }
}

impl Styled for Popup {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Popup {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors().clone();
        let radius = cx.metrics().radius_sm;
        let disabled = self.disabled;
        let handler = self.on_open;
        // Filled in during prepaint by the canvas below, read by the click
        // that comes after it. Both closures live as long as this frame's
        // element tree, which is exactly as long as the answer is true for.
        let frame = Rc::new(Cell::new(Bounds::default()));

        let mut el = h_flex()
            .id(self.id)
            .relative()
            .flex_none()
            .h(px(24.))
            // Tighter on the right than on the left: the well is already inset
            // from its own edge, and the two paddings would add up to a gap.
            .pl(px(6.))
            .pr(px(3.))
            .gap(px(4.))
            .rounded(radius)
            .bg(match disabled {
                true => c.surface,
                false => c.chrome,
            })
            .border_1()
            .border_color(match disabled {
                true => c.border,
                false => c.border_strong,
            });

        if disabled {
            el = el.cursor_default();
        } else {
            el = el.hover(|s| s.bg(c.hover)).active(|s| s.bg(c.active));
            if let Some(handler) = handler {
                let frame = frame.clone();
                el = el.on_click(move |_, window, cx| {
                    // Back out to the border box. What the canvas can see is
                    // the box inside the border, and what a menu has to line
                    // its edge up with is the edge that is drawn.
                    let inner = frame.get();
                    let outer = Bounds {
                        origin: point(inner.origin.x - px(1.), inner.origin.y - px(1.)),
                        size: size(inner.size.width + px(2.), inner.size.height + px(2.)),
                    };
                    handler(&outer, window, cx)
                });
            }
        }

        el.style().refine(&self.style);

        el.child(
            canvas(
                {
                    let frame = frame.clone();
                    move |bounds, _, _| frame.set(bounds)
                },
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
        .child(
            Label::new(self.label)
                .size(LabelSize::Small)
                .color(match disabled {
                    true => IconColor::Disabled,
                    false => IconColor::Default,
                })
                .flex_1()
                .min_w_0(),
        )
        .child(
            // A well rather than a bare caret, because that shape is what says
            // "a list opens here" on this platform. Tinted out of the chrome
            // it sits in and not out of the accent: three of these down a
            // filter stack in the accent colour is a stack shouting.
            h_flex()
                .flex_none()
                .w(px(15.))
                .h(px(14.))
                .justify_center()
                .rounded(px(3.))
                .bg(match disabled {
                    true => c.surface,
                    false => c.active,
                })
                .child(
                    Icon::new(IconName::ChevronExpandY)
                        .size(IconSize::XSmall)
                        .color(match disabled {
                            true => IconColor::Disabled,
                            false => IconColor::Muted,
                        })
                        .flat(),
                ),
        )
    }
}
