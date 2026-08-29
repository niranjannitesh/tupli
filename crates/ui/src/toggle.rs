//! On or off.
//!
//! A switch rather than a checkbox: everything it controls in this app takes
//! effect the moment it is flicked, and a checkbox implies a form you will
//! later submit. Same contract as [`crate::Segmented`] — the state lives in the
//! host, this renders it and reports clicks.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    div, px, App, ElementId, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement as _, Styled, Window,
};

use crate::styled_ext::h_flex;
use crate::theme::ActiveTheme;

/// Track and knob sizes. Small enough to sit on a 24px form row without
/// setting the row's height itself.
const TRACK_WIDTH: gpui::Pixels = px(28.);
const TRACK_HEIGHT: gpui::Pixels = px(16.);
const KNOB: gpui::Pixels = px(12.);

/// Side of the checkbox itself. AppKit draws a small checkbox 14pt across.
const BOX: gpui::Pixels = px(14.);

#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    on: bool,
    disabled: bool,
    on_toggle: Option<Box<dyn Fn(bool, &mut Window, &mut App) + 'static>>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>, on: bool) -> Self {
        Self {
            id: id.into(),
            on,
            disabled: false,
            on_toggle: None,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Handed the state the switch is being moved *to*, not the one it is in.
    pub fn on_toggle(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors().clone();
        let on = self.on;
        let disabled = self.disabled;
        let handler = self.on_toggle;

        h_flex()
            .id(self.id)
            .w(TRACK_WIDTH)
            .h(TRACK_HEIGHT)
            .p(px(2.))
            .rounded(TRACK_HEIGHT / 2.)
            .border_1()
            .when(on, |el| el.bg(c.accent).border_color(c.accent))
            .when(!on, |el| el.bg(c.field).border_color(c.border_strong))
            // The knob is pushed to whichever end is on: no animation layer
            // yet, so the position is the whole signal and it has to be
            // unmistakable at a glance.
            .when(on, |el| el.justify_end())
            .when(disabled, |el| el.opacity(0.4))
            .when(!disabled, |el| {
                el.hover(|el| el.border_color(c.accent_hover))
            })
            .child(div().size(KNOB).rounded(KNOB / 2.).bg(if on {
                c.text_on_accent
            } else {
                c.text_subtle
            }))
            .on_click(move |_, window, cx| {
                if disabled {
                    return;
                }
                if let Some(handler) = &handler {
                    handler(!on, window, cx);
                }
            })
    }
}

/// A box that is ticked or not.
///
/// The other half of [`Switch`], for the case a switch is wrong for: a dense
/// list of rows where the same question is asked of every row, and the answer
/// is part of a record being edited rather than a setting taking effect as it
/// is flicked. A 28px track repeated down forty columns is a lot of furniture
/// to ask one bit each.
#[derive(IntoElement)]
pub struct Checkbox {
    id: ElementId,
    checked: bool,
    disabled: bool,
    on_toggle: Option<Box<dyn Fn(bool, &mut Window, &mut App) + 'static>>,
}

impl Checkbox {
    pub fn new(id: impl Into<ElementId>, checked: bool) -> Self {
        Self {
            id: id.into(),
            checked,
            disabled: false,
            on_toggle: None,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Handed the state the box is being moved *to*.
    pub fn on_toggle(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Checkbox {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors().clone();
        let checked = self.checked;
        let disabled = self.disabled;
        let handler = self.on_toggle;

        h_flex()
            .id(self.id)
            .justify_center()
            // The hit area is the row's height, not the box's: a 14px target
            // is a miss waiting to happen, and the box is the only part that
            // has to be small.
            .size(px(20.))
            .when(disabled, |el| el.opacity(0.4))
            .child(
                // Drawn rather than set in an icon font. A checkbox is a
                // control, and on this platform a control is a filled box with
                // the window's accent in it — an outlined glyph that stays the
                // same shape whether it is ticked or not is a picture of a
                // checkbox, which is what every web framework ships and what
                // makes them all look alike.
                h_flex()
                    .flex_none()
                    .size(BOX)
                    .justify_center()
                    .rounded(px(3.5))
                    .border_1()
                    .when(checked, |el| el.bg(c.accent).border_color(c.accent))
                    .when(!checked, |el| el.bg(c.field).border_color(c.border_strong))
                    .when(!disabled && !checked, |el| {
                        el.hover(|s| s.border_color(c.border_focus))
                    })
                    .children(checked.then(|| {
                        crate::Icon::new(crate::IconName::Check)
                            .size(crate::IconSize::XSmall)
                            .color(crate::IconColor::Custom(c.text_on_accent))
                            .flat()
                    })),
            )
            .on_click(move |_, window, cx| {
                if disabled {
                    return;
                }
                if let Some(handler) = &handler {
                    handler(!checked, window, cx);
                }
            })
    }
}
