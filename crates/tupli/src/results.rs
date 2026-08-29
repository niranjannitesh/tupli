//! The result tabs that are not the grid.
//!
//! **Structure** answers "what is this thing" without making anyone type
//! `\d users`, and it answers it from the snapshot the sidebar was already
//! built from — no extra round trip, and no chance of the tree and the tab
//! disagreeing about the same table.
//!
//! **DDL** is the same catalog read a third way: the object written out as the
//! `CREATE` statements that would build it, in a read-only console — the same
//! editor as the one above it, so SQL is highlighted the same way wherever it
//! is being read.
//!
//! **Privileges** is the answer to "why is this grid read-only", written out
//! in full: who owns the table, who has been granted what, and — the line
//! everybody actually came for — what the role you are logged in as may do.
//!
//! What used to be a fourth tab here — Messages, the log of what this window
//! had run — is the sidebar's History tab now. Two records of the same thing
//! that disagreed about which statements counted was one more than anybody
//! needed, and the durable one is the one worth keeping.

use gpui::{
    div, prelude::*, px, AnyElement, ClipboardItem, Context, IntoElement, ParentElement,
    SharedString, Window,
};
use ui::{
    h_flex, v_flex, ActiveTheme, Button, ButtonSize, EmptyState, Icon, IconColor, IconName,
    IconSize, Label, LabelSize, SectionHeader, StyledExt, Toolbar,
};

use crate::workspace::{count_of, Workspace};

/// Which of the result dock's tabs is showing.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum ResultsTab {
    #[default]
    Data,
    Structure,
    /// The object as the statements that would recreate it.
    Ddl,
    /// Who may do what to the object, and what you yourself may do.
    Privileges,
}

/// Widths of the structure table's fixed columns. Names and defaults take what
/// is left, because those are the two that are genuinely unbounded.
const NUMBER_WIDTH: gpui::Pixels = px(34.);
const TYPE_WIDTH: gpui::Pixels = px(200.);
const NULL_WIDTH: gpui::Pixels = px(64.);
const EXTRA_WIDTH: gpui::Pixels = px(120.);

impl Workspace {
    // ---- structure -------------------------------------------------------

    /// The active tab's relation, as the last snapshot described it.
    ///
    /// Two ways to miss: the tab is a query rather than a browsed table, or the
    /// snapshot predates the table. Both mean "no structure to show" rather than
    /// an error, so both come back as `None`.
    pub(crate) fn structure_source(&self, cx: &gpui::App) -> Option<db::Relation> {
        let reference = self.pane().active()?.relation.clone()?;
        let session = self.session.as_ref()?;
        let snapshot = session.read(cx).snapshot.as_ref()?;
        snapshot.relation(&reference).cloned()
    }

    /// The version of the catalog the snapshot in hand came from.
    ///
    /// The pointer, not a number the server hands out: two reads of the same
    /// database produce two `Arc`s, and all this has to answer is "is this the
    /// same read I rendered from last frame".
    fn schema_version(&self, cx: &gpui::App) -> usize {
        self.session
            .as_ref()
            .and_then(|session| session.read(cx).snapshot.clone())
            .map(|snapshot| std::sync::Arc::as_ptr(&snapshot) as usize)
            .unwrap_or(0)
    }

