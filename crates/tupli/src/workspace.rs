//! The workspace: the one entity that owns window layout.
//!
//! Layout is deliberately centralised. Panels do not know their own size and
//! cannot resize themselves; they render into whatever box the workspace hands
//! them. That is what makes the four regions — left panel, centre stack, bottom
//! dock, right panel — behave identically, and what will make persisting a
//! layout a matter of serialising one struct.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    div, prelude::*, px, App, Context, Entity, FocusHandle, Focusable, IntoElement, KeyDownEvent,
    MouseButton, MouseMoveEvent, ParentElement, Pixels, Point, Render, SharedString, Window,
};
use ui::{
    h_flex, v_flex, ActiveTheme, Axis, BadgeTone, Icon, IconColor, IconName, IconSize, Label,
    LabelSize, ResizeHandle, StatusBar,
};

use editor::{Editor, EditorMode, Input};
use grid::{Grid, GridEvent};

use crate::mock;
use crate::palette::{
    Command, ItemKind, Palette, PaletteAction, PaletteEvent, PaletteItem, PaletteMode,
};
use crate::pane::{Layout, Pane, PaneGroup, PaneId};
use crate::results::{one_line, MessageTone, ResultsTab, RunMessage, MESSAGES_KEPT};
use crate::save_sheet::{SaveQuerySheet, SaveSheetEvent};
use crate::session::{Activity, Session, SessionEvent, SessionState};
use crate::titlebar::{RunAction, Titlebar};
use crate::tree::{self, TreeNode};

// The tabs live with the pane that holds them; everything that already
// names them through the workspace keeps working.
pub use crate::pane::{CenterKind, CenterTab};

/// Which of the sidebar's tabs is showing.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SidebarTab {
    Database,
    Queries,
    History,
}

/// Which face of the current selection the right panel is showing.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InspectorTab {
    /// The selected row as a field list.
    Row,
    /// What the catalog knows about the table the rows came from.
    Table,
}

/// A splitter currently under the mouse button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DragTarget {
    LeftPanel,
    RightPanel,
    BottomDock,
    /// A seam inside the pane tree: which group it belongs to, and which
    /// member it comes after. Not a pane id, because the thing on either side
    /// of a seam can be a whole group rather than a pane.
    Seam {
        path: Vec<usize>,
        index: usize,
    },
}

struct Drag {
    target: DragTarget,
    origin: Point<Pixels>,
    /// Where a panel splitter started, in pixels.
    initial: Pixels,
    /// Where a pane seam started, as a fraction of its group. Panels are sized
    /// in pixels and panes in fractions, and a drag has to remember whichever
    /// of the two it began from — an incremental nudge per mouse-move would
    /// compound rounding over the hundreds of frames a drag lasts.
    initial_flex: f32,
}

pub struct Workspace {
    focus: FocusHandle,

    // ---- layout ----------------------------------------------------------
    pub left_open: bool,
    pub right_open: bool,
    pub dock_open: bool,
    pub left_width: Pixels,
    pub right_width: Pixels,
    pub dock_height: Pixels,
    /// The dock has been given the whole centre for a moment. Not the same as
    /// the collapse a table tab causes: that one is a consequence of what the
    /// tab is, this one is a person saying "the rows are what I am reading
    /// right now". Deliberately not saved with the layout — it is a gesture
    /// about the next two minutes, and a window that reopened with its console
    /// hidden would look broken.
    pub dock_maximized: bool,
    drag: Option<Drag>,

    // ---- sidebar ---------------------------------------------------------
    pub sidebar_tab: SidebarTab,
    pub tree: Vec<TreeNode>,
    pub collapsed: HashSet<usize>,
    pub selected_node: Option<usize>,
    /// The sidebar's filter field. A real editor in single-line configuration,
    /// like every other field in the app.
    pub tree_filter: Entity<Input>,

    // ---- centre ----------------------------------------------------------
    /// Every pane in the window, in no particular order — [`Workspace::layout`]
    /// is what says where they are.
    pub panes: Vec<Pane>,
    /// How the panes are arranged, and how much room each one has.
    pub layout: PaneGroup,
    /// The pane keystrokes go to and the dock reports on.
    pub active_pane: PaneId,
    /// The next id to hand out. Never reused, so a stale id names a pane that
    /// is gone rather than someone else's.
    next_pane: PaneId,
    /// The pane whose statement is in flight, so its rows land in it even if
    /// the focus moved to another pane while the server was thinking.
    running_pane: Option<PaneId>,
    /// How long each group's box came out at the last paint, along the axis
    /// its seams move in, keyed by the group's path. Measured because a seam
    /// drag arrives in pixels and the tree is kept in fractions, and there is
    /// no other way across. A cell rather than a plain field so that measuring
    /// is not a change to the workspace and does not ask for another frame.
    pub(crate) group_boxes: Rc<RefCell<HashMap<Vec<usize>, f32>>>,
    /// Cleared after the first frame, which is the earliest point at which
    /// there is a window to hand focus to.
    booted: bool,

    // ---- results ---------------------------------------------------------
    /// The Messages tab's log, oldest first, capped at [`MESSAGES_KEPT`]. Held
    /// in memory rather than in SQLite because it is about *this window's*
    /// session — the durable record of what was run is the History tab. One
    /// log for the window, not one per pane: it is a record of what this
    /// connection was asked, and which editor did the asking is not the
    /// question anyone brings to it.
    pub messages: Vec<RunMessage>,
    /// The DDL tab's viewer: the same editor as the console, read-only, so
    /// generated SQL is highlighted, selectable and scrollable exactly the way
    /// typed SQL is. One for the window, like the dock it lives in.
    pub(crate) ddl_view: Entity<Editor>,
    /// What is currently in that buffer: the object, and which catalog read it
    /// was rendered from. Both, because a refresh can change an object's DDL
    /// without changing its name, and re-filling the buffer on every frame
    /// would throw away the scroll position and the selection.
    pub(crate) ddl_source: Option<(db::RelationRef, usize)>,

    // ---- connection ------------------------------------------------------
    /// This machine's memory: the saved connection list and query history.
    /// `None` if the file could not be opened, which the app survives — it just
    /// forgets everything when it quits.
    pub store: Option<std::rc::Rc<store::Store>>,
    pub connections: Vec<db::ConnectionConfig>,
    /// The History tab's rows, read from SQLite rather than re-queried on every
    /// frame. Refreshed when a statement finishes and when the tab is opened.
    pub history: Vec<store::HistoryEntry>,
    /// The Queries tab's rows, on the same terms.
    pub saved: Vec<store::SavedQuery>,
    /// The connection the *active tab* is looking at, which is what the
    /// sidebar, the titlebar and the status bar are all describing. Every
    /// reader of "the current connection" goes through here; what changes it
    /// is activating a tab.
    pub session: Option<Entity<Session>>,
    /// Every connection this window has open, one per database.
    ///
    /// A window is a set of tabs and each tab names its own connection, so
    /// this is what keeps the other tabs' connections alive and warm while
    /// you are looking at one of them: coming back to a tab is a repaint
    /// rather than a reconnect. Keyed by nothing — the list is short, and the
    /// key is the pair (connection, database) that
    /// [`Workspace::session_for`] matches on.
    sessions: Vec<Entity<Session>>,
    /// The history row the in-flight statement is being recorded into.
    pending_history: Option<i64>,
    /// The name-this-query sheet, while it is up.
    pub save_sheet: Option<Entity<SaveQuerySheet>>,
    /// The export sheet, while it is up.
    pub(crate) export_sheet: Option<Entity<crate::export::ExportSheet>>,
    /// The import sheet, while it is up.
    pub(crate) import_sheet: Option<Entity<crate::import::ImportSheet>>,
    /// What the transaction now in flight is, when it is an import: how many
    /// rows and out of which file.
    ///
    /// A commit and an import come back through the same door, and the log
    /// entry is the only record either one leaves. "Committed 6 inserts" under
    /// a button that said Import is the app describing its own plumbing.
    pub(crate) import_note: Option<SharedString>,
    /// The command palette, while it is up.
    pub palette: Option<Entity<Palette>>,
    /// What the consoles complete against. Handed to every console editor as
    /// it is built and re-filled from the catalog each time the schema is read,
    /// so the offers in the editor and the objects in the sidebar are the same
    /// list seen twice.
    pub(crate) catalog: crate::complete::Catalog,
    /// What was chosen on purpose, as opposed to arrived at: see
    /// [`crate::settings`]. Held so a change can be written back without
    /// reading the file again.
    pub(crate) settings: crate::settings::Settings,
    /// Something that should take focus at the next paint. Sheets are opened
    /// from event handlers, which have no window; this is where they leave the
    /// request until `render` can honour it.
    pending_focus: Option<FocusHandle>,
    /// The connection the last session was on, waiting for the first frame.
    /// Consumed by [`Workspace::boot_from_environment`], which is where every
    /// other at-launch decision is made too.
    reopen: Option<uuid::Uuid>,
    /// And which database on it, when that was not the connection's own
    /// default. Kept beside `reopen` rather than folded into it because the
    /// two can disagree: the connection still exists, the database it names
    /// may since have been dropped, and only the first of those is fatal.
    reopen_database: Option<String>,
    /// Kept alive so the quit callback stays registered. Dropping the
    /// subscription would silently unregister it.
    _on_quit: Option<gpui::Subscription>,
    /// A table named by `TUPLI_OPEN`, waiting for the catalog to arrive.
    pending_open: Vec<db::RelationRef>,
    /// A key named by `TUPLI_KEY`, waiting for the walk to reach it. Not the
    /// same wait as `pending_open`: a keyspace catalog knows the databases and
    /// nothing about what is in them, so the key's type — which is what
    /// decides how to read it — only turns up when the scan does.
    pending_key: Option<String>,
    /// `TUPLI_PAGE`: the page a browsed table should open on. Applied to the
    /// statement rather than by pressing `›` afterwards, because a second run
    /// issued while the first is still in flight is dropped — see
    /// [`crate::session::Session::run`].
    pending_page: Option<usize>,
    /// `TUPLI_SWITCH`: a database to move to once the first catalog is in, so
    /// that a reconnect-in-place can be driven without a click. Taken rather
    /// than read, because the switch produces a catalog of its own and a flag
    /// that survived it would switch for ever.
    pending_switch: Option<String>,
    /// `TUPLI_AFTER`: a table to browse once the database `TUPLI_SWITCH` asked
    /// for is open, so a window with one tab on each of two databases can be
    /// photographed. Screenshots only.
    pending_after: Option<db::RelationRef>,
    /// `TUPLI_CELL`: the `row,column` to put the cursor on once rows land, so
    /// the value panel can be photographed over something other than the first
    /// cell of the first row. Screenshots only, and consumed by the first
    /// result that arrives.
    pending_cell: Option<(usize, usize)>,
    /// A table to browse again as soon as the catalog it belongs to arrives.
    ///
    /// Set when a browsing tab is moved to another database: the rows on
    /// screen came out of the old one, and the new session has not connected
    /// yet, so the ask has to wait for the catalog rather than be sent into a
    /// socket that is still opening — a statement issued before then is
    /// dropped (see [`crate::session::Session::run`]).
    pending_rebrowse: Option<db::RelationRef>,
    /// `TUPLI_FOLLOW`: press F6 once the first rows land, so the hop a person
    /// makes with a keystroke can be photographed. Screenshots only; there is
    /// nothing to follow until there is a cursor on a row.
    pending_follow: bool,
    /// A demo object menu or sheet asked for by `TUPLI_MENU` / `TUPLI_SHEET`,
    /// also waiting for the catalog. Screenshots only: there is no other way to
    /// photograph a menu that only exists while a mouse button is down.
    pending_demo: Option<(db::RelationRef, Option<crate::objects::ObjectOp>)>,
    /// `TUPLI_DESIGN`: the table to open the structure editor on once there is
    /// a catalog, and whether to put the preview sheet over it. `None` for the
    /// reference means a table that does not exist yet.
    pending_design: Option<(Option<db::RelationRef>, bool)>,
    /// `TUPLI_DECODER`: the inspector column whose chain menu to open, once
    /// there is a row with something decodable in it. Screenshots only, for
    /// the same reason as [`Self::pending_demo`].
    pub(crate) pending_decoder: Option<String>,
    /// This window, learned on the first frame. Held so that something
    /// started in another window — editing a connection from Settings — can
    /// bring the window the sheet actually appears in to the front.
    window: Option<gpui::AnyWindowHandle>,
    /// The Settings window, while it is open. A handle rather than an entity:
    /// it is a window of its own, and this window only needs to be able to
    /// raise it and to know that it exists.
    settings_window: Option<gpui::WindowHandle<crate::settings_window::SettingsWindow>>,
    /// The connection window, while it is open. Like Settings: a window of its
    /// own, and this one only needs to raise it and to know it exists.
    connection_window: Option<gpui::WindowHandle<crate::connection_window::ConnectionWindow>>,
    /// Set when the theme changed under the window. Nothing observes the theme
    /// global, so every colour cached in every element is stale and the whole
    /// window has to be told — which needs a `Window`, which only `render` has.
    pending_refresh: bool,

    // ---- inspector -------------------------------------------------------
    pub inspector_tab: InspectorTab,
    /// The Row tab's field that has been asked to show all of itself, by column
    /// index. One at a time: two expanded documents in a 260px panel is a
    /// scroll with no landmarks, and the question a reader has is about one
    /// field.
    pub(crate) expanded_field: Option<usize>,
    /// A decoder chain somebody chose by hand, by column name.
    ///
    /// Per column rather than per cell because a column of MessagePack blobs is
    /// MessagePack in every row of it, and by name rather than by index so the
    /// choice survives the next key: every hash in a keyspace has a `value`
    /// column, and being told twice how to read the same convention is the
    /// thing a viewer exists to avoid.
    pub(crate) field_decoders: HashMap<String, Vec<db::Decoder>>,
    /// The chain menu, while it is up.
    pub(crate) decoder_menu: Option<crate::inspector::DecoderMenu>,

    // ---- objects ---------------------------------------------------------
    /// The context menu, while it is open.
    pub(crate) menu: Option<crate::objects::ObjectMenu>,
    /// The grid's own context menu, while it is open.
    pub(crate) row_menu: Option<crate::clipboard::RowMenu>,
    /// Where the titlebar's database switcher was clicked, while its menu is
    /// up. A point rather than a bool: the menu opens under the chevron.
    pub(crate) database_menu: Option<Point<Pixels>>,
    /// The chip composer's column or operator list, while it is up, and where
    /// it was asked for.
    pub(crate) filter_menu: Option<(Point<Pixels>, FilterMenu)>,
    /// The rename/truncate/drop sheet, while it is up.
    pub(crate) object_sheet: Option<Entity<crate::objects::ObjectSheet>>,
    /// An object statement that has been sent and not yet answered, and what
    /// the window should do about it when it is.
    pub(crate) pending_object: Option<crate::objects::PendingObject>,

    // ---- structure -------------------------------------------------------
    /// The statements a structure save is about to send, while they are being
    /// read.
    pub(crate) structure_preview: Option<crate::structure::StructurePreview>,
    /// A structure save that has been sent and not yet answered.
    pub(crate) pending_structure: Option<crate::structure::PendingStructure>,
}

/// The first pane's id. Ids are handed out in order and never reused, so this
/// one is only ever the pane a window opens with.
const FIRST_PANE: PaneId = 0;

/// How many keys the browser will walk to before it stops on its own.
///
/// Not a page size and not a total — the tree says "scanned" rather than
/// "keys" for exactly this reason. A keyspace can hold tens of millions of
/// keys and no tree is a useful way to look at those; past this many, the way
/// to find a key is to search for it.
const KEY_BROWSER_LIMIT: usize = 5_000;

/// A pane, wired to the workspace that will hold it.
///
/// The entities are made here rather than inside `Pane` because every one of
/// them reports back — the grid's cursor drives the inspector, the filter's
/// Enter re-asks the server, the console's ⌘⏎ runs a statement — and a
/// subscription needs a `Context<Workspace>`, which is the one thing a pane
/// cannot get for itself. Each closure captures its own pane's id, so an event
/// from a pane that is not the active one still lands in the right place.
fn build_pane(
    id: PaneId,
    data: db::ResultSet,
    sql: &str,
    catalog: &crate::complete::Catalog,
    cx: &mut Context<Workspace>,
) -> Pane {
    let grid = cx.new(|cx| {
        let mut grid = Grid::new(data, cx);
        if grid::bench::FrameMeter::enabled() {
            grid.start_benchmark();
        }
        grid
    });

    // The inspector reads the cursor, so the workspace mirrors it. The grid
    // itself stays ignorant of the inspector's existence.
    cx.subscribe(&grid, move |this, _, event, cx| match event {
        GridEvent::CursorMoved { row, col } => {
            // Touching a pane is what makes it the active one. Nothing else
            // has to be said about focus: the dock and the inspector both read
            // whichever pane this is.
            this.active_pane = id;
            if let Some(pane) = this.pane_by_mut(id) {
                pane.selected_row = Some(*row);
                pane.selected_column = *col;
            }
            cx.notify();
        }
        // The grid records which way the arrow points; putting the rows in
        // that order is this crate's job, because only this crate knows
        // whether the rows on screen are the whole table.
        GridEvent::SortChanged { .. } => {
            this.active_pane = id;
            this.apply_sort(cx);
        }
        GridEvent::Activated { .. } => {}
        GridEvent::ContextMenu { at, row, col } => {
            this.active_pane = id;
            this.open_row_menu(*at, *row, *col, cx);
        }
        // Staged, not saved. The toolbar's counts and the tab's dirty dot both
        // read the grid, so all this has to do is ask for a frame.
        GridEvent::ChangesEdited => {
            this.active_pane = id;
            cx.notify();
        }
    })
    .detach();

    // The console. `set_sql` brings the highlighter and the statement marker
    // with it; everything else about it is the same editor the filter fields
    // use.
    let editor = cx.new(|cx| {
        let mut editor = Editor::new(EditorMode::Full, cx);
        editor.set_sql(true);
        editor.set_text(sql, cx);
        // A handle on the window's catalog, not a copy of it: the console in a
        // pane built before the connection was made still completes against the
        // schema once it arrives.
        editor.set_completions(catalog.clone());
        editor
    });
    // The clause is SQL, so it is highlighted and set in mono like the console
    // above it. A `where` clause in the UI font, with its keywords the same
    // colour as its column names, is the one field in the window that lies
    // about what it holds.
    let filter = cx.new(|cx| {
        let input = Input::new(cx).placeholder("where …", cx);
        let style = editor::EditorStyle::code(cx);
        input.editor().update(cx, |editor, _| {
            editor.set_sql(true);
            editor.set_style(style);
        });
        input
    });
    // Bare, because the chip around it is already the box. See
    // `Input::bare`.
    let chip_value = cx.new(|cx| Input::new(cx).bare().placeholder("value", cx));

    // Enter in the composer commits the chip and re-asks the server, which is
    // the whole gesture: pick a column, type a value, press return.
    cx.subscribe(
        &chip_value,
        move |this, _, event: &editor::EditorEvent, cx| match event {
            editor::EditorEvent::Submit => {
                this.active_pane = id;
                this.commit_chip(cx);
            }
            // Escape abandons the chip rather than committing half of it.
            editor::EditorEvent::Cancel => {
                this.active_pane = id;
                this.close_chip(cx);
            }
            _ => {}
        },
    )
    .detach();

    // Enter in the filter re-asks the server, rather than hiding rows the
    // client already has. Filtering in the client would only ever filter the
    // first fifty thousand rows, which is a lie the moment the table is bigger
    // than that.
    cx.subscribe(&filter, move |this, _, event: &editor::EditorEvent, cx| {
        if matches!(event, editor::EditorEvent::Submit) {
            this.active_pane = id;
            this.apply_filter(cx);
        }
    })
    .detach();

    // ⌘⏎ in the console runs whatever the cursor is in. The editor only
    // reports the gesture; deciding what "run" means is the workspace's job.
    cx.subscribe(
        &editor,
        move |this, editor, event: &editor::EditorEvent, cx| match event {
            editor::EditorEvent::Run => {
                let _ = &editor;
                this.active_pane = id;
                this.run_console(cx);
            }
            // ⌘⇧⏎ runs the whole script, one statement at a time.
            editor::EditorEvent::RunAll => {
                this.active_pane = id;
                this.run_console_all(cx);
            }
            // ⌘S. Same entry point as the toolbar button, so neither can save
            // something the other would not.
            editor::EditorEvent::Save => {
                this.active_pane = id;
                this.save_query(cx);
            }
            _ => {}
        },
    )
    .detach();

    Pane::new(id, editor, grid, filter, chip_value)
}

/// The settings a pane's widgets carry themselves rather than reading off the
/// theme. Applied when a pane is built and again when the settings change, from
/// one list so the two can never drift.
fn dress_pane(pane: &Pane, settings: &crate::settings::Settings, cx: &mut App) {
    let (tab_size, line_numbers) = (settings.tab_size(), settings.line_numbers());
    pane.editor.update(cx, |editor, _| {
        editor.set_tab_size(tab_size);
        editor.set_line_numbers(line_numbers);
    });
    let (density, zebra) = (settings.row_density(), settings.zebra());
    pane.grid.update(cx, |grid, cx| {
        grid.set_density(density, cx);
        grid.set_zebra(zebra, cx);
    });
}

impl Workspace {
    /// The pane keystrokes go to, and the one the dock and the inspector are
    /// reporting on.
    ///
    /// Panics if the active id names no pane, which would mean the tree and the
    /// pane list had come apart — a bug in splitting or closing rather than a
    /// state the window can reach.
    pub fn pane(&self) -> &Pane {
        self.pane_by(self.active_pane)
            .expect("the active pane is in the list")
    }

    pub fn pane_mut(&mut self) -> &mut Pane {
        let id = self.active_pane;
        self.pane_by_mut(id)
            .expect("the active pane is in the list")
    }

    /// The catalog this window is working from, if it has one.
    ///
    /// Every reader of the schema goes through here: the tree, the Structure
    /// and DDL tabs, the SQL generator and the object menu all describe the
    /// same objects, and they can only be trusted to agree if they are reading
    /// the same snapshot.
    pub(crate) fn snapshot(&self, cx: &App) -> Option<std::sync::Arc<db::SchemaSnapshot>> {
        self.session
            .as_ref()
            .and_then(|session| session.read(cx).snapshot.clone())
    }

    /// The table the active tab names, when the open connection has no such
    /// table. A tab restored from the last run carries a name that was true of
    /// whatever was connected then, and reconnecting somewhere else is common
    /// enough — a pooler, a replica, the same app's staging database — that
    /// the window has to be able to say so plainly. `None` while there is no
    /// catalog to check against, because "not found" and "not looked yet" are
    /// different answers and only one of them is worth a banner.
    pub fn absent_relation(&self, cx: &App) -> Option<db::RelationRef> {
        let snapshot = self.snapshot(cx)?;
        let relation = self
            .pane()
            .active()
            .filter(|tab| tab.kind == CenterKind::Table)
            .and_then(|tab| tab.relation.clone())?;
        match snapshot.relation(&relation) {
            Some(_) => None,
            None => Some(relation),
        }
    }

