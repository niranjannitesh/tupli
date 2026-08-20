//! The text editor.
//!
//! One implementation behind every place text is typed in this app — the SQL
//! console, the connection sheet's fields, the sidebar filter, and later the
//! grid's inline cell editor. They differ by [`EditorMode`] and a couple of
//! booleans, per §11.3 of the plan.
//!
//! The layering is deliberate and one-directional: [`buffer`] knows only about
//! text, [`movement`] only about a buffer and an offset, [`selection`] and
//! [`history`] only about offsets, and [`editor`] is the only module that has
//! ever heard of a window.

pub mod buffer;
pub mod completion;
pub mod editor;
pub mod format;
pub mod element;
pub mod history;
pub mod input;
pub mod movement;
pub mod selection;
pub mod sql;
pub mod syntax;

pub use buffer::{Buffer, Point};
pub use completion::{Completion, CompletionContext, CompletionKind, CompletionSource};
pub use editor::{Direction, Editor, EditorEvent, EditorMode, Highlight, Highlighter};
pub use element::{EditorElement, EditorStyle};
pub use input::{Input, InputSize};
pub use selection::{Selection, SelectionSet};
