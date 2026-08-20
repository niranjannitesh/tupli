//! The centre stack: editor tabs on top, results docked below.
//!
//! The dock belongs to the centre stack rather than to the window, because
//! results are always results *of* the thing above them. Moving it under the
//! centre column is what lets the sidebar stay full height, which is the layout
//! every serious editor converged on.

use gpui::{
    div, prelude::*, px, relative, AnyElement, App, Context, ElementId, IntoElement, ParentElement,
    Pixels, SharedString, Window,
};
use ui::{
    h_flex, region, v_flex, ActiveTheme, Axis, Badge, BadgeStyle, BadgeTone, Button, ButtonSize,
    ButtonVariant, EmptyState, Icon, IconColor, IconName, IconSize, Label, LabelSize, Notice,
    NoticeTone, ResizeHandle, Tab, TabBar, Toolbar, Tooltip,
};

use crate::pane::{Layout, Member, PaneGroup, PaneId};
use crate::results::ResultsTab;
use crate::workspace::{count_of, format_duration, thousands, CenterKind, DragTarget, Workspace};

impl Workspace {
    /// Every pane is looking at a table's rows rather than at a script.
    ///
    /// A table tab's console is empty and stays empty — the rows *are* the tab
    /// — so when this is true of all of them the panes give up everything but
    /// their tab strips. Only when all of them agree: a split with a query in
    /// it still needs somewhere to type.
    fn browsing_all(&self) -> bool {
        !self.panes.is_empty()
            && self.panes.iter().all(|pane| {
                pane.active()
                    .is_some_and(|tab| matches!(tab.kind, CenterKind::Table | CenterKind::Key))
            })
    }

    /// Which results tab is showing, which is not always the one that was
    /// picked: Structure and DDL are not offered on a connection with no
    /// schema, and a tab left on one of them by the last connection would
    /// otherwise draw a pane that is not in the strip above it.
    fn results_tab(&self, cx: &App) -> ResultsTab {
        let tab = self.pane().results_tab;
        match (tab, self.capabilities(cx).is_sql()) {
            (ResultsTab::Structure | ResultsTab::Ddl, false) => ResultsTab::Data,
            (tab, _) => tab,
        }
    }

    /// The centre stack has given its whole height to the dock.
    ///
    /// Asked from `render_center`, which is where the reasoning lives; from
    /// `render_results`, which shortens the Data tab's name because the pane's
    /// strip above is already showing it; and from the status bar, which drops
    /// the caret position because there is no editor left on screen to have
    /// one. Nothing is `designing` while everything is browsing — a structure
    /// tab is not a table tab — so that half of the dock's condition is not
    /// repeated here.
    pub(crate) fn collapsed(&self) -> bool {
        self.browsing_all() && self.dock_open
    }

    /// The dock is on screen, so its own footer is reporting the last run.
    ///
    /// The status bar asks before it repeats a duration and a row count. The
    /// two numbers belong to the grid and sit under it; said again forty
    /// pixels lower they stop reading as the same measurement and start
    /// reading as a second one that happens to agree.
    pub(crate) fn results_showing(&self) -> bool {
        self.dock_open
            && !self
                .pane()
                .active()
                .is_some_and(|tab| tab.kind == CenterKind::Structure)
    }

    pub(crate) fn render_center(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hit = cx.metrics().splitter_hit_width;
        // A structure tab hides the dock: the rows under it would belong to
        // whatever this pane ran last, which is not the table being designed,
        // and the editor is the one thing on screen worth the height. The
        // dock's own switch is left alone, so a query tab brings it back.
        let designing = self
            .pane()
            .active()
            .is_some_and(|tab| tab.kind == CenterKind::Structure);
        debug_assert_eq!(self.dock_open && !designing, self.results_showing());
        // Browsing does the opposite. A table tab's console is empty and stays
        // empty — the rows *are* the tab — so the panes give up everything but
        // their tab strip and the dock takes the height. Only when every pane
        // agrees: a split with a query in it still needs somewhere to type.
        let browsing = self.browsing_all();
        let dock_open = self.dock_open && !designing;
        // The collapse is what browsing *starts* as, not what it is locked
        // into: closing the dock on a table tab gives the console back at full
        // height, which is where you write the query the table made you think
        // of. An earlier version forced the dock open here, and the switch in
        // the titlebar then did nothing at all on a table tab — a control that
        // ignores you is worse than one that is not there.
        let collapsed = browsing && dock_open;
        debug_assert_eq!(collapsed, self.collapsed());
        // Asked for, rather than implied by the tab. It swaps the same two
        // flexes the collapse does, but it must not be folded into `collapsed`:
        // the collapse is a consequence of what is open and this is a choice,
        // and closing the dock has to undo the first without forgetting the
        // second.
        let maximized = dock_open && !collapsed && self.dock_maximized;
        let filled = collapsed || maximized;
        let dock_height = self.dock_height;
        // Cloned so the tree can be walked while the panes it names are being
        // borrowed to draw themselves. It is a handful of ids and floats.
        let layout = self.layout.clone();

        let group = self.render_group(&layout, &[], window, cx);
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            // While browsing, the panes are as tall as their tab strips and
            // nothing stretches them: `flex_none` around a `flex_1` child is
            // what turns "take the rest" into "take what you need".
            .child(match filled {
                true => div().flex_none().w_full().child(group).into_any_element(),
                false => group,
            })
            // One dock for the window, not one per pane. Results belong to the
            // statement that produced them, and the statement belongs to the
            // pane in front — a second grid under a second editor would double
            // the chrome to say the same thing twice.
            .when(dock_open, |el| {
                el.child(
                    div()
                        .relative()
                        .w_full()
                        .map(|el| match filled {
                            true => el.flex_1().min_h_0(),
                            false => el.flex_none().h(dock_height),
                        })
                        .child(self.render_results(collapsed, window, cx))
                        // Nothing to drag while browsing: the dock is the
                        // whole box and the panes are their tab strips, so a
                        // handle there would move a number nothing reads.
                        .when(!filled, |el| {
                            el.child(
                                // Straddling the dock's top edge — the seam the
                                // dock itself draws — rather than sitting beside
                                // it, so the grab strip is centred on the line the
                                // pointer is aiming at.
                                div()
                                    .absolute()
                                    .left_0()
                                    .w_full()
                                    .top(-hit / 2.)
                                    .h(hit)
                                    .child(
                                        ResizeHandle::new("dock-splitter", Axis::Horizontal)
                                            .active(matches!(
                                                self.dragging_target(),
                                                Some(DragTarget::BottomDock)
                                            ))
                                            .invisible_line()
                                            .on_drag_start(cx.listener(
                                                |this, e: &gpui::MouseDownEvent, _, cx| {
                                                    this.start_dock_drag(e.position, cx)
                                                },
                                            )),
                                    ),
                            )
                        }),
                )
            })
    }