    pub fn pane_by(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.id == id)
    }

    pub fn pane_by_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|pane| pane.id == id)
    }

    /// Split the active pane, putting a new one beside it or below it.
    ///
    /// The new pane starts empty — one untitled query, no rows. A split is for
    /// holding two things at once; a second view of the same thing is what a
    /// second tab is for, and copying the console into both halves would only
    /// make it ambiguous which copy ⌘S was about to write.
    pub fn split_pane(&mut self, layout: Layout, cx: &mut Context<Self>) {
        // The console's text belongs to the tab that is showing, and a split
        // is a moment where it could otherwise be left behind.
        self.stash_editor(cx);

        let target = self.active_pane;
        if !self.layout.contains(target) {
            log::warn!("cannot split pane {target}: it is not in the tree");
            return;
        }

        let id = self.next_pane;
        let mut pane = build_pane(id, db::ResultSet::new(Vec::new()), "", &self.catalog, cx);
        dress_pane(&pane, &self.settings.clone(), cx);
        pane.tabs.push(CenterTab {
            kind: CenterKind::Query,
            title: "Untitled".into(),
            detail: None,
            dirty: false,
            relation: None,
            key: None,
            saved_query: None,
            sql: String::new(),
            filter: crate::filter::Filter::default(),
            page: None,
            structure: None,
            // A split shows the same connection in both halves: it is one
            // window looking at one thing two ways until it is told otherwise.
            session: self.session.clone(),
            reconnect: None,
        });

        self.layout.split(target, id, layout);
        self.next_pane += 1;
        self.panes.push(pane);
        self.active_pane = id;
        self.focus_editor(cx);
        // A split is a change to what the window is showing, the same as
        // opening a tab is, and it is remembered on the same terms.
        self.save_session(cx);
        cx.notify();
    }

    /// Close a pane and hand its room back to its neighbours.
    ///
    /// The last pane stays. A centre with no editor in it is not a window with
    /// one fewer split — it is a window with nothing in it, which is not what
    /// anyone means by closing a split.
    pub fn close_pane(&mut self, id: PaneId, cx: &mut Context<Self>) {
        if self.panes.len() <= 1 {
            return;
        }
        // Which pane the eye should land on afterwards, worked out while the
        // order still includes the one going away: its neighbour to the right,
        // or to the left if it was last.
        let order = self.layout.panes();
        let next = order
            .iter()
            .position(|&other| other == id)
            .map(|index| order[(index + 1).min(order.len() - 1)])
            .filter(|&other| other != id)
            .or_else(|| order.iter().rev().find(|&&other| other != id).copied());

        if !self.layout.remove(id) {
            return;
        }
        self.panes.retain(|pane| pane.id != id);
        // Its answer, if one is still in flight, has nowhere to land.
        if self.running_pane == Some(id) {
            self.running_pane = None;
        }
        if self.active_pane == id {
            self.active_pane = next.unwrap_or(FIRST_PANE);
        }
        self.focus_editor(cx);
        self.save_session(cx);
        cx.notify();
    }

    /// Make a pane the one the dock, the inspector and the keyboard are about.
    pub fn activate_pane(&mut self, id: PaneId, cx: &mut Context<Self>) {
        if self.active_pane == id || self.pane_by(id).is_none() {
            return;
        }
        self.stash_editor(cx);
        self.active_pane = id;
        // The window describes the pane the keyboard is in, and every tab has
        // its own connection: crossing a split moves the sidebar, the titlebar
        // and where Run would send a statement, all at once.
        let active = self.pane().active_tab;
        self.adopt_tab(active, cx);
        self.focus_editor(cx);
        cx.notify();
    }
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let metrics = cx.metrics().clone();

        // `TUPLI_ROWS` / `TUPLI_COLS` drive the M0 benchmark (docs/PLAN.md §16):
        // with `TUPLI_COLS` set the grid gets the synthetic wide result set
        // instead of the mock users table, and `TUPLI_FPS` starts the meter.
        let rows = env_usize("TUPLI_ROWS").unwrap_or(100_000);
        let cols = env_usize("TUPLI_COLS");
        let started = std::time::Instant::now();
        let data = match cols {
            Some(cols) => grid::bench::synthetic(rows, cols),
            None => mock::result_set(rows),
        };
        let row_count = data.row_count();
        log::info!(
            "result set: {} rows x {} columns, {:.1} MB, built in {:.2}s",
            row_count,
            data.column_count(),
            data.heap_size() as f64 / 1_048_576.,
            started.elapsed().as_secs_f64(),
        );

        let catalog = crate::complete::Catalog::default();
        let pane = build_pane(FIRST_PANE, data, mock::SAMPLE_SQL, &catalog, cx);

        // Read-only, and no line numbers: this is a document about an object,
        // not a script anyone is going to be told the error line of.
        let ddl_view = cx.new(|cx| {
            let mut editor = Editor::new(EditorMode::Full, cx);
            editor.set_sql(true);
            editor.set_read_only(true);
            editor.set_line_numbers(false);
            editor
        });

        let tree_filter = cx.new(|cx| {
            Input::new(cx)
                .icon(IconName::Magnifier)
                .placeholder("Filter…", cx)
        });

        // The tree filter is read straight out of the field at render time, so
        // a change only has to invalidate the frame.
        cx.subscribe(&tree_filter, |_, _, event: &editor::EditorEvent, cx| {
            if matches!(event, editor::EditorEvent::Changed) {
                cx.notify();
            }
        })
        .detach();

        // A store that will not open is a bad day, not a fatal one: everything
        // it holds is a convenience, and refusing to launch over it would be
        // the worse failure.
        let store = match store::Store::open() {
            Ok(store) => Some(std::rc::Rc::new(store)),
            Err(error) => {
                log::error!("could not open the local store: {error:#}");
                None
            }
        };
        let connections = store
            .as_ref()
            .and_then(|store| store.connections().ok())
            .unwrap_or_default();

        // An unbounded history is a slow leak that only shows up on the
        // machines that have had the app longest, so boot is where it gets cut.
        if let Some(store) = store.as_ref() {
            match store.prune_history(now_ms() - HISTORY_KEPT_MS) {
                Ok(0) => {}
                Ok(n) => log::info!("pruned {n} history entries older than 90 days"),
                Err(error) => log::warn!("could not prune history: {error:#}"),
            }
        }
        let history = store
            .as_ref()
            .and_then(|store| store.recent_queries(HISTORY_SHOWN).ok())
            .unwrap_or_default();
        let saved = store
            .as_ref()
            .and_then(|store| store.saved_queries(None).ok())
            .unwrap_or_default();

        // Whatever the panels looked like when the app last quit. Every field
        // falls back to the built-in default, so a first launch and a corrupt
        // settings row produce the same, correct window.
        let layout = crate::layout::Layout::load(store.as_deref());

        // What you last chose, applied before the first paint so the window
        // never flashes something else. `TUPLI_THEME` wins over the saved
        // appearance, because it exists so a screenshot can be taken of either
        // one regardless of what this machine prefers — it says nothing about
        // the accent or the code size, which come from the file either way.
        let settings = crate::settings::Settings::load(store.as_deref());
        let appearance = match std::env::var_os("TUPLI_THEME").is_some() {
            true => cx.theme().appearance,
            false => settings.appearance().unwrap_or(cx.theme().appearance),
        };
        ui::Theme::set_global(settings.theme(appearance, cx), cx);

        // The knobs that live on a widget rather than in the theme. Set here
        // and again in `apply_settings`, which is the path a change takes.
        dress_pane(&pane, &settings, cx);

        // What the window was showing when it last quit, unless that was
        // turned off. A first launch has nothing here and falls through to the
        // sample tabs below, which is also what the screenshot renderer gets.
        let session = match settings.restore_session() {
            true => crate::restore::State::load(store.as_deref()),
            false => crate::restore::State::default(),
        };
        let restored = session.panes();
        let reopen = match settings.reopen_connection() {
            true => session.connection,
            false => None,
        };
        let reopen_database = reopen.and(session.database.clone());

        // A window that is about to open a connection has no business showing
        // the sample. Its rows came from nowhere, its tabs name objects that
        // may not exist on the server, and the first thing anybody would do
        // with them is believe them. The demo is for a launch with nothing to
        // connect to — a first run, and the screenshot renderer.
        let connecting = reopen.is_some() || std::env::var_os("TUPLI_CONNECT").is_some();
        let row_count = match connecting {
            true => 0,
            false => row_count,
        };
        if connecting {
            pane.grid.update(cx, |grid, cx| {
                grid.set_data(db::ResultSet::new(Vec::new()), cx)
            });
            pane.editor.update(cx, |editor, cx| editor.set_text("", cx));
        }

        // The first pane is the one the window was built around: it holds the
        // mock or benchmark result set, and any pane restored beside it comes
        // up empty, because rows belong to the server and a restart is exactly
        // the moment to go and ask again.
        let mut panes = vec![pane];
        let mut restored = restored.unwrap_or_else(|| match connecting {
            // Nothing to put back and nothing to invent: the tree arrives in a
            // moment and the first tab is whatever gets opened out of it.
            true => vec![(Vec::new(), 0)],
            false => vec![(
                vec![
                    CenterTab {
                        kind: CenterKind::Query,
                        title: "mrr_by_plan.sql".into(),
                        detail: None,
                        dirty: true,
                        relation: None,
                        key: None,
                        saved_query: None,
                        // The tab that is showing does not hold its own text; the
                        // console does, and hands it back when the tab is left.
                        sql: String::new(),
                        filter: crate::filter::Filter::default(),
                        page: None,
                        structure: None,
                        // The demo window is talking to nobody.
                        session: None,
                        reconnect: None,
                    },
                    CenterTab {
                        kind: CenterKind::Table,
                        title: "users".into(),
                        detail: Some("public".into()),
                        dirty: false,
                        relation: None,
                        key: None,
                        saved_query: None,
                        sql: String::new(),
                        filter: crate::filter::Filter::default(),
                        page: None,
                        structure: None,
                        session: None,
                        reconnect: None,
                    },
                ],
                0,
            )],
        });

        let (tabs, active_tab) = restored.remove(0);
        let sql = tabs
            .get(active_tab)
            .map(|tab| tab.sql.clone())
            .unwrap_or_default();
        if !sql.is_empty() {
            panes[0]
                .editor
                .update(cx, |editor, cx| editor.set_text(&sql, cx));
        }
        panes[0].tabs = tabs;
        panes[0].active_tab = active_tab;
        panes[0].row_count = row_count;
        panes[0].selected_row = Some(0);

        // Whatever else was open beside it. Ids are handed out in the order the
        // panes were written, which is what makes the saved tree's indices mean
        // something.
        for (index, (tabs, active_tab)) in restored.into_iter().enumerate() {
            let id = FIRST_PANE + 1 + index;
            let sql = tabs
                .get(active_tab)
                .map(|tab| tab.sql.clone())
                .unwrap_or_default();
            let mut pane = build_pane(id, db::ResultSet::new(Vec::new()), &sql, &catalog, cx);
            dress_pane(&pane, &settings, cx);
            pane.tabs = tabs;
            pane.active_tab = active_tab;
            panes.push(pane);
        }
        let pane_tree = session
            .tree(panes.len())
            .unwrap_or_else(|| PaneGroup::new(FIRST_PANE));
        let active_pane = session.active_pane(panes.len());
        let next_pane = panes.len();

        Self {
            focus: cx.focus_handle(),

            left_open: layout.left_open.unwrap_or(true),
            right_open: layout.right_open.unwrap_or(true),
            dock_open: layout.dock_open.unwrap_or(true),
            left_width: layout
                .left_width
                .map(px)
                .unwrap_or(metrics.panel_default_width),
            right_width: layout.right_width.map(px).unwrap_or(px(300.)),
            dock_height: layout
                .dock_height
                .map(px)
                .unwrap_or(metrics.dock_default_height),
            dock_maximized: false,
            drag: None,

            sidebar_tab: SidebarTab::Database,
            // Same reasoning as the sample tabs: a tree of objects that are not
            // on the server being dialled is worse than an empty panel, because
            // it can be clicked.
            tree: match connecting {
                true => Vec::new(),
                false => mock::tree(),
            },
            collapsed: HashSet::new(),
            selected_node: match connecting {
                true => None,
                false => Some(12),
            },
            tree_filter,

            panes,
            layout: pane_tree,
            active_pane,
            next_pane,
            running_pane: None,
            sessions: Vec::new(),
            group_boxes: Rc::new(RefCell::new(HashMap::new())),
            booted: false,

            messages: Vec::new(),
            ddl_view,
            ddl_source: None,

            store,
            connections,
            history,
            saved,
            session: None,
            pending_history: None,
            save_sheet: None,
            export_sheet: None,
            import_sheet: None,
            import_note: None,
            palette: None,
            catalog,
            settings,
            window: None,
            settings_window: None,
            connection_window: None,
            reopen,
            reopen_database,
            _on_quit: None,
            pending_focus: None,
            pending_open: Vec::new(),
            pending_key: None,
            pending_page: None,
            pending_switch: None,
            pending_after: None,
            pending_cell: None,
            pending_rebrowse: None,
            pending_follow: false,
            pending_demo: None,
            pending_design: None,
            pending_decoder: None,
            pending_refresh: false,

            menu: None,
            row_menu: None,
            database_menu: None,
            filter_menu: None,
            object_sheet: None,
            pending_object: None,
            structure_preview: None,
            pending_structure: None,

            inspector_tab: InspectorTab::Row,
            expanded_field: None,
            field_decoders: HashMap::new(),
            decoder_menu: None,
        }
    }

    /// The result set the grid is currently showing. The workspace does not
    /// own it — the grid does — but the inspector and the status bar both need
    /// to read it, and routing that through here keeps them from reaching into
    /// the grid's internals.
    pub fn result(&self, cx: &App) -> std::sync::Arc<db::ResultSet> {
        self.pane().grid.read(cx).data().clone()
    }

    /// Execute the console's SQL.
    ///
    /// Everything that runs a statement comes through here — the Run button,
    /// ⌘⏎, opening a table from the tree — so history, timing and error
    /// display have exactly one implementation.
    pub fn run(&mut self, sql: String, cx: &mut Context<Self>) {
        // A statement typed and run on its own abandons whatever a previous
        // ⌘⇧⏎ had queued: two scripts interleaving down one connection would
        // be nobody's idea of what those keys do.
        self.start_batch(self.active_pane);
        self.run_at(self.active_pane, sql, None, cx);
    }

    /// Forget the last run's answers, because a new one is starting.
    ///
    /// Every entry point that is not the queue advancing calls this: the kept
    /// results belong to one press of Run, and the statements still waiting
    /// belong to a script nobody asked for twice.
    fn start_batch(&mut self, target: PaneId) {
        if let Some(pane) = self.pane_by_mut(target) {
            pane.queue.clear();
            pane.results.clear();
            pane.result_index = 0;
        }
    }

    /// The one path to the server, for a named pane and a known origin.
    fn run_at(
        &mut self,
        target: PaneId,
        sql: String,
        origin: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.session.clone() else {
            if let Some(pane) = self.pane_by_mut(target) {
                pane.queue.clear();
                pane.error = Some(db::DbError::connection(
                    "Not connected. Choose a connection in the sidebar.",
                ));
            }
            cx.notify();
            return;
        };
        log::info!("run: {}", sql.replace('\n', " "));

        // Whose answer this is, remembered now rather than worked out when it
        // lands: by then the active pane may be a different one.
        self.running_pane = Some(target);
        let editor = match self.pane_by_mut(target) {
            Some(pane) => {
                // A new run, so the last one's squiggle goes.
                pane.run_origin = origin;
                pane.editor.clone()
            }
            None => return,
        };
        editor.update(cx, |editor, cx| {
            editor.clear_error(cx);
        });

        // Recorded before it is sent, so a statement that never comes back is
        // still in the history afterwards.
        let id = session.read(cx).config.id;
        self.pending_history = self
            .store
            .as_ref()
            .and_then(|store| store.record_query(Some(id), &sql, now_ms()).ok());

        session.update(cx, |session, cx| session.run(sql, cx));
        cx.notify();
    }

    /// Run what the console is pointing at: the selection if there is one, the
    /// statement under the cursor otherwise.
    ///
    /// Separate from [`Workspace::run`] because it is the only path that knows
    /// *where* the text came from, and a syntax error is a position within it.
    pub fn run_console(&mut self, cx: &mut Context<Self>) {
        let (sql, range) = self
            .pane()
            .editor
            .clone()
            .update(cx, |editor, _| editor.run_range());
        self.start_batch(self.active_pane);
        self.run_at(self.active_pane, sql, Some(range.start), cx);
    }

    /// Lay the pane's SQL out again. See [`editor::Editor::format`].
    pub fn format_query(&mut self, cx: &mut Context<Self>) {
        self.pane()
            .editor
            .clone()
            .update(cx, |editor, cx| editor.format(cx));
    }

    /// What the titlebar's bolt means on the tab that is open.
    ///
    /// A query tab has a statement to send. A browsed table does not: its rows
    /// came from a `select` the app wrote, and the only thing "run" can mean
    /// there is to ask for them again. A structure editor is looking at the
    /// catalog rather than at rows, so its "again" is a catalog read. The
    /// button reports whichever of those it would do, so it is never a glyph
    /// that turns out to be inert.
    pub(crate) fn run_action(&self, cx: &App) -> RunAction {
        if self.is_running(cx) {
            return RunAction::Cancel;
        }
        if !self.is_connected(cx) {
            return RunAction::Offline;
        }
        match self.pane().active() {
            Some(tab) if tab.kind == CenterKind::Structure => RunAction::Reload,
            Some(tab) if tab.relation.is_some() => RunAction::Reload,
            _ => RunAction::Run,
        }
    }

    /// Do it. The one handler behind the bolt, whatever the bolt currently is.
    pub fn run_primary(&mut self, cx: &mut Context<Self>) {
        match self.run_action(cx) {
            RunAction::Cancel => self.cancel(cx),
            RunAction::Reload => match self.pane().active().map(|tab| tab.kind) {
                Some(CenterKind::Structure) => self.refresh_schema(cx),
                _ => self.refresh_results(cx),
            },
            // Offline included: `run_at` turns a statement with no connection
            // into "Not connected. Choose a connection in the sidebar.", which
            // is more use than a button that ignores the press.
            RunAction::Run | RunAction::Offline => self.run_console(cx),
        }
    }

    /// Run every statement in the console — or in the selection, if there is
    /// one — in order, stopping at the first failure.
    ///
    /// They go one at a time rather than as one string: `query` prepares a
    /// single statement, and a script sent whole comes back as a syntax error
    /// at the first semicolon.
    pub fn run_console_all(&mut self, cx: &mut Context<Self>) {
        let statements = self
            .pane()
            .editor
            .clone()
            .update(cx, |editor, _| editor.run_all());
        let mut queued: std::collections::VecDeque<_> = statements.into();
        let Some((sql, origin)) = queued.pop_front() else {
            return;
        };
        self.start_batch(self.active_pane);
        self.pane_mut().queue = queued;
        self.run_at(self.active_pane, sql, Some(origin), cx);
    }

    /// Stop whatever is running, on the server as well as here.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.session.clone() {
            session.update(cx, |session, cx| session.cancel(cx));
        }
    }

    /// Which pane is waiting on the server, if any. The Run button only turns
    /// into Cancel in the pane that actually started something.
    pub(crate) fn running_pane(&self) -> Option<PaneId> {
        self.running_pane
    }

    /// Is there a statement in flight? The Run button becomes Cancel.
    pub fn is_running(&self, cx: &App) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.read(cx).activity() == Activity::Running)
    }

    /// Is there a live connection to send a statement down?
    pub fn is_connected(&self, cx: &App) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.read(cx).state() == &SessionState::Connected)
    }

    /// Re-read the History and Queries tabs from SQLite.
    ///
    /// Both lists are small and both are read at human speed, so this is a
    /// plain synchronous query rather than another trip through Tokio — the
    /// local file is not the thing that will ever be slow here.
    pub fn reload_lists(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let connection = self
            .session
            .as_ref()
            .map(|session| session.read(cx).config.id);

        match connection {
            Some(id) => match store.recent_queries_for(id, HISTORY_SHOWN) {
                Ok(rows) => self.history = rows,
                Err(error) => log::warn!("could not read the history: {error:#}"),
            },
            None => match store.recent_queries(HISTORY_SHOWN) {
                Ok(rows) => self.history = rows,
                Err(error) => log::warn!("could not read the history: {error:#}"),
            },
        }
        match store.saved_queries(connection) {
            Ok(rows) => self.saved = rows,
            Err(error) => log::warn!("could not read the saved queries: {error:#}"),
        }
        cx.notify();
    }

    /// Put a statement in the editor without running it. Clicking a history row
    /// deliberately does not re-run it: the reason people go looking through
    /// history is usually that the statement did something they regret.
    pub fn load_sql(&mut self, sql: &str, cx: &mut Context<Self>) {
        self.pane()
            .editor
            .update(cx, |editor, cx| editor.set_text(sql, cx));
        self.focus_editor(cx);
        cx.notify();
    }

    /// Ask for the console to have focus at the next paint. Everything that
    /// hands work back to the editor — closing the palette, loading a
    /// statement, opening a new tab — goes through here, because none of them
    /// run anywhere that has a `Window`.
    pub(crate) fn focus_editor(&mut self, cx: &App) {
        self.pending_focus = Some(self.pane().editor.read(cx).focus().clone());
    }

    /// ⌘S, and the toolbar's save button.
    ///
    /// A tab that already came from a saved query writes straight back to it —
    /// that is what makes editing one feel like editing a file. A tab that did
    /// not has to be named first, which is the sheet.
    pub fn save_query(&mut self, cx: &mut Context<Self>) {
        let sql = self.pane().editor.read(cx).text();
        if sql.trim().is_empty() {
            return;
        }
        match self.pane().active().and_then(|tab| tab.saved_query) {
            Some(id) => self.write_saved_query(id, None, sql, cx),
            None => self.prompt_for_name(&sql, cx),
        }
    }

    /// Save under a new name whatever the tab is currently editing.
    pub fn save_query_as(&mut self, cx: &mut Context<Self>) {
        let sql = self.pane().editor.read(cx).text();
        if sql.trim().is_empty() {
            return;
        }
        self.prompt_for_name(&sql, cx);
    }

    fn prompt_for_name(&mut self, sql: &str, cx: &mut Context<Self>) {
        let existing = self
            .pane()
            .active()
            .and_then(|tab| tab.saved_query)
            .and_then(|id| self.saved.iter().find(|query| query.id == id))
            .map(|query| query.name.clone());
        let suggested = existing.clone().unwrap_or_else(|| suggest_name(sql));
        let scope: SharedString = match self.session.as_ref() {
            Some(session) => format!("Filed under {}", session.read(cx).config.name).into(),
            // Saving with nothing connected is legitimate — `select version()`
            // is not about any one server — but it is worth saying so, because
            // the sidebar will then show it against every connection.
            None => "Not connected: available to every connection".into(),
        };
        let preview: SharedString = one_line(sql).into();
        let replacing = existing.is_some();

        let current = self.pane().active().and_then(|tab| tab.saved_query);
        let taken: Vec<String> = self
            .saved
            .iter()
            .filter(|query| Some(query.id) != current)
            .map(|query| query.name.clone())
            .collect();
        let sheet =
            cx.new(|cx| SaveQuerySheet::new(&suggested, preview, scope, replacing, taken, cx));
        cx.subscribe(&sheet, Self::on_save_sheet_event).detach();
        self.pending_focus = Some(sheet.focus_handle(cx));
        self.save_sheet = Some(sheet);
        cx.notify();
    }

    fn on_save_sheet_event(
        &mut self,
        _sheet: Entity<SaveQuerySheet>,
        event: &SaveSheetEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            SaveSheetEvent::Dismissed => {
                self.save_sheet = None;
                cx.notify();
            }
            SaveSheetEvent::Named(name) => {
                self.save_sheet = None;
                self.save_query_named(name.clone(), cx);
            }
        }
    }

    /// Save what the active tab is editing under `name`, replacing the saved
    /// query it came from if it came from one. The sheet calls this; so will
    /// the command palette, which is why it is not private to the sheet.
    pub fn save_query_named(&mut self, name: impl Into<String>, cx: &mut Context<Self>) {
        let sql = self.pane().editor.read(cx).text();
        if sql.trim().is_empty() {
            return;
        }
        let id = self
            .pane()
            .active()
            .and_then(|tab| tab.saved_query)
            .unwrap_or_else(uuid::Uuid::new_v4);
        self.write_saved_query(id, Some(name.into()), sql, cx);
    }

    /// The one place a saved query is written.
    ///
    /// `name` is `None` for a save over an existing query, which keeps whatever
    /// it is already called: ⌘S on a query you opened from the sidebar should
    /// not quietly rename it.
    fn write_saved_query(
        &mut self,
        id: uuid::Uuid,
        name: Option<String>,
        sql: String,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.store.clone() else {
            log::warn!("no local store: the query was not saved");
            return;
        };
        let name = name
            .or_else(|| {
                self.saved
                    .iter()
                    .find(|query| query.id == id)
                    .map(|query| query.name.clone())
            })
            .unwrap_or_else(|| suggest_name(&sql));

        let connection = self.session.as_ref().map(|s| s.read(cx).config.id);
        let id = id_for_save(&self.saved, &name, connection, id);

        let query = store::SavedQuery {
            id,
            // A query saved while connected belongs to that connection; one
            // saved with nothing open belongs to all of them.
            connection,
            name: name.clone(),
            sql,
            updated_at: now_ms(),
        };
        if let Err(error) = store.save_query(&query) {
            log::error!("could not save the query: {error:#}");
            return;
        }

        self.claim_query_tab(name, id, cx);
        // Saving is also how the Queries tab gets found: showing it is a
        // cheaper acknowledgement than a toast that has to be dismissed.
        self.sidebar_tab = SidebarTab::Queries;
        self.reload_lists(cx);
    }

    /// Open a saved query in the current tab, and remember which one it is so
    /// ⌘S goes back to the same row.
    pub fn load_saved_query(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(query) = self.saved.get(index).cloned() else {
            return;
        };
        // The tab first, then the text: claiming a tab may move off the one
        // that is showing, and the console's text belongs to whichever tab it
        // is leaving.
        self.claim_query_tab(query.name.clone(), query.id, cx);
        self.load_sql(&query.sql, cx);
        self.save_session(cx);
        cx.notify();
    }

    /// Point a centre tab at a saved query.
    ///
    /// The active tab if it is already a script; a new one if it is a table
    /// being browsed. Renaming someone's open table to "Monthly revenue" and
    /// leaving its rows underneath would be the worse of the two surprises.
    fn claim_query_tab(&mut self, name: String, id: uuid::Uuid, cx: &mut Context<Self>) {
        let index = match self.pane().active() {
            Some(tab) if tab.kind == CenterKind::Query => self.pane().active_tab,
            _ => {
                let session = self.session.clone();
                self.pane_mut().tabs.push(CenterTab {
                    kind: CenterKind::Query,
                    title: name.clone().into(),
                    detail: None,
                    dirty: false,
                    relation: None,
                    key: None,
                    saved_query: Some(id),
                    sql: String::new(),
                    filter: crate::filter::Filter::default(),
                    page: None,
                    structure: None,
                    session,
                    reconnect: None,
                });
                self.pane().tabs.len() - 1
            }
        };
        if index != self.pane().active_tab {
            self.show_tab(index, cx);
        }
        if let Some(tab) = self.pane_mut().tabs.get_mut(index) {
            tab.kind = CenterKind::Query;
            tab.title = name.into();
            tab.detail = None;
            tab.dirty = false;
            tab.relation = None;
            tab.saved_query = Some(id);
        }
    }

    pub fn delete_saved_query(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(query) = self.saved.get(index).cloned() else {
            return;
        };
        if let Some(store) = self.store.clone() {
            if let Err(error) = store.delete_saved_query(query.id) {
                log::error!("could not delete the saved query: {error:#}");
                return;
            }
        }
        // The tab that was editing it keeps its text and its name and becomes
        // an unsaved script again: deleting the row should not close a tab or
        // throw away what is in the editor.
        for tab in &mut self.pane_mut().tabs {
            if tab.saved_query == Some(query.id) {
                tab.saved_query = None;
                tab.dirty = true;
            }
        }
        self.reload_lists(cx);
    }

    // ---- tabs ------------------------------------------------------------

    /// Show a centre tab, carrying the console's text back to the tab that is
    /// leaving.
    ///
    /// One editor, one buffer per tab. The results dock is still shared — it
    /// belongs to the pane tree, which M2 has yet to grow — so switching tabs
    /// changes what you are typing and not yet what you are looking at.
    pub fn activate_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.pane().tabs.len() || index == self.pane().active_tab {
            return;
        }
        self.show_tab(index, cx);
        self.save_session(cx);
        cx.notify();
    }

    /// Swap the console over to a tab. Every path that changes which tab is
    /// active goes through here — including the ones that just pushed the tab
    /// they are about to show — because the text in the console belongs to the
    /// tab that is leaving, and there is exactly one moment to take it.
    pub(crate) fn show_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        self.stash_editor(cx);
        // What the last run reported belongs to the tab that ran it, so it
        // leaves with that tab. A syntax error from a query tab, left showing
        // in red under a table's rows — with a duration that timed a statement
        // about something else — describes nothing that is on screen.
        let editor = self.pane().editor.clone();
        editor.update(cx, |editor, cx| editor.clear_error(cx));
        let pane = self.pane_mut();
        pane.error = None;
        pane.elapsed = None;
        pane.affected = None;
        pane.last_sql = None;
        pane.truncated = false;
        pane.active_tab = index;
        // …and where it can be seen. A tab is activated from all over the app
        // — the tree, ⌘1, an F6 hop — and a strip too narrow for its tabs
        // scrolls, which means the one in front can be the one off the end of
        // it. `scroll_to_item` moves the least it can, so a strip that already
        // had the tab in view does not jump.
        pane.tab_scroll.scroll_to_item(index);
        self.adopt_tab(index, cx);
        let sql = self.pane().tabs[index].sql.clone();
        self.pane()
            .editor
            .update(cx, |editor, cx| editor.set_text(&sql, cx));
        // The filter is the tab's, so switching tabs swaps it the same way the
        // console text is swapped. The composer is not: a half-written chip
        // belongs to the moment, not to the tab.
        let clause = self.pane().tabs[index].filter.text.clone();
        let filter = self.pane().filter.clone();
        filter.update(cx, |filter, cx| filter.set_text(&clause, cx));
        self.pane_mut().composer = None;
        // A table tab *is* its rows, and the grid is the pane's, not the tab's:
        // whatever is in it belongs to whichever tab last ran something. Coming
        // back to a table therefore asks again rather than showing another
        // tab's answer under this one's name. A page costs milliseconds; being
        // wrong about which table is on screen costs more than that.
        let showing = self
            .pane()
            .tabs
            .get(index)
            .map(|tab| (tab.kind, tab.relation.clone(), tab.key.clone()));
        match showing {
            Some((CenterKind::Table, Some(relation), _)) => self.reload_relation(relation, cx),
            // The same rule for a key, which is a browse of something that can
            // change under you rather more often than a table can.
            Some((CenterKind::Key, _, Some((key, kind)))) => self.reload_key(key, kind, cx),
            // Nothing is going to refill the grid, so what is in it — and any
            // edit staged against it — belongs to the tab that just left. A
            // script's answers under another script's name, with a Commit
            // button offering to write them, is the confusion this avoids.
            _ => self.clear_results(cx),
        }
    }

    /// Put what is in the console back into the tab that owns it. Every path
    /// that changes which tab is active goes through here first, or the text
    /// ends up belonging to whichever tab happened to be showing.
    fn stash_editor(&mut self, cx: &App) {
        let text = self.pane().editor.read(cx).text();
        let filter = self.pane().filter.read(cx).text(cx);
        if let Some(tab) = self.pane_mut().active_mut() {
            tab.sql = text;
            // Only the hand-written half: in chip mode the box is not showing
            // and whatever is left in it is the last tab's clause.
            if tab.filter.raw {
                tab.filter.text = filter;
            }
        }
    }

    /// The same, for every pane. Quitting is about the whole window, and the
    /// half of a split you were not typing in has just as much unsaved text in
    /// it as the half you were.
    fn stash_editors(&mut self, cx: &App) {
        for index in 0..self.panes.len() {
            let text = self.panes[index].editor.read(cx).text();
            let filter = self.panes[index].filter.read(cx).text(cx);
            if let Some(tab) = self.panes[index].active_mut() {
                tab.sql = text;
                if tab.filter.raw {
                    tab.filter.text = filter;
                }
            }
        }
    }

    /// ⌘T, and the `+` at the end of the tab strip.
    pub fn new_query_tab(&mut self, cx: &mut Context<Self>) {
        let session = self.session.clone();
        self.pane_mut().tabs.push(CenterTab {
            kind: CenterKind::Query,
            title: "Untitled".into(),
            detail: None,
            dirty: false,
            relation: None,
            key: None,
            saved_query: None,
            sql: String::new(),
            filter: crate::filter::Filter::default(),
            page: None,
            structure: None,
            session,
            reconnect: None,
        });
        self.show_tab(self.pane().tabs.len() - 1, cx);
        self.focus_editor(cx);
        self.save_session(cx);
        cx.notify();
    }

    /// ⌘W, and the × on the tab.
    ///
    /// Every tab can be closed, including the last one. Closing the last tab of
    /// a *split* closes the split — an empty strip taking a third of the window
    /// from the pane still holding a query is not what "close" bought anyone.
    /// The last pane of all stays and goes empty: the window is still this
    /// connection's window, and the empty state says what fills it.
    pub fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.pane().tabs.len() {
            return;
        }
        if self.pane().tabs.len() == 1 && self.panes.len() > 1 {
            let id = self.active_pane;
            self.close_pane(id, cx);
            return;
        }
        let was_active = index == self.pane().active_tab;
        self.stash_editor(cx);
        self.pane_mut().tabs.remove(index);
        let (active, left) = (self.pane().active_tab, self.pane().tabs.len());
        self.pane_mut().active_tab = tab_after_close(active, index, left);
        if was_active {
            // Empty when that was the last tab: the console is per pane and
            // would otherwise still be holding the closed tab's script.
            let sql = self
                .pane()
                .active()
                .map(|tab| tab.sql.clone())
                .unwrap_or_default();
            self.pane()
                .editor
                .update(cx, |editor, cx| editor.set_text(&sql, cx));
        }
        // The rows belonged to the tab that is gone, and the grid is per pane.
        if left == 0 {
            self.clear_results(cx);
        }
        self.save_session(cx);
        cx.notify();
    }

    /// Write down what the window is showing, so a restart can put it back.
    ///
    /// Called whenever the tab list changes rather than on a timer: the only
    /// text that is not already in a tab is the console's, and stashing it
    /// first is what makes this a complete picture.
    pub(crate) fn save_session(&mut self, cx: &App) {
        // Turning the restore off means the app forgets, not that it keeps a
        // record it has promised not to read.
        if !self.settings.restore_session() {
            return;
        }
        self.stash_editors(cx);
        let connection = self.session.as_ref().map(|s| s.read(cx).config.id);
        // Not the connection's default database — the one actually open, which
        // is a different thing the moment anyone uses the switcher.
        let database = self
            .session
            .as_ref()
            .map(|s| s.read(cx).config.database.clone());
        let panes: Vec<_> = self
            .panes
            .iter()
            .map(|pane| crate::restore::PaneSnapshot {
                id: pane.id,
                tabs: &pane.tabs,
                active: pane.active_tab,
                // A live session if the tab has one, and otherwise whatever it
                // was restored with: a tab you have not clicked since launch
                // still knows where it belongs, and quitting again must not be
                // what forgets it.
                sources: pane
                    .tabs
                    .iter()
                    .map(|tab| {
                        tab.session
                            .as_ref()
                            .map(|session| {
                                let config = &session.read(cx).config;
                                (config.id, config.database.clone())
                            })
                            .or_else(|| tab.reconnect.clone())
                    })
                    .collect(),
            })
            .collect();
        crate::restore::State::from_workspace(
            connection,
            database,
            &panes,
            &self.layout,
            self.active_pane,
        )
        .save(self.store.as_deref());
    }

    // ---- the command palette ---------------------------------------------

    /// ⌘K, and ⌘P for the object list.
    ///
    /// `prefix` is the mode the palette opens in, written the way someone would
    /// type it, so that the two entry points are the same feature and not two.
    pub fn open_palette(&mut self, prefix: &str, cx: &mut Context<Self>) {
        let items = self.palette_items(cx);
        let palette = cx.new(|cx| Palette::new(items, prefix, cx));
        cx.subscribe(&palette, Self::on_palette_event).detach();
        self.pending_focus = Some(palette.focus_handle(cx));
        self.palette = Some(palette);
        cx.notify();
    }

    fn close_palette(&mut self, cx: &mut Context<Self>) {
        self.palette = None;
        self.focus_editor(cx);
        cx.notify();
    }

    /// Everything the palette can offer, in the order it offers it: the
    /// commands, then the two prefixes that are also things to do, then the
    /// tables and views this connection has, then the saved queries.
    fn palette_items(&self, cx: &App) -> Vec<PaletteItem> {
        let mut items: Vec<PaletteItem> = Command::ALL
            .iter()
            .copied()
            .map(PaletteItem::command)
            .collect();

        items.push(
            PaletteItem::new(
                ItemKind::Mode,
                PaletteAction::EnterMode(PaletteMode::Themes),
                "Change Theme…",
            )
            .icon(IconName::Sun)
            .shortcut("#"),
        );
        items.push(
            PaletteItem::new(
                ItemKind::Mode,
                PaletteAction::EnterMode(PaletteMode::Line),
                "Go to Line…",
            )
            .icon(IconName::Hashtag)
            .shortcut(":"),
        );

        for node in &self.tree {
            // Relations only. A keyspace has no fixed list of objects to search
            // — the tree holds whatever the last scan happened to reach — so a
            // palette full of keys would be a palette that answers differently
            // every time it is opened.
            let Some(target) = node
                .target
                .as_ref()
                .and_then(tree::Target::relation)
                .cloned()
            else {
                continue;
            };
            if !node.kind.is_relation() {
                continue;
            }
            let icon = match node.kind {
                tree::NodeKind::View => IconName::Eye,
                tree::NodeKind::MaterializedView => IconName::Layers,
                _ => IconName::Table,
            };
            let schema = target.schema.to_string();
            items.push(
                PaletteItem::new(
                    ItemKind::Object,
                    PaletteAction::Open(target),
                    node.name.clone(),
                )
                .icon(icon)
                .detail(schema),
            );
        }

        for query in &self.saved {
            items.push(
                PaletteItem::new(
                    ItemKind::Query,
                    PaletteAction::LoadQuery(query.id),
                    query.name.clone(),
                )
                .icon(IconName::Code)
                .detail("saved"),
            );
        }
        let _ = cx;
        items
    }

    fn on_palette_event(
        &mut self,
        _palette: Entity<Palette>,
        event: &PaletteEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            PaletteEvent::Dismissed => self.close_palette(cx),
            PaletteEvent::Preview(appearance) => self.apply_appearance(*appearance, cx),
            PaletteEvent::Chose(action) => {
                let action = action.clone();
                self.close_palette(cx);
                self.perform(action, cx);
            }
        }
    }

    /// Do what a palette row said to do.
    pub fn perform(&mut self, action: PaletteAction, cx: &mut Context<Self>) {
        match action {
            PaletteAction::Command(command) => self.run_command(command, cx),
            PaletteAction::Open(relation) => self.open_relation(&relation, cx),
            PaletteAction::LoadQuery(id) => {
                if let Some(index) = self.saved.iter().position(|query| query.id == id) {
                    self.load_saved_query(index, cx);
                }
            }
            PaletteAction::Theme(appearance) => self.set_appearance(appearance, cx),
            PaletteAction::GoToLine(line) => {
                self.pane()
                    .editor
                    .update(cx, |editor, cx| editor.go_to_line(line - 1, cx));
                self.focus_editor(cx);
                cx.notify();
            }
            // The palette handles this one itself and never emits it.
            PaletteAction::EnterMode(_) => {}
        }
    }

    /// Show an appearance without committing to it. What the palette's `#`
    /// mode does while you arrow through the list — it may yet be escaped out
    /// of, and a preview that wrote to disk would be a choice.
    ///
    /// The theme is built from the settings rather than from `Theme::of`, or
    /// flipping to light would quietly throw away the accent and the code size
    /// on the way past.
    fn apply_appearance(&mut self, appearance: ui::Appearance, cx: &mut Context<Self>) {
        if cx.theme().appearance == appearance {
            return;
        }
        ui::Theme::set_global(self.settings.theme(appearance, cx), cx);
        self.refresh_everything(cx);
    }

    /// Every window has to be told: nothing observes the theme global, so the
    /// colour each element cached is stale, and the Settings window is looking
    /// at the same theme this one is.
    fn refresh_everything(&mut self, cx: &mut Context<Self>) {
        self.pending_refresh = true;
        cx.refresh_windows();
        cx.notify();
    }

    /// The settings this window is running under. Read by the Settings window,
    /// which owns no state of its own — it is a view onto this.
    pub fn settings(&self) -> &crate::settings::Settings {
        &self.settings
    }

    /// Change a setting, put it into effect, and write it down. The one path:
    /// a knob that took effect without being saved would come back wrong, and
    /// one that saved without taking effect would look broken.
    pub fn update_settings(
        &mut self,
        change: impl FnOnce(&mut crate::settings::Settings),
        cx: &mut Context<Self>,
    ) {
        change(&mut self.settings);
        self.settings.save(self.store.as_deref());
        self.apply_settings(cx);
    }

    /// Put every setting into effect, whatever it is.
    ///
    /// Called after a change and once at launch, so that a preference file and
    /// a freshly clicked switch cannot possibly be interpreted differently.
    pub fn apply_settings(&mut self, cx: &mut Context<Self>) {
        let appearance = self.settings.appearance().unwrap_or(cx.theme().appearance);
        ui::Theme::set_global(self.settings.theme(appearance, cx), cx);

        // Every pane, not just the one in front: a setting that only took
        // effect in whichever half of a split you happened to be looking at
        // would be a bug someone would spend an afternoon on.
        let settings = self.settings.clone();
        for index in 0..self.panes.len() {
            let pane = &self.panes[index];
            dress_pane(pane, &settings, cx);
        }

        self.refresh_everything(cx);
    }

    /// Bring this window to the front. Does nothing before the first frame,
    /// which is the earliest moment there is a window to bring anywhere.
    pub fn raise(&self, cx: &mut App) {
        let Some(window) = self.window else { return };
        let _ = window.update(cx, |_, window, _| window.activate_window());
    }

    /// The Settings window, if it is open. For the screenshot harness, which
    /// has to capture the window rather than the app.
    pub fn settings_window(
        &self,
    ) -> Option<gpui::WindowHandle<crate::settings_window::SettingsWindow>> {
        self.settings_window
    }

    /// Open Settings, or bring it forward if it is already up. One window: a
    /// second copy of a preferences window can only disagree with the first.
    pub fn open_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(window) = self.settings_window {
            let raised = window
                .update(cx, |_, window, _| window.activate_window())
                .is_ok();
            if raised {
                return;
            }
            // The handle outlived the window — it was closed while we held it.
            self.settings_window = None;
        }
        // Deferred, and not for tidiness: opening a window renders its first
        // frame there and then, and the Settings window's first frame reads
        // this workspace — which is checked out for as long as this method is
        // running. The window has to be opened with nobody holding it.
        let workspace = cx.weak_entity();
        cx.defer(
            move |cx: &mut App| match crate::settings_window::open(workspace.clone(), cx) {
                Ok(window) => {
                    let _ = workspace
                        .update(cx, |workspace, _| workspace.settings_window = Some(window));
                }
                Err(error) => log::warn!("could not open the settings window: {error:#}"),
            },
        );
    }

    /// Pick an appearance and keep it. A chosen palette row and the Settings
    /// window both land here.
    fn set_appearance(&mut self, appearance: ui::Appearance, cx: &mut Context<Self>) {
        self.update_settings(|settings| settings.set_appearance(appearance), cx);
    }

    /// The one place a named command runs. The palette, the shortcuts and the
    /// toolbar all arrive here, so none of them can mean something different by
    /// the same word.
    pub fn run_command(&mut self, command: Command, cx: &mut Context<Self>) {
        match command {
            Command::Run => self.run_console(cx),
            Command::RunAll => self.run_console_all(cx),
            Command::Cancel => self.cancel(cx),
            Command::Save => self.save_query(cx),
            Command::SaveAs => self.save_query_as(cx),
            Command::ExportRows => self.open_export(cx),
            Command::ImportRows => self.open_import(cx),
            Command::CommitChanges => self.preview_commit(cx),
            Command::DiscardChanges => self.discard_changes(cx),
            Command::AddRow => self.add_row(cx),
            Command::DeleteRows => self.delete_rows(cx),
            Command::RevertRows => self.revert_rows(cx),
            Command::NewTab => self.new_query_tab(cx),
            Command::NewTable => self.new_table(cx),
            Command::CloseTab => self.close_tab(self.pane().active_tab, cx),
            Command::SplitRight => self.split_pane(Layout::Columns, cx),
            Command::SplitDown => self.split_pane(Layout::Rows, cx),
            Command::ClosePane => self.close_pane(self.active_pane, cx),
            Command::NewConnection => self.new_connection(cx),
            Command::RefreshResults => self.refresh_results(cx),
            Command::RefreshSchema => self.refresh_schema(cx),
            Command::FormatQuery => self.format_query(cx),
            Command::FollowReference => self.follow_reference(cx),
            Command::ToggleSidebar => self.toggle_left_panel(cx),
            Command::ToggleResults => self.toggle_bottom_dock(cx),
            Command::ToggleInspector => self.toggle_right_panel(cx),
            Command::ShowDatabaseTree => self.show_sidebar_tab(SidebarTab::Database, cx),
            Command::ShowSavedQueries => self.show_sidebar_tab(SidebarTab::Queries, cx),
            Command::ShowHistory => self.show_sidebar_tab(SidebarTab::History, cx),
            Command::ShowData => self.show_results_tab(ResultsTab::Data, cx),
            Command::ShowStructure => self.show_results_tab(ResultsTab::Structure, cx),
            Command::ShowDdl => self.show_results_tab(ResultsTab::Ddl, cx),
            Command::ShowMessages => self.show_results_tab(ResultsTab::Messages, cx),
            Command::OpenSettings => self.open_settings(cx),
        }
    }

    /// Showing a tab in a panel that is closed has to open the panel too, or
    /// the command appears to do nothing at all.
    pub(crate) fn show_sidebar_tab(&mut self, tab: SidebarTab, cx: &mut Context<Self>) {
        self.sidebar_tab = tab;
        self.left_open = true;
        cx.notify();
    }

    fn show_results_tab(&mut self, tab: ResultsTab, cx: &mut Context<Self>) {
        self.dock_open = true;
        self.select_results_tab(tab, cx);
    }

    /// Window-wide shortcuts.
    ///
    /// These are the gestures the editor and the grid deliberately let through:
    /// both `return` on an unhandled ⌘ key rather than swallowing it, which is
    /// what makes this the last stop rather than the first.
    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let m = k.modifiers;
        // Escape closes the context menu, which is the one surface in the app
        // with no button on it to close it with.
        if k.key == "escape" && self.menu.is_some() {
            self.close_object_menu(cx);
            cx.stop_propagation();
            return;
        }
        if !m.platform || m.control || m.alt {
            return;
        }

        // ⌘K closes the palette it opened. Everything else belongs to the
        // window behind the modal and waits until the modal is gone.
        if self.palette.is_some() {
            if k.key == "k" && !m.shift {
                self.close_palette(cx);
                cx.stop_propagation();
            }
            return;
        }
        if self.save_sheet.is_some()
            || self.export_sheet.is_some()
            || self.import_sheet.is_some()
            || self.object_sheet.is_some()
            || self.structure_preview.is_some()
        {
            return;
        }

        match k.key.as_str() {
            "k" if !m.shift => self.open_palette("", cx),
            // ⌘P goes straight to the objects, ⇧⌘P straight to the commands:
            // the two halves of the mixed list, for when you already know
            // which half you want.
            "p" if m.shift => self.open_palette(">", cx),
            "p" => self.open_palette("@", cx),
            "t" if !m.shift => self.run_command(Command::NewTab, cx),
            "w" if !m.shift => self.run_command(Command::CloseTab, cx),
            // ⌘D beside, ⇧⌘D below. The same key for both because they are
            // the same gesture asked in two directions, which is also how the
            // two buttons on the tab strip read.
            "d" if m.shift => self.run_command(Command::SplitDown, cx),
            "d" => self.run_command(Command::SplitRight, cx),
            "r" if m.shift => self.run_command(Command::RefreshSchema, cx),
            "r" => self.run_command(Command::RefreshResults, cx),
            "n" if !m.shift => self.run_command(Command::NewConnection, cx),
            "1" => self.run_command(Command::ToggleSidebar, cx),
            "2" => self.run_command(Command::ToggleResults, cx),
            "3" => self.run_command(Command::ToggleInspector, cx),
            "," => self.run_command(Command::OpenSettings, cx),
            // ⌘. is the platform's "stop that", and it has to work while the
            // window is busy — which is exactly when it is pressed.
            "." => self.run_command(Command::Cancel, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// Hang every menu action off an element. See [`crate::menu`] for why the
    /// menu bar exists at all and why these are unit structs.
    fn menu_actions(
        &self,
        el: gpui::Stateful<gpui::Div>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        use crate::menu as m;
        macro_rules! actions {
            ($el:expr, $($action:ty => $command:expr),* $(,)?) => {
                $el $(.on_action(cx.listener(|this, _: &$action, _, cx| {
                    this.run_menu_command($command, cx)
                })))*
            };
        }
        let el = actions!(
            el,
            m::NewTab => Command::NewTab,
            m::NewTable => Command::NewTable,
            m::NewConnection => Command::NewConnection,
            m::CloseTab => Command::CloseTab,
            m::Save => Command::Save,
            m::SaveAs => Command::SaveAs,
            m::ExportRows => Command::ExportRows,
            m::ImportRows => Command::ImportRows,
            m::Run => Command::Run,
            m::RunAll => Command::RunAll,
            m::Cancel => Command::Cancel,
            m::CommitChanges => Command::CommitChanges,
            m::DiscardChanges => Command::DiscardChanges,
            m::AddRow => Command::AddRow,
            m::DeleteRows => Command::DeleteRows,
            m::RevertRows => Command::RevertRows,
            m::RefreshResults => Command::RefreshResults,
            m::RefreshSchema => Command::RefreshSchema,
            m::FormatQuery => Command::FormatQuery,
            m::FollowReference => Command::FollowReference,
            m::SplitRight => Command::SplitRight,
            m::SplitDown => Command::SplitDown,
            m::ClosePane => Command::ClosePane,
            m::ToggleSidebar => Command::ToggleSidebar,
            m::ToggleResults => Command::ToggleResults,
            m::ToggleInspector => Command::ToggleInspector,
            m::ShowData => Command::ShowData,
            m::ShowStructure => Command::ShowStructure,
            m::ShowDdl => Command::ShowDdl,
            m::ShowMessages => Command::ShowMessages,
            m::ShowDatabaseTree => Command::ShowDatabaseTree,
            m::ShowSavedQueries => Command::ShowSavedQueries,
            m::ShowHistory => Command::ShowHistory,
            m::OpenSettings => Command::OpenSettings,
        );
        // The three palette openers are not commands — they put a prefix in
        // the box rather than doing anything — so they are wired by hand.
        el.on_action(cx.listener(|this, _: &m::OpenPalette, _, cx| {
            if this.accepts_commands() {
                this.open_palette("", cx)
            }
        }))
        .on_action(cx.listener(|this, _: &m::OpenObjects, _, cx| {
            if this.accepts_commands() {
                this.open_palette("@", cx)
            }
        }))
        .on_action(cx.listener(|this, _: &m::OpenCommands, _, cx| {
            if this.accepts_commands() {
                this.open_palette(">", cx)
            }
        }))
        .on_action(cx.listener(|_, _: &m::Minimize, window, _| window.minimize_window()))
        .on_action(cx.listener(|_, _: &m::Zoom, window, _| window.zoom_window()))
        .on_action(cx.listener(|_, _: &m::About, _, cx| {
            log::info!("tupli {}", env!("CARGO_PKG_VERSION"));
            cx.propagate();
        }))
    }

    /// A command asked for from the menu bar or a keystroke rather than from
    /// the palette. The guard is the same one [`Self::on_key`] applies: a modal
    /// is a question, and answering it by opening a tab behind it is not an
    /// answer.
    fn accepts_commands(&self) -> bool {
        self.palette.is_none()
            && self.save_sheet.is_none()
            && self.export_sheet.is_none()
            && self.import_sheet.is_none()
            && self.object_sheet.is_none()
            && self.structure_preview.is_none()
    }

    fn run_menu_command(&mut self, command: Command, cx: &mut Context<Self>) {
        if self.accepts_commands() {
            self.run_command(command, cx);
        }
    }

    /// Where a statement typed right now would land: the current database and
    /// the server's `current_schema()`. Em dashes when there is nothing
    /// connected, because inventing `public` would be a guess.
    pub fn current_location(&self, cx: &App) -> (SharedString, SharedString) {
        match self
            .session
            .as_ref()
            .and_then(|session| session.read(cx).snapshot.clone())
        {
            Some(snapshot) => (
                snapshot.database.to_string().into(),
                // `current_schema()`, not `search_path[0]`: the path's first
                // entry is the implicit `pg_catalog` on every ordinary session,
                // and a breadcrumb reading `pg_catalog` would be worse than no
                // breadcrumb at all.
                snapshot.current_schema.to_string().into(),
            ),
            None => ("—".into(), "—".into()),
        }
    }

    // ---- connections -----------------------------------------------------

    /// Make the tab at `index` in the active pane the one the window is
    /// describing, opening its connection if this is the first time it has
    /// been looked at since launch.
    fn adopt_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        // The tab's connection becomes the window's: its catalog in the
        // sidebar, its name in the titlebar, its server on the other end of
        // the Run button. A tab that has none — restored from the last run, or
        // made before anything was connected — takes the one the window is
        // already on, which is what makes ⌘T open a tab against what you are
        // looking at rather than against nothing.
        let restored = self
            .pane()
            .tabs
            .get(index)
            .filter(|tab| tab.session.is_none())
            .and_then(|tab| tab.reconnect.clone());
        match self
            .pane()
            .tabs
            .get(index)
            .and_then(|tab| tab.session.clone())
        {
            Some(session) => self.adopt_session(Some(session), cx),
            // Restored and shown for the first time: this is the moment its
            // connection is worth opening, and the moment the user is there to
            // watch it happen.
            None if restored.is_some() => {
                let (id, database) = restored.expect("checked");
                if let Some(tab) = self.pane_mut().tabs.get_mut(index) {
                    tab.reconnect = None;
                }
                match self.connections.iter().find(|c| c.id == id).cloned() {
                    Some(mut config) => {
                        config.database = database;
                        let session = self.session_for(config, None, cx);
                        if let Some(tab) = self.pane_mut().tabs.get_mut(index) {
                            tab.session = Some(session.clone());
                        }
                        self.adopt_session(Some(session), cx);
                    }
                    // The connection was deleted while the tab was away. The
                    // tab keeps its text and joins the window wherever it is.
                    None => {
                        let current = self.session.clone();
                        if let Some(tab) = self.pane_mut().tabs.get_mut(index) {
                            tab.session = current;
                        }
                    }
                }
            }
            None => {
                let current = self.session.clone();
                if let Some(tab) = self.pane_mut().tabs.get_mut(index) {
                    tab.session = current;
                }
            }
        }
    }

    /// Which database a tab is looking at, by name.
    ///
    /// A live session if it has one, and otherwise the name it was restored
    /// with — a tab you have not clicked since launch still knows where it
    /// belongs, and the strip has to be able to say so before the connection
    /// is opened.
    pub(crate) fn tab_database(&self, tab: &CenterTab, cx: &App) -> Option<SharedString> {
        match &tab.session {
            Some(session) => Some(session.read(cx).config.database.clone().into()),
            None => tab.reconnect.as_ref().map(|(_, db)| db.clone().into()),
        }
    }

    /// Point this window at a connection and open it.
    pub fn open_connection(&mut self, config: db::ConnectionConfig, cx: &mut Context<Self>) {
        self.open_connection_with(config, None, cx)
    }

    /// Connect, with the password already in hand.
    ///
    /// Saving a connection is the one moment the app knows the secret without
    /// asking for it, and asking anyway — writing it to the Keychain and
    /// reading it straight back on another thread — is how a connection that
    /// was saved correctly comes up "password missing".
    pub fn open_connection_with(
        &mut self,
        config: db::ConnectionConfig,
        password: Option<String>,
        cx: &mut Context<Self>,
    ) {
        // A different server is a different place, so it gets a tab of its
        // own — the same rule `open_database` already follows one level down.
        // Repointing the tab in front left a Query tab's rows sitting under a
        // name that never produced them, and took the connection you had been
        // reading off the screen entirely.
        let here = self.pane().active().and_then(|tab| tab.session.clone());
        if here.is_some_and(|session| session.read(cx).config.id != config.id) {
            self.new_query_tab(cx);
            self.clear_results(cx);
        }
        let session = self.session_for(config, password, cx);
        self.bind_active_tab(session, cx);
        self.reload_lists(cx);
        self.save_session(cx);
        cx.notify();
    }

    /// The window's session for one database, connecting it if this is the
    /// first tab to ask.
    ///
    /// Two tabs on the same database share one connection: they are the same
    /// conversation with the same server, and opening a second socket would
    /// only mean two catalogs that can disagree. Two tabs on *different*
    /// databases get one each, which is the whole point.
    fn session_for(
        &mut self,
        config: db::ConnectionConfig,
        password: Option<String>,
        cx: &mut Context<Self>,
    ) -> Entity<Session> {
        let open = self.sessions.iter().find(|session| {
            let held = &session.read(cx).config;
            held.id == config.id && held.database == config.database
        });
        if let Some(session) = open {
            return session.clone();
        }
        let session = cx.new(|_| Session::with_password(config, password));
        cx.subscribe(&session, Self::on_session_event).detach();
        session.update(cx, |session, cx| session.connect(cx));
        self.sessions.push(session.clone());
        session
    }

    /// Point the active tab at a connection, and the window with it.
    fn bind_active_tab(&mut self, session: Entity<Session>, cx: &mut Context<Self>) {
        if let Some(tab) = self.pane_mut().active_mut() {
            tab.session = Some(session.clone());
        }
        self.adopt_session(Some(session), cx);
    }

    /// Make a connection the one the window is describing: the tree, the
    /// completions, the titlebar, the status bar.
    ///
    /// Called when a tab is activated and when a tab is pointed somewhere new.
    /// The tree is rebuilt from the snapshot the session already has, so
    /// coming back to a tab that has been open for an hour costs a repaint —
    /// and a tab still connecting shows what it has, which is nothing yet.
    fn adopt_session(&mut self, session: Option<Entity<Session>>, cx: &mut Context<Self>) {
        let same = match (&self.session, &session) {
            (Some(open), Some(next)) => open == next,
            (None, None) => true,
            _ => false,
        };
        self.session = session;
        if same {
            return;
        }
        log::debug!(
            "the window is now describing {}",
            self.session
                .as_ref()
                .map(|s| s.read(cx).config.database.clone())
                .unwrap_or_else(|| "nothing".into())
        );
        let described = self.session.as_ref().map(|session| {
            let session = session.read(cx);
            (session.config.display_name(), session.snapshot.clone())
        });
        let snapshot = match described {
            Some((_, snapshot)) => snapshot,
            None => None,
        };
        self.rebuild_tree(cx);
        self.collapsed = tree::initially_collapsed(&self.tree).into_iter().collect();
        self.selected_node = None;
        self.catalog.set(snapshot);
        cx.notify();
    }

    /// What the connection in front can do.
    ///
    /// The one question the UI is allowed to ask about an engine. Everything
    /// that would otherwise be spelled `is this Redis?` is spelled as a
    /// capability instead, so a third engine is a row in a table rather than a
    /// branch in every file. A window with nothing open answers as a SQL
    /// server, which is what its empty console is.
    pub(crate) fn capabilities(&self, cx: &App) -> db::Capabilities {
        match self.session.as_ref() {
            Some(session) => session.read(cx).config.capabilities(),
            None => db::Capabilities::POSTGRES,
        }
    }

    /// Open the key `TUPLI_KEY` named, once the walk has turned it up.
    fn open_pending_key(&mut self, session: &Entity<Session>, cx: &mut Context<Self>) {
        let Some(wanted) = self.pending_key.clone() else {
            return;
        };
        let found = session
            .read(cx)
            .keys
            .iter()
            .find(|info| &*info.key == wanted.as_bytes())
            .map(|info| (info.key.clone(), info.kind.clone()));
        match found {
            Some((key, kind)) => {
                self.pending_key = None;
                self.open_key(key, kind, cx);
            }
            // Still walking. A pattern the whole keyspace does not contain
            // simply never opens, and the warning is the log's job.
            None if session.read(cx).keys_complete => {
                log::warn!("TUPLI_KEY={wanted:?} is not in this database");
                self.pending_key = None;
            }
            None => {}
        }
    }

    /// Whether the key browser has all of the keyspace it is going to get.
    ///
    /// A keyspace arrives in two stages — the databases, then the keys — and
    /// anything waiting for "the tree is drawn" has to wait for the second
    /// one. Always true on a server with a schema, which has no second stage.
    pub fn keys_settled(&self, cx: &App) -> bool {
        let Some(session) = self.session.as_ref().map(|session| session.read(cx)) else {
            return true;
        };
        if session.keyspace.is_none() {
            return true;
        }
        let walked = session.keys_complete || session.keys.len() >= KEY_BROWSER_LIMIT;
        // And the key that was asked for is not only found but read: its rows
        // arrive a round trip after the walk that named it.
        let read = match self.pane().active().and_then(|tab| tab.key.clone()) {
            Some((key, _)) => session
                .last_key
                .as_ref()
                .is_some_and(|view| view.key == key),
            None => true,
        };
        walked && self.pending_key.is_none() && read
    }

    /// Carry the keyspace walk on, up to [`KEY_BROWSER_LIMIT`].
    ///
    /// A page at a time and always one more page than has landed, so the tree
    /// fills in while it is being looked at rather than after a pause nobody
    /// asked for. It stops at a limit because a browser is not a dump: past a
    /// few thousand rows the tree has stopped being something a person reads
    /// and the answer is a pattern, not more scrolling.
    fn scan_more_keys(&mut self, session: &Entity<Session>, cx: &mut Context<Self>) {
        let wanted = {
            let session = session.read(cx);
            session.keyspace.is_some()
                && !session.keys_complete
                && session.keys.len() < KEY_BROWSER_LIMIT
        };
        if wanted {
            session.update(cx, |session, cx| session.scan_keys("*", cx));
        }
    }

    /// Redraw the sidebar tree from whatever the window's session is holding.
    ///
    /// There are two kinds of catalog — a schema and a keyspace — and this is
    /// the only place that knows it. Everything that needs a tree asks for one
    /// rather than deciding which builder to call, so adding a third kind of
    /// server is a third arm here and no branches anywhere else.
    fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.tree = Vec::new();
            return;
        };
        let session = session.read(cx);
        let name = session.config.display_name();
        let capabilities = session.config.capabilities();
        self.tree = match (&session.snapshot, &session.keyspace) {
            (Some(snapshot), _) => {
                tree::from_snapshot(&name, snapshot, session.roles.as_deref(), capabilities)
            }
            (None, Some(keyspace)) => tree::from_keyspace(
                &name,
                &session.server_version(),
                keyspace,
                &session.keys,
                session.keys_complete,
            ),
            (None, None) => Vec::new(),
        };
    }

    /// The titlebar's switcher: every database on this server, the open one
    /// marked, and nothing else. Connections live in the sidebar and in
    /// Settings; putting them here too would make the title mean two things.
    pub(crate) fn open_database_menu(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.database_menu = Some(at);
        cx.notify();
    }

    pub(crate) fn close_database_menu(&mut self, cx: &mut Context<Self>) {
        if self.database_menu.take().is_some() {
            cx.notify();
        }
    }

    // ---- the chip composer's two menus -----------------------------------

    pub(crate) fn open_filter_menu(
        &mut self,
        which: FilterMenu,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.filter_menu = Some((at, which));
        cx.notify();
    }

    pub(crate) fn close_filter_menu(&mut self, cx: &mut Context<Self>) {
        if self.filter_menu.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn render_filter_menu(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (at, which) = self.filter_menu?;
        let chosen = self.pane().composer.as_ref()?.chip.clone();
        let mut menu = ui::ContextMenu::new("filter-menu")
            .at(at)
            .width(px(200.))
            .on_dismiss(cx.listener(|this, _, _, cx| this.close_filter_menu(cx)));
        match which {
            FilterMenu::Column => {
                // The columns of the rows on screen, in the order they are in
                // — which is the table's own order, and the order the person
                // choosing has just been reading.
                let columns: Vec<String> = self
                    .pane()
                    .grid
                    .read(cx)
                    .data()
                    .columns
                    .iter()
                    .map(|column| column.meta.name.to_string())
                    .collect();
                for name in columns {
                    let open = name == chosen.column;
                    let chosen_name = name.clone();
                    menu = menu.item(
                        ui::MenuItem::new(name)
                            .icon(match open {
                                true => IconName::Check,
                                false => IconName::Columns,
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_filter_menu(cx);
                                this.set_chip_column(chosen_name.clone(), cx);
                            })),
                    );
                }
            }
            FilterMenu::Op => {
                for op in crate::filter::Op::ALL {
                    let open = op == chosen.op;
                    menu = menu.item(
                        ui::MenuItem::new(op.symbol())
                            .icon(match open {
                                true => IconName::Check,
                                false => IconName::Filter,
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.close_filter_menu(cx);
                                this.set_chip_op(op, cx);
                            })),
                    );
                }
            }
        }
        Some(menu)
    }

    pub(crate) fn render_database_menu(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let at = self.database_menu?;
        let snapshot = self.snapshot(cx)?;
        let current = snapshot.database.clone();
        let mut menu = ui::ContextMenu::new("database-menu")
            .at(at)
            .width(px(220.))
            .on_dismiss(cx.listener(|this, _, _, cx| this.close_database_menu(cx)));
        for database in snapshot.databases.iter() {
            let name = database.to_string();
            let open = *database == current;
            menu = menu.item(
                ui::MenuItem::new(name.clone())
                    // A tick on the one you are in, rather than hiding it: a
                    // list that silently omits where you are is a list you
                    // have to count to read.
                    .icon(if open {
                        IconName::Check
                    } else {
                        IconName::Database
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_database_menu(cx);
                        this.switch_database(&name, cx);
                    })),
            );
        }
        Some(menu)
    }

    /// Open a different database on the server this window is already on.
    ///
    /// A Postgres session belongs to one database for its whole life, so this
    /// is a reconnect and not a `use` — same host, same role, same Keychain
    /// item, new session. Tabs are left alone: a query written against one
    /// database is usually the same query you want against the next one, and
    /// the rows already fetched stay on screen until something asks for them
    /// again, which is what every other client does too.
    /// Another database on the same server, in a tab of its own.
    ///
    /// Not by moving the tab that is already open. That tab is browsing a table
    /// the other database has never heard of, and what dragging it across
    /// bought was a notice saying so, sitting over the rows it used to have. A
    /// database is a place; two places want two tabs, and the sessions are
    /// already per database.
    pub fn open_database(&mut self, database: &str, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let mut config = session.read(cx).config.clone();
        if config.database == database {
            return;
        }
        config.database = database.to_string();
        self.new_query_tab(cx);
        self.clear_results(cx);
        let session = self.session_for(config, None, cx);
        self.bind_active_tab(session, cx);
        self.save_session(cx);
        cx.notify();
    }

    pub fn switch_database(&mut self, database: &str, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let mut config = session.read(cx).config.clone();
        if config.database == database {
            return;
        }
        config.database = database.to_string();
        // The *tab* moves to the other database. Everything else in the window
        // stays where it was: a query you left running against `analytics` in
        // the next tab is still running against `analytics`, and the tab you
        // are in is the only thing that changed its mind.
        // The rows on screen came out of the database being left, so they go
        // now rather than sitting under the new one's name until something
        // replaces them. A browsing tab asks the new server for the same table
        // as soon as it has a catalog to be asked about; if it does not have
        // that table, the notice says so.
        let browsing = self
            .pane()
            .active()
            .filter(|tab| tab.kind == CenterKind::Table)
            .and_then(|tab| tab.relation.clone());
        if browsing.is_some() {
            self.clear_results(cx);
        }
        self.pending_rebrowse = browsing;
        let session = self.session_for(config, None, cx);
        self.bind_active_tab(session, cx);
        self.save_session(cx);
        cx.notify();
    }

    /// Open whatever a tree row points at.
    ///
    /// The one place that knows both kinds of target, so that everything which
    /// merely *has* one — a click, the palette, a restored session — can hand
    /// it over without asking which engine it came from.
    pub fn open_target(&mut self, target: &tree::Target, cx: &mut Context<Self>) {
        match target {
            tree::Target::Relation(relation) => self.open_relation(relation, cx),
            tree::Target::Key(key, kind) => self.open_key(key.clone(), kind.clone(), cx),
        }
    }

    /// Open a table for browsing: a new centre tab and the first page of rows.
    ///
    /// The statement is a plain `select * … limit n` rather than a cursor
    /// because the grid holds the whole result set in memory anyway; paging is
    /// M6's problem, and the limit is what keeps this honest until then.
    pub fn open_relation(&mut self, relation: &db::RelationRef, cx: &mut Context<Self>) {
        self.open_relation_filtered(relation, None, cx)
    }

    /// Browse `relation`, optionally replacing whatever filter its tab carries.
    ///
    /// The filter goes on *before* the tab is shown, because showing it is what
    /// sends the statement and a second one issued behind the first would be
    /// dropped — see [`crate::session::Session::run`].
    pub(crate) fn open_relation_filtered(
        &mut self,
        relation: &db::RelationRef,
        filter: Option<crate::filter::Filter>,
        cx: &mut Context<Self>,
    ) {
        // A table tab whose rows are hidden is a tab about nothing, so browsing
        // opens the dock the way running a statement does. It stays a switch
        // afterwards: closing it on a table tab hands the height to the console.
        self.dock_open = true;
        let title: SharedString = relation.name.to_string().into();
        let detail: SharedString = relation.schema.to_string().into();

        // Reuse the tab if this table is already open; otherwise add one.
        match self.pane().tabs.iter().position(|tab| {
            tab.kind == CenterKind::Table
                && tab.title == title
                && tab.detail.as_ref() == Some(&detail)
        }) {
            Some(index) => {
                // The seeded tabs are named after a table without knowing one:
                // browsing to it is the moment the name becomes a reference,
                // and the Structure tab needs the reference, not the name.
                self.pane_mut().tabs[index].relation = Some(relation.clone());
                // The filter stays unless the caller brought one. It is this
                // tab's, it was written about this table, and coming back to a
                // browse you had narrowed down should show you what you
                // narrowed it to.
                if let Some(filter) = filter {
                    self.pane_mut().tabs[index].filter = filter;
                    self.pane_mut().tabs[index].page = None;
                }
                self.show_tab(index, cx);
            }
            None => {
                let session = self.session.clone();
                self.pane_mut().tabs.push(CenterTab {
                    kind: CenterKind::Table,
                    title,
                    detail: Some(detail),
                    dirty: false,
                    relation: Some(relation.clone()),
                    key: None,
                    saved_query: None,
                    sql: String::new(),
                    filter: filter.unwrap_or_default(),
                    page: None,
                    structure: None,
                    session,
                    reconnect: None,
                });
                self.show_tab(self.pane().tabs.len() - 1, cx);
            }
        }
        self.save_session(cx);
    }

    /// Open a key for browsing: a tab named after it, and its contents.
    ///
    /// The tab is found by the key rather than by its title, because the tree
    /// shows a key by its last `:` segment and two prefixes can end in the same
    /// word. A table can be found by name; a key is its bytes.
    pub fn open_key(&mut self, key: Arc<[u8]>, kind: db::KeyType, cx: &mut Context<Self>) {
        self.dock_open = true;
        let title: SharedString = db::key_text(&key).into();
        let detail: SharedString = kind.label().to_string().into();

        match self
            .pane()
            .tabs
            .iter()
            .position(|tab| tab.key.as_ref().is_some_and(|(open, _)| *open == key))
        {
            Some(index) => {
                // The type is taken from the listing again rather than kept:
                // `DEL` and `SET` on the same name make a key that used to be a
                // list into a string, and reading it as what it was would be an
                // error the server is right to give.
                self.pane_mut().tabs[index].key = Some((key, kind));
                self.show_tab(index, cx);
            }
            None => {
                let session = self.session.clone();
                self.pane_mut().tabs.push(CenterTab {
                    kind: CenterKind::Key,
                    title,
                    detail: Some(detail),
                    dirty: false,
                    relation: None,
                    key: Some((key, kind)),
                    saved_query: None,
                    sql: String::new(),
                    filter: crate::filter::Filter::default(),
                    page: None,
                    structure: None,
                    session,
                    reconnect: None,
                });
                self.show_tab(self.pane().tabs.len() - 1, cx);
            }
        }
        self.save_session(cx);
    }

    /// Ask for a key's contents again — on opening its tab, and on coming back
    /// to it. Not [`Workspace::run`]: there is no statement, so there is
    /// nothing to put in the console, nothing to log, and nothing to time.
    fn reload_key(&mut self, key: Arc<[u8]>, kind: db::KeyType, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.running_pane = Some(self.active_pane);
        session.update(cx, |session, cx| session.open_key(key, kind, cx));
    }

    /// A table tab with nothing behind it: named, pointed at a relation, never
    /// run. Only [`Workspace::boot_from_environment`]'s `TUPLI_OPEN` list makes
    /// these — every other way to a table browses it, because a tab whose rows
    /// have never been asked for is not a thing a person can make on purpose.
    fn seed_tab(&mut self, relation: &db::RelationRef) {
        let title: SharedString = relation.name.to_string().into();
        let detail: SharedString = relation.schema.to_string().into();
        if self.pane().tabs.iter().any(|tab| {
            tab.kind == CenterKind::Table
                && tab.title == title
                && tab.detail.as_ref() == Some(&detail)
        }) {
            return;
        }
        let session = self.session.clone();
        self.pane_mut().tabs.push(CenterTab {
            kind: CenterKind::Table,
            title,
            detail: Some(detail),
            dirty: false,
            relation: Some(relation.clone()),
            key: None,
            saved_query: None,
            sql: String::new(),
            filter: crate::filter::Filter::default(),
            page: None,
            structure: None,
            session,
            reconnect: None,
        });
    }

    /// Empty the pane's grid and everything that describes what was in it.
    ///
    /// For the tabs that have no rows to show and are not going to get any:
    /// the grid is per pane, so leaving it alone means showing one tab's
    /// answer under another tab's name.
    fn clear_results(&mut self, cx: &mut Context<Self>) {
        self.start_batch(self.active_pane);
        let pane = self.pane_mut();
        pane.row_count = 0;
        pane.selected_row = None;
        pane.selected_column = 0;
        pane.elapsed = None;
        pane.affected = None;
        pane.truncated = false;
        pane.last_sql = None;
        pane.error = None;
        pane.unsorted = None;
        let grid = pane.grid.clone();
        grid.update(cx, |grid, cx| {
            grid.set_data_arc(std::sync::Arc::new(db::ResultSet::new(Vec::new())), cx)
        });
        let id = self.active_pane;
        self.refresh_editability(id, cx);
        cx.notify();
    }

    /// Re-ask the server for a browsed table, applying whatever is in the
    /// filter box. Used by the filter, the refresh button, and by opening the
    /// table in the first place.
    fn reload_relation(&mut self, relation: db::RelationRef, cx: &mut Context<Self>) {
        if self.session.is_none() {
            return;
        }
        // A tab can outlive the connection it was opened against — restored
        // from the last run, or left behind when the window was pointed at
        // another server. Asking anyway earns a red
        // `relation "..." does not exist`, which reads as something having
        // just gone wrong; the catalog already knows better, and
        // `render_data_tab` says so calmly instead. Only when there *is* a
        // catalog: before the first read, not knowing is not the same as no.
        let absent = self
            .snapshot(cx)
            .filter(|snapshot| snapshot.relation(&relation).is_none())
            .map(|snapshot| snapshot.database.clone());
        if let Some(database) = absent {
            log::info!("{relation} is not in {database}; not browsing it");
            // And the grid is emptied, because the grid belongs to the pane
            // and still holds whatever the last tab ran. Twenty rows of
            // another table under this tab's name, with a notice above them
            // saying the table does not exist, is the worst of both answers.
            self.clear_results(cx);
            return;
        }
        if let Some(page) = self.pending_page.take() {
            self.set_page(page);
        }
        self.stash_editor(cx);
        let predicate = self
            .pane()
            .active()
            .map(|tab| tab.filter.predicate())
            .unwrap_or_default();
        let predicate = predicate.trim();
        let size = self.settings.page_size();
        let page = self.pane().active().and_then(|tab| tab.page).unwrap_or(0);
        // A table that fits in one page is shown in whatever order the server
        // hands it over in, which is what `select *` means and what the
        // planner is fastest at. The moment there is a second page that stops
        // being an implementation detail: `offset` over an unordered set can
        // hand out the same row twice and never hand out another, so from the
        // first full page onwards the statement carries an order it can page
        // against.
        let paged = self.pane().active().is_some_and(|tab| tab.page.is_some())
            || self.pane().row_count >= size;
        let order = match (self.order_by_clause(cx), paged) {
            (order, false) => order,
            (order, true) => format!("{order}{}", self.tiebreak(&relation, !order.is_empty(), cx)),
        };
        let sql = format!(
            "select * from {}.{}{}{} limit {}{}",
            db::schema::quote_ident(&relation.schema),
            db::schema::quote_ident(&relation.name),
            // The text goes in as written. Anything else would mean parsing
            // SQL to re-emit it, and the person typing here is already talking
            // to their own database in its own language.
            if predicate.is_empty() {
                String::new()
            } else {
                format!(" where {predicate}")
            },
            order,
            size,
            // Page one has no `offset 0` on it. It would be the same statement
            // and it would put a clause in the message log that says nothing.
            match page {
                0 => String::new(),
                page => format!(" offset {}", page * size),
            }
        );
        self.run(sql, cx);
    }

    /// The `order by` for the grid's current sort, or nothing.
    ///
    /// The column goes in quoted by name rather than by ordinal, because
    /// `order by 3` would silently follow the wrong column the moment the
    /// select list is anything but `*`. `nulls last` in both directions matches
    /// what the in-memory sort does, so the two paths cannot disagree about
    /// where the empty rows went.
    fn order_by_clause(&self, cx: &App) -> String {
        let Some(sort) = self.pane().grid.read(cx).sort() else {
            return String::new();
        };
        let data = self.pane().grid.read(cx).data().clone();
        let Some(column) = data.columns.get(sort.col) else {
            return String::new();
        };
        format!(
            " order by {} {} nulls last",
            db::schema::quote_ident(&column.meta.name),
            if sort.descending { "desc" } else { "asc" }
        )
    }

    /// F6: open the table the cell under the cursor points at, showing the one
    /// row it points at.
    ///
    /// A foreign key is the one relationship a database states outright, and
    /// following it by hand means reading a uuid off the screen, finding the
    /// other table in the tree, and typing the uuid back in. The reference is
    /// already written down; this reads it.
    pub fn follow_reference(&mut self, cx: &mut Context<Self>) {
        let column = self.pane().selected_column;
        self.follow_reference_in(column, cx);
    }

    /// The same hop, from a field named by the row inspector rather than by the
    /// grid's cursor.
    pub(crate) fn follow_reference_in(&mut self, column: usize, cx: &mut Context<Self>) {
        let Some((target, predicate)) = self.reference_in_column(column, cx) else {
            return;
        };
        let filter = crate::filter::Filter {
            // The hand-written box rather than a chip: the chip row edits
            // values as text and a hop is an exact match on a key, which is
            // not a thing to half-edit into something that matches two rows.
            raw: true,
            text: predicate,
            chips: Vec::new(),
        };
        self.open_relation_filtered(&target, Some(filter), cx);
    }

    /// The table the selected cell points at, when it points at one.
    ///
    /// The inspector puts the name on a button with it: a foreign key is a fact
    /// about the column that nothing else in the window says out loud, and a
    /// keystroke nobody knows about is not a feature. `None` is the answer for
    /// every cell that is not a key with a value in it, which is most of them.
    pub(crate) fn reference_target(&self, column: usize, cx: &App) -> Option<db::RelationRef> {
        self.reference_in_column(column, cx)
            .map(|(target, _)| target)
    }

    /// The table and `where` clause a field of the selected row refers to.
    ///
    /// `None` when the pane is not on a browsed table, when the column is in
    /// no foreign key, or when any column of that key is null in this row — a
    /// null reference points at nothing, and `where id = NULL` matches nothing
    /// while looking like a filter that failed.
    fn reference_in_column(&self, column: usize, cx: &App) -> Option<(db::RelationRef, String)> {
        let source = self.pane().active()?.relation.clone()?;
        let data = self.pane().grid.read(cx).data().clone();
        let row = self.pane().selected_row?;
        let column = data.columns.get(column)?;
        let snapshot = self.snapshot(cx)?;
        let key = snapshot
            .relation(&source)?
            .foreign_keys
            .iter()
            .find(|key| key.columns.iter().any(|c| c.as_ref() == column.meta.name))?;
        let mut parts = Vec::with_capacity(key.columns.len());
        for (ix, name) in key.columns.iter().enumerate() {
            // Every column of the key, not only the one under the cursor: half
            // a composite key is a filter that would show the wrong row as
            // confidently as the right one.
            let target = key.target_columns.get(ix)?;
            let held = data
                .columns
                .iter()
                .find(|column| column.meta.name == name.as_ref())?;
            let value = held.value(row);
            if value == db::Value::Null {
                return None;
            }
            parts.push(format!(
                "{} = {}",
                db::schema::quote_ident(target),
                sqlgen::literal(&value)
            ));
        }
        Some((key.target.clone(), parts.join(" and ")))
    }

    /// The primary key, appended to a browse's `order by` so that paging over
    /// it is stable. Empty for a relation with no key the app can see — a view
    /// has none, and there is nothing honest to sort it by.
    ///
    /// `continuing` is whether there is already an `order by` to add to, which
    /// is the difference between `, id asc` and ` order by id asc`. A user's
    /// sort on a column with ties needs this as much as no sort at all does:
    /// `order by status` puts a thousand rows in no particular order within
    /// each status, and `offset` walks straight through them.
    fn tiebreak(&self, relation: &db::RelationRef, continuing: bool, cx: &App) -> String {
        let Some(key) = self.snapshot(cx).and_then(|snapshot| {
            snapshot
                .relation(relation)
                .and_then(|r| r.primary_key().cloned())
        }) else {
            return String::new();
        };
        let columns: Vec<_> = key
            .columns
            .iter()
            .map(|name| format!("{} asc", db::schema::quote_ident(name)))
            .collect();
        match (columns.is_empty(), continuing) {
            (true, _) => String::new(),
            (false, true) => format!(", {}", columns.join(", ")),
            (false, false) => format!(" order by {}", columns.join(", ")),
        }
    }

    /// A header was clicked.
    ///
    /// A browsed table goes back to the server, because the fifty thousand rows
    /// on screen are a prefix and sorting a prefix answers the wrong question:
    /// the largest value in the table is very unlikely to be in the first page
    /// of it. Anything else — the result of a statement someone wrote — is
    /// sorted here, because those rows *are* the whole answer and a round trip
    /// would only re-run the query.
    pub fn apply_sort(&mut self, cx: &mut Context<Self>) {
        let sort = self.pane().grid.read(cx).sort();

        if let Some(relation) = self.pane().active().and_then(|tab| tab.relation.clone()) {
            self.pane_mut().pending_sort = sort;
            // Same reason as the filter: a different order means page four
            // holds different rows, and the rows the reader wants to see
            // first are the ones the new order put first.
            self.set_page(0);
            self.reload_relation(relation, cx);
            return;
        }

        let base = match self.pane().unsorted.clone() {
            Some(base) => base,
            None => {
                let data = self.pane().grid.read(cx).data().clone();
                self.pane_mut().unsorted = Some(data.clone());
                data
            }
        };
        let sorted = match sort {
            Some(sort) => base.permuted(&base.sort_order(sort.col, sort.descending)),
            // Cleared: the rows as the server sent them.
            None => (*base).clone(),
        };
        self.pane().grid.update(cx, |grid, cx| {
            grid.set_data(sorted, cx);
            grid.set_sort(sort, cx);
        });
        cx.notify();
    }

    /// Enter in the results filter. Does nothing useful on a tab that is not
    /// browsing a table, because there is nothing to re-issue.
    pub fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let Some(relation) = self.pane().active().and_then(|tab| tab.relation.clone()) else {
            return;
        };
        // Back to the first page: a new predicate is a new set of rows, and
        // page four of it is a page nobody asked to see.
        self.set_page(0);
        self.reload_relation(relation, cx);
        self.save_session(cx);
    }

    fn set_page(&mut self, page: usize) {
        let index = self.pane().active_tab;
        if let Some(tab) = self.pane_mut().tabs.get_mut(index) {
            tab.page = Some(page);
        }
    }

    /// The pagination footer. `page` is 0-based, and a browsed table is the
    /// only tab that has one — see [`crate::pane::CenterTab::page`].
    pub fn show_page(&mut self, page: usize, cx: &mut Context<Self>) {
        let Some(relation) = self.pane().active().and_then(|tab| tab.relation.clone()) else {
            return;
        };
        if self.pane().active().and_then(|tab| tab.page) == Some(page) {
            return;
        }
        self.set_page(page);
        // The cursor was pointing at a row on the page being left, and the row
        // it lands on here is a different row of a different page. Top of the
        // new page is the only honest place for it.
        self.pane_mut().selected_row = Some(0);
        self.reload_relation(relation, cx);
        self.save_session(cx);
    }

    // ---- the chip editor --------------------------------------------------

    /// The funnel: chips or a hand-written clause.
    ///
    /// Leaving the chips takes their SQL with it, so the clause starts as
    /// whatever was already being asked rather than as an empty box — which is
    /// the point of the switch. Coming back is free: the chips were never
    /// deleted, only stopped being the ones in force.
    pub fn toggle_filter_mode(&mut self, cx: &mut Context<Self>) {
        self.stash_editor(cx);
        self.pane_mut().composer = None;
        let Some(tab) = self.pane_mut().active_mut() else {
            return;
        };
        match tab.filter.raw {
            true => tab.filter.to_chips(),
            false => tab.filter.to_raw(),
        }
        let (raw, text) = (tab.filter.raw, tab.filter.text.clone());
        if raw {
            let filter = self.pane().filter.clone();
            filter.update(cx, |filter, cx| filter.set_text(&text, cx));
            self.focus_filter(cx);
        }
        self.apply_filter(cx);
        cx.notify();
    }

    fn focus_filter(&mut self, cx: &mut Context<Self>) {
        let handle = self.pane().filter.read(cx).focus_handle(cx);
        self.pending_focus = Some(handle);
        cx.notify();
    }

    fn focus_chip_value(&mut self, cx: &mut Context<Self>) {
        let handle = self.pane().chip_value.read(cx).focus_handle(cx);
        self.pending_focus = Some(handle);
        cx.notify();
    }

    /// Open the composer: on `Some(index)` to change a chip, on `None` to add
    /// one. A new chip starts on the first column of the table rather than on
    /// nothing, because "which column" is a question with an obvious first
    /// answer and making someone answer it before they can type is a step for
    /// its own sake.
    pub fn open_chip(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        let chip = match index.and_then(|i| self.pane().active()?.filter.chips.get(i).cloned()) {
            Some(chip) => chip,
            None => {
                let column = self
                    .pane()
                    .grid
                    .read(cx)
                    .data()
                    .columns
                    .first()
                    .map(|column| column.meta.name.to_string())
                    .unwrap_or_default();
                let join = match self.pane().active() {
                    // A second chip inherits the join of the one before it, so
                    // a row of `or`s does not need the word clicked every time.
                    Some(tab) => tab.filter.chips.last().map(|c| c.join).unwrap_or_default(),
                    None => crate::filter::Join::And,
                };
                crate::filter::Chip {
                    column,
                    join,
                    ..Default::default()
                }
            }
        };
        let value = chip.value.clone();
        self.pane_mut().composer = Some(crate::pane::Composer {
            editing: index,
            chip,
        });
        let input = self.pane().chip_value.clone();
        input.update(cx, |input, cx| input.set_text(&value, cx));
        self.focus_chip_value(cx);
    }

    pub fn close_chip(&mut self, cx: &mut Context<Self>) {
        self.pane_mut().composer = None;
        cx.notify();
    }

    /// Put the composer's chip into the row and ask the server again.
    ///
    /// An unfinished chip is dropped rather than kept: the composer is open
    /// precisely so that a chip can be abandoned, and a row full of conditions
    /// that do nothing is a row that lies about what is being filtered.
    pub fn commit_chip(&mut self, cx: &mut Context<Self>) {
        let value = self.pane().chip_value.read(cx).text(cx);
        let Some(mut composer) = self.pane_mut().composer.take() else {
            return;
        };
        composer.chip.value = value;
        let complete = composer.chip.to_sql().is_some();
        if let Some(tab) = self.pane_mut().active_mut() {
            match (composer.editing, complete) {
                (Some(index), true) if index < tab.filter.chips.len() => {
                    tab.filter.chips[index] = composer.chip
                }
                (Some(index), false) if index < tab.filter.chips.len() => {
                    tab.filter.chips.remove(index);
                }
                (None, true) => tab.filter.chips.push(composer.chip),
                _ => {}
            }
        }
        self.apply_filter(cx);
        cx.notify();
    }

    /// Change the column or the operator of the chip being composed. Both are
    /// picked from a menu, and both re-ask nothing until the chip is committed:
    /// changing `=` to `is null` mid-edit should not send a statement.
    pub fn set_chip_column(&mut self, column: String, cx: &mut Context<Self>) {
        if let Some(composer) = self.pane_mut().composer.as_mut() {
            composer.chip.column = column;
        }
        cx.notify();
    }

    pub fn set_chip_op(&mut self, op: crate::filter::Op, cx: &mut Context<Self>) {
        if let Some(composer) = self.pane_mut().composer.as_mut() {
            composer.chip.op = op;
        }
        // `is null` takes no value, so the box goes away; the gesture is
        // finished and there is nothing left to type.
        match op.takes_value() {
            true => self.focus_chip_value(cx),
            false => self.commit_chip(cx),
        }
        cx.notify();
    }

    pub fn remove_chip(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(tab) = self.pane_mut().active_mut() {
            if index < tab.filter.chips.len() {
                tab.filter.chips.remove(index);
            }
        }
        // Editing the chip that just went away would put it back.
        if self.pane().composer.as_ref().and_then(|c| c.editing) == Some(index) {
            self.pane_mut().composer = None;
        }
        self.apply_filter(cx);
        cx.notify();
    }

    /// Click the `and` between two chips to make it an `or`.
    pub fn flip_join(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(tab) = self.pane_mut().active_mut() {
            if let Some(chip) = tab.filter.chips.get_mut(index) {
                chip.join = chip.join.flipped();
            }
        }
        self.apply_filter(cx);
        cx.notify();
    }

    /// Take the whole row off. The chips go with it: "clear the filter" that
    /// left six chips behind, greyed out, would be a filter nobody could
    /// convince the app they had finished with.
    pub fn clear_filter(&mut self, cx: &mut Context<Self>) {
        self.pane_mut().composer = None;
        if let Some(tab) = self.pane_mut().active_mut() {
            tab.filter.chips.clear();
            tab.filter.text.clear();
        }
        let filter = self.pane().filter.clone();
        filter.update(cx, |filter, cx| filter.set_text("", cx));
        self.apply_filter(cx);
        cx.notify();
    }

    /// The results toolbar's refresh button: the same statement again.
    pub fn refresh_results(&mut self, cx: &mut Context<Self>) {
        match self.pane().active().and_then(|tab| tab.relation.clone()) {
            Some(relation) => self.reload_relation(relation, cx),
            // Not a browsed table, so the honest re-run is whatever the editor
            // last sent.
            None => {
                if let Some(sql) = self.pane().last_sql.clone() {
                    self.run(sql.to_string(), cx);
                }
            }
        }
    }

    /// Re-read the catalog for the open connection.
    pub fn refresh_schema(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.session.clone() {
            session.update(cx, |session, cx| session.refresh_schema(cx));
        }
    }

    /// Follow an object through a rename, or let go of it after a drop.
    ///
    /// A rename keeps the tab: it is the same table, and closing it because its
    /// name changed would throw away the script sitting in it. A drop keeps the
    /// tab too, but pointing at nothing — an emptied tab is an ordinary query
    /// tab, which is the only honest thing a tab about a table that no longer
    /// exists can be, and closing it outright would take the console's text
    /// with it.
    pub(crate) fn retarget_tabs(
        &mut self,
        from: &db::RelationRef,
        to: Option<db::RelationRef>,
        cx: &mut Context<Self>,
    ) {
        for pane in &mut self.panes {
            crate::objects::retarget(&mut pane.tabs, from, to.as_ref());
        }
        // Whether a grid can be written back is a fact about the table its rows
        // came from, and that table has just been renamed or has stopped
        // existing. Asked again rather than adjusted, because the answer is
        // already written down in one place.
        for id in self.panes.iter().map(|pane| pane.id).collect::<Vec<_>>() {
            self.refresh_editability(id, cx);
        }
        self.save_session(cx);
        cx.notify();
    }

    /// Ask again for a table the grid is showing, if it is showing this one.
    ///
    /// A truncate leaves a screenful of rows that no longer exist. Re-asking is
    /// better than clearing: the table is still there, and an empty grid under
    /// a live table is the right answer arrived at honestly.
    pub(crate) fn reload_active_relation(
        &mut self,
        reference: &db::RelationRef,
        cx: &mut Context<Self>,
    ) {
        let showing = self.pane().active().and_then(|tab| tab.relation.clone());
        if showing.as_ref() == Some(reference) {
            self.reload_relation(reference.clone(), cx);
        }
    }

    /// Hand focus to something at the next paint. Sheets and menus are opened
    /// from a click, which is a frame too early to focus anything.
    pub(crate) fn focus_next(&mut self, handle: FocusHandle) {
        self.pending_focus = Some(handle);
    }

    /// Open the connection window on a blank form.
    pub fn new_connection(&mut self, cx: &mut Context<Self>) {
        self.open_connection_window(None, cx);
    }

    /// Open the connection window on an existing connection.
    pub fn edit_connection(&mut self, config: db::ConnectionConfig, cx: &mut Context<Self>) {
        self.open_connection_window(Some(config), cx);
    }

    /// The connection window, if it is open. For the screenshot harness, which
    /// has to capture the window rather than the app.
    pub fn connection_window(
        &self,
    ) -> Option<gpui::WindowHandle<crate::connection_window::ConnectionWindow>> {
        self.connection_window
    }

    /// One window, like Settings: a second copy of the form could only
    /// disagree with the first about what is saved.
    fn open_connection_window(
        &mut self,
        config: Option<db::ConnectionConfig>,
        cx: &mut Context<Self>,
    ) {
        if let Some(window) = self.connection_window {
            let shown = window
                .update(cx, |view, window, cx| {
                    view.show(config.clone(), window, cx);
                    window.activate_window();
                })
                .is_ok();
            if shown {
                return;
            }
            // The handle outlived the window — it was closed while we held it.
            self.connection_window = None;
        }
        // Deferred for the same reason Settings is: opening a window renders
        // its first frame there and then, and that frame reads this workspace,
        // which is checked out for as long as this method is running.
        let workspace = cx.weak_entity();
        cx.defer(move |cx: &mut App| {
            match crate::connection_window::open(workspace.clone(), config, cx) {
                Ok(window) => {
                    let _ = workspace.update(cx, |workspace, _| {
                        workspace.connection_window = Some(window)
                    });
                }
                Err(error) => log::warn!("could not open the connection window: {error:#}"),
            }
        });
    }

    /// Keep what the connection window collected and open it. The password is
    /// `None` when the field was left alone on an existing connection, which
    /// means "keep whatever is in the Keychain" — distinct from `Some("")`,
    /// which means "there is no password".
    pub(crate) fn save_connection(
        &mut self,
        config: &db::ConnectionConfig,
        password: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(store) = self.store.clone() {
            if let Err(error) = store.save_connection(config) {
                log::error!("could not save the connection: {error:#}");
            }
            // The secret goes to the Keychain and nowhere else.
            if let Some(password) = &password {
                if let Err(error) = store::secrets::set_password(config.id, password) {
                    log::error!("could not save the password: {error:#}");
                }
            }
            self.connections = store.connections().unwrap_or_default();
        }
        // The typed password goes straight to the session as well as to the
        // Keychain. `None` on an edit that left the field alone, which is the
        // case where the Keychain is the only answer.
        self.open_connection_with(config.clone(), password, cx);
    }

    pub fn delete_connection(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        if let Some(store) = self.store.clone() {
            if let Err(error) = store.delete_connection(id) {
                log::error!("could not delete the connection: {error:#}");
            }
            self.connections = store.connections().unwrap_or_default();
        }
        // Close every session on it — a connection deleted while three tabs
        // are on three of its databases takes all three with it — and unbind
        // the tabs that were pointing at them.
        let closed: Vec<_> = self
            .sessions
            .iter()
            .filter(|session| session.read(cx).config.id == id)
            .cloned()
            .collect();
        self.sessions
            .retain(|session| session.read(cx).config.id != id);
        for pane in &mut self.panes {
            for tab in &mut pane.tabs {
                if tab.session.as_ref().is_some_and(|s| closed.contains(s)) {
                    tab.session = None;
                }
            }
        }
        if self.session.as_ref().is_some_and(|s| closed.contains(s)) {
            self.session = None;
            self.tree.clear();
            self.catalog.set(None);
        }
        cx.notify();
    }

    fn on_session_event(
        &mut self,
        session: Entity<Session>,
        event: &SessionEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            SessionEvent::StateChanged => cx.notify(),
            // The privileges pane redraws, and so does the grid: what the
            // connected role may do here is one of the reasons it is read-only.
            SessionEvent::PrivilegesChanged => {
                if self.session.as_ref() == Some(&session) {
                    for id in self.panes.iter().map(|pane| pane.id).collect::<Vec<_>>() {
                        self.refresh_editability(id, cx);
                    }
                }
                cx.notify();
            }
            SessionEvent::SchemaChanged => {
                log::debug!(
                    "a catalog arrived for {}; the window is on {:?}",
                    session.read(cx).config.database,
                    self.session
                        .as_ref()
                        .map(|s| s.read(cx).config.database.clone())
                );
                // A catalog can arrive for a tab nobody is looking at — the
                // one in the next tab along, still connecting while you read
                // this one. It is kept on its own session and put on screen
                // when that tab is activated; the window describes exactly one
                // connection at a time, and this is not it.
                if self.session.as_ref() != Some(&session) {
                    cx.notify();
                    return;
                }
                let snapshot = session.read(cx).snapshot.clone();
                self.rebuild_tree(cx);
                self.collapsed = tree::initially_collapsed(&self.tree).into_iter().collect();
                self.selected_node = None;
                // And the consoles complete against the catalog they can now
                // see. The editors hold a handle on this, so there is nothing
                // to re-install: one write reaches every one of them, including
                // the panes that are not on screen.
                self.catalog.set(snapshot.clone());
                // A keyspace catalog is only the databases and how full they
                // are, so the browser is still empty at this point: the keys
                // themselves are a walk, and this is where it starts.
                self.scan_more_keys(&session, cx);
                // A tab that was moved to this database asks it for the table
                // it was on. `reload_relation` decides whether that is a
                // statement or a notice.
                if let Some(relation) = self.pending_rebrowse.take() {
                    self.reload_relation(relation, cx);
                }
                // `TUPLI_OPEN` names a table before there is a catalog to look
                // it up in, so this is the first moment it can be honoured.
                let opening = std::mem::take(&mut self.pending_open);
                let (known, unknown): (Vec<_>, Vec<_>) =
                    opening.into_iter().partition(|relation| {
                        snapshot
                            .as_ref()
                            .is_some_and(|snapshot| snapshot.relation(relation).is_some())
                    });
                // A name this catalog does not have still gets a tab, empty
                // and never run. That is exactly what a window restored
                // against a different connection looks like, and the notice
                // that explains it cannot be photographed any other way.
                for relation in &unknown {
                    log::warn!("TUPLI_OPEN names {relation}, which the catalog does not have");
                    self.seed_tab(relation);
                }
                // Only the last one is browsed. The others are tabs and nothing
                // more: a statement sent while the first is still in flight is
                // dropped (see [`crate::session::Session::run`]), so opening
                // four tables properly would show three empty grids and lie
                // about why. What this is for is a strip with more tabs in it
                // than fit, which does not need any of them to have rows.
                if let Some((last, rest)) = known.split_last() {
                    for relation in rest {
                        self.seed_tab(relation);
                    }
                    self.open_relation(last, cx);
                } else if !unknown.is_empty() {
                    let index = self.pane().tabs.len() - 1;
                    self.show_tab(index, cx);
                }
                // The same for the menu and the sheets, which cannot be asked
                // for before there is a catalog to say what kind of object it
                // is about.
                if let Some((relation, op)) = self.pending_demo.take() {
                    match op {
                        Some(op) => self.prompt_object(op, relation, cx),
                        None => {
                            self.open_object_menu(relation, gpui::point(px(196.), px(150.)), cx)
                        }
                    }
                }
                // `TUPLI_COMPLETE` types a fragment into the console and opens
                // the popup on it. Screenshots only, and for the same reason as
                // the menu above: a list that only exists while a word is being
                // typed cannot be photographed any other way.
                if let Some(text) = std::env::var_os("TUPLI_COMPLETE") {
                    let text = text.to_string_lossy().into_owned();
                    let editor = self.pane().editor.clone();
                    self.pending_focus = Some(editor.read(cx).focus().clone());
                    editor.update(cx, |editor, cx| {
                        editor.set_text("", cx);
                        editor.insert(&text, cx);
                        editor.refresh_completions(true, cx);
                    });
                }
                // The titlebar switcher, for the same reason: there is nothing
                // in it until the server has said what databases it has.
                if std::env::var_os("TUPLI_SWITCHER").is_some() {
                    self.open_database_menu(gpui::point(px(660.), px(30.)), cx);
                }
                // And `TUPLI_SWITCH` is the same menu's item being chosen.
                let switching = self.pending_switch.take();
                if let Some(database) = &switching {
                    self.switch_database(database, cx);
                }
                // `TUPLI_AFTER` waits for the catalog the switch produces, not
                // for this one: the table it names lives on the other database.
                if switching.is_none() {
                    if let Some(relation) = self.pending_after.take() {
                        self.open_relation(&relation, cx);
                    }
                }
                // Design tabs from the last run become editors now, so that a
                // restored one is the editor it looks like and not an empty
                // shell that a second Design Table would hand straight back.
                self.hydrate_structure_tabs(cx);
                if let Some((reference, preview)) = self.pending_design.take() {
                    self.demo_structure(reference, preview, cx);
                }
                // A structure save waits for this: what the table looks
                // like now is what the server ended up with, which is not
                // always what was asked for.
                self.adopt_structure(cx);
                // A new catalog can be a new answer to "can this grid be
                // written back" — a key added in another session, a table
                // renamed in this one. Nothing else asks again after a refresh,
                // so the grid would otherwise keep yesterday's answer until the
                // next run.
                for id in self.panes.iter().map(|pane| pane.id).collect::<Vec<_>>() {
                    self.refresh_editability(id, cx);
                }
                // A table tab restored from the last run has a name and no
                // rows: there was no server to ask when the window was built.
                // This is the first moment there is one — and the first moment
                // it can be known that this server has no such table, which is
                // what happens when the window comes back against a connection
                // that is not the one it was closed on. Browsing it anyway
                // would send a statement whose only possible answer is
                // `relation "..." does not exist`, and the red banner that
                // comes back reads as something having gone wrong just now.
                // `render_data_tab` says the true thing instead.
                let restored = self
                    .pane()
                    .active()
                    .filter(|tab| tab.kind == CenterKind::Table)
                    .and_then(|tab| tab.relation.clone())
                    .filter(|_| self.pane().results.is_empty());
                if let Some(relation) = restored {
                    // `reload_relation` is what decides whether there is
                    // anything to ask for: this server may not have it.
                    self.reload_relation(relation, cx);
                }
                cx.notify();
            }
            SessionEvent::Finished => self.absorb_run(session, cx),
            SessionEvent::Applied => self.absorb_apply(session, cx),
            SessionEvent::KeysChanged => {
                // Only the tree, and only what is expanded is left alone: a
                // walk lands over and over as it goes, and a sidebar that
                // collapsed itself every few hundred keys would be unusable
                // for exactly as long as the scan is interesting.
                if self.session.as_ref() == Some(&session) {
                    self.rebuild_tree(cx);
                    cx.notify();
                }
                self.scan_more_keys(&session, cx);
                self.open_pending_key(&session, cx);
            }
            SessionEvent::KeyOpened => self.absorb_key(session, cx),
        }
    }

    /// Move an opened key's contents into the pane that asked for it.
    ///
    /// [`Workspace::absorb_run`] without the half that is about statements —
    /// no `sql`, no elapsed time, no message log entry — because a key was
    /// clicked, not typed. What is left is the same: rows into the grid, and
    /// only if the pane is still looking at the connection they came from.
    fn absorb_key(&mut self, session: Entity<Session>, cx: &mut Context<Self>) {
        let Some((rows, error)) = session.update(cx, |session, _| {
            session
                .last_key
                .as_mut()
                .map(|view| (view.rows.take(), view.error.clone()))
        }) else {
            return;
        };
        let target = self.running_pane.take().unwrap_or(self.active_pane);
        let elsewhere = self
            .pane_by(target)
            .and_then(|pane| pane.active())
            .and_then(|tab| tab.session.clone())
            .is_some_and(|showing| showing != session);
        if elsewhere {
            return;
        }
        let Some(pane) = self.pane_by_mut(target) else {
            return;
        };
        pane.error = error;
        pane.elapsed = None;
        pane.affected = None;
        pane.last_sql = None;
        pane.truncated = false;
        pane.results.clear();
        pane.result_index = 0;
        pane.unsorted = None;
        let arrived = rows.map(|rows| {
            pane.row_count = rows.row_count();
            pane.selected_row = (pane.row_count > 0).then_some(0);
            pane.selected_column = 0;
            (pane.grid.clone(), Arc::new(rows))
        });
        if let Some((grid, rows)) = arrived {
            grid.update(cx, |grid, cx| grid.set_data_arc(rows, cx));
        }
        // A key is never editable through the grid — there is no transaction to
        // stage an edit into — so nothing here asks whether it is.
        cx.notify();
    }

    /// Move a finished commit into the window.
    ///
    /// A commit that went through clears the staged changes and re-asks the
    /// server, because a row the database filled in itself — a serial, a
    /// default, a trigger — is only knowable by looking. A commit that failed
    /// changes nothing at all: the transaction rolled back, so the staged
    /// edits are still exactly what is not saved yet, and they stay on screen.
    fn absorb_apply(&mut self, session: Entity<Session>, cx: &mut Context<Self>) {
        let Some((summary, elapsed, error)) = ({
            let session = session.read(cx);
            session
                .last_apply
                .as_ref()
                .map(|applied| (applied.summary(), applied.elapsed, applied.error.clone()))
        }) else {
            return;
        };

        let target = self.active_pane;
        let note = self.import_note.take();
        self.messages.push(RunMessage {
            at_ms: now_ms(),
            sql: match &note {
                Some(_) => format!("import ({summary})").into(),
                None => format!("commit ({summary})").into(),
            },
            elapsed,
            tone: match &error {
                Some(_) => MessageTone::Failed,
                None => MessageTone::Ok,
            },
            text: match (&error, &note) {
                (Some(error), _) => error.full_text().into(),
                (None, Some(note)) => format!("Imported {note}.").into(),
                (None, None) => format!("Committed {summary}.").into(),
            },
            // A commit's own notices go with the statements inside it, and
            // those are not shown one by one. Nothing to attach here.
            notices: Vec::new(),
        });
        if self.messages.len() > MESSAGES_KEPT {
            self.messages.drain(..self.messages.len() - MESSAGES_KEPT);
        }

        if let Some(pane) = self.pane_by_mut(target) {
            pane.error = error.clone();
        }
        // A structure save comes back through the same door as a grid commit —
        // it is one transaction either way — but nothing after this point is
        // about it: there are no staged cells to clear and no rows to re-read.
        if !self.finish_structure(error.is_some(), cx) && error.is_none() {
            if let Some(pane) = self.pane_by(target) {
                let grid = pane.grid.clone();
                grid.update(cx, |grid, cx| grid.discard_changes(cx));
            }
            self.refresh_results(cx);
        }
        cx.notify();
    }

    /// Write one finished statement into the Messages log and the history
    /// table.
    ///
    /// Separate from delivering the rows because the two do not always both
    /// happen: a statement whose tab has been switched away from still ran,
    /// still took the time it took, and still belongs in the log — it just has
    /// nowhere to put its rows.
    #[allow(clippy::too_many_arguments)]
    fn record_run(
        &mut self,
        ran: SharedString,
        elapsed: std::time::Duration,
        truncated: bool,
        affected: Option<u64>,
        error: &Option<db::DbError>,
        notices: Vec<db::Notice>,
        row_count: usize,
        cx: &mut Context<Self>,
    ) {
        // The Messages log gets the same facts the status bar gets, plus the
        // ones it has no room for. Recorded here rather than at submit time so
        // a row can never claim an outcome that has not happened.
        let summary = match (&error, affected) {
            (Some(error), _) => error.full_text(),
            (None, Some(count)) => format!("{} affected", count_of(count as usize, "row")),
            (None, None) if truncated => {
                format!("{}+ rows (row cap reached)", thousands(row_count))
            }
            (None, None) => count_of(row_count, "row"),
        };
        self.messages.push(RunMessage {
            at_ms: now_ms(),
            sql: ran,
            elapsed,
            tone: match &error {
                Some(error) if error.is_canceled() => MessageTone::Canceled,
                Some(_) => MessageTone::Failed,
                None => MessageTone::Ok,
            },
            text: summary.into(),
            notices,
        });
        if self.messages.len() > MESSAGES_KEPT {
            self.messages.drain(..self.messages.len() - MESSAGES_KEPT);
        }
        if let (Some(store), Some(id)) = (self.store.clone(), self.pending_history.take()) {
            let count = if affected.is_some() {
                affected.map(|n| n as i64)
            } else {
                Some(row_count as i64)
            };
            let message = error.as_ref().map(|error| error.full_text());
            if let Err(error) =
                store.finish_query(id, elapsed.as_millis() as i64, count, message.as_deref())
            {
                log::warn!("could not record the query outcome: {error:#}");
            }
            self.reload_lists(cx);
        }
    }

    /// Move a finished statement's result into the window: rows to the grid,
    /// timing to the status bar, outcome to the history.
    fn absorb_run(&mut self, session: Entity<Session>, cx: &mut Context<Self>) {
        let rows = session.update(cx, |session, _| session.take_rows());
        let Some((sql, elapsed, truncated, affected, error, notices)) = ({
            let session = session.read(cx);
            session.last.as_ref().map(|run| {
                (
                    run.sql.clone(),
                    run.elapsed,
                    run.truncated,
                    run.affected,
                    run.error.clone(),
                    run.notices.clone(),
                )
            })
        }) else {
            return;
        };

        // Into the pane that asked, not the pane that happens to be active:
        // a server can take a while, and a person who splits the window and
        // starts typing in the new pane while they wait has not asked for
        // their rows to land somewhere else.
        let target = self.running_pane.take().unwrap_or(self.active_pane);
        // And only if that pane is still looking at the database the rows came
        // from. Tabs hold their own connections, so switching tabs while a
        // query runs is switching servers: the answer arrives addressed to a
        // table that the tab now on screen has never heard of. It still ran, so
        // it still goes in the log — it just has nowhere to be drawn.
        let elsewhere = self
            .pane_by(target)
            .and_then(|pane| pane.active())
            .and_then(|tab| tab.session.clone())
            .is_some_and(|showing| showing != session);
        if elsewhere {
            log::info!("a run finished for a tab that is no longer showing; logging it only");
            let row_count = rows.as_ref().map(|rows| rows.row_count()).unwrap_or(0);
            self.record_run(
                sql, elapsed, truncated, affected, &error, notices, row_count, cx,
            );
            cx.notify();
            return;
        }
        // The table these rows came out of, when they came out of one and only
        // one — the same rule editability uses, and for the same reason: a
        // script's third answer having a column called `id` does not make it
        // that table's key. Read before the pane is borrowed to be written.
        let described = self
            .pane_by(target)
            .filter(|pane| pane.results.is_empty())
            .and_then(|pane| pane.active())
            .filter(|tab| tab.kind == CenterKind::Table)
            .and_then(|tab| tab.relation.clone())
            .zip(self.snapshot(cx))
            .and_then(|(reference, snapshot)| snapshot.relation(&reference).cloned());
        let Some(pane) = self.pane_by_mut(target) else {
            log::warn!("a run finished for pane {target}, which is gone");
            return;
        };

        pane.last_sql = Some(sql);
        pane.elapsed = Some(elapsed);
        pane.truncated = truncated;
        pane.affected = affected;
        pane.error = error.clone();

        // A syntax error comes back as a 1-based character position within the
        // statement that was sent, which is only a place in the document once
        // the statement's own start is added back.
        let mark = match (&error, pane.run_origin) {
            (Some(error), Some(origin)) => error.offset().map(|offset| origin + offset),
            _ => None,
        };
        let editor = pane.editor.clone();

        // The grid is updated after the borrow above ends, because it needs
        // `cx` and the pane is holding `self`.
        let mut arrived = None;
        if let Some(mut rows) = rows {
            if let Some(relation) = &described {
                crate::editing::describe_columns(&mut rows, relation);
            }
            pane.row_count = rows.row_count();
            pane.selected_row = (pane.row_count > 0).then_some(0);
            pane.selected_column = 0;
            // A fresh answer, so the "as the server sent it" copy is stale.
            pane.unsorted = None;
            let rows = std::sync::Arc::new(rows);
            // Every statement that returned rows keeps them, and the grid shows
            // the one that just landed. A script's earlier answers are still
            // there, a tab away.
            pane.results.push(crate::pane::StatementResult {
                sql: pane.last_sql.clone().unwrap_or_default(),
                rows: rows.clone(),
                elapsed,
                truncated,
            });
            pane.result_index = pane.results.len() - 1;
            arrived = Some((pane.grid.clone(), rows, pane.pending_sort.take()));
        }
        // A failure stays where you are looking. The Data tab carries the
        // server's message in a banner above the grid, so the error is already
        // in front of you; switching to Messages on top of that takes away the
        // rows you were reading — and on a typo in the filter box, the rows are
        // exactly what you want to still see while you fix it. Messages remains
        // the log, for when you want the history rather than the last word.
        let row_count = pane.row_count;
        let ran = pane.last_sql.clone().unwrap_or_default();

        // Under the character the server named, and the cursor put there, so
        // the next thing typed is a fix rather than a hunt.
        if let Some(offset) = mark {
            editor.update(cx, |editor, cx| editor.mark_error(offset, cx));
        }

        let landed = arrived.is_some();
        if let Some((grid, rows, sort)) = arrived {
            grid.update(cx, |grid, cx| {
                grid.set_data_arc(rows, cx);
                // These rows already came back in that order, so the arrow is
                // describing them rather than promising something.
                if sort.is_some() {
                    grid.set_sort(sort, cx);
                }
            });
        }
        // Different rows, so possibly a different answer to "can this be
        // written back". Asked here rather than when the tab was opened,
        // because until the rows land nobody knows what was selected.
        if landed {
            self.refresh_editability(target, cx);
            // Where `TUPLI_CELL` asked to be looking, now that there are rows
            // to look at. Once: a second statement in the same script is not
            // the one the flag was talking about.
            if let Some((row, col)) = self.pending_cell.take() {
                if let Some(grid) = self.pane_by(target).map(|pane| pane.grid.clone()) {
                    grid.update(cx, |grid, cx| grid.set_cursor(row, col, false, cx));
                }
            }
            // And then the hop, if `TUPLI_FOLLOW` asked for one. Deferred: the
            // cursor above moved by emitting an event, which this workspace has
            // not been handed yet, and a hop read off a stale cursor is a hop
            // from the wrong cell. The queue is first-in-first-out, so by the
            // time this runs the move has landed.
            if std::mem::take(&mut self.pending_follow) {
                let workspace = cx.weak_entity();
                cx.defer(move |cx: &mut App| {
                    let _ = workspace.update(cx, |workspace, cx| workspace.follow_reference(cx));
                });
            }
        }

        self.record_run(
            ran, elapsed, truncated, affected, &error, notices, row_count, cx,
        );
        // A rename, truncate or drop has consequences past the message log —
        // tabs that were pointing at the old name, rows that are no longer
        // there — and this is the first moment it is known whether the server
        // actually did it.
        self.finish_object_statement(error.is_some(), cx);
        cx.notify();

        // A script carries on where it left off. A failure ends it: the
        // statements after a `create table` that did not happen are going to
        // fail too, and twenty red rows say nothing the first one did not.
        let next = match self.pane_by_mut(target) {
            Some(pane) if error.is_some() => {
                pane.queue.clear();
                None
            }
            Some(pane) => pane.queue.pop_front(),
            None => None,
        };
        if let Some((sql, origin)) = next {
            self.run_at(target, sql, Some(origin), cx);
        }
    }

    /// Honour the switches that let the app boot into a state that would
    /// otherwise take four clicks to reach.
    ///
    /// `TUPLI_CONNECT` takes the same keyword string the integration tests do,
    /// `TUPLI_OPEN` a `schema.table` to browse once the catalog arrives, and
    /// `TUPLI_SIDEBAR` one of `database`, `queries`, `history`,
    /// `TUPLI_RESULTS_TAB` one of `data`, `structure`, `ddl`, `privileges`,
    /// `messages`,
    /// `TUPLI_INSPECTOR` one of `cell`, `row`, `TUPLI_CELL` a `row,column` for
    /// the cursor to land on once rows arrive, `TUPLI_MENU` a `schema.table` to
    /// open the object menu on, `TUPLI_SHEET` one of `rename`, `truncate`,
    /// `drop` to open that sheet on the table `TUPLI_MENU` names, and
    /// `TUPLI_DESIGN` a `schema.table` — or the word `new` — to open the
    /// structure editor on with a change already staged, with
    /// `TUPLI_DESIGN_SHEET` putting its preview over it, `TUPLI_SWITCHER`
    /// dropping the titlebar's database menu open and `TUPLI_SWITCH` naming a
    /// database to move to as soon as the first catalog is in. `TUPLI_PASSWORD`
    /// stands in for the Keychain, which has nothing stored under the id of a
    /// connection that was never saved. They exist
    /// for screenshots and for driving the app against a scratch server; a bad
    /// value is logged and ignored rather than being allowed to stop a launch.
    fn boot_from_environment(&mut self, cx: &mut Context<Self>) {
        // Quitting is the one moment the session has to be written and the one
        // moment nothing else will write it: the console's text belongs to no
        // tab until something makes it, and closing the window is not that.
        self._on_quit = Some(cx.on_app_quit(|workspace: &mut Self, cx| {
            workspace.save_session(cx);
            async {}
        }));

        if let Ok(want) = std::env::var("TUPLI_SIDEBAR") {
            match want.as_str() {
                "database" => self.sidebar_tab = SidebarTab::Database,
                "queries" => self.sidebar_tab = SidebarTab::Queries,
                "history" => self.sidebar_tab = SidebarTab::History,
                other => log::warn!("TUPLI_SIDEBAR={other:?} is not a tab"),
            }
        }
        if let Ok(want) = std::env::var("TUPLI_RESULTS_TAB") {
            match want.as_str() {
                "data" => self.pane_mut().results_tab = ResultsTab::Data,
                "structure" => self.pane_mut().results_tab = ResultsTab::Structure,
                "ddl" => self.pane_mut().results_tab = ResultsTab::Ddl,
                "privileges" => self.pane_mut().results_tab = ResultsTab::Privileges,
                "messages" => self.pane_mut().results_tab = ResultsTab::Messages,
                other => log::warn!("TUPLI_RESULTS_TAB={other:?} is not a tab"),
            }
        }
        if let Ok(want) = std::env::var("TUPLI_INSPECTOR") {
            match want.as_str() {
                "row" => self.inspector_tab = InspectorTab::Row,
                "table" => self.inspector_tab = InspectorTab::Table,
                other => log::warn!("TUPLI_INSPECTOR={other:?} is not a tab"),
            }
        }
        // `TUPLI_PAGE=1` opens `TUPLI_OPEN`'s table on its second page.
        if let Ok(page) = std::env::var("TUPLI_PAGE") {
            match page.parse::<usize>() {
                Ok(page) => self.pending_page = Some(page),
                Err(_) => log::warn!("TUPLI_PAGE={page:?} is not a page"),
            }
        }
        // `TUPLI_PAGE_SIZE=1000` is the setting, so that a screenshot can turn
        // a page without a table of fifty thousand rows in it.
        if let Ok(size) = std::env::var("TUPLI_PAGE_SIZE") {
            match size.parse::<usize>() {
                Ok(size) if crate::settings::PAGE_SIZES.contains(&size) => {
                    self.settings.set_page_size(size)
                }
                _ => log::warn!(
                    "TUPLI_PAGE_SIZE={size:?} is not one of {:?}",
                    crate::settings::PAGE_SIZES
                ),
            }
        }
        // `TUPLI_EXPAND=2` is the Row tab's third field asked to show all of
        // itself — the state a click on its `⋯ 10.7 KB` leaves behind.
        if let Ok(want) = std::env::var("TUPLI_EXPAND") {
            match want.parse::<usize>() {
                Ok(ix) => self.expanded_field = Some(ix),
                Err(_) => log::warn!("TUPLI_EXPAND={want:?} is not a column index"),
            }
        }
        // `TUPLI_FILTER="plan = enterprise, or mrr_cents >= 20000"` seeds the
        // chip row for the headless renderer, `TUPLI_FILTER_RAW` puts the same
        // clause in the hand-written box instead, and `TUPLI_COMPOSER` leaves
        // the editor open on a new chip. None of it is reachable from the UI;
        // it exists so the two modes can be looked at without a human clicking
        // through to them.
        if let Ok(spec) = std::env::var("TUPLI_FILTER") {
            let chips = spec
                .split(',')
                .filter_map(|term| {
                    let term = term.trim();
                    let (join, term) = match term.strip_prefix("or ") {
                        Some(rest) => (crate::filter::Join::Or, rest),
                        None => (crate::filter::Join::And, term),
                    };
                    let mut parts = term.splitn(3, ' ');
                    let column = parts.next()?.to_string();
                    let op = crate::filter::Op::parse(parts.next()?)?;
                    Some(crate::filter::Chip {
                        column,
                        op,
                        value: parts.next().unwrap_or_default().to_string(),
                        join,
                    })
                })
                .collect::<Vec<_>>();
            if let Some(tab) = self.pane_mut().active_mut() {
                tab.filter.chips = chips;
            }
            if std::env::var_os("TUPLI_FILTER_RAW").is_some() {
                if let Some(tab) = self.pane_mut().active_mut() {
                    tab.filter.to_raw();
                }
                let text = self
                    .pane()
                    .active()
                    .map(|tab| tab.filter.text.clone())
                    .unwrap_or_default();
                let filter = self.pane().filter.clone();
                filter.update(cx, |filter, cx| filter.set_text(&text, cx));
            }
            if std::env::var_os("TUPLI_COMPOSER").is_some() {
                self.open_chip(None, cx);
            }
        }
        // One table, or a comma-separated list of them — `TUPLI_OPEN=public.a,
        // public.b,public.c` is how a tab strip with more tabs than room gets
        // photographed. The last one named is the one in front.
        if let Ok(spec) = std::env::var("TUPLI_OPEN") {
            for name in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                match name.split_once('.') {
                    Some((schema, name)) => {
                        self.pending_open.push(db::RelationRef::new(schema, name))
                    }
                    None => log::warn!("TUPLI_OPEN={name:?} is not schema.table"),
                }
            }
        }
        // `TUPLI_KEY=user:1:profile` opens that key once the walk has found
        // it, which is the keyspace's answer to `TUPLI_OPEN`.
        if let Ok(key) = std::env::var("TUPLI_KEY") {
            self.pending_key = Some(key);
        }
        if let Ok(database) = std::env::var("TUPLI_SWITCH") {
            self.pending_switch = Some(database);
        }
        // `TUPLI_AFTER=schema.table` browses a table on the database
        // `TUPLI_SWITCH` moved to, which is the only way to photograph a window
        // whose two tabs are live on two databases at once.
        if let Ok(name) = std::env::var("TUPLI_AFTER") {
            match name.split_once('.') {
                Some((schema, table)) => {
                    self.pending_after = Some(db::RelationRef::new(schema, table))
                }
                None => log::warn!("TUPLI_AFTER={name:?} is not schema.table"),
            }
        }
        // `TUPLI_KEYCHAIN=<uuid>` asks the Keychain for one connection's
        // password and logs what it said — the length of it, never the thing
        // itself. For telling "there is no item" apart from "there is an item
        // and this process may not have it", which are the same sentence from
        // the server's point of view and very different sentences from a
        // person's.
        if let Ok(spec) = std::env::var("TUPLI_KEYCHAIN") {
            match spec.parse::<uuid::Uuid>() {
                Ok(id) => match store::secrets::password(id) {
                    Ok(Some(secret)) => {
                        log::info!(
                            "TUPLI_KEYCHAIN: {id} has a password of {} bytes",
                            secret.len()
                        )
                    }
                    Ok(None) => log::info!("TUPLI_KEYCHAIN: {id} has no password item"),
                    Err(error) => log::warn!("TUPLI_KEYCHAIN: {error:#}"),
                },
                Err(_) => log::warn!("TUPLI_KEYCHAIN={spec:?} is not a uuid"),
            }
        }
        // `TUPLI_FOLLOW=1` presses F6 on whatever `TUPLI_CELL` selected.
        if std::env::var_os("TUPLI_FOLLOW").is_some() {
            self.pending_follow = true;
        }
        if let Ok(spec) = std::env::var("TUPLI_CELL") {
            match spec
                .split_once(',')
                .and_then(|(row, col)| Some((row.trim().parse().ok()?, col.trim().parse().ok()?)))
            {
                Some(cell) => self.pending_cell = Some(cell),
                None => log::warn!("TUPLI_CELL={spec:?} is not row,column"),
            }
        }
        if let Ok(spec) = std::env::var("TUPLI_MENU") {
            let op = match std::env::var("TUPLI_SHEET").as_deref() {
                Ok("rename") => Some(crate::objects::ObjectOp::Rename),
                Ok("truncate") => Some(crate::objects::ObjectOp::Truncate),
                Ok("drop") => Some(crate::objects::ObjectOp::Drop),
                Ok(other) => {
                    log::warn!("TUPLI_SHEET={other:?} is not an operation");
                    None
                }
                Err(_) => None,
            };
            match spec.split_once('.') {
                Some((schema, name)) => {
                    self.pending_demo = Some((db::RelationRef::new(schema, name), op))
                }
                None => log::warn!("TUPLI_MENU={spec:?} is not schema.table"),
            }
        }
        if let Ok(spec) = std::env::var("TUPLI_DESIGN") {
            let preview = std::env::var("TUPLI_DESIGN_SHEET").is_ok();
            match spec.split_once('.') {
                Some((schema, name)) => {
                    self.pending_design = Some((Some(db::RelationRef::new(schema, name)), preview))
                }
                None if spec == "new" => self.pending_design = Some((None, preview)),
                None => log::warn!("TUPLI_DESIGN={spec:?} is not schema.table or `new`"),
            }
        }
        if let Ok(column) = std::env::var("TUPLI_DECODER") {
            self.pending_decoder = Some(column);
        }
        if let Ok(spec) = std::env::var("TUPLI_CONNECT") {
            match db::ConnectionConfig::from_spec(&spec) {
                Ok(config) => self.open_connection(config, cx),
                Err(error) => log::warn!("TUPLI_CONNECT: {error}"),
            }
            return;
        }

        // Otherwise, whatever this window was connected to last time. A
        // connection that has since been deleted is simply not reopened —
        // there is nothing to ask the user about and nothing to warn them of.
        let Some(id) = self.reopen.take() else { return };
        let Some(mut config) = self.connections.iter().find(|c| c.id == id).cloned() else {
            log::info!("the last session's connection is gone; starting disconnected");
            return;
        };
        // Back onto the database that was open, not the one the connection was
        // saved with. Restored table tabs are the reason: they name a relation
        // in a particular database, and reopening on the connection's default
        // asks the wrong server for it and gets a 42P01 for its trouble.
        if let Some(database) = self.reopen_database.take() {
            if !database.is_empty() {
                config.database = database;
            }
        }
        self.open_connection(config, cx);
    }

    /// Show one of a script's answers.
    ///
    /// The grid holds one result set at a time, so switching tabs hands it a
    /// different one — cheap, because the pane kept them all behind an `Arc`
    /// and nothing is copied.
    pub fn show_result(&mut self, index: usize, cx: &mut Context<Self>) {
        let pane = self.pane_mut();
        let Some(result) = pane.results.get(index) else {
            return;
        };
        let (rows, elapsed, truncated) = (result.rows.clone(), result.elapsed, result.truncated);
        pane.result_index = index;
        pane.results_tab = ResultsTab::Data;
        pane.row_count = rows.row_count();
        pane.selected_row = (pane.row_count > 0).then_some(0);
        pane.selected_column = 0;
        pane.elapsed = Some(elapsed);
        pane.truncated = truncated;
        pane.affected = None;
        pane.unsorted = None;
        let grid = pane.grid.clone();
        grid.update(cx, |grid, cx| grid.set_data_arc(rows, cx));
        let id = self.active_pane;
        self.refresh_editability(id, cx);
        cx.notify();
    }

    pub fn select_results_tab(&mut self, tab: ResultsTab, cx: &mut Context<Self>) {
        self.pane_mut().results_tab = tab;
        cx.notify();
    }

    pub fn toggle_left_panel(&mut self, cx: &mut Context<Self>) {
        self.left_open = !self.left_open;
        self.save_layout();
        cx.notify();
    }

    pub fn toggle_right_panel(&mut self, cx: &mut Context<Self>) {
        self.right_open = !self.right_open;
        self.save_layout();
        cx.notify();
    }

    pub fn toggle_bottom_dock(&mut self, cx: &mut Context<Self>) {
        self.dock_open = !self.dock_open;
        // Closing the dock ends the maximise with it: reopening it later into a
        // state the reader set two tables ago, with the console gone, would be
        // the app remembering the wrong thing.
        if !self.dock_open {
            self.dock_maximized = false;
        }
        self.save_layout();
        cx.notify();
    }

    /// Give the dock the whole centre, or give the console back.
    ///
    /// The dragged height is untouched either way, so coming back lands on the
    /// height that was there before rather than on a default.
    pub fn toggle_dock_maximized(&mut self, cx: &mut Context<Self>) {
        self.dock_maximized = !self.dock_maximized;
        cx.notify();
    }

    /// Whether the dock is actually on screen, which is not the same question
    /// as whether its switch is on: a structure tab hides it regardless. The
    /// titlebar asks this rather than reading the field, so the control is
    /// never lit for a panel nobody can see.
    pub(crate) fn dock_visible(&self) -> bool {
        self.dock_open
            && !self
                .pane()
                .active()
                .is_some_and(|tab| tab.kind == CenterKind::Structure)
    }

    pub(crate) fn begin_drag(
        &mut self,
        target: DragTarget,
        origin: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let initial = match target {
            DragTarget::LeftPanel => self.left_width,
            DragTarget::RightPanel => self.right_width,
            DragTarget::BottomDock => self.dock_height,
            DragTarget::Seam { .. } => px(0.),
        };
        let initial_flex = match &target {
            DragTarget::Seam { path, index } => self
                .layout
                .group_at(path)
                .and_then(|group| group.flexes.get(*index).copied())
                .unwrap_or(0.5),
            _ => 0.,
        };
        self.drag = Some(Drag {
            target,
            origin,
            initial,
            initial_flex,
        });
        cx.notify();
    }

    fn update_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else { return };
        let min = cx.metrics().panel_min_width;
        match &drag.target {
            DragTarget::LeftPanel => {
                self.left_width = (drag.initial + position.x - drag.origin.x).clamp(min, px(640.));
            }
            DragTarget::RightPanel => {
                self.right_width =
                    (drag.initial - (position.x - drag.origin.x)).clamp(min, px(640.));
            }
            DragTarget::BottomDock => {
                self.dock_height =
                    (drag.initial - (position.y - drag.origin.y)).clamp(px(90.), px(900.));
            }
            DragTarget::Seam { path, index } => {
                let (path, index, from) = (path.clone(), *index, drag.initial_flex);
                let moved = match self.layout.group_at(&path).map(|group| group.layout) {
                    Some(Layout::Columns) => position.x - drag.origin.x,
                    Some(Layout::Rows) => position.y - drag.origin.y,
                    None => return,
                };
                // The seam moved *this much* of its group, which is the one
                // thing pixels cannot say on their own — hence the measured
                // extent from the last paint.
                let extent = self.group_extent(&path);
                if extent <= 0. {
                    return;
                }
                self.layout
                    .resize_to(&path, index, from + f32::from(moved) / extent);
            }
        }
        cx.notify();
    }

    /// How long a group's box came out at the last paint, along the axis its
    /// seams move in. Zero before the first paint, which is also the answer
    /// that makes a drag do nothing rather than something wild.
    fn group_extent(&self, path: &[usize]) -> f32 {
        self.group_boxes.borrow().get(path).copied().unwrap_or(0.)
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            // At the end of the drag, not during it: a splitter drag is
            // hundreds of frames, and none of the intermediate widths is one
            // anybody chose.
            self.save_layout();
            cx.notify();
        }
    }

    /// Write the panel geometry back to the settings table.
    fn save_layout(&self) {
        crate::layout::Layout {
            left_open: Some(self.left_open),
            right_open: Some(self.right_open),
            dock_open: Some(self.dock_open),
            left_width: Some(f32::from(self.left_width)),
            right_width: Some(f32::from(self.right_width)),
            dock_height: Some(f32::from(self.dock_height)),
        }
        .save(self.store.as_deref());
    }

    /// The full-window shield that swallows mouse events while a splitter is
    /// being dragged, so the pointer can leave the 6px handle without the drag
    /// snapping back.
    fn drag_shield(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        self.drag.as_ref()?;
        Some(
            div()
                .id("drag-shield")
                .absolute()
                .inset_0()
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    // A move with no button down means we missed the mouse-up
                    // (it happened outside the window); treat it as a release.
                    if event.pressed_button.is_none() {
                        this.end_drag(cx);
                    } else {
                        this.update_drag(event.position, cx);
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.end_drag(cx)),
                ),
        )
    }

    /// Which splitter, if any, is being dragged. Used by the centre stack to
    /// highlight its own handle without owning the drag state.
    pub(crate) fn current_drag_target(&self) -> Option<DragTarget> {
        self.drag.as_ref().map(|d| d.target.clone())
    }

    pub(crate) fn start_dock_drag(&mut self, origin: Point<Pixels>, cx: &mut Context<Self>) {
        self.begin_drag(DragTarget::BottomDock, origin, cx);
    }

    pub(crate) fn dragging(&self, target: DragTarget) -> bool {
        self.drag.as_ref().is_some_and(|d| d.target == target)
    }

    fn status_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let c = cx.colors();
        let session = self.session.as_ref().map(|session| session.read(cx));
        let state = session
            .map(|s| s.state().clone())
            .unwrap_or(SessionState::Offline);

        // The dot is the one thing on the bar that is read at a glance, so it
        // carries the state and nothing else does.
        let dot = match &state {
            SessionState::Connected => c.success,
            SessionState::Connecting => c.warning,
            SessionState::Failed(_) => c.danger,
            SessionState::Offline => c.text_disabled,
        };
        // Which server, not which build of it. Postgres reports itself as
        // "17.9 (Debian 17.9-1.pgdg13+1)" and the packaging half of that is
        // forty characters of status bar spent on something nobody has ever
        // needed to know at a glance. The connection's own name is not here
        // either: it is in the titlebar, and saying it twice makes the bar
        // look like it is reporting two different things.
        let server = match session {
            Some(session) => match session.snapshot.as_ref() {
                Some(snapshot) => format!("postgres {}", short_version(&snapshot.server_version)),
                None => state.label().to_string(),
            },
            None => "No connection".to_string(),
        };
        // Where you are, as a path: database, schema, and — when a table is
        // open — the table. The same shape as the reference's breadcrumb, and
        // the answer to the question the sidebar stops answering the moment it
        // is scrolled or hidden.
        let location = session
            .and_then(|session| session.snapshot.as_ref())
            .map(|snapshot| {
                let mut parts = vec![
                    snapshot.database.to_string(),
                    snapshot.current_schema.to_string(),
                ];
                if let Some(open) = self
                    .pane()
                    .active()
                    .filter(|tab| tab.kind == CenterKind::Table)
                    .and_then(|tab| tab.relation.as_ref())
                {
                    // A table in another schema replaces the one the session
                    // happens to be set to rather than being appended to it,
                    // which would spell out a path that does not exist.
                    parts[1] = open.schema.to_string();
                    parts.push(open.name.to_string());
                }
                parts
            })
            .unwrap_or_default();

        StatusBar::new()
            .start_child(
                h_flex()
                    .gap(px(5.))
                    .child(div().size(px(7.)).rounded_full().bg(dot))
                    .child(
                        Label::new(server)
                            .size(LabelSize::Small)
                            .color(IconColor::Muted),
                    ),
            )
            .start_child(
                h_flex()
                    .gap(px(5.))
                    .children(location.iter().enumerate().map(|(ix, part)| {
                        h_flex()
                            .gap(px(5.))
                            // The separators are dimmer than the names they
                            // separate: the path is what is being read, the
                            // slashes are only punctuation.
                            .when(ix > 0, |el| {
                                el.child(
                                    Label::new("/")
                                        .size(LabelSize::Small)
                                        .color(IconColor::Disabled),
                                )
                            })
                            .child(Label::new(part.clone()).size(LabelSize::Small).color(
                                match ix + 1 == location.len() {
                                    true => IconColor::Muted,
                                    false => IconColor::Subtle,
                                },
                            ))
                    })),
            )
            // The duration and the row count live under the grid, in the
            // dock's own footer, and are repeated here only when that footer
            // is not on screen — the dock closed, or a structure tab in front
            // of it. Both at once is how a window ends up stating the same two
            // facts three times and reading like a dashboard.
            .when(!self.results_showing(), |bar| {
                bar.end_child(
                    h_flex()
                        .gap(px(5.))
                        .child(
                            Icon::new(IconName::Clock)
                                .size(IconSize::XSmall)
                                .color(IconColor::Subtle),
                        )
                        .child(
                            // Em dash until something has actually run: an
                            // invented duration is worse than none.
                            Label::new(match self.pane().elapsed {
                                Some(d) => format_duration(d),
                                None => "—".into(),
                            })
                            .size(LabelSize::Small)
                            .color(IconColor::Muted),
                        ),
                )
                .end_child(
                    Label::new(match self.pane().affected {
                        // A statement that returned no rows reports what it
                        // changed instead of a row count of zero, which would
                        // read as "no matches" rather than "three rows
                        // updated".
                        Some(count) => format!("{} affected", count_of(count as usize, "row")),
                        None if self.pane().truncated => {
                            format!("{}+ rows", thousands(self.pane().row_count))
                        }
                        None => count_of(self.pane().row_count, "row"),
                    })
                    .size(LabelSize::Small)
                    .color(IconColor::Muted),
                )
            })
            // Real, and live: the status bar reads the editor's cursor rather
            // than repeating a number someone typed once. Gone while the panes
            // are collapsed to their tab strips, because then there is no
            // editor on screen and a caret position is a fact about something
            // nobody can see.
            .when(!self.collapsed(), |bar| {
                bar.end_child(
                    Label::new({
                        let at = self.pane().editor.read(cx).cursor_position();
                        format!("Ln {}, Col {}", at.row + 1, at.column + 1)
                    })
                    .size(LabelSize::Small)
                    .color(IconColor::Subtle),
                )
            })
    }
}

