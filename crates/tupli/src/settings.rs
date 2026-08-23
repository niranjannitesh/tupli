//! What you chose, as opposed to what you arrived at.
//!
//! [`crate::layout`] holds the panel widths a splitter drag ended up at.
//! This holds the things someone picked on purpose — everything behind §5.9's
//! Settings window. Same storage, same every-field-optional rule so an older
//! build's file still loads, but a separate key: a corrupt layout should not
//! cost you your theme.
//!
//! Every field is `Option` and every reader has a default, which is what makes
//! the file forward- and backward-compatible without a version number: a
//! setting this build does not have is ignored, and one it has but the file
//! does not falls back. Values that name a thing — the accent, the density —
//! are stored as words rather than as indices, because an index means whatever
//! the order of a list happened to be on the day it was written.

use serde::{Deserialize, Serialize};
use ui::{Appearance, Theme, ThemeRegistry};

pub const KEY: &str = "settings";

/// The code sizes offered, in points. Anything else in the file is snapped to
/// the nearest of these rather than rejected.
pub const MONO_SIZES: &[f32] = &[11., 12., 12.5, 14., 16.];
/// Indent widths offered.
pub const TAB_SIZES: &[usize] = &[2, 4, 8];
/// Row caps offered for browsing a table. The grid holds everything it is given
/// in memory, so the top of this range is a promise about the machine as much
/// as about the server.
pub const PAGE_SIZES: &[usize] = &[200, 1_000, 10_000, 50_000, 200_000];

pub const DEFAULT_MONO_SIZE: f32 = 12.5;
pub const DEFAULT_TAB_SIZE: usize = 4;
/// A page, not a download. Fifty thousand rows of a wide table across a pooler
/// is twenty seconds of waiting for a screen that shows forty of them, and the
/// forty are the same forty either way — the pager is right there for the rest.
/// Anyone who would rather have the whole table in memory can say so in
/// Settings, and that choice is remembered.
pub const DEFAULT_PAGE_SIZE: usize = 1_000;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// `"dark"` or `"light"`. Stored as a word rather than a bool so that a
    /// third theme does not need a migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<String>,
    /// The theme to use in each appearance, by name — `"One Dark"`, `"Ayu
    /// Light"`, or the name of a file in the themes directory.
    ///
    /// Two fields rather than one because the appearance is a switch someone
    /// flips several times a day and the theme is a choice they made once:
    /// following the system into light mode should not cost you the dark theme
    /// you picked. A name this build does not have falls back to the
    /// built-in for that appearance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dark_theme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_theme: Option<String>,
    /// The code face, by family name. Empty or missing means "whatever this
    /// machine has", which is what a fresh install wants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mono_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mono_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_size: Option<usize>,
    /// `"compact"`, `"default"` or `"comfortable"` — how tall a grid row is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_density: Option<String>,
    /// Whether the console shows a line-number gutter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_numbers: Option<bool>,
    /// The `limit` on browsing a table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<usize>,
    /// Whether every other grid row is shaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zebra: Option<bool>,
    /// Whether the tabs and the half-written statement come back at launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_session: Option<bool>,
    /// Whether the connection the last session was on is reopened at launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reopen_connection: Option<bool>,
}

impl Settings {
    /// Read the saved settings, or nothing at all. A file that cannot be
    /// parsed is treated as one that was never written: a bad preference must
    /// not be able to stop the app from opening a window.
    pub fn load(store: Option<&store::Store>) -> Self {
        let Some(store) = store else {
            return Self::default();
        };
        match store.setting(KEY) {
            Ok(Some(text)) => serde_json::from_str(&text).unwrap_or_else(|error| {
                log::warn!("ignoring unreadable saved settings: {error}");
                Self::default()
            }),
            Ok(None) => Self::default(),
            Err(error) => {
                log::warn!("could not read the saved settings: {error:#}");
                Self::default()
            }
        }
    }

    pub fn save(&self, store: Option<&store::Store>) {
        let Some(store) = store else { return };
        match serde_json::to_string(self) {
            Ok(text) => {
                if let Err(error) = store.set_setting(KEY, &text) {
                    log::warn!("could not save the settings: {error:#}");
                }
            }
            Err(error) => log::warn!("could not encode the settings: {error}"),
        }
    }

    /// The saved appearance, if it is one this build knows about. An
    /// unrecognised word is not an error — it is a file from a build that had
    /// a theme this one does not — so it falls back rather than failing.
    pub fn appearance(&self) -> Option<Appearance> {
        match self.appearance.as_deref() {
            Some("dark") => Some(Appearance::Dark),
            Some("light") => Some(Appearance::Light),
            _ => None,
        }
    }

