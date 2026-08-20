//! Acting on a database object rather than on its rows.
//!
//! Rename, truncate and drop, plus the right-click menu they hang off. Three
//! statements and a menu would not normally deserve a module, but the care is
//! all here: these are the only things in the app that destroy something the
//! grid cannot put back, and every one of them is one slip of the pointer away
//! from a table nobody has a copy of.
//!
//! So the shape is deliberate. Nothing here runs on a click: a menu item opens
//! a sheet, the sheet shows the exact statement it is about to send, and the
//! two that cannot be undone need the object's own name typed in before their
//! button turns on. The statement then goes through the same
//! [`crate::session::Session::run`] every other statement does — it lands in
//! the message log, in the status bar and in the history, because a `DROP` that
//! left no trace would be the one statement in the app you could not find
//! afterwards.

use gpui::{
    div, prelude::*, px, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Pixels, Point, Render, Styled, Window,
};
use ui::{
    ActiveTheme, Button, ButtonSize, ButtonVariant, ContextMenu, FormRow, IconColor, IconName,
    Label, LabelSize, MenuItem, Notice, NoticeTone, Sheet, Switch,
};

use editor::{Input, InputSize};

use crate::workspace::Workspace;

/// An open context menu: what it is about, and where it was asked for.
pub(crate) struct ObjectMenu {
    /// Window coordinates of the click that opened it.
    pub at: Point<Pixels>,
    pub reference: db::RelationRef,
    pub kind: db::RelationKind,
}

/// Which of the three destructive-ish sheets is up.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ObjectOp {
    Rename,
    Truncate,
    Drop,
}

impl ObjectOp {
    fn title(self, kind: db::RelationKind) -> String {
        let object = sqlgen::ddl::object_keyword(kind).to_lowercase();
        match self {
            Self::Rename => format!("Rename {object}"),
            Self::Truncate => "Truncate table".to_string(),
            Self::Drop => format!("Drop {object}"),
        }
    }

    /// Whether the object's own name has to be typed before the button works.
    ///
    /// Truncate does not ask: it destroys rows, which is bad, but the table and
    /// everything pointing at it survives, and a confirmation people meet daily
    /// is a confirmation people learn to click through. Drop asks, because the
    /// object itself is what stops existing.
    fn needs_typing(self) -> bool {
        matches!(self, Self::Drop)
    }
}

pub enum ObjectSheetEvent {
    Dismissed,
    /// Go ahead: run this, and it is this kind of operation.
    Confirmed {
        op: ObjectOp,
        sql: String,
    },
}

/// The sheet behind Rename, Truncate and Drop.
///
/// One entity for all three because they differ in two fields and a verb, and
/// three near-identical sheets is three places for the confirmation to be
/// subtly weaker in one of them.
pub struct ObjectSheet {
    op: ObjectOp,
    reference: db::RelationRef,
    kind: db::RelationKind,
    /// The new name for a rename, the typed confirmation for a drop. Unused by
    /// truncate, which asks nothing.
    field: Entity<Input>,
    /// `TRUNCATE … CASCADE` / `DROP … CASCADE`.
    cascade: bool,
    /// `TRUNCATE … RESTART IDENTITY`.
    restart_identity: bool,
    /// The planner's estimate, for the sentence that says how much is about to
    /// go. An estimate is said to be one — nobody should read "12,000 rows"
    /// here and believe it to the row.
    estimated_rows: i64,
    /// The sheet's own focus. Rename and Drop have a field to hold focus for
    /// them; Truncate has nothing to type, and without this there would be
    /// nowhere for its Escape to arrive.
    focus: FocusHandle,
    _subscription: gpui::Subscription,
}

impl EventEmitter<ObjectSheetEvent> for ObjectSheet {}

impl Focusable for ObjectSheet {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        match self.op {
            ObjectOp::Truncate => self.focus.clone(),
            _ => self.field.read(cx).focus_handle(cx),
        }
    }
}

