//! Design tokens.
//!
//! Everything visual in Tupli resolves through [`Theme`]. Components never name a
//! literal colour; they name a role (`surface`, `border`, `text_muted`) and the
//! theme decides. That is the only way a two-appearance app stays consistent.
//!
//! The scale is deliberately small. Three background planes, three text weights,
//! two borders, one accent — if a component needs a fourth grey it is usually a
//! sign the component is wrong, not the palette.

use gpui::{font, px, rgb, rgba, App, Font, FontFeatures, Global, Hsla, SharedString};

/// Which of the two built-in appearances a theme is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Appearance {
    Dark,
    Light,
}

impl Appearance {
    pub fn is_dark(self) -> bool {
        matches!(self, Appearance::Dark)
    }
}

/// The colour roles used by every component.
#[derive(Clone, Debug)]
pub struct ThemeColors {
    // ---- background planes, back to front -------------------------------
    /// The bands welded to the window's top and bottom edges: the titlebar and
    /// the status bar. Nothing else is drawn on it now that the regions reach
    /// the window edge, which is why it is the *lightest* plane in the dark
    /// theme rather than the darkest — it frames the window instead of
    /// standing behind it.
    pub background: Hsla,
    /// Chrome: tab strips, table headers, toolbars. One plane with
    /// [`background`](Self::background), because a tab strip and the titlebar
    /// above it are the same band of the window seen at two heights — the
    /// content is what is set into them.
    pub chrome: Hsla,
    /// Panels that flank the content: sidebars, inspectors, docks.
    pub panel: Hsla,
    /// The content plane itself: editor, grid, results. The darkest plane in
    /// the dark theme and the lightest in the light one — whichever direction
    /// it goes, this is the page and everything else is the desk it is on.
    pub surface: Hsla,
    /// Floating things: menus, popovers, the command palette.
    pub overlay: Hsla,
    /// Recessed: text inputs, filter fields, the query console. Darker than the
    /// panel it sits on, so a field reads as a hole rather than as a raised chip.
    pub field: Hsla,
    /// The selected tab. Equal to `panel` — the plane the tab fronts — so the
    /// front tab reads as a notch cut through the strip onto the content below
    /// it rather than as a chip laid on top, which is the whole reason a tab is
    /// drawn where it is.
    pub tab_active: Hsla,

    // ---- separators ------------------------------------------------------
    /// Hairlines between regions and rows.
    pub border: Hsla,
    /// Emphasised separators: the divider a splitter sits on, focused inputs.
    pub border_strong: Hsla,
    /// Draws around the focused control.
    pub border_focus: Hsla,

    // ---- scrollbars ------------------------------------------------------
    /// Overlay scrollbar thumb at rest. Translucent, like every macOS overlay
    /// scrollbar: it sits on top of content, so an opaque thumb would punch a
    /// hole through the last column of every wide table.
    pub scrollbar_thumb: Hsla,
    /// Thumb while the pointer is anywhere over its track, or dragging it.
    pub scrollbar_thumb_hover: Hsla,

    // ---- text ------------------------------------------------------------
    /// Primary reading colour.
    pub text: Hsla,
    /// Secondary: labels, counts, column types.
    pub text_muted: Hsla,
    /// Tertiary: row ordinals, placeholder, NULL.
    pub text_subtle: Hsla,
    /// Non-interactive.
    pub text_disabled: Hsla,
    /// Text drawn on top of `accent`.
    pub text_on_accent: Hsla,

    // ---- interaction -----------------------------------------------------
    /// Translucent wash for hover. Layered over whatever is beneath.
    pub hover: Hsla,
    /// Translucent wash for pressed / toggled-on.
    pub active: Hsla,
    /// The single brand colour: selection, focus, primary buttons, links.
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub accent_active: Hsla,
    /// Selected row / selected tree item background.
    pub selected: Hsla,
    /// Same, when the owning pane does not have focus.
    pub selected_inactive: Hsla,
    /// Text selection inside an editor or input.
    pub text_selection: Hsla,

    // ---- status ----------------------------------------------------------
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
    pub info: Hsla,
    /// Tinted backgrounds for the banner variants of the above.
    pub success_bg: Hsla,
    pub warning_bg: Hsla,
    pub danger_bg: Hsla,
    pub info_bg: Hsla,

