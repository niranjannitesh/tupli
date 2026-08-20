//! tupli as a library, so tools other than the app binary — the headless
//! screenshot renderer, tests — can build the same window.

pub mod center;
pub mod clipboard;
pub mod complete;
pub mod connection_window;
pub mod editing;
pub mod export;
pub mod filter;
pub mod import;
pub mod inspector;
pub mod json;
pub mod layout;
pub mod menu;
pub mod mock;
pub mod objects;
pub mod palette;
pub mod pane;
pub mod privileges;
pub mod restore;
pub mod results;
pub mod save_sheet;
pub mod session;
pub mod settings;
pub mod settings_window;
pub mod sidebar;
pub mod structure;
pub mod tabs;
pub mod tint;
pub mod titlebar;
pub mod tree;
pub mod workspace;
