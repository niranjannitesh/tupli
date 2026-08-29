//! The editor entity: state, commands, key handling, and the input handler.
//!
//! One implementation serves three configurations, per §11.3 of the plan — the
//! SQL console, a single-line [`EditorMode::SingleLine`] field, and (later) the
//! grid's inline cell editor. The differences are a mode flag and a couple of
//! booleans, not three widgets that drift apart.

use std::ops::Range;

use gpui::StatefulInteractiveElement as _;
use gpui::{
    div, point, px, App, Bounds, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Pixels, Render, ScrollHandle,
    SharedString, Size, Styled, Task, UTF16Selection, Window,
};
use ui::{ActiveTheme, SyntaxTheme};

use crate::buffer::{Buffer, Point};
use crate::completion::{self, CompletionContext, CompletionSource};
use crate::history::{Edit, EditKind, History};
use crate::hover::{HoverContext, HoverInfo, HoverSource};
use crate::movement;
use crate::selection::{Selection, SelectionSet};

/// Produces coloured byte ranges for one line of text.
///
/// A trait and not a bare closure because a real parser needs two things a
/// closure over a line cannot have: the whole document, since a string or a
/// comment does not stop at a newline, and somewhere to keep the tree between
/// frames. The editor still does not care where the colours came from — it says
/// when the text changed and asks for a row at a time.
pub trait Highlight {
    /// The document changed. Called at most once per buffer version, before
    /// any `row` call against it.
    fn refresh(&mut self, text: &str) {
        let _ = text;
    }

    /// Colours for one row, as byte ranges within that row's own text.
    fn row(&self, row: usize, line: &str, syntax: &SyntaxTheme) -> Vec<(Range<usize>, gpui::Hsla)>;
}

/// The stateless case: anything that can colour a line on its own is a
/// highlighter, and does not have to say so.
impl<F> Highlight for F
where
    F: Fn(&str, &SyntaxTheme) -> Vec<(Range<usize>, gpui::Hsla)>,
{
    fn row(
        &self,
        _row: usize,
        line: &str,
        syntax: &SyntaxTheme,
    ) -> Vec<(Range<usize>, gpui::Hsla)> {
        self(line, syntax)
    }
}

pub type Highlighter = Box<dyn Highlight>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorMode {
    /// One line, no wrapping, Enter submits instead of inserting.
    SingleLine,
    /// The full console: gutter, line numbers, multi-line.
    Full,
}

#[derive(Clone, Debug)]
pub enum EditorEvent {
    /// The text changed.
    Changed,
    /// The cursor or selection moved.
    SelectionChanged,
    /// Enter in a single-line editor.
    Submit,
    /// ⌘⏎: run the selection, or the statement under the cursor.
    Run,
    /// ⌘⇧⏎: run every statement in the buffer, or in the selection.
    RunAll,
    /// ⌘S. The editor has nowhere to save to — the host decides what a save
    /// means, exactly as it decides what a run means.
    Save,
    /// Escape.
    Cancel,
    /// An arrow or page key in a single-line field.
    ///
    /// There is nowhere for the caret to go on one line, so the gesture belongs
    /// to whatever list the field is driving — the command palette, and later
    /// the completion popup. The editor reports it and takes no view on it.
    Navigate(Direction),
}

/// Which way a [`EditorEvent::Navigate`] went.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    PageUp,
    PageDown,
}

/// Geometry left over from the last paint.
///
/// Only the input handler needs this — macOS asks "where on screen is this
/// range?" long after paint has returned, to place the candidate window. Mouse
/// handling does not use it: those closures capture the shaped lines directly,
/// which is exact rather than approximate.
#[derive(Clone, Copy, Debug)]
pub struct LastLayout {
    pub bounds: Bounds<Pixels>,
    /// Top-left of the first line's text, gutter and scroll already applied.
    pub text_origin: gpui::Point<Pixels>,
    pub line_height: Pixels,
    pub char_width: Pixels,
}

pub struct Editor {
    pub(crate) buffer: Buffer,
    pub(crate) selections: SelectionSet,
    pub(crate) mode: EditorMode,
    pub(crate) scroll: gpui::Point<Pixels>,
    pub(crate) marked: Option<Range<usize>>,
    pub(crate) placeholder: SharedString,
    /// Render every glyph as a bullet. For password fields, and nothing else:
    /// the buffer still holds the real text, so selection, arrow keys and
    /// backspace all behave normally and only the painting changes.
    pub(crate) masked: bool,
    pub(crate) read_only: bool,
    pub(crate) show_line_numbers: bool,
    pub(crate) highlighter: Option<Highlighter>,
    /// The buffer version the highlighter has been shown. `None` means it has
    /// seen nothing yet, which is also what installing a new one resets it to.
    highlighted: Option<usize>,
    pub(crate) layout: Option<LastLayout>,
    /// Set by hosts that draw their own frame; otherwise the code face.
    pub(crate) style: Option<crate::element::EditorStyle>,
    /// SQL mode: syntax colours, and a gutter rule marking the statement ⌘⏎
    /// would run.
    pub(crate) sql: bool,
    /// Where the server said the last statement went wrong, in char offsets.
    /// Cleared by the next edit: a message about text that is no longer there
    /// is worse than no message.
    pub(crate) error: Option<Range<usize>>,
    /// `(buffer version, cursor offset, range)` — recomputing the statement
    /// means scanning the whole text, so it is cached against both inputs.
    statement: Option<(usize, usize, Range<usize>)>,
    /// Set while the mouse is down, so drag extends rather than replaces.
    pub(crate) dragging: bool,
    /// Size of the text area from the last paint, for page-scrolling.
    pub(crate) viewport: Size<Pixels>,
    /// Set by anything that moves the cursor; consumed by the next prepaint.
    pub(crate) autoscroll: bool,
    pub(crate) cursor_visible: bool,
    pub(crate) focused: bool,
    /// How far one indent step goes. Settings' Editor pane writes it; the
    /// default is four, which is what SQL written by other people mostly is.
    pub(crate) tab_size: usize,
    /// Longest line in chars, cached against the buffer version. Recomputing it
    /// every frame would make a long script cost O(lines) per frame for a number
    /// that only changes when the text does.
    longest_line: (usize, usize),
    blink_epoch: usize,
    blink: Option<Task<()>>,
    focus: FocusHandle,
    history: History,

    // ---- completion ------------------------------------------------------
    /// Where the offers come from. `None` — the default — means this editor
    /// completes nothing, which is right for a password field and for every
    /// input that is not SQL.
    pub(crate) source: Option<Box<dyn crate::completion::CompletionSource>>,
    /// The open popup, if there is one.
    pub(crate) completion: Option<CompletionState>,

    // ---- hover -----------------------------------------------------------
    /// Where "what is that?" gets answered. `None` — the default — means this
    /// editor explains nothing, which is right for every field that is not SQL.
    pub(crate) hover_source: Option<Box<dyn HoverSource>>,
    /// The open panel, if there is one.
    pub(crate) hover: Option<HoverState>,
    /// The word the pointer is over that has not yet been rested on long
    /// enough. Kept separately from `hover` so that drifting across a name
    /// does not restart the wait on every pixel.
    hover_pending: Option<Range<usize>>,
    hover_task: Option<Task<()>>,

    // ---- find ------------------------------------------------------------
    /// The open find bar's search, and where in its results the editor is.
    /// `None` — the default — is a closed find bar, and costs nothing.
    pub(crate) find: Option<FindState>,
}

/// An open find.
pub(crate) struct FindState {
    search: crate::find::Search,
    /// Char ranges, left to right. Recomputed when the search or the text
    /// changes and not otherwise: it is O(text), and the text is repainted
    /// sixty times a second.
    pub(crate) matches: Vec<Range<usize>>,
    /// Index into `matches`. `None` when nothing matched.
    pub(crate) current: Option<usize>,
    /// The buffer version `matches` was computed from.
    version: usize,
    /// Where the cursor was when the find opened.
    ///
    /// Every recompute picks the first match at or after this, rather than at
    /// or after the cursor — which is the match it just moved the cursor to.
    /// Without it, typing `sel` in a script full of selects walks forward one
    /// match per keystroke.
    origin: usize,
}

/// An open hover panel.
pub(crate) struct HoverState {
    /// The word it describes, so the pointer moving within that word leaves it
    /// alone and moving out of it takes it away.
    pub(crate) range: Range<usize>,
    pub(crate) info: HoverInfo,
}

/// An open completion popup.
pub(crate) struct CompletionState {
    pub(crate) items: Vec<crate::completion::Completion>,
    pub(crate) selected: usize,
    /// The chars an accepted offer replaces: the word that was being typed.
    pub(crate) range: Range<usize>,
    /// The list scrolls, so the highlight has to be able to drag it: forty
    /// offers is four times what fits, and an arrow key that moved a selection
    /// out of sight would look like it had stopped working.
    pub(crate) scroll: ScrollHandle,
}

/// How long the caret stays in each state.
const BLINK: std::time::Duration = std::time::Duration::from_millis(530);

impl EventEmitter<EditorEvent> for Editor {}

impl Focusable for Editor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Editor {
    pub fn new(mode: EditorMode, cx: &mut Context<Self>) -> Self {
        Self {
            buffer: Buffer::default(),
            selections: SelectionSet::default(),
            mode,
            tab_size: 4,
            scroll: point(px(0.), px(0.)),
            marked: None,
            placeholder: SharedString::default(),
            masked: false,
            read_only: false,
            show_line_numbers: mode == EditorMode::Full,
            highlighter: None,
            highlighted: None,
            layout: None,
            style: None,
            sql: false,
            error: None,
            statement: None,
            dragging: false,
            viewport: gpui::size(px(0.), px(0.)),
            autoscroll: false,
            cursor_visible: true,
            focused: false,
            longest_line: (usize::MAX, 0),
            blink_epoch: 0,
            blink: None,
            focus: cx.focus_handle(),
            history: History::default(),
            source: None,
            completion: None,
            hover_source: None,
            hover: None,
            hover_pending: None,
            hover_task: None,
            find: None,
        }
    }

    pub fn single_line(cx: &mut Context<Self>) -> Self {
        Self::new(EditorMode::SingleLine, cx)
    }

