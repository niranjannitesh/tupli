//! Every theme this build can offer, in one list.
//!
//! Three sources, in order of who wins a name collision: the two built-ins,
//! then the theme files compiled into the binary, then whatever is in the
//! user's themes directory. Last wins, so dropping `One.json` next to the app's
//! settings replaces the bundled One rather than producing two entries called
//! the same thing — which is how someone tweaks a bundled theme, and the only
//! reason the order matters.
//!
//! Loading happens once at startup and the result is a global. A theme is a few
//! hundred bytes of colours; there is nothing to be gained by reading the files
//! lazily and a great deal to be lost by discovering a broken one halfway
//! through a redraw.

use std::path::{Path, PathBuf};

use gpui::{App, Global, SharedString};

use crate::{assets, zed_theme::ThemeFamily, Appearance, Theme};

pub struct ThemeRegistry {
    themes: Vec<Theme>,
}

impl Global for ThemeRegistry {}

impl Default for ThemeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemeRegistry {
    /// Just the built-ins — what the app runs on before anything is read from
    /// disk, and what tests and the benches want.
    pub fn new() -> Self {
        Self {
            themes: vec![Theme::dark(), Theme::light()],
        }
    }

    /// The built-ins, the bundled files, and every `.json` directly inside each
    /// of `user_dirs`.
    ///
    /// A file that will not parse is logged and skipped. The alternative — 
    /// refusing to start, or falling back to built-ins wholesale — punishes
    /// every other theme for one bad one, and the user has no way to see the
    /// error anyway until the window they are trying to open is open.
    pub fn load(user_dirs: &[PathBuf]) -> Self {
        let mut registry = Self::new();
        for (path, bytes) in assets::bundled_themes() {
            registry.add_file(&path, &bytes);
        }
        for dir in user_dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            // Read order from the filesystem is arbitrary, and "last wins"
            // is only a rule if the order is one the user can predict.
            let mut files: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .collect();
            files.sort();
            for path in files {
                match std::fs::read(&path) {
                    Ok(bytes) => registry.add_file(&path.to_string_lossy(), &bytes),
                    Err(err) => log::warn!("theme {}: {err}", path.display()),
                }
            }
        }
        registry
    }

    fn add_file(&mut self, path: &str, bytes: &[u8]) {
        let family = match ThemeFamily::parse(bytes) {
            Ok(family) => family,
            Err(err) => {
                log::warn!("theme {path}: {err}");
                return;
            }
        };
        for theme in family.to_themes() {
            self.insert(theme);
        }
    }

    fn insert(&mut self, theme: Theme) {
        match self
            .themes
            .iter()
            .position(|t| t.name == theme.name && t.appearance == theme.appearance)
        {
            Some(ix) => self.themes[ix] = theme,
            None => self.themes.push(theme),
        }
    }

    /// The themes of one appearance, in the order Settings should list them:
    /// the built-in first, then everything else alphabetically. A picker sorted
    /// purely by name buries the default in the middle of the Os.
    pub fn listed(&self, appearance: Appearance) -> Vec<&Theme> {
        let built_in = Theme::of(appearance).name;
        let mut listed: Vec<&Theme> = self
            .themes
            .iter()
            .filter(|t| t.appearance == appearance && t.name != built_in)
            .collect();
        listed.sort_by(|a, b| a.name.cmp(&b.name));
        if let Some(first) = self
            .themes
            .iter()
            .find(|t| t.appearance == appearance && t.name == built_in)
        {
            listed.insert(0, first);
        }
        listed
    }

    /// Every theme there is, in one list — dark and light together.
    ///
    /// The order is the two built-ins first, then everything else by name, so
    /// the list opens on something recognisable rather than on whatever begins
    /// with A. Appearance is a property of a theme here, not a filter over
    /// them: choosing Ayu Light *is* choosing to be in light mode, and a picker
    /// that hides half the themes behind a mode switch makes the person
    /// answer that question twice.
    pub fn all(&self) -> Vec<&Theme> {
        let dark = Theme::of(Appearance::Dark).name;
        let light = Theme::of(Appearance::Light).name;
        let built_in = |t: &Theme| t.name == dark || t.name == light;
        let mut listed: Vec<&Theme> = self.themes.iter().filter(|t| !built_in(t)).collect();
        listed.sort_by(|a, b| a.name.cmp(&b.name));
        let firsts = self
            .themes
            .iter()
            .filter(|t| built_in(t))
            .collect::<Vec<_>>();
        for (at, theme) in firsts.into_iter().enumerate() {
            listed.insert(at, theme);
        }
        listed
    }

    /// Just the names, for anything that does not need the colours.
    pub fn names(&self, appearance: Appearance) -> Vec<SharedString> {
        self.listed(appearance)
            .into_iter()
            .map(|t| t.name.clone())
            .collect()
    }

    /// The named theme, or the built-in for that appearance.
    ///
    /// A name from a build that had a theme this one does not, or from a file
    /// the user has since deleted, is a preference that no longer exists rather
    /// than an error — the same rule the accent picker follows.
    pub fn get(&self, name: &str, appearance: Appearance) -> Theme {
        let of = |pred: &dyn Fn(&Theme) -> bool| {
            self.themes
                .iter()
                .find(|t| t.appearance == appearance && pred(t))
                .cloned()
        };
        of(&|t| t.name == name)
            // The sibling: `One Dark` asked for in a light window is One Light,
            // not the built-in. This is the path a window takes every time it
            // follows the system across, and the settings file only ever holds
            // one name per appearance because of it.
            .or_else(|| {
                let family = self.themes.iter().find(|t| t.name == name)?.family.clone();
                of(&|t| t.family == family)
            })
            // Or the family named on its own, which is what someone writing
            // the file by hand — or the screenshot harness — will type.
            .or_else(|| of(&|t| t.family == name))
            .unwrap_or_else(|| Theme::of(appearance))
    }

    pub fn global(cx: &App) -> Option<&ThemeRegistry> {
        cx.try_global::<ThemeRegistry>()
    }

    /// The named theme, from the global registry if one has been installed.
    ///
    /// Tests, benches and the moment before startup finishes reading the themes
    /// directory all want a theme and none of them have a registry, and "there
    /// is no registry yet" is not a different answer from "that name is not in
    /// it" — both mean the built-in.
    pub fn resolve(name: &str, appearance: Appearance, cx: &App) -> Theme {
        match cx.try_global::<ThemeRegistry>() {
            Some(registry) => registry.get(name, appearance),
            None => Theme::of(appearance),
        }
    }

    /// Install the registry, reading `user_dirs` if they exist.
    pub fn init(user_dirs: &[PathBuf], cx: &mut App) {
        cx.set_global(Self::load(user_dirs));
    }

    /// Where a user's own themes go, given the app's data directory. Here
    /// rather than in the caller so that the answer to "where do I put a theme"
    /// is next to the code that reads it.
    pub fn user_dir(data_dir: &Path) -> PathBuf {
        data_dir.join("themes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_KEY: &str = r##"{"editor.background":"#101010ff"}"##;

    fn file(name: &str, appearance: &str) -> Vec<u8> {
        format!(
            r##"{{"name":"F","themes":[{{"name":"{name}","appearance":"{appearance}","style":{ONE_KEY}}}]}}"##
        )
        .into_bytes()
    }

    #[test]
    fn a_later_file_replaces_an_earlier_one_of_the_same_name() {
        let mut r = ThemeRegistry::new();
        r.add_file("a", &file("Mine", "dark"));
        let before = r.themes.len();
        r.add_file(
            "b",
            br##"{"name":"F","themes":[{"name":"Mine","appearance":"dark","style":{"editor.background":"#202020ff"}}]}"##,
        );
        assert_eq!(r.themes.len(), before, "replaced, not appended");
        assert_eq!(
            r.get("Mine", Appearance::Dark).colors.surface,
            gpui::rgb(0x202020).into()
        );
    }

    #[test]
    fn same_name_in_both_appearances_is_two_themes() {
        // Families do this constantly — `Ayu` ships Light and Dark, and a user
        // theme is entitled to name both halves the same thing.
        let mut r = ThemeRegistry::new();
        r.add_file("a", &file("Twin", "dark"));
        r.add_file("b", &file("Twin", "light"));
        assert_eq!(r.get("Twin", Appearance::Dark).appearance, Appearance::Dark);
        assert_eq!(
            r.get("Twin", Appearance::Light).appearance,
            Appearance::Light
        );
    }

    #[test]
    fn a_broken_file_costs_only_itself() {
        let mut r = ThemeRegistry::new();
        r.add_file("bad", b"{ not json");
        r.add_file("good", &file("Mine", "dark"));
        assert_eq!(r.get("Mine", Appearance::Dark).colors.surface, gpui::rgb(0x101010).into());
    }

    #[test]
    fn a_theme_named_for_the_other_appearance_finds_its_sibling() {
        // What happens every time a window follows the system across: the
        // settings hold "One Dark" and the light window has to land on One
        // Light rather than on the built-in.
        let mut r = ThemeRegistry::new();
        r.add_file(
            "one",
            br##"{"name":"One","themes":[
                {"name":"One Dark","appearance":"dark","style":{"editor.background":"#101010ff"}},
                {"name":"One Light","appearance":"light","style":{"editor.background":"#fefefeff"}}]}"##,
        );
        assert_eq!(r.get("One Dark", Appearance::Light).name, "One Light");
        assert_eq!(r.get("One Light", Appearance::Dark).name, "One Dark");
        // and the family on its own works from either side
        assert_eq!(r.get("One", Appearance::Dark).name, "One Dark");
        assert_eq!(r.get("One", Appearance::Light).name, "One Light");
    }

    #[test]
    fn an_unknown_name_falls_back_rather_than_failing() {
        let r = ThemeRegistry::new();
        assert_eq!(r.get("Gone", Appearance::Dark).name, Theme::dark().name);
        // and does not quietly hand back a dark theme to a light window
        assert_eq!(r.get("Gone", Appearance::Light).appearance, Appearance::Light);
    }

    #[test]
    fn the_built_in_is_listed_first_and_the_rest_alphabetically() {
        let mut r = ThemeRegistry::new();
        r.add_file("a", &file("Zebra", "dark"));
        r.add_file("b", &file("Alpha", "dark"));
        r.add_file("c", &file("Light Only", "light"));
        let names = r.names(Appearance::Dark);
        assert_eq!(names[0], Theme::dark().name);
        assert_eq!(&names[1..], ["Alpha", "Zebra"]);
        assert!(!names.iter().any(|n| n == "Light Only"));
    }
}
