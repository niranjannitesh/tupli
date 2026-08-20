//! Cursors and selections.
//!
//! A `Vec<Selection>` from day one even though the UI only ever makes one, per
//! §11.3 of the plan: retrofitting multi-cursor into a single-selection model
//! means touching every edit path twice.

use std::ops::Range;

use crate::buffer::Buffer;

/// One cursor, possibly with a selection behind it.
///
/// `anchor` is where the selection was started and `head` is where the cursor
/// is now, so `anchor > head` is a perfectly ordinary backwards selection and
/// callers must not assume otherwise. A collapsed selection — `anchor ==
/// head` — is a plain cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    /// The column vertical movement is trying to get back to.
    ///
    /// Walking down past a short line and back out the other side should return
    /// to the column you started in, not the one the short line clamped you to.
    /// Set by up/down, cleared by everything else.
    pub goal_column: Option<usize>,
}

impl Selection {
    pub fn cursor(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
            goal_column: None,
        }
    }

    pub fn new(anchor: usize, head: usize) -> Self {
        Self {
            anchor,
            head,
            goal_column: None,
        }
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn range(&self) -> Range<usize> {
        self.start()..self.end()
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Collapse to the head, dropping any selection.
    pub fn collapse(&mut self) {
        self.anchor = self.head;
    }

    /// Move the head, taking the anchor along unless we are extending.
    pub fn move_to(&mut self, offset: usize, extend: bool) {
        self.head = offset;
        if !extend {
            self.anchor = offset;
        }
        self.goal_column = None;
    }
}

/// The live set of selections, kept sorted and non-overlapping.
#[derive(Clone, Debug)]
pub struct SelectionSet {
    selections: Vec<Selection>,
}

impl Default for SelectionSet {
    fn default() -> Self {
        Self {
            selections: vec![Selection::cursor(0)],
        }
    }
}

impl SelectionSet {
    pub fn single(selection: Selection) -> Self {
        Self {
            selections: vec![selection],
        }
    }

    pub fn all(&self) -> &[Selection] {
        &self.selections
    }

    pub fn all_mut(&mut self) -> &mut Vec<Selection> {
        &mut self.selections
    }

    /// The selection the UI treats as *the* cursor: the last one added, which
    /// is the one the user most recently moved.
    pub fn newest(&self) -> Selection {
        *self.selections.last().unwrap_or(&Selection {
            anchor: 0,
            head: 0,
            goal_column: None,
        })
    }

    pub fn set(&mut self, selections: Vec<Selection>) {
        self.selections = selections;
        if self.selections.is_empty() {
            self.selections.push(Selection::cursor(0));
        }
        self.merge();
    }

    pub fn replace_with_cursor(&mut self, offset: usize) {
        self.selections = vec![Selection::cursor(offset)];
    }

    pub fn len(&self) -> usize {
        self.selections.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Sort by start, then fuse anything that touches or overlaps.
    ///
    /// Two cursors that have collided are one cursor; leaving both means the
    /// next keystroke gets typed twice.
    pub fn merge(&mut self) {
        self.selections.sort_by_key(|s| (s.start(), s.end()));
        let mut merged: Vec<Selection> = Vec::with_capacity(self.selections.len());
        for sel in self.selections.drain(..) {
            match merged.last_mut() {
                Some(last) if sel.start() <= last.end() => {
                    // Keep the later selection's direction — it is the one the
                    // user is actively driving.
                    let start = last.start().min(sel.start());
                    let end = last.end().max(sel.end());
                    let reversed = sel.head < sel.anchor;
                    *last = Selection {
                        anchor: if reversed { end } else { start },
                        head: if reversed { start } else { end },
                        goal_column: sel.goal_column,
                    };
                }
                _ => merged.push(sel),
            }
        }
        self.selections = merged;
    }

    /// Clamp every selection into a buffer that may have shrunk under it.
    pub fn clip(&mut self, buffer: &Buffer) {
        for sel in &mut self.selections {
            sel.anchor = buffer.clip_offset(sel.anchor);
            sel.head = buffer.clip_offset(sel.head);
        }
        self.merge();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_selections_fuse() {
        let mut set = SelectionSet::single(Selection::new(0, 5));
        set.all_mut().push(Selection::new(3, 9));
        set.merge();
        assert_eq!(set.len(), 1);
        assert_eq!(set.newest().range(), 0..9);
    }

    #[test]
    fn touching_cursors_fuse() {
        let mut set = SelectionSet::single(Selection::cursor(4));
        set.all_mut().push(Selection::cursor(4));
        set.merge();
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn a_fused_selection_keeps_the_newer_direction() {
        let mut set = SelectionSet::single(Selection::new(0, 5));
        set.all_mut().push(Selection::new(9, 3));
        set.merge();
        let s = set.newest();
        assert_eq!((s.anchor, s.head), (9, 0));
    }

    #[test]
    fn disjoint_selections_survive() {
        let mut set = SelectionSet::single(Selection::new(0, 2));
        set.all_mut().push(Selection::new(5, 7));
        set.merge();
        assert_eq!(set.len(), 2);
    }
}
