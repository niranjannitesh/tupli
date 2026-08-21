//! The command palette.
//!
//! One floating card that answers "what can I do" and "where is that table"
//! with the same keystroke. It is the only place in the app where a feature is
//! allowed to be discoverable without also being visible: everything reachable
//! from a menu, a toolbar or a shortcut is listed here, spelled the way it is
//! spelled everywhere else, with the shortcut printed beside it so that using
//! the palette teaches you not to need it.
//!
//! The prefix chooses what is being searched, exactly as in §5.8 of the plan:
//! nothing for commands and objects together, `>` for commands, `@` for schema
//! objects, `#` for themes, `:` for a line number, `?` for the list of prefixes.
//! The palette owns the last three itself — a theme and a line number are facts
//! about the window, not about the database — and is handed the first three by
//! the workspace, which is the only thing that knows what is connected.

use gpui::{
    div, prelude::*, px, Context, Entity, EventEmitter, FocusHandle, Focusable, HighlightStyle,
    IntoElement, MouseButton, ParentElement, Render, ScrollHandle, SharedString, StyledText,
    Subscription, Window,
};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ui::{
    h_flex, v_flex, ActiveTheme, Appearance, Icon, IconColor, IconName, IconSize, Label, LabelSize,
};

use editor::{Direction, Editor, EditorEvent, EditorMode, EditorStyle};

/// What the palette is searching.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PaletteMode {
    /// No prefix: everything that can be opened or done.
    Mixed,
    Commands,
    Objects,
    Themes,
    Line,
    Help,
}

impl PaletteMode {
    fn from_query(query: &str) -> Self {
        match query.chars().next() {
            Some('>') => Self::Commands,
            Some('@') => Self::Objects,
            Some('#') => Self::Themes,
            Some(':') => Self::Line,
            Some('?') => Self::Help,
            _ => Self::Mixed,
        }
    }

    pub fn prefix(self) -> &'static str {
        match self {
            Self::Mixed => "",
            Self::Commands => ">",
            Self::Objects => "@",
            Self::Themes => "#",
            Self::Line => ":",
            Self::Help => "?",
        }
    }

    /// The chip to the left of the field. `None` in the mixed mode, where there
    /// is nothing to say that the placeholder does not already say.
    fn chip(self) -> Option<&'static str> {
        match self {
            Self::Mixed => None,
            Self::Commands => Some("Command"),
            Self::Objects => Some("Object"),
            Self::Themes => Some("Theme"),
            Self::Line => Some("Line"),
            Self::Help => Some("Help"),
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::Mixed => "Search commands and objects…",
            Self::Commands => "Run a command…",
            Self::Objects => "Open a table, view or function…",
            Self::Themes => "Select a theme…",
            Self::Line => "Go to line…",
            Self::Help => "Prefixes",
        }
    }
}

/// What choosing a row does.
///
/// The palette carries these to the workspace rather than acting on them,
/// because every one of them has a non-palette way in — a button, a shortcut, a
/// click in the tree — and having two implementations of "open this table" is
/// how the two of them start to disagree.
#[derive(Clone, Debug, PartialEq)]
pub enum PaletteAction {
    Command(Command),
    /// Browse a table, view or materialized view.
    Open(db::RelationRef),
    /// Load a saved query into the editor.
    LoadQuery(uuid::Uuid),
    Theme(Appearance),
    /// One-based, as it is typed and as the status bar reports it.
    GoToLine(usize),
    /// Re-enter the palette in another mode. Never leaves the palette.
    EnterMode(PaletteMode),
}