    // ---- editor & grid ---------------------------------------------------
    /// Full-width band behind the line the cursor is on.
    pub editor_active_line: Hsla,
    /// Outline around the statement that ⌘⏎ would run. Translucent: it is
    /// drawn over the console's own background and over the active-line band,
    /// and an opaque hairline crossing both would change colour halfway down.
    pub editor_active_statement: Hsla,
    /// Line-number colour, and the current line number when brighter.
    pub editor_line_number: Hsla,
    pub editor_line_number_active: Hsla,
    /// Cell background for a row edited but not yet committed.
    pub grid_dirty: Hsla,
    /// Cell background for a row queued for deletion.
    pub grid_deleted: Hsla,
    /// Cell background for a row inserted but not yet committed.
    pub grid_inserted: Hsla,
    /// Every other row, when zebra striping is on.
    pub grid_stripe: Hsla,
    /// Border of the focused cell.
    pub grid_cursor: Hsla,
}

/// Colours for SQL (and JSON) tokens.
#[derive(Clone, Debug)]
pub struct SyntaxTheme {
    pub keyword: Hsla,
    pub type_name: Hsla,
    pub function: Hsla,
    pub identifier: Hsla,
    pub string: Hsla,
    pub number: Hsla,
    pub comment: Hsla,
    pub operator: Hsla,
    pub punctuation: Hsla,
    pub variable: Hsla,
    pub invalid: Hsla,
}

/// Font families and the type ramp.
#[derive(Clone, Debug)]
///
/// Every size in the app comes from here. There are four steps and no fifth:
/// three for the UI face and one for code. A component that wants a size the
/// ramp does not have is asking for the wrong emphasis, not for a new number —
/// which is why nothing outside this struct is allowed to write a `px` into a
/// `text_size`.
pub struct Typography {
    pub ui_family: SharedString,
    pub mono_family: SharedString,
    /// Body / control text.
    pub ui_size: gpui::Pixels,
    /// Labels, counts, badges.
    pub ui_size_sm: gpui::Pixels,
    /// Dialog and section titles.
    pub ui_size_lg: gpui::Pixels,
    /// Editor and grid cells.
    pub mono_size: gpui::Pixels,
    pub ui_line_height: gpui::Pixels,
    pub ui_line_height_sm: gpui::Pixels,
    pub ui_line_height_lg: gpui::Pixels,
    pub mono_line_height: gpui::Pixels,
}

/// Layout constants that must agree across unrelated components — a tab strip in
/// the sidebar and one in the bottom dock have to be the same height or the app
/// looks assembled rather than designed.
#[derive(Clone, Debug)]
pub struct Metrics {
    pub titlebar_height: gpui::Pixels,
    /// Deliberately the same as the titlebar and the toolbar: the three of them
    /// stack directly on top of each other at the head of the window, and a
    /// strip that stood taller than its neighbours read as a band bolted on
    /// rather than as the middle course of one frame.
    pub tab_strip_height: gpui::Pixels,
    pub toolbar_height: gpui::Pixels,
    pub status_bar_height: gpui::Pixels,
    pub row_height: gpui::Pixels,
    pub grid_header_height: gpui::Pixels,
    pub grid_row_height: gpui::Pixels,
    /// Indent per level in the schema tree.
    pub tree_indent: gpui::Pixels,
    pub panel_min_width: gpui::Pixels,
    pub panel_default_width: gpui::Pixels,
    pub dock_default_height: gpui::Pixels,
    /// Grab area of a splitter; wider than the 1px line it draws.
    pub splitter_hit_width: gpui::Pixels,
    pub radius_sm: gpui::Pixels,
    pub radius: gpui::Pixels,
    pub radius_lg: gpui::Pixels,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            titlebar_height: px(32.),
            tab_strip_height: px(32.),
            toolbar_height: px(32.),
            status_bar_height: px(22.),
            row_height: px(24.),
            grid_header_height: px(28.),
            grid_row_height: px(24.),
            tree_indent: px(14.),
            panel_min_width: px(180.),
            panel_default_width: px(272.),
            dock_default_height: px(280.),
            splitter_hit_width: px(6.),
            radius_sm: px(4.),
            radius: px(6.),
            radius_lg: px(7.),
        }
    }
}

