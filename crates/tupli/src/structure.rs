//! Designing a table.
//!
//! The other half of the write path. The grid edits rows; this edits the shape
//! the rows have to fit, which is the same job done one level up: type into a
//! draft, read the exact statements it would send, then send them as one
//! transaction. Postgres commits DDL transactionally, so a save that fails
//! half way leaves a table nobody has to un-break by hand.
//!
//! Nothing here writes SQL. The draft and both generators live in
//! [`sqlgen::table`], where they can be tested against every rename-and-retype
//! combination without a window, and this module is what a pointer does to
//! them: which cell is being typed into, which column is about to be dropped,
//! and whether Save is on yet.
//!
//! The editor holds two drafts — the table as the server described it, and the
//! table as it is being edited. Every question worth asking is a comparison of
//! the two: whether anything changed, what changed, and what statement would
//! make the first look like the second. Nothing is a flag, so nothing can
//! disagree with what is on screen.

use gpui::{
    div, prelude::*, px, AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement, Render, SharedString, Styled, Subscription, Window,
};
use ui::{
    h_flex, v_flex, ActiveTheme, Button, ButtonSize, ButtonVariant, Checkbox, Icon, IconColor,
    IconName, IconSize, Label, LabelSize, Notice, NoticeTone, Toolbar, Tooltip,
};

use editor::{Input, InputSize};
use sqlgen::table::{self, ColumnDraft, TableDraft};

/// Column widths, shared by the header and the rows under it so the two can
/// never drift apart.
const NUMBER_WIDTH: gpui::Pixels = px(28.);
const TYPE_WIDTH: gpui::Pixels = px(220.);
const FLAG_WIDTH: gpui::Pixels = px(44.);
const DEFAULT_WIDTH: gpui::Pixels = px(150.);
const DROP_WIDTH: gpui::Pixels = px(24.);
const ROW_HEIGHT: gpui::Pixels = px(28.);

/// Which text is being typed into. One field is open at a time, and it is the
/// same [`Input`] every time: forty columns is a hundred and sixty text fields,
/// and an app that builds all of them to show one caret is an app that opens a
/// table slowly.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Slot {
    TableName,
    TableComment,
    Column(usize, Field),
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Field {
    Name,
    Type,
    Default,
    Comment,
}

pub(crate) enum StructureEvent {
    /// Save was pressed. The workspace decides what that means — it is the one
    /// that has a connection and a preview sheet.
    Save,
    /// Something changed, so the tab's dot may need to appear or go away.
    Changed,
}

pub(crate) struct StructureEditor {
    /// The table on the server, or `None` while designing a new one.
    pub(crate) reference: Option<db::RelationRef>,
    /// The table as the catalog last described it. A save is diffed against
    /// this, not against whatever the editor was first drawn with.
    original: TableDraft,
    draft: TableDraft,
    editing: Option<Slot>,
    /// What the open field held when it was opened, so Escape has something to
    /// put back.
    before: String,
    input: Entity<Input>,
    focus: FocusHandle,
    _subscription: Subscription,
}

impl EventEmitter<StructureEvent> for StructureEditor {}

impl Focusable for StructureEditor {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl StructureEditor {
    /// An editor for a table that exists.
    pub(crate) fn editing(relation: &db::Relation, cx: &mut Context<Self>) -> Self {
        let draft = TableDraft::of(relation);
        Self::new(Some(relation.reference.clone()), draft, cx)
    }

    /// An editor for a table that does not exist yet.
    pub(crate) fn creating(schema: &str, cx: &mut Context<Self>) -> Self {
        Self::new(None, TableDraft::blank(schema), cx)
    }

    fn new(reference: Option<db::RelationRef>, draft: TableDraft, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| Input::new(cx).size(InputSize::Small));
        // Typing goes straight into the draft rather than being collected when
        // the field closes: a Save while a field still has the caret in it has
        // to send what is on screen, not what was there before it was clicked.
        let _subscription =
            cx.subscribe(
                &input,
                |this, _, event: &editor::EditorEvent, cx| match event {
                    editor::EditorEvent::Changed => this.absorb(cx),
                    editor::EditorEvent::Submit => this.close_field(cx),
                    editor::EditorEvent::Cancel => this.revert_field(cx),
                    _ => {}
                },
            );
        Self {
            reference,
            original: match reference_is_new(&draft) {
                // A table being created has nothing on the server to diff
                // against, and an "original" that matched the first draft
                // would make Save look like it had nothing to do.
                true => TableDraft::default(),
                false => draft.clone(),
            },
            draft,
            editing: None,
            before: String::new(),
            input,
            focus: cx.focus_handle(),
            _subscription,
        }
    }