impl ObjectSheet {
    pub fn new(op: ObjectOp, relation: &db::Relation, cx: &mut Context<Self>) -> Self {
        let reference = relation.reference.clone();
        let name = reference.name.to_string();
        let field = cx.new(|cx| {
            let input = Input::new(cx).size(InputSize::Medium);
            match op {
                // Pre-filled and selected: a rename usually starts from the old
                // name, and the common case is typing over it.
                ObjectOp::Rename => {
                    let input = input.placeholder("new_name", cx);
                    input.set_text(&name, cx);
                    input
                        .editor()
                        .update(cx, |editor, cx| editor.select_all(cx));
                    input
                }
                // Never pre-filled. The point of typing the name is that it
                // cannot be done by accident.
                _ => input.placeholder(name.clone(), cx),
            }
        });
        let _subscription =
            cx.subscribe(
                &field,
                |this, _, event: &editor::EditorEvent, cx| match event {
                    editor::EditorEvent::Submit => this.confirm(cx),
                    editor::EditorEvent::Cancel => cx.emit(ObjectSheetEvent::Dismissed),
                    editor::EditorEvent::Changed => cx.notify(),
                    _ => {}
                },
            );

        Self {
            op,
            reference,
            kind: relation.kind,
            field,
            cascade: false,
            restart_identity: false,
            estimated_rows: relation.estimated_rows,
            focus: cx.focus_handle(),
            _subscription,
        }
    }

    /// What the typed text is, trimmed.
    fn typed(&self, cx: &gpui::App) -> String {
        self.field.read(cx).text(cx).trim().to_string()
    }

    /// Whether the button is on, and why it might not be.
    fn ready(&self, cx: &gpui::App) -> bool {
        let typed = self.typed(cx);
        match self.op {
            // Renaming to the name it already has is a statement that succeeds
            // and does nothing, which is not what anyone pressed the button for.
            ObjectOp::Rename => !typed.is_empty() && typed != *self.reference.name,
            ObjectOp::Truncate => true,
            ObjectOp::Drop => typed == *self.reference.name,
        }
    }

    /// The statement this sheet is showing, and would send.
    fn statement(&self, cx: &gpui::App) -> String {
        match self.op {
            ObjectOp::Rename => sqlgen::ddl::rename(&self.reference, self.kind, &self.typed(cx)),
            ObjectOp::Truncate => {
                sqlgen::ddl::truncate(&self.reference, self.restart_identity, self.cascade)
            }
            ObjectOp::Drop => sqlgen::ddl::drop_object(&self.reference, self.kind, self.cascade),
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if !self.ready(cx) {
            return;
        }
        let sql = self.statement(cx);
        cx.emit(ObjectSheetEvent::Confirmed { op: self.op, sql });
    }
}

impl Render for ObjectSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors().clone();
        let m = cx.metrics().clone();
        let ready = self.ready(cx);
        let statement = self.statement(cx);
        let destructive = self.op != ObjectOp::Rename;

        div()
            // The sheet inside lays itself out against the whole window, so
            // this wrapper has to be the whole window too — a zero-sized box
            // would take the sheet down with it.
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            // Escape and Return, for the sheet that has no field to give them
            // to. Return confirms only what is ready to be confirmed, which is
            // the same rule the button follows.
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => cx.emit(ObjectSheetEvent::Dismissed),
                    "enter" => this.confirm(cx),
                    _ => {}
                }
            }))
            .child(
                Sheet::new("object-sheet", self.op.title(self.kind))
                    .subtitle(self.reference.to_string())
                    .width(px(500.))
                    .on_dismiss(cx.listener(|_, _, _, cx| cx.emit(ObjectSheetEvent::Dismissed)))
                    // Rename types the new name; drop types the old one back. Truncate
                    // has nothing to type, so it has no field at all rather than a
                    // disabled one.
                    .children((self.op != ObjectOp::Truncate).then(|| {
                        let label = match self.op {
                            ObjectOp::Rename => "New name",
                            _ => "Confirm",
                        };
                        let row = FormRow::new(label).child(self.field.clone());
                        match self.op.needs_typing() {
                            true => row.hint(format!("Type {} to confirm.", self.reference.name)),
                            false => row,
                        }
                    }))
                    .children(matches!(self.op, ObjectOp::Truncate).then(|| {
                        let sheet = cx.entity();
                        FormRow::new("Identity")
                            .hint("Start serial and identity columns again from their first value.")
                            .child(
                                Switch::new("truncate-restart", self.restart_identity).on_toggle(
                                    move |on, _, cx| {
                                        sheet.update(cx, |sheet, cx| {
                                            sheet.restart_identity = on;
                                            cx.notify();
                                        });
                                    },
                                ),
                            )
                    }))
                    .children(matches!(self.op, ObjectOp::Truncate).then(|| {
                        let sheet = cx.entity();
                        FormRow::new("Cascade")
                            .hint("Also empty every table with a foreign key pointing here.")
                            .child(Switch::new("truncate-cascade", self.cascade).on_toggle(
                                move |on, _, cx| {
                                    sheet.update(cx, |sheet, cx| {
                                        sheet.cascade = on;
                                        cx.notify();
                                    });
                                },
                            ))
                    }))
                    .children(matches!(self.op, ObjectOp::Drop).then(|| {
                        let sheet = cx.entity();
                        FormRow::new("Cascade")
                            .hint("Also drop the views, constraints and keys that depend on it.")
                            .child(Switch::new("drop-cascade", self.cascade).on_toggle(
                                move |on, _, cx| {
                                    sheet.update(cx, |sheet, cx| {
                                        sheet.cascade = on;
                                        cx.notify();
                                    });
                                },
                            ))
                    }))
                    // The statement itself, not a description of it. This is the last
                    // place it can be read before the server sees it.
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
                                .child(
                                    Label::new(format!("{statement};"))
                                        .size(LabelSize::Small)
                                        .color(IconColor::Muted)
                                        .mono()
                                        .wrap(),
                                ),
                        ),
                    )
                    .children(
                        self.warning()
                            .map(|text| Notice::new(NoticeTone::Danger, text)),
                    )
                    .footer_end(
                        Button::new("object-cancel", "Cancel")
                            .size(ButtonSize::Small)
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(ObjectSheetEvent::Dismissed)),
                            ),
                    )
                    .footer_end(
                        Button::new(
                            "object-confirm",
                            match self.op {
                                ObjectOp::Rename => "Rename",
                                ObjectOp::Truncate => "Truncate",
                                ObjectOp::Drop => "Drop",
                            },
                        )
                        .variant(if destructive {
                            ButtonVariant::Danger
                        } else {
                            ButtonVariant::Accent
                        })
                        .size(ButtonSize::Small)
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                    ),
            )
    }
}

