//! Reading a theme written for Zed.
//!
//! The app needs more themes than two, and writing them by hand is writing
//! forty numbers per theme and getting the relationships between them wrong.
//! Editors have solved this: a theme file is a published, versioned artefact
//! with an author and a licence, and Zed's format in particular is a flat map
//! of dotted role names — `editor.background`, `border.variant` — rather than a
//! tree, which makes it a dictionary lookup rather than a schema.
//!
//! So this reads that format instead of inventing one. The mapping is the
//! interesting part, and it is a mapping rather than a rename because Zed is an
//! editor and this is a database client: it has no grid, no result dock, no
//! connection tree, and we have no terminal or diff gutter. Where a role has no
//! counterpart the built-in theme's value survives, which is why this starts
//! from [`Theme::of`] and overwrites rather than building from nothing — a
//! theme file that only sets a background still produces a usable app.
//!
//! What is deliberately *not* taken: type sizes, row heights and radii. Those
//! are this app's, and a theme that could change them would be a theme that
//! could break the grid.

use gpui::{Hsla, SharedString};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::{Appearance, Theme};

/// A theme file: one or more variants published together under a family name.
///
/// Zed ships light and dark as siblings in one file — `One` contains `One Dark`
/// and `One Light` — so a file is a family and the thing a user picks is a
/// variant inside it.
#[derive(Debug, Clone, Deserialize)]
pub struct ThemeFamily {
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub themes: Vec<ThemeVariant>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThemeVariant {
    pub name: String,
    /// `"dark"` or `"light"`. Anything else is read as dark, because a theme
    /// that gets this wrong is far more likely to be dark than to be white.
    #[serde(default)]
    pub appearance: String,
    /// Left as a map rather than a struct with a hundred and forty fields.
    /// Every key is optional in practice — themes in the wild omit whole
    /// families of them — so a struct would be a hundred and forty `Option`s
    /// and a rename away from silently losing a colour.
    #[serde(default)]
    pub style: Map<String, Value>,
}

impl ThemeFamily {
    pub fn parse(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    /// Every variant in the file, each stamped with the family name. The
    /// variants themselves do not carry it — in the format the family is the
    /// object they are nested in.
    pub fn to_themes(&self) -> Vec<Theme> {
        self.themes
            .iter()
            .map(|variant| {
                let mut theme = variant.to_theme();
                theme.family = SharedString::from(self.name.clone());
                theme
            })
            .collect()
    }
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa`. Zed writes the last of these; the others
/// turn up in hand-edited files.
fn parse_hex(text: &str) -> Option<Hsla> {
    let hex = text.trim().strip_prefix('#')?;
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let (rgb, alpha) = match hex.len() {
        3 => {
            let v = u32::from_str_radix(hex, 16).ok()?;
            // `#abc` means `#aabbcc`: each nibble doubled, not shifted.
            let expand = |shift: u32| {
                let n = (v >> shift) & 0xf;
                n << 4 | n
            };
            (expand(8) << 16 | expand(4) << 8 | expand(0), 0xff)
        }
        6 => (u32::from_str_radix(hex, 16).ok()?, 0xff),
        8 => (
            u32::from_str_radix(&hex[..6], 16).ok()?,
            u32::from_str_radix(&hex[6..], 16).ok()?,
        ),
        _ => return None,
    };
    Some(gpui::rgba(rgb << 8 | alpha).into())
}

/// Push `line` away from `plane` until their lightness differs by at least
/// `least`, in whichever direction it already leaned.
fn separate(line: Hsla, plane: Hsla, least: f32) -> Hsla {
    let delta = line.l - plane.l;
    if delta.abs() >= least {
        return line;
    }
    let away = match delta == 0. {
        // A line the theme did not distinguish from its plane at all: go
        // towards the middle, which for a dark theme is lighter and for a
        // light one is darker.
        true => match plane.l > 0.5 {
            true => -1.,
            false => 1.,
        },
        false => delta.signum(),
    };
    // The plane may be against a wall — a near-black panel has nothing darker
    // to put a line on — in which case the seam goes the other way instead of
    // clamping to the plane's own colour and disappearing.
    let out = plane.l + away * least;
    let l = match (0. ..=1.).contains(&out) {
        true => out,
        false => plane.l - away * least,
    };
    Hsla {
        l: l.clamp(0., 1.),
        ..line
    }
}