    // ---- panes -----------------------------------------------------------

    /// One level of the pane tree: a row or a column of members, each given
    /// its share of the box by `flex_basis`, with a seam in the gutter after
    /// every member but the last.
    ///
    /// `path` is where this group sits in the tree — the same path a seam drag
    /// and a measurement are keyed by, which is what lets a nested group be
    /// resized without the outer one hearing about it.
    fn render_group(
        &mut self,
        group: &PaneGroup,
        path: &[usize],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hit = cx.metrics().splitter_hit_width;
        let along = group.layout;
        let mut row = match along {
            // Not `h_flex`: that centres its children, and a pane is supposed
            // to be as tall as the row it is in.
            Layout::Columns => div().flex().flex_row().items_stretch(),
            Layout::Rows => v_flex(),
        }
        .flex_1()
        .min_w_0()
        .min_h_0();

        // A seam drag arrives in pixels and the tree is kept in fractions, so
        // the one thing it needs is how big this group came out. Written into
        // a cell rather than into the workspace: measuring is not a change,
        // and asking for another frame from inside a paint would never stop.
        let boxes = self.group_boxes.clone();
        let key = path.to_vec();
        row = row.on_children_prepainted(move |bounds, _, _| {
            let total: f32 = bounds
                .iter()
                .map(|b| match along {
                    Layout::Columns => f32::from(b.size.width),
                    Layout::Rows => f32::from(b.size.height),
                })
                .sum();
            boxes.borrow_mut().insert(key.clone(), total);
        });

        let last = group.members.len().saturating_sub(1);
        for (index, member) in group.members.iter().enumerate() {
            let flex = group.flexes.get(index).copied().unwrap_or(1.);
            let inner = match member {
                Member::Pane(id) => self.render_pane(*id, window, cx),
                Member::Group(inner) => {
                    let mut deeper = path.to_vec();
                    deeper.push(index);
                    self.render_group(inner, &deeper, window, cx)
                }
            };
            let seam = (index < last).then(|| self.render_seam(path, index, along, hit, cx));
            row = row.child(
                // `flex_basis` in per cent, so the whole group always adds up
                // to itself whatever it is measured at.
                v_flex()
                    .relative()
                    .flex_grow_1()
                    .flex_shrink_1()
                    .flex_basis(relative(flex))
                    .min_w_0()
                    .min_h_0()
                    .child(inner)
                    .children(seam),
            );
        }
        row.into_any_element()
    }