/// The active theme. Installed as a gpui global; read through [`ActiveTheme`].
#[derive(Clone, Debug)]
pub struct Theme {
    /// What the theme picker shows — `"Ayu Dark"`, `"Vesper"`.
    pub name: SharedString,
    /// The set this theme belongs to, so the picker can group the light and
    /// dark cuts of one design together instead of scattering them through an
    /// alphabetical list.
    pub family: SharedString,
    pub appearance: Appearance,
    pub colors: ThemeColors,
    pub syntax: SyntaxTheme,
    pub typography: Typography,
    pub metrics: Metrics,
}

impl Global for Theme {}

impl Theme {
    pub fn dark() -> Self {
        Self {
            name: "Tupli Dark".into(),
            family: "Tupli".into(),
            appearance: Appearance::Dark,
            colors: ThemeColors::dark(),
            syntax: SyntaxTheme::dark(),
            typography: Typography::default(),
            metrics: Metrics::default(),
        }
    }

    pub fn light() -> Self {
        Self {
            name: "Tupli Light".into(),
            family: "Tupli".into(),
            appearance: Appearance::Light,
            colors: ThemeColors::light(),
            syntax: SyntaxTheme::light(),
            typography: Typography::default(),
            metrics: Metrics::default(),
        }
    }

    /// The two built-in appearances, by name.
    pub fn of(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Dark => Self::dark(),
            Appearance::Light => Self::light(),
        }
    }

    /// Install the theme, resolving fonts against what the platform has.
    ///
    /// The sizes come in untouched: by the time a theme reaches here it may
    /// have been through Settings, and the face is the only part of typography
    /// this machine gets a vote on.
    pub fn set_global(mut theme: Theme, cx: &mut App) {
        theme.typography.resolve_families(cx);
        cx.set_global(theme);
    }

    /// Swap dark for light and back. The caller is responsible for asking the
    /// window to redraw: a global is data, not an observable, so changing it
    /// does not by itself invalidate anything that read it.
    pub fn toggle_appearance(cx: &mut App) {
        let next = match cx.global::<Theme>().appearance {
            Appearance::Dark => Appearance::Light,
            Appearance::Light => Appearance::Dark,
        };
        Self::set_global(Self::of(next), cx);
    }
}


impl Theme {
    /// Recolour everything that is the brand colour.
    ///
    /// Ten roles rather than one: a blue focus ring around an amber-accented
    /// app would look like a bug, and it is the derived surfaces — the selected
    /// row, the active editor line — that give the appearance its temperature.
    /// Each keeps the saturation and lightness the hand-picked blue had, so the
    /// default accent comes out where it started and every other one arrives in
    /// the same register.
    pub fn with_accent(mut self, accent: Hsla) -> Self {
        let dark = self.appearance.is_dark();
        let h = accent.h;
        let c = &mut self.colors;
        c.accent = accent;
        // What is legible on top of it. The six the picker offers run from a
        // 51%-lightness blue to a 66% amber, and white holds on one and not on
        // the other; asking the colour is the only way this survives someone
        // adding a seventh.
        c.text_on_accent = match accent.l > 0.6 {
            true => Hsla {
                h: accent.h,
                s: 0.1,
                l: 0.08,
                a: 1.,
            },
            false => gpui::white(),
        };
        c.accent_hover = lighten(accent, if dark { 0.06 } else { -0.06 });
        c.accent_active = lighten(accent, if dark { -0.06 } else { -0.12 });
        c.border_focus = accent;
        c.info = accent;
        c.grid_cursor = accent;
        c.text_selection = Hsla {
            a: if dark { 0.35 } else { 0.25 },
            ..accent
        };
        c.info_bg = Hsla { a: 0.12, ..accent };
        c.selected = match dark {
            true => Hsla {
                h,
                s: 0.51,
                l: 0.225,
                a: 1.,
            },
            false => Hsla {
                h,
                s: 0.92,
                l: 0.925,
                a: 1.,
            },
        };
        c.editor_active_line = match dark {
            true => Hsla {
                h,
                s: 0.53,
                l: 0.17,
                a: 1.,
            },
            false => Hsla {
                h,
                s: 0.83,
                l: 0.97,
                a: 1.,
            },
        };
        self
    }


