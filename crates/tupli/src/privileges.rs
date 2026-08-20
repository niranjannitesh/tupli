//! Who may do what to the table that is open.
//!
//! The tab exists because of a question the grid could not answer. A table you
//! cannot write to looked exactly like a table with no primary key — the row
//! toolbar was simply absent — and the app knew the reason and did not say it.
//! Now it says it, in the one place where the whole answer fits: the owner, the
//! grants, and what the connected role itself is allowed to do.
//!
//! Read on demand rather than with the catalog. Grants are a different answer
//! for every relation and most relations are never asked about, so pulling them
//! for two thousand tables to draw a sidebar would be two thousand answers
//! nobody reads.

use gpui::{div, prelude::*, px, AnyElement, Context, IntoElement, ParentElement, Window};
use ui::{
    h_flex, v_flex, ActiveTheme, EmptyState, Icon, IconColor, IconName, IconSize, Label, LabelSize,
    SectionHeader, StyledExt, Tooltip,
};

use crate::results::{cell, flex_cell, structure_header, structure_row, Slot};
use crate::workspace::{count_of, Workspace};

/// One privilege's column. Wide enough for `REFERENCES`, which is the longest
/// word on the list and the one that decides the width of all eight.
const PRIVILEGE_WIDTH: gpui::Pixels = px(76.);
const GRANTEE_WIDTH: gpui::Pixels = px(164.);

impl Workspace {
    pub(crate) fn render_privileges(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(reference) = self.pane().active().and_then(|tab| tab.relation.clone()) else {
            return EmptyState::new(IconName::Shield, "Nothing to check")
                .description("Open a table or a view from the sidebar to see who may read it.")
                .into_any_element();
        };

        // Asked for here rather than when the tab was opened, because this is
        // the moment somebody is looking: a pane switched to Data and back
        // costs nothing, and a pane nobody ever switches costs nothing at all.
        let grants = self.session.clone().and_then(|session| {
            session.update(cx, |session, cx| {
                session.load_grants(reference.clone(), cx);
                session.grants.get(&reference).cloned()
            })
        });
        let Some(grants) = grants else {
            return EmptyState::new(IconName::Shield, "Reading the privileges…")
                .description(format!(
                    "Asking the server about {}.",
                    reference.qualified()
                ))
                .into_any_element();
        };

        let c = cx.colors().clone();
        let you = self
            .session
            .as_ref()
            .and_then(|session| session.read(cx).roles.clone())
            .map(|roles| roles.current.to_string())
            .unwrap_or_default();
        let grantees = grants.grantees();
        let columns = grants.columns();

        let header: Vec<(&'static str, Slot)> =
            std::iter::once(("Grantee", Slot::Fixed(GRANTEE_WIDTH)))
                .chain(
                    db::Privilege::TABLE
                        .iter()
                        .map(|privilege| (short(privilege), Slot::Fixed(PRIVILEGE_WIDTH))),
                )
                .chain(std::iter::once(("", Slot::Flex(1.))))
                .collect();

        v_flex()
            .id("privileges")
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .child(SectionHeader::new("this connection"))
            .child(
                v_flex()
                    .px(px(8.))
                    .py(px(6.))
                    .gap(px(4.))
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(
                                Icon::new(IconName::User)
                                    .size(IconSize::XSmall)
                                    .color(IconColor::Muted),
                            )
                            .child(Label::new(you).mono().size(LabelSize::Code))
                            .child(
                                Label::new(sentence(&grants))
                                    .size(LabelSize::Small)
                                    .color(IconColor::Muted),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(
                                Icon::new(IconName::Key)
                                    .size(IconSize::XSmall)
                                    .color(IconColor::Subtle),
                            )
                            .child(
                                Label::new("owned by")
                                    .size(LabelSize::Small)
                                    .color(IconColor::Subtle),
                            )
                            .child(
                                Label::new(grants.owner.to_string())
                                    .mono()
                                    .size(LabelSize::Code)
                                    .color(IconColor::Muted),
                            ),
                    ),
            )
            .child(SectionHeader::new(count_of(grantees.len(), "grantee")))
            .child(structure_header(&c, &header))
            .children(grantees.iter().enumerate().map(|(index, grantee)| {
                let held = grants.of(grantee);
                let mut row = structure_row(&c, index).child(
                    cell(GRANTEE_WIDTH)
                        .gap(px(5.))
                        .child(
                            Icon::new(match &**grantee {
                                db::PUBLIC => IconName::Users,
                                _ => IconName::User,
                            })
                            .size(IconSize::XSmall)
                            .color(match &**grantee {
                                // Everyone is the grantee worth noticing: a
                                // `grant select on … to public` is how a table
                                // becomes readable by an account nobody
                                // remembers creating.
                                db::PUBLIC => IconColor::Warning,
                                _ => IconColor::Subtle,
                            }),
                        )
                        .child(Label::new(grantee.to_string()).mono().size(LabelSize::Code)),
                );
                for privilege in db::Privilege::TABLE.iter() {
                    let grant = held.iter().find(|grant| &grant.privilege == privilege);
                    row = row.child(cell(PRIVILEGE_WIDTH).child(tick(grant.copied())));
                }
                row.child(flex_cell(1.)).into_any_element()
            }))
            .when_true(!columns.is_empty(), |el| {
                el.child(SectionHeader::new(count_of(
                    columns.len(),
                    "column privilege",
                )))
                // Column grants are the reason an otherwise readable table
                // hands back "permission denied for column", so they are listed
                // rather than folded into the row above: a grantee with a
                // column grant and no table grant is invisible in that matrix.
                .child(structure_header(
                    &c,
                    &[
                        ("Column", Slot::Fixed(GRANTEE_WIDTH)),
                        ("Grantee", Slot::Fixed(GRANTEE_WIDTH)),
                        ("Privileges", Slot::Flex(1.)),
                    ],
                ))
                .children(
                    columns
                        .iter()
                        .enumerate()
                        .flat_map(|(index, (name, held))| {
                            let c = c.clone();
                            by_grantee(held).into_iter().map(move |(grantee, list)| {
                                structure_row(&c, index)
                                    .child(cell(GRANTEE_WIDTH).child(
                                        Label::new(name.to_string()).mono().size(LabelSize::Code),
                                    ))
                                    .child(
                                        cell(GRANTEE_WIDTH).child(
                                            Label::new(grantee.to_string())
                                                .mono()
                                                .size(LabelSize::Code)
                                                .color(IconColor::Muted),
                                        ),
                                    )
                                    .child(
                                        flex_cell(1.).child(
                                            Label::new(list.join(", "))
                                                .size(LabelSize::Small)
                                                .color(IconColor::Muted),
                                        ),
                                    )
                                    .into_any_element()
                            })
                        }),
                )
            })
            .into_any_element()
    }
}

/// One cell of the matrix.
///
/// Three states and not two: held, held with the right to pass it on, and not
/// held. `with grant option` is the difference between a role that can read a
/// table and a role that can decide who else reads it, which is worth more than
/// a tooltip on a tick that looks like every other tick.
fn tick(grant: Option<&db::Grant>) -> AnyElement {
    match grant {
        None => Label::new("·")
            .size(LabelSize::Small)
            .color(IconColor::Disabled)
            .into_any_element(),
        Some(grant) if grant.grantable => div()
            .id("grantable")
            .child(
                Icon::new(IconName::CircleCheck)
                    .size(IconSize::XSmall)
                    .color(IconColor::Accent),
            )
            .tooltip(Tooltip::text("with grant option"))
            .into_any_element(),
        Some(_) => Icon::new(IconName::Check)
            .size(IconSize::XSmall)
            .color(IconColor::Success)
            .into_any_element(),
    }
}

/// What the connected role may do, as a sentence rather than as eight ticks it
/// would have to find its own row in.
fn sentence(grants: &db::Grants) -> String {
    match grants.mine.as_slice() {
        [] => "may do nothing here".to_string(),
        held => {
            let list: Vec<&str> = held.iter().map(|p| p.keyword()).collect();
            format!("may {}", list.join(", ").to_lowercase())
        }
    }
}

/// The column grants of one column, gathered per grantee, so a role with three
/// of them is one line rather than three.
fn by_grantee<'a>(grants: &[&'a db::Grant]) -> Vec<(&'a str, Vec<&'a str>)> {
    let mut out: Vec<(&str, Vec<&str>)> = Vec::new();
    for grant in grants {
        let keyword = grant.privilege.keyword();
        match out.iter_mut().find(|(name, _)| *name == &*grant.grantee) {
            Some((_, list)) => list.push(keyword),
            None => out.push((&grant.grantee, vec![keyword])),
        }
    }
    out
}