    /// The handle between two members, straddling the seam after the first of
    /// them — half in each — so that the grab strip is centred on the line the
    /// pointer is aiming at. The line itself belongs to the pane, which draws
    /// its own border whenever the window is split.
    fn render_seam(
        &self,
        path: &[usize],
        index: usize,
        along: Layout,
        hit: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let target = DragTarget::Seam {
            path: path.to_vec(),
            index,
        };
        let handle = ResizeHandle::new(seam_id(path, index), along.seam())
            .active(self.dragging(target.clone()))
            .invisible_line()
            .on_drag_start(cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                this.begin_drag(target.clone(), e.position, cx)
            }));
        match along {
            Layout::Columns => div().absolute().top_0().h_full().right(-hit / 2.).w(hit),
            Layout::Rows => div().absolute().left_0().w_full().bottom(-hit / 2.).h(hit),
        }
        .child(handle)
        .into_any_element()
    }

    /// One pane: its tab strip and its console.
    fn render_pane(
        &mut self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = cx.colors().clone();
        let Some(pane) = self.pane_by(id) else {
            // The tree named a pane the list does not have, which is a bug
            // rather than a state — draw nothing rather than panic mid-frame.
            log::warn!("the tree names pane {id}, which is not in the list");
            return div().into_any_element();
        };
        let focused = self.active_pane == id;
        // A window with one pane has nothing to split off, so it shows neither
        // the split cue nor the close button.
        let split = self.panes.len() > 1;
        let active = pane.active_tab;

        // What each tab is connected to, and whether that is worth saying. One
        // database across the whole strip is the ordinary case and naming it on
        // every tab would be noise; two is the thing a person has to be able to
        // see without clicking, because it decides what `select * from users`
        // means.
        let sources: Vec<Option<SharedString>> = pane
            .tabs
            .iter()
            .map(|tab| self.tab_database(tab, cx))
            .collect();
        let mixed = {
            let named: std::collections::HashSet<_> = sources.iter().flatten().collect();
            named.len() > 1
        };
        let pane = self.pane_by(id).expect("checked above");

        let tabs: Vec<_> = pane
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                // The database instead of the schema: when the strip spans two
                // of them, which schema a table is in is the smaller half of
                // the answer.
                let detail = match mixed {
                    true => sources.get(i).cloned().flatten().or(tab.detail.clone()),
                    false => tab.detail.clone(),
                };
                let (icon, tone) = match tab.kind {
                    CenterKind::Query => (IconName::Code, IconColor::Accent),
                    CenterKind::Table => (IconName::Table, IconColor::Custom(c.warning)),
                    CenterKind::Structure => (IconName::Columns, IconColor::Muted),
                    CenterKind::Key => (IconName::Key, IconColor::Custom(c.info)),
                };
                Tab::new(pane_id("center-tab", id, i), tab.title.clone())
                    .icon(icon)
                    .icon_color(tone)
                    .active(i == active)
                    .dirty(tab.dirty)
                    .closable(!tab.dirty)
                    .when_some(detail, |t, d| t.detail(d))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.activate_pane(id, cx);
                        this.activate_tab(i, cx)
                    }))
                    .on_close(cx.listener(move |this, _, _, cx| {
                        this.activate_pane(id, cx);
                        this.close_tab(i, cx)
                    }))
            })
            .collect();

        // A pane browsing a table has nothing to put in its body — the rows
        // are in the dock — so it is only as tall as its own tab strip. When
        // every pane is browsing the group collapses too and the dock takes
        // the whole height; in a mixed split the console beside it keeps its
        // height and this one simply stops taking room it cannot use.
        let browsing = self.browsing(id);
        // Nothing under this strip but another strip: the panes are all down to
        // their tab bars and the dock's own bar is the next band. The line
        // between the two is that one's to draw.
        let stacked = self.collapsed();
        let empty = self.pane_by(id).is_some_and(|pane| pane.tabs.is_empty());
        let scroll = self
            .pane_by(id)
            .map(|pane| pane.tab_scroll.clone())
            .unwrap_or_default();
        // Half a pixel of slack: the width is a laid-out float and comparing
        // two of those for equality would re-reveal on rounding noise.
        let width = scroll.bounds().size.width;
        let resized = self
            .pane_by(id)
            .is_some_and(|pane| (width - pane.tab_strip_width).abs() > px(0.5));
        if resized {
            if let Some(pane) = self.pane_by_mut(id) {
                pane.tab_strip_width = width;
                let active = pane.active_tab;
                pane.tab_scroll.scroll_to_item(active);
            }
        }
        region(cx)
            .map(|el| match browsing {
                true => el.flex_none(),
                false => el.flex_1().min_h_0(),
            })
            // Which pane the keyboard is in, said once and quietly. Only when
            // there is more than one: a border around the only pane in the
            // window would be answering a question nobody asked.
            .when(split, |el| {
                el.border_1()
                    .border_color(if focused { c.border_focus } else { c.border })
            })
            // Clicking anywhere in a pane is what makes it the active one.
            // Mouse-down rather than click, so it has happened before whatever
            // was clicked acts on it.
            .when(!focused, |el| {
                el.on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, cx| this.activate_pane(id, cx)),
                )
            })
            .child(
                TabBar::new(pane_id("center-tabs", id, 0))
                    .tabs(tabs)
                    // Collapsed, the dock's own strip is directly below this
                    // one and draws the line between them. See `TabBar::stacked`.
                    .when(stacked, |bar| bar.stacked())
                    // The one strip in the window whose contents are unbounded:
                    // a pane can hold any number of tabs and a split pane can be
                    // 300px wide. Clipped, the fourth tab is a tab nobody can
                    // reach — including, after a restore, the active one.
                    .track_scroll(scroll)
                    .end_child(
                        Button::icon(pane_id("new-query", id, 0), IconName::Plus)
                            .size(ButtonSize::XSmall)
                            .tooltip(Tooltip::key("New Query", "⌘T"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activate_pane(id, cx);
                                this.new_query_tab(cx)
                            })),
                    )
                    .end_child(
                        Button::icon(pane_id("split-right", id, 0), IconName::SplitX)
                            .size(ButtonSize::XSmall)
                            .tooltip(Tooltip::key("Split Right", "⌘D"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activate_pane(id, cx);
                                this.split_pane(Layout::Columns, cx)
                            })),
                    )
                    .end_child(
                        Button::icon(pane_id("split-down", id, 0), IconName::SplitY)
                            .size(ButtonSize::XSmall)
                            .tooltip(Tooltip::key("Split Down", "⇧⌘D"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activate_pane(id, cx);
                                this.split_pane(Layout::Rows, cx)
                            })),
                    )
                    .when(split, |bar| {
                        bar.end_child(
                            Button::icon(pane_id("close-pane", id, 0), IconName::Xmark)
                                .size(ButtonSize::XSmall)
                                .tooltip(Tooltip::text("Close Pane"))
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.close_pane(id, cx)),
                                ),
                        )
                    }),
            )
            // A table tab has no console: its rows are in the dock, which
            // has taken the height this editor would have wasted. What is left
            // is the tab strip, which is how you get back to a query.
            .when(!browsing, |el| {
                el
                    // A structure tab has no console: the thing being edited is the
                    // table's shape, and the statements that change it are read in the
                    // preview sheet rather than typed.
                    .child(match (empty, self.designing(id, cx)) {
                        // Every tab closed. The console would be showing the script of
                        // a tab that is gone, so it is not shown at all.
                        (true, _) => self.render_no_tabs(id, cx).into_any_element(),
                        // The editor, or — for a design tab restored from the last run
                        // before the catalog has arrived — the space where it will be.
                        // Never the console: this tab is not one.
                        (_, Some(editor)) => gpui::div()
                            .flex_1()
                            .min_h_0()
                            .children(editor)
                            .into_any_element(),
                        (_, None) => self.render_editor(id, window, cx).into_any_element(),
                    })
            })
            .into_any_element()
    }

    /// A pane whose tabs have all been closed.
    ///
    /// Not an error and not a dead end: the two ways back — a blank script or
    /// a table from the tree — are the two things this says.
    fn render_no_tabs(&mut self, id: PaneId, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex_1().min_h_0().child(
            EmptyState::new(IconName::Code, "No tabs open")
                .description("Open a table from the sidebar, or start a new query.")
                .action(
                    Button::new(pane_id("empty-new-query", id, 0), "New Query")
                        .variant(ButtonVariant::Accent)
                        .size(ButtonSize::Small)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.activate_pane(id, cx);
                            this.new_query_tab(cx)
                        })),
                ),
        )
    }

    /// Whether the tab in front of a pane is a table being browsed. Its rows
    /// live in the dock, so the pane itself has nothing left to draw.
    fn browsing(&self, id: PaneId) -> bool {
        self.pane_by(id)
            .and_then(|pane| pane.active())
            .is_some_and(|tab| tab.kind == CenterKind::Table)
    }

    // ---- editor ----------------------------------------------------------

    /// Whether the tab in front of a pane is a design tab, and the editor it
    /// is showing. `Some(None)` is a design tab that has no editor yet.
    fn designing(
        &self,
        id: PaneId,
        _cx: &Context<Self>,
    ) -> Option<Option<gpui::Entity<crate::structure::StructureEditor>>> {
        let tab = self.pane_by(id)?.active()?;
        (tab.kind == CenterKind::Structure).then(|| tab.structure.clone())
    }

    fn render_editor(
        &mut self,
        id: PaneId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let running = self.is_running(cx) && self.running_pane() == Some(id);
        let connected = self.is_connected(cx);
        let Some(pane) = self.pane_by(id) else {
            return v_flex().flex_1().min_h_0();
        };
        let editor = pane.editor.clone();
        let empty = editor.read(cx).is_empty();
        // The breadcrumb names where a statement would actually land, which is
        // the server's `search_path`, not a name someone typed into the sheet.
        let (database, schema) = self.current_location(cx);

        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                Toolbar::new(pane_id("editor-toolbar", id, 0))
                    .transparent()
                    .borderless()
                    .start_child(
                        // The breadcrumb is also the switch. The connection
                        // belongs to this tab, so the control that changes it
                        // belongs in the tab too — reaching up to the titlebar
                        // to move one tab to another database is reaching past
                        // the thing you are pointing at.
                        h_flex()
                            .id(pane_id("editor-source", id, 0))
                            .gap(px(5.))
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(cx.metrics().radius_sm)
                            .when(connected, |el| {
                                el.cursor_pointer().hover(|el| el.bg(cx.colors().hover))
                            })
                            .child(Icon::new(IconName::Database).size(IconSize::Small).color(
                                if connected {
                                    IconColor::Accent
                                } else {
                                    IconColor::Disabled
                                },
                            ))
                            .child(Label::new(database).color(IconColor::Muted))
                            .child(Label::new("/").color(IconColor::Disabled))
                            .child(Label::new(schema).color(IconColor::Muted))
                            .child(
                                Icon::new(IconName::ChevronDown)
                                    .size(IconSize::Small)
                                    .color(IconColor::Disabled),
                            )
                            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                                this.activate_pane(id, cx);
                                this.open_database_menu(event.position(), cx);
                            })),
                    )
                    .end_child(
                        // Whitespace only — it never rewrites the statement,
                        // so pressing it can never change what runs.
                        Button::icon(pane_id("editor-format", id, 0), IconName::TextAlignLeft)
                            .size(ButtonSize::Small)
                            .tooltip(Tooltip::key("Format SQL", "\u{2325}\u{21e7}F"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activate_pane(id, cx);
                                this.format_query(cx)
                            })),
                    )
                    .end_child(
                        // The history is a sidebar list, not a second popup of
                        // its own: one place where past statements live, and
                        // this is a way to walk to it rather than a copy of it.
                        Button::icon(pane_id("editor-history", id, 0), IconName::History)
                            .size(ButtonSize::Small)
                            .tooltip(Tooltip::text("Query History"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activate_pane(id, cx);
                                this.show_sidebar_tab(crate::workspace::SidebarTab::History, cx)
                            })),
                    )
                    .end_child(
                        // ⌘S. Disabled on an empty editor rather than hidden,
                        // so the button never moves.
                        Button::icon(pane_id("editor-save", id, 0), IconName::Save)
                            .size(ButtonSize::Small)
                            .tooltip(Tooltip::key("Save Query", "⌘S"))
                            .disabled(empty)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.activate_pane(id, cx);
                                this.save_query(cx)
                            })),
                    )
                    .end_child(
                        // One button, two states. While a statement is in
                        // flight the only useful thing it can do is stop it,
                        // and a second Run next to a running query is how you
                        // end up with two.
                        //
                        // Run takes the same path ⌘⏎ takes, so the button can
                        // never do something the keystroke does not.
                        if running {
                            Button::new(pane_id("editor-run", id, 0), "Cancel")
                                .start_icon(IconName::Stop)
                                .variant(ButtonVariant::Danger)
                                .size(ButtonSize::Small)
                                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx)))
                        } else {
                            Button::new(pane_id("editor-run", id, 0), "Run")
                                .start_icon(IconName::RunFilled)
                                .variant(ButtonVariant::Accent)
                                .size(ButtonSize::Small)
                                .disabled(!connected)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.activate_pane(id, cx);
                                    this.run_console(cx);
                                }))
                        },
                    ),
            )
            // The editor is one custom element, not a div per line: see
            // crates/editor/src/element.rs. It owns its own scrolling, so there
            // is no scroll container around it.
            .child(div().flex_1().min_h_0().child(editor))
    }

    // ---- results ---------------------------------------------------------

    /// The dock's four tabs: the rows, the columns, the DDL and the log.
    ///
    /// Always in the dock, directly above the thing they switch — Data,
    /// Structure, DDL, Messages, at the bottom of the window where they have
    /// been since the first sketch. An earlier version promoted them into the
    /// pane's own tab strip whenever the centre stack collapsed, on the theory
    /// that two strips forty pixels apart was one too many. It is not: moving a
    /// control to a different edge of the window depending on which tab is open
    /// means the answer to "where is DDL" is "it depends", and a switcher that
    /// moves is a switcher you have to look for. `brief` is what is left of
    /// that idea — see the naming below.
    fn results_tabs(&mut self, brief: bool, cx: &mut Context<Self>) -> Vec<Tab> {
        let c = cx.colors().clone();
        // The results tab is named after whatever produced the rows, so a
        // window with four tables open never shows four tabs all called
        // "users". Except while browsing, when the pane's strip directly above
        // is already showing that name and the console under it is gone: then
        // the switcher says what it switches to instead of saying `users` a
        // second time, forty pixels below the first.
        let (data_title, data_detail) = match (brief, self.pane().active()) {
            (true, _) => (SharedString::from("Data"), None),
            (_, Some(tab)) if tab.kind == CenterKind::Table => {
                (tab.title.clone(), tab.detail.clone())
            }
            _ => (SharedString::from("Result"), None),
        };
        let tab = self.results_tab(cx);
        // Messages carries a count once there is anything in it, because the
        // whole point of the tab is that you can ignore it until you cannot.
        let message_count = self.messages.len();
        let failures = self
            .messages
            .iter()
            .filter(|m| m.tone == crate::results::MessageTone::Failed)
            .count();
        // A server `WARNING` is the server saying something happened that you
        // did not ask for. Nothing else in the window says so — the statement
        // succeeded — so the tab has to, or a migration that half-worked looks
        // like one that worked.
        let warnings = self
            .messages
            .iter()
            .any(|m| m.notices.iter().any(db::Notice::is_warning));

        // A script's answers each get a tab. One answer keeps the single tab
        // named after whatever produced it, because "Result 1" of one is a
        // count nobody needs.
        let results = self.pane().results.len();
        let selected = self.pane().result_index;
        let mut tabs: Vec<Tab> = match results > 1 {
            false => vec![Tab::new("results-data", data_title)
                .when_some(data_detail, |t, d| t.detail(d))
                .icon(IconName::Table)
                .icon_color(IconColor::Custom(c.warning))
                .active(tab == ResultsTab::Data)
                .on_click(
                    cx.listener(|this, _, _, cx| this.select_results_tab(ResultsTab::Data, cx)),
                )],
            true => (0..results)
                .map(|index| {
                    Tab::new(
                        SharedString::from(format!("results-data-{index}")),
                        format!("Result {}", index + 1),
                    )
                    .detail(thousands(self.pane().results[index].rows.row_count()))
                    .icon(IconName::Table)
                    .icon_color(IconColor::Custom(c.warning))
                    .active(tab == ResultsTab::Data && index == selected)
                    .on_click(cx.listener(move |this, _, _, cx| this.show_result(index, cx)))
                })
                .collect(),
        };
        // Structure and DDL are about a table: a list of columns with their
        // types, and the `create table` that would make them. A keyspace has
        // neither, and a tab that could only ever be empty is worse than one
        // that is not there. Asked of the connection rather than of the tab,
        // because a console on a Redis connection has no DDL either.
        if self.capabilities(cx).is_sql() {
            tabs.push(
                Tab::new("results-structure", "Structure")
                    .icon(IconName::Columns)
                    .active(tab == ResultsTab::Structure)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.select_results_tab(ResultsTab::Structure, cx)
                    })),
            );
            tabs.push(
                Tab::new("results-ddl", "DDL")
                    .icon(IconName::Code)
                    .active(tab == ResultsTab::Ddl)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.select_results_tab(ResultsTab::Ddl, cx)),
                    ),
            );
        }
        tabs.push(
            Tab::new("results-messages", "Messages")
                .icon(IconName::Terminal)
                .icon_color(match (failures > 0, warnings) {
                    (true, _) => IconColor::Danger,
                    (false, true) => IconColor::Warning,
                    (false, false) => IconColor::Muted,
                })
                .when_some(
                    (message_count > 0).then(|| thousands(message_count)),
                    |t, n| t.detail(n),
                )
                .active(tab == ResultsTab::Messages)
                .on_click(
                    cx.listener(|this, _, _, cx| this.select_results_tab(ResultsTab::Messages, cx)),
                ),
        );
        tabs
    }

    fn render_results(
        &mut self,
        collapsed: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let c = cx.colors().clone();
        // The button only exists when the dock is not already the whole centre
        // by way of a table tab, so this is the only maximise it can be about.
        // See `render_center`.
        let maximized = self.dock_maximized && !collapsed;
        let row_count = self.pane().row_count;
        // A browsed table is the only tab that can turn a page: the statement
        // was written here, so an `offset` may be added to it. See
        // `Workspace::show_page`.
        let page = self
            .pane()
            .active()
            .filter(|tab| tab.kind == CenterKind::Table)
            .map(|tab| tab.page.unwrap_or(0));
        let page_size = self.settings.page_size();
        let running = self.is_running(cx);
        let error = self.pane().error.clone();

        let tabs = self.results_tabs(collapsed, cx);
        let tab = self.results_tab(cx);
        // The statement a result came from, shown under the strip when a script
        // produced several and the tab is only called "Result 3".
        let shown_sql = (self.pane().results.len() > 1)
            .then(|| self.pane().results.get(self.pane().result_index))
            .flatten()
            .map(|result| SharedString::from(crate::results::one_line(&result.sql)));

        region(cx)
            .size_full()
            // No border on the region itself: the strip below draws its own top
            // hairline, and a region border here as well is the same line twice
            // — two pixels where every other seam in the window is one.
            .child(
                TabBar::new("results-tabs")
                    .tabs(tabs)
                    // Nothing to maximise while the panes are already collapsed
                    // to their tab strips: the dock *is* the centre, and a
                    // button whose only state is the one you are in is a button
                    // that does nothing.
                    .when(!collapsed, |bar| {
                        bar.end_child(
                            Button::icon(
                                "results-maximize",
                                match maximized {
                                    true => IconName::Collapse,
                                    false => IconName::Expand,
                                },
                            )
                            .size(ButtonSize::XSmall)
                            .tooltip(match maximized {
                                true => Tooltip::text("Restore Results"),
                                false => Tooltip::text("Maximize Results"),
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_dock_maximized(cx))),
                        )
                    }),
            )
            // Only the grid gets the row toolbar and the error banner. A
            // filter field above a list of column types would do nothing, and a
            // control that does nothing is worse than an absent one.
            .child(match tab {
                ResultsTab::Data => self.render_data_tab(error.clone(), cx),
                ResultsTab::Structure => v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_structure(_window, cx))
                    .into_any_element(),
                ResultsTab::Ddl => self.render_ddl(_window, cx),
                ResultsTab::Messages => v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_messages(_window, cx))
                    .into_any_element(),
            })
            .child(
                h_flex()
                    .h(px(24.))
                    .w_full()
                    .flex_none()
                    .px(px(8.))
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(c.border)
                    .child(
                        // What the last run actually did, not a relative time
                        // nobody can check. A failure says so: the rows above
                        // are the *previous* statement's, and "Completed"
                        // under them would be a claim about the wrong one.
                        Label::new(match (running, &error, self.pane().elapsed) {
                            (true, _, _) => "Running…".to_string(),
                            (false, Some(e), elapsed) => {
                                let verb = if e.is_canceled() {
                                    "Canceled"
                                } else {
                                    "Failed"
                                };
                                match elapsed {
                                    Some(d) => format!("{verb} after {}", format_duration(d)),
                                    None => verb.to_string(),
                                }
                            }
                            (false, None, Some(d)) => {
                                format!("Completed in {}", format_duration(d))
                            }
                            (false, None, None) => "Ready".to_string(),
                        })
                        .size(LabelSize::Small)
                        .color(match &error {
                            Some(_) if running => IconColor::Subtle,
                            Some(e) if e.is_canceled() => IconColor::Warning,
                            Some(_) => IconColor::Danger,
                            None => IconColor::Subtle,
                        }),
                    )
                    // Which statement these rows came out of. Only worth the
                    // space when a script produced several answers: with one
                    // result the console above is already showing it.
                    .when_some(shown_sql, |el, sql| {
                        el.child(
                            Label::new(sql)
                                .mono()
                                .size(LabelSize::Small)
                                .color(IconColor::Disabled)
                                .flex_1()
                                .min_w_0(),
                        )
                    })
                    .child(div().flex_1())
                    // Only once there is a page to turn: two greyed arrows
                    // under a nine-row table are two controls that have never
                    // done anything, and a footer full of those teaches the
                    // reader to stop looking at the footer.
                    .children(
                        page.filter(|page| *page > 0 || row_count >= page_size)
                            .map(|page| {
                                let first = page * page_size;
                                h_flex()
                                    .flex_none()
                                    .gap(px(2.))
                                    .child(
                                        Button::icon("rows-prev", IconName::ChevronLeft)
                                            .size(ButtonSize::XSmall)
                                            .tooltip(Tooltip::text("Previous Page"))
                                            // Off while the last page is still on
                                            // its way: a second statement sent
                                            // down a busy connection is dropped,
                                            // and an arrow that sometimes does
                                            // nothing is worse than one that is
                                            // visibly waiting.
                                            .disabled(page == 0 || running)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.show_page(page.saturating_sub(1), cx)
                                            })),
                                    )
                                    .child(
                                        // Which rows these are, counted from the
                                        // top of the table rather than from the top
                                        // of the page: "1–500" and then "1–500"
                                        // again would be the same footer for two
                                        // different screens.
                                        Label::new(match row_count {
                                            0 => format!("from {}", thousands(first + 1)),
                                            n => format!(
                                                "{}–{}",
                                                thousands(first + 1),
                                                thousands(first + n)
                                            ),
                                        })
                                        .size(LabelSize::Small)
                                        .color(IconColor::Muted),
                                    )
                                    .child(
                                        // A full page is the only evidence there is
                                        // a next one — no `count(*)` was run, and
                                        // running one to grey out an arrow would be
                                        // a sequential scan per page turn.
                                        Button::icon("rows-next", IconName::ChevronRight)
                                            .size(ButtonSize::XSmall)
                                            .tooltip(Tooltip::text("Next Page"))
                                            .disabled(row_count < page_size || running)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.show_page(page + 1, cx)
                                            })),
                                    )
                            }),
                    )
                    .child(
                        Label::new(match self.pane().affected {
                            Some(count) => format!("{} affected", count_of(count as usize, "row")),
                            None if self.pane().truncated => {
                                format!("{}+ rows", thousands(row_count))
                            }
                            None => count_of(row_count, "row"),
                        })
                        .size(LabelSize::Small)
                        .color(IconColor::Muted),
                    ),
            )
    }

    /// The filter, above the rows: either a row of chips or the clause someone
    /// wrote by hand.
    ///
    /// Two modes rather than one, because the two are not the same tool. Chips
    /// are for the ninety per cent — one column, one operator, one value, and
    /// nothing to get syntactically wrong — and the funnel hands over to a
    /// plain `where` box for the rest, carrying the chips' SQL across so the
    /// The facts bar over a key's contents: what it is, how long it has left,
    /// how it is stored, how big it is.
    ///
    /// `None` for anything that is not a key tab, which is what puts the
    /// filter bar back. Every fact is optional on purpose — a server that
    /// refuses `MEMORY USAGE` still has a TTL and an encoding, and the honest
    /// answer to a question the server would not answer is to leave the label
    /// out rather than to print a zero.
    fn render_key_facts(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (key, kind) = self
            .pane()
            .active()
            .filter(|tab| tab.kind == CenterKind::Key)
            .and_then(|tab| tab.key.clone())?;
        let c = cx.colors().clone();
        let ty = cx.typography().clone();
        // The facts belong to the key that was opened, which is not this tab
        // when a second tab has been opened since. Nothing is shown then
        // rather than another key's TTL under this key's name.
        let facts = self
            .session
            .as_ref()
            .map(|session| session.read(cx))
            .and_then(|session| session.last_key.as_ref())
            .filter(|view| view.key == key)
            .and_then(|view| view.facts.clone());

        let mut pairs: Vec<(SharedString, SharedString)> =
            vec![("Type".into(), kind.label().to_string().into())];
        if let Some(facts) = &facts {
            pairs.push(("TTL".into(), db::format_ttl(facts.ttl).into()));
            if let Some(encoding) = &facts.encoding {
                pairs.push(("Encoding".into(), encoding.to_string().into()));
            }
            if let Some(memory) = facts.memory {
                pairs.push((
                    "Memory".into(),
                    db::value::byte_size(memory as usize).into(),
                ));
            }
            if let Some(length) = facts.length {
                pairs.push((length_label(&kind).into(), thousands(length as usize).into()));
            }
        }

        Some(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(14.))
                .children(pairs.into_iter().map(|(name, value)| {
                    h_flex()
                        .flex_none()
                        .gap(px(5.))
                        .child(Label::new(name).size(LabelSize::Small).color(IconColor::Subtle))
                        .child(
                            div()
                                .font(ty.mono_font())
                                .text_size(ty.ui_size_sm)
                                .text_color(c.text)
                                .child(value),
                        )
                }))
                .into_any_element(),
        )
    }

    /// hard case starts from the easy one instead of from an empty field.
    fn render_filter_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = cx.colors().clone();
        let ty = cx.typography().clone();
        let radius = cx.metrics().radius_sm;
        let value_color = cx.syntax().string;
        let Some(tab) = self.pane().active() else {
            return div().flex_1().into_any_element();
        };
        if tab.filter.raw {
            return div()
                .flex_1()
                .min_w_0()
                .font(ty.mono_font())
                .child(self.pane().filter.clone())
                .into_any_element();
        }
        let chips = tab.filter.chips.clone();
        let composer = self.pane().composer.clone();
        let anything = !chips.is_empty() || composer.is_some();

        // A pill: the shape both a committed chip and the composer wear, so
        // editing one does not make it jump to somewhere else on the row.
        let pill = move |id: ElementId| {
            h_flex()
                .id(id)
                .flex_none()
                .h(px(22.))
                .px(px(6.))
                .gap(px(5.))
                .rounded(radius)
                .bg(c.field)
                .border_1()
                .border_color(c.border)
        };

        h_flex()
            .id("filter-chips")
            .flex_1()
            .min_w_0()
            .h(px(24.))
            .gap(px(4.))
            .overflow_x_scroll()
            .children(chips.iter().enumerate().flat_map(|(i, chip)| {
                // The join sits *before* the chip it belongs to, which is where
                // it is read, and the first chip has none: `and email = ...`
                // with nothing to its left is a clause missing its beginning.
                let join = (i > 0).then(|| {
                    h_flex()
                        .id(("filter-join", i))
                        .flex_none()
                        .h(px(22.))
                        .px(px(4.))
                        .rounded(radius)
                        .cursor_pointer()
                        .hover(|s| s.bg(c.hover))
                        .on_click(cx.listener(move |this, _, _, cx| this.flip_join(i, cx)))
                        .child(
                            Label::new(chip.join.keyword())
                                .size(LabelSize::Small)
                                .color(IconColor::Subtle),
                        )
                        .into_any_element()
                });
                let body = pill(("filter-chip", i).into())
                    .cursor_pointer()
                    .hover(|s| s.bg(c.hover))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_chip(Some(i), cx)))
                    .child(Label::new(chip.column.clone()).size(LabelSize::Small))
                    .child(
                        Label::new(chip.op.symbol())
                            .size(LabelSize::Small)
                            .color(IconColor::Subtle),
                    )
                    // The value in mono and in the string colour: it is data,
                    // and the column name beside it is not. Without the split
                    // `plan = free` reads as two column names.
                    .when(chip.op.takes_value(), |el| {
                        el.child(
                            Label::new(chip.value.clone())
                                .mono()
                                .size(LabelSize::Small)
                                .color(IconColor::Custom(value_color)),
                        )
                    })
                    .child(
                        div()
                            .id(("filter-chip-x", i))
                            .flex_none()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.remove_chip(i, cx)
                            }))
                            .child(
                                Icon::new(IconName::Xmark)
                                    .size(IconSize::XSmall)
                                    .color(IconColor::Disabled)
                                    .flat(),
                            ),
                    )
                    .into_any_element();
                join.into_iter().chain(std::iter::once(body))
            }))
            // The composer wears the same pill, with its column and operator as
            // menus and its value as a live field. Committing is Enter, leaving
            // it is Escape; both are wired on the input.
            .children(composer.map(|composer| {
                let (column, op) = (composer.chip.column.clone(), composer.chip.op);
                pill("filter-composer".into())
                    // The chip being composed is not yet in the row, so nothing
                    // renders the join in front of it and the row would read
                    // `plan = enterprise  id = 7` with no word between them.
                    .when(!chips.is_empty(), |el| {
                        el.child(
                            Label::new(composer.chip.join.keyword())
                                .size(LabelSize::Small)
                                .color(IconColor::Subtle),
                        )
                    })
                    .border_color(c.border_focus)
                    .child(
                        div()
                            .id("filter-composer-column")
                            .cursor_pointer()
                            .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                                let at = e.position();
                                this.open_filter_menu(crate::workspace::FilterMenu::Column, at, cx)
                            }))
                            .child(Label::new(column).size(LabelSize::Small)),
                    )
                    .child(
                        div()
                            .id("filter-composer-op")
                            .cursor_pointer()
                            .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                                let at = e.position();
                                this.open_filter_menu(crate::workspace::FilterMenu::Op, at, cx)
                            }))
                            .child(
                                Label::new(op.symbol())
                                    .size(LabelSize::Small)
                                    .color(IconColor::Accent),
                            ),
                    )
                    .when(op.takes_value(), |el| {
                        el.child(
                            div()
                                .w(px(140.))
                                .font(ty.mono_font())
                                .child(self.pane().chip_value.clone()),
                        )
                    })
            }))
            .child(
                // Add. Reads as part of the row rather than as a toolbar
                // button, because what it adds lands right here.
                h_flex()
                    .id("filter-add")
                    .flex_none()
                    .size(px(22.))
                    .justify_center()
                    .rounded(radius)
                    .cursor_pointer()
                    .hover(|s| s.bg(c.hover))
                    .on_click(cx.listener(|this, _, _, cx| this.open_chip(None, cx)))
                    .child(
                        Icon::new(IconName::Plus)
                            .size(IconSize::XSmall)
                            .color(IconColor::Subtle)
                            .flat(),
                    ),
            )
            .when(!anything, |el| {
                // Not a disabled control and not an empty box: the row says
                // what pressing the `+` beside it would do.
                el.child(
                    Label::new("no filter")
                        .size(LabelSize::Small)
                        .color(IconColor::Disabled),
                )
            })
            .child(div().flex_1().min_w_0())
            .when(anything, |el| {
                el.child(
                    h_flex()
                        .id("filter-clear")
                        .flex_none()
                        .size(px(22.))
                        .justify_center()
                        .rounded(radius)
                        .cursor_pointer()
                        .hover(|s| s.bg(c.hover))
                        .on_click(cx.listener(|this, _, _, cx| this.clear_filter(cx)))
                        .child(
                            Icon::new(IconName::Xmark)
                                .size(IconSize::XSmall)
                                .color(IconColor::Subtle)
                                .flat(),
                        ),
                )
            })
            .into_any_element()
    }

    /// The Data tab: the row toolbar, the grid, and the failure banner.
    fn render_data_tab(
        &mut self,
        error: Option<db::DbError>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let grid = self.pane().grid.read(cx);
        let editable = grid.is_editable();
        let counts = grid.changes().counts();
        let (can_undo, can_redo) = (grid.changes().can_undo(), grid.changes().can_redo());
        let staged = counts.total();
        // Why not, when the answer is interesting. A pane showing a query
        // rather than a table says nothing — see `Workspace::not_editable`.
        // A tab naming a table this connection does not have comes first and
        // in a calmer tone: nothing is broken, the tab is simply about
        // somewhere else, and no statement was sent to find that out.
        let banner = match self.absent_relation(cx) {
            Some(relation) => Some((
                NoticeTone::Info,
                SharedString::from(format!(
                    "{relation} is not in this database. Pick a table from the sidebar."
                )),
            )),
            None => self
                .not_editable(cx)
                .map(|message| (NoticeTone::Warning, message)),
        };
        let filtering = self
            .pane()
            .active()
            .is_some_and(|tab| tab.filter.is_active());
        let keyed = self
            .pane()
            .active()
            .is_some_and(|tab| tab.kind == CenterKind::Key);

        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                Toolbar::new("results-toolbar")
                    .transparent()
                    .borderless()
                    // Nothing to filter on a key: its rows are what the key
                    // holds, not a `select` this app wrote, and there is no
                    // clause to add to a `HGETALL`.
                    .when(!keyed, |bar| {
                        bar.start_child(
                            // The funnel switches between the two ways of
                            // saying the same thing. It carries the accent when
                            // a filter is in force, because the row of chips can
                            // be scrolled out of sight and "why are there only
                            // nine rows" needs an answer that is always visible.
                            Button::icon("results-filter", IconName::Filter)
                                .size(ButtonSize::XSmall)
                                .tooltip(Tooltip::text("Filter Rows"))
                                .selected(filtering)
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_filter_mode(cx))),
                        )
                    })
                    // A key's facts stand where a table's filter does: it is
                    // the same question — "what am I looking at" — and a key
                    // answers it with a TTL rather than with a `where`.
                    .center_child(match self.render_key_facts(cx) {
                        Some(facts) => facts,
                        None => self.render_filter_bar(cx),
                    })
                    .end_child(
                        Button::icon("row-add", IconName::Plus)
                            .size(ButtonSize::XSmall)
                            .tooltip(Tooltip::text("Add Row"))
                            .disabled(!editable)
                            .on_click(cx.listener(|this, _, _, cx| this.add_row(cx))),
                    )
                    .end_child(
                        Button::icon("row-delete", IconName::Minus)
                            .size(ButtonSize::XSmall)
                            .tooltip(Tooltip::text("Delete Selected Rows"))
                            .disabled(!editable)
                            .on_click(cx.listener(|this, _, _, cx| this.delete_rows(cx))),
                    )
                    .end_child(
                        Button::icon("results-undo", IconName::Undo)
                            .size(ButtonSize::XSmall)
                            .tooltip(Tooltip::text("Undo Row Edit"))
                            .disabled(!can_undo)
                            .on_click(cx.listener(|this, _, _, cx| this.undo_edit(cx))),
                    )
                    .end_child(
                        Button::icon("results-redo", IconName::Redo)
                            .size(ButtonSize::XSmall)
                            .tooltip(Tooltip::text("Redo Row Edit"))
                            .disabled(!can_redo)
                            .on_click(cx.listener(|this, _, _, cx| this.redo_edit(cx))),
                    )
                    .end_child(
                        Button::icon("results-refresh", IconName::Refresh)
                            .size(ButtonSize::XSmall)
                            .tooltip(Tooltip::key("Refresh Results", "⌘R"))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_results(cx))),
                    )
                    .end_child(
                        // Counted on the button rather than beside it: the one
                        // number anybody wants before pressing it is how much
                        // is about to happen.
                        Button::new(
                            "results-commit",
                            if staged > 0 {
                                format!("Commit {staged}")
                            } else {
                                "Commit".to_string()
                            },
                        )
                        .size(ButtonSize::XSmall)
                        .variant(ButtonVariant::Filled)
                        .disabled(staged == 0)
                        .on_click(cx.listener(|this, _, _, cx| this.preview_commit(cx))),
                    ),
            )
            .children(banner.map(|(tone, message)| {
                div()
                    .flex_none()
                    .w_full()
                    .px(px(6.))
                    .pb(px(6.))
                    .child(Notice::new(tone, message))
            }))
            .child(self.render_grid(cx))
            // Postgres' DETAIL and HINT are unusually good, so they are shown
            // verbatim rather than folded into one line. The banner sits under
            // the grid because the grid still holds the previous, valid result
            // — a failed statement does not erase what you were looking at.
            .children(error.map(|error| {
                div().flex_none().w_full().px(px(6.)).pb(px(6.)).child(
                    Notice::new(NoticeTone::Danger, error.message.to_string())
                        .when_some(error_detail(&error), |n, d| n.detail(d)),
                )
            }))
            .into_any_element()
    }

    /// The results grid.
    ///
    /// The whole thing is one custom element in the `grid` crate rather than a
    /// tree of divs: a million rows means the per-row cost has to be zero for
    /// rows that are not on screen, and the only way to get that is to paint
    /// the visible window directly. See docs/PLAN.md §16.
    fn render_grid(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(self.pane().grid.clone())
    }

    fn dragging_target(&self) -> Option<DragTarget> {
        self.current_drag_target()
    }
}