impl ObjectSheet {
    /// The red sentence, when there is one worth reading.
    fn warning(&self) -> Option<String> {
        let rows = match self.estimated_rows {
            n if n <= 0 => "every row".to_string(),
            n => format!("about {} rows", crate::workspace::thousands(n as usize)),
        };
        match self.op {
            ObjectOp::Rename => None,
            ObjectOp::Truncate => Some(format!("Deletes {rows}. This cannot be undone.")),
            ObjectOp::Drop if self.cascade => Some(
                "Cascade also drops every view, foreign key and constraint that depends on this. This cannot be undone."
                    .to_string(),
            ),
            ObjectOp::Drop => Some("This cannot be undone.".to_string()),
        }
    }
}

impl Workspace {
    /// Right-click on a tree row.
    pub(crate) fn open_object_menu(
        &mut self,
        reference: db::RelationRef,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        // The kind decides which items the menu has, so a menu for an object
        // the catalog no longer knows about is not shown at all rather than
        // shown with the wrong verbs on it.
        let Some(kind) = self
            .snapshot(cx)
            .and_then(|snapshot| snapshot.relation(&reference).map(|r| r.kind))
        else {
            return;
        };
        self.menu = Some(ObjectMenu {
            at,
            reference,
            kind,
        });
        cx.notify();
    }

