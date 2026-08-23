//! The editor's custom [`Element`].
//!
//! Same shape as the grid's: `request_layout` claims space, `prepaint` decides
//! which lines are on screen and shapes them, `paint` emits quads and text.
//! Only the visible lines are shaped, so a ten-thousand-line script costs the
//! same per frame as a ten-line one.
//!
//! Everything the mouse handlers need — the shaped lines included — is copied
//! into a [`Frame`] and captured by value. The handlers run on the *next* event,
//! long after the borrow of the entity has ended.

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    fill, point, px, relative, size, App, Bounds, ContentMask, CursorStyle, DispatchPhase, Element,
    ElementId, ElementInputHandler, Entity, Font, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollWheelEvent, ShapedLine, Style, TextAlign, TextRun,
    UnderlineStyle, Window,
};
use smallvec::SmallVec;
use ui::{ActiveTheme, SyntaxTheme};

use crate::buffer::{byte_to_char, char_to_byte};
use crate::editor::{Editor, EditorMode, LastLayout};
use crate::selection::Selection;

/// Space either side of the line-number column.
const GUTTER_PAD: f32 = 10.;
/// Narrowest the number column gets, in digits — two, so a file growing from 9
/// to 10 lines does not shift every line of code sideways.
const MIN_DIGITS: usize = 2;
/// Caret thickness, in logical pixels — so two device pixels on a retina
/// display, which is the thinnest a line can be drawn there and what every
/// native text field on the platform draws. This was 2 for a while, which is
/// twice that: a 4-device-pixel bar is wider than the stems of the monospace
/// glyphs it stands between, so it read as a block cursor that had lost its
/// nerve rather than as an insertion point.
const CARET: f32 = 1.;

pub struct EditorElement {
    editor: Entity<Editor>,
    style: EditorStyle,
}

/// The knobs a host sets. Everything else comes from the theme.
#[derive(Clone, Debug)]
pub struct EditorStyle {
    pub font: Font,
    pub font_size: Pixels,
    pub line_height: Pixels,
    /// Inset either side of the text.
    pub padding_x: Pixels,
    pub padding_y: Pixels,
}

impl EditorStyle {
    /// The code face: what the SQL console and the cell editor use.
    pub fn mono(cx: &App) -> Self {
        let ty = cx.typography();
        Self {
            font: ty.mono_font(),
            font_size: ty.mono_size,
            line_height: ty.mono_line_height,
            padding_x: px(10.),
            padding_y: px(6.),
        }
    }

    /// The code face at field scale: mono, no padding, and the line height of
    /// the row it sits in rather than of a paragraph of code.
    ///
    /// For a one-line field whose contents really are code — the `where` clause
    /// above the rows. The argument in [`EditorStyle::ui`] cuts the other way
    /// there: what is typed into that box is a SQL fragment, it is highlighted
    /// as one, and setting it in the UI face makes it the only place in the
    /// window where code is dressed as chrome.
    pub fn code(cx: &App) -> Self {
        let ty = cx.typography();
        Self {
            font: ty.mono_font(),
            font_size: ty.mono_size,
            line_height: ty.ui_line_height,
            padding_x: px(0.),
            padding_y: px(0.),
        }
    }

    /// The UI face, no padding: for a field that draws its own frame.
    ///
    /// A search box set in the code face reads as a piece of the data rather
    /// than a piece of the chrome, which is exactly backwards.
    pub fn ui(cx: &App) -> Self {
        let ty = cx.typography();
        Self {
            font: gpui::font(ty.ui_family.clone()),
            font_size: ty.ui_size,
            line_height: ty.ui_line_height,
            padding_x: px(0.),
            padding_y: px(0.),
        }
    }
}

impl EditorElement {
    pub fn new(editor: Entity<Editor>, style: EditorStyle) -> Self {
        Self { editor, style }
    }
}

/// One visible line, shaped and placed.
#[derive(Clone)]
struct Line {
    row: usize,
    /// Char offset the row starts at.
    start: usize,
    /// Length in chars, newline excluded.
    len: usize,
    shaped: ShapedLine,
}