    /// The object as the statements that would recreate it.
    ///
    /// Rendered from the same snapshot the tree and the Structure tab read, so
    /// the three can never disagree and none of them costs a round trip. What
    /// the snapshot cannot carry — storage parameters, ownership, grants — is
    /// listed in [`sqlgen::ddl`] rather than guessed at here.
    pub(crate) fn render_ddl(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(relation) = self.structure_source(cx) else {
            return EmptyState::new(IconName::Code, "Nothing to script")
                .description(
                    "Open a table or a view from the sidebar to see the SQL that builds it.",
                )
                .into_any_element();
        };

        // Generating is cheap but not free, and `set_text` moves the cursor and
        // drops the scroll — so it happens when the object or the catalog read
        // behind it changes, and not once per frame.
        let key = (relation.reference.clone(), self.schema_version(cx));
        if self.ddl_source.as_ref() != Some(&key) {
            let ddl = sqlgen::ddl::relation(&relation);
            self.ddl_view
                .update(cx, |editor, cx| editor.set_text(&ddl, cx));
            self.ddl_source = Some(key);
        }
        let name = SharedString::from(relation.reference.to_string());
        let view = self.ddl_view.clone();

        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                Toolbar::new("ddl-toolbar")
                    .transparent()
                    .borderless()
                    .start_child(
                        h_flex()
                            .gap(px(5.))
                            .child(
                                Icon::new(IconName::Table)
                                    .size(IconSize::Small)
                                    .color(IconColor::Muted),
                            )
                            .child(Label::new(name).mono().color(IconColor::Muted)),
                    )
                    // Copy the whole script, which is the thing anybody opens
                    // this tab to do. Selecting part of it is what the editor
                    // underneath is for.
                    .end_child(
                        Button::new("ddl-copy", "Copy")
                            .start_icon(IconName::Copy)
                            .size(ButtonSize::XSmall)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let text = this.ddl_view.read(cx).text();
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                            })),
                    ),
            )
            // The editor owns its own scrolling, so there is no scroll
            // container around it — same as the console.
            .child(div().flex_1().min_h_0().child(view))
            .into_any_element()
    }

    pub(crate) fn render_structure(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(relation) = self.structure_source(cx) else {
            return self.render_result_columns(cx);
        };

        let c = cx.colors().clone();
        let primary: Vec<&str> = relation
            .primary_key()
            .map(|index| index.columns.iter().map(|c| c.as_ref()).collect())
            .unwrap_or_default();

        v_flex()
            .id("structure")
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .child(SectionHeader::new(count_of(
                relation.columns.len(),
                "column",
            )))
            .child(structure_header(
                &c,
                &[
                    ("#", Slot::Fixed(NUMBER_WIDTH)),
                    ("Name", Slot::Flex(1.)),
                    ("Type", Slot::Fixed(TYPE_WIDTH)),
                    ("Null", Slot::Fixed(NULL_WIDTH)),
                    ("Default", Slot::Flex(3.)),
                    ("", Slot::Fixed(EXTRA_WIDTH)),
                ],
            ))
            .children(relation.columns.iter().enumerate().map(|(i, column)| {
                let is_pk = primary.contains(&column.name.as_ref());
                structure_row(&c, i)
                    .child(
                        cell(NUMBER_WIDTH).child(
                            Label::new(format!("{}", column.position))
                                .size(LabelSize::Small)
                                .color(IconColor::Subtle),
                        ),
                    )
                    .child(
                        flex_cell(1.)
                            .gap(px(5.))
                            // The key sits before the name rather than in its
                            // own column: it applies to one or two rows out of
                            // fifty, and a column that is blank forty-eight
                            // times is a column of whitespace.
                            .when_true(is_pk, |el| {
                                el.child(
                                    Icon::new(IconName::Key)
                                        .size(IconSize::XSmall)
                                        .color(IconColor::Warning),
                                )
                            })
                            .child(
                                Label::new(column.name.to_string())
                                    .mono()
                                    .size(LabelSize::Code),
                            ),
                    )
                    .child(
                        cell(TYPE_WIDTH).child(
                            // Verbatim from the server: `character varying(64)`,
                            // not a normalised guess at what it meant.
                            Label::new(column.type_name.to_string())
                                .mono()
                                .size(LabelSize::Code)
                                .color(IconColor::Muted),
                        ),
                    )
                    .child(
                        cell(NULL_WIDTH).child(
                            Label::new(if column.nullable { "yes" } else { "no" })
                                .size(LabelSize::Small)
                                .color(if column.nullable {
                                    IconColor::Subtle
                                } else {
                                    IconColor::Muted
                                }),
                        ),
                    )
                    .child(
                        flex_cell(3.).child(match column.default.as_deref() {
                            Some(default) => Label::new(default.to_string())
                                .mono()
                                .size(LabelSize::Code)
                                .color(IconColor::Subtle),
                            None => Label::new("—")
                                .size(LabelSize::Small)
                                .color(IconColor::Disabled),
                        }),
                    )
                    .child(
                        cell(EXTRA_WIDTH).child(
                            Label::new(extra_note(column))
                                .size(LabelSize::Small)
                                .color(IconColor::Subtle),
                        ),
                    )
                    .into_any_element()
            }))
            .when_true(!relation.indexes.is_empty(), |el| {
                el.child(SectionHeader::new(count_of(
                    relation.indexes.len(),
                    "index",
                )))
                .child(structure_header(
                    &c,
                    &[
                        ("", Slot::Fixed(NUMBER_WIDTH)),
                        ("Name", Slot::Flex(1.)),
                        ("Columns", Slot::Flex(1.)),
                        ("", Slot::Fixed(NULL_WIDTH)),
                        ("Method", Slot::Fixed(TYPE_WIDTH)),
                    ],
                ))
                .children(relation.indexes.iter().enumerate().map(|(i, index)| {
                    structure_row(&c, i)
                        .child(
                            cell(NUMBER_WIDTH).child(
                                Icon::new(if index.is_primary {
                                    IconName::Key
                                } else {
                                    IconName::Hashtag
                                })
                                .size(IconSize::XSmall)
                                .color(if index.is_primary {
                                    IconColor::Warning
                                } else {
                                    IconColor::Subtle
                                }),
                            ),
                        )
                        .child(
                            flex_cell(1.).child(
                                Label::new(index.name.to_string())
                                    .mono()
                                    .size(LabelSize::Code),
                            ),
                        )
                        .child(
                            flex_cell(1.).child(
                                Label::new(index.columns.join(", "))
                                    .mono()
                                    .size(LabelSize::Code)
                                    .color(IconColor::Muted),
                            ),
                        )
                        .child(
                            cell(NULL_WIDTH).child(
                                Label::new(if index.is_unique { "unique" } else { "" })
                                    .size(LabelSize::Small)
                                    .color(IconColor::Subtle),
                            ),
                        )
                        .child(
                            cell(TYPE_WIDTH).child(
                                // A partial index is a different object from the
                                // index of the same columns, so the predicate is
                                // shown rather than summarised as "partial".
                                Label::new(match index.predicate.as_deref() {
                                    Some(predicate) => {
                                        format!("{} where {}", index.method, predicate)
                                    }
                                    None => index.method.to_string(),
                                })
                                .size(LabelSize::Small)
                                .color(IconColor::Subtle),
                            ),
                        )
                        .into_any_element()
                }))
            })
            .when_true(!relation.foreign_keys.is_empty(), |el| {
                el.child(SectionHeader::new(count_of(
                    relation.foreign_keys.len(),
                    "foreign key",
                )))
                .child(structure_header(
                    &c,
                    &[
                        ("", Slot::Fixed(NUMBER_WIDTH)),
                        ("Name", Slot::Flex(1.)),
                        ("References", Slot::Flex(1.)),
                        ("", Slot::Fixed(EXTRA_WIDTH)),
                    ],
                ))
                .children(relation.foreign_keys.iter().enumerate().map(|(i, fk)| {
                    structure_row(&c, i)
                        .child(
                            cell(NUMBER_WIDTH).child(
                                Icon::new(IconName::Link)
                                    .size(IconSize::XSmall)
                                    .color(IconColor::Subtle),
                            ),
                        )
                        .child(
                            flex_cell(1.).child(
                                Label::new(fk.name.to_string()).mono().size(LabelSize::Code),
                            ),
                        )
                        .child(
                            flex_cell(1.).child(
                                // Read as the SQL it came from, so the meaning
                                // does not depend on remembering which side of
                                // a two-column layout is which.
                                Label::new(format!(
                                    "({}) → {}.{} ({})",
                                    fk.columns.join(", "),
                                    fk.target.schema,
                                    fk.target.name,
                                    fk.target_columns.join(", ")
                                ))
                                .mono()
                                .size(LabelSize::Code)
                                .color(IconColor::Muted),
                            ),
                        )
                        .child(
                            cell(EXTRA_WIDTH).child(
                                Label::new(format!("on delete {}", action_word(fk.on_delete)))
                                    .size(LabelSize::Small)
                                    .color(IconColor::Subtle),
                            ),
                        )
                        .into_any_element()
                }))
            })
            .when_true(!relation.checks.is_empty(), |el| {
                el.child(SectionHeader::new(count_of(relation.checks.len(), "check")))
                    .child(structure_header(
                        &c,
                        &[
                            ("", Slot::Fixed(NUMBER_WIDTH)),
                            ("Name", Slot::Flex(1.)),
                            ("Expression", Slot::Flex(3.)),
                        ],
                    ))
                    .children(relation.checks.iter().enumerate().map(|(i, check)| {
                        structure_row(&c, i)
                            .child(
                                cell(NUMBER_WIDTH).child(
                                    Icon::new(IconName::Shield)
                                        .size(IconSize::XSmall)
                                        .color(IconColor::Subtle),
                                ),
                            )
                            .child(
                                flex_cell(1.).child(
                                    Label::new(check.name.to_string())
                                        .mono()
                                        .size(LabelSize::Code),
                                ),
                            )
                            .child(
                                flex_cell(3.).child(
                                    // The server's own printing of the expression,
                                    // parentheses and casts included: what it shows
                                    // is what it will accept back.
                                    Label::new(check.definition.to_string())
                                        .mono()
                                        .size(LabelSize::Code)
                                        .color(IconColor::Muted),
                                ),
                            )
                            .into_any_element()
                    }))
            })
            .when_true(!relation.triggers.is_empty(), |el| {
                el.child(SectionHeader::new(count_of(
                    relation.triggers.len(),
                    "trigger",
                )))
                .child(structure_header(
                    &c,
                    &[
                        ("", Slot::Fixed(NUMBER_WIDTH)),
                        ("Name", Slot::Flex(1.)),
                        ("When", Slot::Flex(2.)),
                        ("", Slot::Fixed(EXTRA_WIDTH)),
                    ],
                ))
                .children(relation.triggers.iter().enumerate().map(|(i, trigger)| {
                    structure_row(&c, i)
                        .child(cell(NUMBER_WIDTH).child(
                            Icon::new(IconName::Bolt).size(IconSize::XSmall).color(
                                if trigger.enabled {
                                    IconColor::Subtle
                                } else {
                                    IconColor::Disabled
                                },
                            ),
                        ))
                        .child(
                            flex_cell(1.).child(
                                Label::new(trigger.name.to_string())
                                    .mono()
                                    .size(LabelSize::Code)
                                    .color(if trigger.enabled {
                                        IconColor::Default
                                    } else {
                                        IconColor::Disabled
                                    }),
                            ),
                        )
                        .child(
                            flex_cell(2.).child(
                                Label::new(sqlgen::ddl::trigger_summary(trigger))
                                    .mono()
                                    .size(LabelSize::Code)
                                    .color(IconColor::Muted),
                            ),
                        )
                        .child(
                            cell(EXTRA_WIDTH).child(
                                // A disabled trigger is the answer to "why did
                                // nothing happen", so it is stated rather than
                                // left to the grey.
                                Label::new(if trigger.enabled { "" } else { "disabled" })
                                    .size(LabelSize::Small)
                                    .color(IconColor::Warning),
                            ),
                        )
                        .into_any_element()
                }))
            })
            .into_any_element()
    }

    /// The fallback structure view: what the *result set* is made of.
    ///
    /// An ad-hoc `select` has no relation behind it — its columns can be
    /// expressions over four joined tables — but it does have a shape, and the
    /// shape is exactly what someone opening Structure wants to see.
    fn render_result_columns(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = cx.colors().clone();
        let data = self.pane().grid.read(cx).data().clone();
        if data.columns.is_empty() {
            return EmptyState::new(IconName::Columns, "Nothing to describe")
                .description("Open a table from the sidebar, or run a statement that returns rows.")
                .into_any_element();
        }

        v_flex()
            .id("structure-result")
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .child(SectionHeader::new(count_of(
                data.columns.len(),
                "result column",
            )))
            .child(structure_header(
                &c,
                &[
                    ("#", Slot::Fixed(NUMBER_WIDTH)),
                    ("Name", Slot::Flex(1.)),
                    ("Type", Slot::Flex(1.)),
                ],
            ))
            .children(data.columns.iter().enumerate().map(|(i, column)| {
                structure_row(&c, i)
                    .child(
                        cell(NUMBER_WIDTH).child(
                            Label::new(format!("{}", i + 1))
                                .size(LabelSize::Small)
                                .color(IconColor::Subtle),
                        ),
                    )
                    .child(
                        flex_cell(1.).child(
                            Label::new(column.meta.name.to_string())
                                .mono()
                                .size(LabelSize::Code),
                        ),
                    )
                    .child(
                        flex_cell(1.).child(
                            Label::new(column.meta.type_name.to_string())
                                .mono()
                                .size(LabelSize::Code)
                                .color(IconColor::Muted),
                        ),
                    )
                    .into_any_element()
            }))
            .into_any_element()
    }
}

