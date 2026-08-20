//! Writing the grid back: what can be edited, and what happens on Commit.
//!
//! The staging model and the SQL generator live in the `sqlgen` crate and know
//! nothing about a window; the grid holds the staged changes and knows nothing
//! about a catalog. This module is the seam between them — it is where a result
//! set is matched against the relation it came out of, where the toolbar's
//! buttons turn into calls on the grid, and where a commit becomes one
//! transaction on the session's connection.
//!
//! See docs/PLAN.md §12.

use gpui::{div, prelude::*, px, Context, IntoElement, ParentElement, SharedString, Styled};
use sqlgen::{Concurrency, Identity, NotEditable, Statement, Target};
use ui::{
    v_flex, ActiveTheme, Button, ButtonVariant, IconColor, Label, LabelSize, Notice, NoticeTone,
    Sheet,
};

use crate::pane::PaneId;
use crate::workspace::Workspace;

/// Tell a browsed table's rows what the catalog knows about their columns.
///
/// The wire protocol describes a column as a `uuid` and stops there. Which of
/// them is the primary key, and which one points at another table, are facts
/// about the *relation*, and they are two of the few things the grid draws
/// before anybody reads a value: the key glyph in the header, and — from M6 —
/// the affordance that follows a foreign key to the row it names.
///
/// Matched by name, because that is all a result set has. A column the catalog
/// does not know — an expression, an alias, a joined table's column — is left
/// exactly as the driver described it.
pub(crate) fn describe_columns(rows: &mut db::ResultSet, relation: &db::Relation) {
    let primary: Vec<&str> = relation
        .primary_key()
        .map(|index| index.columns.iter().map(|c| c.as_ref()).collect())
        .unwrap_or_default();
    // Every column that starts a foreign key, not only single-column ones: in
    // a composite key each column is still part of the reference, and marking
    // one of them and not the others would be a lie about the other two.
    let referencing: Vec<&str> = relation
        .foreign_keys
        .iter()
        .flat_map(|key| key.columns.iter().map(|c| c.as_ref()))
        .collect();
    for column in &mut rows.columns {
        let name = column.meta.name.as_str();
        column.meta.is_pk = primary.contains(&name);
        column.meta.is_fk = referencing.contains(&name);
    }
}

impl Workspace {
    /// Work out whether the rows a pane is showing can be written back, and
    /// tell its grid.
    ///
    /// Editability is a property of the result, not of the connection or of
    /// the tab: the same table browsed with `select id, name` is editable and
    /// browsed with `select count(*)` is not. So this is recomputed every time
    /// the grid is handed different rows.
    pub(crate) fn refresh_editability(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let snapshot = self
            .session
            .as_ref()
            .and_then(|session| session.read(cx).snapshot.clone());

        let Some(pane_ref) = self.pane_by(pane) else {
            return;
        };
        let grid = pane_ref.grid.clone();
        // Only a tab that is browsing one table, showing that table's one
        // answer. A script's third result happens to have the same columns as
        // a table often enough to be dangerous, and never means the user asked
        // to edit that table.
        let relation = pane_ref
            .active()
            .and_then(|tab| tab.relation.clone())
            .filter(|_| pane_ref.results.len() <= 1);

        let identity = match (&relation, &snapshot) {
            (Some(reference), Some(snapshot)) => match snapshot.relation(reference) {
                Some(def) => {
                    let data = grid.read(cx).data().clone();
                    let names: Vec<&str> = data.columns.iter().map(|c| &*c.meta.name).collect();
                    sqlgen::resolve(def, &names)
                }
                None => Err(NotEditable::NotATable),
            },
            _ => Err(NotEditable::NotATable),
        };

        // Ask the server what this role may do here, if it has not been asked
        // already. Detached and cached, so browsing a table costs one extra
        // round trip once and never blocks the rows arriving.
        if let (Some(session), Some(reference)) = (self.session.clone(), relation.clone()) {
            session.update(cx, |session, cx| session.load_grants(reference, cx));
        }
        let editable = identity.is_ok()
            && !self.is_read_only(cx)
            && self.write_denied(relation.as_ref(), cx).is_none();
        if let Some(pane_ref) = self.pane_by_mut(pane) {
            pane_ref.editing_relation = relation;
            pane_ref.identity = identity;
        }
        grid.update(cx, |grid, cx| grid.set_editable(editable, cx));
    }

