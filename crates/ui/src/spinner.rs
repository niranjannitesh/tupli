//! The one thing in the design system that moves.

use std::time::Duration;

use gpui::{
    percentage, prelude::*, svg, Animation, AnimationExt, App, ElementId, IntoElement, RenderOnce,
    StyleRefinement, Styled, Transformation, Window,
};

use crate::{IconColor, IconName, IconSize};

/// How long one turn takes. Slow enough not to look frantic, fast enough that a
/// connection which resolves in half a second still visibly spun.
const PERIOD: Duration = Duration::from_millis(900);

/// A spinner: the eight-spoke loader glyph, turning.
///
/// Every other icon in the app is a still shape, so this is deliberately the
/// only widget with a clock in it — motion is how a reader tells *waiting* from
/// *stuck*, and a static glyph next to the word "Connecting…" says the opposite
/// of what it means.
///
/// The turn is phase-locked to the app's shared clock rather than started when
/// the element first appears, so two spinners on screen at once turn together
/// instead of beating against each other. gpui stops the animation by itself
/// when the reader has asked the system to reduce motion.
#[derive(IntoElement)]
pub struct Spinner {
    id: ElementId,
    size: IconSize,
    color: IconColor,
    style: StyleRefinement,
}

impl Spinner {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            size: IconSize::default(),
            color: IconColor::Muted,
            style: StyleRefinement::default(),
        }
    }

    pub fn size(mut self, size: IconSize) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: IconColor) -> Self {
        self.color = color;
        self
    }
}

impl Styled for Spinner {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Spinner {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let side = self.size.rems();
        let mut glyph = svg()
            .flex_none()
            .size(side)
            .path(IconName::Loader.path())
            .when_some(self.color.resolve(cx), |el, c| el.text_color(c));
        glyph.style().refine(&self.style);
        glyph.with_animation(
            self.id,
            Animation::new(PERIOD).repeat_synced(),
            // The glyph's spokes fade around the dial, so a whole turn returns
            // it to itself: the loop has no seam to hide.
            |glyph, turn| glyph.with_transformation(Transformation::rotate(percentage(turn))),
        )
    }
}