/// Which calendar day a unix-millisecond timestamp falls on, in the timezone
/// the machine is set to, as a day number since the epoch. Only differences
/// between these numbers are used, so the origin does not matter — what matters
/// is that two timestamps on the same local day produce the same number, which
/// is why this is not just `ms / 86_400_000`.
/// Which tab should be showing once the one at `closed` is gone, given that
/// `active` was showing and `remaining` tabs are left.
///
/// Closing a tab to the left of the active one shifts it down a place; closing
/// the active last tab falls back onto its new neighbour; closing anything to
/// the right leaves the active one where it is.
pub(crate) fn tab_after_close(active: usize, closed: usize, remaining: usize) -> usize {
    if active > closed || active >= remaining {
        active.saturating_sub(1)
    } else {
        active
    }
}

pub(crate) fn day_of(ms: i64) -> i64 {
    let seconds = ms.div_euclid(1000) + local_offset_seconds();
    seconds.div_euclid(86_400)
}

/// A timestamp as a log reads it: `14:03:27`, local time, no date. The date is
/// the same for every row anyone will ever scroll past in one session, and the
/// History tab is where a date actually matters.
pub(crate) fn format_clock(ms: i64) -> String {
    let seconds = ms.div_euclid(1000) + local_offset_seconds();
    let in_day = seconds.rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}",
        in_day / 3600,
        (in_day % 3600) / 60,
        in_day % 60
    )
}