/// Everything the workspace can be asked to do by name.
///
/// The list is the app's menu bar in every sense but the platform one: a
/// command that is not here is a command nobody can find.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Run,
    RunAll,
    Cancel,
    Save,
    SaveAs,
    ExportRows,
    ImportRows,
    CommitChanges,
    DiscardChanges,
    AddRow,
    DeleteRows,
    RevertRows,
    NewTab,
    NewTable,
    CloseTab,
    SplitRight,
    SplitDown,
    ClosePane,
    NewConnection,
    RefreshResults,
    RefreshSchema,
    FormatQuery,
    FollowReference,
    ToggleSidebar,
    ToggleResults,
    ToggleInspector,
    ShowDatabaseTree,
    ShowSavedQueries,
    ShowHistory,
    ShowData,
    ShowStructure,
    ShowDdl,
    OpenSettings,
}

impl Command {
    /// In the order they are offered. Roughly by how often they are wanted,
    /// because a fuzzy match on two characters often leaves several standing
    /// and the tie is broken by this order.
    pub const ALL: &'static [Command] = &[
        Command::Run,
        Command::RunAll,
        Command::Cancel,
        Command::Save,
        Command::SaveAs,
        Command::ExportRows,
        Command::ImportRows,
        Command::CommitChanges,
        Command::DiscardChanges,
        Command::AddRow,
        Command::DeleteRows,
        Command::RevertRows,
        Command::NewTab,
        Command::NewTable,
        Command::CloseTab,
        Command::SplitRight,
        Command::SplitDown,
        Command::ClosePane,
        Command::RefreshResults,
        Command::RefreshSchema,
        Command::FormatQuery,
        Command::FollowReference,
        Command::NewConnection,
        Command::ShowData,
        Command::ShowStructure,
        Command::ShowDdl,
        Command::ShowDatabaseTree,
        Command::ShowSavedQueries,
        Command::ShowHistory,
        Command::ToggleSidebar,
        Command::ToggleResults,
        Command::ToggleInspector,
        Command::OpenSettings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Run => "Run Statement",
            Self::RunAll => "Run All Statements",
            Self::Cancel => "Cancel Running Query",
            Self::Save => "Save Query",
            Self::SaveAs => "Save Query As…",
            Self::ExportRows => "Export Rows…",
            Self::ImportRows => "Import Rows…",
            Self::CommitChanges => "Save Row Changes…",
            Self::DiscardChanges => "Discard Row Changes",
            Self::AddRow => "Add Row",
            Self::DeleteRows => "Delete Selected Rows",
            Self::RevertRows => "Revert Selected Rows",
            Self::NewTab => "New Query Tab",
            Self::NewTable => "New Table…",
            Self::CloseTab => "Close Tab",
            Self::SplitRight => "Split Editor Right",
            Self::SplitDown => "Split Editor Down",
            Self::ClosePane => "Close Split",
            Self::NewConnection => "New Connection…",
            Self::RefreshResults => "Refresh Results",
            Self::RefreshSchema => "Refresh Schema",
            Self::FormatQuery => "Format SQL",
            Self::FollowReference => "Follow Reference",
            Self::ToggleSidebar => "Toggle Sidebar",
            Self::ToggleResults => "Toggle Results",
            Self::ToggleInspector => "Toggle Inspector",
            Self::ShowDatabaseTree => "Show Database Tree",
            Self::ShowSavedQueries => "Show Saved Queries",
            Self::ShowHistory => "Show Query History",
            Self::ShowData => "Show Data",
            Self::ShowStructure => "Show Structure",
            Self::ShowDdl => "Show DDL",
            Self::OpenSettings => "Open Settings…",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Run | Self::RunAll => IconName::Run,
            Self::Cancel => IconName::Ban,
            Self::Save | Self::SaveAs | Self::CommitChanges => IconName::Save,
            Self::ExportRows => IconName::DatabaseExport,
            Self::ImportRows => IconName::Import,
            Self::DiscardChanges => IconName::Undo,
            Self::AddRow => IconName::Plus,
            Self::DeleteRows => IconName::Minus,
            Self::RevertRows => IconName::Undo,
            Self::NewTab => IconName::Plus,
            Self::NewTable => IconName::Columns,
            Self::CloseTab => IconName::Xmark,
            Self::SplitRight => IconName::SplitX,
            Self::SplitDown => IconName::SplitY,
            Self::ClosePane => IconName::Xmark,
            Self::NewConnection => IconName::Plug,
            Self::RefreshResults | Self::RefreshSchema => IconName::Refresh,
            Self::FormatQuery => IconName::TextAlignLeft,
            Self::FollowReference => IconName::ArrowUpRight,
            Self::ToggleSidebar => IconName::SidebarLeft,
            Self::ToggleResults => IconName::SplitY,
            Self::ToggleInspector => IconName::SidebarRight,
            Self::ShowDatabaseTree => IconName::Database,
            Self::ShowSavedQueries => IconName::Code,
            Self::ShowHistory => IconName::History,
            Self::ShowData => IconName::Table,
            Self::ShowStructure => IconName::Columns,
            Self::ShowDdl => IconName::Code,
            Self::OpenSettings => IconName::Gear,
        }
    }

    /// Printed on the right of the row. Only for gestures that actually work:
    /// a palette that advertises a shortcut the app does not have is worse than
    /// one that advertises none.
    pub fn shortcut(self) -> Option<&'static str> {
        match self {
            Self::Run => Some("⌘⏎"),
            Self::RunAll => Some("⇧⌘⏎"),
            Self::Cancel => Some("⌘."),
            Self::Save => Some("⌘S"),
            Self::ExportRows => Some("⇧⌘E"),
            Self::ImportRows => Some("⇧⌘I"),
            Self::NewTab => Some("⌘T"),
            Self::CloseTab => Some("⌘W"),
            Self::SplitRight => Some("⌘D"),
            Self::SplitDown => Some("⇧⌘D"),
            Self::NewConnection => Some("⌘N"),
            Self::RefreshResults => Some("⌘R"),
            Self::FollowReference => Some("F6"),
            Self::RefreshSchema => Some("⇧⌘R"),
            Self::FormatQuery => Some("⌥⇧F"),
            Self::ToggleSidebar => Some("⌘1"),
            Self::ToggleResults => Some("⌘2"),
            Self::ToggleInspector => Some("⌘3"),
            Self::OpenSettings => Some("⌘,"),
            _ => None,
        }
    }
}