    /// Resize the code face. The leading follows at 1.6×, rounded to a whole
    /// pixel, because a fractional line height puts every row of the grid on a
    /// different subpixel and the text starts to shimmer as it scrolls.
    pub fn with_mono_size(mut self, size: gpui::Pixels) -> Self {
        self.typography.mono_size = size;
        self.typography.mono_line_height = px((f32::from(size) * 1.6).round());
        self
    }

    /// Use a named code face. Empty means "whatever this machine has".
    pub fn with_mono_family(mut self, family: impl Into<SharedString>) -> Self {
        self.typography.mono_family = family.into();
        self
    }
}

/// Move a colour along its own lightness, staying inside the range.
fn lighten(color: Hsla, delta: f32) -> Hsla {
    Hsla {
        l: (color.l + delta).clamp(0., 1.),
        ..color
    }
}

impl ThemeColors {
    fn dark() -> Self {
        // Vesper's palette, which is a neutral one: not a single grey in it
        // has a hue, and the only colours are a peach and a mint. A database
        // client spends its day showing coloured state — dirty cells, failed
        // statements, a dozen syntax classes — and a chrome that is already
        // slightly blue is competing with all of it. Pure grey is the only
        // background that lets a warning be the warmest thing on screen.
        let accent = rgb(0xffc799).into();
        Self {
            // Two planes that matter and two that support them. The window
            // is a frame with content set into it: `background` and `chrome`
            // are the frame — titlebar, status bar, every tab strip — and
            // `panel` is everything the frame holds. The step between them is
            // what makes a tab read as a notch, so unlike the other pairs
            // here it is meant to be seen.
            background: rgb(0x1b1b1b).into(),
            chrome: rgb(0x1b1b1b).into(),
            panel: rgb(0x131313).into(),
            surface: rgb(0x0f0f0f).into(),
            overlay: rgb(0x1c1c1c).into(),
            field: rgb(0x0c0c0c).into(),
            tab_active: rgb(0x131313).into(),

            // Vesper's own border is its background — the theme separates
            // regions by lightness alone. That works in an editor with two
            // regions and not in a window with seven, so this is the one
            // value taken from its element scale instead: the lightest thing
            // in the palette that is still unmistakably a line and not a plane.
            border: rgb(0x282828).into(),
            border_strong: rgb(0x3b3b3b).into(),
            border_focus: accent,

            scrollbar_thumb: rgba(0xffffff30).into(),
            scrollbar_thumb_hover: rgba(0xffffff66).into(),

            text: rgb(0xffffff).into(),
            text_muted: rgb(0xa0a0a0).into(),
            text_subtle: rgb(0x7e7e7e).into(),
            text_disabled: rgb(0x585858).into(),
            // The accent is a pale peach, so what goes on top of it is the
            // background, not white on white.
            text_on_accent: rgb(0x131313).into(),

            hover: rgba(0xffffff0f).into(),
            active: rgba(0xffffff1a).into(),
            accent,
            accent_hover: rgb(0xffd7b3).into(),
            accent_active: rgb(0xe9ad7e).into(),
            selected: rgb(0x36291f).into(),
            selected_inactive: rgb(0x232323).into(),
            text_selection: rgba(0xffc79945).into(),

            success: rgb(0x99ffe4).into(),
            warning: rgb(0xffc799).into(),
            danger: rgb(0xff8080).into(),
            info: accent,
            success_bg: rgba(0x99ffe41f).into(),
            warning_bg: rgba(0xffc7991f).into(),
            danger_bg: rgba(0xff80801f).into(),
            info_bg: rgba(0xffc7991f).into(),

            // Barely there, on purpose. The caret already says which line it
            // is on; a band across the console is a second answer to a
            // question that was not asked twice, and in a palette this flat it
            // would be the loudest shape on the page.
            editor_active_line: rgb(0x181818).into(),
            editor_active_statement: rgba(0x99ffe45c).into(),
            editor_line_number: rgb(0x505050).into(),
            editor_line_number_active: rgb(0xffffff).into(),
            grid_dirty: rgba(0xffc79926).into(),
            grid_deleted: rgba(0xff808026).into(),
            grid_inserted: rgba(0x99ffe426).into(),
            grid_stripe: rgba(0xffffff05).into(),
            grid_cursor: accent,
        }
    }

