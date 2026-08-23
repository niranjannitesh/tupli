//! The grid's custom [`Element`].
//!
//! A grid of `div`s cannot work at this scale, and not because divs are slow:
//! it is because a div per cell makes the cost of a frame proportional to the
//! data, and no amount of optimisation changes an asymptote. This element does
//! layout arithmetically — a division to find the first visible row, a binary
//! search to find the first visible column — so a frame costs the same for a
//! million rows as for ten.
//!
//! The three phases do exactly what their names say. `request_layout` claims
//! the space. `prepaint` decides *what* is on screen and clamps the scroll.
//! `paint` emits quads and text runs in back-to-front layers, then registers
//! the frame's mouse listeners.

use std::fmt::Write as _;
use std::ops::Range;
use std::sync::Arc;

use db::ResultSet;
use gpui::{
    black, fill, point, px, relative, size, App, Bounds, ContentMask, Corners, CursorStyle,
    DispatchPhase, Element, ElementId, Entity, Font, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollWheelEvent, Style, TextAlign, TextRun, TransformationMatrix,
    Window,
};
use smallvec::SmallVec;
use sqlgen::{PendingChanges, RowRef};
use ui::{ActiveTheme, IconName};

use crate::state::{
    is_right_aligned, CellRect, ColumnLayout, Grid, GridEvent, Sort, SCROLLBAR, SCROLLBAR_INSET,
};

/// What is under the pointer, which decides the cursor shape and what a drag does.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Region {
    /// A column's right edge in the header: drag resizes.
    ColumnEdge(usize),
    Header(usize),
    Cell(usize, usize),
    Gutter(usize),
    Empty,
}

/// Grab distance for a column edge, each side.
const EDGE_GRAB: f32 = 4.;

/// Space reserved at the right of every header cell for the sort chevron: the
/// glyph, then the gap that keeps it off the column border. The glyph carries a
/// margin of its own — the artwork fills about three quarters of its box — so
/// the two pixels here are all that has to be added by hand.
const SORT_ARROW_WIDTH: Pixels = px(16.);

/// How big the chevron itself is drawn, inside [`SORT_ARROW_WIDTH`].
const SORT_ARROW_GLYPH: Pixels = px(14.);

/// The chevron that says which way a column is sorted.
///
/// It is the same `chevron-up` / `chevron-down` file the disclosure triangles
/// and the dropdowns use, so the header points the way the rest of the window
/// points. This was three stacked quads once, on the theory that an arrowhead
/// four pixels tall did not justify an asset; at that size the steps ran
/// together and it read as a smudge rather than as a direction.
fn paint_sort_arrow(
    bounds: Bounds<Pixels>,
    descending: bool,
    color: Hsla,
    window: &mut Window,
    cx: &App,
) {
    let icon = if descending {
        IconName::ChevronDown
    } else {
        IconName::ChevronUp
    };
    // Left of the slot, so the leftover falls between the chevron and the
    // border rather than between the chevron and the name it belongs to.
    let glyph = Bounds {
        origin: point(
            bounds.origin.x,
            bounds.origin.y + (bounds.size.height - SORT_ARROW_GLYPH) / 2.,
        ),
        size: size(SORT_ARROW_GLYPH, SORT_ARROW_GLYPH),
    };
    // A missing or unparseable icon costs the column its arrow and nothing
    // else; the header still sorts, and taking the frame down over it would be
    // out of all proportion.
    if let Err(error) = window.paint_svg(
        glyph,
        icon.path(),
        None,
        TransformationMatrix::unit(),
        color,
        cx,
    ) {
        log::warn!("sort chevron: {error:#}");
    }
}
/// Shortest a scrollbar thumb may get. Without a floor, a million rows produce
/// a sub-pixel thumb that cannot be seen, let alone grabbed.
const MIN_THUMB: f32 = 24.;

pub struct GridElement {
    grid: Entity<Grid>,
}

impl GridElement {
    pub fn new(grid: Entity<Grid>) -> Self {
        Self { grid }
    }
}

/// The frame's geometry, captured by value.
///
/// Everything the paint pass and the mouse listeners need is copied out of the
/// entity here, once. The listeners outlive the borrow — they run on the *next*
/// event, not now — so they cannot hold a reference into the model, and the
/// paint pass needs `&mut App` for text shaping, which a live `&Grid` borrow
/// would forbid.
#[derive(Clone)]
struct Frame {
    bounds: Bounds<Pixels>,
    /// The scrolling region: below the header, right of the frozen gutter.
    body: Bounds<Pixels>,
    gutter_width: Pixels,
    header_height: Pixels,
    sort: Option<Sort>,
    /// Whether a column resize is in flight, so the pointer can keep the resize
    /// shape while the gesture wanders off the edge it started on.
    dragging: bool,
    /// The header under the pointer, which is where the sort hint goes.
    hover_header: Option<usize>,
    row_height: Pixels,
    scroll: Point<Pixels>,
    max_scroll: Point<Pixels>,
    rows: Range<usize>,
    cols: Range<usize>,
    data: Arc<ResultSet>,
    columns: Vec<ColumnLayout>,
    x_offsets: Vec<Pixels>,
    selection: Vec<CellRect>,
    zebra: bool,
    /// Rows that came back from the server. Rows past this are staged inserts
    /// and have no backing column data at all.
    fetched_rows: usize,
    /// Everything staged. Cloning an `Arc` per frame is the whole cost of
    /// painting pending state, which is why the grid keeps it behind one.
    changes: Arc<PendingChanges>,
}

impl Frame {
    fn column_bounds(&self, col: usize) -> (Pixels, Pixels) {
        (self.x_offsets[col], self.columns[col].width)
    }

    fn content_width(&self) -> Pixels {
        self.x_offsets.last().copied().unwrap_or_default()
    }