    pub fn set_appearance(&mut self, appearance: Appearance) {
        self.appearance = Some(name_of(appearance).to_string());
    }

    /// Hand the choice back to macOS.
    pub fn follow_system(&mut self) {
        self.appearance = None;
    }

    /// Use this theme, and be whatever appearance it is.
    ///
    /// One call rather than two, because to a person these are not two
    /// decisions: picking Ayu Light is picking to be in light mode, and a
    /// picker that made you set the mode first before it would even show you
    /// Ayu Light is asking the same question twice. Pinning is deliberate —
    /// following the system is a thing you turn back on, not something a theme
    /// pick silently leaves you in while showing you a different theme.
    pub fn choose_theme(&mut self, name: &str, appearance: Appearance) {
        self.set_theme_name(appearance, name);
        self.set_appearance(appearance);
    }

    /// The chosen theme for an appearance, or the built-in's name.
    ///
    /// `TUPLI_THEME_NAME` overrides it, the way `TUPLI_THEME` overrides the
    /// appearance: the screenshot harness has to be able to name a theme
    /// without a settings database, and a bug report about a theme is far
    /// easier to reproduce from a command line than from a file. A family name
    /// — `One`, not `One Dark` — is enough, and is what to use when both
    /// appearances are being rendered in one run.
    pub fn theme_name(&self, appearance: Appearance) -> &str {
        if let Some(name) = env_theme_name() {
            return name;
        }
        let chosen = match appearance {
            Appearance::Dark => self.dark_theme.as_deref(),
            Appearance::Light => self.light_theme.as_deref(),
        };
        chosen.unwrap_or("")
    }

    pub fn set_theme_name(&mut self, appearance: Appearance, name: &str) {
        let slot = match appearance {
            Appearance::Dark => &mut self.dark_theme,
            Appearance::Light => &mut self.light_theme,
        };
        *slot = Some(name.to_string());
    }

    /// The chosen code face, or nothing — nothing meaning the machine decides.
    pub fn mono_family(&self) -> &str {
        self.mono_family.as_deref().unwrap_or("")
    }

    pub fn set_mono_family(&mut self, family: &str) {
        self.mono_family = Some(family.to_string());
    }

    /// The code size, snapped to one the app offers. A file saying `9.7` came
    /// from somewhere other than this window, and the honest reading is the
    /// nearest size the ramp actually has.
    pub fn mono_size(&self) -> f32 {
        let Some(size) = self.mono_size else {
            return DEFAULT_MONO_SIZE;
        };
        nearest(MONO_SIZES, size)
    }

    pub fn set_mono_size(&mut self, size: f32) {
        self.mono_size = Some(size);
    }

    pub fn tab_size(&self) -> usize {
        match self.tab_size {
            Some(size) if TAB_SIZES.contains(&size) => size,
            _ => DEFAULT_TAB_SIZE,
        }
    }

    pub fn set_tab_size(&mut self, size: usize) {
        self.tab_size = Some(size);
    }

    pub fn row_density(&self) -> grid::Density {
        match self.row_density.as_deref() {
            Some("compact") => grid::Density::Compact,
            Some("comfortable") => grid::Density::Comfortable,
            _ => grid::Density::Default,
        }
    }

    pub fn set_row_density(&mut self, density: grid::Density) {
        self.row_density = Some(
            match density {
                grid::Density::Compact => "compact",
                grid::Density::Default => "default",
                grid::Density::Comfortable => "comfortable",
            }
            .to_string(),
        );
    }

    pub fn line_numbers(&self) -> bool {
        self.line_numbers.unwrap_or(true)
    }

    pub fn set_line_numbers(&mut self, on: bool) {
        self.line_numbers = Some(on);
    }

    pub fn zebra(&self) -> bool {
        self.zebra.unwrap_or(true)
    }

    pub fn set_zebra(&mut self, on: bool) {
        self.zebra = Some(on);
    }

    pub fn page_size(&self) -> usize {
        match self.page_size {
            Some(size) if PAGE_SIZES.contains(&size) => size,
            _ => DEFAULT_PAGE_SIZE,
        }
    }

    pub fn set_page_size(&mut self, size: usize) {
        self.page_size = Some(size);
    }

    /// On unless it was turned off. Coming back to what you were doing is the
    /// behaviour someone who has never opened this window expects.
    pub fn restore_session(&self) -> bool {
        self.restore_session.unwrap_or(true)
    }

    pub fn set_restore_session(&mut self, on: bool) {
        self.restore_session = Some(on);
    }

    pub fn reopen_connection(&self) -> bool {
        self.reopen_connection.unwrap_or(true)
    }