    /// Take the table as the catalog now describes it.
    ///
    /// Called after a save has gone through, so that the next save is diffed
    /// against what the server actually did — which is not always what was
    /// asked for, since a type change can widen a default and a dropped key
    /// can take an index with it.
    pub(crate) fn adopt(&mut self, relation: &db::Relation, cx: &mut Context<Self>) {
        self.reference = Some(relation.reference.clone());
        self.original = TableDraft::of(relation);
        self.draft = self.original.clone();
        self.editing = None;
        cx.notify();
    }

    pub(crate) fn is_creating(&self) -> bool {
        self.reference.is_none()
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.draft != self.original
    }

    pub(crate) fn title(&self) -> SharedString {
        match self.draft.name.trim() {
            "" => "New Table".into(),
            name => name.to_string().into(),
        }
    }

    pub(crate) fn schema(&self) -> String {
        self.draft.schema.clone()
    }

    /// Where the table will be once this save lands.
    pub(crate) fn target(&self) -> db::RelationRef {
        self.draft.reference()
    }

    pub(crate) fn problems(&self) -> Vec<String> {
        table::problems(&self.draft)
    }

    /// Everything Save would send, in order.
    pub(crate) fn statements(&self) -> Vec<String> {
        match self.is_creating() {
            true => table::create(&self.draft),
            false => table::alter(&self.original, &self.draft),
        }
    }

    fn can_save(&self) -> bool {
        (self.is_creating() || self.is_dirty()) && self.problems().is_empty()
    }

    // ---- editing ---------------------------------------------------------

    fn open_field(&mut self, slot: Slot, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing == Some(slot) {
            return;
        }
        let text = self.text_of(slot);
        self.before = text.clone();
        self.editing = Some(slot);
        self.input.update(cx, |input, cx| {
            input.set_text(&text, cx);
            input
                .editor()
                .update(cx, |editor, cx| editor.select_all(cx));
        });
        window.focus(&self.input.focus_handle(cx), cx);
        cx.notify();
    }

    fn close_field(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        cx.notify();
    }

    /// Escape: put back what was there when the field was opened.
    fn revert_field(&mut self, cx: &mut Context<Self>) {
        if let Some(slot) = self.editing {
            let before = self.before.clone();
            self.write(slot, before);
        }
        self.editing = None;
        cx.emit(StructureEvent::Changed);
        cx.notify();
    }

    /// Take what has been typed into the draft.
    fn absorb(&mut self, cx: &mut Context<Self>) {
        let Some(slot) = self.editing else { return };
        let text = self.input.read(cx).text(cx);
        self.write(slot, text);
        cx.emit(StructureEvent::Changed);
        cx.notify();
    }

    fn text_of(&self, slot: Slot) -> String {
        match slot {
            Slot::TableName => self.draft.name.clone(),
            Slot::TableComment => self.draft.comment.clone(),
            Slot::Column(index, field) => match self.draft.columns.get(index) {
                Some(column) => match field {
                    Field::Name => column.name.clone(),
                    Field::Type => column.type_name.clone(),
                    Field::Default => column.default.clone(),
                    Field::Comment => column.comment.clone(),
                },
                None => String::new(),
            },
        }
    }

    fn write(&mut self, slot: Slot, text: String) {
        match slot {
            Slot::TableName => self.draft.name = text,
            Slot::TableComment => self.draft.comment = text,
            Slot::Column(index, field) => {
                let Some(column) = self.draft.columns.get_mut(index) else {
                    return;
                };
                match field {
                    Field::Name => column.name = text,
                    Field::Type => column.type_name = text,
                    Field::Default => column.default = text,
                    Field::Comment => column.comment = text,
                }
            }
        }
    }

    fn add_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.draft.columns.push(ColumnDraft::new());
        let index = self.draft.columns.len() - 1;
        // Straight into the name field: an empty row that has to be clicked
        // before it can be typed in is an extra click every single time.
        self.open_field(Slot::Column(index, Field::Name), window, cx);
        cx.emit(StructureEvent::Changed);
        cx.notify();
    }

    /// Take a column out of the draft.
    ///
    /// No confirmation here. Nothing has been sent yet, the preview sheet
    /// spells out the `DROP COLUMN` before anything is, and Revert puts the
    /// whole table back.
    fn remove_column(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.draft.columns.len() {
            return;
        }
        self.draft.columns.remove(index);
        self.editing = None;
        cx.emit(StructureEvent::Changed);
        cx.notify();
    }

    /// The change the screenshot knob stages: one column made required and
    /// commented, one new column added. Not reachable from the UI.
    pub(crate) fn demo_change(&mut self, cx: &mut Context<Self>) {
        if let Some(column) = self
            .draft
            .columns
            .iter_mut()
            .find(|column| !column.is_server_owned() && !column.is_new())
        {
            column.nullable = false;
            column.comment = "Where to write.".into();
        }
        self.draft.columns.push(ColumnDraft {
            name: "archived_at".into(),
            type_name: "timestamp with time zone".into(),
            comment: "When it stopped counting.".into(),
            ..ColumnDraft::new()
        });
        cx.emit(StructureEvent::Changed);
        cx.notify();
    }

    fn revert(&mut self, cx: &mut Context<Self>) {
        self.draft = self.original.clone();
        self.editing = None;
        cx.emit(StructureEvent::Changed);
        cx.notify();
    }
}

