//! Where the panels were when you last quit.
//!
//! Layout is not a preference anyone sets — it is one they arrive at, by
//! dragging a splitter until the column is the width they wanted. Losing that
//! on every launch is the kind of small rudeness that makes an app feel like a
//! demo, so it is written to the same SQLite file everything else lives in.
//!
//! It is stored as one JSON value under one key rather than as a column per
//! field, because unlike the connection list nothing ever queries it: the app
//! reads the whole thing at boot and writes the whole thing back. Every field
//! is optional on the way in, so a layout written by an older build still
//! loads and simply leaves the new fields at their defaults.

use serde::{Deserialize, Serialize};

pub const KEY: &str = "layout";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_open: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_open: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dock_open: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dock_height: Option<f32>,
}

impl Layout {
    /// Read the saved layout, or nothing at all. A file that cannot be parsed
    /// is treated as a file that was never written: a corrupt layout must not
    /// be able to stop the app from opening a window.
    pub fn load(store: Option<&store::Store>) -> Self {
        let Some(store) = store else {
            return Self::default();
        };
        match store.setting(KEY) {
            Ok(Some(text)) => serde_json::from_str(&text).unwrap_or_else(|error| {
                log::warn!("ignoring an unreadable saved layout: {error}");
                Self::default()
            }),
            Ok(None) => Self::default(),
            Err(error) => {
                log::warn!("could not read the saved layout: {error:#}");
                Self::default()
            }
        }
    }

    pub fn save(&self, store: Option<&store::Store>) {
        let Some(store) = store else { return };
        match serde_json::to_string(self) {
            Ok(text) => {
                if let Err(error) = store.set_setting(KEY, &text) {
                    log::warn!("could not save the layout: {error:#}");
                }
            }
            Err(error) => log::warn!("could not encode the layout: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layout_from_an_older_build_still_loads() {
        // Only one field, and one this build has never heard of.
        let old: Layout = serde_json::from_str(r#"{"left_width":260.0}"#).unwrap();
        assert_eq!(old.left_width, Some(260.));
        assert_eq!(old.dock_height, None);
    }

    #[test]
    fn nothing_unset_is_written() {
        let mut layout = Layout::default();
        layout.dock_open = Some(false);
        assert_eq!(
            serde_json::to_string(&layout).unwrap(),
            r#"{"dock_open":false}"#
        );
    }

    #[test]
    fn a_saved_layout_survives_the_round_trip() {
        let store = store::Store::in_memory().unwrap();
        let layout = Layout {
            left_open: Some(false),
            right_open: Some(true),
            dock_open: Some(true),
            left_width: Some(312.5),
            right_width: Some(280.),
            dock_height: Some(400.),
        };
        layout.save(Some(&store));
        assert_eq!(Layout::load(Some(&store)), layout);
    }

    #[test]
    fn nonsense_in_the_settings_table_is_ignored_not_fatal() {
        let store = store::Store::in_memory().unwrap();
        store.set_setting(KEY, "{not json").unwrap();
        assert_eq!(Layout::load(Some(&store)), Layout::default());
    }
}