/// Seconds east of UTC, as the OS currently has it, read once.
///
/// `time` will not give this up in a multi-threaded process — its local-offset
/// support is documented as unsound there and returns an error — and GPUI is
/// several threads deep before the first frame. `localtime_r` answers the same
/// question safely as long as the environment is not being mutated underneath
/// it, which is why it is read once at first use rather than per row.
fn local_offset_seconds() -> i64 {
    use std::sync::OnceLock;
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        // SAFETY: `localtime_r` writes into a `tm` this call owns, and takes a
        // `time_t` this call owns. Nothing else here touches the environment.
        unsafe {
            let now: libc::time_t = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&now, &mut tm).is_null() {
                return 0;
            }
            tm.tm_gmtoff as i64
        }
    })
}

/// How many history rows and saved queries the sidebar lists. Past this the
/// list is not something anyone scrolls — it is something they search.
const HISTORY_SHOWN: usize = 200;

/// How long history is kept: ninety days. Long enough to cover "what did I run
/// before the last release", short enough that the file never becomes a problem.
const HISTORY_KEPT_MS: i64 = 90 * 24 * 60 * 60 * 1000;

// How many rows opening a table from the tree asks for is
// `Settings::page_size` — large enough by default that most tables arrive
// whole and the count in the footer is the real one, small enough that clicking
// the wrong row in the sidebar does not pull a hundred million rows across the
// wire, and adjustable in Settings for the people either half of that is wrong
// for.