/// A draft that has never been to the server: the caller has just built it out
/// of [`TableDraft::blank`], which is the only draft with no schema-side twin.
fn reference_is_new(draft: &TableDraft) -> bool {
    draft.columns.iter().all(ColumnDraft::is_new)
}

impl Render for StructureEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors().clone();
        let problems = self.problems();
        let dirty = self.is_dirty();
        let creating = self.is_creating();
        let can_save = self.can_save();

        v_flex()
            .track_focus(&self.focus)
            .flex_1()
            .min_h_0()
            .child(self.render_toolbar(creating, dirty, can_save, cx))
            .child(self.render_identity(cx))
            .child(header_row(&c))
            .child(
                v_flex()
                    .id("structure-columns")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(
                        (0..self.draft.columns.len())
                            .map(|index| self.render_column(index, cx))
                            .collect::<Vec<_>>(),
                    ),
            )
            // Only ever one problem at a time: the list is ordered by where it
            // is on screen, and the first one is the one to go and fix.
            .children(problems.first().map(|problem| {
                div()
                    .flex_none()
                    .px(px(8.))
                    .py(px(6.))
                    .child(Notice::new(NoticeTone::Warning, problem.clone()))
            }))
    }
}

impl StructureEditor {
    fn render_toolbar(
        &self,
        creating: bool,
        dirty: bool,
        can_save: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Toolbar::new("structure-toolbar")
            .transparent()
            .borderless()
            .start_child(
                h_flex()
                    .gap(px(5.))
                    .child(
                        Icon::new(IconName::Columns)
                            .size(IconSize::Small)
                            .color(IconColor::Muted),
                    )
                    .child(Label::new(self.draft.schema.clone()).color(IconColor::Muted))
                    .child(Label::new("/").color(IconColor::Disabled))
                    .child(Label::new(self.title()).medium()),
            )
            .end_child(
                Button::new("structure-add", "Add Column")
                    .start_icon(IconName::Plus)
                    .size(ButtonSize::Small)
                    .on_click(cx.listener(|this, _, window, cx| this.add_column(window, cx))),
            )
            .end_child(
                Button::new("structure-revert", "Revert")
                    .size(ButtonSize::Small)
                    .disabled(!dirty)
                    .on_click(cx.listener(|this, _, _, cx| this.revert(cx))),
            )
            .end_child(
                Button::new(
                    "structure-save",
                    if creating {
                        "Create Table"
                    } else {
                        "Save Changes"
                    },
                )
                .variant(ButtonVariant::Accent)
                .size(ButtonSize::Small)
                .disabled(!can_save)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(StructureEvent::Save))),
            )
    }

    /// The table's own name and comment, above the columns they belong to.
    fn render_identity(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors().clone();
        h_flex()
            .flex_none()
            .w_full()
            .h(ROW_HEIGHT)
            .px(px(8.))
            .gap(px(8.))
            .border_b_1()
            .border_color(c.border)
            .child(
                Label::new("Name")
                    .size(LabelSize::Small)
                    .color(IconColor::Subtle),
            )
            .child(
                div()
                    .w(px(200.))
                    .child(self.field(Slot::TableName, "table_name", true, cx)),
            )
            .child(
                Label::new("Comment")
                    .size(LabelSize::Small)
                    .color(IconColor::Subtle),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(self.field(Slot::TableComment, "—", false, cx)),
            )
    }

    fn render_column(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let c = cx.colors().clone();
        let column = &self.draft.columns[index];
        let server_owned = column.is_server_owned();
        let is_new = column.is_new();
        // What this row will do when Save is pressed, said in the margin rather
        // than in a column of its own: it applies to two rows out of forty.
        let marker = match (is_new, self.changed(column)) {
            (true, _) => Some((IconName::Plus, IconColor::Accent)),
            (false, true) => Some((IconName::Pen, IconColor::Warning)),
            _ => None,
        };

        h_flex()
            .w_full()
            .flex_none()
            .h(ROW_HEIGHT)
            .px(px(8.))
            .gap(px(8.))
            .when(index % 2 == 1, |el| el.bg(c.grid_stripe))
            .child(
                h_flex()
                    .w(NUMBER_WIDTH)
                    .flex_none()
                    .justify_center()
                    .child(match marker {
                        Some((icon, color)) => Icon::new(icon)
                            .size(IconSize::XSmall)
                            .color(color)
                            .flat()
                            .into_any_element(),
                        None => Label::new(format!("{}", index + 1))
                            .size(LabelSize::Small)
                            .color(IconColor::Disabled)
                            .into_any_element(),
                    }),
            )
            .child(div().flex_1().min_w_0().child(self.field(
                Slot::Column(index, Field::Name),
                "name",
                true,
                cx,
            )))
            .child(
                div()
                    .w(TYPE_WIDTH)
                    .flex_none()
                    // The server's own columns are shown and left alone: the
                    // type of an identity column is not changed by typing over
                    // it, and a field that pretends otherwise is a lie with a
                    // caret in it.
                    .child(match server_owned {
                        true => static_text(&column.type_name, true, IconColor::Disabled),
                        false => self.field(Slot::Column(index, Field::Type), "type", true, cx),
                    }),
            )
            .child(
                h_flex()
                    .w(FLAG_WIDTH)
                    .flex_none()
                    .justify_center()
                    .child(self.null_box(index, cx)),
            )
            .child(
                h_flex()
                    .w(FLAG_WIDTH)
                    .flex_none()
                    .justify_center()
                    .child(self.key_box(index, cx)),
            )
            .child(
                div()
                    .w(DEFAULT_WIDTH)
                    .flex_none()
                    .child(match server_owned {
                        true => static_text(server_note(column), false, IconColor::Subtle),
                        false => self.field(Slot::Column(index, Field::Default), "—", true, cx),
                    }),
            )
            .child(div().flex_1().min_w_0().child(self.field(
                Slot::Column(index, Field::Comment),
                "—",
                false,
                cx,
            )))
            .child(
                h_flex().w(DROP_WIDTH).flex_none().child(
                    Button::icon(("structure-drop", index), IconName::Trash)
                        .size(ButtonSize::XSmall)
                        .tooltip(Tooltip::text("Drop Column"))
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_column(index, cx))),
                ),
            )
            .into_any_element()
    }

    /// Has this column been edited since the catalog described it?
    fn changed(&self, column: &ColumnDraft) -> bool {
        let Some(origin) = column.origin.as_deref() else {
            return false;
        };
        self.original
            .columns
            .iter()
            .find(|c| c.origin.as_deref() == Some(origin))
            .is_none_or(|was| was != column)
    }

    fn null_box(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let column = &self.draft.columns[index];
        let editor = cx.entity();
        Checkbox::new(("structure-null", index), column.nullable)
            // A generated or identity column's nullability is not this
            // editor's to change.
            .disabled(column.is_server_owned())
            .on_toggle(move |on, _, cx| {
                editor.update(cx, |this, cx| {
                    if let Some(column) = this.draft.columns.get_mut(index) {
                        column.nullable = on;
                    }
                    cx.emit(StructureEvent::Changed);
                    cx.notify();
                });
            })
    }

    fn key_box(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let column = &self.draft.columns[index];
        let editor = cx.entity();
        Checkbox::new(("structure-key", index), column.is_pk).on_toggle(move |on, _, cx| {
            editor.update(cx, |this, cx| {
                if let Some(column) = this.draft.columns.get_mut(index) {
                    column.is_pk = on;
                    // A key column that can be null is not a key Postgres will
                    // accept, so saying it is one says the rest too.
                    if on {
                        column.nullable = false;
                    }
                }
                cx.emit(StructureEvent::Changed);
                cx.notify();
            });
        })
    }

    /// One editable cell: the text, until it is clicked.
    fn field(
        &self,
        slot: Slot,
        placeholder: &'static str,
        mono: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.editing == Some(slot) {
            return div().child(self.input.clone()).into_any_element();
        }
        let c = cx.colors().clone();
        let text = self.text_of(slot);
        let empty = text.trim().is_empty();

        div()
            .id(("structure-field", slot_id(slot)))
            .w_full()
            .px(px(5.))
            .py(px(2.))
            .rounded(cx.metrics().radius)
            .overflow_hidden()
            .cursor_pointer()
            .hover(|el| el.bg(c.hover))
            .child(
                Label::new(match empty {
                    true => placeholder.to_string(),
                    false => text,
                })
                .size(if mono {
                    LabelSize::Code
                } else {
                    LabelSize::Small
                })
                .when(mono, Label::mono)
                .color(match empty {
                    true => IconColor::Disabled,
                    false => IconColor::Default,
                }),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.open_field(slot, window, cx)))
            .into_any_element()
    }
}