/// Which list a row belongs to, and therefore which prefixes show it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Command,
    Object,
    Query,
    Theme,
    /// A row that switches the palette into another mode.
    Mode,
}

#[derive(Clone, Debug)]
pub struct PaletteItem {
    pub kind: ItemKind,
    pub action: PaletteAction,
    pub label: SharedString,
    /// Dim text on the right: the schema a table is in, the connection a saved
    /// query belongs to.
    pub detail: Option<SharedString>,
    pub shortcut: Option<SharedString>,
    pub icon: IconName,
    /// Draws a checkmark: this is the value already in effect.
    pub current: bool,
}

impl PaletteItem {
    pub fn new(kind: ItemKind, action: PaletteAction, label: impl Into<SharedString>) -> Self {
        Self {
            kind,
            action,
            label: label.into(),
            detail: None,
            shortcut: None,
            icon: IconName::CircleDashed,
            current: false,
        }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = icon;
        self
    }

    pub fn current(mut self, current: bool) -> Self {
        self.current = current;
        self
    }

    pub fn command(command: Command) -> Self {
        let mut item = Self::new(
            ItemKind::Command,
            PaletteAction::Command(command),
            command.label(),
        )
        .icon(command.icon());
        if let Some(keys) = command.shortcut() {
            item = item.shortcut(keys);
        }
        item
    }
}

pub enum PaletteEvent {
    Dismissed,
    /// The arrow keys landed on a theme. Applied at once and taken back if the
    /// palette is dismissed, which is the only way to choose a theme by looking
    /// at it rather than by reading its name.
    Preview(Appearance),
    Chose(PaletteAction),
}

/// A row that survived the filter.
struct Match {
    /// Index into `candidates`.
    index: usize,
    /// Byte ranges of the characters the query matched, for highlighting.
    ranges: Vec<std::ops::Range<usize>>,
}

