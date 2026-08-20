//! The right panel: what the selected row holds, and what its table is.
//!
//! A grid cell can hold a JSON document, a 40KB text blob or a timestamp with a
//! zone, and none of those fit in a 24px row. The panel is the expanded form of
//! the row the grid has selected — every field, with the long ones openable in
//! place — and, beside it, what the catalog says about the table the rows came
//! from. The object's DDL is not here: it is a document, not a detail, and it
//! lives in the result dock where there is room to read it without re-wrapping
//! every line.

use gpui::{
    div, prelude::*, px, AnyElement, ClipboardItem, Context, HighlightStyle, IntoElement,
    ParentElement, StyledText, Window,
};
use ui::{
    h_flex, region, v_flex, ActiveTheme, Button, ButtonSize, Icon, IconColor, IconName, IconSize,
    Label, LabelSize, SectionHeader, Tab, TabBar, Tooltip,
};

use crate::workspace::{cell_text, InspectorTab, Workspace};

impl Workspace {
    pub(crate) fn render_inspector(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tab = self.inspector_tab;

        region(cx)
            .border_l_1()
            .border_color(cx.colors().border)
            .size_full()
            .child(
                TabBar::new("inspector-tabs")
                    .tab(
                        Tab::new("inspector-row", "Row")
                            .icon(IconName::BulletList)
                            .active(tab == InspectorTab::Row)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.inspector_tab = InspectorTab::Row;
                                cx.notify();
                            })),
                    )
                    .tab(
                        Tab::new("inspector-table", "Table")
                            .icon(IconName::Table)
                            .active(tab == InspectorTab::Table)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.inspector_tab = InspectorTab::Table;
                                cx.notify();
                            })),
                    ),
            )
            .child(match tab {
                InspectorTab::Row => self.render_row_inspector(cx).into_any_element(),
                InspectorTab::Table => self.render_table_inspector(cx).into_any_element(),
            })
    }

    fn render_row_inspector(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = cx.colors().clone();
        let ty = cx.typography().clone();
        let syntax = cx.syntax().clone();
        let data = self.result(cx);
        if data.row_count() == 0 {
            return nothing_selected("No row selected.").into_any_element();
        }
        let total = data.row_count();
        let row = self
            .pane()
            .selected_row
            .unwrap_or(0)
            .min(total.saturating_sub(1));

        // Everything the fields need, gathered before the first listener is
        // built: the grid and the catalog are read through the same borrow the
        // closures below want mutably.
        let grid = self.pane().grid.read(cx);
        let editable = grid.is_editable();
        // What the grid would show, and — where a cell has been edited but not
        // yet committed — what it would show *now*: a panel that kept insisting
        // on the fetched value would be describing a row nobody can see.
        let values: Vec<Option<String>> = data
            .columns
            .iter()
            .enumerate()
            .map(|(ix, col)| {
                grid.cell_value(row, ix)
                    .map(|staged| match staged {
                        db::Value::Null => None,
                        other => Some(other.to_string()),
                    })
                    .unwrap_or_else(|| cell_text(col, row))
            })
            .collect();
        let here = self
            .pane()
            .active()
            .and_then(|tab| tab.relation.as_ref())
            .map(|relation| relation.schema.clone());
        let follows: Vec<Option<db::RelationRef>> = (0..data.column_count())
            .map(|ix| self.reference_target(ix, cx))
            .collect();

        v_flex()
            .id("row-inspector")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .py(px(6.))
            .child(SectionHeader::new(format!("row {} of {total}", row + 1)))
            .children(data.columns.iter().enumerate().map(|(ix, col)| {
                let meta = &col.meta;
                let value = values[ix].clone();
                let copied = value.clone();
                let expanded = self.expanded_field == Some(ix);
                let more = value.as_deref().and_then(overflows);
                // `jsonb` arrives as a single line however long it is. Laid
                // out, it is the reason this panel exists; the clipboard still
                // gets the line, which is what the column actually holds.
                let shown = match expanded {
                    true => value
                        .as_deref()
                        .and_then(crate::json::pretty)
                        .or_else(|| value.clone()),
                    false => value.clone(),
                };
                let follow = follows[ix].clone();
                // Label above value rather than beside it: column names and
                // values are both unbounded, and a two-column layout in a 260px
                // panel truncates one of them on every row.
                v_flex()
                    // Clicking a field opens it where it is. It used to hand
                    // the panel to a tab that showed one cell; there is no such
                    // tab now, because a cell was never the thing being looked
                    // at — a field of the row was.
                    .id(("row-field", ix))
                    .group("row-field")
                    .cursor_pointer()
                    .hover(|s| s.bg(c.hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.expanded_field = match this.expanded_field == Some(ix) {
                            true => None,
                            false => Some(ix),
                        };
                        cx.notify();
                    }))
                    .px(px(10.))
                    .py(px(5.))
                    .gap(px(2.))
                    .border_b_1()
                    .border_color(c.border)
                    .child(
                        h_flex()
                            .gap(px(5.))
                            .when(meta.is_pk, |el| {
                                el.child(
                                    Icon::new(IconName::Key)
                                        .size(IconSize::XSmall)
                                        .color(IconColor::Accent)
                                        .flat(),
                                )
                            })
                            .child(
                                Label::new(meta.name.clone())
                                    .mono()
                                    .size(LabelSize::Small)
                                    .color(IconColor::Muted)
                                    .flex_1()
                                    .min_w_0(),
                            )
                            .child(
                                Label::new(meta.type_name.clone())
                                    .size(LabelSize::Small)
                                    .color(IconColor::Disabled),
                            )
                            // The way back up, at the top. An expanded 10KB
                            // document puts its own end a screen and a half
                            // below the fold, and a collapse control down there
                            // is a control you have to scroll past the thing
                            // you wanted to hide in order to reach.
                            .when(expanded, |el| {
                                el.child(
                                    Button::new(("row-field-collapse", ix), "collapse")
                                        .size(ButtonSize::XSmall)
                                        // A step brighter than the type beside
                                        // it, which is the difference between a
                                        // fact about the field and a thing to
                                        // press.
                                        .content_color(IconColor::Muted)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.expanded_field = None;
                                            cx.notify();
                                        })),
                                )
                            })
                            // What can be done to one field, on the field.
                            // Only on hover: three buttons on every one of
                            // fifteen fields would be forty-five things to
                            // look at and nothing to read.
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap(px(2.))
                                    .invisible()
                                    .group_hover("row-field", |s| s.visible())
                                    // Where the key points. An icon with the
                                    // table's name in its tooltip rather than a
                                    // button wearing the name: the buttons are
                                    // laid out whether they are shown or not,
                                    // and one field-wide word of a button would
                                    // push that field's type label out of line
                                    // with every other field's.
                                    .child(match follow {
                                        Some(target) => {
                                            // Qualified only when the hop
                                            // crosses schemas, which is the
                                            // only time the schema is news.
                                            let name = match here.as_deref()
                                                == Some(target.schema.as_ref())
                                            {
                                                true => target.name.to_string(),
                                                false => {
                                                    format!("{}.{}", target.schema, target.name)
                                                }
                                            };
                                            Button::icon(
                                                ("row-field-follow", ix),
                                                IconName::ArrowUpRight,
                                            )
                                            .size(ButtonSize::XSmall)
                                            .tooltip(Tooltip::text(format!("Open {name}")))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.follow_reference_in(ix, cx);
                                            }))
                                            .into_any_element()
                                        }
                                        // The gap a key would have left, so that
                                        // a field with one and a field without
                                        // agree about where the row ends.
                                        None => div().size(px(20.)).into_any_element(),
                                    })
                                    .when(editable, |el| {
                                        el.child(
                                            Button::icon(("row-field-null", ix), IconName::Ban)
                                                .size(ButtonSize::XSmall)
                                                .tooltip(Tooltip::text("Set NULL"))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    let grid = this.pane().grid.clone();
                                                    grid.update(cx, |grid, cx| {
                                                        grid.null_cell(row, ix, cx)
                                                    });
                                                    cx.notify();
                                                })),
                                        )
                                    })
                                    .child(
                                        Button::icon(("row-field-copy", ix), IconName::Copy)
                                            .size(ButtonSize::XSmall)
                                            .tooltip(Tooltip::text("Copy Value"))
                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                // The click is the button's,
                                                // not the row's: copying a
                                                // field should not also open
                                                // it. NULL copies as nothing
                                                // rather than as the word:
                                                // pasting `NULL` into a form is
                                                // a bug waiting to be typed.
                                                cx.stop_propagation();
                                                let text = copied.clone().unwrap_or_default();
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    text,
                                                ));
                                            })),
                                    ),
                            ),
                    )
                    .child(match shown {
                        // Four lines of it. One line meant every JSON document
                        // in the row read as `[{"type": "thinking", "t…`, which
                        // is four columns of punctuation and no information;
                        // the whole of a long one would push the next field a
                        // screen down. Four lines is enough to tell two
                        // documents apart, and one click opens the rest.
                        Some(v) => {
                            v_flex()
                                .gap(px(1.))
                                .child(
                                    div()
                                        .when(!expanded, |el| {
                                            el.max_h(ty.mono_line_height * 4.).overflow_hidden()
                                        })
                                        .child(json_document(&v, &syntax, &ty).unwrap_or_else(
                                            || {
                                                Label::new(v)
                                                    .mono()
                                                    .size(LabelSize::Code)
                                                    .wrap()
                                                    .into_any_element()
                                            },
                                        )),
                                )
                                // How much of it is below the cut, and the way
                                // to lift it. Without the note the clip is
                                // silent and a 4KB value looks like a
                                // 200-character one; as a control it is the
                                // same fact plus a way to act on it.
                                .children(more.filter(|_| !expanded).map(|size| {
                                    Button::new(("row-field-expand", ix), format!("⋯ {size}"))
                                        .size(ButtonSize::XSmall)
                                        .content_color(IconColor::Disabled)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.expanded_field = Some(ix);
                                            cx.notify();
                                        }))
                                }))
                                .into_any_element()
                        }
                        None => Label::new("NULL")
                            .size(LabelSize::Small)
                            .color(IconColor::Disabled)
                            .into_any_element(),
                    })
            }))
            .into_any_element()
    }

    /// What the catalog knows about the table the rows came from.
    ///
    /// The counts, not the contents: the structure tab already lists every
    /// index and every constraint, with the width to do it in. This is the
    /// answer to "what am I looking at" — how big it is, whether it can be
    /// written to, and what it is keyed by — beside the row it is about.
    fn render_table_inspector(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = cx.colors().clone();
        let ty = cx.typography().clone();
        let Some(relation) = self.structure_source(cx) else {
            // A statement's answers have columns but no table: the join that
            // produced them is not an object the catalog has anything to say
            // about, and inventing a name for it would be worse than silence.
            return nothing_selected("These rows came from a statement, not a table.")
                .into_any_element();
        };
        let kind = match relation.kind {
            db::RelationKind::Table => "table",
            db::RelationKind::View => "view",
            db::RelationKind::MaterializedView => "materialized view",
            db::RelationKind::Foreign => "foreign table",
            db::RelationKind::Partitioned => "partitioned table",
        };
        let key = relation.primary_key().map(|key| key.columns.join(", "));
        // Why the grid is or is not writable, said once, where the question
        // comes up. A view is refused outright; a table without a key the app
        // can trust is refused for a reason nothing else on screen states.
        let writable = match (relation.kind.is_editable(), relation.row_identity()) {
            (false, _) => "no — not a table".to_string(),
            (true, None) => "no — no primary or unique key".to_string(),
            (true, Some(identity)) => format!("yes, by {}", identity.columns.join(", ")),
        };

        v_flex()
            .id("table-inspector")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .py(px(6.))
            .child(
                v_flex()
                    .px(px(10.))
                    .py(px(4.))
                    .gap(px(2.))
                    .child(
                        Label::new(relation.reference.name.to_string())
                            .mono()
                            .medium(),
                    )
                    .child(
                        Label::new(format!("{} in {}", kind, relation.reference.schema))
                            .size(LabelSize::Small)
                            .color(IconColor::Subtle),
                    ),
            )
            // Whatever somebody wrote about the table, first, because it is the
            // only line here that is not derivable from the others.
            .children(relation.comment.as_ref().map(|comment| {
                div().px(px(10.)).py(px(4.)).child(
                    Label::new(comment.to_string())
                        .size(LabelSize::Small)
                        .color(IconColor::Muted)
                        .wrap(),
                )
            }))
            .child(div().pt(px(8.)).child(SectionHeader::new("size")))
            .child(fact("Rows", row_estimate(relation.estimated_rows), cx))
            .child(fact(
                "On disk",
                byte_size(relation.size_bytes.max(0) as usize),
                cx,
            ))
            .child(fact("Columns", relation.columns.len().to_string(), cx))
            .child(div().pt(px(8.)).child(SectionHeader::new("keys")))
            .child(fact(
                "Primary key",
                key.unwrap_or_else(|| "none".to_string()),
                cx,
            ))
            .child(fact("Editable", writable, cx))
            .child(div().pt(px(8.)).child(SectionHeader::new("objects")))
            .child(fact("Indexes", relation.indexes.len().to_string(), cx))
            .child(fact(
                "Foreign keys",
                relation.foreign_keys.len().to_string(),
                cx,
            ))
            .child(fact("Checks", relation.checks.len().to_string(), cx))
            .child(fact("Triggers", relation.triggers.len().to_string(), cx))
            // A view is its query. Nothing else about it says what it shows,
            // and the rows on screen are the only other clue.
            .children(relation.definition.as_ref().map(|sql| {
                v_flex()
                    .child(div().pt(px(8.)).child(SectionHeader::new("definition")))
                    .child(
                        div()
                            .px(px(10.))
                            .py(px(4.))
                            .border_t_1()
                            .border_color(c.border)
                            .font(ty.mono_font())
                            .child(
                                Label::new(sql.trim().to_string())
                                    .mono()
                                    .size(LabelSize::Code)
                                    .wrap(),
                            ),
                    )
            }))
            .into_any_element()
    }
}