    // ---- configuration ---------------------------------------------------

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buffer.reset(text);
        self.history.clear();
        self.selections = SelectionSet::default();
        // A field seeded with text is a field about to be typed into, and the
        // caret belongs after what is already there. A document is different:
        // you read it from the top, so the console keeps the caret at zero.
        if self.is_single_line() {
            let end = self.buffer.len();
            self.selections.set(vec![Selection::new(end, end)]);
        }
        self.marked = None;
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    pub fn text(&self) -> String {
        self.buffer.text()
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Show bullets instead of characters. Copy still yields the real text —
    /// masking is about shoulder surfing, not about withholding the value from
    /// the person who typed it.
    pub fn set_masked(&mut self, masked: bool) {
        self.masked = masked;
    }

    pub fn set_placeholder(&mut self, text: impl Into<SharedString>) {
        self.placeholder = text.into();
    }

    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    pub fn set_line_numbers(&mut self, on: bool) {
        self.show_line_numbers = on;
    }

    pub fn set_style(&mut self, style: crate::element::EditorStyle) {
        self.style = Some(style);
    }

    pub fn set_highlighter(&mut self, highlighter: Highlighter) {
        self.highlighter = Some(highlighter);
        self.highlighted = None;
    }

    /// Turn on everything that makes this a SQL console: the highlighter and
    /// the statement marker. One switch, because a caller who wants one always
    /// wants the other.
    pub fn set_sql(&mut self, on: bool) {
        self.sql = on;
        self.highlighter = on.then(crate::sql::highlighter);
        self.highlighted = None;
    }

    /// The statement under the cursor, in char offsets.
    pub(crate) fn active_statement(&mut self) -> Option<Range<usize>> {
        if !self.sql {
            return None;
        }
        let head = self.selections.newest().head;
        let version = self.buffer.version();
        if let Some((v, offset, range)) = &self.statement {
            if *v == version && *offset == head {
                return Some(range.clone());
            }
        }
        let range = crate::sql::statement_at(&self.buffer.text(), head);
        self.statement = Some((version, head, range.clone()));
        Some(range)
    }

    // ---- completion ------------------------------------------------------

    /// Install the thing that answers "what could go here?". Setting a source is
    /// the whole of turning completion on for an editor.
    pub fn set_completions(&mut self, source: impl CompletionSource) {
        self.source = Some(Box::new(source));
    }

    pub fn is_completing(&self) -> bool {
        self.completion.is_some()
    }

    /// Install the thing that answers "what is that?". Separate from
    /// [`Self::set_completions`] because they are separate questions: a field
    /// may want offers and no explanations, and the console wants both from
    /// the same catalog.
    pub fn set_hover(&mut self, source: impl HoverSource) {
        self.hover_source = Some(Box::new(source));
    }

    /// Rebuild the popup for wherever the cursor is now.
    ///
    /// `explicit` is ⌃Space: it opens the list even where nothing has been
    /// typed, which is how you ask a table what its columns are. Typing only
    /// opens it once there is a word to narrow by, because a list that appears
    /// on every space bar is a list you learn to dismiss without reading.
    pub fn refresh_completions(&mut self, explicit: bool, cx: &mut Context<Self>) {
        if self.source.is_none() || self.read_only || self.masked {
            return;
        }
        // Only with a single cursor. Multi-cursor completion would have to
        // agree on one word across several places, and there is no honest
        // answer when they differ.
        let selections = self.selections.all();
        let Some(cursor) = selections.first().filter(|_| selections.len() == 1) else {
            return self.close_completions(cx);
        };
        if !cursor.is_empty() {
            return self.close_completions(cx);
        }

        let text = self.buffer.text();
        let (range, qualifier) = completion::word_at(&text, cursor.head);
        let prefix: String = text.chars().skip(range.start).take(range.len()).collect();
        // A bare cursor in whitespace is not a question. After a dot it is:
        // `orders.` has asked something even though nothing follows it.
        if !explicit && prefix.is_empty() && qualifier.is_none() {
            return self.close_completions(cx);
        }

        let context = CompletionContext {
            text,
            offset: cursor.head,
            prefix: prefix.clone(),
            qualifier,
            explicit,
        };
        let items = completion::rank(
            self.source
                .as_ref()
                .expect("checked above")
                .completions(&context),
            &prefix,
        );
        if items.is_empty() {
            return self.close_completions(cx);
        }
        // Keep the highlight on the same offer across a keystroke where that
        // makes sense: narrowing a list should not move the selection off what
        // the user was already looking at.
        let selected = self
            .completion
            .as_ref()
            .and_then(|open| open.items.get(open.selected))
            .and_then(|was| items.iter().position(|item| item == was))
            .unwrap_or(0);
        let scroll = match self.completion.take() {
            // Narrowing the same popup keeps its scroll, so a list that grew
            // shorter under the cursor does not also jump.
            Some(open) => open.scroll,
            None => ScrollHandle::new(),
        };
        scroll.scroll_to_item(selected);
        self.completion = Some(CompletionState {
            items,
            selected,
            range,
            scroll,
        });
        cx.notify();
    }

    pub fn close_completions(&mut self, cx: &mut Context<Self>) {
        if self.completion.take().is_some() {
            cx.notify();
        }
    }

    fn move_completion(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(open) = self.completion.as_mut() else {
            return;
        };
        let len = open.items.len() as isize;
        // Wraps, because a list this short is faster to walk round than to walk
        // back through.
        open.selected = (open.selected as isize + delta).rem_euclid(len) as usize;
        open.scroll.scroll_to_item(open.selected);
        cx.notify();
    }

    /// Put the highlighted offer in, in place of the word that was being typed.
    fn accept_completion(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.completion.take() else {
            return;
        };
        let Some(item) = open.items.get(open.selected).cloned() else {
            cx.notify();
            return;
        };
        let range = open.range;
        let mut text = item.text().to_string();

        // A function is a thing you call, so it completes as a call and leaves
        // the caret between the brackets — accepting `coalesce` and then
        // reaching for shift-9 is two gestures for one word. Not when the
        // brackets are already there, either in the offer or in the text the
        // cursor is sitting in front of: completing over `count(1)` must not
        // make it `count()(1)`.
        let cursor = self.selections.newest().head;
        let call = item.kind == completion::CompletionKind::Function
            && !text.ends_with('(')
            && self.buffer.char_at(cursor) != Some('(');
        if call {
            text.push_str("()");
        }

        self.edit_with(EditKind::Other, cx, move |_, sel| {
            // The word may have grown since the popup opened — the cursor is
            // always its end, so take the end from the cursor rather than from
            // what was recorded.
            Some((range.start..sel.head.max(range.start), text.clone()))
        });
        if call {
            // Between the brackets rather than after them. `edit_with` leaves
            // every cursor at the end of what it inserted, which is the right
            // default for everything else it does.
            let head = self.selections.newest().head.saturating_sub(1);
            self.selections.set(vec![Selection::cursor(head)]);
        }
        cx.notify();
    }

    /// The SQL ⌘⏎ should send: the selection if there is one, otherwise the
    /// statement the cursor sits in, otherwise everything.
    pub fn run_text(&mut self) -> String {
        self.run_range().0
    }

    /// The text ⌘⏎ would send, and where in the buffer it came from.
    ///
    /// The range matters because Postgres reports a syntax error as a position
    /// *within the statement it was given*, and the console has to turn that
    /// back into a place in the document.
    pub fn run_range(&mut self) -> (String, Range<usize>) {
        let selected = self.selections.newest().range();
        if !selected.is_empty() {
            let text = self.buffer.slice(selected.clone());
            if !text.trim().is_empty() {
                return (text, selected);
            }
        }
        match self.active_statement() {
            Some(range) if !range.is_empty() => (self.buffer.slice(range.clone()), range),
            _ => (self.buffer.text(), 0..self.buffer.len()),
        }
    }

    /// Every statement ⌘⇧⏎ should send, each with where in the buffer it came
    /// from.
    ///
    /// The selection narrows it when there is one — "run all" inside a
    /// selection means all of *that*, which is how you re-run three statements
    /// out of a script of twenty. Each origin is absolute, so a syntax error in
    /// the fourth statement still lands under the right word.
    pub fn run_all(&mut self) -> Vec<(String, usize)> {
        let selected = self.selections.newest().range();
        let (text, base) = match selected.is_empty() {
            false => (self.buffer.slice(selected.clone()), selected.start),
            true => (self.buffer.text(), 0),
        };
        crate::sql::statements(&text)
            .into_iter()
            .map(|range| {
                let start = base + range.start;
                let bytes = crate::buffer::char_to_byte(&text, range.start)
                    ..crate::buffer::char_to_byte(&text, range.end);
                (text[bytes].to_string(), start)
            })
            .collect()
    }

    /// Mark the character the server complained about.
    ///
    /// `offset` is a char offset into the buffer, already translated out of the
    /// server's 1-based position within the statement. The mark covers the
    /// whole word under it rather than the single character, because a squiggle
    /// under one letter of a misspelled table name reads as a rendering bug.
    pub fn mark_error(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.buffer.clip_offset(offset);
        let mut end = offset;
        while self
            .buffer
            .char_at(end)
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            end += 1;
        }
        // Punctuation, or the very end of the text: one character, so there is
        // always something to see.
        if end == offset {
            end = (offset + 1).min(self.buffer.len());
        }
        self.error = Some(offset..end);
        self.selections.set(vec![Selection::cursor(offset)]);
        self.autoscroll = true;
        cx.notify();
    }

    /// Take the mark off, whether or not there was one.
    pub fn clear_error(&mut self, cx: &mut Context<Self>) {
        if self.error.take().is_some() {
            cx.notify();
        }
    }

    pub fn focus(&self) -> &FocusHandle {
        &self.focus
    }

    /// The cursor as a line/column pair, for the status bar.
    pub fn cursor_position(&self) -> Point {
        self.buffer.offset_to_point(self.selections.newest().head)
    }

    pub fn selections(&self) -> &SelectionSet {
        &self.selections
    }

    // ---- editing ---------------------------------------------------------

