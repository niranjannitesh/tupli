//! The left panel: schema tree, saved queries, history.
//!
//! Three tabs over one column, because a database client's left edge is always
//! "where things live" and splitting it into separate panels would waste the
//! narrowest, most contested space in the window.

use std::collections::HashSet;
use std::time::Duration;

use gpui::{div, prelude::*, px, Context, IntoElement, ParentElement, Window};
use ui::{
    region, v_flex, ActiveTheme, Button, ButtonSize, ButtonVariant, Disclosure, EmptyState,
    IconColor, IconName, Label, LabelSize, ListItem, SectionHeader, Tab, TabBar, Toolbar, Tooltip,
};

use crate::session::Activity;
use crate::tree::NodeKind;
use crate::workspace::{day_of, format_duration, now_ms, SidebarTab, Workspace};

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

    /// Every saved connection, with the one the window is on opened into its
    /// tree.
    ///
    /// The other connections stay on screen while one is open. Hiding them was
    /// the sidebar saying a connection stops existing the moment you use
    /// another one, and the only way back to the second server was Settings.
    fn render_database_tab(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        if self.connections.is_empty() && self.session.is_none() {
            return self.render_connections(cx);
        }
        let open = self
            .session
            .as_ref()
            .map(|session| session.read(cx).config.id);
        // A session the saved list has never heard of — `TUPLI_CONNECT`, or a
        // connection deleted while it was open. It is still what the window is
        // showing, so it goes at the top rather than nowhere.
        let adopted = open.is_some_and(|id| self.connections.iter().any(|c| c.id == id));
        let mut rows = Vec::new();
        if !adopted && self.session.is_some() {
            rows.extend(self.render_tree(cx));
        }
        let connecting = self
            .session
            .as_ref()
            .is_some_and(|session| session.read(cx).activity() == Activity::Connecting);
        for (index, config) in self.connections.iter().enumerate() {
            if adopted && Some(config.id) == open {
                rows.extend(self.render_tree(cx));
            } else {
                rows.push(self.render_saved_connection(index, config, connecting, cx));
            }
        }
        if rows.is_empty() {
            rows.push(no_matches());
        }
        rows
    }

    /// One closed connection: a row that opens it. The colour it was tagged
    /// with is on the icon, which is the only mark a row this small has room
    /// for and the one that survives being scanned rather than read.
    fn render_saved_connection(
        &self,
        index: usize,
        config: &db::ConnectionConfig,
        connecting: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let config = config.clone();
        let tint = crate::tint::tint(config.color, cx);
        ListItem::new(("connection", index), config.display_name())
            .icon(IconName::Plug)
            .icon_color(match tint {
                Some(tint) => IconColor::Custom(tint),
                None => IconColor::Muted,
            })
            .meta(config.endpoint())
            .on_click(cx.listener(move |this, _, _, cx| {
                if !connecting {
                    this.open_connection(config.clone(), cx);
                }
            }))
            .into_any_element()
    }

    fn render_tree(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let selected = self.selected_node;
        // The tag a connection was given is the whole point of tagging it, so
        // it stays on the row once the connection is open — a red server that
        // turns accent-blue the moment you connect to it is a guardrail that
        // works only while you do not need it.
        let tint = self
            .session
            .as_ref()
            .and_then(|session| crate::tint::tint(session.read(cx).config.color, cx));
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
            let (icon, mut color) = icon_for(node.kind, cx);
            if node.kind == NodeKind::Connection {
                if let Some(tint) = tint {
                    color = IconColor::Custom(tint);
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
                            if let Some(target) = this
                                .tree
                                .iter()
                                .find(|node| node.id == id)
                                .and_then(|node| node.target.clone())
                            {
                                this.open_object_menu(target, event.position(), cx);
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
                            Some(node) if node.kind == NodeKind::Database => {
                                // The one already open is the row you clicked
                                // to select it; `open_database` knows that and
                                // does nothing.
                                this.open_database(&node.name.clone(), cx);
                            }
                            Some(node) => match node.target.clone() {
                                Some(target) => this.open_relation(&target, cx),
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
        let connecting = self
            .session
            .as_ref()
            .is_some_and(|session| session.read(cx).activity() == Activity::Connecting);

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
            let tint = crate::tint::tint(config.color, cx);
            rows.push(
                ListItem::new(("connection", index), config.display_name())
                    .icon(IconName::Plug)
                    .icon_color(match tint {
                        Some(tint) => IconColor::Custom(tint),
                        None => IconColor::Muted,
                    })
                    .meta(config.endpoint())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !connecting {
                            this.open_connection(config.clone(), cx);
                        }
                    }))
                    .into_any_element(),
            );
        }
        rows
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

    /// The History tab.
    ///
    /// Grouped by day, newest first, because "what did I run this morning" is
    /// the question this list exists to answer and a flat list of two hundred
    /// timestamps answers it badly.
    fn render_history(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let c = cx.colors().clone();

        if self.history.is_empty() {
            return vec![div()
                .p(px(12.))
                // The scrolling body sizes to its content, so the state cannot
                // fill the panel and centre in it; this is the gap that keeps
                // it from sitting directly under the filter field instead.
                .pt(px(56.))
                .child(
                    EmptyState::new(IconName::History, "No history yet")
                        .description("Every statement you run is recorded here."),
                )
                .into_any_element()];
        }

        let today = day_of(now_ms());
        let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(self.history.len() + 4);
        let mut current_day = None;

        for (i, entry) in self.history.iter().enumerate() {
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

            let ok = entry.succeeded();
            let sql = entry.sql.clone();
            rows.push(
                ListItem::new(("history", i), entry.one_line())
                    .mono()
                    .height(px(26.))
                    .icon(if ok {
                        IconName::CircleCheck
                    } else {
                        IconName::CircleXmark
                    })
                    .icon_color(if ok {
                        IconColor::Custom(c.success)
                    } else {
                        IconColor::Custom(c.danger)
                    })
                    // An em dash for a statement that never reported back, so a
                    // hung query is visibly different from an instant one.
                    .meta(match entry.duration_ms {
                        Some(ms) => format_duration(Duration::from_millis(ms.max(0) as u64)),
                        None => "—".to_string(),
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.load_sql(&sql, cx)))
                    .into_any_element(),
            );
        }
        rows
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
fn icon_for(kind: NodeKind, cx: &Context<Workspace>) -> (IconName, IconColor) {
    let c = cx.colors();
    match kind {
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
    }
}