/// A cell that cannot be typed into, drawn to line up with the ones that can.
fn static_text(text: &str, mono: bool, color: IconColor) -> AnyElement {
    div()
        .px(px(5.))
        .py(px(2.))
        .overflow_hidden()
        .child(
            Label::new(match text.trim().is_empty() {
                true => "—".to_string(),
                false => text.to_string(),
            })
            .size(if mono {
                LabelSize::Code
            } else {
                LabelSize::Small
            })
            .when(mono, Label::mono)
            .color(color),
        )
        .into_any_element()
}

/// What fills a server-owned column's default cell — the one word that says
/// why there is nothing to type there.
fn server_note(column: &ColumnDraft) -> &'static str {
    match column.identity {
        Some(_) => "identity",
        None => "generated",
    }
}

/// An id for a cell that is stable across frames and unique in the pane.
fn slot_id(slot: Slot) -> usize {
    match slot {
        Slot::TableName => 0,
        Slot::TableComment => 1,
        Slot::Column(index, field) => {
            2 + index * 4
                + match field {
                    Field::Name => 0,
                    Field::Type => 1,
                    Field::Default => 2,
                    Field::Comment => 3,
                }
        }
    }
}

fn header_row(c: &ui::ThemeColors) -> impl IntoElement {
    let label = |text: &'static str| {
        Label::new(text)
            .size(LabelSize::Small)
            .color(IconColor::Subtle)
    };
    h_flex()
        .w_full()
        .flex_none()
        .h(px(22.))
        .px(px(8.))
        .gap(px(8.))
        .bg(c.chrome)
        .border_b_1()
        .border_color(c.border)
        .child(h_flex().w(NUMBER_WIDTH).flex_none().child(label("#")))
        .child(h_flex().flex_1().min_w_0().child(label("Name")))
        .child(h_flex().w(TYPE_WIDTH).flex_none().child(label("Type")))
        .child(
            h_flex()
                .w(FLAG_WIDTH)
                .flex_none()
                .justify_center()
                .child(label("Null")),
        )
        .child(
            h_flex()
                .w(FLAG_WIDTH)
                .flex_none()
                .justify_center()
                .child(label("Key")),
        )
        .child(
            h_flex()
                .w(DEFAULT_WIDTH)
                .flex_none()
                .child(label("Default")),
        )
        .child(h_flex().flex_1().min_w_0().child(label("Comment")))
        .child(div().w(DROP_WIDTH).flex_none())
}