// ---- pieces --------------------------------------------------------------

/// A fixed-width cell that clips rather than pushing its neighbours around.
pub(crate) fn cell(width: gpui::Pixels) -> gpui::Div {
    h_flex().w(width).flex_none().overflow_hidden()
}

/// A header whose slots are the same widths, in the same order, as the rows
/// underneath it. Passed explicitly rather than derived, because the three
/// sections genuinely have different shapes and a shared "six columns" header
/// silently mislabels two of them.
/// How wide one slot of a structure table is: a fixed number of pixels, or a
/// share of what is left over. Two flexible columns with different weights is
/// the whole reason this is not just `Option<Pixels>` — a `default` of
/// `ARRAY['developer'::character varying]` next to a name of `role` wants three
/// quarters of the free space, not half of it.
#[derive(Copy, Clone)]
pub(crate) enum Slot {
    Fixed(gpui::Pixels),
    Flex(f32),
}

/// A flexible slot: `flex-basis: 0` so the weights decide the whole width
/// rather than only the space left after the text, and `min-width: 0` so a long
/// value elides instead of pushing its neighbours off the edge.
pub(crate) fn flex_cell(weight: f32) -> gpui::Div {
    h_flex()
        .flex_grow(weight)
        .flex_shrink(1.)
        .flex_basis(px(0.))
        .min_w_0()
}

