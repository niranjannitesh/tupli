//! The left panel: schema tree, saved queries, history.
//!
//! Three tabs over one column, because a database client's left edge is always
//! "where things live" and splitting it into separate panels would waste the
//! narrowest, most contested space in the window.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::{div, prelude::*, px, Context, IntoElement, ParentElement, Pixels, Point, Window};
use ui::{
    region, v_flex, ActiveTheme, Button, ButtonSize, ButtonVariant, ContextMenu, Disclosure,
    EmptyState, IconColor, IconName, IconSize, Label, LabelSize, ListItem, MenuItem, SectionHeader,
    Segmented, Sheet, Spinner, Tab, TabBar, Toolbar, Tooltip,
};

use crate::session::Activity;
use crate::tree::{NodeKind, Target, TreeNode};
use crate::workspace::{day_of, format_duration, now_ms, HistoryScope, SidebarTab, Workspace};

impl Workspace {
    pub(crate) fn render_sidebar(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tab = self.sidebar_tab;

        region(cx)
            .size_full()
            // The seam to the centre stack. Drawn here rather than by the
            // splitter that sits on it, because the splitter is a grab strip
            // that only shows itself while it is being used.
            .border_r_1()
            .border_color(cx.colors().border)
            .child(
                TabBar::new("sidebar-tabs")
                    .tab(
                        Tab::new("sidebar-db", "Database")
                            .fill()
                            .icon(IconName::ListTree)
                            .active(tab == SidebarTab::Database)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar_tab = SidebarTab::Database;
                                cx.notify();
                            })),
                    )
                    .tab(
                        Tab::new("sidebar-queries", "Queries")
                            .fill()
                            .icon(IconName::Bookmark)
                            .active(tab == SidebarTab::Queries)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar_tab = SidebarTab::Queries;
                                this.reload_lists(cx);
                                cx.notify();
                            })),
                    )
                    .tab(
                        Tab::new("sidebar-history", "History")
                            .fill()
                            .icon(IconName::History)
                            .active(tab == SidebarTab::History)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar_tab = SidebarTab::History;
                                this.reload_lists(cx);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                Toolbar::new("sidebar-toolbar")
                    .transparent()
                    .borderless()
                    // The centre slot, not the leading one: this field is the
                    // thing that should take the width the buttons do not.
                    .center_child(div().flex_1().min_w_0().child(self.tree_filter.clone()))
                    .end_child(
                        Button::icon("sidebar-add", IconName::Plus)
                            .size(ButtonSize::XSmall)
                            .tooltip(Tooltip::key("New Connection", "⌘N"))
                            .on_click(cx.listener(|this, _, _, cx| this.new_connection(cx))),
                    )
                    .end_child(
                        Button::icon("sidebar-refresh", IconName::Refresh)
                            .size(ButtonSize::XSmall)
                            .tooltip(Tooltip::key("Refresh Schema", "⇧⌘R"))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_schema(cx))),
                    ),
            )
            // The scope switch sits above the scroll rather than in it: it is
            // what decides the list, and a control that scrolls away with the
            // list it governs is one nobody finds twice.
            .when(tab == SidebarTab::History, |panel| {
                panel.child(self.render_history_scope(cx))
            })
            .child(
                v_flex()
                    .id("sidebar-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .py(px(4.))
                    .children(match tab {
                        SidebarTab::Database => self.render_database_tab(cx),
                        SidebarTab::Queries => self.render_query_list(cx),
                        SidebarTab::History => self.render_history(cx),
                    }),
            )
    }

    /// Every saved connection, each opened into its own tree.
    ///
    /// One list and not two. The sidebar used to draw the saved connections
    /// itself and splice the tree in where the open one was, which made the
    /// open connection a special case in a list of ordinary ones — and there
    /// is more than one of them open now. The tree carries every connection,
    /// so a connection with nothing under it is a row with no disclosure
    /// triangle and nothing more to explain.
    fn render_database_tab(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        if self.connections.is_empty() && self.session.is_none() {
            return self.render_connections(cx);
        }
        let mut rows = self.render_tree(cx);
        if rows.is_empty() {
            rows.push(no_matches());
        }
        rows
    }

    fn render_tree(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let selected = self.selected_node;
        // The tag a connection was given is the whole point of tagging it, so
        // it stays on the row once the connection is open — a red server that
        // turns accent-blue the moment you connect to it is a guardrail that
        // works only while you do not need it. Looked up per row, because
        // there is more than one server on screen and the guardrail is worth
        // nothing if every root wears the colour of the one in front.
        let mut tints: HashMap<uuid::Uuid, Option<gpui::Hsla>> = HashMap::new();
        for config in &self.connections {
            tints.insert(config.id, crate::tint::tint(config.color, cx));
        }
        for session in &self.sessions {
            let config = &session.read(cx).config;
            tints
                .entry(config.id)
                .or_insert_with(|| crate::tint::tint(config.color, cx));
        }
        // Which servers are being dialled, gathered once rather than asked per
        // row: the answer is the same for every row and the tree is long.
        let connecting: HashSet<uuid::Uuid> = self
            .connections
            .iter()
            .map(|config| config.id)
            .chain(self.sessions.iter().map(|s| s.read(cx).config.id))
            .filter(|id| self.connection_connecting(*id, cx))
            .collect();
        let query = self.tree_filter.read(cx).text(cx);
        let query = query.trim();
        // While filtering, the disclosure state is ignored: a match five levels
        // down is useless if the branch above it is still closed.
        let keep = (!query.is_empty()).then(|| self.matching_nodes(query));

        // A collapsed node hides its whole subtree: skip forward while the depth
        // stays greater than the collapsed node's.
        let mut rows = Vec::new();
        let mut skip_below: Option<usize> = None;

        for node in &self.tree {
            if let Some(keep) = &keep {
                if !keep.contains(&node.id) {
                    continue;
                }
            } else {
                if let Some(depth) = skip_below {
                    if node.depth > depth {
                        continue;
                    }
                    skip_below = None;
                }
                if self.collapsed.contains(&node.id) && node.expandable {
                    skip_below = Some(node.depth);
                }
            }
            let collapsed = keep.is_none() && self.collapsed.contains(&node.id);

            let id = node.id;
            let (icon, mut color) = icon_for(node, cx);
            if node.kind == NodeKind::Connection {
                if let Some(Some(tint)) = tints.get(&node.origin.connection) {
                    color = IconColor::Custom(*tint);
                }
            }
            rows.push(
                ListItem::new(("tree", id), node.name.clone())
                    // Tables in one schema are named to a convention and a
                    // narrow panel elides them all to the same word; the end
                    // of the name is what tells them apart, so the end is what
                    // stays.
                    .elide_middle()
                    .indent(node.depth)
                    .disclosure(if node.expandable {
                        if collapsed {
                            Disclosure::Collapsed
                        } else {
                            Disclosure::Expanded
                        }
                    } else {
                        Disclosure::Leaf
                    })
                    .icon(icon)
                    .icon_color(color)
                    .selected(selected == Some(id))
                    .when(
                        node.kind == NodeKind::Connection
                            && connecting.contains(&node.origin.connection),
                        |el| {
                            el.end_child(
                                Spinner::new(("tree-connecting", id)).size(IconSize::XSmall),
                            )
                        },
                    )
                    .when_some(node.meta.clone(), |el, meta| el.meta(meta))
                    .on_toggle(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(&id) {
                            this.collapsed.insert(id);
                        }
                        cx.notify();
                    }))
                    // Right-click puts the object's own verbs under the
                    // pointer. Selecting first, so the menu is unambiguously
                    // about the row it came out of even when the click landed
                    // somewhere other than the current selection.
                    .on_secondary_click(cx.listener(
                        move |this, event: &gpui::ClickEvent, _, cx| {
                            this.selected_node = Some(id);
                            let node = this.tree.iter().find(|node| node.id == id).cloned();
                            match node {
                                // A connection row's verbs are about the
                                // server rather than about anything in it, and
                                // are the same four whether the row is still a
                                // door or has a tree under it.
                                Some(node) if node.kind == NodeKind::Connection => this
                                    .open_connection_menu(
                                        node.origin.connection,
                                        event.position(),
                                        cx,
                                    ),
                                // Below that, only a relation has a menu: its
                                // verbs are DDL, and there is no `rename` or
                                // `truncate` to put under a key.
                                Some(node) => {
                                    if let Some(relation) =
                                        node.target.as_ref().and_then(Target::relation).cloned()
                                    {
                                        this.open_object_menu(relation, event.position(), cx);
                                    }
                                }
                                None => {}
                            }
                            cx.notify();
                        },
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_node = Some(id);
                        // A relation opens on the first click, the way every
                        // other database client does it. The grouping rows only
                        // select, because there is nothing to open.
                        let node = this.tree.iter().find(|node| node.id == id).cloned();
                        match node {
                            // A connection with nothing under it is a door:
                            // the click is the one that connects. So is one
                            // whose last attempt failed — the row still has
                            // yesterday's databases under it, and without this
                            // there is nothing anywhere that tries again.
                            Some(node)
                                if node.kind == NodeKind::Connection
                                    && (!node.expandable
                                        || this.connection_failed(node.origin.connection, cx)) =>
                            {
                                let config = this
                                    .connections
                                    .iter()
                                    .find(|config| config.id == node.origin.connection)
                                    .cloned();
                                if let Some(config) = config {
                                    this.open_connection(config, cx);
                                }
                            }
                            Some(node) if node.kind == NodeKind::Database => {
                                // The one already open is the row you clicked
                                // to select it; `open_database` knows that and
                                // does nothing.
                                this.focus_origin(&node.origin, cx);
                                this.open_database(&node.name.clone(), cx);
                            }
                            Some(node) => match node.target.clone() {
                                Some(target) => {
                                    // Which server this row came out of, before
                                    // anything is opened on it: the tab about to
                                    // be made binds to whatever the window is
                                    // describing.
                                    this.focus_origin(&node.origin, cx);
                                    this.open_target(&target, cx)
                                }
                                // A schema or a folder has nothing to open, so
                                // the click does the only thing it could have
                                // meant: opens the row. Aiming for the eight
                                // pixels of chevron is not a skill this app
                                // should be asking anybody to have.
                                None if node.expandable => {
                                    if !this.collapsed.remove(&id) {
                                        this.collapsed.insert(id);
                                    }
                                }
                                None => {}
                            },
                            None => {}
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }

        rows
    }

    /// The saved connection list, shown in place of the tree until something is
    /// open. This is the app's front door: on a first launch it is the only
    /// thing in the window with anything to say.
    fn render_connections(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let connecting = self.session.as_ref().and_then(|session| {
            let session = session.read(cx);
            (session.activity() == Activity::Connecting).then_some(session.config.id)
        });

        if self.connections.is_empty() {
            return vec![div()
                .p(px(12.))
                // The scrolling body sizes to its content, so the state cannot
                // fill the panel and centre in it; this is the gap that keeps
                // it from sitting directly under the filter field instead.
                .pt(px(56.))
                .child(
                    EmptyState::new(IconName::Plug, "No connections")
                        .description("Add a Postgres server to get started.")
                        .action(
                            Button::new("empty-add", "New Connection…")
                                .variant(ButtonVariant::Accent)
                                .size(ButtonSize::Small)
                                .on_click(cx.listener(|this, _, _, cx| this.new_connection(cx))),
                        ),
                )
                .into_any_element()];
        }

        let mut rows = vec![SectionHeader::new("Connections").into_any_element()];
        for (index, config) in self.connections.iter().enumerate() {
            let config = config.clone();
            let id = config.id;
            let tint = crate::tint::tint(config.color, cx);
            rows.push(
                ListItem::new(("connection", index), config.display_name())
                    .icon(IconName::Plug)
                    .icon_color(match tint {
                        Some(tint) => IconColor::Custom(tint),
                        None => IconColor::Muted,
                    })
                    .meta(config.endpoint())
                    // The spinner is the whole answer to "did my click land":
                    // the list stays up for the length of the connect, so
                    // without it the front door does nothing visible until the
                    // tree replaces it.
                    .when(connecting == Some(config.id), |el| {
                        el.end_child(Spinner::new(("connecting", index)).size(IconSize::XSmall))
                    })
                    .on_secondary_click(cx.listener(
                        move |this, event: &gpui::ClickEvent, _, cx| {
                            this.open_connection_menu(id, event.position(), cx);
                        },
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if connecting.is_none() {
                            this.open_connection(config.clone(), cx);
                        }
                    }))
                    .into_any_element(),
            );
        }
        rows
    }

    /// Right-click on a connection row, wherever the row is drawn.
    pub fn open_connection_menu(
        &mut self,
        id: uuid::Uuid,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        self.row_menu = None;
        self.tab_menu = None;
        self.connection_menu = Some((at, id));
        cx.notify();
    }

    pub(crate) fn close_connection_menu(&mut self, cx: &mut Context<Self>) {
        if self.connection_menu.take().is_some() {
            cx.notify();
        }
    }

    /// The connection row's four verbs.
    ///
    /// Connect and Disconnect are the pair the sidebar had no gesture for at
    /// all: a click on a closed row connects, and until this there was nothing
    /// anywhere that closed one again. Edit and Remove are here because this
    /// is where the saved connection is on screen, and walking to Settings to
    /// change a port you are looking at is the kind of trip an app makes you
    /// take once.
    pub(crate) fn render_connection_menu(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let (at, id) = self.connection_menu?;
        // A row the saved list has never heard of — `TUPLI_CONNECT`, or a
        // connection deleted while it was open. It has a session, so it can be
        // closed; there is no record behind it to edit or remove.
        let config = self.connections.iter().find(|c| c.id == id).cloned();
        let open = self.sessions.iter().any(|s| s.read(cx).config.id == id);
        let connecting = self.connection_connecting(id, cx);
        // A server that is already up has nothing to connect. One whose last
        // attempt failed has, which is why this asks about the state of the
        // session rather than about whether there is one.
        let failed = self.connection_failed(id, cx);

        let connect = config.clone();
        let edit = config.clone();

        Some(
            ContextMenu::new("connection-menu")
                .at(at)
                .width(px(216.))
                .on_dismiss(cx.listener(|this, _, _, cx| this.close_connection_menu(cx)))
                .item(
                    MenuItem::new("Connect")
                        .icon(IconName::Plug)
                        .disabled(connect.is_none() || connecting || (open && !failed))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_connection_menu(cx);
                            if let Some(config) = connect.clone() {
                                this.open_connection(config, cx);
                            }
                        })),
                )
                .item(
                    MenuItem::new("Disconnect")
                        .icon(IconName::CircleXmark)
                        .disabled(!open)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_connection_menu(cx);
                            this.close_sessions_on(id, cx);
                        })),
                )
                .separator()
                .item(
                    MenuItem::new("Edit…")
                        .icon(IconName::Pen)
                        .disabled(edit.is_none())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_connection_menu(cx);
                            if let Some(config) = edit.clone() {
                                this.edit_connection(config, cx);
                            }
                        })),
                )
                .item(
                    MenuItem::new("Remove…")
                        .icon(IconName::Trash)
                        .danger()
                        .disabled(config.is_none())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_connection_menu(cx);
                            this.prompt_remove_connection(id, cx);
                        })),
                ),
        )
    }

    /// Ask about removing one saved connection.
    pub fn prompt_remove_connection(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        self.removing_connection = Some(id);
        cx.notify();
    }

    /// The sheet Remove asks through.
    ///
    /// Nothing here reaches the server and nothing is lost that cannot be
    /// typed again, so this is one button rather than the name-typing the drop
    /// sheet asks for — but a saved connection is a thing you set up once and
    /// then never think about again, and a menu item one row under Edit is
    /// close enough to it to be worth a question.
    pub(crate) fn render_remove_connection(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let id = self.removing_connection?;
        let config = self.connections.iter().find(|c| c.id == id)?;
        let (name, endpoint) = (config.display_name(), config.endpoint());

        Some(
            Sheet::new("remove-connection", format!("Remove “{name}”?"))
                .subtitle(endpoint)
                .width(px(420.))
                .child(
                    Label::new(
                        "The server is not touched. This forgets how to reach it, and closes \
                         whatever is open on it.",
                    )
                    .size(LabelSize::Small)
                    .color(IconColor::Muted)
                    .wrap(),
                )
                .on_dismiss(cx.listener(|this, _, _, cx| this.cancel_remove_connection(cx)))
                .footer_end(
                    Button::new("remove-connection-cancel", "Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_remove_connection(cx))),
                )
                .footer_end(
                    Button::new("remove-connection-confirm", "Remove")
                        .variant(ButtonVariant::Danger)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.removing_connection = None;
                            this.delete_connection(id, cx);
                        })),
                ),
        )
    }

    fn cancel_remove_connection(&mut self, cx: &mut Context<Self>) {
        self.removing_connection = None;
        cx.notify();
    }

    /// Ids to show for `query`: every node whose name contains it, plus the
    /// ancestors needed to reach them. Depth is enough to recover parentage
    /// because the tree is stored flattened in visit order.
    fn matching_nodes(&self, query: &str) -> HashSet<usize> {
        let needle = query.to_lowercase();
        let mut keep = HashSet::new();
        let mut ancestors: Vec<usize> = Vec::new();
        for node in &self.tree {
            ancestors.truncate(node.depth);
            ancestors.push(node.id);
            if node.name.to_lowercase().contains(&needle) {
                keep.extend(ancestors.iter().copied());
            }
        }
        keep
    }

    fn render_query_list(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        if self.saved.is_empty() {
            return vec![div()
                .p(px(12.))
                // The scrolling body sizes to its content, so the state cannot
                // fill the panel and centre in it; this is the gap that keeps
                // it from sitting directly under the filter field instead.
                .pt(px(56.))
                .child(
                    EmptyState::new(IconName::Code, "No saved queries")
                        .description("Statements you save appear here."),
                )
                .into_any_element()];
        }

        let mut rows = vec![SectionHeader::new("Saved").into_any_element()];
        let open = self.pane().active().and_then(|tab| tab.saved_query);
        for (i, query) in self.saved.iter().enumerate() {
            let mut item = ListItem::new(("query", i), query.name.clone())
                .icon(IconName::Code)
                // The row for the query the active tab is editing, so ⌘S has a
                // visible destination.
                .selected(open == Some(query.id));
            // A query saved with nothing connected applies to every server, and
            // the list says so rather than looking like a row that lost its
            // connection.
            if query.connection.is_none() {
                item = item.meta("any");
            }
            rows.push(
                item.end_child(
                    Button::icon(("query-delete", i), IconName::Trash)
                        .size(ButtonSize::XSmall)
                        .tooltip(Tooltip::text("Delete Saved Query"))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.delete_saved_query(i, cx)),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.load_saved_query(i, cx)))
                .into_any_element(),
            );
        }
        rows
    }

    /// The scope switch above the History list.
    ///
    /// The log is durable and shared between windows, which is the whole
    /// reason it is worth keeping and also the reason "what have I just been
    /// doing" got hard to see in it once commits, imports and exports moved in
    /// beside the statements. Everything is the default, because a window that
    /// has run nothing yet would otherwise open on an empty list and look
    /// broken.
    fn render_history_scope(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(6.))
            .border_b_1()
            .border_color(cx.colors().border)
            .child(
                Segmented::new("history-scope", ["Everything", "This window"])
                    .no_wrap()
                    .selected(match self.history_scope {
                        HistoryScope::Everything => 0,
                        HistoryScope::ThisWindow => 1,
                    })
                    .on_select({
                        let workspace = cx.entity();
                        move |index, _, cx| {
                            workspace.update(cx, |this, cx| {
                                this.history_scope = match index {
                                    0 => HistoryScope::Everything,
                                    _ => HistoryScope::ThisWindow,
                                };
                                cx.notify();
                            });
                        }
                    }),
            )
    }

    /// The History tab.
    ///
    /// Grouped by day, newest first, because "what did I run this morning" is
    /// the question this list exists to answer and a flat list of two hundred
    /// timestamps answers it badly.
    fn render_history(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let c = cx.colors().clone();

        let shown: Vec<&store::HistoryEntry> = self
            .history
            .iter()
            .filter(|entry| match self.history_scope {
                HistoryScope::Everything => true,
                HistoryScope::ThisWindow => self.history_mine.contains(&entry.id),
            })
            .collect();

        if shown.is_empty() {
            let (title, description) = match self.history_scope {
                HistoryScope::ThisWindow => (
                    "Nothing yet in this window",
                    "Statements, commits, imports and exports show up here as you make them.",
                ),
                HistoryScope::Everything => (
                    "No history yet",
                    "Every statement you run is recorded here, along with every commit, import and export.",
                ),
            };
            return vec![div()
                .p(px(12.))
                // The scrolling body sizes to its content, so the state cannot
                // fill the panel and centre in it; this is the gap that keeps
                // it from sitting directly under the filter field instead.
                .pt(px(56.))
                .child(EmptyState::new(IconName::History, title).description(description))
                .into_any_element()];
        }

        // Only worth saying which connection a row belongs to when the list
        // holds more than one — which it does exactly when no tab is open, and
        // then it is the only thing telling two identical statements apart.
        let connections: HashSet<_> = shown.iter().map(|entry| entry.connection).collect();
        let name_them = connections.len() > 1;

        let today = day_of(now_ms());
        let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(shown.len() + 4);
        let mut current_day = None;

        for (i, entry) in shown.iter().enumerate() {
            let day = day_of(entry.started_at);
            if current_day != Some(day) {
                current_day = Some(day);
                rows.push(
                    SectionHeader::new(match today - day {
                        0 => "Today".to_string(),
                        1 => "Yesterday".to_string(),
                        n if n < 7 => format!("{n} days ago"),
                        n => format!("{} weeks ago", n / 7),
                    })
                    .into_any_element(),
                );
            }

            // A statement still in flight has no outcome to draw yet, and a
            // clock is the difference between one that is taking a while and
            // one that came back instantly.
            let (icon, tint) = match (entry.pending(), entry.outcome) {
                (true, _) => (IconName::Clock, c.text_muted),
                (_, store::Outcome::Ok) => (IconName::CircleCheck, c.success),
                (_, store::Outcome::Failed) => (IconName::CircleXmark, c.danger),
                (_, store::Outcome::Canceled) => (IconName::Ban, c.warning),
            };
            let sql = entry.sql.clone();
            rows.push(
                ListItem::new(("history", i), entry.one_line())
                    .mono()
                    .height(px(26.))
                    .icon(icon)
                    .icon_color(IconColor::Custom(tint))
                    // An em dash for a statement that never reported back, so a
                    // hung query is visibly different from an instant one.
                    .meta(match entry.duration_ms {
                        Some(ms) => format_duration(Duration::from_millis(ms.max(0) as u64)),
                        None => "—".to_string(),
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.load_sql(&sql, cx)))
                    .into_any_element(),
            );

            // Under the row rather than beside it: what a failure said and
            // what the server volunteered are both about the statement above
            // them, and a log of either detached from what provoked it is
            // unreadable.
            rows.extend(self.render_history_aside(entry, name_them, cx));
        }
        rows
    }

    /// The second line, when there is one: which connection, what went wrong,
    /// and anything the server said on the side.
    fn render_history_aside(
        &self,
        entry: &store::HistoryEntry,
        name_connection: bool,
        cx: &Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let c = cx.colors().clone();
        let mut lines: Vec<gpui::AnyElement> = Vec::new();

        if name_connection {
            let name = entry
                .connection
                .and_then(|id| self.connections.iter().find(|config| config.id == id))
                .map(|config| config.name.clone())
                .unwrap_or_else(|| "unsaved".to_string());
            lines.push(
                Label::new(name)
                    .size(LabelSize::Small)
                    .color(IconColor::Subtle)
                    .into_any_element(),
            );
        }
        if let Some(error) = entry.error.as_deref() {
            lines.push(
                Label::new(error.to_string())
                    .size(LabelSize::Small)
                    .color(IconColor::Custom(c.danger))
                    .wrap()
                    .into_any_element(),
            );
        }
        // The severity is the server's own word for it, which is worth more
        // than a rank this app would have had to invent.
        lines.extend(entry.notices.iter().map(|notice| {
            Label::new(notice.clone())
                .size(LabelSize::Small)
                .color(match notice.starts_with("WARNING") {
                    true => IconColor::Custom(c.warning),
                    false => IconColor::Muted,
                })
                .wrap()
                .into_any_element()
        }));

        if lines.is_empty() {
            return Vec::new();
        }
        vec![v_flex()
            .w_full()
            .gap(px(1.))
            .pb(px(4.))
            // Indented to where the row above starts its text, so the aside
            // hangs off the statement rather than starting a new column.
            .pl(px(28.))
            .pr(px(10.))
            .children(lines)
            .into_any_element()]
    }
}