/// Unix milliseconds. The history table stores an integer rather than a
/// formatted date so it can be ordered and pruned without parsing anything.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The version number out of a Postgres `server_version` string, dropping the
/// packaging note the server volunteers after it. "17.9 (Debian 17.9-1.pgdg13+1)"
/// is "17.9"; anything that does not look like that is passed through, because
/// a string this code did not expect is better shown than silently emptied.
fn short_version(version: &str) -> &str {
    match version.split_whitespace().next() {
        Some(first) if !first.is_empty() => first,
        _ => version,
    }
}

/// Durations the way a query timer reads them: sub-millisecond work still
/// deserves a number, and nothing past a second needs three decimals.
pub(crate) fn format_duration(d: std::time::Duration) -> String {
    let ms = d.as_secs_f64() * 1000.;
    if ms >= 1000. {
        format!("{:.2} s", ms / 1000.)
    } else if ms >= 10. {
        format!("{:.0} ms", ms)
    } else {
        format!("{:.1} ms", ms)
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The editor is the thing you came here to type in, so it starts
        // focused. This is the first frame with a window to focus into.
        if !self.booted {
            self.booted = true;
            self.window = Some(window.window_handle());
            let handle = self.pane().editor.read(cx).focus().clone();
            window.focus(&handle, cx);
            self.boot_from_environment(cx);
        }

        // A sheet opened from an event handler had no window to focus into.
        // This is the first frame that does.
        if let Some(handle) = self.pending_focus.take() {
            window.focus(&handle, cx);
        }

        // The theme changed under us. Every element in the tree cached its
        // colours when it was built, so the whole window has to be rebuilt and
        // this is the only place that can say so.
        if std::mem::take(&mut self.pending_refresh) {
            window.refresh();
        }

        let c = cx.colors().clone();
        let hit = cx.metrics().splitter_hit_width;
        let left_width = self.left_width;
        let right_width = self.right_width;

        v_flex()
            .id("workspace")
            // Focusable so that the window-wide shortcuts have somewhere to
            // land when nothing else is focused. A child that focuses itself
            // on mouse-down prevents the default, so this never steals focus
            // from the console.
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            // The menu bar's other end. Registering the handlers here rather
            // than globally is what makes macOS grey the items out when the
            // settings window has focus: it asks whether anything in the
            // dispatch path would take the action, and with the workspace out
            // of the path the answer is no.
            .map(|el| self.menu_actions(el, cx))
            .relative()
            .size_full()
            .bg(c.background)
            .text_color(c.text)
            .font_family(cx.typography().ui_family.clone())
            .child({
                let session = self.session.as_ref().map(|session| session.read(cx));
                let database = session
                    .and_then(|session| session.snapshot.as_ref())
                    .map(|snapshot| snapshot.database.to_string())
                    .unwrap_or_else(|| "tupli".to_string());
                let name = session
                    .map(|session| session.config.display_name())
                    .unwrap_or_else(|| "no connection".into());
                let safety = session.map(|session| session.config.safety);
                let connected = session.is_some_and(|session| session.state().is_connected());
                Titlebar::new(name)
                    .database(database)
                    .connected(connected)
                    // The badge is the safety level, not an invented
                    // environment name: it is the thing that actually changes
                    // what the app will let you do.
                    .when_some(safety, |titlebar, safety| match safety {
                        db::SafetyLevel::Normal => titlebar,
                        db::SafetyLevel::Confirm => {
                            titlebar.environment("confirm", BadgeTone::Warning)
                        }
                        db::SafetyLevel::ReadOnly => {
                            titlebar.environment("read-only", BadgeTone::Danger)
                        }
                    })
                    .panels(self.left_open, self.dock_visible(), self.right_open)
                    .on_toggle_left(cx.listener(|this, _, _, cx| this.toggle_left_panel(cx)))
                    .on_toggle_bottom(cx.listener(|this, _, _, cx| this.toggle_bottom_dock(cx)))
                    .on_toggle_right(cx.listener(|this, _, _, cx| this.toggle_right_panel(cx)))
                    .on_open_switcher(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                        this.open_database_menu(event.position(), cx)
                    }))
                    .on_new_query(cx.listener(|this, _, _, cx| this.new_query_tab(cx)))
                    .on_new_connection(cx.listener(|this, _, _, cx| this.new_connection(cx)))
            })
            .child(
                // Edge to edge. The regions butt against one another and each
                // one draws the seam on whichever of its edges has a
                // neighbour, so a line is drawn once and by whoever owns it.
                h_flex()
                    .relative()
                    .flex_1()
                    .w_full()
                    .min_h_0()
                    .items_stretch()
                    // ---- left panel ------------------------------------
                    .when(self.left_open, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .w(left_width)
                                .h_full()
                                .child(self.render_sidebar(window, cx)),
                        )
                    })
                    // ---- centre stack ----------------------------------
                    .child(self.render_center(window, cx))
                    // ---- right panel -----------------------------------
                    .when(self.right_open, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .w(right_width)
                                .h_full()
                                .child(self.render_inspector(window, cx)),
                        )
                    })
                    // ---- splitters -------------------------------------
                    // Last, and placed on the seams rather than parented to
                    // the region on one side of them. A handle that belongs to
                    // the sidebar is painted before the centre stack is, and
                    // the half of its grab strip that hangs over the seam is
                    // buried — a six pixel target quietly becomes three.
                    .when(self.left_open, |el| {
                        el.child(
                            div()
                                .absolute()
                                .top_0()
                                .h_full()
                                .left(left_width - hit / 2.)
                                .w(hit)
                                .child(
                                    ResizeHandle::new("left-splitter", Axis::Vertical)
                                        .active(self.dragging(DragTarget::LeftPanel))
                                        .invisible_line()
                                        .on_drag_start(cx.listener(
                                            |this, e: &gpui::MouseDownEvent, _, cx| {
                                                this.begin_drag(
                                                    DragTarget::LeftPanel,
                                                    e.position,
                                                    cx,
                                                )
                                            },
                                        )),
                                ),
                        )
                    })
                    .when(self.right_open, |el| {
                        el.child(
                            div()
                                .absolute()
                                .top_0()
                                .h_full()
                                .right(right_width - hit / 2.)
                                .w(hit)
                                .child(
                                    ResizeHandle::new("right-splitter", Axis::Vertical)
                                        .active(self.dragging(DragTarget::RightPanel))
                                        .invisible_line()
                                        .on_drag_start(cx.listener(
                                            |this, e: &gpui::MouseDownEvent, _, cx| {
                                                this.begin_drag(
                                                    DragTarget::RightPanel,
                                                    e.position,
                                                    cx,
                                                )
                                            },
                                        )),
                                ),
                        )
                    }),
            )
            .child(self.status_bar(cx))
            .children(self.drag_shield(cx))
            // The sheets are last so they paint over everything, including the
            // drag shield.
            .children(self.save_sheet.clone())
            .children(self.export_sheet.clone())
            .children(self.import_sheet.clone())
            .children(self.palette.clone())
            .children(self.render_commit_preview(cx))
            .children(self.object_sheet.clone())
            .children(self.render_structure_preview(cx))
            // The menu paints over even the sheets, because it is the only
            // thing here that is anchored to a point the user just clicked.
            .children(self.render_object_menu(cx))
            .children(self.render_row_menu(cx))
            .children(self.render_database_menu(cx))
            .children(self.render_filter_menu(cx))
            .children(self.render_decoder_menu(cx))
    }
}

