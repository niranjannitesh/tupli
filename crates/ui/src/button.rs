//! Buttons.
//!
//! Three shapes — text, icon, icon+text — behind one builder, because the only
//! thing that actually varies is which slots are filled. Variants are ordered by
//! emphasis: `Ghost` (chrome, no chrome of its own) → `Subtle` (a surface that
//! appears on hover) → `Filled` → `Accent`/`Danger` (one per dialog, at most).

use gpui::{
    prelude::*, px, AnyElement, App, ClickEvent, ElementId, Hsla, IntoElement, MouseButton,
    RenderOnce, SharedString, StyleRefinement, Styled, Window,
};

use crate::{h_flex, ActiveTheme, Icon, IconColor, IconName, IconSize, Label, LabelSize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum ButtonVariant {
    /// No background until hovered. The default for anything living in chrome.
    #[default]
    Ghost,
    /// Always has a faint background — for controls that must look clickable
    /// when surrounded by text, like a segmented control.
    Subtle,
    /// A bordered surface. Secondary dialog actions.
    Filled,
    /// Solid accent. One per surface.
    Accent,
    /// Solid danger. Destructive confirmation only.
    Danger,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum ButtonSize {
    /// 20px — inside a toolbar row that is itself only 28px tall.
    XSmall,
    /// 24px — chrome default.
    #[default]
    Small,
    /// 28px — forms.
    Medium,
    /// 32px — dialog footers.
    Large,
}

impl ButtonSize {
    fn height(self) -> gpui::Pixels {
        match self {
            ButtonSize::XSmall => px(20.),
            ButtonSize::Small => px(24.),
            ButtonSize::Medium => px(28.),
            ButtonSize::Large => px(32.),
        }
    }

    fn padding_x(self) -> gpui::Pixels {
        match self {
            ButtonSize::XSmall => px(5.),
            ButtonSize::Small => px(8.),
            ButtonSize::Medium => px(10.),
            ButtonSize::Large => px(14.),
        }
    }

    fn icon_size(self) -> IconSize {
        match self {
            ButtonSize::XSmall => IconSize::XSmall,
            ButtonSize::Small | ButtonSize::Medium => IconSize::Small,
            ButtonSize::Large => IconSize::Medium,
        }
    }

    fn label_size(self) -> LabelSize {
        match self {
            ButtonSize::XSmall => LabelSize::Small,
            _ => LabelSize::Default,
        }
    }
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// What builds the hover label, if the caller gave one. Boxed rather than
/// generic so a button with a tooltip and one without are the same type.
type TooltipBuilder = Box<dyn Fn(&mut Window, &mut App) -> gpui::AnyView + 'static>;

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<IconName>,
    trailing_icon: Option<IconName>,
    variant: ButtonVariant,
    size: ButtonSize,
    disabled: bool,
    /// Toggled-on state, for buttons that latch (panel toggles, filter pins).
    selected: bool,
    /// Square, for the icon-only form.
    square: bool,
    full_width: bool,
    color_override: Option<IconColor>,
    on_click: Option<ClickHandler>,
    tooltip: Option<TooltipBuilder>,
    style: StyleRefinement,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self::bare(id).label(label)
    }

    /// Icon-only. Square by construction so a row of them is evenly spaced.
    pub fn icon(id: impl Into<ElementId>, icon: IconName) -> Self {
        Self::bare(id).start_icon(icon).square()
    }

    fn bare(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            label: None,
            icon: None,
            trailing_icon: None,
            variant: ButtonVariant::default(),
            size: ButtonSize::default(),
            disabled: false,
            selected: false,
            square: false,
            full_width: false,
            color_override: None,
            on_click: None,
            tooltip: None,
            style: StyleRefinement::default(),
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn start_icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn end_icon(mut self, icon: IconName) -> Self {
        self.trailing_icon = Some(icon);
        self
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn square(mut self) -> Self {
        self.square = true;
        self
    }

    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// Recolour the content — a ghost button that should read as destructive
    /// without carrying a red background.
    pub fn content_color(mut self, color: IconColor) -> Self {
        self.color_override = Some(color);
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    /// The name of the thing this button does, shown on hover. Take
    /// [`crate::Tooltip::text`] or [`crate::Tooltip::key`]; an icon-only button
    /// should always have one, because a glyph is a guess until it is named.
    pub fn tooltip(
        mut self,
        builder: impl Fn(&mut Window, &mut App) -> gpui::AnyView + 'static,
    ) -> Self {
        self.tooltip = Some(Box::new(builder));
        self
    }
}

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

struct Skin {
    bg: Option<Hsla>,
    hover: Hsla,
    active: Hsla,
    border: Option<Hsla>,
    content: IconColor,
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors().clone();
        let radius = cx.metrics().radius_sm;

        let skin = match self.variant {
            ButtonVariant::Ghost => Skin {
                bg: self.selected.then_some(c.active),
                hover: c.hover,
                active: c.active,
                border: None,
                content: if self.selected {
                    IconColor::Default
                } else {
                    IconColor::Muted
                },
            },
            ButtonVariant::Subtle => Skin {
                bg: Some(if self.selected { c.active } else { c.hover }),
                hover: c.active,
                active: c.active,
                border: None,
                content: IconColor::Default,
            },
            ButtonVariant::Filled => Skin {
                bg: Some(c.chrome),
                hover: c.hover,
                active: c.active,
                border: Some(c.border_strong),
                content: IconColor::Default,
            },
            ButtonVariant::Accent => Skin {
                bg: Some(c.accent),
                hover: c.accent_hover,
                active: c.accent_active,
                border: None,
                content: IconColor::Custom(c.text_on_accent),
            },
            ButtonVariant::Danger => Skin {
                bg: Some(c.danger),
                hover: c.danger,
                active: c.danger,
                border: None,
                content: IconColor::Custom(c.text_on_accent),
            },
        };

        let content_color = if self.disabled {
            IconColor::Disabled
        } else {
            self.color_override.unwrap_or(skin.content)
        };
        // A filled button that keeps its colour while doing nothing is a button
        // people press, and press again. Disabled takes the fill away and
        // leaves the outline, so the shape is still there to come back to.
        let skin = match self.disabled {
            true => Skin {
                bg: skin.bg.map(|_| c.surface),
                border: Some(c.border),
                ..skin
            },
            false => skin,
        };

        let height = self.size.height();
        let mut el = h_flex()
            .id(self.id)
            .h(height)
            .flex_none()
            .rounded(radius)
            .gap(px(5.));

        if self.square {
            el = el.w(height).justify_center();
        } else {
            el = el.px(self.size.padding_x());
        }
        if self.full_width {
            el = el.w_full().justify_center();
        }
        if let Some(bg) = skin.bg {
            el = el.bg(bg);
        }
        if let Some(border) = skin.border {
            el = el.border_1().border_color(border);
        }

        // Before the disabled branch: a control that is off is the one people
        // most want explained, and a tooltip is not an interaction.
        if let Some(tooltip) = self.tooltip {
            el = el.tooltip(move |window, cx| tooltip(window, cx));
        }

        if self.disabled {
            el = el.cursor_default();
        } else {
            el = el
                .cursor_pointer()
                .hover(move |s| s.bg(skin.hover))
                .active(move |s| s.bg(skin.active));
            if let Some(handler) = self.on_click {
                el = el.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
                el = el.on_click(move |event, window, cx| handler(event, window, cx));
            }
        }

        el.style().refine(&self.style);

        el.children(self.icon.map(|i| {
            Icon::new(i)
                .size(self.size.icon_size())
                .color(content_color)
        }))
        .children(self.label.map(|l| {
            Label::new(l)
                .size(self.size.label_size())
                .color(content_color)
                .medium()
        }))
        .children(self.trailing_icon.map(|i| {
            Icon::new(i)
                .size(self.size.icon_size())
                .color(content_color)
        }))
    }
}

#[allow(dead_code)]
fn _assert(b: Button) -> AnyElement {
    b.into_any_element()
}