/// Pull `line` back towards `plane` when their lightness differs by more than
/// `most`, leaving its hue and saturation alone.
///
/// The mirror of [`separate`], and needed for the same reason: an editor puts
/// its hairlines between a gutter and a tab bar, where a strong line is a
/// deliberate edge. This window puts them between two grids and a sidebar, where
/// a strong line is a scaffold drawn over the data.
fn contain(line: Hsla, plane: Hsla, most: f32) -> Hsla {
    let delta = line.l - plane.l;
    if delta.abs() <= most || line.a < 0.99 {
        return line;
    }
    Hsla {
        l: (plane.l + delta.signum() * most).clamp(0., 1.),
        ..line
    }
}

impl ThemeVariant {
    pub fn appearance(&self) -> Appearance {
        match self.appearance.eq_ignore_ascii_case("light") {
            true => Appearance::Light,
            false => Appearance::Dark,
        }
    }

    /// The first of `keys` the file actually has a colour for.
    ///
    /// A list rather than a key because the fallbacks are part of the mapping:
    /// a theme without `border.variant` has `border`, and taking the coarser
    /// line is better than taking ours and having one seam in a foreign colour.
    fn color(&self, keys: &[&str]) -> Option<Hsla> {
        keys.iter()
            .find_map(|k| self.style.get(*k)?.as_str().and_then(parse_hex))
    }

    /// The cursor's own selection colour — Zed's `players` array, first entry.
    fn player_selection(&self) -> Option<Hsla> {
        self.style
            .get("players")?
            .as_array()?
            .first()?
            .get("selection")?
            .as_str()
            .and_then(parse_hex)
    }

    fn syntax(&self, key: &str) -> Option<Hsla> {
        self.style
            .get("syntax")?
            .as_object()?
            .get(key)?
            .get("color")?
            .as_str()
            .and_then(parse_hex)
    }