pub struct Palette {
    query: Entity<Editor>,
    /// Commands, schema objects and saved queries, handed over at open time.
    items: Vec<PaletteItem>,
    /// The rows the current mode is searching. Rebuilt when the mode changes,
    /// not on every keystroke.
    candidates: Vec<PaletteItem>,
    matches: Vec<Match>,
    mode: PaletteMode,
    selected: usize,
    scroll: ScrollHandle,
    matcher: Matcher,
    /// The appearance the window had when the palette opened, restored if the
    /// palette is dismissed after previewing another one.
    original: Appearance,
    previewing: bool,
    _subscription: Subscription,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Focusable for Palette {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.query.read(cx).focus().clone()
    }
}

/// How many rows fit before the list scrolls. Ten is the point at which a list
/// stops being something you can take in and starts being something you have to
/// read, which is when typing another character is the better move anyway.
const ROWS_SHOWN: usize = 10;
/// The same height as a tab, and four points more than a sidebar row. A modal
/// list carries an icon, a label and a shortcut on one line, and the tab strip
/// is the app's existing answer to how tall that has to be.
const ROW_HEIGHT: gpui::Pixels = px(28.);

impl Palette {
    pub fn new(items: Vec<PaletteItem>, prefix: &str, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            let mut editor = Editor::new(EditorMode::SingleLine, cx);
            editor.set_style(EditorStyle::ui(cx));
            editor.set_text(prefix, cx);
            editor
        });
        let _subscription = cx.subscribe(&query, |this, _, event: &EditorEvent, cx| match event {
            EditorEvent::Changed => this.on_query_changed(cx),
            EditorEvent::Submit => this.confirm(cx),
            EditorEvent::Cancel => this.dismiss(cx),
            EditorEvent::Navigate(direction) => this.navigate(*direction, cx),
            _ => {}
        });

        let mut palette = Self {
            query,
            items,
            candidates: Vec::new(),
            matches: Vec::new(),
            mode: PaletteMode::Mixed,
            selected: 0,
            scroll: ScrollHandle::new(),
            matcher: Matcher::new(Config::DEFAULT),
            original: cx.theme().appearance,
            previewing: false,
            _subscription,
        };
        palette.rebuild(cx);
        palette
    }

    /// The text after the prefix.
    fn needle(&self, cx: &gpui::App) -> String {
        let text = self.query.read(cx).text();
        match self.mode.prefix().is_empty() {
            true => text,
            false => text.chars().skip(1).collect(),
        }
    }

    fn on_query_changed(&mut self, cx: &mut Context<Self>) {
        let mode = PaletteMode::from_query(&self.query.read(cx).text());
        if mode != self.mode {
            self.mode = mode;
            self.candidates = self.build_candidates(cx);
            self.set_placeholder(cx);
        }
        self.filter(cx);
    }

    /// Rebuild both the candidate list and the matches. For the constructor and
    /// for a mode entered by choosing a row rather than by typing a prefix.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.mode = PaletteMode::from_query(&self.query.read(cx).text());
        self.candidates = self.build_candidates(cx);
        self.set_placeholder(cx);
        self.filter(cx);
    }

    fn set_placeholder(&self, cx: &mut Context<Self>) {
        let text = self.mode.placeholder();
        self.query
            .update(cx, |editor, _| editor.set_placeholder(text));
    }

    fn build_candidates(&self, cx: &gpui::App) -> Vec<PaletteItem> {
        let appearance = cx.theme().appearance;
        match self.mode {
            // Commands first: someone who typed nothing is far more likely to
            // be looking for a verb than for a particular table, and the tables
            // are one `@` away.
            PaletteMode::Mixed => self
                .items
                .iter()
                .filter(|item| item.kind != ItemKind::Theme)
                .cloned()
                .collect(),
            PaletteMode::Commands => self
                .items
                .iter()
                .filter(|item| matches!(item.kind, ItemKind::Command | ItemKind::Mode))
                .cloned()
                .collect(),
            PaletteMode::Objects => self
                .items
                .iter()
                .filter(|item| item.kind == ItemKind::Object)
                .cloned()
                .collect(),
            PaletteMode::Themes => [Appearance::Dark, Appearance::Light]
                .into_iter()
                .map(|which| {
                    PaletteItem::new(
                        ItemKind::Theme,
                        PaletteAction::Theme(which),
                        ui::Theme::of(which).name,
                    )
                    .icon(if which.is_dark() {
                        IconName::Moon
                    } else {
                        IconName::Sun
                    })
                    .current(which == appearance)
                })
                .collect(),
            // The line number is typed, not chosen, so the list is however many
            // rows the number is valid for: one, or none.
            PaletteMode::Line => Vec::new(),
            PaletteMode::Help => [
                (PaletteMode::Commands, "Commands", IconName::Command),
                (
                    PaletteMode::Objects,
                    "Tables, views and functions",
                    IconName::Table,
                ),
                (PaletteMode::Themes, "Themes", IconName::Sun),
                (PaletteMode::Line, "Go to line", IconName::Hashtag),
            ]
            .into_iter()
            .map(|(mode, what, icon)| {
                PaletteItem::new(ItemKind::Mode, PaletteAction::EnterMode(mode), what)
                    .icon(icon)
                    .shortcut(mode.prefix())
            })
            .collect(),
        }
    }

    fn filter(&mut self, cx: &mut Context<Self>) {
        let needle = self.needle(cx);

        if self.mode == PaletteMode::Line {
            self.matches.clear();
            self.candidates = match parse_line(&needle) {
                Some(line) => vec![PaletteItem::new(
                    ItemKind::Mode,
                    PaletteAction::GoToLine(line),
                    format!("Go to line {line}"),
                )
                .icon(IconName::Hashtag)
                .shortcut("⏎")],
                None => Vec::new(),
            };
            self.matches = (0..self.candidates.len())
                .map(|index| Match {
                    index,
                    ranges: Vec::new(),
                })
                .collect();
            self.selected = 0;
            cx.notify();
            return;
        }

        if needle.trim().is_empty() {
            self.matches = (0..self.candidates.len())
                .map(|index| Match {
                    index,
                    ranges: Vec::new(),
                })
                .collect();
            self.selected = 0;
            self.scroll.scroll_to_item(0);
            cx.notify();
            return;
        }

        let pattern = Pattern::parse(needle.trim(), CaseMatching::Ignore, Normalization::Smart);
        let mut haystack = Vec::new();
        let mut positions = Vec::new();
        let mut scored: Vec<(u32, Match)> = Vec::new();
        for (index, item) in self.candidates.iter().enumerate() {
            positions.clear();
            haystack.clear();
            let text = Utf32Str::new(item.label.as_ref(), &mut haystack);
            let Some(score) = pattern.indices(text, &mut self.matcher, &mut positions) else {
                continue;
            };
            positions.sort_unstable();
            positions.dedup();
            scored.push((
                score,
                Match {
                    index,
                    ranges: char_ranges(&item.label, &positions),
                },
            ));
        }
        // Best first, and ties in the order the candidates were built: the
        // command list is already in the order we would want to break them.
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.index.cmp(&b.1.index)));
        self.matches = scored.into_iter().map(|(_, m)| m).collect();
        self.selected = 0;
        self.scroll.scroll_to_item(0);
        cx.notify();
    }

    fn navigate(&mut self, direction: Direction, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() - 1;
        let step = match direction {
            Direction::Up | Direction::Down => 1,
            Direction::PageUp | Direction::PageDown => ROWS_SHOWN,
        };
        self.selected = match direction {
            // Wrapping, but only by one: arrowing off the bottom of a short
            // list is a common way to get back to the top, while paging off it
            // is someone looking for the end.
            Direction::Down if self.selected == last => 0,
            Direction::Up if self.selected == 0 => last,
            Direction::Down | Direction::PageDown => (self.selected + step).min(last),
            Direction::Up | Direction::PageUp => self.selected.saturating_sub(step),
        };
        self.scroll.scroll_to_item(self.selected);
        self.preview_selection(cx);
        cx.notify();
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.matches.len() {
            return;
        }
        self.selected = index;
        self.preview_selection(cx);
        cx.notify();
    }

    /// Theme rows apply as they are highlighted. Nothing else does: previewing
    /// "Close Tab" would be a strange thing for an arrow key to do.
    fn preview_selection(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.selected_item() else {
            return;
        };
        if let PaletteAction::Theme(appearance) = item.action {
            self.previewing = true;
            cx.emit(PaletteEvent::Preview(appearance));
        }
    }

    fn selected_item(&self) -> Option<&PaletteItem> {
        let m = self.matches.get(self.selected)?;
        self.candidates.get(m.index)
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.selected_item().cloned() else {
            return;
        };
        match item.action {
            // Entering a mode is the palette rearranging itself, not a command:
            // it replaces the query with the prefix and stays open.
            PaletteAction::EnterMode(mode) => {
                self.query
                    .update(cx, |editor, cx| editor.set_text(mode.prefix(), cx));
                self.rebuild(cx);
            }
            action => {
                // A previewed theme that is then chosen is already in effect;
                // saying so again would be harmless but confirming it here is
                // what stops the dismissal below from putting it back.
                self.previewing = false;
                cx.emit(PaletteEvent::Chose(action));
            }
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.previewing {
            cx.emit(PaletteEvent::Preview(self.original));
        }
        cx.emit(PaletteEvent::Dismissed);
    }

    /// Put text in the field as though it had been typed, keeping whatever
    /// prefix the palette was opened with. For the screenshot harness, which
    /// has no keyboard.
    pub fn type_in(&mut self, text: &str, cx: &mut Context<Self>) {
        let full = format!("{}{text}", self.mode.prefix());
        self.query
            .update(cx, |editor, cx| editor.set_text(&full, cx));
        self.rebuild(cx);
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        let prefix = self.mode.prefix().to_string();
        self.query
            .update(cx, |editor, cx| editor.set_text(&prefix, cx));
        self.rebuild(cx);
    }
}