    /// The one path every mutation takes.
    ///
    /// `plan` turns each selection into the range it replaces and the text that
    /// goes there. Everything else — ordering the edits so offsets stay valid,
    /// moving the other cursors by the right delta, recording undo — happens
    /// here once instead of in every command.
    fn edit_with(
        &mut self,
        kind: EditKind,
        cx: &mut Context<Self>,
        plan: impl Fn(&Buffer, &Selection) -> Option<(Range<usize>, String)>,
    ) {
        if self.read_only {
            return;
        }
        let before: Vec<Selection> = self.selections.all().to_vec();
        let mut plans: Vec<(Range<usize>, String)> = before
            .iter()
            .filter_map(|sel| plan(&self.buffer, sel))
            .collect();
        if plans.is_empty() {
            return;
        }
        plans.sort_by_key(|(range, _)| range.start);

        // Walk forwards to work out where each cursor ends up, accumulating the
        // length change of every edit to its left.
        let mut delta: isize = 0;
        let mut after = Vec::with_capacity(plans.len());
        for (range, text) in &plans {
            let inserted = text.chars().count();
            let removed = range.end.saturating_sub(range.start);
            let head = (range.start as isize + delta) as usize + inserted;
            after.push(Selection::cursor(head));
            delta += inserted as isize - removed as isize;
        }

        // Apply backwards so each edit sees offsets it was computed against.
        let mut edits: Vec<Edit> = Vec::with_capacity(plans.len());
        for (range, text) in plans.iter().rev() {
            let old = self.buffer.replace(range.clone(), text);
            edits.push(Edit {
                start: range.start,
                old,
                new: text.clone(),
            });
        }
        edits.reverse();

        self.history.push(edits, before, after.clone(), kind);
        self.selections.set(after);
        self.marked = None;
        self.error = None;
        self.autoscroll = true;
        self.restart_blink(cx);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    /// Lay the SQL out again, without changing what it says.
    ///
    /// The selection decides the scope, the same way it decides what ⌘⏎ sends:
    /// format the part you highlighted, or the whole document if you
    /// highlighted nothing. One edit either way, so one ⌘Z puts it back — a
    /// format you cannot undo is a format nobody presses twice.
    pub fn format(&mut self, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let selected = self.selections.newest().range();
        let scope = match self.buffer.slice(selected.clone()).trim().is_empty() {
            false => selected,
            true => 0..self.buffer.len(),
        };
        let source = self.buffer.slice(scope.clone());
        let formatted = crate::format::format(&source);
        if formatted == source {
            return;
        }
        // Where the caret was, counted in characters that are not whitespace.
        // The formatter only moves whitespace around, so that count is the one
        // thing about a position it cannot change — which makes it an exact
        // anchor, and means the caret comes out of a format on the same word it
        // went in on rather than at the bottom of the document.
        let head = self.selections.newest().head;
        let anchor = (head >= scope.start && head <= scope.end).then(|| {
            source
                .chars()
                .take(head - scope.start)
                .filter(|c| !c.is_whitespace())
                .count()
        });

        // Through the selection rather than around it: `edit_with` is what
        // records an undo entry and moves the cursors, and a rewrite of the
        // buffer that skipped it would leave both stale.
        self.selections
            .set(vec![Selection::new(scope.start, scope.end)]);
        self.insert(&formatted, cx);

        if let Some(anchor) = anchor {
            // Char offsets throughout, the way the buffer counts. See
            // [`crate::buffer`].
            let mut seen = 0;
            let mut at = formatted.chars().count();
            for (index, c) in formatted.chars().enumerate() {
                if seen == anchor {
                    at = index;
                    break;
                }
                if !c.is_whitespace() {
                    seen += 1;
                }
            }
            let offset = scope.start + at;
            self.selections.replace_with_cursor(offset);
            self.autoscroll = true;
            cx.emit(EditorEvent::SelectionChanged);
            cx.notify();
        }
    }

    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = if self.mode == EditorMode::SingleLine {
            text.replace(['\n', '\r'], " ")
        } else {
            text.to_string()
        };
        let kind = if text.chars().count() == 1 {
            EditKind::Insert
        } else {
            EditKind::Other
        };
        self.edit_with(kind, cx, |_, sel| Some((sel.range(), text.clone())));
    }

    /// Insert one typed character, with bracket and quote handling.
    fn type_char(&mut self, c: char, cx: &mut Context<Self>) {
        let closing = closing_for(c);

        // Typing a closing char that is already there just steps over it —
        // otherwise auto-pairing makes you delete what it inserted.
        if is_closing(c) {
            let all_at_closer = self
                .selections
                .all()
                .iter()
                .all(|s| s.is_empty() && self.buffer.char_at(s.head) == Some(c));
            if all_at_closer {
                self.move_cursors(cx, false, |buffer, sel| movement::right(buffer, sel.head));
                return;
            }
        }

        match closing {
            // Wrap a selection rather than replacing it: selecting a word and
            // hitting `'` should quote the word.
            Some(close) if self.selections.all().iter().any(|s| !s.is_empty()) => {
                let open = c;
                self.edit_with(EditKind::Other, cx, move |buffer, sel| {
                    let inner = buffer.slice(sel.range());
                    Some((sel.range(), format!("{open}{inner}{close}")))
                });
            }
            // Auto-close only before whitespace or another closer; typing `(`
            // in the middle of a word means you are editing that word.
            Some(close) => {
                let should_pair =
                    self.selections
                        .all()
                        .iter()
                        .all(|s| match self.buffer.char_at(s.head) {
                            None => true,
                            Some(next) => next.is_whitespace() || is_closing(next) || next == ',',
                        });
                let text = if should_pair {
                    format!("{c}{close}")
                } else {
                    c.to_string()
                };
                self.edit_with(EditKind::Insert, cx, |_, sel| {
                    Some((sel.range(), text.clone()))
                });
                if should_pair {
                    // Leave the cursor between the pair.
                    self.move_cursors(cx, false, |buffer, sel| movement::left(buffer, sel.head));
                }
            }
            None => {
                let text = c.to_string();
                self.edit_with(EditKind::Insert, cx, |_, sel| {
                    Some((sel.range(), text.clone()))
                });
            }
        }
    }

    pub(crate) fn is_single_line(&self) -> bool {
        self.mode == EditorMode::SingleLine
    }

    pub fn newline(&mut self, cx: &mut Context<Self>) {
        if self.mode == EditorMode::SingleLine {
            cx.emit(EditorEvent::Submit);
            return;
        }
        self.edit_with(EditKind::Other, cx, |buffer, sel| {
            let row = buffer.offset_to_point(sel.start()).row;
            let line = buffer.line(row);
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            // An open bracket at the end of the line earns one more level.
            let extra = if line.trim_end().ends_with('(') {
                "    "
            } else {
                ""
            };
            Some((sel.range(), format!("\n{indent}{extra}")))
        });
    }

    pub fn backspace(&mut self, cx: &mut Context<Self>) {
        self.edit_with(EditKind::Delete, cx, |buffer, sel| {
            if !sel.is_empty() {
                return Some((sel.range(), String::new()));
            }
            if sel.head == 0 {
                return None;
            }
            // Delete both halves of an auto-inserted pair at once.
            let before = buffer.char_at(sel.head - 1);
            let after = buffer.char_at(sel.head);
            if let (Some(open), Some(close)) = (before, after) {
                if closing_for(open) == Some(close) {
                    return Some((sel.head - 1..sel.head + 1, String::new()));
                }
            }
            Some((sel.head - 1..sel.head, String::new()))
        });
    }

    pub fn delete_forward(&mut self, cx: &mut Context<Self>) {
        self.edit_with(EditKind::Delete, cx, |buffer, sel| {
            if !sel.is_empty() {
                return Some((sel.range(), String::new()));
            }
            if sel.head >= buffer.len() {
                return None;
            }
            Some((sel.head..sel.head + 1, String::new()))
        });
    }

    pub fn delete_word_left(&mut self, cx: &mut Context<Self>) {
        self.edit_with(EditKind::Other, cx, |buffer, sel| {
            if !sel.is_empty() {
                return Some((sel.range(), String::new()));
            }
            let start = movement::prev_word_boundary(buffer, sel.head);
            (start < sel.head).then(|| (start..sel.head, String::new()))
        });
    }

    pub fn delete_word_right(&mut self, cx: &mut Context<Self>) {
        self.edit_with(EditKind::Other, cx, |buffer, sel| {
            if !sel.is_empty() {
                return Some((sel.range(), String::new()));
            }
            let end = movement::next_word_boundary(buffer, sel.head);
            (end > sel.head).then(|| (sel.head..end, String::new()))
        });
    }

    pub fn delete_to_line_end(&mut self, cx: &mut Context<Self>) {
        self.edit_with(EditKind::Other, cx, |buffer, sel| {
            let end = movement::line_end(buffer, sel.head);
            // At the end of a line, ⌃K joins the next one.
            let end = if end == sel.head {
                movement::right(buffer, end)
            } else {
                end
            };
            (end > sel.head).then(|| (sel.head..end, String::new()))
        });
    }

    pub fn delete_line(&mut self, cx: &mut Context<Self>) {
        self.edit_with(EditKind::Other, cx, |buffer, sel| {
            let (start, end) = movement::line_range(buffer, sel.head);
            Some((start..end, String::new()))
        });
    }

    pub fn duplicate_line(&mut self, cx: &mut Context<Self>) {
        self.edit_with(EditKind::Other, cx, |buffer, sel| {
            let (start, end) = movement::line_range(buffer, sel.head);
            let mut line = buffer.slice(start..end);
            if !line.ends_with('\n') {
                line.insert(0, '\n');
            }
            Some((end..end, line))
        });
    }

    /// Indent, or insert spaces if there is nothing selected.
    ///
    /// Tab is never a literal tab: a SQL client shows other people's queries,
    /// and a tab character renders at whatever width the next tool decides.
    pub fn tab(&mut self, cx: &mut Context<Self>) {
        let tab_size = self.tab_size;
        if self.selections.all().iter().all(|s| s.is_empty()) {
            self.edit_with(EditKind::Other, cx, |buffer, sel| {
                let column = buffer.offset_to_point(sel.head).column;
                // To the next stop, not a fixed run: pressing tab twice from
                // column 1 should land on the same columns as pressing it once
                // from column 0.
                let width = tab_size - (column % tab_size);
                Some((sel.range(), " ".repeat(width)))
            });
        } else {
            self.indent_selected_lines(tab_size as isize, cx);
        }
    }