    /// Why the *server* would refuse a write to this relation, if it would.
    ///
    /// A grid that lets an edit be typed and then fails on Commit with
    /// `permission denied for table users` has wasted the work and explained
    /// nothing. The privileges are already loaded for the Privileges tab, so
    /// the same answer can be given before the first keystroke instead — and
    /// it names the role, because "you cannot" is only useful next to "as
    /// whom".
    ///
    /// `None` while the answer is unknown: an engine with no roles, a
    /// connection whose grants have not arrived yet, or a pane that is not
    /// browsing a table. Being optimistic there costs a failed commit at
    /// worst; being pessimistic would lock a grid the user can in fact edit.
    pub(crate) fn write_denied(
        &self,
        relation: Option<&db::RelationRef>,
        cx: &gpui::App,
    ) -> Option<SharedString> {
        let session = self.session.as_ref()?.read(cx);
        let grants = session.grants.get(relation?)?;
        if grants.may_write() {
            return None;
        }
        let who = session
            .roles
            .as_ref()
            .map(|roles| format!("You are connected as {}, which", roles.current))
            .unwrap_or_else(|| "This connection".into());
        Some(
            format!(
                "{who} may read this table but not change it. It is owned by {}.",
                grants.owner
            )
            .into(),
        )
    }

    /// Whether the open connection forbids writes. A read-only connection is
    /// checked again in the session before anything is sent — this one is so
    /// the grid never lets an edit be typed that could not be saved.
    pub(crate) fn is_read_only(&self, cx: &gpui::App) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.read(cx).config.is_read_only())
    }

    /// Why the active pane's grid is read-only, for the banner. `None` when it
    /// is editable, which is also when the banner is not drawn.
    pub(crate) fn not_editable(&self, cx: &gpui::App) -> Option<SharedString> {
        if self.is_read_only(cx) {
            return Some("This connection is read-only.".into());
        }
        // An engine that has no cell editing has no read-only state to explain
        // either. Everything below this line reasons about *why this table*
        // cannot be written to, which on ClickHouse or Redis would be a banner
        // over every tab saying the same thing about the engine — and one that
        // reached for Postgres words to say it, because that is what the
        // reasons below are made of.
        if !self.capabilities(cx).editable_rows {
            return None;
        }
        // The server's answer outranks ours: a table whose rows we know how to
        // address is still not editable if this role was never granted it.
        if let Some(denied) = self.write_denied(self.pane().editing_relation.as_ref(), cx) {
            return Some(denied);
        }
        match &self.pane().identity {
            Ok(Identity::Ctid) => Some(
                "This table has no key, so rows are addressed by physical position. \
                 Saving will fail if anything else changes them first."
                    .into(),
            ),
            Ok(_) => None,
            // A pane that is not browsing a table at all says nothing: every
            // ad-hoc query would carry a banner, which trains people to stop
            // reading it.
            Err(NotEditable::NotATable) => None,
            Err(other) => Some(other.message().into()),
        }
    }

    // ---- the row toolbar -------------------------------------------------

    pub fn add_row(&mut self, cx: &mut Context<Self>) {
        let grid = self.pane().grid.clone();
        grid.update(cx, |grid, cx| grid.add_row(cx));
    }

    pub fn delete_rows(&mut self, cx: &mut Context<Self>) {
        let grid = self.pane().grid.clone();
        grid.update(cx, |grid, cx| grid.delete_rows(cx));
    }

    /// Throw the staged changes away. The one gesture in the editing path that
    /// loses work, which is why it is not on a toolbar button.
    pub fn discard_changes(&mut self, cx: &mut Context<Self>) {
        let grid = self.pane().grid.clone();
        grid.update(cx, |grid, cx| grid.discard_changes(cx));
    }

    pub fn undo_edit(&mut self, cx: &mut Context<Self>) {
        let grid = self.pane().grid.clone();
        grid.update(cx, |grid, cx| grid.undo(cx));
    }

    pub fn redo_edit(&mut self, cx: &mut Context<Self>) {
        let grid = self.pane().grid.clone();
        grid.update(cx, |grid, cx| grid.redo(cx));
    }

    pub fn revert_rows(&mut self, cx: &mut Context<Self>) {
        let grid = self.pane().grid.clone();
        grid.update(cx, |grid, cx| grid.revert_rows(cx));
    }

    // ---- committing ------------------------------------------------------

    /// The statements a commit of the active pane would send.
    ///
    /// Generated rather than remembered: the staged changes are the truth, and
    /// building the SQL from them at the moment it is asked for means the
    /// preview can never disagree with what is sent.
    pub(crate) fn pending_statements(&self, cx: &gpui::App) -> Vec<Statement> {
        let pane = self.pane();
        let (Ok(identity), Some(relation)) = (&pane.identity, &pane.editing_relation) else {
            return Vec::new();
        };
        let grid = pane.grid.read(cx);
        if !grid.has_changes() {
            return Vec::new();
        }
        sqlgen::statements(
            grid.changes(),
            &Target {
                relation,
                identity,
                rows: grid.data(),
                // Every generated `UPDATE` carries the old values of the
                // columns it writes. It costs nothing on the wire and it is
                // the difference between saving what you saw and overwriting
                // whatever is there now.
                concurrency: Concurrency::CheckUnchanged,
            },
        )
    }

    /// Show what Commit would send, before it sends it.
    pub fn preview_commit(&mut self, cx: &mut Context<Self>) {
        let statements = self.pending_statements(cx);
        if statements.is_empty() {
            return;
        }
        let id = self.active_pane;
        if let Some(pane) = self.pane_by_mut(id) {
            pane.preview = Some(statements);
        }
        cx.notify();
    }

    pub fn cancel_preview(&mut self, cx: &mut Context<Self>) {
        let id = self.active_pane;
        if let Some(pane) = self.pane_by_mut(id) {
            pane.preview = None;
        }
        cx.notify();
    }

    /// Send the staged changes as one transaction.
    ///
    /// The staged changes are deliberately *not* cleared here. They are cleared
    /// when the server says it committed — see `Workspace::absorb_apply` —
    /// because a rollback has to leave the user exactly where they were.
    pub fn commit_changes(&mut self, cx: &mut Context<Self>) {
        let statements = self.pending_statements(cx);
        if statements.is_empty() {
            return;
        }
        let counts = self.pane().grid.read(cx).changes().counts();
        let id = self.active_pane;
        if let Some(pane) = self.pane_by_mut(id) {
            pane.preview = None;
        }
        if let Some(session) = self.session.clone() {
            session.update(cx, |session, cx| session.apply(statements, counts, cx));
        }
        cx.notify();
    }
}