/// Which of the composer's two lists is open. They share one slot because
/// only one can be: the column menu is opened from the chip's left half and
/// the operator menu from its middle, and both close on a click anywhere else.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FilterMenu {
    Column,
    Op,
}

/// Which row a save lands on: the one that already has this name on this
/// connection, or `fallback` — the tab's own saved query, or a fresh id.
///
/// Saving under a name that is already taken replaces the query that has it
/// rather than leaving two rows with the same label, which is a list nobody can
/// use. The sheet says so before the button is pressed. The same name under a
/// *different* connection is a different query and is left alone.
pub(crate) fn id_for_save(
    saved: &[store::SavedQuery],
    name: &str,
    connection: Option<uuid::Uuid>,
    fallback: uuid::Uuid,
) -> uuid::Uuid {
    saved
        .iter()
        .find(|query| query.name == name && query.connection == connection)
        .map(|query| query.id)
        .unwrap_or(fallback)
}

/// `1 row`, `2 rows`. A status bar that says "1 rows" is a status bar nobody
/// proof-read, and the grid reports a count of one often enough — a lookup by
/// primary key — for it to be the first thing anyone notices.
pub(crate) fn count_of(n: usize, noun: &str) -> String {
    if n == 1 {
        return format!("1 {noun}");
    }
    // Enough English for the nouns this app counts: rows, columns, indexes,
    // foreign keys. A general pluraliser would be a library, and every word
    // that goes through here is one we chose.
    let plural = if noun.ends_with('x') || noun.ends_with('s') {
        format!("{noun}es")
    } else {
        format!("{noun}s")
    };
    format!("{} {plural}", thousands(n))
}