/// The server's `DETAIL` and `HINT`, joined into the one secondary line the
/// notice has room for. Both are optional and either can carry the sentence
/// that actually explains the failure, so neither is dropped.
fn error_detail(error: &db::DbError) -> Option<String> {
    let parts: Vec<&str> = [error.detail.as_deref(), error.hint.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[allow(dead_code)]
fn _unused(_: Badge, _: BadgeStyle, _: BadgeTone, _: AnyElement, _: SharedString) {}

/// An element id that is a pane's and nobody else's. Two panes draw the same
/// buttons, and gpui keys hover and click state by id — without the pane in
/// the name, pressing Run in one would light up the other.
fn pane_id(name: &'static str, pane: PaneId, index: usize) -> ElementId {
    SharedString::from(format!("{name}-{pane}-{index}")).into()
}

/// A seam's id, which has to include the group it is in: the second seam of a
/// nested group and the second seam of its parent are different handles.
fn seam_id(path: &[usize], index: usize) -> ElementId {
    let mut name = String::from("seam");
    for step in path {
        name.push_str(&format!("-{step}"));
    }
    name.push_str(&format!("-at-{index}"));
    SharedString::from(name).into()
}

/// What a key's length counts, in that key's own words. `LLEN` and `HLEN` are
/// both "how many", but a hash has fields and a list has items, and the label
/// is the cheapest place to say which one is on screen.
fn length_label(kind: &db::KeyType) -> &'static str {
    match kind {
        db::KeyType::Hash => "Fields",
        db::KeyType::List => "Items",
        db::KeyType::Set | db::KeyType::SortedSet => "Members",
        db::KeyType::Stream => "Entries",
        db::KeyType::String | db::KeyType::Other(_) => "Length",
    }
}