/// The header for one privilege's column.
///
/// Abbreviated because eight full keywords is wider than the pane and the
/// alternative — a horizontal scroll to find out whether anyone may delete — is
/// worse than a word people already read as `REFERENCES`.
fn short(privilege: &db::Privilege) -> &'static str {
    match privilege {
        db::Privilege::Select => "SELECT",
        db::Privilege::Insert => "INSERT",
        db::Privilege::Update => "UPDATE",
        db::Privilege::Delete => "DELETE",
        db::Privilege::Truncate => "TRUNC",
        db::Privilege::References => "REFS",
        db::Privilege::Trigger => "TRIGGER",
        db::Privilege::Maintain => "MAINT",
        db::Privilege::Other(_) => "OTHER",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::{Grant, Grants, Privilege};

    fn grant(grantee: &str, privilege: Privilege) -> Grant {
        Grant {
            grantee: grantee.into(),
            privilege,
            grantable: false,
            column: None,
        }
    }

    #[test]
    fn a_role_with_nothing_is_told_so_rather_than_shown_a_blank() {
        let grants = Grants {
            owner: "app".into(),
            grants: Vec::new(),
            mine: Vec::new(),
        };
        assert_eq!(sentence(&grants), "may do nothing here");
    }

    #[test]
    fn what_you_may_do_reads_as_a_sentence() {
        let grants = Grants {
            owner: "app".into(),
            grants: Vec::new(),
            mine: vec![Privilege::Select, Privilege::Insert],
        };
        assert_eq!(sentence(&grants), "may select, insert");
    }

    #[test]
    fn one_grantees_column_privileges_are_one_line() {
        let mut select = grant("reporting", Privilege::Select);
        select.column = Some("email".into());
        let mut update = grant("reporting", Privilege::Update);
        update.column = Some("email".into());
        let held = vec![&select, &update];
        assert_eq!(
            by_grantee(&held),
            vec![("reporting", vec!["SELECT", "UPDATE"])]
        );
    }

    #[test]
    fn every_privilege_the_matrix_columns_has_a_heading() {
        for privilege in Privilege::TABLE {
            assert!(!short(&privilege).is_empty());
        }
    }
}