/// A name for a query nobody has named: the first few words of the statement,
/// which is almost always `select something from somewhere`. It is a starting
/// point in an editable field, not a guess anyone has to live with.
pub(crate) fn suggest_name(sql: &str) -> String {
    // A statement that opens with a comment has already been named by whoever
    // wrote it, and the clause before the first comma is the title — what
    // follows it is the qualifier the author added for themselves.
    let first = sql
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let name = match first.strip_prefix("--") {
        Some(comment) => {
            let clause = comment.split([',', ';', '.']).next().unwrap_or(comment);
            clip(clause.trim(), 48)
        }
        // Without one, the first few words of the statement: enough to
        // recognise in a list, and short enough to type over.
        None => {
            let line = crate::results::one_line(sql);
            let words: Vec<&str> = line.split_whitespace().take(6).collect();
            clip(&words.join(" "), 48)
        }
    };
    if name.is_empty() {
        "Untitled query".to_string()
    } else {
        name
    }
}

/// `text`, cut to at most `max` characters on a word boundary.
fn clip(text: &str, max: usize) -> String {
    let text = text.trim_matches(|c: char| c == ',' || c == ';');
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    for word in text.split_whitespace() {
        if out.chars().count() + word.chars().count() + 1 > max {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// `1284913` → `1,284,913`. Grid counts are read, not computed, so they get
/// separators; values inside cells never do.
pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Access to the workspace entity from nested components.
pub type WorkspaceHandle = Entity<Workspace>;

/// Reads a `usize` from the environment, ignoring anything unparseable rather
/// than failing a launch over a typo in a benchmark flag.
fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key).ok()?.replace('_', "").parse().ok()
}

/// One cell as display text, or `None` for SQL NULL.
///
/// The grid itself never does this — it paints straight out of the column
/// buffer — but a panel showing one value at a time can afford the allocation.
pub fn cell_text(column: &db::Column, row: usize) -> Option<String> {
    let mut scratch = String::new();
    match column.render(row, &mut scratch) {
        db::CellText::Null => None,
        db::CellText::Borrowed(text) => Some(text.to_string()),
        db::CellText::Formatted => Some(scratch),
    }
}

#[cfg(test)]
mod tests {
    use super::{count_of, id_for_save, short_version, suggest_name, tab_after_close};
    use uuid::Uuid;

    #[test]
    fn closing_a_tab_to_the_left_shifts_the_active_one_down() {
        assert_eq!(tab_after_close(2, 0, 2), 1);
    }

    #[test]
    fn closing_a_tab_to_the_right_leaves_the_active_one_alone() {
        assert_eq!(tab_after_close(0, 2, 2), 0);
    }

    #[test]
    fn closing_the_active_last_tab_falls_back_onto_its_neighbour() {
        assert_eq!(tab_after_close(2, 2, 2), 1);
    }

    #[test]
    fn closing_the_active_tab_in_the_middle_keeps_the_position() {
        // The tab that was to its right slides into the same slot, which is
        // what every other editor does.
        assert_eq!(tab_after_close(1, 1, 2), 1);
    }

    #[test]
    fn a_count_of_one_is_singular() {
        assert_eq!(count_of(1, "row"), "1 row");
        assert_eq!(count_of(0, "row"), "0 rows");
        assert_eq!(count_of(20_000, "row"), "20,000 rows");
    }

    #[test]
    fn a_noun_ending_in_x_takes_es() {
        assert_eq!(count_of(1, "index"), "1 index");
        assert_eq!(count_of(3, "index"), "3 indexes");
        assert_eq!(count_of(2, "foreign key"), "2 foreign keys");
    }

    #[test]
    fn an_unnamed_query_is_named_after_its_first_words() {
        assert_eq!(
            suggest_name("select id, email\n  from users\n where id = 1"),
            "select id, email from users where"
        );
        assert_eq!(suggest_name("   \n  "), "Untitled query");
    }

    #[test]
    fn a_second_save_under_the_same_name_replaces_the_first() {
        let mine = Uuid::new_v4();
        let theirs = Uuid::new_v4();
        let existing = store::SavedQuery::new("Revenue", "select 1", 0).for_connection(mine);
        let saved = vec![existing.clone()];
        let fresh = Uuid::new_v4();

        assert_eq!(
            id_for_save(&saved, "Revenue", Some(mine), fresh),
            existing.id,
            "the same name on the same connection is the same query"
        );
        assert_eq!(
            id_for_save(&saved, "Revenue", Some(theirs), fresh),
            fresh,
            "the same name on another connection is another query"
        );
        assert_eq!(
            id_for_save(&saved, "Revenue", None, fresh),
            fresh,
            "an unattached query does not replace a connection's own"
        );
        assert_eq!(id_for_save(&saved, "Costs", Some(mine), fresh), fresh);
    }

    #[test]
    fn a_statement_that_opens_with_a_comment_is_named_after_it() {
        assert_eq!(
            suggest_name("-- Monthly recurring revenue by plan\nselect 1"),
            "Monthly recurring revenue by plan"
        );
        assert_eq!(
            suggest_name("-- Monthly recurring revenue by plan, current month\nselect 1"),
            "Monthly recurring revenue by plan",
            "the qualifier after the comma is not part of the title"
        );
        assert_eq!(
            suggest_name(&format!("-- {}\nselect 1", "word ".repeat(20))),
            "word word word word word word word word word",
            "a comment that runs on is cut on a word boundary"
        );
    }

    #[test]
    fn the_status_bar_reports_a_version_not_a_package() {
        assert_eq!(short_version("17.9 (Debian 17.9-1.pgdg13+1)"), "17.9");
        assert_eq!(short_version("16.2"), "16.2");
        assert_eq!(
            short_version("something unexpected"),
            "something",
            "a string in an unknown shape still yields its first word"
        );
        assert_eq!(short_version(""), "", "and an empty one stays empty");
    }
}
