//! Virtualized data grid.
//!
//! Split in two on purpose. [`Grid`] is the model — numbers only, no widgets,
//! testable without a window. [`GridElement`] is the custom GPUI element that
//! turns those numbers into a frame. Every feature belongs to one or the other,
//! and the seam between them is the reason the grid can be reasoned about at
//! all: scrolling, selection and sizing are arithmetic you can unit-test, and
//! painting is a pure function of the result.

pub mod bench;
mod element;
mod state;
mod view;

pub use element::GridElement;
pub use state::{CellRect, ColumnLayout, Density, Grid, GridEvent, Sort};