    fn light() -> Self {
        // The dark theme's peach, brought down until it reads on paper.
        // Vesper's own accent is `#ffc799`, which is a colour for a black
        // ground and invisible on a white one — so light keeps the hue and
        // trades the lightness away. It lands on the same amber the warning
        // role already uses, which is deliberate and is what dark does too:
        // one warm in the palette, not two that are nearly the same.
        let accent = rgb(0xb45309).into();
        Self {
            // The same four planes, in the order light wants them: the page is
            // paper and everything around it is a shade of desk.
            background: rgb(0xe4e4e8).into(),
            chrome: rgb(0xe4e4e8).into(),
            panel: rgb(0xf1f1f3).into(),
            surface: rgb(0xfdfdfd).into(),
            overlay: rgb(0xffffff).into(),
            field: rgb(0xffffff).into(),
            tab_active: rgb(0xf1f1f3).into(),

            border: rgb(0xdcdce0).into(),
            border_strong: rgb(0xc2c2c9).into(),
            border_focus: accent,

            scrollbar_thumb: rgba(0x00000038).into(),
            scrollbar_thumb_hover: rgba(0x00000066).into(),

            text: rgb(0x1b1b1f).into(),
            text_muted: rgb(0x60606a).into(),
            text_subtle: rgb(0x8b8b95).into(),
            text_disabled: rgb(0xb4b4bc).into(),
            text_on_accent: rgb(0xffffff).into(),

            hover: rgba(0x0000000a).into(),
            active: rgba(0x00000014).into(),
            accent,
            accent_hover: rgb(0x92400e).into(),
            accent_active: rgb(0x78350f).into(),
            selected: rgb(0xfaead8).into(),
            selected_inactive: rgb(0xe4e4e8).into(),
            text_selection: rgba(0xb4530940).into(),

            success: rgb(0x0f9d58).into(),
            warning: rgb(0xb45309).into(),
            danger: rgb(0xd42f2f).into(),
            info: accent,
            success_bg: rgba(0x0f9d581f).into(),
            warning_bg: rgba(0xb453091f).into(),
            danger_bg: rgba(0xd42f2f1f).into(),
            info_bg: rgba(0xb453091f).into(),

            editor_active_line: rgb(0xfbf5ee).into(),
            editor_active_statement: rgba(0x0f9d5866).into(),
            editor_line_number: rgb(0xb0b0b8).into(),
            editor_line_number_active: rgb(0x50505a).into(),
            grid_dirty: rgba(0xb4530926).into(),
            grid_deleted: rgba(0xd42f2f26).into(),
            grid_inserted: rgba(0x0f9d5826).into(),
            grid_stripe: rgba(0x00000005).into(),
            grid_cursor: accent,
        }
    }
}

impl SyntaxTheme {
    fn dark() -> Self {
        // Four colours for eleven roles, which is Vesper's whole argument:
        // grammar in grey, values in peach, text in mint, names in white. A
        // `select` list where the keywords, the identifiers, the schema and
        // the literals are four different hues is a rainbow that has stopped
        // saying which of them is the value you came to read.
        Self {
            keyword: rgb(0xa0a0a0).into(),
            type_name: rgb(0xffc799).into(),
            function: rgb(0xffc799).into(),
            identifier: rgb(0xffffff).into(),
            string: rgb(0x99ffe4).into(),
            number: rgb(0xffc799).into(),
            comment: rgba(0x8b8b8b94).into(),
            operator: rgb(0xa0a0a0).into(),
            punctuation: rgb(0xa0a0a0).into(),
            variable: rgb(0xffffff).into(),
            invalid: rgb(0xff8080).into(),
        }
    }