    pub fn outdent(&mut self, cx: &mut Context<Self>) {
        self.indent_selected_lines(-(self.tab_size as isize), cx);
    }

    /// How wide one indent step is. Ignored if it is zero: a tab key that
    /// inserts nothing is a broken keyboard, not a preference.
    pub fn set_tab_size(&mut self, tab_size: usize) {
        if tab_size > 0 {
            self.tab_size = tab_size;
        }
    }

    fn indent_selected_lines(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.selected_rows();
        let plans: Vec<(Range<usize>, String)> = rows
            .into_iter()
            .filter_map(|row| {
                let line = self.buffer.line(row);
                let indent = line.chars().take_while(|c| *c == ' ').count();
                let new_indent = (indent as isize + delta).max(0) as usize;
                if new_indent == indent {
                    return None;
                }
                let start = self.buffer.point_to_offset(Point::new(row, 0));
                Some((start..start + indent, " ".repeat(new_indent)))
            })
            .collect();
        self.apply_line_edits(plans, cx);
    }

    /// Toggle a leading `--` on every selected line.
    pub fn toggle_comment(&mut self, cx: &mut Context<Self>) {
        let rows = self.selected_rows();
        let lines: Vec<(usize, String)> = rows
            .iter()
            .map(|row| (*row, self.buffer.line(*row)))
            .collect();
        // If every non-blank line is already commented, this is an uncomment.
        let commenting = lines
            .iter()
            .filter(|(_, line)| !line.trim().is_empty())
            .any(|(_, line)| !line.trim_start().starts_with("--"));

        let plans: Vec<(Range<usize>, String)> = lines
            .into_iter()
            .filter_map(|(row, line)| {
                if line.trim().is_empty() {
                    return None;
                }
                let indent = line.chars().take_while(|c| c.is_whitespace()).count();
                let start = self.buffer.point_to_offset(Point::new(row, indent));
                if commenting {
                    Some((start..start, "-- ".to_string()))
                } else {
                    let rest = &line[crate::buffer::char_to_byte(&line, indent)..];
                    let strip = if rest.starts_with("-- ") { 3 } else { 2 };
                    Some((start..start + strip, String::new()))
                }
            })
            .collect();
        self.apply_line_edits(plans, cx);
    }

    /// Apply edits that were computed per *line* rather than per selection, and
    /// keep the existing selections roughly where they were.
    fn apply_line_edits(&mut self, mut plans: Vec<(Range<usize>, String)>, cx: &mut Context<Self>) {
        if plans.is_empty() || self.read_only {
            return;
        }
        plans.sort_by_key(|(range, _)| range.start);
        let before: Vec<Selection> = self.selections.all().to_vec();

        let shift = |offset: usize, plans: &[(Range<usize>, String)]| -> usize {
            let mut delta: isize = 0;
            for (range, text) in plans {
                if range.start >= offset {
                    break;
                }
                let removed = range
                    .end
                    .saturating_sub(range.start)
                    .min(offset - range.start);
                delta += text.chars().count() as isize - removed as isize;
            }
            (offset as isize + delta).max(0) as usize
        };
        let after: Vec<Selection> = before
            .iter()
            .map(|sel| Selection {
                anchor: shift(sel.anchor, &plans),
                head: shift(sel.head, &plans),
                goal_column: None,
            })
            .collect();

        let mut edits: Vec<Edit> = Vec::with_capacity(plans.len());
        for (range, text) in plans.iter().rev() {
            let old = self.buffer.replace(range.clone(), text);
            edits.push(Edit {
                start: range.start,
                old,
                new: text.clone(),
            });
        }
        edits.reverse();

        self.history
            .push(edits, before, after.clone(), EditKind::Other);
        self.selections.set(after);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    fn selected_rows(&self) -> Vec<usize> {
        let mut rows: Vec<usize> = Vec::new();
        for sel in self.selections.all() {
            let start = self.buffer.offset_to_point(sel.start()).row;
            let end = self.buffer.offset_to_point(sel.end()).row;
            for row in start..=end {
                if rows.last() != Some(&row) {
                    rows.push(row);
                }
            }
        }
        rows.dedup();
        rows
    }

    // ---- movement --------------------------------------------------------

    pub(crate) fn move_cursors(
        &mut self,
        cx: &mut Context<Self>,
        extend: bool,
        f: impl Fn(&Buffer, &Selection) -> usize,
    ) {
        let mut selections = self.selections.all().to_vec();
        for sel in &mut selections {
            // A plain arrow key with a selection active collapses to the
            // relevant edge instead of moving off the head.
            if !extend && !sel.is_empty() {
                let target = f(&self.buffer, sel);
                let collapsed = if target < sel.head {
                    sel.start()
                } else {
                    sel.end()
                };
                let moved = f(&self.buffer, &Selection::cursor(collapsed));
                sel.move_to(
                    if moved == collapsed {
                        target
                    } else {
                        collapsed
                    },
                    false,
                );
                continue;
            }
            let target = f(&self.buffer, sel);
            sel.move_to(target, extend);
        }
        self.selections.set(selections);
        self.history.break_group();
        self.autoscroll = true;
        self.restart_blink(cx);
        cx.emit(EditorEvent::SelectionChanged);
        cx.notify();
    }

    fn move_vertically(&mut self, cx: &mut Context<Self>, extend: bool, down: bool) {
        let mut selections = self.selections.all().to_vec();
        for sel in &mut selections {
            let (target, goal) = if down {
                movement::down(&self.buffer, sel.head, sel.goal_column)
            } else {
                movement::up(&self.buffer, sel.head, sel.goal_column)
            };
            sel.head = target;
            if !extend {
                sel.anchor = target;
            }
            sel.goal_column = goal;
        }
        self.selections.set(selections);
        self.history.break_group();
        self.autoscroll = true;
        self.restart_blink(cx);
        cx.emit(EditorEvent::SelectionChanged);
        cx.notify();
    }

    /// Put the caret at the start of `row`, counting from zero and clamped to
    /// the last line. The palette's `:` mode is what this is for.
    pub fn go_to_line(&mut self, row: usize, cx: &mut Context<Self>) {
        let row = row.min(self.buffer.line_count().saturating_sub(1));
        let offset = self.buffer.point_to_offset(Point::new(row, 0));
        self.selections.set(vec![Selection::new(offset, offset)]);
        self.autoscroll = true;
        self.restart_blink(cx);
        cx.emit(EditorEvent::SelectionChanged);
        cx.notify();
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selections
            .set(vec![Selection::new(0, self.buffer.len())]);
        cx.emit(EditorEvent::SelectionChanged);
        cx.notify();
    }

    pub(crate) fn select_word_at(&mut self, offset: usize, cx: &mut Context<Self>) {
        let (start, end) = movement::word_at(&self.buffer, offset);
        self.selections.set(vec![Selection::new(start, end)]);
        cx.emit(EditorEvent::SelectionChanged);
        cx.notify();
    }

    pub(crate) fn select_line_at(&mut self, offset: usize, cx: &mut Context<Self>) {
        let (start, end) = movement::line_range(&self.buffer, offset);
        self.selections.set(vec![Selection::new(start, end)]);
        cx.emit(EditorEvent::SelectionChanged);
        cx.notify();
    }

    pub(crate) fn place_cursor(&mut self, offset: usize, extend: bool, cx: &mut Context<Self>) {
        let offset = self.buffer.clip_offset(offset);
        if extend {
            let mut sel = self.selections.newest();
            sel.head = offset;
            sel.goal_column = None;
            self.selections.set(vec![sel]);
        } else {
            self.selections.replace_with_cursor(offset);
        }
        self.history.break_group();
        self.autoscroll = true;
        self.restart_blink(cx);
        cx.emit(EditorEvent::SelectionChanged);
        cx.notify();
    }

    // ---- history & clipboard ---------------------------------------------

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        if let Some(selections) = self.history.undo(&mut self.buffer) {
            self.selections.set(selections);
            self.selections.clip(&self.buffer);
            cx.emit(EditorEvent::Changed);
            cx.notify();
        }
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        if let Some(selections) = self.history.redo(&mut self.buffer) {
            self.selections.set(selections);
            self.selections.clip(&self.buffer);
            cx.emit(EditorEvent::Changed);
            cx.notify();
        }
    }