/// `12` → line 12. Anything else is not a line number, and an empty `:` is
/// someone who has not typed one yet rather than someone who meant line zero.
fn parse_line(text: &str) -> Option<usize> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    text.parse::<usize>().ok().filter(|line| *line > 0)
}

/// Char positions from the matcher, as byte ranges into the label, with
/// adjacent characters merged so a contiguous match is one highlighted run
/// rather than five.
fn char_ranges(label: &str, positions: &[u32]) -> Vec<std::ops::Range<usize>> {
    let offsets: Vec<usize> = label
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(label.len()))
        .collect();
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    for &position in positions {
        let index = position as usize;
        let (Some(&start), Some(&end)) = (offsets.get(index), offsets.get(index + 1)) else {
            continue;
        };
        match ranges.last_mut() {
            Some(last) if last.end == start => last.end = end,
            _ => ranges.push(start..end),
        }
    }
    ranges
}

impl Render for Palette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors().clone();
        let m = cx.metrics().clone();
        let ty = cx.typography().clone();
        let empty = self.query.read(cx).text() == self.mode.prefix();

        let rows: Vec<_> = self
            .matches
            .iter()
            .enumerate()
            .filter_map(|(row, matched)| {
                let item = self.candidates.get(matched.index)?;
                Some(self.render_row(row, item, &matched.ranges, cx))
            })
            .collect();
        let nothing = rows.is_empty();

        div()
            .id("palette")
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            // Without this the card stretches to the bottom of the window:
            // the default cross-axis alignment is `stretch`, and a card that
            // is 600 wide and as tall as the screen is not a card.
            .items_start()
            // Near the top, not centred: the list grows downwards, and a card
            // that moves as it is typed into is a card that cannot be read.
            .pt(px(96.))
            // Clicking past the card dismisses it. No scrim: the palette is a
            // way to get at the window, and dimming what you are about to act
            // on helps nobody. Occluded all the same, so that a drag or a
            // wheel over the palette is not also a drag or a wheel on the grid
            // behind it.
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.dismiss(cx)),
            )
            .child(
                v_flex()
                    .w(px(600.))
                    .flex_none()
                    .bg(c.overlay)
                    .rounded(m.radius_lg)
                    .border_1()
                    .border_color(c.border_strong)
                    .shadow_lg()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    // ---- the field ------------------------------------
                    .child(
                        h_flex()
                            .flex_none()
                            .h(px(40.))
                            .px(px(10.))
                            .gap(px(8.))
                            .children(self.mode.chip().map(|text| {
                                div()
                                    .flex_none()
                                    .px(px(6.))
                                    .py(px(2.))
                                    .rounded(m.radius_sm)
                                    .bg(c.field)
                                    .border_1()
                                    .border_color(c.border)
                                    .child(
                                        Label::new(text)
                                            .size(LabelSize::Small)
                                            .color(IconColor::Muted),
                                    )
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h(ty.ui_line_height)
                                    .overflow_hidden()
                                    .child(self.query.clone()),
                            )
                            .when(!empty, |el| {
                                el.child(
                                    ui::Button::icon("palette-clear", IconName::XmarkSm)
                                        .size(ui::ButtonSize::XSmall)
                                        .tooltip(ui::Tooltip::text("Clear"))
                                        .on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
                                )
                            }),
                    )
                    .child(div().flex_none().h(px(1.)).w_full().bg(c.border))
                    // ---- the list -------------------------------------
                    .when(nothing, |el| {
                        el.child(
                            div().w_full().py(px(20.)).flex().justify_center().child(
                                Label::new(match self.mode {
                                    PaletteMode::Line => "Type a line number",
                                    _ => "No Results",
                                })
                                .color(IconColor::Subtle),
                            ),
                        )
                    })
                    .when(!nothing, |el| {
                        el.child(
                            v_flex()
                                .id("palette-list")
                                .py(px(4.))
                                .max_h(ROW_HEIGHT * ROWS_SHOWN as f32 + px(8.))
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll)
                                .children(rows),
                        )
                    })
                    // ---- the prefixes ---------------------------------
                    //
                    // Only on an empty mixed query: the moment someone knows
                    // what they are doing this row is noise, and the moment
                    // they do not it is the whole feature.
                    .when(empty && self.mode == PaletteMode::Mixed, |el| {
                        el.child(
                            h_flex()
                                .flex_none()
                                .h(px(26.))
                                .px(px(10.))
                                .gap(px(10.))
                                .border_t_1()
                                .border_color(c.border)
                                .bg(c.chrome)
                                .children(
                                    [
                                        (">", "commands"),
                                        ("@", "objects"),
                                        ("#", "themes"),
                                        (":", "line"),
                                        ("?", "help"),
                                    ]
                                    .into_iter()
                                    .map(|(prefix, what)| {
                                        h_flex()
                                            .gap(px(4.))
                                            .child(
                                                Label::new(prefix)
                                                    .size(LabelSize::Small)
                                                    .mono()
                                                    .color(IconColor::Default),
                                            )
                                            .child(
                                                Label::new(what)
                                                    .size(LabelSize::Small)
                                                    .color(IconColor::Subtle),
                                            )
                                    }),
                                ),
                        )
                    }),
            )
    }
}