    fn column_at(&self, x: Pixels) -> usize {
        match self
            .x_offsets
            .binary_search_by(|off| off.partial_cmp(&x).unwrap())
        {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
        .min(self.columns.len().saturating_sub(1))
    }

    fn row_selected(&self, row: usize) -> bool {
        self.selection.iter().any(|r| r.rows.contains(&row))
    }

    fn row_count(&self) -> usize {
        self.fetched_rows + self.changes.new_row_count()
    }

    /// Which row of the model a screen row is: the fetched ones in order, then
    /// the staged inserts under them in the order they were added.
    fn row_ref(&self, row: usize) -> RowRef {
        match row.checked_sub(self.fetched_rows) {
            None => RowRef::Existing(row),
            Some(nth) => RowRef::New(self.changes.new_rows().nth(nth).unwrap_or(usize::MAX)),
        }
    }

    /// Map a window point to what is under it — arithmetic, not a search over
    /// painted elements, which is why hit-testing costs the same at a million
    /// rows as at ten.
    fn region(&self, p: Point<Pixels>) -> Region {
        let local = p - self.bounds.origin;
        if self.columns.is_empty() {
            return Region::Empty;
        }

        if local.y < self.header_height {
            if local.x < self.gutter_width {
                return Region::Empty;
            }
            let x = local.x - self.gutter_width + self.scroll.x;
            let col = self.column_at(x);
            let (off, w) = self.column_bounds(col);
            if f32::from(x - (off + w)).abs() <= EDGE_GRAB {
                return Region::ColumnEdge(col);
            }
            return Region::Header(col);
        }

        let offset = local.y - self.header_height + self.scroll.y;
        if offset < px(0.) {
            return Region::Empty;
        }
        let row = (f32::from(offset) / f32::from(self.row_height)) as usize;
        if row >= self.row_count() {
            return Region::Empty;
        }
        if local.x < self.gutter_width {
            return Region::Gutter(row);
        }
        let x = local.x - self.gutter_width + self.scroll.x;
        if x > self.content_width() {
            return Region::Empty;
        }
        Region::Cell(row, self.column_at(x))
    }

    /// The row a drag is pointing at, clamped into the grid.
    ///
    /// Unlike [`Frame::region`] this never answers "nowhere". A drag that has
    /// run past the last row, off the side, or out of the window altogether is
    /// still pointing at a row, and stopping the selection dead because the
    /// pointer left the cells by two pixels is the bug this exists to avoid.
    fn drag_row(&self, p: Point<Pixels>) -> usize {
        let row = row_at(p.y, self.body, self.row_height, self.scroll.y);
        row.min(self.row_count().saturating_sub(1))
    }

    /// How many rows past the visible edge a drag is reaching, signed.
    fn drag_beyond(&self, p: Point<Pixels>) -> isize {
        rows_beyond(p.y, self.body, self.row_height)
    }
}

/// The row under `y`, with the pointer clamped into the body first.
fn row_at(y: Pixels, body: Bounds<Pixels>, row_height: Pixels, scroll: Pixels) -> usize {
    let top = body.origin.y;
    let last = (top + body.size.height - row_height).max(top);
    let offset = (y.clamp(top, last) - top + scroll).max(px(0.));
    (f32::from(offset) / f32::from(row_height)) as usize
}

/// How many rows past the visible edge `y` is, signed, zero while inside.
///
/// Dragging below the last visible row means "keep going", so each move event
/// walks the cursor one row further and lets the autoscroll that every cursor
/// move already asks for do the scrolling. Reaching further out walks faster,
/// which is how every list on this platform behaves. The cap is there because
/// a pointer flung to the other end of a 6K display should not select ten
/// thousand rows in one event.
fn rows_beyond(y: Pixels, body: Bounds<Pixels>, row_height: Pixels) -> isize {
    let top = body.origin.y;
    let bottom = top + body.size.height;
    let past = match () {
        _ if y < top => y - top,
        _ if y > bottom => y - bottom,
        _ => return 0,
    };
    let rows = (f32::from(past) / f32::from(row_height)) as isize;
    rows.clamp(-24, 24) + if past > px(0.) { 1 } else { -1 }
}

pub struct PrepaintState {
    hitbox: Hitbox,
    frame: Frame,
    /// Set only in benchmark runs; keeps `Instant::now()` off the ordinary path.
    timed: bool,
}

impl IntoElement for GridElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for GridElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        Some(ElementId::Name("data-grid".into()))
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
        // The grid always takes everything it is given: it is a viewport onto
        // data, so its size is the container's decision, never the data's.
        let mut style = Style::default();
        style.size = size(relative(1.).into(), relative(1.).into());
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
        let header_height = cx.metrics().grid_header_height;
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let timed = self.grid.read(cx).bench.is_some();

        let frame = self.grid.update(cx, |grid, cx| {
            if !grid.measured {
                measure_columns(grid, window, cx);
            }
            let row_height = grid.density.row_height(cx);
            let gutter_width = gutter_width(grid, window, cx);

            let body = Bounds {
                origin: point(
                    bounds.origin.x + gutter_width,
                    bounds.origin.y + header_height,
                ),
                size: size(
                    (bounds.size.width - gutter_width).max(px(0.)),
                    (bounds.size.height - header_height).max(px(0.)),
                ),
            };
            grid.viewport = body.size;

            // Autoscroll before clamping, so a cursor move to the last row can
            // push the offset past the old maximum and then be clamped to the
            // new one, rather than being clamped away first.
            if std::mem::take(&mut grid.autoscroll) {
                grid.scroll_cursor_into_view(body.size, cx);
            }
            grid.clamp_scroll(body.size, cx);
            grid.gutter = gutter_width;

            Frame {
                bounds,
                body,
                gutter_width,
                header_height,
                sort: grid.sort,
                dragging: grid.dragging_column.is_some(),
                hover_header: grid.hover_header,
                row_height,
                scroll: grid.scroll,
                max_scroll: grid.max_scroll(body.size, cx),
                rows: grid.visible_rows(grid.scroll.y, body.size.height, cx),
                cols: visible_columns(grid, grid.scroll.x, body.size.width),
                data: grid.data().clone(),
                columns: grid.columns.clone(),
                x_offsets: grid.x_offsets.clone(),
                selection: grid.selection().to_vec(),
                zebra: grid.zebra,
                fetched_rows: grid.fetched_row_count(),
                changes: grid.changes.clone(),
            }
        });

