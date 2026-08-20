//! Small extensions to gpui's `Styled` that encode house style.

use gpui::{div, Div, Styled};

/// A row. Used often enough that spelling out `flex().flex_row().items_center()`
/// everywhere would bury the interesting part of each component.
pub fn h_flex() -> Div {
    div().flex().flex_row().items_center()
}

/// A column.
pub fn v_flex() -> Div {
    div().flex().flex_col()
}

/// House-style helpers available on any styled element.
pub trait StyledExt: Styled + Sized {
    fn h_flex(self) -> Self {
        self.flex().flex_row().items_center()
    }

    fn v_flex(self) -> Self {
        self.flex().flex_col()
    }

    /// Apply `f` only when `condition` holds. gpui has `when` on
    /// `InteractiveElement`; this mirrors it for plain styling chains.
    fn when_true(self, condition: bool, f: impl FnOnce(Self) -> Self) -> Self {
        if condition {
            f(self)
        } else {
            self
        }
    }
}

impl<E: Styled> StyledExt for E {}
