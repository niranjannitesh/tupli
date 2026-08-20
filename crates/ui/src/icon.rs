//! Icons.
//!
//! gpui rasterises an SVG into a single-channel alpha mask and tints it with the
//! element's text colour, so an icon file carries shape but never colour. The
//! duotone icons are therefore shipped as two files and drawn as two stacked
//! layers; [`Icon::duo_color`] picks the colour of the lower one.

use gpui::{
    div, prelude::*, px, svg, App, Hsla, IntoElement, RenderOnce, StyleRefinement, Styled, Window,
};

use crate::{ActiveTheme, IconName};

/// The size ramp. Icons snap to these so unrelated toolbars stay aligned.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum IconSize {
    /// 12px — inline with small text: badges, tree affordances, sort arrows.
    XSmall,
    /// 14px — the default for dense chrome: tab strips, tree rows, status bar.
    #[default]
    Small,
    /// 16px — toolbar buttons.
    Medium,
    /// 20px — prominent single actions.
    Large,
    /// 32px — empty-state and dialog art.
    XLarge,
}

impl IconSize {
    pub fn rems(self) -> gpui::Pixels {
        match self {
            IconSize::XSmall => px(12.),
            IconSize::Small => px(14.),
            IconSize::Medium => px(16.),
            IconSize::Large => px(20.),
            IconSize::XLarge => px(32.),
        }
    }
}

/// Semantic colours an icon can take, so callers rarely reach for a literal.
#[derive(Copy, Clone, Debug, PartialEq, Default)]
pub enum IconColor {
    #[default]
    Default,
    Muted,
    Subtle,
    Disabled,
    Accent,
    Success,
    Warning,
    Danger,
    /// Inherit whatever `text_color` the parent set.
    Inherit,
    Custom(Hsla),
}

impl IconColor {
    pub fn resolve(self, cx: &App) -> Option<Hsla> {
        let c = cx.colors();
        Some(match self {
            IconColor::Default => c.text,
            IconColor::Muted => c.text_muted,
            IconColor::Subtle => c.text_subtle,
            IconColor::Disabled => c.text_disabled,
            IconColor::Accent => c.accent,
            IconColor::Success => c.success,
            IconColor::Warning => c.warning,
            IconColor::Danger => c.danger,
            IconColor::Custom(c) => c,
            IconColor::Inherit => return None,
        })
    }
}

#[derive(IntoElement)]
pub struct Icon {
    name: IconName,
    size: IconSize,
    color: IconColor,
    /// Colour of the secondary layer. Defaults to the accent at low emphasis.
    duo_color: Option<IconColor>,
    /// Suppress the secondary layer even on a duotone icon.
    flat: bool,
    style: StyleRefinement,
}

impl Icon {
    pub fn new(name: IconName) -> Self {
        Self {
            name,
            size: IconSize::default(),
            color: IconColor::default(),
            duo_color: None,
            flat: false,
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

    pub fn duo_color(mut self, color: IconColor) -> Self {
        self.duo_color = Some(color);
        self
    }

    /// Draw only the primary layer.
    pub fn flat(mut self) -> Self {
        self.flat = true;
        self
    }
}

impl Styled for Icon {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Icon {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let side = self.size.rems();
        let primary = self.color.resolve(cx);
        let duo = self.name.duo_path().filter(|_| !self.flat).map(|path| {
            let color = self
                .duo_color
                .unwrap_or(IconColor::Accent)
                .resolve(cx)
                .unwrap_or_else(|| cx.colors().accent);
            svg()
                .absolute()
                .top_0()
                .left_0()
                .size(side)
                .path(path)
                .text_color(color)
        });

        let mut base = div().relative().flex_none().size(side);
        base.style().refine(&self.style);

        base.child(
            svg()
                .absolute()
                .top_0()
                .left_0()
                .size(side)
                .path(self.name.path())
                .when_some(primary, |el, c| el.text_color(c)),
        )
        .children(duo)
    }
}

/// An icon centred inside a fixed square — use where an icon sits alone in a row
/// so that swapping the glyph never shifts the layout.
#[derive(IntoElement)]
pub struct IconSlot {
    icon: Option<Icon>,
    side: gpui::Pixels,
}

impl IconSlot {
    pub fn new(side: gpui::Pixels) -> Self {
        Self { icon: None, side }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
}

impl RenderOnce for IconSlot {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .flex_none()
            .size(self.side)
            .flex()
            .items_center()
            .justify_center()
            .children(self.icon)
    }
}