// ---- the window's half ---------------------------------------------------

/// A structure save that has been asked for and not yet sent.
pub(crate) struct StructurePreview {
    pub editor: Entity<StructureEditor>,
    pub statements: Vec<String>,
    pub creating: bool,
}

/// A structure save that has been sent and not yet answered.
///
/// Held until the *catalog* comes back rather than until the transaction does:
/// what the editor should show next is what the server ended up with, and that
/// is only knowable from a fresh snapshot.
pub(crate) struct PendingStructure {
    pub editor: Entity<StructureEditor>,
    /// Where the table was before the save. `None` when it was being created.
    pub from: Option<db::RelationRef>,
    /// Where it is meant to be afterwards.
    pub to: db::RelationRef,
}

impl crate::workspace::Workspace {
    /// Give the restored design tabs their editors, once there is a catalog to
    /// build them from.
    ///
    /// A tab is restored with the table it was designing but not with the
    /// draft: a half-typed design from before a restart is not something to
    /// put back and call the table's shape. A tab whose table is no longer
    /// there keeps its placeholder — the tab says which one it was, which is
    /// more use than a tab that quietly disappeared.
    pub(crate) fn hydrate_structure_tabs(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot(cx) else {
            return;
        };
        let empty: Vec<(usize, usize, db::RelationRef)> = self
            .panes
            .iter()
            .enumerate()
            .flat_map(|(p, pane)| {
                pane.tabs.iter().enumerate().filter_map(move |(t, tab)| {
                    (tab.kind == crate::workspace::CenterKind::Structure && tab.structure.is_none())
                        .then(|| tab.relation.clone())
                        .flatten()
                        .map(|reference| (p, t, reference))
                })
            })
            .collect();
        for (p, t, reference) in empty {
            let Some(relation) = snapshot.relation(&reference).cloned() else {
                continue;
            };
            let editor = cx.new(|cx| StructureEditor::editing(&relation, cx));
            cx.subscribe(&editor, Self::on_structure_event).detach();
            self.panes[p].tabs[t].structure = Some(editor);
        }
        cx.notify();
    }

