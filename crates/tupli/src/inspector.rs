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
    div, prelude::*, px, AnyElement, ClickEvent, ClipboardItem, Context, HighlightStyle,
    IntoElement, ParentElement, Pixels, Point, StyledText, Window,
};
use ui::{
    h_flex, region, v_flex, ActiveTheme, Button, ButtonSize, Icon, IconColor, IconName, IconSize,
    Label, LabelSize, SectionHeader, Tab, TabBar, Tooltip,
};

use db::value::byte_size;

use crate::workspace::{cell_text, InspectorTab, Workspace};

/// The decoder menu, while it is up: where it was asked for, which column it
/// will set, and what is running there now so the ticks say something.
pub(crate) struct DecoderMenu {
    pub at: Point<Pixels>,
    pub column: String,
    pub chain: Vec<db::Decoder>,
}

/// One field's bytes, read.
struct FieldDecode {
    /// What ran — or, when nothing did, what was asked for. The menu ticks and
    /// the chip's label are both about this rather than about the override,
    /// because most of the time nobody chose and [`db::sniff`] did.
    chain: Vec<db::Decoder>,
    result: Result<db::Decoded, db::DecodeError>,
}

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
        let untyped = !self.capabilities(cx).is_sql();
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
        // Values that arrived with no type attached — every column of a Redis
        // reply, and a `bytea` anywhere — get read by the decoders. Everything
        // else came with a type the server declared, and sniffing at that would
        // be guessing at an answer already given.
        let decodings: Vec<Option<FieldDecode>> = data
            .columns
            .iter()
            .enumerate()
            .map(|(ix, col)| {
                if !untyped && col.meta.kind != db::ValueKind::Bytes {
                    return None;
                }
                let value = grid.cell_value(row, ix).unwrap_or_else(|| col.value(row));
                let bytes = field_bytes(&value)?;
                // The override is read inside the gate rather than outside it,
                // so a chain chosen for a keyspace's `value` column cannot
                // follow the name onto a `value` column of a table, where
                // there is no chip to undo it with.
                let asked = self
                    .field_decoders
                    .get(col.meta.name.as_str())
                    .cloned()
                    .unwrap_or_default();
                let result = db::decode(&bytes, &asked);
                let chain = match (&result, asked.is_empty()) {
                    (Ok(decoded), _) => decoded.applied.clone(),
                    (Err(error), true) => vec![error.decoder],
                    (Err(_), false) => asked,
                };
                Some(FieldDecode { chain, result })
            })
            .collect();
        // `TUPLI_DECODER` puts the chain menu up on a named column. It has to
        // wait until here because the ticks are about what actually ran, and
        // nothing knows that until the bytes have been read.
        if let Some(column) = self.pending_decoder.clone() {
            let found = data
                .columns
                .iter()
                .position(|col| col.meta.name == column)
                .and_then(|ix| decodings[ix].as_ref());
            if let Some(read) = found {
                self.pending_decoder = None;
                self.decoder_menu = Some(DecoderMenu {
                    at: gpui::point(px(1150.), px(108.)),
                    column,
                    chain: read.chain.clone(),
                });
            }
        }
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
                let decoded = decodings[ix].as_ref();
                let expanded = self.expanded_field == Some(ix);
                let failed = matches!(decoded, Some(read) if read.result.is_err());
                let hex = matches!(
                    decoded,
                    Some(FieldDecode { result: Ok(read), .. }) if read.form == db::Form::Hex
                );
                // `jsonb` arrives as a single line however long it is. Laid
                // out, it is the reason this panel exists; the clipboard still
                // gets the line, which is what the column actually holds.
                //
                // A decoded field skips that: the line a gzip blob "actually
                // holds" is not text and copying it would put a screenful of
                // mojibake on the clipboard, so what was read is what is
                // copied.
                let shown = match decoded {
                    Some(read) => match &read.result {
                        Ok(read) => Some(read.text.clone()),
                        Err(error) => Some(error.to_string()),
                    },
                    None => match expanded {
                        true => values[ix]
                            .as_deref()
                            .and_then(crate::json::pretty)
                            .or_else(|| values[ix].clone()),
                        false => values[ix].clone(),
                    },
                };
                let copied = match (decoded, failed) {
                    (Some(_), false) => shown.clone(),
                    _ => values[ix].clone(),
                };
                let more = shown.as_deref().and_then(overflows);
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
                            // What the bytes turned out to be, where the
                            // column's own type name would go. `string` is
                            // what Redis says about every value it has ever
                            // held; `gzip → MessagePack` is the fact, and it
                            // is a control because a sniffer is allowed to be
                            // wrong and being wrong should cost one click.
                            .child(match decoded {
                                Some(read) => {
                                    let column = meta.name.clone();
                                    let chain = read.chain.clone();
                                    Button::new(("row-field-decoder", ix), chain_label(&read.chain))
                                        .size(ButtonSize::XSmall)
                                        .end_icon(IconName::ChevronDown)
                                        .content_color(match failed {
                                            true => IconColor::Danger,
                                            false => IconColor::Disabled,
                                        })
                                        .tooltip(Tooltip::text("Read these bytes as…"))
                                        .on_click(cx.listener(
                                            move |this, event: &ClickEvent, _, cx| {
                                                cx.stop_propagation();
                                                this.open_decoder_menu(
                                                    column.clone(),
                                                    chain.clone(),
                                                    event.position(),
                                                    cx,
                                                );
                                            },
                                        ))
                                        .into_any_element()
                                }
                                None => Label::new(meta.type_name.clone())
                                    .size(LabelSize::Small)
                                    .color(IconColor::Disabled)
                                    .into_any_element(),
                            })
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
                                        .id(("row-field-value", ix))
                                        .when(!expanded, |el| {
                                            el.max_h(ty.mono_line_height * 4.).overflow_hidden()
                                        })
                                        // A hex dump is sixteen bytes wide by
                                        // convention and by usefulness, and no
                                        // panel this narrow fits that. Sliding
                                        // it sideways keeps the columns lined
                                        // up; wrapping it would make an
                                        // eight-byte gutter of nonsense.
                                        .when(hex, |el| el.overflow_x_scroll())
                                        .child(match (failed, hex) {
                                            // Why nothing could be read, in
                                            // place of the value: a chain that
                                            // did not run has no text to show
                                            // and the reason is the news.
                                            (true, _) => Label::new(v)
                                                .mono()
                                                .size(LabelSize::Code)
                                                .color(IconColor::Danger)
                                                .wrap()
                                                .into_any_element(),
                                            (false, true) => hex_block(&v, &ty),
                                            (false, false) => json_document(&v, &syntax, &ty)
                                                .unwrap_or_else(|| {
                                                    Label::new(v)
                                                        .mono()
                                                        .size(LabelSize::Code)
                                                        .wrap()
                                                        .into_any_element()
                                                }),
                                        }),
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

