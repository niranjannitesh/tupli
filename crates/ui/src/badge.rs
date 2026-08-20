//! Badges and chips.
//!
//! Small, high-contrast labels: Redis key types, connection environments,
//! `PK`/`FK`/`NOT NULL` column flags, row counts. They carry meaning through
//! colour, so the palette here is intentionally fixed rather than themeable —
//! a `HASH` badge has to be the same purple in both appearances or muscle memory
//! breaks.

use gpui::{
    div, prelude::*, px, rgb, App, Hsla, IntoElement, RenderOnce, SharedString, StyleRefinement,
    Styled, Window,
};

use crate::{ActiveTheme, IconColor, Label, LabelSize};

/// Fixed hues for categorical badges.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BadgeTone {
    /// Theme-neutral: counts, "0 rows".
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
    Purple,
    Pink,
    Orange,
    Teal,
}

impl BadgeTone {
    fn color(self, cx: &App) -> Hsla {
        let c = cx.colors();
        match self {
            BadgeTone::Neutral => c.text_muted,
            BadgeTone::Accent => c.accent,
            BadgeTone::Success => c.success,
            BadgeTone::Warning => c.warning,
            BadgeTone::Danger => c.danger,
            BadgeTone::Purple => rgb(0xa78bfa).into(),
            BadgeTone::Pink => rgb(0xf472b6).into(),
            BadgeTone::Orange => rgb(0xf59e0b).into(),
            BadgeTone::Teal => rgb(0x2dd4bf).into(),
        }
    }
}

/// How much the badge asserts itself.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum BadgeStyle {
    /// Solid fill, white text. Redis type badges.
    Solid,
    /// 12%-alpha fill, coloured text. Status pills.
    #[default]
    Soft,
    /// Text and a 1px border. Column flags in a dense header.
    Outline,
    /// Text only.
    Plain,
}

#[derive(IntoElement)]
pub struct Badge {
    text: SharedString,
    tone: BadgeTone,
    style_kind: BadgeStyle,
    /// Uppercase and letter-spaced, the way `STRING`/`HASH` read in Medis.
    caps: bool,
    style: StyleRefinement,
}

impl Badge {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            tone: BadgeTone::Neutral,
            style_kind: BadgeStyle::default(),
            caps: false,
            style: StyleRefinement::default(),
        }
    }

    pub fn tone(mut self, tone: BadgeTone) -> Self {
        self.tone = tone;
        self
    }

    pub fn kind(mut self, kind: BadgeStyle) -> Self {
        self.style_kind = kind;
        self
    }

    pub fn caps(mut self) -> Self {
        self.caps = true;
        self
    }
}

impl Styled for Badge {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Badge {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let tone = self.tone.color(cx);
        let radius = cx.metrics().radius_sm;
        let on_accent = cx.colors().text_on_accent;

        let (bg, fg, border) = match self.style_kind {
            BadgeStyle::Solid => (Some(tone), on_accent, None),
            BadgeStyle::Soft => (Some(with_alpha(tone, 0.16)), tone, None),
            BadgeStyle::Outline => (None, tone, Some(with_alpha(tone, 0.45))),
            BadgeStyle::Plain => (None, tone, None),
        };

        let text = if self.caps {
            SharedString::from(self.text.to_uppercase())
        } else {
            self.text
        };

        let mut el = div()
            .flex_none()
            .flex()
            .items_center()
            .h(px(16.))
            .px(px(5.))
            .rounded(radius)
            .when_some(bg, |el, bg| el.bg(bg))
            .when_some(border, |el, b| el.border_1().border_color(b));
        el.style().refine(&self.style);

        el.child(
            Label::new(text)
                .size(LabelSize::Small)
                .color(IconColor::Custom(fg))
                .medium(),
        )
    }
}

fn with_alpha(mut color: Hsla, alpha: f32) -> Hsla {
    color.a = alpha;
    color
}