    /// Open the structure editor for a table that exists.
    pub(crate) fn open_structure(&mut self, reference: &db::RelationRef, cx: &mut Context<Self>) {
        let Some(relation) = self
            .snapshot(cx)
            .and_then(|snapshot| snapshot.relation(reference).cloned())
        else {
            log::warn!("no structure to open: the catalog does not have {reference}");
            return;
        };
        // One editor per table per pane. A second tab for the same table would
        // be two drafts of one thing, and the one you were not looking at
        // would be the one that saved.
        if let Some(index) = self.pane().tabs.iter().position(|tab| {
            tab.kind == crate::workspace::CenterKind::Structure
                && tab.relation.as_ref() == Some(reference)
        }) {
            self.show_tab(index, cx);
            cx.notify();
            return;
        }
        let editor = cx.new(|cx| StructureEditor::editing(&relation, cx));
        self.add_structure_tab(
            editor,
            reference.name.to_string(),
            Some(reference.schema.to_string()),
            Some(reference.clone()),
            cx,
        );
    }

    /// Design a table that does not exist yet, in the schema statements are
    /// landing in.
    pub(crate) fn new_table(&mut self, cx: &mut Context<Self>) {
        let schema = self.current_location(cx).1;
        let editor = cx.new(|cx| StructureEditor::creating(&schema, cx));
        self.add_structure_tab(
            editor,
            "New Table".into(),
            Some(schema.to_string()),
            None,
            cx,
        );
    }

    /// Open a design tab from the boot knob, with a change already staged and,
    /// if asked, the preview sheet over it. Screenshots need a state that takes
    /// six keystrokes to reach, and typing them is not something a screenshot
    /// script can do.
    pub(crate) fn demo_structure(
        &mut self,
        reference: Option<db::RelationRef>,
        preview: bool,
        cx: &mut Context<Self>,
    ) {
        match &reference {
            Some(reference) => self.open_structure(reference, cx),
            None => self.new_table(cx),
        }
        let Some(editor) = self
            .pane()
            .tabs
            .last()
            .and_then(|tab| tab.structure.clone())
        else {
            return;
        };
        editor.update(cx, |editor, cx| editor.demo_change(cx));
        if preview {
            self.on_structure_event(editor, &StructureEvent::Save, cx);
        }
    }

    fn add_structure_tab(
        &mut self,
        editor: Entity<StructureEditor>,
        title: String,
        detail: Option<String>,
        relation: Option<db::RelationRef>,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe(&editor, Self::on_structure_event).detach();
        let session = self.session.clone();
        self.pane_mut().tabs.push(crate::workspace::CenterTab {
            key: None,
            kind: crate::workspace::CenterKind::Structure,
            title: title.into(),
            detail: detail.map(Into::into),
            dirty: false,
            relation,
            saved_query: None,
            sql: String::new(),
            filter: crate::filter::Filter::default(),
            page: None,
            structure: Some(editor.clone()),
            session,
            reconnect: None,
        });
        let index = self.pane().tabs.len() - 1;
        self.show_tab(index, cx);
        self.focus_next(editor.focus_handle(cx));
        self.save_session(cx);
        cx.notify();
    }

