//! The grid as a view: focus, keyboard, and the frame around the element.
//!
//! Keyboard handling lives here rather than in the element because keys are
//! dispatched to a focused node, and focus belongs to the view tree. The
//! element only ever sees the mouse.

use std::time::Instant;

use gpui::{
    div, prelude::*, px, Context, Focusable as _, IntoElement, KeyDownEvent, ParentElement, Render,
    Styled, Window,
};
use ui::ActiveTheme;

use crate::element::GridElement;
use crate::export::Format;
use crate::state::Grid;

impl Render for Grid {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.bench.is_some() {
            self.tick_benchmark(window, cx);
        }
        // An edit that has just ended left the focus inside an editor that is
        // about to stop existing. Taking it back here rather than in the
        // handler is only because this is where the window is.
        if std::mem::take(&mut self.refocus) {
            self.focus_handle(cx).focus(window, cx);
        }
        if std::mem::take(&mut self.focus_edit) {
            if let Some(editing) = &self.editing {
                editing.editor.focus_handle(cx).focus(window, cx);
            }
        }
        let c = cx.colors().clone();
        let editing = self.editing_overlay(cx);

        div()
            .relative()
            .size_full()
            .track_focus(&self.focus_handle(cx))
            .key_context("Grid")
            .bg(c.panel)
            .on_key_down(cx.listener(Self::on_key))
            .child(GridElement::new(cx.entity()))
            .children(editing)
    }
}

impl Grid {
    /// The cell editor, placed over the cell it belongs to.
    ///
    /// A real element positioned in the container rather than something the
    /// grid paints itself: text editing is the editor crate's job, and the
    /// grid already knows exactly where every cell is.
    fn editing_overlay(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let editing = self.editing.as_ref()?;
        let c = cx.colors().clone();
        let (x, width) = self.column_bounds(editing.col);
        let height = self.density.row_height(cx);
        let top = cx.metrics().grid_header_height + height * editing.row as f32 - self.scroll.y;
        let left = self.gutter + x - self.scroll.x;
        // Scrolled out from under itself. Hiding it rather than clamping it
        // keeps the editor honest about which cell it is over.
        if top < px(0.) || left < self.gutter {
            return None;
        }
        Some(
            div()
                .absolute()
                .top(top)
                .left(left)
                .w(width)
                .h(height)
                .bg(c.panel)
                .border_1()
                .border_color(c.accent)
                .child(editing.editor.clone())
                .into_any_element(),
        )
    }
}

impl Grid {
    /// One benchmark frame: time the gap since the last one, sweep the viewport
    /// so the next frame has to shape text it has not shaped before, and ask
    /// for another frame immediately.
    ///
    /// The sweep is the point. A grid that repaints the same forty rows forever
    /// hits the line-layout cache every time and reports a number that has
    /// nothing to do with scrolling a million rows.
    fn tick_benchmark(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut b) = self.bench.take() else {
            return;
        };

        let now = Instant::now();
        if let Some(last) = b.last {
            b.frame.record((now - last).as_secs_f32() * 1000.);
        }
        b.last = Some(now);

        let max_y = (self.content_height(cx) - self.viewport.height).max(px(0.));
        let max_x = (self.content_width() - self.viewport.width).max(px(0.));
        self.scroll.y += px(13.);
        self.scroll.x += px(7.) * b.direction;
        if self.scroll.y > max_y {
            self.scroll.y = px(0.);
        }
        if self.scroll.x > max_x {
            self.scroll.x = max_x;
            b.direction = -1.;
        } else if self.scroll.x < px(0.) {
            self.scroll.x = px(0.);
            b.direction = 1.;
        }

        // Reported once a second or so rather than per frame: logging is not
        // free and this runs inside the thing being measured.
        if b.frame.due(300) {
            log::info!("{}", b.frame.report());
            log::info!("{}", b.paint.report());
        }

        self.bench = Some(b);
        window.request_animation_frame();
        cx.notify();
    }

    fn on_key(&mut self, e: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        let shift = k.modifiers.shift;
        // ⌘ jumps to the end of the axis rather than moving by one, matching
        // every spreadsheet and every macOS text field.
        let jump = k.modifiers.platform;
        let rows = self.row_count();

        match k.key.as_str() {
            "up" if jump => self.go_to_row(0, shift, cx),
            "down" if jump => self.go_to_row(rows.saturating_sub(1), shift, cx),
            "up" => self.move_cursor(-1, 0, shift, cx),
            "down" => self.move_cursor(1, 0, shift, cx),
            // Sideways moves the viewport, not a selection. There is no cell
            // cursor to walk along a row any more — the selection is the row —
            // so the only thing left of the axis is what is off the edge of it.
            "left" | "right" => {
                let step = match k.key.as_str() {
                    "left" => 1.,
                    _ => -1.,
                };
                let far = match jump {
                    true => f32::from(self.content_width()),
                    false => 120.,
                };
                self.scroll_by(gpui::point(px(step * far), px(0.)), cx);
            }
            "pageup" => self.page(-1, shift, cx),
            "pagedown" => self.page(1, shift, cx),
            "home" => self.go_to_row(0, shift, cx),
            "end" => self.go_to_row(rows.saturating_sub(1), shift, cx),
            "a" if jump => self.select_all(cx),

            // ---- clipboard ------------------------------------------------
            //
            // Tab-separated, because the overwhelmingly common destination for
            // a copied selection is a spreadsheet, and tabs are the one format
            // it pastes into columns without an import dialog. ⇧ adds the
            // column names for when the rows are landing somewhere empty; the
            // other formats are named explicitly from the context menu.
            "c" if jump => self.copy_selection(Format::Tsv { headers: shift }, cx),

            // ---- editing --------------------------------------------------
            //
            // Nothing here opens an editor: a keystroke would have to guess
            // which column of the selected row it meant. Editing starts with a
            // double click, which names the cell as part of the gesture.
            //
            // All of this stages and nothing of it writes: both are reversible
            // until Commit, which is what makes it safe to put a
            // destructive-sounding gesture on a single keystroke at all.
            "z" if jump && shift => self.redo(cx),
            "z" if jump => self.undo(cx),
            "backspace" | "delete" if jump => self.delete_rows(cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}