    fn selected_text(&self) -> String {
        self.selections
            .all()
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| self.buffer.slice(s.range()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn copy(&mut self, cx: &mut Context<Self>) {
        // Copying with nothing selected takes the whole line, the way it does
        // in every editor people arrive here from.
        let text = if self.selections.all().iter().all(|s| s.is_empty()) {
            let (start, end) = movement::line_range(&self.buffer, self.selections.newest().head);
            self.buffer.slice(start..end)
        } else {
            self.selected_text()
        };
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub fn cut(&mut self, cx: &mut Context<Self>) {
        self.copy(cx);
        if self.selections.all().iter().all(|s| s.is_empty()) {
            self.delete_line(cx);
        } else {
            self.edit_with(EditKind::Other, cx, |_, sel| {
                (!sel.is_empty()).then(|| (sel.range(), String::new()))
            });
        }
    }

    pub fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        self.insert(&text, cx);
    }

    // ---- keys ------------------------------------------------------------

    pub(crate) fn on_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Typing answers the question the panel was answering, and the text
        // under it is about to move anyway.
        self.close_hover(cx);

        let k = &event.keystroke;
        let m = k.modifiers;
        let extend = m.shift;
        // ⌘ on macOS jumps to a boundary; ⌥ moves by word. Everything below
        // reads `word` and `jump` rather than testing modifiers inline.
        let word = m.alt;
        let jump = m.platform;

        // With the popup open, the keys that drive it are its own: it is a
        // list with the keyboard, not a hint beside the text. Everything else
        // — every character, every other movement — goes to the editor and the
        // list re-narrows or closes underneath.
        if self.completion.is_some() && !jump {
            match k.key.as_str() {
                "up" => return self.move_completion(-1, cx),
                "down" => return self.move_completion(1, cx),
                "escape" => return self.close_completions(cx),
                // ⏎ and ⇥ both accept. Tab is the habit from every other
                // editor; return is the habit from every other list.
                "enter" | "tab" => return self.accept_completion(cx),
                // Movement that is not the list's own has left the word the
                // list was about. The key still does what it does; the popup
                // just stops following it.
                "left" | "right" | "home" | "end" | "pageup" | "pagedown" => {
                    self.close_completions(cx)
                }
                _ => {}
            }
        }
        // ⌃Space asks for the list where nothing has been typed yet, which is
        // how you find out what a table has in it.
        if k.key == "space" && m.control {
            return self.refresh_completions(true, cx);
        }

        match k.key.as_str() {
            "left" if jump => {
                self.move_cursors(cx, extend, |b, s| movement::smart_line_start(b, s.head))
            }
            "left" if word => {
                self.move_cursors(cx, extend, |b, s| movement::prev_word_boundary(b, s.head))
            }
            "left" => self.move_cursors(cx, extend, |b, s| movement::left(b, s.head)),
            "right" if jump => self.move_cursors(cx, extend, |b, s| movement::line_end(b, s.head)),
            "right" if word => {
                self.move_cursors(cx, extend, |b, s| movement::next_word_boundary(b, s.head))
            }
            "right" => self.move_cursors(cx, extend, |b, s| movement::right(b, s.head)),
            "up" if jump => self.move_cursors(cx, extend, |_, _| 0),
            "up" if self.is_single_line() => cx.emit(EditorEvent::Navigate(Direction::Up)),
            "up" => self.move_vertically(cx, extend, false),
            "down" if jump => self.move_cursors(cx, extend, |b, _| b.len()),
            "down" if self.is_single_line() => cx.emit(EditorEvent::Navigate(Direction::Down)),
            "down" => self.move_vertically(cx, extend, true),
            "home" => self.move_cursors(cx, extend, |b, s| movement::smart_line_start(b, s.head)),
            "end" => self.move_cursors(cx, extend, |b, s| movement::line_end(b, s.head)),
            "pageup" if self.is_single_line() => cx.emit(EditorEvent::Navigate(Direction::PageUp)),
            "pageup" => {
                for _ in 0..20 {
                    self.move_vertically(cx, extend, false);
                }
            }
            "pagedown" if self.is_single_line() => {
                cx.emit(EditorEvent::Navigate(Direction::PageDown))
            }
            "pagedown" => {
                for _ in 0..20 {
                    self.move_vertically(cx, extend, true);
                }
            }

            "backspace" if word => self.delete_word_left(cx),
            "backspace" if jump => self.delete_to_line_start(cx),
            "backspace" => {
                self.backspace(cx);
                if self.completion.is_some() {
                    self.refresh_completions(false, cx);
                }
            }
            "delete" if word => self.delete_word_right(cx),
            "delete" => self.delete_forward(cx),
            "k" if m.control => self.delete_to_line_end(cx),

            // ⌘⏎ runs; plain ⏎ types. The host decides what "run" means, so
            // this only reports the gesture.
            "enter" if jump && m.shift => cx.emit(EditorEvent::RunAll),
            "enter" if jump => cx.emit(EditorEvent::Run),
            // ⌘S saves. The `jump` guard below would swallow it anyway, so
            // this has to come first or the gesture never leaves the editor.
            "s" if jump => cx.emit(EditorEvent::Save),
            "enter" => self.newline(cx),
            "escape" => cx.emit(EditorEvent::Cancel),
            "tab" if m.shift => self.outdent(cx),
            "tab" => self.tab(cx),

            "a" if jump => self.select_all(cx),
            "c" if jump => self.copy(cx),
            "x" if jump => self.cut(cx),
            "v" if jump => self.paste(cx),
            "z" if jump && m.shift => self.redo(cx),
            "z" if jump => self.undo(cx),
            "/" if jump => self.toggle_comment(cx),
            "d" if jump && m.shift => self.duplicate_line(cx),
            "k" if jump && m.shift => self.delete_line(cx),

            _ => {
                // Anything the platform turned into a character is text. The
                // modifier check keeps ⌘S from typing an `s`.
                if jump || m.control {
                    return;
                }
                let Some(chars) = k.key_char.as_deref() else {
                    return;
                };
                if chars.is_empty() || chars.chars().any(|c| c.is_control()) {
                    return;
                }
                if chars.chars().count() == 1 {
                    self.type_char(chars.chars().next().unwrap(), cx);
                } else {
                    self.insert(chars, cx);
                }
                self.completions_after_typing(chars, cx);
            }
        }
        cx.stop_propagation();
    }

    /// What the popup does about a character having been typed.
    ///
    /// A word character or a dot re-asks the question; anything else — a space,
    /// a comma, a bracket — has ended the word the popup was about, so the
    /// popup goes. Deletion is handled where deletion happens, for the same
    /// reason: backspacing through a word should keep narrowing, and
    /// backspacing past its start should not leave a list of everything.
    fn completions_after_typing(&mut self, typed: &str, cx: &mut Context<Self>) {
        if self.source.is_none() {
            return;
        }
        let word = typed
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.');
        match word {
            true => self.refresh_completions(false, cx),
            false => self.close_completions(cx),
        }
    }

    fn delete_to_line_start(&mut self, cx: &mut Context<Self>) {
        self.edit_with(EditKind::Other, cx, |buffer, sel| {
            let start = movement::smart_line_start(buffer, sel.head);
            let start = if start == sel.head {
                movement::line_start(buffer, sel.head)
            } else {
                start
            };
            (start < sel.head).then(|| (start..sel.head, String::new()))
        });
    }

    // ---- input handler helpers -------------------------------------------

    pub(crate) fn utf16_selection(&self) -> UTF16Selection {
        let sel = self.selections.newest();
        UTF16Selection {
            range: self.buffer.char_to_utf16(sel.start())..self.buffer.char_to_utf16(sel.end()),
            reversed: sel.head < sel.anchor,
        }
    }

    pub(crate) fn char_range(&self, utf16: Range<usize>) -> Range<usize> {
        self.buffer.utf16_to_char(utf16.start)..self.buffer.utf16_to_char(utf16.end)
    }

    /// Screen bounds of a char range, used to place the IME candidate window.
    ///
    /// Approximate on purpose: it assumes the fixed advance of the mono face
    /// rather than re-shaping. The candidate window only needs to land near the
    /// text, and this runs on a code path that must not allocate.
    pub(crate) fn approximate_bounds(&self, range: Range<usize>) -> Option<Bounds<Pixels>> {
        let layout = self.layout?;
        let start = self.buffer.offset_to_point(range.start);
        let end = self.buffer.offset_to_point(range.end);
        let x = layout.text_origin.x + layout.char_width * start.column as f32;
        let y = layout.text_origin.y + layout.line_height * start.row as f32;
        let width = if start.row == end.row {
            layout.char_width * (end.column.saturating_sub(start.column)) as f32
        } else {
            layout.char_width
        };
        Some(Bounds {
            origin: point(x, y),
            size: gpui::size(width.max(px(1.)), layout.line_height),
        })
    }
}

// ---- hover ---------------------------------------------------------------

/// How long the pointer has to sit still on a name before the panel appears.
///
/// Long enough that crossing a query on the way somewhere else does not flash
/// four panels, short enough that resting on a word feels like it answered
/// rather than like it eventually got round to it.
const HOVER_DELAY: std::time::Duration = std::time::Duration::from_millis(320);

impl Editor {
    /// The pointer is over this offset, or over nothing.
    ///
    /// Public because posing a pointer is the only way to photograph the panel,
    /// and the screenshot harness has no mouse. Called on every mouse move
    /// otherwise, so the work up to the timer has to be cheap:
    /// finding the word is a scan of a few characters, and asking the source
    /// what it means — which may walk the whole statement — waits for the rest.
    pub fn hover_at(&mut self, offset: Option<usize>, cx: &mut Context<Self>) {
        if self.hover_source.is_none() {
            return;
        }
        // Before the text is copied: this runs on every mouse move anywhere in
        // the window, and most of those are over nothing.
        let Some(offset) = offset else {
            return self.close_hover(cx);
        };
        // Still inside the word already answered for, which is where the
        // pointer spends most of its time once it has stopped.
        let inside = |range: &Range<usize>| range.contains(&offset);
        if self.hover.as_ref().is_some_and(|open| inside(&open.range))
            || self.hover_pending.as_ref().is_some_and(inside)
        {
            return;
        }

        let text = self.buffer.text();
        let Some((range, qualifier)) = crate::hover::word_around(&text, offset) else {
            return self.close_hover(cx);
        };
        // Still the same word. Without this the wait would restart on every
        // pixel of a slow drift and the panel would never arrive.
        let showing = self.hover.as_ref().map(|open| &open.range) == Some(&range);
        if showing || self.hover_pending.as_ref() == Some(&range) {
            return;
        }

        self.close_hover(cx);
        self.hover_pending = Some(range.clone());
        let word: String = text.chars().skip(range.start).take(range.len()).collect();
        let wanted = range.clone();
        self.hover_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_DELAY).await;
            this.update(cx, |this, cx| {
                // The pointer has moved on in the meantime, and the answer it
                // waited for is about a word nobody is looking at any more.
                if this.hover_pending.as_ref() != Some(&wanted) {
                    return;
                }
                let context = HoverContext {
                    text,
                    offset: range.start,
                    word,
                    qualifier,
                };
                let info = this
                    .hover_source
                    .as_ref()
                    .and_then(|source| source.hover(&context));
                // Nothing to say is the common case — keywords, literals,
                // names the catalog has never heard of — and it shows nothing
                // rather than an empty box.
                if let Some(info) = info {
                    this.hover = Some(HoverState { range, info });
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Take the panel away, and forget any wait in flight.
    pub fn close_hover(&mut self, cx: &mut Context<Self>) {
        self.hover_pending = None;
        self.hover_task = None;
        if self.hover.take().is_some() {
            cx.notify();
        }
    }
}

// ---- find -----------------------------------------------------------------

impl Editor {
    /// Start a find, change the one that is open, or — with `None` — end it.
    ///
    /// The cursor lands on the first match at or after where it was when the
    /// find began, which is the one the reader is looking for: a find opened
    /// halfway down a script is asking about the half below, not about the
    /// first `select` in the file.
    pub fn set_search(&mut self, search: Option<crate::find::Search>, cx: &mut Context<Self>) {
        let Some(search) = search else {
            if self.find.take().is_some() {
                cx.notify();
            }
            return;
        };
        let origin = match &self.find {
            Some(open) => open.origin,
            None => self.selections.newest().start(),
        };
        self.find = Some(FindState {
            search,
            matches: Vec::new(),
            current: None,
            // Nothing has been searched yet, and a version no buffer can be at
            // is what makes the refresh below do the work.
            version: usize::MAX,
            origin,
        });
        self.refresh_find();
        self.reveal_match(cx);
    }

    pub fn search(&self) -> Option<&crate::find::Search> {
        self.find.as_ref().map(|open| &open.search)
    }

    /// Which match the cursor is on and how many there are, both one-based, or
    /// `None` when the query has found nothing. For the count beside the field.
    pub fn find_status(&self) -> Option<(usize, usize)> {
        let open = self.find.as_ref()?;
        Some((open.current? + 1, open.matches.len()))
    }

    /// Step to the next match, or the previous one, wrapping at either end.
    ///
    /// Wrapping rather than stopping because a find that goes quiet at the last
    /// match looks broken, and because the count beside the field already says
    /// where in the list you are.
    pub fn find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.refresh_find();
        let Some(open) = self.find.as_mut() else {
            return;
        };
        let count = open.matches.len();
        if count == 0 {
            return;
        }
        open.current = Some(match open.current {
            Some(at) if forward => (at + 1) % count,
            Some(at) => (at + count - 1) % count,
            None => 0,
        });
        self.reveal_match(cx);
    }

    /// Recompute the matches if the text has moved under them.
    ///
    /// Pulled from the frame for the same reason [`Editor::refresh_highlights`]
    /// is: there are a dozen ways to change a buffer and one way to draw it.
    pub(crate) fn refresh_find(&mut self) {
        let version = self.buffer.version();
        let Some(open) = self.find.as_mut() else {
            return;
        };
        if open.version == version {
            return;
        }
        open.matches = open.search.find_all(&self.buffer.text());
        open.version = version;
        // Whichever match the reader was on has almost certainly moved. Going
        // back to the one at the origin is the only answer that does not
        // depend on how the text was edited.
        let at_origin = open.matches.iter().position(|m| m.start >= open.origin);
        open.current = match at_origin {
            Some(index) => Some(index),
            // Every match is behind the origin, so the search wraps.
            None => (!open.matches.is_empty()).then_some(0),
        };
    }

    /// Select the current match and scroll it into view.
    fn reveal_match(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.find.as_ref() else {
            return;
        };
        let Some(range) = open.current.and_then(|at| open.matches.get(at)).cloned() else {
            cx.notify();
            return;
        };
        self.selections
            .set(vec![Selection::new(range.start, range.end)]);
        self.autoscroll = true;
        self.restart_blink(cx);
        cx.emit(EditorEvent::SelectionChanged);
        cx.notify();
    }

    /// What ⌘F should put in the field: the selection, when there is exactly
    /// one and it is on one line. The text someone has just highlighted is
    /// nearly always the text they are about to go looking for.
    pub fn find_seed(&self) -> Option<String> {
        if self.selections.len() != 1 {
            return None;
        }
        let sel = self.selections.newest();
        if sel.is_empty() {
            return None;
        }
        let text = self.buffer.slice(sel.range());
        (!text.contains('\n')).then_some(text)
    }
}

/// The closing half of a pair, if `c` opens one.
///
/// Backticks are not here: Postgres does not use them, and a `` ` `` in a query
/// is far more likely to be inside a string than to be opening anything.
fn closing_for(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '\'' => Some('\''),
        '"' => Some('"'),
        _ => None,
    }
}

fn is_closing(c: char) -> bool {
    matches!(c, ')' | ']' | '}' | '\'' | '"')
}

// ---- scrolling ------------------------------------------------------------

impl Editor {
    /// Recompute the longest line, but only when the text has changed.
    pub(crate) fn refresh_longest_line(&mut self) {
        if self.longest_line.0 == self.buffer.version() {
            return;
        }
        let longest = (0..self.buffer.line_count())
            .map(|row| self.buffer.line_len(row))
            .max()
            .unwrap_or(0);
        self.longest_line = (self.buffer.version(), longest);
    }

