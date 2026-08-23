//! Settings (§5.9).
//!
//! A window of its own rather than a sheet, because it is not about the thing
//! the main window is showing: you can leave it open while you work, and
//! nothing in it needs the query you were writing.
//!
//! It owns no state. Every control reads [`crate::settings::Settings`] off the
//! workspace and writes back through `Workspace::update_settings`, which is
//! also the path that puts a change into effect — so a switch cannot end up
//! flicked but not applied, and the window cannot drift out of agreement with
//! the app behind it. The rule for what is in here: a control appears only if
//! it does something. A greyed-out row promising a feature is worse than no
//! row, so the panes are short.

use editor::Input;
use gpui::{
    div, point, prelude::*, px, size, App, Bounds, Context, Entity, FocusHandle, Focusable,
    IntoElement, KeyDownEvent, ParentElement, Render, SharedString, TitlebarOptions, WeakEntity,
    Window, WindowBounds, WindowHandle, WindowOptions,
};
use ui::{
    h_flex, v_flex, ActiveTheme, Button, ButtonSize, ButtonVariant, Divider, FormRow, Icon,
    IconColor, IconName, IconSize, Label, LabelSize, ListItem, SectionHeader, Segmented, Switch,
};

use crate::palette::Command;
use crate::settings::{Settings, DEFAULT_PAGE_SIZE, MONO_SIZES, PAGE_SIZES, TAB_SIZES};
use crate::workspace::Workspace;

/// Wide enough for the shortcut table's two columns without the keys crowding
/// the labels, and short enough to sit over a 900px-tall main window without
/// covering it.
const WINDOW_SIZE: (f32, f32) = (760., 560.);
/// The pane list. Fits the longest name, "Connections", at the UI size.
const SIDEBAR_WIDTH: gpui::Pixels = px(168.);

/// Open Settings in its own window.
pub fn open(
    workspace: WeakEntity<Workspace>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<SettingsWindow>> {
    let bounds = Bounds::centered(None, size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Settings".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.), px(9.))),
            }),
            ..Default::default()
        },
        |_window, cx| cx.new(|cx| SettingsWindow::new(workspace, cx)),
    )
}

/// Which pane is showing.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Pane {
    General,
    Appearance,
    Editor,
    Data,
    Connections,
    Shortcuts,
    Advanced,
}

impl Pane {
    /// In the order they are listed. General first because it is where someone
    /// looking for a setting they cannot name will start.
    pub const ALL: &'static [Pane] = &[
        Pane::General,
        Pane::Appearance,
        Pane::Editor,
        Pane::Data,
        Pane::Connections,
        Pane::Shortcuts,
        Pane::Advanced,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Editor => "Editor",
            Self::Data => "Data",
            Self::Connections => "Connections",
            Self::Shortcuts => "Shortcuts",
            Self::Advanced => "Advanced",
        }
    }

    /// By the name Settings shows, lower-cased. For the screenshot harness,
    /// which names a pane on the command line.
    pub fn named(name: &str) -> Option<Pane> {
        Pane::ALL
            .iter()
            .copied()
            .find(|pane| pane.label().eq_ignore_ascii_case(name))
    }

    fn icon(self) -> IconName {
        match self {
            Self::General => IconName::Gear,
            Self::Appearance => IconName::Sun,
            Self::Editor => IconName::Code,
            Self::Data => IconName::Table,
            Self::Connections => IconName::Plug,
            Self::Shortcuts => IconName::Command,
            Self::Advanced => IconName::CircleInfo,
        }
    }
}

pub struct SettingsWindow {
    focus: FocusHandle,
    pane: Pane,
    /// Weak: the main window owns the app's state, and Settings must not be
    /// the reason it stays alive. Every read goes through here, so a workspace
    /// that has gone away leaves an empty window rather than a stale one.
    workspace: WeakEntity<Workspace>,
    booted: bool,
    /// Narrows the theme list. Two built-ins and three families do not need
    /// one; a few hundred baked-in themes do, and the field costs nothing while
    /// the list is short — it is the difference between a list you scroll and
    /// one you can only scroll.
    theme_filter: Entity<Input>,
}

