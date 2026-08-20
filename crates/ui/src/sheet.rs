//! Modal sheets and the form rows inside them.
//!
//! A sheet is the app's only modal surface. It exists because a few operations —
//! adding a connection, confirming a write on a production database — genuinely
//! must be answered before anything else happens, and everything else in the app
//! is deliberately non-modal.
//!
//! It is drawn as an absolutely positioned overlay over the whole window rather
//! than as a platform sheet: the window has its own titlebar and its own ground,
//! and a native sheet sliding out of a transparent titlebar looks wrong.

use gpui::{
    div, hsla, prelude::*, px, AnyElement, App, ClickEvent, ElementId, IntoElement, ParentElement,
    Pixels, RenderOnce, SharedString, Styled, Window,
};

use crate::icon::{IconColor, IconSize};
use crate::icon_name::IconName;
use crate::label::{Label, LabelSize};
use crate::styled_ext::{h_flex, v_flex, StyledExt};
use crate::theme::ActiveTheme;
use crate::{Button, ButtonSize, Icon};

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Sheet {
    id: ElementId,
    title: SharedString,
    subtitle: Option<SharedString>,
    width: Pixels,
    body: Vec<AnyElement>,
    footer_start: Vec<AnyElement>,
    footer_end: Vec<AnyElement>,
    on_dismiss: Option<ClickHandler>,
}

impl Sheet {
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            subtitle: None,
            width: px(440.),
            body: Vec::new(),
            footer_start: Vec::new(),
            footer_end: Vec::new(),
            on_dismiss: None,
        }
    }

    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }

    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.body.push(child.into_any_element());
        self
    }

    pub fn children(mut self, children: impl IntoIterator<Item = impl IntoElement>) -> Self {
        self.body
            .extend(children.into_iter().map(IntoElement::into_any_element));
        self
    }

    /// Bottom-left of the footer: destructive or secondary actions, kept away
    /// from the confirm button so neither is hit by accident.
    pub fn footer_start(mut self, child: impl IntoElement) -> Self {
        self.footer_start.push(child.into_any_element());
        self
    }

    /// Bottom-right of the footer, in reading order, ending with the primary.
    pub fn footer_end(mut self, child: impl IntoElement) -> Self {
        self.footer_end.push(child.into_any_element());
        self
    }

    /// Called by the close button and by a click on the scrim.
    pub fn on_dismiss(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_dismiss = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Sheet {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors().clone();
        let m = cx.metrics().clone();
        let dismiss = self.on_dismiss;

        div()
            .id(self.id)
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            // A scrim rather than a blur: the window behind stays legible, which
            // matters when the sheet is asking about something on it.
            .bg(hsla(0., 0., 0., 0.45))
            // Swallow every click that reaches the scrim, whether or not there
            // is a dismiss handler, so a stray click never lands on the grid
            // behind the sheet.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // And every other kind of mouse event with it. Stopping the press
            // is not enough: elements below hit-test the pointer for
            // themselves, so a drag that starts inside a field of the sheet
            // goes on extending the grid's selection behind it, and the wheel
            // over the sheet scrolls the table nobody can see. Occluding takes
            // this scrim out of the hit test for everything underneath.
            .occlude()
            .child(
                v_flex()
                    .w(self.width)
                    .max_h(px(640.))
                    .bg(c.surface)
                    .rounded(m.radius_lg)
                    .border_1()
                    .border_color(c.border)
                    .overflow_hidden()
                    // Clicks inside the card are the card's business.
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        h_flex()
                            .flex_none()
                            .px(px(16.))
                            .pt(px(14.))
                            .pb(px(10.))
                            .gap(px(8.))
                            .items_start()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap(px(1.))
                                    .child(Label::new(self.title).size(LabelSize::Large).medium())
                                    .children(self.subtitle.map(|text| {
                                        Label::new(text)
                                            .size(LabelSize::Small)
                                            .color(IconColor::Subtle)
                                    })),
                            )
                            .child(
                                Button::icon("sheet-close", IconName::XmarkSm)
                                    .size(ButtonSize::XSmall)
                                    .when_some(dismiss, |button, handler| {
                                        button.on_click(move |event, window, cx| {
                                            handler(event, window, cx)
                                        })
                                    }),
                            ),
                    )
                    .child(
                        v_flex()
                            .id("sheet-body")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px(px(16.))
                            .pb(px(14.))
                            .gap(px(10.))
                            .children(self.body),
                    )
                    .when(
                        !self.footer_start.is_empty() || !self.footer_end.is_empty(),
                        |el| {
                            el.child(
                                h_flex()
                                    .flex_none()
                                    .px(px(16.))
                                    .py(px(12.))
                                    .gap(px(8.))
                                    .border_t_1()
                                    .border_color(c.border)
                                    .bg(c.panel)
                                    .children(self.footer_start)
                                    .child(div().flex_1())
                                    .children(self.footer_end),
                            )
                        },
                    ),
            )
    }
}

