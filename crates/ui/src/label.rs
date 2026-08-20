//! Text.
//!
//! Every string the user reads goes through here so that size, weight and colour
//! come from the type ramp rather than from whatever the calling site felt like.

use gpui::{
    div, prelude::*, App, FontWeight, IntoElement, RenderOnce, SharedString, StyleRefinement,
    Styled, Window,
};

use crate::{ActiveTheme, IconColor};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum LabelSize {
    /// 11px — badges, counts, column types, the status bar.
    Small,
    /// 13px — the default: menus, buttons, tree rows, tabs.
    #[default]
    Default,
    /// 15px — dialog and section titles.
    Large,
    /// The monospace ramp — grid cells, editor text, value inspectors. Always
    /// pairs with [`Label::mono`]; separate from the UI ramp because code needs
    /// a different optical size than chrome at the same nominal px.
    Code,
}

/// What gives when a label is wider than the box it is in.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Elide {
    /// `conversation_annot…`. The default, and right for anything read
    /// left to right.
    #[default]
    End,
    /// `…on_annotations`. For a name whose end is the part that identifies it.
    Start,
}

#[derive(IntoElement)]
pub struct Label {
    text: SharedString,
    size: LabelSize,
    color: IconColor,
    weight: FontWeight,
    mono: bool,
    /// Ellipsise instead of wrapping. On by default: every label in this app
    /// lives in a resizable panel and wrapping would change row heights.
    truncate: bool,
    /// Which end of the text goes when there is not enough room for it.
    elide: Elide,
    style: StyleRefinement,
}

impl Label {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            size: LabelSize::default(),
            color: IconColor::Default,
            weight: FontWeight::NORMAL,
            mono: false,
            truncate: true,
            elide: Elide::default(),
            style: StyleRefinement::default(),
        }
    }

    pub fn size(mut self, size: LabelSize) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: IconColor) -> Self {
        self.color = color;
        self
    }

    pub fn weight(mut self, weight: FontWeight) -> Self {
        self.weight = weight;
        self
    }

    /// Semibold. Used for active tabs and section headers, never for body text.
    pub fn strong(self) -> Self {
        self.weight(FontWeight::SEMIBOLD)
    }

    pub fn medium(self) -> Self {
        self.weight(FontWeight::MEDIUM)
    }

    /// Render in the monospace family — values, SQL fragments, identifiers.
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    pub fn wrap(mut self) -> Self {
        self.truncate = false;
        self
    }

    /// Keep the end of the text and elide the start: `…on_annotations`.
    pub fn elide_start(mut self) -> Self {
        self.elide = Elide::Start;
        self
    }

    pub fn muted(self) -> Self {
        self.color(IconColor::Muted)
    }

    pub fn subtle(self) -> Self {
        self.color(IconColor::Subtle)
    }
}

impl Styled for Label {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Label {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ty = cx.typography();
        // A pure lookup into the ramp — no arithmetic, no literals. If a size
        // is wrong it is wrong in `Typography` for the whole app at once.
        let (size, line_height) = match self.size {
            LabelSize::Small => (ty.ui_size_sm, ty.ui_line_height_sm),
            LabelSize::Default => (ty.ui_size, ty.ui_line_height),
            LabelSize::Large => (ty.ui_size_lg, ty.ui_line_height_lg),
            LabelSize::Code => (ty.mono_size, ty.mono_line_height),
        };
        let color = self.color.resolve(cx);

        let mut el = div()
            // `.font` and not `.font_family`: the mono face carries a feature
            // set (ligatures off) that a bare family name would drop.
            .font(if self.mono {
                ty.mono_font()
            } else {
                gpui::font(ty.ui_family.clone())
            })
            .text_size(size)
            .line_height(line_height)
            .font_weight(self.weight);
        if let Some(color) = color {
            el = el.text_color(color);
        }
        if self.truncate {
            el = el.overflow_hidden().whitespace_nowrap();
            el = match self.elide {
                Elide::End => el.text_ellipsis(),
                Elide::Start => el.text_ellipsis_start(),
            };
        }
        el.style().refine(&self.style);
        el.child(self.text)
    }
}