        PrepaintState {
            hitbox,
            frame,
            timed,
        }
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
        let started = if st.timed {
            Some(std::time::Instant::now())
        } else {
            None
        };
        let c = cx.colors().clone();
        let ty = cx.typography().clone();
        let f = &st.frame;

        window.paint_quad(fill(bounds, c.panel));

        // Everything the grid draws is set in the mono face, headers included.
        // A column name is an identifier — the same identifier you would type
        // into the `where` box — and setting it in the UI face makes the header
        // read as a label about the data rather than as part of it. It also
        // costs the header its alignment with the column beneath, which for the
        // numeric columns is the whole point of a right edge.
        let mono = ty.mono_font();

        // ---- body: stripes, selection, cell text --------------------------
        //
        // Clipped to the body rather than trusting the loops to stay inside it:
        // a partially visible row at the bottom edge has to be cut off, not
        // skipped, or scrolling steps a whole row at a time instead of a pixel.
        window.with_content_mask(Some(ContentMask { bounds: f.body }), |window| {
            let mut scratch = String::with_capacity(64);
            for row in f.rows.clone() {
                let y = f.body.origin.y + f.row_height * row as f32 - f.scroll.y;
                let row_bounds = Bounds {
                    origin: point(f.body.origin.x, y),
                    size: size(f.body.size.width, f.row_height),
                };

                if f.zebra && row % 2 == 1 {
                    window.paint_quad(fill(row_bounds, c.grid_stripe));
                }

                // Pending state is a wash under the row, not a replacement for
                // it: a staged row still reads as a row, it just reads as one
                // that has not been agreed with the server yet.
                let row_ref = f.row_ref(row);
                let deleted = matches!(row_ref, RowRef::Existing(r) if f.changes.is_deleted(r));
                if deleted {
                    window.paint_quad(fill(row_bounds, c.grid_deleted));
                } else if matches!(row_ref, RowRef::New(_)) {
                    window.paint_quad(fill(row_bounds, c.grid_inserted));
                }

                if f.row_selected(row) {
                    window.paint_quad(fill(row_bounds, c.selected));
                }

                for col in f.cols.clone() {
                    let (off, cw) = f.column_bounds(col);
                    let x = f.body.origin.x + off - f.scroll.x;
                    let cell = Bounds {
                        origin: point(x, y),
                        size: size(cw, f.row_height),
                    };

                    let column = &f.data.columns[col];
                    // A staged value wins over the fetched one, and is tinted
                    // so it is obvious which cells the commit will carry.
                    let (text, color) = match f.changes.value(row_ref, col) {
                        Some(value) => {
                            if !deleted {
                                window.paint_quad(fill(cell, c.grid_dirty));
                            }
                            if value.is_null() {
                                ("NULL", c.text_disabled)
                            } else {
                                scratch.clear();
                                let _ = write!(scratch, "{value}");
                                (scratch.as_str(), c.text)
                            }
                        }
                        // A new row's untouched cells have nothing behind them:
                        // the server will fill them in with its defaults.
                        None if matches!(row_ref, RowRef::New(_)) => ("DEFAULT", c.text_disabled),
                        None => match column.render(row, &mut scratch) {
                            db::CellText::Null => ("NULL", c.text_disabled),
                            db::CellText::Borrowed(s) => (s, c.text),
                            db::CellText::Formatted => (scratch.as_str(), c.text),
                        },
                    };
                    if text.is_empty() {
                        continue;
                    }
                    // Only an ordinary value gets a quiet tail. When the colour
                    // is already saying something — null, staged, disabled — a
                    // second colour inside the same cell would be saying it
                    // twice, in a different language.
                    let quiet = (color == c.text).then_some((column.meta.kind, c.text_subtle));
                    paint_cell_text(
                        text,
                        cell,
                        color,
                        quiet,
                        &mono,
                        ty.mono_size,
                        f.row_height,
                        is_right_aligned(column.meta.kind),
                        window,
                        cx,
                    );
                }

                // The strike goes over the text, once per row, rather than
                // through `TextRun::strikethrough` per cell — the same line,
                // for the cost of one quad instead of a re-shape of every cell.
                if deleted {
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(
                                f.body.origin.x - f.scroll.x,
                                y + f.row_height / 2. - px(0.5),
                            ),
                            size: size(f.content_width(), px(1.)),
                        },
                        c.danger,
                    ));
                }
            }

            // Column separators last, so they sit on top of the row fills
            // rather than being repainted over by the next row's stripe.
            for col in f.cols.clone() {
                let (off, cw) = f.column_bounds(col);
                let x = f.body.origin.x + off + cw - f.scroll.x;
                window.paint_quad(fill(
                    Bounds {
                        origin: point(x - px(1.), f.body.origin.y),
                        size: size(px(1.), f.body.size.height),
                    },
                    c.border,
                ));
            }
        });

        // ---- frozen gutter: row ordinals ---------------------------------
        let gutter = Bounds {
            origin: point(bounds.origin.x, f.body.origin.y),
            size: size(f.gutter_width, f.body.size.height),
        };
        window.paint_quad(fill(gutter, c.panel));
        window.with_content_mask(Some(ContentMask { bounds: gutter }), |window| {
            let mut buf = String::with_capacity(24);
            for row in f.rows.clone() {
                let y = f.body.origin.y + f.row_height * row as f32 - f.scroll.y;
                let cell = Bounds {
                    origin: point(bounds.origin.x, y),
                    size: size(f.gutter_width, f.row_height),
                };
                if f.row_selected(row) {
                    window.paint_quad(fill(cell, c.selected_inactive));
                }
                // A staged row has no ordinal to show — it has no place in the
                // result set yet — so it gets a mark instead of a number.
                let (mark, color) = match f.row_ref(row) {
                    RowRef::New(_) => (true, c.success),
                    RowRef::Existing(r) if f.changes.is_deleted(r) => (false, c.danger),
                    RowRef::Existing(r) if f.changes.is_row_edited(r) => (false, c.warning),
                    RowRef::Existing(_) => (false, c.text_subtle),
                };
                buf.clear();
                if mark {
                    buf.push('+');
                } else {
                    write_ordinal(&mut buf, row + 1);
                }
                paint_cell_text(
                    &buf,
                    cell,
                    color,
                    None,
                    &mono,
                    ty.ui_size_sm,
                    f.row_height,
                    true,
                    window,
                    cx,
                );
            }
        });
        window.paint_quad(fill(
            Bounds {
                origin: point(bounds.origin.x + f.gutter_width - px(1.), f.body.origin.y),
                size: size(px(1.), f.body.size.height),
            },
            c.border,
        ));

        // ---- header ------------------------------------------------------
        let header = Bounds {
            origin: bounds.origin,
            size: size(bounds.size.width, f.header_height),
        };
        window.paint_quad(fill(header, c.chrome));
        // The gutter's own heading. It is there to say that the left column is
        // an ordinal and not data — the first thing anyone wonders about a
        // grid whose first column is all small integers.
        paint_cell_text(
            "#",
            Bounds {
                origin: bounds.origin,
                size: size(f.gutter_width, f.header_height),
            },
            c.text_subtle,
            None,
            &mono,
            ty.ui_size_sm,
            f.header_height,
            true,
            window,
            cx,
        );
        let header_clip = Bounds {
            origin: point(bounds.origin.x + f.gutter_width, bounds.origin.y),
            size: size(
                (bounds.size.width - f.gutter_width).max(px(0.)),
                f.header_height,
            ),
        };
        window.with_content_mask(
            Some(ContentMask {
                bounds: header_clip,
            }),
            |window| {
                for col in f.cols.clone() {
                    let (off, cw) = f.column_bounds(col);
                    let x = bounds.origin.x + f.gutter_width + off - f.scroll.x;
                    let cell = Bounds {
                        origin: point(x, bounds.origin.y),
                        size: size(cw, f.header_height),
                    };
                    let meta = &f.data.columns[col].meta;
                    // A primary key is accent-coloured rather than badged: the
                    // header is 28px tall and a badge would crowd out the name it
                    // is annotating.
                    let color = if meta.is_pk { c.accent } else { c.text_muted };
                    let sorted = f.sort.filter(|s| s.col == col);
                    // The name gives up the last few pixels of the cell to the
                    // arrow, so a long name truncates into the ellipsis instead of
                    // running underneath it.
                    let name_cell = Bounds {
                        origin: cell.origin,
                        size: size((cw - SORT_ARROW_WIDTH).max(px(0.)), f.header_height),
                    };
                    paint_cell_text(
                        &meta.name,
                        name_cell,
                        color,
                        None,
                        &mono,
                        ty.ui_size_sm,
                        f.header_height,
                        false,
                        window,
                        cx,
                    );
                    // The arrow the column has, or — under the pointer — the
                    // one a click would give it. Drawing all of them all the
                    // time turns a twenty-column header into a row of arrows
                    // with names attached; drawing none of them leaves nothing
                    // to say the header is a control at all.
                    let arrow = match (sorted, f.hover_header == Some(col)) {
                        (Some(sort), _) => Some((sort.descending, c.accent)),
                        (None, true) => Some((false, c.text_disabled)),
                        (None, false) => None,
                    };
                    if let Some((descending, color)) = arrow {
                        paint_sort_arrow(
                            Bounds {
                                origin: point(x + cw - SORT_ARROW_WIDTH, bounds.origin.y),
                                size: size(SORT_ARROW_WIDTH, f.header_height),
                            },
                            descending,
                            color,
                            window,
                            cx,
                        );
                    }
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(x + cw - px(1.), bounds.origin.y),
                            size: size(px(1.), f.header_height),
                        },
                        c.border,
                    ));
                }
            },
        );
        window.paint_quad(fill(
            Bounds {
                origin: point(bounds.origin.x, bounds.origin.y + f.header_height - px(1.)),
                size: size(bounds.size.width, px(1.)),
            },
            c.border,
        ));

        paint_scrollbars(f, bounds, window, cx);
        self.register_handlers(st, window);

        if let Some(started) = started {
            let ms = started.elapsed().as_secs_f32() * 1000.;
            self.grid.update(cx, |grid, _| {
                if let Some(bench) = grid.bench.as_mut() {
                    bench.paint.record(ms);
                }
            });
        }
    }
}

