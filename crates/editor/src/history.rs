//! Undo, redo, and the rule for what counts as one step.
//!
//! Every mutation goes through a [`Transaction`], including the ones that look
//! atomic. That is the whole trick: undo has exactly one mechanism to
//! implement, so a multi-cursor delete and a single backspace unwind through
//! the same code and there is no operation that quietly forgets to be undoable.

use std::ops::Range;
use std::time::{Duration, Instant};

use crate::buffer::Buffer;
use crate::selection::Selection;

/// How long a run of typing stays one undo step.
const COALESCE_WINDOW: Duration = Duration::from_millis(300);

/// One replacement, recorded with both sides so it can be run either way.
#[derive(Clone, Debug)]
pub struct Edit {
    /// Char offset the replacement starts at.
    pub start: usize,
    pub old: String,
    pub new: String,
}

impl Edit {
    fn old_range(&self) -> Range<usize> {
        self.start..self.start + self.old.chars().count()
    }

    fn new_range(&self) -> Range<usize> {
        self.start..self.start + self.new.chars().count()
    }
}

/// What kind of change this was, for the purpose of grouping.
///
/// Typing coalesces with typing and deleting with deleting; anything else
/// starts a fresh step. Mixing them would make one ⌘Z swallow both the word you
/// typed and the word you deleted before it, which is never what was meant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Insert,
    Delete,
    Other,
}

#[derive(Clone, Debug)]
pub struct Transaction {
    pub edits: Vec<Edit>,
    pub before: Vec<Selection>,
    pub after: Vec<Selection>,
    kind: EditKind,
    at: Instant,
}

#[derive(Default)]
pub struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
}

impl History {
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Record a transaction, folding it into the previous one when it is a
    /// continuation of the same gesture.
    pub fn push(
        &mut self,
        edits: Vec<Edit>,
        before: Vec<Selection>,
        after: Vec<Selection>,
        kind: EditKind,
    ) {
        self.redo.clear();
        let now = Instant::now();

        if kind != EditKind::Other {
            if let Some(last) = self.undo.last_mut() {
                if last.kind == kind
                    && now.duration_since(last.at) < COALESCE_WINDOW
                    && continues(&last.edits, &edits, kind)
                {
                    last.edits.extend(edits);
                    last.after = after;
                    last.at = now;
                    return;
                }
            }
        }

        self.undo.push(Transaction {
            edits,
            before,
            after,
            kind,
            at: now,
        });
    }

    /// Undo one step, applying it to `buffer`, and hand back the selections to
    /// restore.
    pub fn undo(&mut self, buffer: &mut Buffer) -> Option<Vec<Selection>> {
        let tx = self.undo.pop()?;
        for edit in tx.edits.iter().rev() {
            buffer.replace(edit.new_range(), &edit.old);
        }
        let selections = tx.before.clone();
        self.redo.push(tx);
        Some(selections)
    }

    pub fn redo(&mut self, buffer: &mut Buffer) -> Option<Vec<Selection>> {
        let tx = self.redo.pop()?;
        for edit in &tx.edits {
            buffer.replace(edit.old_range(), &edit.new);
        }
        let selections = tx.after.clone();
        self.undo.push(tx);
        Some(selections)
    }

    /// Force the next edit to start a new step.
    ///
    /// Called when the cursor is moved or the editor loses focus: coming back
    /// and typing again is a new thought, not a continuation of the old one.
    pub fn break_group(&mut self) {
        if let Some(last) = self.undo.last_mut() {
            last.kind = EditKind::Other;
        }
    }
}

/// Does `next` pick up exactly where `prev` left off?
///
/// Only single-caret runs coalesce: `next` must be one edit, and it is compared
/// against the last edit already in the group. With several cursors the offsets
/// interleave in ways that are not worth reasoning about for a feature whose
/// whole benefit is saving a few ⌘Z presses.
fn continues(prev: &[Edit], next: &[Edit], kind: EditKind) -> bool {
    if next.len() != 1 {
        return false;
    }
    let (Some(prev), Some(next)) = (prev.last(), next.first()) else {
        return false;
    };
    match kind {
        // Typing: the new text starts where the old text ended.
        EditKind::Insert => {
            next.old.is_empty()
                && prev.old.is_empty()
                && next.start == prev.start + prev.new.chars().count()
                && !next.new.contains('\n')
        }
        // Backspacing: the new deletion ends where the old one began.
        EditKind::Delete => {
            next.new.is_empty()
                && prev.new.is_empty()
                && (next.start + next.old.chars().count() == prev.start || next.start == prev.start)
        }
        EditKind::Other => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(history: &mut History, buffer: &mut Buffer, at: usize, text: &str) {
        let old = buffer.replace(at..at, text);
        history.push(
            vec![Edit {
                start: at,
                old,
                new: text.into(),
            }],
            vec![Selection::cursor(at)],
            vec![Selection::cursor(at + text.chars().count())],
            EditKind::Insert,
        );
    }

    #[test]
    fn a_run_of_typing_undoes_as_one_step() {
        let mut b = Buffer::new("");
        let mut h = History::default();
        for (i, c) in "hello".chars().enumerate() {
            typed(&mut h, &mut b, i, &c.to_string());
        }
        assert_eq!(b.text(), "hello");
        h.undo(&mut b);
        assert_eq!(b.text(), "");
        assert!(!h.can_undo());
    }

    #[test]
    fn redo_replays_it() {
        let mut b = Buffer::new("");
        let mut h = History::default();
        typed(&mut h, &mut b, 0, "abc");
        h.undo(&mut b);
        assert_eq!(b.text(), "");
        let selections = h.redo(&mut b).unwrap();
        assert_eq!(b.text(), "abc");
        assert_eq!(selections[0].head, 3);
    }

    #[test]
    fn a_cursor_jump_breaks_the_group() {
        let mut b = Buffer::new("");
        let mut h = History::default();
        typed(&mut h, &mut b, 0, "ab");
        h.break_group();
        typed(&mut h, &mut b, 2, "cd");
        h.undo(&mut b);
        assert_eq!(b.text(), "ab");
    }

    #[test]
    fn a_new_edit_discards_the_redo_stack() {
        let mut b = Buffer::new("");
        let mut h = History::default();
        typed(&mut h, &mut b, 0, "abc");
        h.undo(&mut b);
        typed(&mut h, &mut b, 0, "z");
        assert!(!h.can_redo());
    }
}