/// One labelled control in a sheet.
///
/// The label column is a fixed width rather than intrinsic: a form whose
/// controls start at a different x on every row reads as a list of unrelated
/// things instead of as one form.
#[derive(IntoElement)]
pub struct FormRow {
    label: SharedString,
    hint: Option<SharedString>,
    error: Option<SharedString>,
    control: Option<AnyElement>,
    trailing: Vec<AnyElement>,
    plain: bool,
}

/// Width of the label column. Wide enough for "Root certificate" at the UI
/// size, which is the longest label any sheet currently has.
const LABEL_WIDTH: Pixels = px(104.);

impl FormRow {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            hint: None,
            error: None,
            control: None,
            trailing: Vec::new(),
            plain: false,
        }
    }

    pub fn child(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }

    /// The value is a line of text rather than a control in a box — the paths
    /// and the version in Settings. The nudge that puts a label on a field's
    /// baseline puts it *below* a bare line, so the two are centred on each
    /// other instead.
    pub fn plain(mut self) -> Self {
        self.plain = true;
        self
    }

    /// A second control on the same row — the port beside the host, the
    /// "choose…" button beside a path.
    pub fn trailing(mut self, child: impl IntoElement) -> Self {
        self.trailing.push(child.into_any_element());
        self
    }

    /// Grey text under the control. For explaining a default, not for
    /// restating the label.
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        // An empty hint is no hint. It would take a line under the control and
        // leave it blank, which is what a caller whose hint depends on the
        // state ends up passing on the states that have nothing to say.
        let hint = hint.into();
        self.hint = (!hint.is_empty()).then_some(hint);
        self
    }

    /// Replaces the hint when set, in the danger colour.
    pub fn error(mut self, error: impl Into<SharedString>) -> Self {
        self.error = Some(error.into());
        self
    }
}

impl RenderOnce for FormRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors();
        let note = self
            .error
            .clone()
            .map(|text| (text, IconColor::Custom(c.danger)))
            .or_else(|| self.hint.clone().map(|text| (text, IconColor::Subtle)));

        h_flex()
            .w_full()
            .gap(px(10.))
            .when_true(self.plain, |el| el.items_center())
            .when_true(!self.plain, |el| el.items_start())
            .child(
                div()
                    .w(LABEL_WIDTH)
                    .flex_none()
                    // Nudged down so the label sits on the control's baseline
                    // rather than on the top of its box.
                    .when_true(!self.plain, |el| el.pt(px(5.)))
                    .child(
                        Label::new(self.label)
                            .size(LabelSize::Small)
                            .color(IconColor::Muted),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .child(
                        h_flex()
                            .w_full()
                            .gap(px(6.))
                            .child(div().flex_1().min_w_0().children(self.control))
                            .children(self.trailing),
                    )
                    .children(note.map(|(text, color)| {
                        // Hints wrap. They are sentences, and a sentence cut
                        // off at the column's edge explains nothing.
                        Label::new(text).size(LabelSize::Small).color(color).wrap()
                    })),
            )
    }
}

/// A banner inside a sheet — the result of a connection test, a warning about
/// what is about to be run.
#[derive(IntoElement)]
pub struct Notice {
    tone: NoticeTone,
    message: SharedString,
    detail: Option<SharedString>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoticeTone {
    Info,
    Success,
    Warning,
    Danger,
}

impl Notice {
    pub fn new(tone: NoticeTone, message: impl Into<SharedString>) -> Self {
        Self {
            tone,
            message: message.into(),
            detail: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

impl RenderOnce for Notice {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors();
        let (fg, bg, icon) = match self.tone {
            NoticeTone::Info => (c.info, c.info_bg, IconName::CircleInfo),
            NoticeTone::Success => (c.success, c.success_bg, IconName::CircleCheck),
            NoticeTone::Warning => (c.warning, c.warning_bg, IconName::Warning),
            NoticeTone::Danger => (c.danger, c.danger_bg, IconName::CircleXmark),
        };

        h_flex()
            .w_full()
            .items_start()
            .gap(px(8.))
            .px(px(10.))
            .py(px(8.))
            .rounded(cx.metrics().radius_sm)
            .bg(bg)
            .child(
                div().flex_none().pt(px(1.)).child(
                    Icon::new(icon)
                        .size(IconSize::Small)
                        .color(IconColor::Custom(fg)),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    // Wrapped, not ellipsised: a notice is a sentence, and the
                    // half of a warning that fits on one line is the half that
                    // says the least.
                    .child(Label::new(self.message).size(LabelSize::Small).wrap())
                    .children(self.detail.map(|text| {
                        Label::new(text)
                            .size(LabelSize::Small)
                            .color(IconColor::Muted)
                            .wrap()
                    })),
            )
    }
}