    /// Build the app's theme from this variant.
    pub fn to_theme(&self) -> Theme {
        let appearance = self.appearance();
        let dark = appearance.is_dark();
        let mut theme = Theme::of(appearance);
        theme.name = SharedString::from(self.name.clone());

        // The accent goes first: it derives half a dozen roles — focus ring,
        // selection, the grid cursor — and the file's own values for those
        // should win over anything derived, not the other way round.
        if let Some(accent) = self.color(&["text.accent", "icon.accent", "info"]) {
            theme = theme.with_accent(accent);
            // Legibility is not the theme's to get wrong. Zed never draws text
            // on a filled accent so it has no key for this.
            theme.colors.text_on_accent = match accent.l > 0.6 {
                true => Hsla {
                    h: accent.h,
                    s: 0.2,
                    l: 0.08,
                    a: 1.,
                },
                false => gpui::rgb(0xffffff).into(),
            };
        }

        let c = &mut theme.colors;
        let take = |field: &mut Hsla, keys: &[&str]| {
            if let Some(v) = self.color(keys) {
                *field = v;
            }
        };

        // ---- planes -------------------------------------------------------
        // Zed's window is a title bar and a tab bar — one frame, two heights —
        // with an editor set into it, and that is exactly the split this app
        // draws: `background`/`chrome` are the frame, `panel` is everything
        // held inside it. So the content plane comes from `editor.background`
        // rather than from `panel.background`: Zed's project panel is chrome
        // and shares the tab bar's colour, while our sidebar is content and
        // shares the grid's. Taking Zed's panel colour here would put the
        // sidebar on the frame plane and leave the active tab darker than the
        // tree it opens onto, which is the notch upside down.
        //
        // Both halves of the frame come from the *tab bar*, including the one
        // Zed would call the title bar. Zed's title bar is a lighter shelf
        // above everything — Ayu Dark puts `#313337` over a `#0d1016` editor,
        // Gruvbox Dark Hard `#4c4642` over `#1d2021` — which works there
        // because it is a thin strip holding project and branch names. Here it
        // would be a 32px band across the top and a 22px band across the
        // bottom in a colour the rest of the window never uses again, with the
        // tab strip a visibly different tone one pixel below it. So the
        // titlebar joins the tab strip on one plane, the way it does in the
        // built-in themes, and `title_bar.background` is only the fallback for
        // a file that has no tab bar at all.
        take(
            &mut c.background,
            &["tab_bar.background", "title_bar.background"],
        );
        take(
            &mut c.chrome,
            &["tab_bar.background", "title_bar.background"],
        );
        take(&mut c.panel, &["editor.background", "panel.background"]);
        take(&mut c.surface, &["editor.background"]);
        take(&mut c.overlay, &["elevated_surface.background"]);
        take(&mut c.field, &["element.background"]);
        take(
            &mut c.tab_active,
            &["tab.active_background", "editor.background"],
        );

        // ---- lines --------------------------------------------------------
        // `border.variant` is the hairline between panes; plain `border` is the
        // heavier one Zed uses around a focused thing. That is our two weights
        // in the same order.
        take(&mut c.border, &["border.variant", "border"]);
        take(&mut c.border_strong, &["border"]);
        take(&mut c.border_focus, &["border.focused"]);
        // The seam does more work here than it does in an editor. Zed puts a
        // gutter and a tab bar either side of its dividers, so a line that
        // barely registers is still enough; this window puts two grids either
        // side of one, and the line is the only thing saying they are two
        // regions. So the hairline gets a floor, and a theme that already draws
        // it stronger than the floor keeps exactly what it drew.
        // …and a ceiling, which is the complaint the floor cannot answer. Fleet
        // hands both weights one colour a sixth of the way up from its plane;
        // Gruvbox Dark Hard puts its heavy line a fifth of the way up. In an
        // editor that is one rule around one pane. Here it is the outline of
        // every field, button, split and dock at once, and the window stops
        // being a surface with things on it and becomes a wireframe of itself.
        // The band is the built-in dark theme's own: One Dark already sits
        // inside it, which is why One Dark already looks right.
        if dark {
            c.border = contain(c.border, c.panel, 0.09);
            c.border_strong = contain(c.border_strong, c.panel, 0.14);
        }
        c.border = separate(c.border, c.panel, 0.06);
        c.border_strong = separate(c.border_strong, c.panel, 0.12);
        // A Zed palette has one hairline where this window wants two. The seam
        // is that line drawn most of the way back towards the plane it sits on:
        // enough to confirm an edge the plane step already made, not enough to
        // be the edge itself.
        c.seam = separate(
            Hsla {
                l: c.panel.l + (c.border.l - c.panel.l) * 0.35,
                ..c.border
            },
            c.panel,
            0.015,
        );
        take(&mut c.scrollbar_thumb, &["scrollbar.thumb.background"]);
        take(
            &mut c.scrollbar_thumb_hover,
            &["scrollbar.thumb.hover_background"],
        );

        // ---- text ---------------------------------------------------------
        take(&mut c.text, &["text"]);
        take(&mut c.text_muted, &["text.muted"]);
        take(&mut c.text_subtle, &["text.placeholder", "text.disabled"]);
        take(&mut c.text_disabled, &["text.disabled"]);

        // ---- states -------------------------------------------------------
        take(&mut c.hover, &["element.hover", "ghost_element.hover"]);
        take(&mut c.active, &["element.active", "ghost_element.active"]);
        // Our focused selection stays accent-tinted — a selected row has to
        // read as *chosen*, not merely hovered — but the unfocused one is
        // exactly what Zed means by a selected element in a panel that is not
        // the one you are typing in.
        take(&mut c.selected_inactive, &["element.selected"]);
        if let Some(v) = self.player_selection() {
            c.text_selection = v;
        }

        // ---- signals ------------------------------------------------------
        take(&mut c.success, &["success", "created"]);
        take(&mut c.warning, &["warning", "modified"]);
        take(&mut c.danger, &["error", "deleted"]);
        take(&mut c.info, &["info"]);
        take(
            &mut c.success_bg,
            &["success.background", "created.background"],
        );
        take(
            &mut c.warning_bg,
            &["warning.background", "modified.background"],
        );
        take(
            &mut c.danger_bg,
            &["error.background", "deleted.background"],
        );
        take(&mut c.info_bg, &["info.background"]);

        // ---- editor -------------------------------------------------------
        take(
            &mut c.editor_active_line,
            &["editor.active_line.background"],
        );
        take(
            &mut c.editor_active_statement,
            &["editor.highlighted_line.background"],
        );
        take(&mut c.search_match, &["search.match_background"]);
        take(&mut c.editor_line_number, &["editor.line_number"]);
        take(
            &mut c.editor_line_number_active,
            &["editor.active_line_number", "editor.hover_line_number"],
        );

        // ---- grid ---------------------------------------------------------
        // A cell with an uncommitted edit is a modified line; a row queued for
        // deletion is a deleted one. Zed's diff colours are already tuned to
        // sit under text without eating it, which is the whole requirement.
        take(
            &mut c.grid_dirty,
            &["modified.background", "conflict.background"],
        );
        take(
            &mut c.grid_deleted,
            &["deleted.background", "error.background"],
        );
        take(
            &mut c.grid_inserted,
            &["created.background", "success.background"],
        );
        // No editor has a zebra stripe, so this is the one plane that has to be
        // invented. One step off the page in whichever direction the page is
        // not — small enough that it reads as ruling rather than as banding.
        c.grid_stripe = Hsla {
            l: (c.panel.l + if dark { 0.022 } else { -0.022 }).clamp(0., 1.),
            ..c.panel
        };

        // ---- syntax -------------------------------------------------------
        // SQL is a small language and Zed's palette is built for large ones, so
        // several of ours collapse onto one of theirs. `property` for column
        // references is the one worth arguing about: in a `select` list a bare
        // name is a column, which is much closer to a struct field than to a
        // local variable, and every one of these themes gives fields their own
        // colour precisely so they stand out from the expression around them.
        let s = &mut theme.syntax;
        let take_syntax = |field: &mut Hsla, keys: &[&str]| {
            if let Some(v) = keys.iter().find_map(|k| self.syntax(k)) {
                *field = v;
            }
        };
        take_syntax(&mut s.keyword, &["keyword"]);
        take_syntax(&mut s.type_name, &["type", "constructor"]);
        take_syntax(&mut s.function, &["function"]);
        take_syntax(&mut s.identifier, &["variable", "primary"]);
        take_syntax(&mut s.string, &["string"]);
        take_syntax(&mut s.number, &["number", "constant"]);
        take_syntax(&mut s.comment, &["comment"]);
        take_syntax(&mut s.operator, &["operator"]);
        take_syntax(
            &mut s.punctuation,
            &["punctuation", "punctuation.delimiter"],
        );
        take_syntax(&mut s.variable, &["property", "variable.parameter"]);
        if let Some(v) = self.color(&["error", "deleted"]) {
            s.invalid = v;
        }

        theme
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_in_every_length() {
        let six = parse_hex("#282c33").unwrap();
        let eight = parse_hex("#282c33ff").unwrap();
        assert_eq!(six, eight);
        assert_eq!(eight.a, 1.);
        // Each nibble doubled, not shifted into the high half.
        assert_eq!(parse_hex("#abc").unwrap(), parse_hex("#aabbcc").unwrap());
        let half = parse_hex("#00000080").unwrap();
        assert!((half.a - 0.5).abs() < 0.01, "{half:?}");
    }

    #[test]
    fn nonsense_is_none_rather_than_black() {
        // A key whose value is `null`, a name, or a truncated hex has to fall
        // through to the built-in colour. Reading it as black would give a
        // theme with one invisible seam and no error anywhere.
        assert!(parse_hex("").is_none());
        assert!(parse_hex("transparent").is_none());
        assert!(parse_hex("#12345").is_none());
        assert!(parse_hex("#gggggg").is_none());
    }

    fn family(style: &str) -> ThemeFamily {
        let json = format!(
            r##"{{"name":"T","author":"a","themes":[{{"name":"T Dark","appearance":"dark","style":{style}}}]}}"##
        );
        ThemeFamily::parse(json.as_bytes()).unwrap()
    }

    #[test]
    fn the_titlebar_joins_the_tab_strip_rather_than_zeds_shelf() {
        // Zed's title bar is a lighter shelf over the editor. Ours is a band
        // welded to the tab strip below it, so the strip's colour wins and the
        // shelf is not used at all when there is a strip to take.
        let f = family(
            r##"{"title_bar.background":"#313337ff","tab_bar.background":"#1f2127ff","editor.background":"#0d1016ff"}"##,
        );
        let c = f.themes[0].to_theme().colors;
        assert_eq!(c.background, parse_hex("#1f2127").unwrap());
        assert_eq!(c.background, c.chrome);
    }

    #[test]
    fn a_file_with_no_tab_bar_still_gets_a_frame() {
        let f = family(r##"{"title_bar.background":"#313337ff"}"##);
        let c = f.themes[0].to_theme().colors;
        assert_eq!(c.background, parse_hex("#313337").unwrap());
        assert_eq!(c.background, c.chrome);
    }

    #[test]
    fn a_nearly_empty_file_still_makes_a_usable_theme() {
        // The failure this guards against is a theme that sets four keys and
        // leaves the app with black text on black.
        let f = family(r##"{"editor.background":"#101010ff"}"##);
        let theme = f.themes[0].to_theme();
        assert_eq!(theme.colors.surface, parse_hex("#101010").unwrap());
        assert_eq!(theme.colors.text, Theme::dark().colors.text);
        assert_eq!(theme.syntax.keyword, Theme::dark().syntax.keyword);
        // The built-in's line, but measured against the file's plane rather
        // than the one it was picked for: a hairline is a distance from a
        // background, and this file moved the background.
        assert!(theme.colors.border.l - theme.colors.panel.l <= 0.091);
    }

    #[test]
    fn unknown_keys_and_null_values_are_not_errors() {
        let f =
            family(r##"{"pane.focused_border":null,"whatever.new":"#ffffff","text":"#eeeeee"}"##);
        let theme = f.themes[0].to_theme();
        assert_eq!(theme.colors.text, parse_hex("#eeeeee").unwrap());
    }

    #[test]
    fn the_files_accent_stands_in_for_ours() {
        let f = family(r##"{"text.accent":"#74ade8ff"}"##);
        let theme = f.themes[0].to_theme();
        assert_eq!(theme.colors.accent, parse_hex("#74ade8").unwrap());
        // and everything derived from it followed
        assert_eq!(theme.colors.border_focus, theme.colors.accent);
        assert_eq!(theme.colors.grid_cursor, theme.colors.accent);
    }

    #[test]
    fn a_pale_accent_gets_dark_text_on_it() {
        let f = family(r##"{"text.accent":"#f0e68c"}"##);
        let theme = f.themes[0].to_theme();
        assert!(theme.colors.text_on_accent.l < 0.2);
    }

    #[test]
    fn appearance_decides_which_built_in_is_the_floor() {
        let json = r##"{"name":"T","themes":[{"name":"L","appearance":"light","style":{}}]}"##;
        let f = ThemeFamily::parse(json.as_bytes()).unwrap();
        let theme = f.themes[0].to_theme();
        assert_eq!(theme.appearance, Appearance::Light);
        assert_eq!(theme.colors.text, Theme::light().colors.text);
    }

    #[test]
    fn a_line_too_bright_to_ignore_is_pulled_back_onto_its_plane() {
        // Fleet Dark: one colour for both weights, a sixth of the way up from
        // the plane. Every field and split in the window outlined in it reads
        // as a wireframe drawn over the data.
        let f = family(
            r##"{"panel.background":"#18191bff","editor.background":"#18191bff","border":"#3e4147ff"}"##,
        );
        let c = f.themes[0].to_theme().colors;
        assert!(c.border.l - c.panel.l <= 0.091, "{:?}", c.border);
        assert!(
            c.border_strong.l - c.panel.l <= 0.141,
            "{:?}",
            c.border_strong
        );
        // The two weights part company, which the file did not let them do.
        assert!(c.border_strong.l > c.border.l);
        // and the theme's hue survives being turned down
        assert_eq!(c.border.h, parse_hex("#3e4147").unwrap().h);
    }

    #[test]
    fn a_line_already_quiet_enough_is_left_alone() {
        // One Dark sits inside the band the built-in dark theme occupies, and
        // is the reason the band is where it is.
        let f = family(
            r##"{"editor.background":"#282c33ff","border.variant":"#363c46ff","border":"#464b57ff"}"##,
        );
        let c = f.themes[0].to_theme().colors;
        assert_eq!(c.border, parse_hex("#363c46").unwrap());
        assert_eq!(c.border_strong, parse_hex("#464b57").unwrap());
    }

    #[test]
    fn a_seam_too_faint_to_see_is_pushed_off_its_plane() {
        // One Dark draws its pane divider three points off the panel, which is
        // fine either side of a gutter and not fine between two grids.
        let f = family(
            r##"{"panel.background":"#2f343eff","border.variant":"#31363fff","border":"#33383fff"}"##,
        );
        let c = f.themes[0].to_theme().colors;
        assert!((c.border.l - c.panel.l).abs() >= 0.059, "{:?}", c.border);
        assert!((c.border_strong.l - c.panel.l).abs() >= 0.119);
        // and it stayed on the side of the plane the theme put it on
        assert!(c.border.l > c.panel.l);
    }

    #[test]
    fn the_quiet_seam_lands_between_the_plane_and_the_hairline() {
        let f = family(r##"{"panel.background":"#101010ff","border.variant":"#606060ff"}"##);
        let c = f.themes[0].to_theme().colors;
        assert!(
            c.seam.l > c.panel.l && c.seam.l < c.border.l,
            "{:?}",
            c.seam
        );
    }

    #[test]
    fn a_seam_the_theme_already_draws_clearly_is_left_alone() {
        let f = family(r##"{"editor.background":"#101010ff","border.variant":"#242424ff"}"##);
        let c = f.themes[0].to_theme().colors;
        assert_eq!(c.border, parse_hex("#242424").unwrap());
    }

    #[test]
    fn a_seam_with_nowhere_to_go_turns_round() {
        // Black panel: there is no darker line, so it has to become a lighter
        // one rather than clamp to black and vanish.
        let f = family(r##"{"panel.background":"#000000ff","border.variant":"#000000ff"}"##);
        let c = f.themes[0].to_theme().colors;
        assert!(c.border.l >= 0.06, "{:?}", c.border);
    }

    #[test]
    fn the_zebra_stripe_is_derived_and_close_to_the_page() {
        let f = family(r##"{"editor.background":"#282c33ff"}"##);
        let theme = f.themes[0].to_theme();
        let delta = (theme.colors.grid_stripe.l - theme.colors.surface.l).abs();
        assert!(delta > 0.005 && delta < 0.05, "{delta}");
    }
}