    fn light() -> Self {
        Self {
            keyword: rgb(0x1a7f6d).into(),
            type_name: rgb(0x1d5fa8).into(),
            function: rgb(0x7038a8).into(),
            identifier: rgb(0x1b1b1f).into(),
            string: rgb(0x8a6100).into(),
            number: rgb(0xa04a1a).into(),
            comment: rgb(0x9a9aa2).into(),
            operator: rgb(0x50505a).into(),
            punctuation: rgb(0x7a7a84).into(),
            variable: rgb(0x1d5fa8).into(),
            invalid: rgb(0xd42f2f).into(),
        }
    }
}

/// Monospace families we are happy to render code in, best first. Resolved
/// against what the platform actually has installed — asking for a family that
/// is not there does not error, it silently falls back to a proportional face,
/// which is far worse than picking second choice deliberately.
pub const MONO_FALLBACKS: &[&str] = &["Geist Mono", "Berkeley Mono", "Menlo", "Monaco"];

impl Typography {
    /// The code face, ligatures off.
    ///
    /// Geist Mono fuses `>=` into `≥` and `!=` into `≠`. That is charming in
    /// prose and wrong here: the entire job of this app is showing you exactly
    /// the bytes you typed and exactly the bytes the server sent back. Every
    /// mono run in the app goes through this.
    ///
    /// Three tags rather than gpui's `disable_ligatures`, which sets `calt`
    /// alone: Geist Mono has no `calt` table at all — its operator fusions live
    /// in `liga` — while Fira Code and JetBrains Mono put theirs in `calt`. We
    /// do not know which face `detect` will land on, so turn off all three.
    pub fn mono_font(&self) -> Font {
        Font {
            features: FontFeatures(std::sync::Arc::new(vec![
                ("liga".into(), 0),
                ("dlig".into(), 0),
                ("calt".into(), 0),
            ])),
            ..font(self.mono_family.clone())
        }
    }

    /// Point the families at faces this machine actually has.
    ///
    /// An empty family means nothing was chosen, so the first installed
    /// fallback wins. A named one was picked in Settings and is kept — unless
    /// it has since been uninstalled, in which case it is treated as a choice
    /// that no longer exists rather than as a reason to render in tofu.
    pub fn resolve_families(&mut self, cx: &App) {
        let available = cx.text_system().all_font_names();
        let installed = |name: &str| available.iter().any(|have| have == name);
        if self.mono_family.is_empty() || !installed(&self.mono_family) {
            self.mono_family = MONO_FALLBACKS
                .iter()
                .find(|want| installed(want))
                .copied()
                .unwrap_or("monospace")
                .into();
        }
    }

    /// The code faces this machine has, in preference order. What the Settings
    /// window offers: a list of faces the app knows how to render code in,
    /// minus the ones that are not installed.
    pub fn installed_mono(cx: &App) -> Vec<SharedString> {
        let available = cx.text_system().all_font_names();
        MONO_FALLBACKS
            .iter()
            .filter(|want| available.iter().any(|have| have == *want))
            .map(|name| SharedString::from(*name))
            .collect()
    }
}

impl Default for Typography {
    fn default() -> Self {
        Self {
            ui_family: ".SystemUIFont".into(),
            // Empty means "whatever this machine has": the family is chosen by
            // `resolve_families` when the theme is installed. A name here would
            // be a claim about the font set of a machine we have not met.
            mono_family: "".into(),
            ui_size: px(13.),
            ui_size_sm: px(11.),
            ui_size_lg: px(15.),
            mono_size: px(12.5),
            ui_line_height: px(18.),
            ui_line_height_sm: px(16.),
            ui_line_height_lg: px(21.),
            // 1.6× — code needs more leading than chrome does. Grid rows are
            // 24px, so this still clears a cell without clipping.
            mono_line_height: px(20.),
        }
    }
}

/// Sugar for reading the theme off any gpui context.
pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
    fn colors(&self) -> &ThemeColors {
        &self.theme().colors
    }
    fn syntax(&self) -> &SyntaxTheme {
        &self.theme().syntax
    }
    fn metrics(&self) -> &Metrics {
        &self.theme().metrics
    }
    fn typography(&self) -> &Typography {
        &self.theme().typography
    }
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}

impl<T: 'static> ActiveTheme for gpui::Context<'_, T> {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}