impl Line {
    /// Char column for an x offset measured from the start of the text.
    fn column_for_x(&self, x: Pixels) -> usize {
        let byte = self.shaped.closest_index_for_x(x);
        byte_to_char(&self.shaped.text, byte)
    }

    fn x_for_column(&self, column: usize) -> Pixels {
        let byte = char_to_byte(&self.shaped.text, column.min(self.len));
        self.shaped.x_for_index(byte)
    }
}

/// The frame's geometry, captured by value for the mouse listeners.
#[derive(Clone)]
struct Frame {
    /// Where text is drawn: inside the padding, right of the gutter.
    text_bounds: Bounds<Pixels>,
    line_height: Pixels,
    char_width: Pixels,
    scroll: Point<Pixels>,
    rows: Range<usize>,
    line_count: usize,
    lines: Arc<Vec<Line>>,
    selections: Vec<Selection>,
    cursor_row: usize,
    marked: Option<Range<usize>>,
    focused: bool,
    cursor_visible: bool,
    mode: EditorMode,
    gutter_width: Pixels,
    show_line_numbers: bool,
    placeholder: Option<ShapedLine>,
    /// Rows spanned by the statement ⌘⏎ would run.
    statement_rows: Option<Range<usize>>,
    /// The word the open hover panel is describing.
    hovered: Option<Range<usize>>,
}

impl Frame {
    /// Char offset under a window point.
    ///
    /// Rows are arithmetic; the column goes through the shaped line, so a
    /// proportional fallback face or a CJK run lands on the right character
    /// rather than on `x / char_width`.
    fn offset_at(&self, p: Point<Pixels>) -> usize {
        let y = p.y - self.text_bounds.origin.y + self.scroll.y;
        let row = (f32::from(y) / f32::from(self.line_height)).floor().max(0.) as usize;
        let row = row.min(self.line_count.saturating_sub(1));
        let x = p.x - self.text_bounds.origin.x + self.scroll.x;
        match self.lines.iter().find(|l| l.row == row) {
            Some(line) => line.start + line.column_for_x(x.max(px(0.))),
            // Outside the shaped window — only reachable from a drag that has
            // run off the top or bottom, where the exact column does not matter.
            None => {
                let above = row < self.rows.start;
                match if above {
                    self.lines.first()
                } else {
                    self.lines.last()
                } {
                    Some(line) if above => line.start,
                    Some(line) => line.start + line.len,
                    None => 0,
                }
            }
        }
    }

    /// Char offset the pointer is *over*, as opposed to nearest to.
    ///
    /// A click to the right of a line puts the caret at the end of it; a
    /// pointer there is over nothing at all, and a panel describing the last
    /// word of the line would be describing something nobody is pointing at.
    fn offset_over(&self, p: Point<Pixels>) -> Option<usize> {
        if !self.text_bounds.contains(&p) {
            return None;
        }
        let y = p.y - self.text_bounds.origin.y + self.scroll.y;
        if y < px(0.) {
            return None;
        }
        let row = (f32::from(y) / f32::from(self.line_height)).floor() as usize;
        let line = self.lines.iter().find(|l| l.row == row)?;
        let x = p.x - self.text_bounds.origin.x + self.scroll.x;
        if x < px(0.) || x > line.shaped.width {
            return None;
        }
        Some(line.start + line.column_for_x(x))
    }
}

