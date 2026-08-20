//! The text field.
//!
//! A frame — border, background, optional icon — wrapped around an [`Editor`] in
//! single-line configuration. It lives in this crate rather than in `ui` because
//! it *is* the editor: `ui` cannot depend on `editor`, and a second, simpler text
//! field implementation living in `ui` is exactly the duplication §11.3 of the
//! plan exists to prevent. Every field in the app — the sidebar filter, the grid
//! filter, the connection sheet — is one of these.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    div, px, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window,
};
use ui::{h_flex, ActiveTheme, Icon, IconColor, IconName, IconSize};

use crate::editor::{Editor, EditorEvent, EditorMode};
use crate::element::EditorStyle;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputSize {
    /// Filters and toolbars.
    Small,
    /// Forms and dialogs, where the field is the primary thing on the row.
    Medium,
}

impl InputSize {
    fn height(self) -> gpui::Pixels {
        match self {
            InputSize::Small => px(22.),
            InputSize::Medium => px(28.),
        }
    }

    fn icon(self) -> IconSize {
        match self {
            InputSize::Small => IconSize::Small,
            InputSize::Medium => IconSize::Medium,
        }
    }
}

pub struct Input {
    editor: Entity<Editor>,
    icon: Option<IconName>,
    size: InputSize,
    /// No box of its own. For a field that sits *inside* something already
    /// drawn as a field — the value slot in a filter chip — where a second
    /// border makes one control look like two.
    bare: bool,
    /// Forwards the inner editor's events, so a host subscribes to the `Input`
    /// and never has to know there is an `Editor` underneath.
    _forward: Subscription,
}

impl EventEmitter<EditorEvent> for Input {}

impl Focusable for Input {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus().clone()
    }
}

impl Input {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::new(EditorMode::SingleLine, cx);
            editor.set_style(EditorStyle::ui(cx));
            editor
        });
        let _forward = cx.subscribe(&editor, |_, _, event: &EditorEvent, cx| {
            cx.emit(event.clone());
        });
        Self {
            editor,
            icon: None,
            size: InputSize::Small,
            bare: false,
            _forward,
        }
    }

    pub fn placeholder(self, text: impl Into<SharedString>, cx: &mut App) -> Self {
        self.editor
            .update(cx, |editor, _| editor.set_placeholder(text));
        self
    }

    /// Bullets instead of characters. The value is still readable through
    /// [`Input::text`] — this hides it from the room, not from the app.
    pub fn masked(self, cx: &mut App) -> Self {
        self.editor.update(cx, |editor, _| editor.set_masked(true));
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Drop the field's own border and fill; see [`Input::bare`].
    pub fn bare(mut self) -> Self {
        self.bare = true;
        self
    }

    pub fn size(mut self, size: InputSize) -> Self {
        self.size = size;
        self
    }

    pub fn editor(&self) -> &Entity<Editor> {
        &self.editor
    }

    pub fn text(&self, cx: &App) -> String {
        self.editor.read(cx).text()
    }

    pub fn set_text(&self, text: &str, cx: &mut App) {
        self.editor
            .update(cx, |editor, cx| editor.set_text(text, cx));
    }

    pub fn is_empty(&self, cx: &App) -> bool {
        self.editor.read(cx).is_empty()
    }
}

impl Render for Input {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors();
        let m = cx.metrics();
        let focused = self.editor.read(cx).focus().contains_focused(window, cx);

        h_flex()
            .h(self.size.height())
            .gap_1p5()
            .when(!self.bare, |el| {
                el.px_1p5()
                    .rounded(m.radius_sm)
                    .bg(c.field)
                    .border_1()
                    .border_color(if focused { c.border_focus } else { c.border })
            })
            .when_some(self.icon, |el, icon| {
                el.child(
                    Icon::new(icon)
                        .size(self.size.icon())
                        .color(IconColor::Muted),
                )
            })
            // The editor takes the rest of the row and clips: a long value
            // scrolls inside the field rather than pushing the icon out of it.
            .child(div().flex_1().overflow_hidden().child(self.editor.clone()))
    }
}
