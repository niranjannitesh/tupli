//! Staged edits.
//!
//! Nothing typed into the grid reaches the database until commit. Until then
//! it lives here: a set of cell edits over rows that exist, a list of rows
//! that do not exist yet, and a set of rows on their way out. The grid draws
//! straight out of this — a cell with a pending value shows the pending value —
//! so there is one copy of the truth and no way for the display and the commit
//! to disagree about what is about to happen.
//!
//! Rows are addressed by their index in the fetched result set. That is the
//! same handle the grid already uses, and it stays valid because the result
//! set a change set belongs to is immutable: sorting produces a new one, and a
//! new one gets a new change set.

use std::collections::{BTreeMap, BTreeSet};

use db::Value;

/// A row a change is about.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum RowRef {
    /// Row `n` of the result set on screen.
    Existing(usize),
    /// The `n`th row added in this editing session, whether or not earlier
    /// ones are still around — slots are never reused, so a reference taken
    /// before a discard still means what it meant.
    New(usize),
}

/// How many of each kind of change are staged.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Counts {
    pub inserts: usize,
    pub updates: usize,
    pub deletes: usize,
    /// Schema changes. Never staged in a [`PendingChanges`] — the structure
    /// editor stages those — but counted here so that one commit is described
    /// by one sentence however it was written.
    pub ddl: usize,
}

impl Counts {
    pub fn total(self) -> usize {
        self.inserts + self.updates + self.deletes + self.ddl
    }
}

/// Everything staged against one result set.
#[derive(Clone, Default, Debug)]
pub struct PendingChanges {
    /// Existing row → column index → the value it should become.
    edits: BTreeMap<usize, BTreeMap<usize, Value>>,
    /// New rows, by slot. A discarded one leaves a hole rather than shifting
    /// its neighbours.
    inserts: BTreeMap<usize, BTreeMap<usize, Value>>,
    next_slot: usize,
    deletes: BTreeSet<usize>,
    /// Every change in the order it was made, so ⌘Z can walk back out.
    undo: Vec<Op>,
    redo: Vec<Op>,
}

/// One reversible step. Each carries what was there before, because undo has
/// to restore a previous *pending* value, which is not the same thing as the
/// value in the result set.
#[derive(Clone, Debug)]
enum Op {
    Set {
        row: RowRef,
        column: usize,
        before: Option<Value>,
        after: Option<Value>,
    },
    Insert {
        slot: usize,
    },
    Delete {
        row: usize,
        added: bool,
    },
    /// A new row taken back out, with everything that had been typed into it.
    Discard {
        slot: usize,
        values: BTreeMap<usize, Value>,
    },
}