impl IntoElement for EditorElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        Some(ElementId::Name("editor".into()))
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = match self.editor.read(cx).mode {
            // A single-line field is exactly as tall as its text and its
            // padding — it is a control, and controls do not stretch.
            EditorMode::SingleLine => (self.style.line_height + self.style.padding_y * 2.).into(),
            EditorMode::Full => relative(1.).into(),
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> PrepaintState {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let font = self.style.font.clone();
        let font_size = self.style.font_size;
        let line_height = self.style.line_height;
        // The advance of `0` in the mono face: every glyph in it is that wide,
        // and the few that are not (CJK, emoji) are handled by going through the
        // shaped line wherever exactness matters.
        let font_id = window.text_system().resolve_font(&font);
        let char_width = window
            .text_system()
            .advance(font_id, font_size, '0')
            .map(|a| a.width)
            .unwrap_or(font_size * 0.6);

        let focused = self.editor.read(cx).focus().is_focused(window);
        let syntax = cx.theme().syntax.clone();
        let colors = cx.theme().colors.clone();

        // Everything that reads or clamps model state happens in one update, so
        // the scroll the frame reports is the scroll paint will use.
        let frame_seed = self.editor.update(cx, |editor, cx| {
            editor.sync_focus(focused, cx);
            let line_count = editor.buffer.line_count();

            let digits = digit_count(line_count).max(MIN_DIGITS);
            let gutter_width = if editor.show_line_numbers && editor.mode == EditorMode::Full {
                char_width * digits as f32 + px(GUTTER_PAD * 2.)
            } else {
                px(0.)
            };

            let text_bounds = Bounds {
                origin: point(
                    bounds.origin.x + gutter_width + self.style.padding_x,
                    bounds.origin.y + self.style.padding_y,
                ),
                size: size(
                    (bounds.size.width - gutter_width - self.style.padding_x * 2.).max(px(0.)),
                    (bounds.size.height - self.style.padding_y * 2.).max(px(0.)),
                ),
            };

            editor.viewport = text_bounds.size;
            editor.refresh_longest_line();
            editor.refresh_highlights();
            let max_scroll = editor.max_scroll(text_bounds.size, line_height, char_width);
            if std::mem::take(&mut editor.autoscroll) {
                editor.scroll_cursor_into_view(text_bounds.size, line_height, char_width);
            }
            editor.scroll.x = editor.scroll.x.clamp(px(0.), max_scroll.x);
            editor.scroll.y = editor.scroll.y.clamp(px(0.), max_scroll.y);

            let first = (f32::from(editor.scroll.y) / f32::from(line_height)).floor() as usize;
            let visible =
                (f32::from(text_bounds.size.height) / f32::from(line_height)).ceil() as usize;
            let rows = first..(first + visible + 1).min(line_count);
            let rows_clone = rows.clone();

            // An empty range is the cursor sitting past the last `;`, where
            // there is no next statement yet. Boxing that is boxing nothing —
            // an outline around a blank line, which reads as a rendering fault
            // rather than as "⌘⏎ has nothing to send".
            let statement_rows = editor
                .active_statement()
                .filter(|range| !range.is_empty())
                .map(|range| {
                    let first = editor.buffer.offset_to_point(range.start).row;
                    let last = editor.buffer.offset_to_point(range.end).row;
                    first..last + 1
                });
            let cursor = editor.selections.newest();
            editor.layout = Some(LastLayout {
                bounds,
                text_origin: point(
                    text_bounds.origin.x - editor.scroll.x,
                    text_bounds.origin.y - editor.scroll.y,
                ),
                line_height,
                char_width,
            });

            let texts = rows_for_shaping(&editor.buffer, rows_clone, editor.masked);
            // Resolved here, inside the one borrow of the entity, because the
            // highlighter holds a parse of the whole document and the shaping
            // below deliberately holds nothing at all.
            let spans = highlight_rows(editor, &texts, &syntax);
            let errors = error_spans(editor, &texts);

            let seed = FrameSeed {
                text_bounds,
                gutter_width,
                scroll: editor.scroll,
                rows,
                line_count,
                selections: editor.selections.all().to_vec(),
                cursor_row: editor.buffer.offset_to_point(cursor.head).row,
                marked: editor.marked.clone(),
                cursor_visible: editor.cursor_visible,
                mode: editor.mode,
                show_line_numbers: editor.show_line_numbers && editor.mode == EditorMode::Full,
                statement_rows,
                hovered: editor.hover.as_ref().map(|open| open.range.clone()),
                texts,
                spans,
                errors,
                empty: editor.buffer.is_empty(),
                placeholder: editor.placeholder.to_string(),
            };
            let _ = cx;
            seed
        });

        // Shaping needs `&mut App` and no live borrow of the entity, which is
        // why the text was copied out above.
        let base = colors.text;
        let lines: Vec<Line> = frame_seed
            .texts
            .iter()
            .enumerate()
            .map(|(index, (row, start, text))| {
                let runs = runs_for(
                    text,
                    frame_seed
                        .spans
                        .get(index)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                    frame_seed.errors.get(index).cloned().flatten(),
                    base,
                    colors.danger,
                    &font,
                );
                let shaped =
                    window
                        .text_system()
                        .shape_line(text.clone().into(), font_size, &runs, None);
                Line {
                    row: *row,
                    start: *start,
                    len: text.chars().count(),
                    shaped,
                }
            })
            .collect();

        let placeholder = if frame_seed.empty && !frame_seed.placeholder.is_empty() {
            let run = TextRun {
                len: frame_seed.placeholder.len(),
                font: font.clone(),
                color: colors.text_disabled,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            Some(window.text_system().shape_line(
                frame_seed.placeholder.clone().into(),
                font_size,
                std::slice::from_ref(&run),
                None,
            ))
        } else {
            None
        };

        let frame = Frame {
            text_bounds: frame_seed.text_bounds,
            line_height,
            char_width,
            scroll: frame_seed.scroll,
            rows: frame_seed.rows,
            line_count: frame_seed.line_count,
            lines: Arc::new(lines),
            selections: frame_seed.selections,
            cursor_row: frame_seed.cursor_row,
            marked: frame_seed.marked,
            focused,
            cursor_visible: frame_seed.cursor_visible,
            mode: frame_seed.mode,
            gutter_width: frame_seed.gutter_width,
            show_line_numbers: frame_seed.show_line_numbers,
            placeholder,
            statement_rows: frame_seed.statement_rows,
            hovered: frame_seed.hovered,
        };

        PrepaintState { hitbox, frame }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _layout: &mut (),
        st: &mut PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let c = cx.colors().clone();
        let f = &st.frame;
        let font = self.style.font.clone();
        let font_size = self.style.font_size;

        window.set_cursor_style(CursorStyle::IBeam, &st.hitbox);
        let focus = self.editor.read(cx).focus().clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );

        // ---- active line ---------------------------------------------------
        //
        // Only with a collapsed cursor: with a selection on screen the highlight
        // competes with it and both get harder to read.
        let collapsed = f.selections.iter().all(|s| s.is_empty());
        if f.mode == EditorMode::Full && f.focused && collapsed {
            let y = f.text_bounds.origin.y + f.line_height * f.cursor_row as f32 - f.scroll.y;
            window.paint_quad(fill(
                Bounds {
                    origin: point(bounds.origin.x, y),
                    size: size(bounds.size.width, f.line_height),
                },
                c.editor_active_line,
            ));
        }

        // ---- the statement ⌘⏎ would run -----------------------------------
        //
        // An outline around the whole thing, not a bar beside it. A rule at the
        // gutter's inner edge is a pixel away from the caret's home column and
        // reads as a stray border on the line numbers; a box says *this block*,
        // which is the only thing ⌘⏎ needs to communicate. It starts where the
        // text starts, so the gutter stays a column of numbers and nothing else.
        if let Some(rows) = &f.statement_rows {
            let first = rows.start.max(f.rows.start);
            let last = rows.end.min(f.rows.end);
            if last > first {
                let top = f.text_bounds.origin.y + f.line_height * first as f32 - f.scroll.y;
                let left = bounds.origin.x + f.gutter_width;
                window.paint_quad(gpui::quad(
                    Bounds {
                        origin: point(left, top),
                        size: size(
                            (bounds.size.width - f.gutter_width).max(px(0.)),
                            f.line_height * (last - first) as f32,
                        ),
                    },
                    px(3.),
                    gpui::transparent_black(),
                    px(1.),
                    c.editor_active_statement,
                    gpui::BorderStyle::Solid,
                ));
            }
        }

        // ---- gutter --------------------------------------------------------
        if f.show_line_numbers {
            window.with_content_mask(
                Some(ContentMask {
                    bounds: Bounds {
                        origin: bounds.origin,
                        size: size(f.gutter_width, bounds.size.height),
                    },
                }),
                |window| {
                    for line in f.lines.iter() {
                        let y =
                            f.text_bounds.origin.y + f.line_height * line.row as f32 - f.scroll.y;
                        let text = (line.row + 1).to_string();
                        let color = if line.row == f.cursor_row && f.focused {
                            c.editor_line_number_active
                        } else {
                            c.editor_line_number
                        };
                        let run = TextRun {
                            len: text.len(),
                            font: font.clone(),
                            color,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let shaped = window.text_system().shape_line(
                            text.into(),
                            font_size,
                            std::slice::from_ref(&run),
                            None,
                        );
                        // Right-aligned against the gutter's inner edge, so the
                        // ones column never moves.
                        let x = bounds.origin.x + f.gutter_width - px(GUTTER_PAD) - shaped.width();
                        let _ = shaped.paint(
                            point(x, y),
                            f.line_height,
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                },
            );
        }

        // ---- selections, text, caret ---------------------------------------
        let clip = Bounds {
            origin: point(bounds.origin.x + f.gutter_width, bounds.origin.y),
            size: size(
                (bounds.size.width - f.gutter_width).max(px(0.)),
                bounds.size.height,
            ),
        };
        window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
            let selection_color = if f.focused {
                c.text_selection
            } else {
                c.selected_inactive
            };

            for line in f.lines.iter() {
                let y = f.text_bounds.origin.y + f.line_height * line.row as f32 - f.scroll.y;
                let row_start = line.start;
                let row_end = line.start + line.len;
                let text_x = f.text_bounds.origin.x - f.scroll.x;

                // Under the selection, so that selecting the word the panel
                // describes still looks like a selection and not like a third
                // colour nobody chose.
                if let Some(word) = &f.hovered {
                    if word.end >= row_start && word.start <= row_end {
                        let a = word.start.clamp(row_start, row_end) - row_start;
                        let b = word.end.clamp(row_start, row_end) - row_start;
                        let x0 = text_x + line.x_for_column(a);
                        let x1 = text_x + line.x_for_column(b);
                        if x1 > x0 {
                            window.paint_quad(gpui::quad(
                                Bounds {
                                    origin: point(x0, y),
                                    size: size(x1 - x0, f.line_height),
                                },
                                px(2.),
                                c.active,
                                px(0.),
                                gpui::transparent_black(),
                                gpui::BorderStyle::Solid,
                            ));
                        }
                    }
                }

                for sel in &f.selections {
                    if sel.is_empty() {
                        continue;
                    }
                    let (s, e) = (sel.start(), sel.end());
                    if e < row_start || s > row_end {
                        continue;
                    }
                    let a = s.clamp(row_start, row_end) - row_start;
                    let b = e.clamp(row_start, row_end) - row_start;
                    let x0 = text_x + line.x_for_column(a);
                    // A selection running through the newline gets a sliver past
                    // the last glyph — otherwise a multi-line selection looks
                    // like it stops at each line's last character.
                    let x1 = text_x
                        + line.x_for_column(b)
                        + if e > row_end { f.char_width } else { px(0.) };
                    if x1 > x0 {
                        window.paint_quad(fill(
                            Bounds {
                                origin: point(x0, y),
                                size: size(x1 - x0, f.line_height),
                            },
                            selection_color,
                        ));
                    }
                }

                let _ = line.shaped.paint(
                    point(text_x, y),
                    f.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );

                // Marked (pre-edit) IME text gets an underline, which is the
                // only signal that those characters are not committed yet.
                if let Some(marked) = &f.marked {
                    let (s, e) = (marked.start, marked.end);
                    if e >= row_start && s <= row_end {
                        let a = s.clamp(row_start, row_end) - row_start;
                        let b = e.clamp(row_start, row_end) - row_start;
                        let x0 = text_x + line.x_for_column(a);
                        let x1 = text_x + line.x_for_column(b);
                        if x1 > x0 {
                            window.paint_quad(fill(
                                Bounds {
                                    origin: point(x0, y + f.line_height - px(2.)),
                                    size: size(x1 - x0, px(1.)),
                                },
                                c.text_muted,
                            ));
                        }
                    }
                }
            }

            if let Some(placeholder) = &f.placeholder {
                let _ = placeholder.paint(
                    point(f.text_bounds.origin.x, f.text_bounds.origin.y),
                    f.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }

            if f.focused && f.cursor_visible {
                for sel in &f.selections {
                    let Some(line) = f
                        .lines
                        .iter()
                        .find(|l| sel.head >= l.start && sel.head <= l.start + l.len)
                    else {
                        continue;
                    };
                    let column = sel.head - line.start;
                    let x = f.text_bounds.origin.x - f.scroll.x + line.x_for_column(column);
                    let y = f.text_bounds.origin.y + f.line_height * line.row as f32 - f.scroll.y;
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(x, y),
                            size: size(px(CARET), f.line_height),
                        },
                        c.accent,
                    ));
                }
            }
        });

        self.register_handlers(st, window);
    }
}