/// The bytes behind one value, for the decoders to work on.
///
/// Text hands back its own UTF-8 rather than being refused, because a Redis
/// value that happens to be valid UTF-8 is still bytes somebody may have
/// base64'd a document into. Numbers and booleans have none: a decoder chain
/// over `42` would be answering a question nobody asked.
fn field_bytes(value: &db::Value) -> Option<Vec<u8>> {
    match value {
        db::Value::Bytes(bytes) => Some(bytes.to_vec()),
        db::Value::Text { text, .. } => Some(text.as_bytes().to_vec()),
        db::Value::Null | db::Value::Bool(_) | db::Value::Int(_) | db::Value::Float(_) => None,
    }
}

/// `gzip → MessagePack`. Every step named, because the useful half of a wrong
/// guess is knowing which step to change.
fn chain_label(chain: &[db::Decoder]) -> String {
    chain
        .iter()
        .map(|decoder| decoder.label())
        .collect::<Vec<_>>()
        .join(" → ")
}

/// A chain split the way the menu presents it: what to unwrap first, and the
/// one step at the end that turns bytes into text.
///
/// The two are chosen differently — the unwrappings stack and the view is one
/// of a set — and a menu that offered nine equal items would make
/// `gzip → JSON` unreachable without saying so.
fn split_chain(chain: &[db::Decoder]) -> (Vec<db::Decoder>, Option<db::Decoder>) {
    match chain.split_last() {
        Some((last, rest)) if !last.is_transform() => (rest.to_vec(), Some(*last)),
        _ => (chain.to_vec(), None),
    }
}

impl Workspace {
    /// Open one of the row inspector's fields, the way clicking it would.
    pub fn expand_field(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.expanded_field = Some(ix);
        cx.notify();
    }

    pub(crate) fn open_decoder_menu(
        &mut self,
        column: String,
        chain: Vec<db::Decoder>,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.decoder_menu = Some(DecoderMenu { at, column, chain });
        cx.notify();
    }

    pub(crate) fn close_decoder_menu(&mut self, cx: &mut Context<Self>) {
        if self.decoder_menu.take().is_some() {
            cx.notify();
        }
    }