/// One `label   value` line of the table tab.
///
/// The label column is fixed so the values line up down the panel; the value
/// wraps rather than eliding, because every one of them is short except the
/// key list, and a key list cut off in the middle names the wrong key.
fn fact(label: &'static str, value: String, cx: &mut Context<Workspace>) -> impl IntoElement {
    h_flex()
        .px(px(10.))
        .py(px(3.))
        .gap(px(8.))
        .items_start()
        .border_t_1()
        .border_color(cx.colors().border)
        .child(
            Label::new(label)
                .size(LabelSize::Small)
                .color(IconColor::Subtle)
                .w(px(84.))
                .flex_none(),
        )
        .child(
            Label::new(value)
                .size(LabelSize::Small)
                .wrap()
                .flex_1()
                .min_w_0(),
        )
}

/// `~12k`, or `—` for a table Postgres has never counted.
///
/// Postgres 14 and later store `reltuples = -1` for a relation that has never
/// been vacuumed or analysed, which is the normal state of a freshly restored
/// database. Printing that as a row count would be a lie with a minus sign on
/// it.
fn row_estimate(rows: i64) -> String {
    match rows {
        ..=-1 => "—".into(),
        0 => "0".into(),
        n if n < 1_000 => format!("~{n}"),
        n if n < 1_000_000 => format!("~{:.0}k", n as f64 / 1_000.),
        n if n < 1_000_000_000 => format!("~{:.1}M", n as f64 / 1_000_000.),
        n => format!("~{:.1}B", n as f64 / 1_000_000_000.),
    }
}