/// What `prepaint` pulls out of the model before it can shape anything.
struct FrameSeed {
    text_bounds: Bounds<Pixels>,
    gutter_width: Pixels,
    scroll: Point<Pixels>,
    rows: Range<usize>,
    line_count: usize,
    selections: Vec<Selection>,
    cursor_row: usize,
    marked: Option<Range<usize>>,
    cursor_visible: bool,
    mode: EditorMode,
    show_line_numbers: bool,
    statement_rows: Option<Range<usize>>,
    hovered: Option<Range<usize>>,
    texts: Vec<(usize, usize, String)>,
    /// Colours for each entry of `texts`, in the same order. Empty when there
    /// is no highlighter at all.
    spans: Vec<Vec<(Range<usize>, Hsla)>>,
    /// The part of each row the server complained about, if any — byte ranges
    /// within that row's own text. Empty when nothing has failed.
    errors: Vec<Option<Range<usize>>>,
    empty: bool,
    placeholder: String,
}

pub struct PrepaintState {
    hitbox: Hitbox,
    frame: Frame,
}

impl EditorElement {
    fn register_handlers(&self, st: &PrepaintState, window: &mut Window) {
        let hitbox = st.hitbox.clone();
        let editor = self.editor.clone();
        let f = st.frame.clone();

        {
            let hitbox = hitbox.clone();
            let editor = editor.clone();
            let line_height = f.line_height;
            window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !hitbox.id.should_handle_scroll(window) {
                    return;
                }
                let delta = e.delta.pixel_delta(line_height);
                editor.update(cx, |editor, cx| editor.scroll_by(delta, cx));
                cx.stop_propagation();
            });
        }

        {
            let hitbox = hitbox.clone();
            let editor = editor.clone();
            let f = f.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || e.button != MouseButton::Left
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                let offset = f.offset_at(e.position);
                let handle = editor.read(cx).focus().clone();
                window.focus(&handle, cx);
                editor.update(cx, |editor, cx| match e.click_count {
                    1 => {
                        // Clicking somewhere else is an answer of "none of
                        // these" to whatever the popup was offering.
                        editor.close_completions(cx);
                        editor.close_hover(cx);
                        editor.dragging = true;
                        editor.place_cursor(offset, e.modifiers.shift, cx);
                    }
                    2 => editor.select_word_at(offset, cx),
                    _ => editor.select_line_at(offset, cx),
                });
                cx.stop_propagation();
            });
        }

        {
            let hitbox = hitbox.clone();
            let editor = editor.clone();
            let f = f.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                if e.dragging() {
                    if !editor.read(cx).dragging {
                        return;
                    }
                    let offset = f.offset_at(e.position);
                    editor.update(cx, |editor, cx| editor.place_cursor(offset, true, cx));
                    return;
                }
                // Resting on a name is the whole gesture: no click, no
                // modifier. Off the end of a line and outside the editor both
                // come back `None`, and both mean the same thing to the panel.
                let over = match hitbox.is_hovered(window) {
                    true => f.offset_over(e.position),
                    false => None,
                };
                editor.update(cx, |editor, cx| editor.hover_at(over, cx));
            });
        }

        window.on_mouse_event(move |_: &MouseUpEvent, phase, _window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            if editor.read(cx).dragging {
                editor.update(cx, |editor, _| editor.dragging = false);
            }
        });
    }
}