impl GridElement {
    fn register_handlers(&self, st: &PrepaintState, window: &mut Window) {
        let hitbox = st.hitbox.clone();
        let grid = self.grid.clone();
        let f = st.frame.clone();

        // Setting the pointer shape has to happen while painting, not while
        // moving: cursor styles are per-frame state, so a hover that only
        // updated on mouse-move would flicker back on every repaint. The other
        // half of that — asking for a repaint when the answer changes — is the
        // hover handler below, without which the shape set by the last frame
        // that happened to be drawn stays on the whole grid.
        let over_edge = matches!(f.region(window.mouse_position()), Region::ColumnEdge(_));
        // During a drag the pointer is often past the edge it is dragging, and
        // sometimes past the grid: the shape has to follow the gesture, not the
        // geometry.
        if over_edge || f.dragging {
            window.set_cursor_style(CursorStyle::ResizeLeftRight, &hitbox);
        }

        // Hover: repaint only when the pointer crosses into or out of a column
        // edge, which is the only thing about a buttonless move this element
        // draws differently.
        {
            let hitbox = hitbox.clone();
            let grid = grid.clone();
            let f = f.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || e.pressed_button.is_some() {
                    return;
                }
                let region = match hitbox.is_hovered(window) {
                    true => f.region(e.position),
                    false => Region::Empty,
                };
                let edge = match region {
                    Region::ColumnEdge(col) => Some(col),
                    _ => None,
                };
                // An edge belongs to the header it is in, so the hint stays put
                // while the pointer crosses the grab strip on its way out.
                let header = match region {
                    Region::Header(col) | Region::ColumnEdge(col) => Some(col),
                    _ => None,
                };
                let grid_ref = grid.read(cx);
                if grid_ref.hover_edge != edge || grid_ref.hover_header != header {
                    grid.update(cx, |grid, cx| {
                        grid.hover_edge = edge;
                        grid.hover_header = header;
                        cx.notify();
                    });
                }
            });
        }

        // Scroll. Deliberately keyed on `should_handle_scroll` rather than
        // `is_hovered`: hover is suppressed during keyboard input, and a grid
        // that stops scrolling because you just pressed a key is maddening.
        {
            let hitbox = hitbox.clone();
            let grid = grid.clone();
            window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !hitbox.id.should_handle_scroll(window) {
                    return;
                }
                let line_height = grid.read(cx).density.row_height(cx);
                let delta = e.delta.pixel_delta(line_height);
                grid.update(cx, |grid, cx| grid.scroll_by(delta, cx));
                cx.stop_propagation();
            });
        }

        // Press: start a resize, or move the cursor.
        {
            let hitbox = hitbox.clone();
            let grid = grid.clone();
            let f = f.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || e.button != MouseButton::Left
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                let extend = e.modifiers.shift;
                let region = f.region(e.position);
                let handle = grid.read(cx).focus().clone();
                // An open editor belongs to the cell it was opened on, and a
                // press anywhere else in the grid has left that cell — the
                // editor cannot go on floating over one row while the selection
                // is on another. Keeping what was typed rather than dropping it
                // is what every spreadsheet does: Escape is the gesture for
                // throwing an edit away, and a click that silently discarded it
                // would be a trap. Nothing reaches the server either way; the
                // value is staged like any other. A press inside the editor
                // never reaches here, because the editor's own hitbox is over
                // this one.
                grid.update(cx, |grid, cx| grid.stage_edit(cx));
                grid.update(cx, |grid, cx| match region {
                    Region::ColumnEdge(col) => {
                        grid.dragging_column = Some((col, e.position.x, grid.columns[col].width));
                    }
                    Region::Cell(row, col) => {
                        grid.selecting = true;
                        grid.set_cursor(row, col, extend, cx);
                        // Double-click is the only way into a cell: the click
                        // that came before it picked the row, and this one says
                        // which of its values to open. Where the result set
                        // cannot be written back it is the container's own idea
                        // of activation instead.
                        if e.click_count >= 2 {
                            if grid.is_editable() {
                                grid.begin_edit(row, col, None, cx);
                            } else {
                                cx.emit(GridEvent::Activated { row, col });
                            }
                        }
                    }
                    Region::Gutter(row) => {
                        grid.selecting = true;
                        grid.set_cursor(row, 0, extend, cx);
                    }
                    Region::Header(col) => grid.cycle_sort(col, cx),
                    Region::Empty => {}
                });
                // Not when an editor just opened over the cell — it wants the
                // keyboard, and taking it back here would close it immediately.
                if !matches!(region, Region::Empty) && !grid.read(cx).is_editing() {
                    window.focus(&handle, cx);
                }
            });
        }

        // Right press: aim the menu, but do not steal a selection.
        //
        // A right click inside the current selection means "these rows"; one
        // outside it means "that row", and picking it up first is what every
        // file manager and every spreadsheet does. Either way the grid only
        // says where the click was — the menu itself belongs to whatever
        // container knows which of the row operations make sense here.
        {
            let hitbox = hitbox.clone();
            let grid = grid.clone();
            let f = f.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || e.button != MouseButton::Right
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                let (row, col) = match f.region(e.position) {
                    Region::Cell(row, col) => (row, col),
                    Region::Gutter(row) => (row, 0),
                    _ => return,
                };
                let handle = grid.read(cx).focus().clone();
                grid.update(cx, |grid, cx| {
                    // As on the left button: the menu is about to open over the
                    // grid, and an editor left floating under it is neither
                    // reachable nor honest about which cell it belongs to.
                    grid.stage_edit(cx);
                    if !grid.selected_rows().contains(&row) {
                        grid.set_cursor(row, col, false, cx);
                    }
                    cx.emit(GridEvent::ContextMenu {
                        at: e.position,
                        row,
                        col,
                    });
                });
                window.focus(&handle, cx);
                cx.stop_propagation();
            });
        }

        // Drag: resize a column, or extend the selection.
        {
            let grid = grid.clone();
            let f = f.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, _window, cx| {
                if phase != DispatchPhase::Bubble || e.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                if let Some((col, start_x, start_w)) = grid.read(cx).dragging_column {
                    let width = start_w + (e.position.x - start_x);
                    grid.update(cx, |grid, cx| grid.resize_column(col, width, cx));
                    return;
                }
                if !grid.read(cx).selecting {
                    return;
                }
                let beyond = f.drag_beyond(e.position);
                let row = f.drag_row(e.position).saturating_add_signed(beyond);
                grid.update(cx, |grid, cx| {
                    let col = grid.cursor.1;
                    grid.set_cursor(row, col, true, cx);
                });
            });
        }

        {
            let grid = grid.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                if grid.read(cx).selecting {
                    grid.update(cx, |grid, _| grid.selecting = false);
                }
                if grid.read(cx).dragging_column.is_some() {
                    // Notify, because the next frame is what takes the resize
                    // shape back off the pointer, and letting go of a button is
                    // not by itself a reason for gpui to draw one. The frame
                    // after this re-reads the pointer against the new column
                    // widths, so it gets the shape right either way.
                    grid.update(cx, |grid, cx| {
                        grid.dragging_column = None;
                        grid.hover_edge = None;
                        grid.hover_header = None;
                        cx.notify();
                    });
                }
            });
        }
    }
}

