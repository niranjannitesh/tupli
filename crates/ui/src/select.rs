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

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    div, px, App, ElementId, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled, Window,
};

use crate::icon::IconColor;
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
                .cursor_pointer()
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