impl SettingsWindow {
    pub fn new(workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        let theme_filter = cx.new(|cx| {
            Input::new(cx)
                .icon(IconName::Magnifier)
                .placeholder("Filter themes…", cx)
        });
        // Read straight out of the field at render time, so a keystroke only
        // has to invalidate the frame.
        cx.subscribe(&theme_filter, |_, _, event: &editor::EditorEvent, cx| {
            if matches!(event, editor::EditorEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        Self {
            focus: cx.focus_handle(),
            pane: Pane::General,
            workspace,
            booted: false,
            theme_filter,
        }
    }

    fn settings(&self, cx: &App) -> Settings {
        self.workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).settings().clone())
            .unwrap_or_default()
    }

    pub fn show_pane(&mut self, pane: Pane, cx: &mut Context<Self>) {
        self.show(pane, cx);
    }

    fn show(&mut self, pane: Pane, cx: &mut Context<Self>) {
        self.pane = pane;
        cx.notify();
    }

    /// ⎋ and ⌘W both mean "I am done here". Nothing in this window is
    /// uncommitted, so there is nothing to ask about on the way out.
    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, _cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let closing = k.key == "escape" || (k.key == "w" && k.modifiers.platform);
        if closing {
            window.remove_window();
        }
    }
}

/// Change a setting from inside a control's callback, where there is no `self`.
/// A section header with air above it. The first header in a pane sits under
/// the pane's own top padding and wants none; every later one is a change of
/// subject, and reads as one only if the gap before it beats the gap between
/// the rows it heads.
fn section(title: &'static str) -> impl IntoElement {
    div().pt(px(10.)).child(SectionHeader::new(title).flush())
}

fn change(
    workspace: &WeakEntity<Workspace>,
    cx: &mut App,
    edit: impl FnOnce(&mut Settings) + 'static,
) {
    let _ = workspace.update(cx, |workspace, cx| workspace.update_settings(edit, cx));
}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.booted {
            self.booted = true;
            window.focus(&self.focus, cx);
        }

        let c = cx.colors().clone();
        let m = cx.metrics().clone();
        let ty = cx.typography().clone();
        let pane = self.pane;

        v_flex()
            .id("settings")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .bg(c.panel)
            .text_color(c.text)
            .font_family(ty.ui_family.clone())
            .text_size(ty.ui_size)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.sidebar(cx))
                    .child(Divider::vertical())
                    .child(
                        v_flex()
                            .id("settings-pane")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            // The traffic lights sit over the top-left of the
                            // window; the pane starts below them so its first
                            // heading is never underneath one.
                            .pt(m.titlebar_height)
                            .px(px(24.))
                            .pb(px(20.))
                            .gap(px(18.))
                            .child(self.body(pane, cx)),
                    ),
            )
    }
}