impl Workspace {
    /// The confirmation sheet: every statement Commit is about to send, in the
    /// order it will send them.
    ///
    /// Shown before the write rather than after it because this is the last
    /// moment anything can be called off, and because a generated `DELETE` is
    /// exactly the kind of statement people want to read once.
    pub(crate) fn render_commit_preview(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let statements = self.pane().preview.as_ref()?;
        let c = cx.colors().clone();
        let ty = cx.typography().clone();
        let counts = self.pane().grid.read(cx).changes().counts();
        let destructive = counts.deletes > 0;
        let lines: Vec<String> = statements.iter().map(|s| s.preview()).collect();

        Some(
            Sheet::new("commit-preview", "Save changes")
                .subtitle(format!(
                    "{} in one transaction. Nothing is written unless all of it succeeds.",
                    count_parts(&counts)
                ))
                .width(px(620.))
                .child(
                    div()
                        .id("commit-preview-sql")
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
                // A delete is the one thing here that cannot be undone by
                // editing the cell back, so it is said out loud.
                .children(destructive.then(|| {
                    Notice::new(
                        NoticeTone::Danger,
                        format!(
                            "{} will be deleted. This cannot be undone.",
                            plural(counts.deletes, "row")
                        ),
                    )
                }))
                .on_dismiss(cx.listener(|this, _, _, cx| this.cancel_preview(cx)))
                .footer_end(
                    Button::new("commit-cancel", "Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_preview(cx))),
                )
                .footer_end(
                    Button::new("commit-confirm", "Save")
                        .variant(if destructive {
                            ButtonVariant::Danger
                        } else {
                            ButtonVariant::Filled
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.commit_changes(cx))),
                ),
        )
    }
}

/// `2 inserts, 1 delete`. Empty parts are dropped rather than printed as zero:
/// a sentence about zero deletes is a sentence about nothing.
fn count_parts(counts: &sqlgen::Counts) -> String {
    let parts: Vec<String> = [
        (counts.inserts, "new row"),
        (counts.updates, "changed row"),
        (counts.deletes, "deleted row"),
        (counts.ddl, "schema change"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, word)| plural(n, word))
    .collect();
    parts.join(", ")
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}