pub(crate) fn structure_header(c: &ui::ThemeColors, columns: &[(&'static str, Slot)]) -> gpui::Div {
    let mut row = h_flex()
        .w_full()
        .flex_none()
        .h(px(22.))
        .px(px(8.))
        .gap(px(8.))
        .bg(c.chrome)
        .border_b_1()
        .border_color(c.seam);
    for (title, width) in columns {
        let label = Label::new(*title)
            .size(LabelSize::Small)
            .color(IconColor::Subtle);
        row = match width {
            Slot::Fixed(width) => row.child(cell(*width).child(label)),
            Slot::Flex(weight) => row.child(flex_cell(*weight).child(label)),
        };
    }
    row
}

pub(crate) fn structure_row(c: &ui::ThemeColors, index: usize) -> gpui::Div {
    h_flex()
        .w_full()
        .flex_none()
        .h(px(24.))
        .px(px(8.))
        .gap(px(8.))
        // Striping, like the grid: forty rows of identical type names are hard
        // to track across without one.
        .when_true(index % 2 == 1, |el| el.bg(c.grid_stripe))
}

/// The one-word summary of everything a column is besides its type.
fn extra_note(column: &db::ColumnDef) -> &'static str {
    if column.is_identity() {
        "identity"
    } else if column.is_generated {
        "generated"
    } else {
        ""
    }
}

fn action_word(action: db::RefAction) -> &'static str {
    match action {
        db::RefAction::NoAction => "no action",
        db::RefAction::Restrict => "restrict",
        db::RefAction::Cascade => "cascade",
        db::RefAction::SetNull => "set null",
        db::RefAction::SetDefault => "set default",
    }
}

/// A statement collapsed onto the single line the log has room for. Runs of
/// whitespace become one space, so an indented multi-line query still reads.
pub(crate) fn one_line(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut spaced = true;
    for ch in sql.chars() {
        if ch.is_whitespace() {
            if !spaced {
                out.push(' ');
                spaced = true;
            }
        } else {
            out.push(ch);
            spaced = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::one_line;

    #[test]
    fn a_multi_line_statement_becomes_one_readable_line() {
        assert_eq!(
            one_line("select *\n  from users\n  where id = 1\n"),
            "select * from users where id = 1"
        );
    }

    #[test]
    fn nothing_but_whitespace_collapses_to_nothing() {
        assert_eq!(one_line("   \n\t "), "");
    }
}