    pub(crate) fn close_object_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn render_object_menu(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let menu = self.menu.as_ref()?;
        let (at, reference, kind) = (menu.at, menu.reference.clone(), menu.kind);
        let connected = self.is_connected(cx);
        // Truncate is a table verb. Postgres will not truncate a view, and an
        // item that always fails is worse than an item that is not there.
        let has_rows = matches!(
            kind,
            db::RelationKind::Table | db::RelationKind::Partitioned
        );

        let open = reference.clone();
        let design = reference.clone();
        let copy_name = reference.clone();
        let copy_ddl = reference.clone();
        let rename = reference.clone();
        let truncate = reference.clone();
        let drop = reference.clone();

        Some(
            ContextMenu::new("object-menu")
                .at(at)
                .width(px(216.))
                .on_dismiss(cx.listener(|this, _, _, cx| this.close_object_menu(cx)))
                .item(
                    MenuItem::new("Open")
                        .icon(IconName::Table)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_relation(&open, cx);
                        })),
                )
                .item(
                    // Only for the things whose structure can be edited. A
                    // view's shape is its query, and that is a `CREATE OR
                    // REPLACE`, not a column list.
                    MenuItem::new("Design Table…")
                        .icon(IconName::Columns)
                        .disabled(!has_rows)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_structure(&design, cx);
                        })),
                )
                .separator()
                .item(
                    MenuItem::new("Copy Name")
                        .icon(IconName::Copy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let _ = this;
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                copy_name.qualified(),
                            ));
                        })),
                )
                .item(
                    MenuItem::new("Copy DDL")
                        .icon(IconName::Code)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let Some(relation) = this
                                .snapshot(cx)
                                .and_then(|snapshot| snapshot.relation(&copy_ddl).cloned())
                            else {
                                return;
                            };
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                sqlgen::ddl::relation(&relation),
                            ));
                        })),
                )
                .item(
                    MenuItem::new("Refresh Schema")
                        .icon(IconName::Refresh)
                        .shortcut("⌘R")
                        .disabled(!connected)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_schema(cx))),
                )
                .separator()
                .item(
                    MenuItem::new("Rename…")
                        .icon(IconName::Pen)
                        .disabled(!connected)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.prompt_object(ObjectOp::Rename, rename.clone(), cx);
                        })),
                )
                .item(
                    MenuItem::new("Truncate…")
                        .icon(IconName::CircleDashed)
                        .danger()
                        .disabled(!connected || !has_rows)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.prompt_object(ObjectOp::Truncate, truncate.clone(), cx);
                        })),
                )
                .item(
                    MenuItem::new("Drop…")
                        .icon(IconName::Trash)
                        .danger()
                        .disabled(!connected)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.prompt_object(ObjectOp::Drop, drop.clone(), cx);
                        })),
                ),
        )
    }

    /// Put up the sheet for one of the object operations.
    pub(crate) fn prompt_object(
        &mut self,
        op: ObjectOp,
        reference: db::RelationRef,
        cx: &mut Context<Self>,
    ) {
        let Some(relation) = self
            .snapshot(cx)
            .and_then(|snapshot| snapshot.relation(&reference).cloned())
        else {
            return;
        };
        let sheet = cx.new(|cx| ObjectSheet::new(op, &relation, cx));
        cx.subscribe(
            &sheet,
            move |this, _, event: &ObjectSheetEvent, cx| match event {
                // The console gets the focus back either way: the sheet was
                // holding it, and a window whose focus is on something that no
                // longer exists is a window where nothing types.
                ObjectSheetEvent::Dismissed => {
                    this.object_sheet = None;
                    this.focus_editor(cx);
                    cx.notify();
                }
                ObjectSheetEvent::Confirmed { op, sql } => {
                    this.object_sheet = None;
                    this.focus_editor(cx);
                    this.run_object_statement(*op, reference.clone(), sql.clone(), cx);
                }
            },
        )
        .detach();
        self.focus_next(sheet.focus_handle(cx));
        self.object_sheet = Some(sheet);
        cx.notify();
    }

    /// Send an object statement, and remember what it was about.
    ///
    /// What happens *after* it lands is the interesting part, and it cannot be
    /// decided here: a drop that failed must not close the tab, and a rename
    /// that failed must not retitle it. So this records the intent and
    /// [`Workspace::finish_object_statement`] acts on it once the server has
    /// answered.
    fn run_object_statement(
        &mut self,
        op: ObjectOp,
        reference: db::RelationRef,
        sql: String,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.clone() else {
            return;
        };
        if session.read(cx).is_busy() {
            log::warn!("not sending {op:?} on {reference}: the connection is busy");
            return;
        }
        // The new name comes back out of the statement rather than being
        // threaded alongside it, so what the tab is retitled to and what the
        // server was told can never be two different strings.
        self.pending_object = Some(PendingObject {
            op,
            reference,
            sql: sql.clone(),
        });
        session.update(cx, |session, cx| session.run(sql, cx));
        cx.notify();
    }

    /// Act on an object statement that has come back.
    ///
    /// Called from `absorb_run`, after the run has been logged like any other:
    /// the object operations are not special enough to bypass the message log,
    /// only special enough to have consequences beyond it.
    pub(crate) fn finish_object_statement(&mut self, failed: bool, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_object.take() else {
            return;
        };
        if failed {
            return;
        }
        match pending.op {
            // A renamed object keeps its tab: it is the same table, and closing
            // it because its name changed would lose the console beside it.
            ObjectOp::Rename => {
                if let Some(to) = renamed_to(&pending.sql) {
                    let renamed = db::RelationRef::new(pending.reference.schema.clone(), to);
                    self.retarget_tabs(&pending.reference, Some(renamed), cx);
                }
            }
            // The rows are gone, so what the grid is showing is a screenful of
            // rows that no longer exist. Re-ask rather than clear: the table is
            // still there, and an empty grid under a live table is the right
            // answer arrived at honestly.
            ObjectOp::Truncate => self.reload_active_relation(&pending.reference, cx),
            // The object is gone. Its tabs are about nothing.
            ObjectOp::Drop => self.retarget_tabs(&pending.reference, None, cx),
        }
        self.refresh_schema(cx);
        cx.notify();
    }
}