impl Palette {
    fn render_row(
        &self,
        row: usize,
        item: &PaletteItem,
        ranges: &[std::ops::Range<usize>],
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = cx.colors().clone();
        let m = cx.metrics().clone();
        let ty = cx.typography().clone();
        let selected = row == self.selected;
        let text = if selected { c.text_on_accent } else { c.text };
        let dim = if selected {
            c.text_on_accent
        } else {
            c.text_subtle
        };

        let highlight = HighlightStyle {
            color: Some(if selected { c.text_on_accent } else { c.accent }),
            font_weight: Some(gpui::FontWeight::SEMIBOLD),
            ..Default::default()
        };

        h_flex()
            .id(("palette-row", row))
            .h(ROW_HEIGHT)
            .flex_none()
            .mx(px(4.))
            .px(px(6.))
            .gap(px(8.))
            .rounded(m.radius)
            .cursor_pointer()
            // `StyledText` paints in the ambient style rather than in one of
            // its own, so the row is where the label's face is decided. Left
            // to the window default it would come out a couple of points
            // larger than every other list in the app.
            .font_family(ty.ui_family.clone())
            .text_size(ty.ui_size)
            .line_height(ty.ui_line_height)
            .text_color(text)
            .when(selected, |el| el.bg(c.accent))
            .when(!selected, |el| el.hover(|s| s.bg(c.hover)))
            .on_mouse_move(cx.listener(move |this, _, _, cx| this.select(row, cx)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = row;
                this.confirm(cx);
            }))
            .child(
                Icon::new(item.icon)
                    .size(IconSize::Small)
                    .color(if selected {
                        IconColor::Custom(c.text_on_accent)
                    } else {
                        IconColor::Muted
                    }),
            )
            .child(
                div().flex_1().min_w_0().overflow_hidden().child(
                    StyledText::new(item.label.clone())
                        .with_highlights(ranges.iter().cloned().map(|r| (r, highlight))),
                ),
            )
            .children(item.detail.clone().map(|detail| {
                Label::new(detail)
                    .size(LabelSize::Small)
                    .color(IconColor::Custom(dim))
            }))
            .children(item.shortcut.clone().map(|keys| {
                Label::new(keys)
                    .size(LabelSize::Small)
                    .color(IconColor::Custom(dim))
            }))
            .when(item.current, |el| {
                el.child(
                    Icon::new(IconName::Check)
                        .size(IconSize::Small)
                        .color(IconColor::Custom(text)),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_chooses_the_mode() {
        assert_eq!(PaletteMode::from_query(""), PaletteMode::Mixed);
        assert_eq!(PaletteMode::from_query("users"), PaletteMode::Mixed);
        assert_eq!(PaletteMode::from_query(">run"), PaletteMode::Commands);
        assert_eq!(PaletteMode::from_query("@users"), PaletteMode::Objects);
        assert_eq!(PaletteMode::from_query("#dark"), PaletteMode::Themes);
        assert_eq!(PaletteMode::from_query(":42"), PaletteMode::Line);
        assert_eq!(PaletteMode::from_query("?"), PaletteMode::Help);
    }

    #[test]
    fn a_line_number_has_to_be_one_or_more() {
        assert_eq!(parse_line("12"), Some(12));
        assert_eq!(parse_line("  7 "), Some(7));
        assert_eq!(parse_line("0"), None);
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("12a"), None);
    }

    #[test]
    fn adjacent_matched_characters_become_one_run() {
        // `sav` against "Save Query": three characters, one highlight.
        assert_eq!(char_ranges("Save Query", &[0, 1, 2]), vec![0..3]);
        // And a gap splits it.
        assert_eq!(char_ranges("Save Query", &[0, 5]), vec![0..1, 5..6]);
    }

    #[test]
    fn a_highlight_range_lands_on_character_boundaries() {
        // The matcher counts characters; the highlighter wants bytes.
        let label = "Ärger Query";
        assert_eq!(char_ranges(label, &[0]), vec![0..2]);
        assert_eq!(char_ranges(label, &[1]), vec![2..3]);
    }

    #[test]
    fn every_command_has_a_label_and_an_icon() {
        // The list is what the palette shows; a command missing from it is a
        // command nobody can reach.
        for command in Command::ALL {
            assert!(!command.label().is_empty(), "{command:?}");
        }
        assert_eq!(
            Command::ALL.len(),
            33,
            "add the new command to ALL, not just to the enum"
        );
    }
}