/// The size of a value that will not fit in the four lines a field gets, or
/// `None` for one that will.
///
/// Estimated from the text rather than measured from the layout, which is a
/// guess at the panel's width and wrong at the edges — a value that just fits
/// may still be labelled. It is a note about size, though, not a promise about
/// what is hidden, so the edge case is a redundant fact rather than a false one.
fn overflows(text: &str) -> Option<String> {
    let long = text.len() > 150 || text.lines().count() > 4;
    long.then(|| byte_size(text.len()))
}

impl Workspace {
    /// Open one of the row inspector's fields, the way clicking it would.
    pub fn expand_field(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.expanded_field = Some(ix);
        cx.notify();
    }
}

/// A field's value coloured as JSON, or `None` when it is not a document.
///
/// The panel is the only place this happens. In the grid a cell is one line of
/// a table and its colour already means something — staged, deleted, null — and
/// a second colour system running through the same 24px row would be reading
/// two things at once. Here there is one value, laid out, with the room to be
/// read: keys apart from their values is most of what reading an object is.
///
/// Built from byte ranges over the text as shown, so the collapsed single line
/// and the expanded document are coloured by the same code.
fn json_document(text: &str, syntax: &ui::SyntaxTheme, ty: &ui::Typography) -> Option<AnyElement> {
    let spans = crate::json::spans(text)?;
    let highlights = spans.into_iter().map(|(range, token)| {
        let color = match token {
            crate::json::Token::Key => syntax.identifier,
            crate::json::Token::String => syntax.string,
            crate::json::Token::Number => syntax.number,
            crate::json::Token::Literal => syntax.keyword,
            crate::json::Token::Punctuation => syntax.punctuation,
        };
        (
            range,
            HighlightStyle {
                color: Some(color),
                ..Default::default()
            },
        )
    });
    Some(
        // `StyledText` paints in the ambient style, so the face is set here —
        // the same lookup `Label::mono().size(Code)` would have made.
        div()
            .font(ty.mono_font())
            .text_size(ty.mono_size)
            .line_height(ty.mono_line_height)
            .child(StyledText::new(text.to_string()).with_highlights(highlights))
            .into_any_element(),
    )
}

/// `4.2 KB`, in whichever unit keeps it to a couple of digits.
fn byte_size(len: usize) -> String {
    match len {
        len if len < 1024 => format!("{len} B"),
        len if len < 1024 * 1024 => format!("{:.1} KB", len as f32 / 1024.),
        len => format!("{:.1} MB", len as f32 / (1024. * 1024.)),
    }
}

/// What the panel says when there is nothing to show: a pane that has not run
/// anything yet, or a result with no rows in it. A sentence rather than an empty
/// card, because an empty card reads as something that failed to load.
fn nothing_selected(text: &'static str) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .p(px(16.))
        .child(
            Label::new(text)
                .size(LabelSize::Small)
                .color(IconColor::Subtle)
                .wrap(),
        )
}