/// Ask the highlighter about each row the frame is about to shape.
///
/// Nothing at all when there is no highlighter — an empty `Vec` rather than a
/// `Vec` of empties, so the common case of a plain single-line input allocates
/// nothing per frame.
fn highlight_rows(
    editor: &Editor,
    texts: &[(usize, usize, String)],
    syntax: &SyntaxTheme,
) -> Vec<Vec<(Range<usize>, Hsla)>> {
    // A masked field shows bullets, and any range computed against the text
    // behind them would be describing something nobody can see.
    let highlighter = match editor.masked {
        true => None,
        false => editor.highlighter.as_ref(),
    };
    let Some(highlighter) = highlighter else {
        return Vec::new();
    };
    texts
        .iter()
        .map(|(row, _, text)| highlighter.row(*row, text, syntax))
        .collect()
}

/// Where the error mark falls on each row the frame is about to shape.
///
/// The mark is kept in char offsets over the whole buffer and wanted in bytes
/// within one line, which is two conversions and no shortcut: char offsets are
/// what selections speak, and bytes are what a `TextRun` counts.
fn error_spans(editor: &Editor, texts: &[(usize, usize, String)]) -> Vec<Option<Range<usize>>> {
    let Some(error) = editor.error.clone() else {
        return Vec::new();
    };
    texts
        .iter()
        .map(|(_, start, text)| {
            let from = error.start.max(*start);
            let to = error.end.min(start + text.chars().count());
            (from < to).then(|| char_to_byte(text, from - start)..char_to_byte(text, to - start))
        })
        .collect()
}