    /// Show the highlighter the text, but only when the text has changed.
    ///
    /// Pulled from the frame rather than pushed from every edit: there are a
    /// dozen ways to change a buffer and one way to draw it, and a parse that
    /// nothing is going to look at is a parse not worth doing.
    pub(crate) fn refresh_highlights(&mut self) {
        let version = self.buffer.version();
        if self.highlighted == Some(version) {
            return;
        }
        if let Some(highlighter) = self.highlighter.as_mut() {
            highlighter.refresh(&self.buffer.text());
        }
        self.highlighted = Some(version);
    }

    /// How far the content can scroll past the viewport, per axis.
    ///
    /// A trailing screen of blank space below the last line is deliberately not
    /// offered: this is a query console, not a document, and scrolling code off
    /// the top to stare at nothing is not a gesture anyone wants here.
    pub(crate) fn max_scroll(
        &self,
        viewport: Size<Pixels>,
        line_height: Pixels,
        char_width: Pixels,
    ) -> gpui::Point<Pixels> {
        let content_h = line_height * self.buffer.line_count() as f32;
        let content_w = char_width * self.longest_line.1 as f32 + char_width;
        point(
            (content_w - viewport.width).max(px(0.)),
            match self.mode {
                EditorMode::SingleLine => px(0.),
                EditorMode::Full => (content_h - viewport.height).max(px(0.)),
            },
        )
    }

    pub(crate) fn scroll_by(&mut self, delta: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        // Clamping happens in prepaint, which is the only place that knows the
        // viewport; here we only need to refuse to go negative so a flick at the
        // top does not bank up a large positive number to unwind later.
        self.scroll.x = (self.scroll.x - delta.x).max(px(0.));
        self.scroll.y = (self.scroll.y - delta.y).max(px(0.));
        // The panel is anchored to a word that has just moved out from under
        // the pointer, so it is now pointing at whatever slid into its place.
        self.close_hover(cx);
        cx.notify();
    }

    /// Bring the cursor back on screen, moving as little as possible.
    pub(crate) fn scroll_cursor_into_view(
        &mut self,
        viewport: Size<Pixels>,
        line_height: Pixels,
        char_width: Pixels,
    ) {
        let cursor = self.buffer.offset_to_point(self.selections.newest().head);
        // Three lines of margin: seeing the cursor is not the same as seeing
        // what it is next to.
        let margin = line_height * 3.;
        let top = line_height * cursor.row as f32;
        if top < self.scroll.y + margin {
            self.scroll.y = (top - margin).max(px(0.));
        } else if top + line_height > self.scroll.y + viewport.height - margin {
            self.scroll.y = top + line_height + margin - viewport.height;
        }

        let x = char_width * cursor.column as f32;
        let margin = char_width * 4.;
        if x < self.scroll.x + margin {
            self.scroll.x = (x - margin).max(px(0.));
        } else if x + char_width > self.scroll.x + viewport.width - margin {
            self.scroll.x = x + char_width + margin - viewport.width;
        }
    }

    // ---- caret blink -----------------------------------------------------

    /// Called from prepaint with what the window thinks.
    ///
    /// Focus is window state, not model state, so the model finds out at paint
    /// time. The transition is what matters: it starts and stops the blink and
    /// breaks the undo group, both of which must happen exactly once.
    pub(crate) fn sync_focus(&mut self, focused: bool, cx: &mut Context<Self>) {
        if focused == self.focused {
            return;
        }
        self.focused = focused;
        if focused {
            self.restart_blink(cx);
        } else {
            self.blink_epoch += 1;
            self.blink = None;
            self.cursor_visible = true;
            self.history.break_group();
            // A popup belonging to an editor nobody is typing in is a menu
            // floating over the window with nothing behind it.
            self.completion = None;
            self.hover = None;
            self.hover_pending = None;
            self.hover_task = None;
        }
        cx.notify();
    }

    /// Show the caret and restart the phase.
    ///
    /// Every keystroke calls this, which is the point: a caret that keeps
    /// blinking through a burst of typing is the thing that makes a hand-rolled
    /// editor feel hand-rolled.
    pub(crate) fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = true;
        if !self.focused {
            self.blink = None;
            return;
        }
        self.blink_epoch += 1;
        let epoch = self.blink_epoch;
        self.blink = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(BLINK).await;
            let alive = this
                .update(cx, |this, cx| {
                    if this.blink_epoch != epoch || !this.focused {
                        return false;
                    }
                    this.cursor_visible = !this.cursor_visible;
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !alive {
                return;
            }
        }));
    }
}

// ---- input methods --------------------------------------------------------

impl gpui::EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.char_range(range_utf16.clone());
        let clipped = range.start.min(self.buffer.len())..range.end.min(self.buffer.len());
        if clipped != range {
            *adjusted = Some(
                self.buffer.char_to_utf16(clipped.start)..self.buffer.char_to_utf16(clipped.end),
            );
        }
        Some(self.buffer.slice(clipped))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(self.utf16_selection())
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let marked = self.marked.clone()?;
        Some(self.buffer.char_to_utf16(marked.start)..self.buffer.char_to_utf16(marked.end))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        // The range the system gives us wins; failing that, whatever is marked;
        // failing that, the selection. That order is what makes a half-composed
        // Japanese syllable replace itself instead of accumulating.
        let range = range_utf16
            .map(|r| self.char_range(r))
            .or_else(|| self.marked.clone());
        match range {
            Some(range) => {
                let text = text.to_string();
                self.edit_with(EditKind::Other, cx, move |_, _| {
                    Some((range.clone(), text.clone()))
                });
            }
            None => self.insert(text, cx),
        }
        self.marked = None;
        self.autoscroll = true;
        self.restart_blink(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let range = range_utf16
            .map(|r| self.char_range(r))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selections.newest().range());
        let start = range.start;
        let text = new_text.to_string();
        self.edit_with(EditKind::Other, cx, move |_, _| {
            Some((range.clone(), text.clone()))
        });

        let end = start + new_text.chars().count();
        self.marked = (start != end).then(|| start..end);
        if let Some(selected) = new_selected_range {
            // Offsets in `new_selected_range` are relative to the text just
            // inserted, not to the document.
            let inserted = self.buffer.slice(start..end);
            let to_char = |utf16: usize| {
                let mut units = 0;
                for (i, c) in inserted.chars().enumerate() {
                    if units >= utf16 {
                        return i;
                    }
                    units += c.len_utf16();
                }
                inserted.chars().count()
            };
            self.selections.set(vec![Selection::new(
                start + to_char(selected.start),
                start + to_char(selected.end),
            )]);
        }
        self.autoscroll = true;
        self.restart_blink(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.char_range(range_utf16);
        self.approximate_bounds(range)
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let layout = self.layout?;
        let row = (f32::from(point.y - layout.text_origin.y) / f32::from(layout.line_height))
            .floor()
            .max(0.) as usize;
        let column = (f32::from(point.x - layout.text_origin.x) / f32::from(layout.char_width))
            .round()
            .max(0.) as usize;
        let offset = self.buffer.point_to_offset(Point::new(row, column));
        Some(self.buffer.char_to_utf16(offset))
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.buffer.len_utf16())
    }

    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        !self.read_only
    }
}