    fn on_structure_event(
        &mut self,
        editor: Entity<StructureEditor>,
        event: &StructureEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            StructureEvent::Changed => self.sync_structure_tab(&editor, cx),
            StructureEvent::Save => {
                let statements = editor.read(cx).statements();
                // Save is off when there is nothing to save, so this is the
                // case where the only difference was whitespace.
                if statements.is_empty() {
                    return;
                }
                let creating = editor.read(cx).is_creating();
                self.structure_preview = Some(StructurePreview {
                    editor,
                    statements,
                    creating,
                });
                cx.notify();
            }
        }
    }

    /// Keep the tab's name and its unsaved dot in step with the draft.
    fn sync_structure_tab(&mut self, editor: &Entity<StructureEditor>, cx: &mut Context<Self>) {
        let (title, detail, dirty, reference) = {
            let editor = editor.read(cx);
            (
                editor.title(),
                editor.schema(),
                editor.is_dirty(),
                editor.reference.clone(),
            )
        };
        for pane in self.panes.iter_mut() {
            for tab in pane.tabs.iter_mut() {
                if tab.structure.as_ref() == Some(editor) {
                    tab.title = title.clone();
                    tab.detail = Some(detail.clone().into());
                    tab.dirty = dirty;
                    tab.relation = reference.clone();
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn cancel_structure_preview(&mut self, cx: &mut Context<Self>) {
        if let Some(preview) = self.structure_preview.take() {
            self.focus_next(preview.editor.focus_handle(cx));
        }
        cx.notify();
    }

    /// Send the whole design as one transaction.
    ///
    /// Postgres commits DDL transactionally, so eleven `ALTER`s that fail on
    /// the eighth leave a table nobody has to repair by hand — which is the
    /// only reason this editor is allowed to write eleven statements from one
    /// button.
    pub(crate) fn commit_structure(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.structure_preview.take() else {
            return;
        };
        let Some(session) = self.session.clone() else {
            return;
        };
        if session.read(cx).is_busy() {
            log::warn!("not saving the structure: the connection is busy");
            return;
        }
        let statements: Vec<sqlgen::Statement> = preview
            .statements
            .iter()
            .map(|sql| sqlgen::Statement {
                sql: sql.clone(),
                params: Vec::new(),
                kind: sqlgen::StatementKind::Ddl,
                expect_rows: None,
            })
            .collect();
        let counts = sqlgen::Counts {
            ddl: statements.len(),
            ..Default::default()
        };
        self.pending_structure = Some(PendingStructure {
            from: preview.editor.read(cx).reference.clone(),
            to: preview.editor.read(cx).target(),
            editor: preview.editor.clone(),
        });
        self.focus_next(preview.editor.focus_handle(cx));
        session.update(cx, |session, cx| session.apply(statements, counts, cx));
        cx.notify();
    }

    /// A structure save has come back. Returns whether there was one, so the
    /// commit path knows this was not a grid commit and leaves the grid alone.
    pub(crate) fn finish_structure(&mut self, failed: bool, cx: &mut Context<Self>) -> bool {
        if self.pending_structure.is_none() {
            return false;
        }
        match failed {
            // The draft stays exactly as it was: the transaction rolled back,
            // so what is on screen is still what is not saved.
            true => {
                self.pending_structure = None;
            }
            // Everything else waits for the catalog. What the table looks like
            // now is the server's answer, not the draft's.
            false => self.refresh_schema(cx),
        }
        true
    }

    /// Take the table as the fresh catalog describes it, once one arrives.
    pub(crate) fn adopt_structure(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_structure.take() else {
            return;
        };
        let Some(relation) = self
            .snapshot(cx)
            .and_then(|snapshot| snapshot.relation(&pending.to).cloned())
        else {
            log::warn!("saved the structure, but the catalog has no {}", pending.to);
            return;
        };
        pending
            .editor
            .update(cx, |editor, cx| editor.adopt(&relation, cx));
        // A renamed table takes its other tabs with it, exactly as it does
        // when it is renamed from the object menu.
        if let Some(from) = &pending.from {
            if from != &pending.to {
                self.retarget_tabs(from, Some(pending.to.clone()), cx);
            }
        }
        self.sync_structure_tab(&pending.editor, cx);
    }

    /// The sheet: every statement the save is about to send, in order.
    pub(crate) fn render_structure_preview(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let preview = self.structure_preview.as_ref()?;
        let c = cx.colors().clone();
        let ty = cx.typography().clone();
        let lines: Vec<String> = preview.statements.clone();
        // A dropped column is the one thing here that cannot be typed back,
        // and it is worth saying out loud before it is sent.
        let dropped = lines
            .iter()
            .filter(|sql| sql.contains(" DROP COLUMN "))
            .count();

        Some(
            ui::Sheet::new("structure-preview", "Save structure")
                .subtitle(format!(
                    "{} in one transaction. Nothing is changed unless all of it succeeds.",
                    match lines.len() {
                        1 => "1 statement".to_string(),
                        n => format!("{n} statements"),
                    }
                ))
                .width(px(620.))
                .child(
                    div()
                        .id("structure-preview-sql")
                        .max_h(px(320.))
                        .overflow_y_scroll()
                        .p(px(10.))
                        .rounded(cx.metrics().radius)
                        .bg(c.field)
                        .border_1()
                        .border_color(c.border)
                        .font(ty.mono_font())
                        .child(v_flex().gap(px(6.)).children(lines.into_iter().map(|line| {
                            Label::new(format!("{line};"))
                                .mono()
                                .size(LabelSize::Code)
                                .color(IconColor::Muted)
                                .wrap()
                        }))),
                )
                .children((dropped > 0).then(|| {
                    Notice::new(
                        NoticeTone::Danger,
                        match dropped {
                            1 => "A column is dropped, with everything in it. This cannot be undone."
                                .to_string(),
                            n => format!(
                                "{n} columns are dropped, with everything in them. This cannot be undone."
                            ),
                        },
                    )
                }))
                .on_dismiss(cx.listener(|this, _, _, cx| this.cancel_structure_preview(cx)))
                .footer_end(
                    Button::new("structure-preview-cancel", "Cancel")
                        .size(ButtonSize::Small)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_structure_preview(cx))),
                )
                .footer_end(
                    Button::new(
                        "structure-preview-save",
                        if preview.creating { "Create" } else { "Save" },
                    )
                    .variant(match dropped {
                        0 => ButtonVariant::Accent,
                        _ => ButtonVariant::Danger,
                    })
                    .size(ButtonSize::Small)
                    .on_click(cx.listener(|this, _, _, cx| this.commit_structure(cx))),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use db::{ColumnDef, IdentityKind, IndexDef, Relation, RelationKind, ValueKind};
    use gpui::TestAppContext;

    use super::*;

    fn column(name: &str, type_name: &str, nullable: bool) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            position: 1,
            type_name: type_name.into(),
            kind: ValueKind::Text,
            nullable,
            default: None,
            identity: None,
            is_generated: false,
            comment: None,
        }
    }

    fn users() -> Relation {
        Relation {
            reference: db::RelationRef::new("public", "users"),
            kind: RelationKind::Table,
            columns: vec![
                ColumnDef {
                    identity: Some(IdentityKind::ByDefault),
                    ..column("id", "bigint", false)
                },
                column("email", "text", false),
            ],
            indexes: vec![IndexDef {
                name: "users_pkey".into(),
                columns: vec!["id".into()],
                is_unique: true,
                is_primary: true,
                method: "btree".into(),
                predicate: None,
            }],
            foreign_keys: Vec::new(),
            checks: Vec::new(),
            triggers: Vec::new(),
            definition: None,
            estimated_rows: 0,
            size_bytes: 0,
            comment: None,
            detail_loaded: true,
        }
    }

    /// The editor holds an `Input`, and an input asks the theme how tall it is
    /// the moment it is built. Every test here needs one in place first.
    fn app(cx: &mut TestAppContext) {
        cx.update(|cx| ui::Theme::set_global(ui::Theme::of(ui::Appearance::Dark), cx));
    }

    fn editing(cx: &mut TestAppContext) -> Entity<StructureEditor> {
        app(cx);
        cx.update(|cx| cx.new(|cx| StructureEditor::editing(&users(), cx)))
    }

    /// A table just opened is a table nobody has changed: no dot on the tab, no
    /// Save, and nothing to send if the button were pressed anyway.
    #[gpui::test]
    fn a_table_just_opened_has_nothing_to_save(cx: &mut TestAppContext) {
        let editor = editing(cx);
        editor.update(cx, |editor, _| {
            assert!(!editor.is_dirty());
            assert!(!editor.can_save());
            assert!(editor.statements().is_empty());
            assert_eq!(editor.title(), "users");
        });
    }

    /// The tab is named after the draft, not after the table, so a rename shows
    /// up before it is saved — and turns back into the old name on Revert.
    #[gpui::test]
    fn renaming_the_table_renames_the_tab_and_reverting_puts_it_back(cx: &mut TestAppContext) {
        let editor = editing(cx);
        editor.update(cx, |editor, cx| {
            editor.draft.name = "people".into();
            assert_eq!(editor.title(), "people");
            assert!(editor.is_dirty());
            assert_eq!(
                editor.statements(),
                ["ALTER TABLE public.users RENAME TO people"]
            );
            editor.revert(cx);
            assert_eq!(editor.title(), "users");
            assert!(!editor.is_dirty());
        });
    }

    /// What comes back from the server replaces the draft, and is then the
    /// thing the next save is diffed against: a second Save straight after the
    /// first has nothing left to send.
    #[gpui::test]
    fn adopting_the_saved_table_leaves_nothing_to_save_again(cx: &mut TestAppContext) {
        let editor = editing(cx);
        editor.update(cx, |editor, cx| {
            editor.draft.columns[1].nullable = true;
            assert!(editor.is_dirty());
            let mut saved = users();
            saved.columns[1].nullable = true;
            editor.adopt(&saved, cx);
            assert!(!editor.is_dirty());
            assert!(editor.statements().is_empty());
        });
    }

    /// A table that does not exist yet is created, never altered, and cannot be
    /// saved until it has a name — the button is off and the sheet says why.
    #[gpui::test]
    fn a_new_table_is_created_once_it_has_a_name(cx: &mut TestAppContext) {
        app(cx);
        let editor = cx.update(|cx| cx.new(|cx| StructureEditor::creating("public", cx)));
        editor.update(cx, |editor, _| {
            assert!(editor.is_creating());
            assert!(!editor.can_save(), "a table with no name is not savable");
            assert_eq!(editor.problems(), ["The table needs a name."]);

            editor.draft.name = "invoices".into();
            assert!(editor.can_save());
            let statements = editor.statements();
            assert_eq!(statements.len(), 1);
            assert!(
                statements[0].starts_with("CREATE TABLE public.invoices ("),
                "{}",
                statements[0]
            );
        });
    }
}