/// Split one line into coloured runs, filling the gaps the highlighter left and
/// putting a squiggle under whatever the server objected to.
fn runs_for(
    text: &str,
    spans: &[(Range<usize>, Hsla)],
    error: Option<Range<usize>>,
    base: Hsla,
    danger: Hsla,
    font: &Font,
) -> SmallVec<[TextRun; 8]> {
    let mut runs: SmallVec<[TextRun; 8]> = SmallVec::new();
    let mut push = |range: Range<usize>, color: Hsla| {
        // One cut where the mark starts and one where it ends, so no run has
        // to be half underlined. Both clamp into the range, so a mark that is
        // somewhere else entirely leaves it in one piece.
        let mut cuts = [range.start, range.start, range.end, range.end];
        if let Some(error) = &error {
            cuts[1] = error.start.clamp(range.start, range.end);
            cuts[2] = error.end.clamp(range.start, range.end);
        }
        for pair in cuts.windows(2) {
            let (from, to) = (pair[0], pair[1]);
            if from >= to {
                continue;
            }
            let marked = error
                .as_ref()
                .is_some_and(|error| from >= error.start && to <= error.end);
            let run = TextRun {
                len: to - from,
                font: font.clone(),
                color,
                background_color: None,
                underline: marked.then(|| UnderlineStyle {
                    thickness: px(1.),
                    color: Some(danger),
                    wavy: true,
                }),
                strikethrough: None,
            };
            // Merge with the previous run when both agree: fewer runs is less
            // work for the shaper, and highlighters emit adjacent same-colour
            // spans constantly.
            match runs.last_mut() {
                Some(last) if last.color == run.color && last.underline == run.underline => {
                    last.len += run.len
                }
                _ => runs.push(run),
            }
        }
    };

    let mut at = 0;
    for (range, color) in spans {
        if range.start > at {
            push(at..range.start, base);
        }
        if range.end > range.start {
            push(range.clone(), *color);
        }
        at = range.end;
    }
    if text.len() > at {
        push(at..text.len(), base);
    }
    runs
}

/// Copy the visible lines out of the buffer, with the char offset each starts
/// at, so shaping can happen after the entity borrow has ended.
fn rows_for_shaping(
    buffer: &crate::buffer::Buffer,
    rows: Range<usize>,
    masked: bool,
) -> Vec<(usize, usize, String)> {
    rows.map(|row| {
        let line = buffer.line(row);
        // One bullet per character, so every offset the layout computes — the
        // caret, the selection, the hit test — still lands where it would have.
        let line = if masked {
            "\u{2022}".repeat(line.chars().count())
        } else {
            line
        };
        (
            row,
            buffer.point_to_offset(crate::buffer::Point::new(row, 0)),
            line,
        )
    })
    .collect()
}

fn digit_count(n: usize) -> usize {
    let mut n = n.max(1);
    let mut digits = 0;
    while n > 0 {
        digits += 1;
        n /= 10;
    }
    digits
}