    pub fn set_reopen_connection(&mut self, on: bool) {
        self.reopen_connection = Some(on);
    }

    /// Build the theme these settings describe.
    ///
    /// The appearance is passed in rather than read from the file because the
    /// caller sometimes knows better: the screenshot harness announces which
    /// pass it is on, and a live preview in the Settings window is showing an
    /// appearance that has not been chosen yet.
    pub fn theme(&self, appearance: Appearance, cx: &gpui::App) -> Theme {
        // No accent knob: a theme's accent is part of the theme, the same way
        // its greens and its selection are, and every one of them was chosen
        // against that accent. Overriding it from a picker would leave the
        // rest of the palette answering to a colour that is no longer there.
        self.theme_named(self.theme_name(appearance), appearance, cx)
    }

    /// A named theme, dressed in the settings that are not part of a theme.
    ///
    /// Split out for the palette's preview, which is showing a theme nobody
    /// has chosen yet — the code face and its size still have to survive it,
    /// or arrowing down the theme list would resize the editor on every row.
    pub fn theme_named(&self, name: &str, appearance: Appearance, cx: &gpui::App) -> Theme {
        ThemeRegistry::resolve(name, appearance, cx)
            .with_mono_family(self.mono_family().to_string())
            .with_mono_size(gpui::px(self.mono_size()))
    }
}

/// Read once. The variable is a launch-time switch, and re-reading it every
/// frame would let a `setenv` from somewhere else change the theme mid-draw.
fn env_theme_name() -> Option<&'static str> {
    static NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        std::env::var("TUPLI_THEME_NAME")
            .ok()
            .filter(|name| !name.trim().is_empty())
    })
    .as_deref()
}

fn name_of(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Dark => "dark",
        Appearance::Light => "light",
    }
}

/// The offered value closest to the one asked for.
fn nearest(offered: &[f32], value: f32) -> f32 {
    offered
        .iter()
        .copied()
        .min_by(|a, b| {
            (a - value)
                .abs()
                .partial_cmp(&(b - value).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_appearance_survives_a_round_trip() {
        let mut settings = Settings::default();
        settings.set_appearance(Appearance::Light);
        let text = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.appearance(), Some(Appearance::Light));
    }

    #[test]
    fn a_theme_this_build_does_not_have_is_not_an_error() {
        let settings: Settings = serde_json::from_str(r#"{"appearance":"solarized"}"#).unwrap();
        assert_eq!(settings.appearance(), None);
    }

    #[test]
    fn settings_from_an_older_build_still_load() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.appearance(), None);
        assert_eq!(settings.mono_size(), DEFAULT_MONO_SIZE);
        assert_eq!(settings.page_size(), DEFAULT_PAGE_SIZE);
        assert!(settings.restore_session());
    }

    #[test]
    fn a_setting_this_build_no_longer_has_is_ignored() {
        // The accent picker was removed once themes became the unit. A file
        // written by the build that had it must still load.
        let settings: Settings = serde_json::from_str(r#"{"accent":"chartreuse"}"#).unwrap();
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn a_size_the_ramp_does_not_have_snaps_to_the_nearest_one() {
        let settings: Settings = serde_json::from_str(r#"{"mono_size":13.6}"#).unwrap();
        assert_eq!(settings.mono_size(), 14.);
        let settings: Settings = serde_json::from_str(r#"{"mono_size":1.0}"#).unwrap();
        assert_eq!(settings.mono_size(), 11.);
    }

    #[test]
    fn a_page_size_from_nowhere_is_ignored() {
        let settings: Settings = serde_json::from_str(r#"{"page_size":7}"#).unwrap();
        assert_eq!(settings.page_size(), DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn every_knob_survives_a_round_trip() {
        let mut settings = Settings::default();
        settings.set_appearance(Appearance::Dark);
        settings.set_mono_family("Menlo");
        settings.set_mono_size(14.);
        settings.set_tab_size(2);
        settings.set_row_density(grid::Density::Comfortable);
        settings.set_page_size(1_000);
        settings.set_line_numbers(false);
        settings.set_zebra(false);
        settings.set_restore_session(false);
        settings.set_reopen_connection(false);

        let text = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back, settings);
        assert_eq!(back.mono_family(), "Menlo");
        assert_eq!(back.mono_size(), 14.);
        assert_eq!(back.tab_size(), 2);
        assert_eq!(back.row_density(), grid::Density::Comfortable);
        assert_eq!(back.page_size(), 1_000);
        assert!(!back.line_numbers());
        assert!(!back.zebra());
        assert!(!back.restore_session());
        assert!(!back.reopen_connection());
    }
}
