//! Where the cursor goes.
//!
//! Pure functions over a [`Buffer`] and a char offset. Keeping motion out of
//! the editor entity means every motion is testable without a window, and the
//! same functions serve the cursor, the selection extension, and the delete
//! commands — ⌥⌫ is "delete to `prev_word_boundary`", not a second
//! implementation of word scanning that drifts out of agreement with ⌥←.

use crate::buffer::{Buffer, Point};

/// Rough character classes, which is all word motion needs.
///
/// `_` counts as a word char because SQL identifiers are full of it and
/// stopping inside `created_at` is never what ⌥← was for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Whitespace,
    Word,
    Punctuation,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Whitespace
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punctuation
    }
}

pub fn left(buffer: &Buffer, offset: usize) -> usize {
    offset.saturating_sub(1).min(buffer.len())
}

pub fn right(buffer: &Buffer, offset: usize) -> usize {
    (offset + 1).min(buffer.len())
}

/// Move one line up, aiming for `goal` if there is one.
///
/// Returns the new offset and the goal column to carry forward — the caller
/// stores it on the selection so a walk down through a short line and out the
/// other side lands back where it started.
pub fn up(buffer: &Buffer, offset: usize, goal: Option<usize>) -> (usize, Option<usize>) {
    vertical(buffer, offset, goal, -1)
}

pub fn down(buffer: &Buffer, offset: usize, goal: Option<usize>) -> (usize, Option<usize>) {
    vertical(buffer, offset, goal, 1)
}

fn vertical(
    buffer: &Buffer,
    offset: usize,
    goal: Option<usize>,
    delta: isize,
) -> (usize, Option<usize>) {
    let point = buffer.offset_to_point(offset);
    let goal = goal.unwrap_or(point.column);
    let row = point.row as isize + delta;
    if row < 0 {
        return (0, Some(goal));
    }
    let row = row as usize;
    if row >= buffer.line_count() {
        return (buffer.len(), Some(goal));
    }
    let column = goal.min(buffer.line_len(row));
    (buffer.point_to_offset(Point::new(row, column)), Some(goal))
}

pub fn line_start(buffer: &Buffer, offset: usize) -> usize {
    let point = buffer.offset_to_point(offset);
    buffer.point_to_offset(Point::new(point.row, 0))
}

pub fn line_end(buffer: &Buffer, offset: usize) -> usize {
    let point = buffer.offset_to_point(offset);
    buffer.point_to_offset(Point::new(point.row, buffer.line_len(point.row)))
}

/// Home, the way every editor written since about 1995 does it: to the first
/// non-blank char, and to column 0 only if you are already there.
pub fn smart_line_start(buffer: &Buffer, offset: usize) -> usize {
    let point = buffer.offset_to_point(offset);
    let line = buffer.line(point.row);
    let indent = line.chars().take_while(|c| c.is_whitespace()).count();
    let indent = if indent == line.chars().count() {
        0
    } else {
        indent
    };
    if point.column == indent {
        buffer.point_to_offset(Point::new(point.row, 0))
    } else {
        buffer.point_to_offset(Point::new(point.row, indent))
    }
}

/// Start of the word to the left.
///
/// Skips any run of whitespace first, then consumes one run of a single class,
/// so `foo.bar` stops at the `.` rather than jumping the whole thing.
pub fn prev_word_boundary(buffer: &Buffer, offset: usize) -> usize {
    let mut i = offset.min(buffer.len());
    while i > 0 && matches!(buffer.char_at(i - 1).map(class), Some(Class::Whitespace)) {
        i -= 1;
    }
    let Some(start_class) = buffer.char_at(i.wrapping_sub(1)).map(class) else {
        return i;
    };
    while i > 0 && buffer.char_at(i - 1).map(class) == Some(start_class) {
        i -= 1;
    }
    i
}

pub fn next_word_boundary(buffer: &Buffer, offset: usize) -> usize {
    let len = buffer.len();
    let mut i = offset.min(len);
    while i < len && matches!(buffer.char_at(i).map(class), Some(Class::Whitespace)) {
        i += 1;
    }
    let Some(start_class) = buffer.char_at(i).map(class) else {
        return i;
    };
    while i < len && buffer.char_at(i).map(class) == Some(start_class) {
        i += 1;
    }
    i
}

/// The word under `offset`, for double-click and ⌘D.
pub fn word_at(buffer: &Buffer, offset: usize) -> (usize, usize) {
    let len = buffer.len();
    let offset = offset.min(len);
    // Prefer the word the cursor sits *inside*; failing that, the one it sits
    // at the end of — clicking just past `users` should select `users`.
    let here = buffer.char_at(offset).map(class);
    let before = buffer.char_at(offset.wrapping_sub(1)).map(class);
    let target = match (here, before) {
        (Some(Class::Word), _) => Class::Word,
        (_, Some(Class::Word)) => Class::Word,
        (Some(c), _) if c != Class::Whitespace => c,
        (_, Some(c)) if c != Class::Whitespace => c,
        _ => Class::Whitespace,
    };
    let mut start = offset;
    while start > 0 && buffer.char_at(start - 1).map(class) == Some(target) {
        start -= 1;
    }
    let mut end = offset;
    while end < len && buffer.char_at(end).map(class) == Some(target) {
        end += 1;
    }
    (start, end)
}

/// The whole line, including its newline, for ⌘⇧K and triple-click.
pub fn line_range(buffer: &Buffer, offset: usize) -> (usize, usize) {
    let point = buffer.offset_to_point(offset);
    let start = buffer.point_to_offset(Point::new(point.row, 0));
    let end = if point.row + 1 < buffer.line_count() {
        buffer.point_to_offset(Point::new(point.row + 1, 0))
    } else {
        buffer.len()
    };
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_movement_remembers_the_goal_column() {
        let b = Buffer::new("longest line here\nshort\nanother long line");
        let start = b.point_to_offset(Point::new(0, 15));
        let (mid, goal) = down(&b, start, None);
        assert_eq!(b.offset_to_point(mid), Point::new(1, 5));
        let (end, _) = down(&b, mid, goal);
        assert_eq!(b.offset_to_point(end), Point::new(2, 15));
    }

    #[test]
    fn smart_home_toggles() {
        let b = Buffer::new("    select 1");
        let end = b.point_to_offset(Point::new(0, 12));
        let indent = smart_line_start(&b, end);
        assert_eq!(indent, 4);
        assert_eq!(smart_line_start(&b, indent), 0);
    }

    #[test]
    fn word_motion_stops_at_punctuation() {
        let b = Buffer::new("select u.created_at from");
        assert_eq!(next_word_boundary(&b, 0), 6);
        assert_eq!(next_word_boundary(&b, 6), 8); // over the space, then `u`
        assert_eq!(next_word_boundary(&b, 8), 9); // the `.`
        assert_eq!(prev_word_boundary(&b, 19), 9);
    }

    #[test]
    fn underscores_are_part_of_a_word() {
        let b = Buffer::new("created_at");
        assert_eq!(word_at(&b, 4), (0, 10));
    }

    #[test]
    fn clicking_just_past_a_word_selects_it() {
        let b = Buffer::new("users ");
        assert_eq!(word_at(&b, 5), (0, 5));
    }
}
