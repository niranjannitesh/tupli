//! The sheet that names a query before it is saved.
//!
//! Saving is one field and two buttons, so it could have been an inline row in
//! the toolbar. It is a sheet because naming is the whole point: a query saved
//! under whatever the toolbar guessed is a query nobody finds again, and a
//! modal is what makes someone read the name before it becomes permanent.

use gpui::{
    div, px, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, SharedString, Styled, Window,
};
use ui::{
    ActiveTheme, Button, ButtonSize, ButtonVariant, FormRow, IconColor, Label, LabelSize, Sheet,
};

use editor::{Input, InputSize};

pub enum SaveSheetEvent {
    Dismissed,
    /// Save it under this name. Never empty — the button is disabled until
    /// there is something to call it.
    Named(String),
}

pub struct SaveQuerySheet {
    name: Entity<Input>,
    /// The statement being saved, on one line. Shown rather than described:
    /// with several tabs open, "which query is this" is a real question.
    preview: SharedString,
    /// Which connection it will be filed under, or the note that it will not be
    /// filed under any.
    scope: SharedString,
    /// True when this replaces an existing saved query rather than adding one.
    replacing: bool,
    /// The names already in use, so typing one of them can say what it will do
    /// before the button is pressed rather than after.
    taken: Vec<String>,
    _subscription: gpui::Subscription,
}

impl EventEmitter<SaveSheetEvent> for SaveQuerySheet {}

impl Focusable for SaveQuerySheet {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        // The field, not the card: the sheet exists to be typed into, and a
        // modal that opens with focus on nothing costs a click every time.
        self.name.read(cx).focus_handle(cx)
    }
}

impl SaveQuerySheet {
    pub fn new(
        suggested: &str,
        preview: impl Into<SharedString>,
        scope: impl Into<SharedString>,
        replacing: bool,
        taken: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            let input = Input::new(cx)
                .size(InputSize::Medium)
                .placeholder("Monthly revenue", cx);
            input.set_text(suggested, cx);
            // Selected, not just filled: the suggestion is a default, and the
            // common case is typing over it.
            input
                .editor()
                .update(cx, |editor, cx| editor.select_all(cx));
            input
        });
        // Enter is Save. A one-field form where the only way to commit is to
        // reach for the mouse is a form that annoys people twice a day.
        let _subscription =
            cx.subscribe(
                &name,
                |this, _, event: &editor::EditorEvent, cx| match event {
                    editor::EditorEvent::Submit => this.save(cx),
                    editor::EditorEvent::Cancel => cx.emit(SaveSheetEvent::Dismissed),
                    editor::EditorEvent::Changed => cx.notify(),
                    _ => {}
                },
            );

        Self {
            name,
            preview: preview.into(),
            scope: scope.into(),
            replacing,
            taken,
            _subscription,
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text(cx).trim().to_string();
        if name.is_empty() {
            return;
        }
        cx.emit(SaveSheetEvent::Named(name));
    }
}

impl Render for SaveQuerySheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors().clone();
        let m = cx.metrics().clone();
        let typed = self.name.read(cx).text(cx).trim().to_string();
        let named = !typed.is_empty();
        // Saving over a name that is already taken replaces that query, which
        // is the right behaviour and a surprising one to discover afterwards.
        let collides = named && self.taken.iter().any(|name| name == &typed);

        Sheet::new(
            "save-query-sheet",
            if self.replacing {
                "Rename Query"
            } else {
                "Save Query"
            },
        )
        .subtitle(self.scope.clone())
        .width(px(460.))
        .on_dismiss(cx.listener(|_, _, _, cx| cx.emit(SaveSheetEvent::Dismissed)))
        .child({
            let row = FormRow::new("Name").child(self.name.clone());
            if collides {
                row.hint("Replaces the saved query already called this.")
            } else {
                row
            }
        })
        .child(
            FormRow::new("Statement").child(
                div()
                    .w_full()
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(m.radius_sm)
                    .bg(c.field)
                    .border_1()
                    .border_color(c.border)
                    .overflow_hidden()
                    .child(
                        Label::new(self.preview.clone())
                            .size(LabelSize::Small)
                            .color(IconColor::Muted)
                            .mono(),
                    ),
            ),
        )
        .footer_end(
            Button::new("cancel", "Cancel")
                .size(ButtonSize::Small)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(SaveSheetEvent::Dismissed))),
        )
        .footer_end(
            Button::new("save", if collides { "Replace" } else { "Save" })
                .variant(ButtonVariant::Accent)
                .size(ButtonSize::Small)
                .disabled(!named)
                .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
        )
    }
}