impl SettingsWindow {
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors().clone();
        let m = cx.metrics().clone();
        v_flex()
            .w(SIDEBAR_WIDTH)
            .flex_none()
            .h_full()
            .bg(c.chrome)
            // Clear of the traffic lights, like the main window's sidebar.
            .pt(m.titlebar_height)
            .px(px(6.))
            .gap(px(1.))
            .children(Pane::ALL.iter().copied().map(|pane| {
                ListItem::new(("pane", pane as usize), pane.label())
                    .icon(pane.icon())
                    .selected(pane == self.pane)
                    .on_click(cx.listener(move |this, _, _, cx| this.show(pane, cx)))
            }))
    }

    fn body(&self, pane: Pane, cx: &mut Context<Self>) -> gpui::AnyElement {
        match pane {
            Pane::General => self.general(cx).into_any_element(),
            Pane::Appearance => self.appearance(cx).into_any_element(),
            Pane::Editor => self.editor(cx).into_any_element(),
            Pane::Data => self.data(cx).into_any_element(),
            Pane::Connections => self.connections(cx).into_any_element(),
            Pane::Shortcuts => shortcuts(cx).into_any_element(),
            Pane::Advanced => advanced(cx).into_any_element(),
        }
    }

    fn general(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.settings(cx);
        let workspace = self.workspace.clone();

        v_flex()
            .gap(px(14.))
            .child(SectionHeader::new("At launch").flush())
            .child(
                FormRow::new("Restore")
                    .hint("Bring back the tabs and the half-written statement from last time.")
                    .child(
                        Switch::new("restore-session", settings.restore_session()).on_toggle({
                            let workspace = workspace.clone();
                            move |on, _, cx| {
                                change(&workspace, cx, move |s| s.set_restore_session(on))
                            }
                        }),
                    ),
            )
            .child(
                FormRow::new("Reconnect")
                    .hint("Open the connection the last session was on.")
                    .child(
                        Switch::new("reopen-connection", settings.reopen_connection()).on_toggle({
                            let workspace = workspace.clone();
                            move |on, _, cx| {
                                change(&workspace, cx, move |s| s.set_reopen_connection(on))
                            }
                        }),
                    ),
            )
    }

    fn appearance(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.settings(cx);
        let workspace = self.workspace.clone();
        let c = cx.colors().clone();
        let appearance = cx.theme().appearance;
        let system = settings.appearance().is_none();
        // The theme actually on screen, which is the one the tick belongs
        // against — not the one saved in the slot for the other appearance.
        let theme_name =
            ui::ThemeRegistry::resolve(settings.theme_name(appearance), appearance, cx)
                .name
                .to_string();
        let query = self.theme_filter.read(cx).text(cx).to_lowercase();
        let themes: Vec<(SharedString, ui::Appearance, [gpui::Hsla; 4])> =
            ui::ThemeRegistry::global(cx)
                .map(|registry| {
                    registry
                        .all()
                        .into_iter()
                        .filter(|t| {
                            query.is_empty() || t.name.to_lowercase().contains(query.as_str())
                        })
                        .map(|t| {
                            (
                                t.name.clone(),
                                t.appearance,
                                [
                                    t.colors.background,
                                    t.colors.panel,
                                    t.colors.surface,
                                    t.colors.accent,
                                ],
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();

        v_flex()
            .gap(px(14.))
            .child(SectionHeader::new("Theme").flush())
            .child(
                FormRow::new("Match System")
                    .hint("Follow macOS between light and dark, using the last theme you picked for each.")
                    .child(
                        Switch::new("match-system", system).on_toggle({
                            let workspace = workspace.clone();
                            move |on, _, cx| {
                                change(&workspace, cx, move |s| match on {
                                    true => s.follow_system(),
                                    false => s.set_appearance(appearance),
                                });
                            }
                        }),
                    ),
            )
            .child(
                FormRow::new("Theme")
                    .hint("Light or dark is whichever one you pick. Zed's format — drop more into ~/Library/Application Support/tupli/themes.")
                    .child(
                        v_flex()
                            .gap(px(6.))
                            .child(self.theme_filter.clone())
                            // Bounded and scrolling, so that however many
                            // themes are installed the rows *below* this one
                            // stay reachable. A list that grows without limit
                            // turns every other setting on the page into
                            // something you have to scroll past a theme
                            // catalogue to find.
                            .child(v_flex()
                            .id("themes")
                            .max_h(px(260.))
                            .overflow_y_scroll()
                            .gap(px(1.))
                            .children(themes.into_iter().enumerate().map(
                                |(index, (name, of, swatches))| {
                                    let selected = name == theme_name;
                                    let workspace = workspace.clone();
                                    let key = name.clone();
                                    ListItem::new(("theme", index), name)
                                        .selected(selected)
                                        // The row is the only preview there is
                                        // room for, so it shows the three
                                        // planes and the accent in the order
                                        // the window stacks them — enough to
                                        // tell One Dark from Gruvbox without
                                        // applying either.
                                        .end_child(
                                            h_flex()
                                                .gap(px(6.))
                                                .child(h_flex().children(swatches.into_iter().map(
                                                    |color| {
                                                        div()
                                                            .size(px(12.))
                                                            .flex_none()
                                                            .bg(color)
                                                    },
                                                )).rounded(px(3.)).overflow_hidden().border_1().border_color(c.border))
                                                .child(match selected {
                                                    true => Icon::new(IconName::Check)
                                                        .size(IconSize::Small)
                                                        .color(IconColor::Accent)
                                                        .into_any_element(),
                                                    false => div().size(px(14.)).into_any_element(),
                                                })
                                                .into_any_element(),
                                        )
                                        .on_click(move |_, _, cx| {
                                            let name = key.to_string();
                                            change(&workspace, cx, move |s| {
                                                s.choose_theme(&name, of)
                                            })
                                        })
                                },
                            ))),
                    ),
            )
            .child(section("Rows"))
            .child(
                FormRow::new("Row height")
                    .hint("How tall a row in the results grid is.")
                    .child(
                        Segmented::new("row-density", ["Compact", "Default", "Comfortable"])
                            .hug()
                            .selected(match settings.row_density() {
                                grid::Density::Compact => 0,
                                grid::Density::Default => 1,
                                grid::Density::Comfortable => 2,
                            })
                            .on_select({
                                let workspace = workspace.clone();
                                move |index, _, cx| {
                                    let density = match index {
                                        0 => grid::Density::Compact,
                                        2 => grid::Density::Comfortable,
                                        _ => grid::Density::Default,
                                    };
                                    change(&workspace, cx, move |s| s.set_row_density(density));
                                }
                            }),
                    ),
            )
    }

    fn editor(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.settings(cx);
        let workspace = self.workspace.clone();
        let ty = cx.typography().clone();
        let installed = ui::Typography::installed_mono(cx);
        // An empty choice means "whatever this machine has", which is the face
        // the theme resolved to — so the list shows that one as chosen rather
        // than showing nothing chosen at all.
        let chosen = match settings.mono_family() {
            "" => ty.mono_family.to_string(),
            family => family.to_string(),
        };
        let size = settings.mono_size();
        let tab_size = settings.tab_size();

        v_flex()
            .gap(px(14.))
            .child(SectionHeader::new("Code").flush())
            .child(
                FormRow::new("Font")
                    .hint("The face used by the console, the grid and every value.")
                    .child(
                        v_flex()
                            .gap(px(1.))
                            .children(installed.into_iter().enumerate().map(|(index, family)| {
                                let selected = family == chosen;
                                let workspace = workspace.clone();
                                let key: SharedString = family.clone();
                                ListItem::new(("mono", index), family.clone())
                                    .mono()
                                    .selected(selected)
                                    .end_child(match selected {
                                        true => Icon::new(IconName::Check)
                                            .size(IconSize::Small)
                                            .color(IconColor::Accent)
                                            .into_any_element(),
                                        false => div().into_any_element(),
                                    })
                                    .on_click(move |_, _, cx| {
                                        let family = key.to_string();
                                        change(&workspace, cx, move |s| s.set_mono_family(&family))
                                    })
                            })),
                    ),
            )
            .child(
                FormRow::new("Size").child(
                    Segmented::new("mono-size", MONO_SIZES.iter().map(|s| trim_zero(*s)))
                        .hug()
                        .selected(MONO_SIZES.iter().position(|s| *s == size).unwrap_or(2))
                        .on_select({
                            let workspace = workspace.clone();
                            move |index, _, cx| {
                                let size = MONO_SIZES[index];
                                change(&workspace, cx, move |s| s.set_mono_size(size));
                            }
                        }),
                ),
            )
            .child(section("Typing"))
            .child(
                FormRow::new("Indent")
                    .hint("Spaces per indent step. Tab never inserts a tab character.")
                    .child(
                        Segmented::new("tab-size", TAB_SIZES.iter().map(|n| n.to_string()))
                            .hug()
                            .selected(TAB_SIZES.iter().position(|n| *n == tab_size).unwrap_or(1))
                            .on_select({
                                let workspace = workspace.clone();
                                move |index, _, cx| {
                                    let size = TAB_SIZES[index];
                                    change(&workspace, cx, move |s| s.set_tab_size(size));
                                }
                            }),
                    ),
            )
            .child(
                FormRow::new("Gutter")
                    .hint("Line numbers down the left of the console.")
                    .child(
                        Switch::new("line-numbers", settings.line_numbers()).on_toggle({
                            let workspace = workspace.clone();
                            move |on, _, cx| change(&workspace, cx, move |s| s.set_line_numbers(on))
                        }),
                    ),
            )
    }

    fn data(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.settings(cx);
        let workspace = self.workspace.clone();
        let page_size = settings.page_size();

        v_flex()
            .gap(px(14.))
            .child(SectionHeader::new("Browsing a table").flush())
            .child(
                FormRow::new("Row limit")
                    .hint("The limit applied when a table is opened. Every row is fetched before anything is drawn, so a bigger number costs time on a slow link as well as memory \u{2014} the pager reaches the rest either way.")
                    .child(
                        Segmented::new("page-size", PAGE_SIZES.iter().map(|n| thousands(*n)))
                            .hug()
                            .selected(
                                PAGE_SIZES
                                    .iter()
                                    .position(|n| *n == page_size)
                                    .or_else(|| PAGE_SIZES.iter().position(|n| *n == DEFAULT_PAGE_SIZE))
                                    .unwrap_or(0),
                            )
                            .on_select({
                                let workspace = workspace.clone();
                                move |index, _, cx| {
                                    let size = PAGE_SIZES[index];
                                    change(&workspace, cx, move |s| s.set_page_size(size));
                                }
                            }),
                    ),
            )
            .child(section("The grid"))
            .child(
                FormRow::new("Shading").hint("Shade every other row.").child(
                    Switch::new("zebra", settings.zebra()).on_toggle({
                        let workspace = workspace.clone();
                        move |on, _, cx| change(&workspace, cx, move |s| s.set_zebra(on))
                    }),
                ),
            )
    }

    fn connections(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let connections = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).connections.clone())
            .unwrap_or_default();
        let workspace = self.workspace.clone();
        let count = connections.len();

        v_flex()
            .gap(px(14.))
            .child(
                SectionHeader::new("Saved connections").flush().end_child(
                    Label::new(count.to_string())
                        .size(LabelSize::Small)
                        .color(IconColor::Subtle),
                ),
            )
            .when(count == 0, |el| {
                el.child(
                    Label::new("Nothing saved yet — the first connection you keep shows up here.")
                        .size(LabelSize::Small)
                        .color(IconColor::Subtle),
                )
            })
            .child(
                v_flex()
                    .gap(px(1.))
                    .children(connections.into_iter().enumerate().map(|(index, config)| {
                        let id = config.id;
                        let meta = format!(
                            "{}@{}:{}/{}",
                            config.user, config.host, config.port, config.database
                        );
                        let workspace = workspace.clone();
                        ListItem::new(("connection", index), config.name.clone())
                            .icon(IconName::Plug)
                            .meta(meta)
                            .on_click(move |_, _, cx| {
                                // The form is its own window, and there is only ever one
                                // of it, so editing from here hands the existing window
                                // another connection rather than putting a second copy
                                // of the form in this one. It raises itself; raising the
                                // main window here would land on top of it.
                                let _ = workspace.update(cx, |workspace, cx| {
                                    if let Some(config) =
                                        workspace.connections.iter().find(|c| c.id == id).cloned()
                                    {
                                        workspace.edit_connection(config, cx);
                                    }
                                });
                            })
                    })),
            )
            .child(
                h_flex().child(
                    Button::new("new-connection", "New Connection…")
                        .start_icon(IconName::Plus)
                        .size(ButtonSize::Small)
                        .variant(ButtonVariant::Filled)
                        .on_click({
                            let workspace = workspace.clone();
                            move |_, _, cx| {
                                let _ = workspace.update(cx, |workspace, cx| {
                                    workspace.new_connection(cx);
                                });
                            }
                        }),
                ),
            )
    }
}

/// A shortcut table, grouped. Only gestures that actually work: this window is
/// where someone comes to find out what the app can do, and a keystroke listed
/// here that does nothing is a lie in the one place it is least forgivable.
fn shortcuts(cx: &mut Context<SettingsWindow>) -> impl IntoElement {
    let palette: Vec<(&str, &str)> = vec![
        ("Command Palette", "⌘K"),
        ("Go to Object", "⌘P"),
        ("Run a Command", "⇧⌘P"),
        ("Go to Line", "⌘K then :"),
    ];
    let commands: Vec<(&str, &str)> = Command::ALL
        .iter()
        .filter_map(|command| command.shortcut().map(|keys| (command.label(), keys)))
        .collect();
    let editing: Vec<(&str, &str)> = vec![
        ("Undo", "⌘Z"),
        ("Redo", "⇧⌘Z"),
        ("Select All", "⌘A"),
        ("Copy", "⌘C"),
        ("Cut", "⌘X"),
        ("Paste", "⌘V"),
        ("Indent", "⇥"),
        ("Outdent", "⇧⇥"),
    ];

    v_flex()
        .gap(px(14.))
        .child(SectionHeader::new("Finding things").flush())
        .child(shortcut_table("palette", palette, cx))
        .child(section("Commands"))
        .child(shortcut_table("commands", commands, cx))
        .child(section("Editing"))
        .child(shortcut_table("editing", editing, cx))
}

fn shortcut_table(
    id: &'static str,
    rows: Vec<(&str, &str)>,
    cx: &mut Context<SettingsWindow>,
) -> impl IntoElement {
    let c = cx.colors().clone();
    let m = cx.metrics().clone();
    v_flex()
        .gap(px(1.))
        .children(rows.into_iter().enumerate().map(|(index, (label, keys))| {
            h_flex()
                .id((id, index))
                .h(m.row_height)
                .px(px(8.))
                .rounded(m.radius_sm)
                .justify_between()
                .when(index % 2 == 1, |el| el.bg(c.grid_stripe))
                .child(Label::new(label.to_string()))
                .child(
                    div()
                        .px(px(6.))
                        .py(px(1.))
                        .rounded(m.radius_sm)
                        .bg(c.field)
                        .border_1()
                        .border_color(c.border)
                        .child(
                            Label::new(keys.to_string())
                                .size(LabelSize::Small)
                                .color(IconColor::Muted),
                        ),
                )
        }))
}

/// Facts, not knobs. Where the app keeps its things and what it is — the two
/// questions that otherwise need a support thread to answer.
fn advanced(_cx: &mut Context<SettingsWindow>) -> impl IntoElement {
    let rows = [
        ("Version", env!("CARGO_PKG_VERSION").to_string()),
        ("Data folder", display_path(store::paths::data_dir())),
        ("Database", display_path(store::paths::database_file())),
        ("Logs", display_path(store::paths::log_dir())),
    ];

    v_flex()
        .gap(px(14.))
        .child(SectionHeader::new("About").flush())
        .children(rows.into_iter().map(|(label, value)| {
            FormRow::new(label).plain().child(
                Label::new(value)
                    .size(LabelSize::Code)
                    .mono()
                    .color(IconColor::Muted),
            )
        }))
        .child(
            Label::new("Connections, history and saved queries all live in that one file.")
                .size(LabelSize::Small)
                .color(IconColor::Subtle),
        )
}

/// `~` for the home directory, the way every other app writes it.
fn display_path(path: std::path::PathBuf) -> String {
    let text = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && text.starts_with(&home) => {
            format!("~{}", &text[home.len()..])
        }
        _ => text,
    }
}

/// `12.5` stays, `14.0` becomes `14`.
fn trim_zero(size: f32) -> String {
    match size.fract() == 0. {
        true => format!("{size:.0}"),
        false => format!("{size}"),
    }
}

/// `50 000`, with a thin space: a five-digit row count is unreadable run
/// together, and a comma would be wrong in half the world.
fn thousands(n: usize) -> String {
    let digits: Vec<char> = n.to_string().chars().collect();
    let mut out = String::new();
    for (index, digit) in digits.iter().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push('\u{2009}');
        }
        out.push(*digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_prints_without_a_trailing_zero() {
        assert_eq!(trim_zero(14.), "14");
        assert_eq!(trim_zero(12.5), "12.5");
    }

    #[test]
    fn a_row_count_is_grouped_in_threes() {
        assert_eq!(thousands(1_000), "1\u{2009}000");
        assert_eq!(thousands(50_000), "50\u{2009}000");
        assert_eq!(thousands(200_000), "200\u{2009}000");
        assert_eq!(thousands(7), "7");
    }

    #[test]
    fn a_pane_can_be_found_by_name() {
        assert_eq!(Pane::named("shortcuts"), Some(Pane::Shortcuts));
        assert_eq!(Pane::named("Appearance"), Some(Pane::Appearance));
        assert_eq!(Pane::named("nowhere"), None);
    }

    #[test]
    fn every_pane_has_a_name_and_an_icon() {
        // The sidebar is built from `ALL`, so a pane missing from it is a pane
        // that cannot be reached.
        assert_eq!(Pane::ALL.len(), 7);
        for pane in Pane::ALL {
            assert!(!pane.label().is_empty());
        }
    }

    #[test]
    fn a_path_under_home_is_written_with_a_tilde() {
        let home = std::env::var("HOME").unwrap_or_default();
        if home.is_empty() {
            return;
        }
        let path = std::path::PathBuf::from(&home).join("Library/x");
        assert_eq!(display_path(path), "~/Library/x");
    }
}