impl PendingChanges {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty() && self.inserts.is_empty() && self.deletes.is_empty()
    }

    pub fn counts(&self) -> Counts {
        Counts {
            inserts: self.inserts.len(),
            // A row is one update however many of its cells were touched: one
            // statement goes to the server for it.
            updates: self
                .edits
                .keys()
                .filter(|r| !self.deletes.contains(r))
                .count(),
            deletes: self.deletes.len(),
            ddl: 0,
        }
    }

    // ---- reading ---------------------------------------------------------

    /// The staged value for a cell, if it has one.
    pub fn value(&self, row: RowRef, column: usize) -> Option<&Value> {
        match row {
            RowRef::Existing(row) => self.edits.get(&row)?.get(&column),
            RowRef::New(slot) => self.inserts.get(&slot)?.get(&column),
        }
    }

    pub fn is_edited(&self, row: usize, column: usize) -> bool {
        self.edits
            .get(&row)
            .is_some_and(|cells| cells.contains_key(&column))
    }

    pub fn is_row_edited(&self, row: usize) -> bool {
        self.edits.contains_key(&row)
    }

    pub fn is_deleted(&self, row: usize) -> bool {
        self.deletes.contains(&row)
    }

    /// The slots of the rows waiting to be inserted, in the order they were
    /// added — which is the order they are drawn under the last real row.
    pub fn new_rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.inserts.keys().copied()
    }

    pub fn new_row_count(&self) -> usize {
        self.inserts.len()
    }

    // ---- writing ---------------------------------------------------------

    /// Stage a value. Setting a cell back to what the database already has is
    /// still an edit here: proving it is the same value means comparing
    /// against the result set, which this type deliberately does not hold.
    pub fn set(&mut self, row: RowRef, column: usize, value: Value) {
        let before = self.put(row, column, Some(value.clone()));
        self.record(Op::Set {
            row,
            column,
            before,
            after: Some(value),
        });
    }

    /// Add an empty row. Its columns fill in as they are typed; the ones that
    /// are never touched are left out of the `INSERT` so their defaults apply.
    pub fn insert(&mut self) -> RowRef {
        let slot = self.next_slot;
        self.next_slot += 1;
        self.inserts.insert(slot, BTreeMap::new());
        self.record(Op::Insert { slot });
        RowRef::New(slot)
    }

    /// Mark a row for deletion, or take a not-yet-inserted one back out.
    pub fn delete(&mut self, row: RowRef) {
        match row {
            RowRef::Existing(row) => {
                if self.deletes.insert(row) {
                    self.record(Op::Delete { row, added: true });
                }
            }
            RowRef::New(slot) => {
                if let Some(values) = self.inserts.remove(&slot) {
                    self.record(Op::Discard { slot, values });
                }
            }
        }
    }

    /// Take a row off the deletion list.
    pub fn undelete(&mut self, row: usize) {
        if self.deletes.remove(&row) {
            self.record(Op::Delete { row, added: false });
        }
    }

    /// Throw one cell's edit away.
    pub fn revert(&mut self, row: RowRef, column: usize) {
        let before = self.put(row, column, None);
        if before.is_some() {
            self.record(Op::Set {
                row,
                column,
                before,
                after: None,
            });
        }
    }

    /// Throw everything away. Not undoable — it is the undo.
    pub fn clear(&mut self) {
        *self = Self {
            next_slot: self.next_slot,
            ..Self::default()
        };
    }

    // ---- undo ------------------------------------------------------------

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) {
        let Some(op) = self.undo.pop() else { return };
        let inverse = self.apply_inverse(&op);
        self.redo.push(inverse);
    }

    pub fn redo(&mut self) {
        let Some(op) = self.redo.pop() else { return };
        let inverse = self.apply_inverse(&op);
        self.undo.push(inverse);
    }

    // ---- internals -------------------------------------------------------

    /// Write a cell and hand back what was there. `None` removes it.
    fn put(&mut self, row: RowRef, column: usize, value: Option<Value>) -> Option<Value> {
        let cells = match row {
            RowRef::Existing(row) => self.edits.entry(row).or_default(),
            // A slot that is gone stays gone: typing into a discarded row
            // would resurrect it as a row nobody asked for.
            RowRef::New(slot) => match self.inserts.get_mut(&slot) {
                Some(cells) => cells,
                None => return None,
            },
        };
        let before = match value {
            Some(value) => cells.insert(column, value),
            None => cells.remove(&column),
        };
        // An existing row with nothing staged is not an edited row.
        if let RowRef::Existing(row) = row {
            if self.edits.get(&row).is_some_and(BTreeMap::is_empty) {
                self.edits.remove(&row);
            }
        }
        before
    }

    fn record(&mut self, op: Op) {
        self.undo.push(op);
        self.redo.clear();
    }

    /// Undo one op and return the op that would undo *that*, which is how the
    /// same code serves undo and redo.
    fn apply_inverse(&mut self, op: &Op) -> Op {
        match op {
            Op::Set {
                row,
                column,
                before,
                after,
            } => {
                self.put(*row, *column, before.clone());
                Op::Set {
                    row: *row,
                    column: *column,
                    before: after.clone(),
                    after: before.clone(),
                }
            }
            Op::Insert { slot } => {
                let values = self.inserts.remove(slot).unwrap_or_default();
                Op::Discard {
                    slot: *slot,
                    values,
                }
            }
            Op::Discard { slot, values } => {
                self.inserts.insert(*slot, values.clone());
                Op::Insert { slot: *slot }
            }
            Op::Delete { row, added } => {
                match added {
                    true => self.deletes.remove(row),
                    false => self.deletes.insert(*row),
                };
                Op::Delete {
                    row: *row,
                    added: !added,
                }
            }
        }
    }

    /// The staged edits of one existing row, column index → new value.
    pub(crate) fn row_edits(&self, row: usize) -> Option<&BTreeMap<usize, Value>> {
        self.edits.get(&row)
    }

    pub(crate) fn edited_rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.edits
            .keys()
            .copied()
            .filter(|row| !self.deletes.contains(row))
    }

    pub(crate) fn insert_rows(&self) -> impl Iterator<Item = &BTreeMap<usize, Value>> + '_ {
        self.inserts.values()
    }

    pub(crate) fn deleted_rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.deletes.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::ValueKind;

    fn text(s: &str) -> Value {
        Value::text(ValueKind::Text, s)
    }

    #[test]
    fn a_row_edited_twice_is_still_one_update() {
        let mut changes = PendingChanges::new();
        changes.set(RowRef::Existing(3), 0, text("a"));
        changes.set(RowRef::Existing(3), 1, text("b"));
        assert_eq!(changes.counts().updates, 1);
        assert_eq!(changes.value(RowRef::Existing(3), 1), Some(&text("b")));
    }

    #[test]
    fn reverting_the_last_cell_unedits_the_row() {
        let mut changes = PendingChanges::new();
        changes.set(RowRef::Existing(0), 2, text("x"));
        changes.revert(RowRef::Existing(0), 2);
        assert!(!changes.is_row_edited(0));
        assert!(changes.is_empty());
    }

    #[test]
    fn undo_walks_back_out_and_redo_walks_back_in() {
        let mut changes = PendingChanges::new();
        changes.set(RowRef::Existing(0), 0, text("one"));
        changes.set(RowRef::Existing(0), 0, text("two"));
        changes.undo();
        assert_eq!(changes.value(RowRef::Existing(0), 0), Some(&text("one")));
        changes.undo();
        assert!(changes.is_empty());
        changes.redo();
        assert_eq!(changes.value(RowRef::Existing(0), 0), Some(&text("one")));
        changes.redo();
        assert_eq!(changes.value(RowRef::Existing(0), 0), Some(&text("two")));
        assert!(!changes.can_redo());
    }

    #[test]
    fn a_new_row_taken_back_out_can_be_undone_with_what_was_typed_in_it() {
        let mut changes = PendingChanges::new();
        let row = changes.insert();
        changes.set(row, 0, text("draft"));
        changes.delete(row);
        assert_eq!(changes.new_row_count(), 0);
        changes.undo();
        assert_eq!(changes.value(row, 0), Some(&text("draft")));
    }

    #[test]
    fn a_discarded_slot_is_not_reused() {
        let mut changes = PendingChanges::new();
        let first = changes.insert();
        changes.delete(first);
        let second = changes.insert();
        assert_ne!(first, second);
    }

    #[test]
    fn a_deleted_row_is_not_also_an_update() {
        let mut changes = PendingChanges::new();
        changes.set(RowRef::Existing(1), 0, text("edited"));
        changes.delete(RowRef::Existing(1));
        let counts = changes.counts();
        assert_eq!((counts.updates, counts.deletes), (0, 1));
    }

    #[test]
    fn a_new_change_forgets_the_redo_stack() {
        let mut changes = PendingChanges::new();
        changes.set(RowRef::Existing(0), 0, text("a"));
        changes.undo();
        changes.set(RowRef::Existing(0), 0, text("b"));
        assert!(!changes.can_redo());
    }
}