/// What an object statement is waiting on.
pub(crate) struct PendingObject {
    op: ObjectOp,
    reference: db::RelationRef,
    /// The statement as sent, so what happens next is derived from what was
    /// actually said rather than from a second copy of the intent.
    sql: String,
}

/// Point the tabs that were browsing `from` at `to`, or let go of the object.
///
/// A free function over the tabs rather than a method on the window, because
/// what it does is entirely a matter of what is in the list — which is also
/// what makes it testable without a window to put the list in.
pub(crate) fn retarget(
    tabs: &mut [crate::pane::CenterTab],
    from: &db::RelationRef,
    to: Option<&db::RelationRef>,
) {
    for tab in tabs {
        if tab.relation.as_ref() != Some(from) {
            continue;
        }
        match to {
            Some(to) => {
                tab.title = to.name.to_string().into();
                tab.detail = Some(to.schema.to_string().into());
                tab.relation = Some(to.clone());
            }
            None => {
                // The title stays as it was. A tab that renamed itself to
                // "Untitled" the moment a drop landed would leave nothing to
                // recognise the script in it by.
                tab.kind = crate::pane::CenterKind::Query;
                tab.relation = None;
            }
        }
    }
}

/// The new name out of a `RENAME TO`, unquoted.
fn renamed_to(sql: &str) -> Option<String> {
    let tail = sql.rsplit_once(" RENAME TO ")?.1.trim();
    let name = match tail.strip_prefix('"') {
        Some(quoted) => quoted.strip_suffix('"')?.replace("\"\"", "\""),
        None => tail.to_string(),
    };
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_new_name_is_read_back_out_of_the_statement() {
        assert_eq!(
            renamed_to("ALTER TABLE public.users RENAME TO people").as_deref(),
            Some("people")
        );
        assert_eq!(
            renamed_to(r#"ALTER TABLE public.users RENAME TO "Order Items""#).as_deref(),
            Some("Order Items")
        );
        assert_eq!(
            renamed_to(r#"ALTER TABLE public.users RENAME TO "say ""hi""""#).as_deref(),
            Some(r#"say "hi""#)
        );
        assert_eq!(renamed_to("TRUNCATE TABLE public.users"), None);
    }

    fn tab(title: &str, relation: Option<db::RelationRef>) -> crate::pane::CenterTab {
        crate::pane::CenterTab {
            kind: match relation {
                Some(_) => crate::pane::CenterKind::Table,
                None => crate::pane::CenterKind::Query,
            },
            title: title.to_string().into(),
            detail: relation.as_ref().map(|r| r.schema.to_string().into()),
            dirty: false,
            relation,
            saved_query: None,
            sql: format!("-- {title}"),
            filter: crate::filter::Filter::default(),
            page: None,
            structure: None,
            session: None,
            reconnect: None,
        }
    }

    #[test]
    fn a_renamed_table_takes_its_tabs_with_it() {
        let from = db::RelationRef::new("public", "users");
        let to = db::RelationRef::new("public", "people");
        let mut tabs = vec![
            tab("users", Some(from.clone())),
            tab("scratch", None),
            tab("orders", Some(db::RelationRef::new("public", "orders"))),
        ];
        retarget(&mut tabs, &from, Some(&to));

        assert_eq!(tabs[0].title, "people");
        assert_eq!(tabs[0].relation, Some(to));
        assert_eq!(tabs[0].kind, crate::pane::CenterKind::Table);
        // Its script is still its script: a rename is not a close.
        assert_eq!(tabs[0].sql, "-- users");
        assert_eq!(tabs[2].title, "orders", "another table is left alone");
    }

    #[test]
    fn a_dropped_table_leaves_its_tab_behind_as_a_query() {
        let from = db::RelationRef::new("public", "users");
        let mut tabs = vec![tab("users", Some(from.clone()))];
        retarget(&mut tabs, &from, None);

        assert_eq!(tabs[0].kind, crate::pane::CenterKind::Query);
        assert_eq!(tabs[0].relation, None);
        assert_eq!(tabs[0].title, "users", "still recognisable");
        assert_eq!(tabs[0].sql, "-- users", "and still holding its script");
    }
}
