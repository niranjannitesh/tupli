//! The label an icon-only control does not have room for.
//!
//! Half the controls in this window are a single glyph: the titlebar's nine,
//! the toolbar's run and stop, the grid's sort and fit, every panel toggle. A
//! glyph is the right size for a 28px bar and the wrong size for saying what it
//! does, and the usual answer — a text label — would make the bar three times
//! as wide. So the name arrives on hover instead, with the keystroke that does
//! the same thing beside it, which is the only place in the app where a person
//! can *discover* a binding rather than having to already know it.
//!
//! It is an entity because gpui's tooltip layer wants an `AnyView`, and it is
//! built by a closure because the layer builds it lazily — nothing is created
//! for a button nobody hovers.

use gpui::{div, prelude::*, px, AnyView, App, Context, IntoElement, Render, SharedString, Window};

use crate::{h_flex, ActiveTheme, IconColor, Label, LabelSize};

pub struct Tooltip {
    label: SharedString,
    /// The keystroke for the same action, if it has one. Written the way a menu
    /// writes it — `⌘⏎`, not `Cmd+Enter` — because it is the same reminder in a
    /// different place, and two spellings of one key is one too many.
    key: Option<SharedString>,
}

impl Tooltip {
    /// A tooltip builder for [`gpui::InteractiveElement::tooltip`].
    pub fn text(label: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
        let label = label.into();
        move |_, cx| {
            let label = label.clone();
            cx.new(|_| Self { label, key: None }).into()
        }
    }

    /// The same, with the keystroke that does it.
    pub fn key(
        label: impl Into<SharedString>,
        key: impl Into<SharedString>,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView {
        let label = label.into();
        let key = key.into();
        move |_, cx| {
            let (label, key) = (label.clone(), key.clone());
            cx.new(|_| Self {
                label,
                key: Some(key),
            })
            .into()
        }
    }
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors();
        let radius = cx.metrics().radius_sm;
        // The margin is the gap: gpui puts this view's own origin against the
        // hovered element, so anything that looks like breathing room has to be
        // outside the bubble rather than inside it.
        div().m(px(5.)).child(
            h_flex()
                .flex_none()
                .px(px(7.))
                .py(px(3.))
                .gap(px(8.))
                .bg(c.overlay)
                .rounded(radius)
                .border_1()
                .border_color(c.border_strong)
                .shadow_lg()
                .child(Label::new(self.label.clone()).size(LabelSize::Small))
                .children(self.key.clone().map(|key| {
                    Label::new(key)
                        .size(LabelSize::Small)
                        .color(IconColor::Subtle)
                })),
        )
    }
}