fn no_matches() -> gpui::AnyElement {
    div()
        .px(px(10.))
        .py(px(6.))
        .child(
            Label::new("No matches")
                .size(LabelSize::Small)
                .color(IconColor::Subtle),
        )
        .into_any_element()
}

/// Node kind → glyph. Colour carries the category so a long tree can be scanned
/// without reading: blue for containers, amber for tables, green for views.
///
/// A key takes its glyph from what it holds rather than from its kind, which is
/// the one thing a keyspace has instead of a schema: every row is a key, and
/// the only structure on offer is the difference between a hash and a list.
/// The colours are the syntax palette's, so they are already six hues a theme
/// has committed to keeping apart, in a theme that is already showing them next
/// to each other in the console.
fn icon_for(node: &TreeNode, cx: &Context<Workspace>) -> (IconName, IconColor) {
    let c = cx.colors();
    if let Some((_, kind)) = node.target.as_ref().and_then(Target::key) {
        let syntax = cx.syntax();
        return match kind {
            db::KeyType::String => (IconName::TextA, IconColor::Custom(syntax.string)),
            db::KeyType::List => (IconName::BulletList, IconColor::Custom(syntax.number)),
            db::KeyType::Hash => (IconName::BracketsCurly, IconColor::Custom(syntax.keyword)),
            db::KeyType::Set => (IconName::Layers, IconColor::Custom(syntax.type_name)),
            db::KeyType::SortedSet => (IconName::SortAsc, IconColor::Custom(syntax.function)),
            db::KeyType::Stream => (IconName::History, IconColor::Custom(c.info)),
            // A module type — RedisJSON, a time series, something this build
            // has never heard of. It is still a key and still has a name.
            db::KeyType::Other(_) => (IconName::File, IconColor::Muted),
        };
    }
    match node.kind {
        NodeKind::Connection => (IconName::Plug, IconColor::Custom(c.accent)),
        NodeKind::Database => (IconName::Database, IconColor::Custom(c.accent)),
        NodeKind::SchemaGroup | NodeKind::TableGroup | NodeKind::FunctionGroup => {
            (IconName::Folder, IconColor::Subtle)
        }
        NodeKind::Schema => (IconName::Layers, IconColor::Muted),
        NodeKind::Table => (IconName::Table, IconColor::Custom(c.warning)),
        NodeKind::View => (IconName::Eye, IconColor::Custom(c.success)),
        NodeKind::MaterializedView => (IconName::EyeFilled, IconColor::Custom(c.success)),
        NodeKind::Function => (IconName::BracketsCurly, IconColor::Muted),
        NodeKind::RoleGroup => (IconName::Users, IconColor::Subtle),
        NodeKind::Role => (IconName::User, IconColor::Muted),
        NodeKind::RoleGroupMember => (IconName::Users, IconColor::Muted),
        NodeKind::KeyDatabase => (IconName::Database, IconColor::Custom(c.accent)),
        NodeKind::KeyFolder => (IconName::Folder, IconColor::Subtle),
        // A key row with no target: nothing builds one, but the tree is data
        // and this is the honest glyph for a key nobody can open.
        NodeKind::Key => (IconName::Key, IconColor::Muted),
    }
}