    /// Read this column with `chain` from here on — or, for an empty one, go
    /// back to letting the sniffer decide.
    fn set_field_decoder(
        &mut self,
        column: String,
        chain: Vec<db::Decoder>,
        cx: &mut Context<Self>,
    ) {
        match chain.is_empty() {
            true => {
                self.field_decoders.remove(&column);
            }
            false => {
                self.field_decoders.insert(column, chain);
            }
        }
        cx.notify();
    }

    /// Medis's `Viewer:` and `Encoder:` in one list.
    ///
    /// Auto first and on its own, because it is not a tenth format — it is the
    /// answer to "I do not know", and it is what every field starts on. Then
    /// the views, which are exclusive, and then the unwrappings, which are not:
    /// a value can be base64 of gzip of MessagePack and each layer is a
    /// separate thing to be right or wrong about.
    pub(crate) fn render_decoder_menu(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let open = self.decoder_menu.as_ref()?;
        let at = open.at;
        let column = open.column.clone();
        let (unwrap, view) = split_chain(&open.chain);
        let chosen = self.field_decoders.contains_key(&column);

        let mut menu = ui::ContextMenu::new("decoder-menu")
            .at(at)
            .width(px(200.))
            .on_dismiss(cx.listener(|this, _, _, cx| this.close_decoder_menu(cx)));

        let auto = column.clone();
        menu = menu
            .item(
                ui::MenuItem::new("Auto")
                    .icon(match chosen {
                        false => IconName::Check,
                        true => IconName::Sparkle,
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_decoder_menu(cx);
                        this.set_field_decoder(auto.clone(), Vec::new(), cx);
                    })),
            )
            .separator();

        for decoder in db::Decoder::ALL.into_iter().filter(|d| !d.is_transform()) {
            let showing = view == Some(decoder) && chosen;
            let column = column.clone();
            let unwrap = unwrap.clone();
            menu = menu.item(
                ui::MenuItem::new(decoder.label())
                    .icon(match showing {
                        true => IconName::Check,
                        false => IconName::Eye,
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_decoder_menu(cx);
                        let mut next = unwrap.clone();
                        next.push(decoder);
                        this.set_field_decoder(column.clone(), next, cx);
                    })),
            );
        }
        menu = menu.separator();

        for decoder in db::Decoder::ALL.into_iter().filter(|d| d.is_transform()) {
            let on = unwrap.contains(&decoder);
            let column = column.clone();
            let unwrap = unwrap.clone();
            menu = menu.item(
                ui::MenuItem::new(decoder.label())
                    .icon(match on {
                        true => IconName::CheckboxChecked,
                        false => IconName::CheckboxUnchecked,
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_decoder_menu(cx);
                        // Appended rather than inserted: `base64 → gzip` is
                        // the order they were wrapped in, and it is the order
                        // somebody ticking them off reaches for.
                        let mut next: Vec<db::Decoder> =
                            unwrap.iter().copied().filter(|d| *d != decoder).collect();
                        if !on {
                            next.push(decoder);
                        }
                        next.extend(view);
                        this.set_field_decoder(column.clone(), next, cx);
                    })),
            );
        }

        Some(menu)
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

/// A hex dump, laid out rather than wrapped.
///
/// The whole use of a dump is that byte 3 is under byte 3 on the line above,
/// and wrapping sixteen bytes into a 280px panel destroys exactly that. So the
/// lines keep their width and the panel slides sideways over them.
fn hex_block(text: &str, ty: &ui::Typography) -> AnyElement {
    div()
        .font(ty.mono_font())
        .text_size(ty.mono_size)
        .line_height(ty.mono_line_height)
        .whitespace_nowrap()
        .child(StyledText::new(text.to_string()))
        .into_any_element()
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

#[cfg(test)]
mod tests {
    use super::{chain_label, split_chain};
    use db::Decoder;

    #[test]
    fn a_chain_splits_into_what_to_unwrap_and_what_to_show() {
        let (unwrap, view) = split_chain(&[Decoder::Base64, Decoder::Gzip, Decoder::MsgPack]);
        assert_eq!(unwrap, vec![Decoder::Base64, Decoder::Gzip]);
        assert_eq!(view, Some(Decoder::MsgPack));
    }

    #[test]
    fn a_chain_that_only_unwraps_has_no_view_to_tick() {
        // What such a chain shows is decided by `db::decode`'s fallback, and
        // ticking a view the user never chose would claim otherwise.
        let (unwrap, view) = split_chain(&[Decoder::Gzip]);
        assert_eq!(unwrap, vec![Decoder::Gzip]);
        assert_eq!(view, None);
    }

    #[test]
    fn every_step_is_named() {
        assert_eq!(chain_label(&[Decoder::Gzip, Decoder::Json]), "gzip → JSON");
    }
}