// ---- render ---------------------------------------------------------------

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = self
            .style
            .clone()
            .unwrap_or_else(|| crate::element::EditorStyle::mono(cx));
        let popup = self.render_completions(window, cx);
        let hover = self.render_hover(cx);
        div()
            .id("editor")
            .track_focus(&self.focus)
            .key_context("Editor")
            .relative()
            .size_full()
            .cursor_text()
            .on_key_down(cx.listener(Self::on_key))
            .child(crate::element::EditorElement::new(cx.entity(), style))
            .children(popup)
            .children(hover)
    }
}

/// How wide the completion popup is. Fixed rather than measured: a list whose
/// edge moves as the longest name in it changes reads as flicker, and the
/// widest thing on offer is usually a type name nobody is aiming at.
const POPUP_WIDTH: f32 = 320.;

/// How wide the hover panel is. Wider than the completion popup, because a
/// column's type and its default are read as sentences rather than scanned as
/// a list, and wrapping them costs more than the width does.
const HOVER_WIDTH: f32 = 380.;

impl Editor {
    /// The completion popup, anchored under the word it is completing.
    ///
    /// Positioned from the last painted layout rather than from a measured
    /// element: the geometry is already known to the pixel, and an anchored
    /// overlay that has to be laid out first would arrive a frame late — which
    /// on a list that moves with every keystroke is visible as a lag behind the
    /// caret.
    fn render_completions(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let open = self.completion.as_ref()?;
        let layout = self.layout?;
        let c = cx.colors().clone();
        let m = cx.metrics().clone();
        let ty = cx.typography().clone();

        let caret = self.approximate_bounds(open.range.start..open.range.start)?;
        // Local to the editor, which is what an absolutely positioned child is
        // measured from.
        let x = (caret.origin.x - layout.bounds.origin.x).max(px(0.));
        // Kept inside the editor. A caret near the right margin would otherwise
        // put most of the list past the edge of the pane, where the half of a
        // name that is showing is the half nobody needs to read.
        let x = x
            .min(layout.bounds.size.width - px(POPUP_WIDTH) - px(8.))
            .max(px(0.));
        // The room is the window's, not the editor's: the list is deferred and
        // draws over whatever is beneath it, so the only edges it has to
        // respect are the window's. Measuring against a three-line console
        // instead is what makes a list flip to a place there is no room in
        // either, and land on the word being typed.
        let wanted = (open.items.len() as f32 * f32::from(m.row_height) + 8.).min(260.);
        let margin = 8.;
        let line_top = f32::from(caret.origin.y);
        let line_bottom = line_top + f32::from(layout.line_height);
        let below = f32::from(window.viewport_size().height) - line_bottom - margin;
        let above = line_top - margin;
        // Under the line by preference, over it when the list does not fit
        // under, and squeezed into the taller side when it fits neither. Never
        // across the line itself: the letters being typed are what is choosing
        // what is in the list, and a list that covers them is one you have to
        // dismiss to find out what you have written.
        let (top, height) = match (below >= wanted, above >= wanted) {
            (true, _) => (line_bottom, wanted),
            (false, true) => (line_top - wanted, wanted),
            (false, false) if above > below => (line_top - above.max(0.), above.max(0.)),
            (false, false) => (line_bottom, below.max(0.)),
        };
        let height = height.max(f32::from(m.row_height) + 8.);
        let y = px(top - f32::from(layout.bounds.origin.y));

        let selected = open.selected;
        let rows: Vec<_> = open
            .items
            .iter()
            .enumerate()
            .map(|(ix, item)| {
                let row = ui::ListItem::new(("completion", ix), item.label.clone())
                    .icon(item.kind.icon())
                    .icon_color(item.kind.color())
                    .mono()
                    .selected(ix == selected)
                    .height(m.row_height);
                let row = match item.detail.clone() {
                    Some(detail) => row.meta(detail),
                    None => row,
                };
                row.on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(open) = this.completion.as_mut() {
                        open.selected = ix;
                    }
                    this.accept_completion(cx);
                }))
            })
            .collect();

        Some(
            gpui::deferred(
                div()
                    .absolute()
                    // Solid to the mouse. A hitbox that does not block leaves
                    // the editor underneath thinking it was clicked, and the
                    // editor's answer to a click is to close the popup — which
                    // takes the row out from under the pointer before the
                    // button comes up, so the click never lands on anything.
                    .occlude()
                    .left(px(f32::from(x)))
                    .top(y)
                    .w(px(POPUP_WIDTH))
                    .max_h(px(height))
                    .overflow_hidden()
                    .rounded(m.radius)
                    .bg(c.overlay)
                    .border_1()
                    .border_color(c.border_strong)
                    .shadow_lg()
                    .py(px(4.))
                    .text_size(ty.ui_size_sm)
                    .child(
                        div()
                            .id("completions")
                            .max_h(px(height - 8.))
                            .overflow_y_scroll()
                            .track_scroll(&open.scroll)
                            .children(rows),
                    ),
            )
            // Above the grid and the panel splitters, which are the only other
            // things in the window that draw over the editor.
            .with_priority(1),
        )
    }

    /// The hover panel, anchored over the word it describes.
    ///
    /// Above the line by preference, unlike the completion popup: the pointer
    /// is sitting on the word, and a panel below it would be a panel the hand
    /// is covering.
    fn render_hover(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        // One popup at a time. They answer different questions but they would
        // be drawn in the same place, and the one the caret is driving wins.
        if self.completion.is_some() {
            return None;
        }
        let open = self.hover.as_ref()?;
        let layout = self.layout?;
        let c = cx.colors().clone();
        let m = cx.metrics().clone();
        let ty = cx.typography().clone();
        let info = open.info.clone();

        let word = self.approximate_bounds(open.range.clone())?;
        let x = (word.origin.x - layout.bounds.origin.x).max(px(0.));
        let x = x
            .min(layout.bounds.size.width - px(HOVER_WIDTH) - px(8.))
            .max(px(0.));
        // Estimated rather than measured, and only to choose a side: an
        // anchored overlay that has to be laid out first arrives a frame late,
        // and being a few pixels out about which half of the pane has room is
        // not a mistake anybody can see.
        let height = 34.
            + info.subtitle.is_some() as u8 as f32 * 20.
            + info.rows.len() as f32 * 18.
            + info.doc.is_some() as u8 as f32 * 34.;
        let top = word.origin.y - layout.bounds.origin.y;
        let y = match f32::from(top) < height + 6. {
            true => top + layout.line_height + px(4.),
            false => px(f32::from(top) - height - 6.),
        };

        let rows: Vec<_> = info
            .rows
            .iter()
            .map(|(label, value)| {
                div()
                    .flex()
                    .gap_2()
                    // A column rather than free-running text: two or three of
                    // these get scanned down the left edge, and a ragged one
                    // has to be read.
                    .child(
                        div()
                            .w(px(68.))
                            .flex_none()
                            .text_color(c.text_subtle)
                            .child(label.clone()),
                    )
                    .child(div().text_color(c.text_muted).child(value.clone()))
            })
            .collect();

        Some(
            gpui::deferred(
                div()
                    .absolute()
                    .left(px(f32::from(x)))
                    .top(px(f32::from(y)))
                    // Wide enough for the longest line and no wider: a fixed
                    // width is right for the completion list, whose rows are
                    // all the same shape, and wrong here, where three words
                    // about a column would leave two thirds of the box empty.
                    .max_w(px(HOVER_WIDTH))
                    .rounded(m.radius)
                    .bg(c.overlay)
                    .border_1()
                    .border_color(c.border_strong)
                    .shadow_lg()
                    .p(px(8.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_size(ty.ui_size_sm)
                    .text_color(c.text)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                ui::Icon::new(info.kind.icon())
                                    .size(ui::IconSize::XSmall)
                                    .color(info.kind.color()),
                            )
                            .child(
                                div()
                                    .font_family(ty.mono_family.clone())
                                    .child(info.title.clone()),
                            )
                            .child(
                                div()
                                    .text_color(c.text_subtle)
                                    .child(SharedString::from(info.kind.label())),
                            ),
                    )
                    .children(info.subtitle.clone().map(|subtitle| {
                        div()
                            .font_family(ty.mono_family.clone())
                            .text_color(c.text_muted)
                            .child(subtitle)
                    }))
                    .children(match rows.is_empty() {
                        true => None,
                        false => Some(div().flex().flex_col().gap_0p5().children(rows)),
                    })
                    .children(info.doc.clone().map(|doc| {
                        div()
                            .pt_1()
                            .border_t_1()
                            .border_color(c.border)
                            .text_color(c.text_muted)
                            .child(doc)
                    })),
            )
            .with_priority(1),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, TestAppContext};

    fn new_editor(cx: &mut TestAppContext, text: &str) -> gpui::Entity<Editor> {
        let editor = cx.update(|cx| cx.new(|cx| Editor::new(EditorMode::Full, cx)));
        editor.update(cx, |editor, cx| {
            editor.set_text(text, cx);
            editor
                .selections
                .set(vec![Selection::cursor(editor.buffer.len())]);
        });
        editor
    }

    fn type_str(editor: &gpui::Entity<Editor>, cx: &mut TestAppContext, s: &str) {
        editor.update(cx, |editor, cx| {
            for c in s.chars() {
                editor.type_char(c, cx);
            }
        });
    }

    /// Records every text the editor hands it, so a test can see whether the
    /// version gate let a reparse through.
    struct Recorder(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    impl Highlight for Recorder {
        fn refresh(&mut self, text: &str) {
            self.0.lock().unwrap().push(text.to_string());
        }

        fn row(
            &self,
            _row: usize,
            _line: &str,
            _syntax: &SyntaxTheme,
        ) -> Vec<(Range<usize>, gpui::Hsla)> {
            Vec::new()
        }
    }

    // ---- find ------------------------------------------------------------

    /// The cursor after a find, as `start..end`.
    fn selected(editor: &gpui::Entity<Editor>, cx: &mut TestAppContext) -> Range<usize> {
        editor.update(cx, |editor, _| {
            let sel = editor.selections.newest();
            sel.start()..sel.end()
        })
    }

    #[gpui::test]
    fn a_find_lands_on_the_first_hit_after_where_the_cursor_was(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "select id from t where id = 1");
        editor.update(cx, |editor, cx| {
            editor.place_cursor(10, false, cx);
            editor.set_search(Some(crate::find::Search::new("id")), cx);
            assert_eq!(editor.find_status(), Some((2, 2)));
        });
        assert_eq!(selected(&editor, cx), 23..25);
    }

    /// Typing into the field walks the query, not the results: every keystroke
    /// searches again from where the find opened.
    #[gpui::test]
    fn narrowing_the_query_does_not_walk_forward_a_hit_at_a_time(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "select a, select b, select c");
        editor.update(cx, |editor, cx| {
            editor.place_cursor(0, false, cx);
            editor.set_search(Some(crate::find::Search::new("s")), cx);
            editor.set_search(Some(crate::find::Search::new("se")), cx);
            editor.set_search(Some(crate::find::Search::new("sel")), cx);
            assert_eq!(editor.find_status(), Some((1, 3)));
        });
        assert_eq!(selected(&editor, cx), 0..3);
    }

    #[gpui::test]
    fn stepping_past_the_last_hit_wraps_to_the_first(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "a a a");
        editor.update(cx, |editor, cx| {
            editor.place_cursor(0, false, cx);
            editor.set_search(Some(crate::find::Search::new("a")), cx);
            editor.find_step(true, cx);
            editor.find_step(true, cx);
            assert_eq!(editor.find_status(), Some((3, 3)));
            editor.find_step(true, cx);
            assert_eq!(editor.find_status(), Some((1, 3)));
        });
        assert_eq!(selected(&editor, cx), 0..1);
    }

    #[gpui::test]
    fn stepping_back_from_the_first_hit_wraps_to_the_last(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "a a a");
        editor.update(cx, |editor, cx| {
            editor.place_cursor(0, false, cx);
            editor.set_search(Some(crate::find::Search::new("a")), cx);
            editor.find_step(false, cx);
            assert_eq!(editor.find_status(), Some((3, 3)));
        });
    }

    /// Editing under an open find is not an error; the hits move with the text.
    #[gpui::test]
    fn changing_the_text_re_finds_it(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "id");
        editor.update(cx, |editor, cx| {
            editor.place_cursor(0, false, cx);
            editor.set_search(Some(crate::find::Search::new("id")), cx);
            assert_eq!(editor.find_status(), Some((1, 1)));
            editor.set_text("id and id", cx);
            editor.refresh_find();
            assert_eq!(editor.find_status(), Some((1, 2)));
        });
    }

    #[gpui::test]
    fn a_query_nothing_matches_has_no_status_at_all(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "select 1");
        editor.update(cx, |editor, cx| {
            editor.set_search(Some(crate::find::Search::new("nobody")), cx);
            assert_eq!(editor.find_status(), None);
        });
    }

    #[gpui::test]
    fn closing_the_find_leaves_nothing_highlighted(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "select 1");
        editor.update(cx, |editor, cx| {
            editor.set_search(Some(crate::find::Search::new("select")), cx);
            editor.set_search(None, cx);
            assert!(editor.find.is_none());
            assert_eq!(editor.find_status(), None);
        });
    }

    /// ⌘F seeds the field from the selection, which is nearly always the word
    /// somebody has just double-clicked.
    #[gpui::test]
    fn a_one_line_selection_is_what_the_field_starts_with(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "select id\nfrom t");
        editor.update(cx, |editor, _| {
            editor.selections.set(vec![Selection::new(7, 9)]);
            assert_eq!(editor.find_seed().as_deref(), Some("id"));

            // A selection spanning lines is a block of text, not a word: using
            // it as a query would find nothing and look broken.
            editor.selections.set(vec![Selection::new(0, 12)]);
            assert_eq!(editor.find_seed(), None);

            editor.selections.set(vec![Selection::cursor(3)]);
            assert_eq!(editor.find_seed(), None);
        });
    }

    #[gpui::test]
    fn replacing_the_text_wholesale_reparses_it(cx: &mut TestAppContext) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let editor = new_editor(cx, "select 1");
        editor.update(cx, |editor, _| {
            editor.set_highlighter(Box::new(Recorder(seen.clone())));
        });

        editor.update(cx, |editor, _| {
            editor.refresh_highlights();
            // Twice in a row without an edit in between is one parse: that gate
            // is the whole point of the version.
            editor.refresh_highlights();
        });
        assert_eq!(seen.lock().unwrap().len(), 1);

        // `set_text` drops the old buffer for a new one. A version counter that
        // restarted here would leave the parse describing the previous text.
        editor.update(cx, |editor, cx| {
            editor.set_text("select id from t", cx);
            editor.refresh_highlights();
        });
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["select 1".to_string(), "select id from t".to_string()]
        );
    }

    #[gpui::test]
    fn typing_then_undo_removes_the_whole_run(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "");
        type_str(&editor, cx, "select");
        editor.update(cx, |editor, cx| {
            assert_eq!(editor.text(), "select");
            editor.undo(cx);
            assert_eq!(editor.text(), "");
            editor.redo(cx);
            assert_eq!(editor.text(), "select");
        });
    }

    #[gpui::test]
    fn brackets_close_themselves_only_where_it_helps(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "");
        type_str(&editor, cx, "count(");
        editor.update(cx, |editor, _| assert_eq!(editor.text(), "count()"));

        // Typing the closer that is already there steps over it rather than
        // producing `))`.
        type_str(&editor, cx, "*)");
        editor.update(cx, |editor, _| {
            assert_eq!(editor.text(), "count(*)");
            assert_eq!(editor.selections.newest().head, 8);
        });

        // In the middle of a word, `(` is just a character.
        let editor = new_editor(cx, "abc");
        editor.update(cx, |editor, cx| {
            editor.selections.set(vec![Selection::cursor(1)]);
            editor.type_char('(', cx);
            assert_eq!(editor.text(), "a(bc");
        });
    }

    #[gpui::test]
    fn a_quote_wraps_the_selection(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "hello");
        editor.update(cx, |editor, cx| {
            editor.select_all(cx);
            editor.type_char('\'', cx);
            assert_eq!(editor.text(), "'hello'");
        });
    }

    #[gpui::test]
    fn backspace_removes_both_halves_of_a_pair(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "");
        type_str(&editor, cx, "(");
        editor.update(cx, |editor, cx| {
            assert_eq!(editor.text(), "()");
            editor.backspace(cx);
            assert_eq!(editor.text(), "");
        });
    }

    #[gpui::test]
    fn tab_goes_to_the_next_stop(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "ab");
        editor.update(cx, |editor, cx| {
            editor.tab(cx);
            assert_eq!(editor.text(), "ab  ");
            editor.tab(cx);
            assert_eq!(editor.text(), "ab      ");
        });
    }

    #[gpui::test]
    fn a_narrower_indent_step_moves_the_stops(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "abc");
        editor.update(cx, |editor, cx| {
            editor.set_tab_size(2);
            editor.tab(cx);
            assert_eq!(editor.text(), "abc ");
            editor.tab(cx);
            assert_eq!(editor.text(), "abc   ");
        });
    }

    #[gpui::test]
    fn an_indent_step_of_zero_is_refused(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "ab");
        editor.update(cx, |editor, cx| {
            editor.set_tab_size(0);
            editor.tab(cx);
            assert_eq!(editor.text(), "ab  ");
        });
    }

    #[gpui::test]
    fn outdent_takes_back_one_step(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "      select 1");
        editor.update(cx, |editor, cx| {
            editor.select_all(cx);
            editor.outdent(cx);
            assert_eq!(editor.text(), "  select 1");
            editor.set_tab_size(2);
            editor.outdent(cx);
            assert_eq!(editor.text(), "select 1");
        });
    }

    #[gpui::test]
    fn comments_toggle_over_the_selected_lines(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "select 1\n\n  select 2");
        editor.update(cx, |editor, cx| {
            editor.select_all(cx);
            editor.toggle_comment(cx);
            assert_eq!(editor.text(), "-- select 1\n\n  -- select 2");
            editor.toggle_comment(cx);
            assert_eq!(editor.text(), "select 1\n\n  select 2");
        });
    }

    #[gpui::test]
    fn a_new_line_keeps_the_indent(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "    where id = 1");
        editor.update(cx, |editor, cx| {
            editor.newline(cx);
            assert_eq!(editor.text(), "    where id = 1\n    ");
        });
    }

    #[gpui::test]
    fn an_open_bracket_earns_an_extra_level(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "  values (");
        editor.update(cx, |editor, cx| {
            editor.newline(cx);
            assert_eq!(editor.text(), "  values (\n      ");
        });
    }

    #[gpui::test]
    fn word_delete_stops_at_the_boundary(cx: &mut TestAppContext) {
        let editor = new_editor(cx, "select created_at");
        editor.update(cx, |editor, cx| {
            editor.delete_word_left(cx);
            assert_eq!(editor.text(), "select ");
        });
    }
}