/// Draw one cell's text, clipped to the cell only when it does not fit.
///
/// The `if` matters. A content mask is a scene operation, and pushing one for
/// every cell when nearly all of them fit is pure overhead on the common frame.
#[allow(clippy::too_many_arguments)]
fn paint_cell_text(
    text: &str,
    cell: Bounds<Pixels>,
    color: Hsla,
    quiet: Option<(db::ValueKind, Hsla)>,
    font: &Font,
    font_size: Pixels,
    line_height: Pixels,
    right_aligned: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let pad = px(Grid::CELL_PADDING);
    let inner_width = (cell.size.width - pad * 2.).max(px(0.));
    if inner_width <= px(0.) {
        return;
    }
    let text = &*one_line(text);

    // Split after `one_line`, because that is the string being shaped: a byte
    // offset into the original would land mid-character in a value that had a
    // newline replaced.
    let split = quiet
        .and_then(|(kind, _)| quiet_tail(kind, text))
        .filter(|at| *at > 0 && *at < text.len());

    let mut runs = SmallVec::<[TextRun; 2]>::new();
    runs.push(TextRun {
        len: split.unwrap_or(text.len()),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    });
    if let Some(at) = split {
        runs.push(TextRun {
            len: text.len() - at,
            font: font.clone(),
            color: quiet.map(|(_, dim)| dim).unwrap_or(color),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    // Hash-keyed shaping: on a cache hit — every cell that was also on screen
    // last frame — this never materialises a `SharedString` at all, which is
    // the difference between one allocation per visible cell per frame and none.
    let hash = {
        use std::hash::{Hash as _, Hasher as _};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        color.h.to_bits().hash(&mut h);
        color.s.to_bits().hash(&mut h);
        color.l.to_bits().hash(&mut h);
        font.family.hash(&mut h);
        split.hash(&mut h);
        h.finish()
    };
    let line =
        window
            .text_system()
            .shape_line_by_hash(hash, text.len(), font_size, &runs, None, || {
                text.to_string().into()
            });

    let overflows = line.width() > inner_width;
    let x = if right_aligned && !overflows {
        cell.origin.x + cell.size.width - pad - line.width()
    } else {
        cell.origin.x + pad
    };
    let origin = point(x, cell.origin.y);

    if overflows {
        let clip = Bounds {
            origin: point(cell.origin.x + pad, cell.origin.y),
            size: size(inner_width, cell.size.height),
        };
        window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
            let _ = line.paint(origin, line_height, TextAlign::Left, None, window, cx);
        });
    } else {
        let _ = line.paint(origin, line_height, TextAlign::Left, None, window, cx);
    }
}

/// Where a value's quiet tail begins, as a byte offset into `text`.
///
/// None of these characters can be dropped — `2026-08-20 11:02:31.482913+00` is
/// only that value with all of it — but a column of them is read at the minute,
/// and the rest is something the eye has to step over on every row. Dimming
/// keeps the value whole and still lets the column be scanned. The same goes
/// for a `numeric(20,16)` that is holding an integer, and for the twenty-eight
/// characters of a uuid that are not what tells two rows apart.
fn quiet_tail(kind: db::ValueKind, text: &str) -> Option<usize> {
    match kind {
        db::ValueKind::Timestamp | db::ValueKind::Time => {
            // From the *first* colon: a `+05:30` offset has colons of its own,
            // and searching backwards would find one of those.
            let clock = text.find(':')?;
            let fraction = text[clock..].find('.').map(|at| clock + at);
            let zone = text[clock..].find(['+', '-', 'Z']).map(|at| clock + at);
            match (fraction, zone) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        db::ValueKind::Decimal | db::ValueKind::Float => {
            let point = text.find('.')?;
            // An exponent has no zeros to spare: `1.500e10` means the scale.
            if text[point..].contains(['e', 'E']) {
                return None;
            }
            match text[point + 1..].rfind(|c: char| c != '0') {
                // 7065.0000000000000000 — the point itself is part of the noise.
                None => Some(point),
                Some(last) => Some(point + 2 + last),
            }
        }
        // The first group of a uuid is enough to tell two rows apart, and is
        // what anyone reads when they are looking for one.
        db::ValueKind::Uuid => (text.len() == 36 && text.as_bytes()[8] == b'-').then_some(8),
        _ => None,
    }
}

/// A cell value with its line breaks turned into something a row can hold.
///
/// A row is one line tall, and the text system will not shape a string with a
/// newline in it at all — it panics — so a `text` column holding two lines is
/// not a rendering question but a crash. The break becomes `↵`, which says a
/// line ended there; joining the lines with a space instead would make
/// `'a\nb'` and `'a b'` the same cell. Other control characters go to spaces:
/// they have no width and no glyph, and a NUL painted as nothing at all is a
/// value that looks shorter than it is.
///
/// Borrowed unless there is something to change, so the ordinary cell — every
/// cell, in almost every table — allocates nothing.
fn one_line(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(|c: char| c.is_control()) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            // CRLF is one break, not two.
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => out.push('↵'),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Overlay scrollbars: drawn on top of the content, taking no layout width.
/// A gutter-reserving scrollbar would make the grid permanently narrower to
/// serve a control macOS hides by default.
/// Overlay scrollbars, painted last so they float above the cells.
///
/// The thumb lights up whenever the pointer is anywhere over its track, not
/// only when it is over the thumb itself — an 8px target is hard to find, and
/// the brighter thumb is what tells you where to aim.
fn paint_scrollbars(f: &Frame, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let c = cx.colors();
    let w = px(SCROLLBAR);
    let inset = px(SCROLLBAR_INSET);
    // How far from a track the pointer counts as "over" it.
    let reach = px(16.);

    let has_v = f.max_scroll.y > px(0.);
    let has_h = f.max_scroll.x > px(0.);
    let mouse = window.mouse_position();

    // Where two scrollbars meet, each gives up the corner rather than crossing
    // the other.
    let corner = if has_v && has_h {
        w + inset * 2.
    } else {
        px(0.)
    };

    if has_v {
        let track = (f.body.size.height - corner).max(px(0.));
        let vis = f32::from(f.body.size.height);
        let thumb = (vis * vis / (vis + f32::from(f.max_scroll.y)))
            .max(MIN_THUMB)
            .min(f32::from(track));
        let t = f32::from(f.scroll.y) / f32::from(f.max_scroll.y);
        let x = bounds.origin.x + bounds.size.width - w - inset;
        let over = mouse.x >= x - reach
            && mouse.x <= x + w + inset
            && mouse.y >= f.body.origin.y
            && mouse.y <= f.body.origin.y + track;
        window.paint_quad(
            fill(
                Bounds {
                    origin: point(x, f.body.origin.y + px((f32::from(track) - thumb) * t)),
                    size: size(w, px(thumb)),
                },
                if over {
                    c.scrollbar_thumb_hover
                } else {
                    c.scrollbar_thumb
                },
            )
            .corner_radii(Corners::all(w / 2.)),
        );
    }

    if has_h {
        let track = (f.body.size.width - corner).max(px(0.));
        let vis = f32::from(f.body.size.width);
        let thumb = (vis * vis / (vis + f32::from(f.max_scroll.x)))
            .max(MIN_THUMB)
            .min(f32::from(track));
        let t = f32::from(f.scroll.x) / f32::from(f.max_scroll.x);
        let y = bounds.origin.y + bounds.size.height - w - inset;
        let over = mouse.y >= y - reach
            && mouse.y <= y + w + inset
            && mouse.x >= f.body.origin.x
            && mouse.x <= f.body.origin.x + track;
        window.paint_quad(
            fill(
                Bounds {
                    origin: point(f.body.origin.x + px((f32::from(track) - thumb) * t), y),
                    size: size(px(thumb), w),
                },
                if over {
                    c.scrollbar_thumb_hover
                } else {
                    c.scrollbar_thumb
                },
            )
            .corner_radii(Corners::all(w / 2.)),
        );
    }
}

/// Columns whose span intersects the horizontal window.
fn visible_columns(grid: &Grid, left: Pixels, width: Pixels) -> Range<usize> {
    if grid.columns.is_empty() {
        return 0..0;
    }
    let first = grid.column_at(left);
    let mut last = first;
    while last < grid.columns.len() && grid.x_offsets[last] < left + width {
        last += 1;
    }
    first..last.min(grid.columns.len())
}

/// Width of the frozen ordinal gutter: enough for the largest row number this
/// result set can show, and no more. A constant either wastes space on small
/// tables or clips the numbers on large ones.
fn gutter_width(grid: &Grid, window: &mut Window, cx: &App) -> Pixels {
    let digits = digit_count(grid.row_count().max(1));
    // Plus the separators the ordinals will carry. The face is mono, so a
    // comma is a digit wide and counting characters is enough.
    let chars = digits + (digits - 1) / 3;
    mono_advance(cx.typography().ui_size_sm, window, cx) * chars as f32
        + px(Grid::CELL_PADDING * 2.)
}

fn digit_count(mut n: usize) -> usize {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

/// Advance width of a digit in the mono face. One shaping call, cached by the
/// text system, and the number every width calculation in the grid rests on.
fn mono_advance(font_size: Pixels, window: &mut Window, cx: &App) -> Pixels {
    let run = TextRun {
        len: 1,
        font: cx.typography().mono_font(),
        color: black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .layout_line("0", font_size, std::slice::from_ref(&run), None)
        .width
}

/// Give every column a width from a sample of the data.
///
/// Runs once per result set, not per frame. Columns the user has resized keep
/// their width: an explicit decision outranks a heuristic.
fn measure_columns(grid: &mut Grid, window: &mut Window, cx: &App) {
    let advance = mono_advance(cx.typography().mono_size, window, cx);
    // The header is mono too, so its width is a character count as well.
    let header_advance = mono_advance(cx.typography().ui_size_sm, window, cx);
    let samples = grid.sample_widths();
    let data = grid.data().clone();

    for (i, column) in data.columns.iter().enumerate() {
        if grid.columns[i].user_sized {
            continue;
        }
        // Plus the strip the sort arrow is always given: without it the name
        // is measured to a width it is then truncated out of.
        let header = header_advance * column.meta.name.chars().count() as f32 + SORT_ARROW_WIDTH;
        let widest = header.max(advance * samples[i] as f32);
        grid.columns[i].width = px((f32::from(widest) + Grid::CELL_PADDING * 2.)
            .clamp(Grid::MIN_COLUMN_WIDTH, Grid::MAX_COLUMN_WIDTH));
    }
    grid.rebuild_offsets();
    grid.measured = true;
}

/// Row numbers without `format!`, which would otherwise be one allocation per
/// visible row per frame — the exact kind of per-cell cost this element exists
/// to avoid.
/// A row ordinal, grouped in threes.
///
/// `999982` and `9999982` are the same shape at a glance; `999,982` and
/// `9,999,982` are not. The gutter is the one place in the grid where the
/// *size* of the number is what is being read, so it gets separators. Cells do
/// not: a value there is data, and data that is copied out has to come back as
/// it went in.
fn write_ordinal(out: &mut String, n: usize) {
    let start = out.len();
    write_usize(out, n);
    // Walk forward from the first group, which is the short one.
    let mut at = start + (out.len() - start) % 3;
    if at == start {
        at += 3;
    }
    while at < out.len() {
        out.insert(at, ',');
        at += 4;
    }
}

fn write_usize(out: &mut String, mut n: usize) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.push_str(std::str::from_utf8(&buf[i..]).unwrap());
}

#[cfg(test)]
mod tests {
    use super::{one_line, quiet_tail, row_at, rows_beyond, write_ordinal};
    use gpui::{point, px, size, Bounds};

    /// A body 100px tall at y=50, showing five 20px rows.
    fn body() -> Bounds<gpui::Pixels> {
        Bounds {
            origin: point(px(0.), px(50.)),
            size: size(px(400.), px(100.)),
        }
    }

    fn ordinal(n: usize) -> String {
        let mut out = String::new();
        write_ordinal(&mut out, n);
        out
    }

    #[test]
    fn a_value_with_no_control_characters_is_not_copied() {
        let text = "ordinary";
        assert!(matches!(one_line(text), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn line_breaks_become_a_mark_that_fits_on_one_line() {
        // The shaper panics on a newline, so this is a crash rather than a
        // layout question: `select E'a\nb'` used to take the window with it.
        assert_eq!(one_line("a\nb"), "a↵b");
        assert_eq!(one_line("a\r\nb"), "a↵b");
        assert_eq!(one_line("a\rb"), "a↵b");
        assert_eq!(one_line("a\tb\0c"), "a b c");
    }

    #[test]
    fn a_drag_inside_the_body_lands_on_the_row_under_it() {
        assert_eq!(row_at(px(50.), body(), px(20.), px(0.)), 0);
        assert_eq!(row_at(px(69.), body(), px(20.), px(0.)), 0);
        assert_eq!(row_at(px(70.), body(), px(20.), px(0.)), 1);
        // Scrolled by two rows, the top of the body is row 2.
        assert_eq!(row_at(px(50.), body(), px(20.), px(40.)), 2);
    }

    #[test]
    fn a_drag_past_either_edge_still_names_a_row() {
        // Above the grid, and far above it: the first visible row, not nothing.
        assert_eq!(row_at(px(0.), body(), px(20.), px(0.)), 0);
        assert_eq!(row_at(px(-4000.), body(), px(20.), px(0.)), 0);
        // Below it: the last row that fits, not the one the pointer is over.
        assert_eq!(row_at(px(200.), body(), px(20.), px(0.)), 4);
        assert_eq!(row_at(px(4000.), body(), px(20.), px(0.)), 4);
    }

    #[test]
    fn only_a_drag_outside_the_body_walks_further() {
        assert_eq!(rows_beyond(px(100.), body(), px(20.)), 0);
        assert_eq!(rows_beyond(px(151.), body(), px(20.)), 1);
        assert_eq!(rows_beyond(px(200.), body(), px(20.)), 3);
        assert_eq!(rows_beyond(px(49.), body(), px(20.)), -1);
        assert_eq!(rows_beyond(px(-50.), body(), px(20.)), -6);
        // A pointer thrown to the far edge of a large display does not select
        // the whole result set in one event.
        assert_eq!(rows_beyond(px(40_000.), body(), px(20.)), 25);
    }

    #[test]
    fn ordinals_are_grouped_in_threes() {
        assert_eq!(ordinal(1), "1");
        assert_eq!(ordinal(999), "999");
        assert_eq!(ordinal(1_000), "1,000");
        assert_eq!(ordinal(999_982), "999,982");
        assert_eq!(ordinal(1_234_567), "1,234,567");
        assert_eq!(ordinal(12_345_678), "12,345,678");
    }

    fn quiet(kind: db::ValueKind, text: &str) -> &str {
        match quiet_tail(kind, text) {
            Some(at) => &text[at..],
            None => "",
        }
    }

    #[test]
    fn a_timestamp_goes_quiet_after_the_seconds() {
        use db::ValueKind::{Time, Timestamp};
        assert_eq!(
            quiet(Timestamp, "2026-08-20 11:02:31.482913+00"),
            ".482913+00"
        );
        assert_eq!(quiet(Timestamp, "2026-08-20 11:02:31+05:30"), "+05:30");
        assert_eq!(quiet(Timestamp, "2026-08-20T11:02:31Z"), "Z");
        // Nothing to dim: this one is already only what it says.
        assert_eq!(quiet(Timestamp, "2026-08-20 11:02:31"), "");
        assert_eq!(quiet(Time, "11:02:31.482913"), ".482913");
        // A date has no clock, so it has no tail — and the dashes in it must
        // not be read as a zone offset.
        assert_eq!(quiet(db::ValueKind::Date, "2026-08-20"), "");
    }

    #[test]
    fn a_numeric_goes_quiet_where_it_stops_saying_anything() {
        use db::ValueKind::{Decimal, Float};
        assert_eq!(quiet(Decimal, "7065.0000000000000000"), ".0000000000000000");
        assert_eq!(quiet(Decimal, "7065.5000000000000000"), "000000000000000");
        assert_eq!(quiet(Decimal, "-0.1200"), "00");
        assert_eq!(quiet(Decimal, "7065.5"), "");
        assert_eq!(quiet(Decimal, "1200"), "");
        // The zeros of an exponent are its scale, not padding.
        assert_eq!(quiet(Float, "1.500e10"), "");
    }

    #[test]
    fn a_uuid_keeps_its_first_group() {
        use db::ValueKind::{Text, Uuid};
        assert_eq!(
            quiet(Uuid, "3f3e0a1b-701b-4eb1-b7be-04a48586d53f"),
            "-701b-4eb1-b7be-04a48586d53f"
        );
        // Not every uuid column holds a uuid — a driver that could not map the
        // type hands over whatever text the server sent.
        assert_eq!(quiet(Uuid, "not-a-uuid"), "");
        assert_eq!(quiet(Text, "3f3e0a1b-701b-4eb1-b7be-04a48586d53f"), "");
    }
}
