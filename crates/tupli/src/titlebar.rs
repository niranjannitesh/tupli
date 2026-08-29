//! The window titlebar.
//!
//! Painted on the window ground with no fill of its own — the cards below float
//! on the same plane, so a filled bar would read as a fifth panel welded to the
//! top edge. macOS traffic lights are left in place, which is why the bar starts
//! with a fixed gutter.
//!
//! Two groups and nothing else. On the left, next to the traffic lights, the
//! two things that make something new: a query and a connection. On the right,
//! the three panel toggles, grouped rather than scattered to the side each one
//! controls — split up, the "show the inspector" button would live 1400px from
//! the "show the sidebar" button and the pair would never read as one control
//! set; grouped, they are a single three-state layout switch you learn once.
//!
//! Nothing else earns a seat. Run, search, the appearance switch and settings
//! were all here once, and each one was a second door to something that already
//! had a nearer one — Run to the editor toolbar's own button and ⌘⏎, search to
//! ⌘K, the other two to the Settings window and the app menu. A bar that
//! repeats what is already within reach is not a shortcut, it is decoration on
//! the one strip that should be quiet enough to read a connection name from.
//!
//! Two things here are taken from the reference and are worth stating because
//! both look like omissions otherwise. The glyphs are larger than anywhere else
//! in the app and stand further apart: this is the one strip that is read at a
//! glance from across the desk, and the shapes have to survive that. And a
//! toggled-on control gets no filled box — it is simply brighter than the ones
//! that are off. Six little grey rectangles across the top would be six things
//! competing with the window's actual content, and the icons already change
//! shape when they latch, so the box was saying it twice.

use gpui::{
    div, prelude::*, px, AnyView, App, ClickEvent, IntoElement, MouseButton, ParentElement,
    RenderOnce, SharedString, Window,
};
use ui::{h_flex, ActiveTheme, Icon, IconColor, IconName, IconSize, Label, Tooltip};

/// What the primary action would do right now.
///
/// One verb whose meaning depends on what is on screen: a script tab has a
/// statement to send, a browsed table has none at all — "run" there can only
/// mean asking the server for the rows again. ⌘⏎ and the editor toolbar's Run
/// button both dispatch through this, so the tab decides the action once and
/// every door into it agrees.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum RunAction {
    /// Send the statement under the cursor. The default, and what a query tab
    /// always means.
    #[default]
    Run,
    /// Ask again for what this tab is already showing — the rows of a browsed
    /// table, or the catalog behind a structure editor.
    Reload,
    /// Something is in flight, and the same button is how it stops.
    Cancel,
    /// No connection to send anything down.
    Offline,
}

/// Width reserved for the close/minimise/zoom buttons. AppKit draws them 14px
/// across and 23px apart, so starting at x=12 (see `traffic_light_position` in
/// `main`) the three of them end at x=72. A control's glyph starts 6px inside
/// its hit area, which leaves 24px of air between the last light and the first
/// glyph: the system's own buttons read as the window's, not as the first item
/// of our toolbar, and that only happens when the gap to our controls is
/// clearly wider than the gaps within the cluster.
const TRAFFIC_LIGHT_GUTTER: f32 = 90.;

/// Side of a titlebar control's hit area. Bigger than the 24px chrome button
/// because the bar has the room and these are the app's coarsest targets.
const HIT: f32 = 26.;

/// Between controls of the same group. Small: each control already carries
/// [`HIT`] of padding around a 14px glyph, so the gap the eye sees is this
/// plus twelve. Between groups it is the divider that does the spacing.
const GAP: f32 = 2.;

type Handler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Titlebar {
    /// The saved connection: "analytics", "prod-eu", whatever it was named.
    connection: SharedString,
    /// The database open on it, which is the half that changes while you work.
    database: Option<SharedString>,
    left_panel_open: bool,
    bottom_dock_open: bool,
    right_panel_open: bool,
    connected: bool,
    on_toggle_left: Option<Handler>,
    on_toggle_bottom: Option<Handler>,
    on_toggle_right: Option<Handler>,
    on_new_query: Option<Handler>,
    on_new_connection: Option<Handler>,
    sidebar: Option<gpui::Pixels>,
    vibrant: bool,
}

impl Titlebar {
    pub fn new(connection: impl Into<SharedString>) -> Self {
        Self {
            connection: connection.into(),
            database: None,
            left_panel_open: true,
            bottom_dock_open: true,
            right_panel_open: false,
            connected: true,
            on_toggle_left: None,
            on_toggle_bottom: None,
            on_toggle_right: None,
            on_new_query: None,
            on_new_connection: None,
            sidebar: None,
            vibrant: false,
        }
    }

    /// How far the sidebar reaches, so the bar can carry its plane and its
    /// seam across the band above it.
    ///
    /// A sidebar that stops at the titlebar is a panel with something resting
    /// on it; one whose colour and edge run to the top of the window is a
    /// column, which is what every macOS window with a sidebar looks like and
    /// what ours is. The traffic lights end up over the sidebar for the same
    /// reason they do in Finder — that is where the top-left of the window is.
    pub fn sidebar(mut self, width: Option<gpui::Pixels>) -> Self {
        self.sidebar = width;
        self
    }

    /// Take the translucent tints, because the window is letting the desktop
    /// through and this band is the top of it.
    pub fn vibrant(mut self, on: bool) -> Self {
        self.vibrant = on;
        self
    }

    pub fn database(mut self, database: impl Into<SharedString>) -> Self {
        self.database = Some(database.into());
        self
    }

    pub fn panels(mut self, left: bool, bottom: bool, right: bool) -> Self {
        self.left_panel_open = left;
        self.bottom_dock_open = bottom;
        self.right_panel_open = right;
        self
    }

    pub fn on_toggle_left(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle_left = Some(Box::new(f));
        self
    }

    pub fn on_toggle_bottom(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle_bottom = Some(Box::new(f));
        self
    }

    pub fn on_toggle_right(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle_right = Some(Box::new(f));
        self
    }

    pub fn on_new_query(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_new_query = Some(Box::new(f));
        self
    }

    pub fn on_new_connection(
        mut self,
        f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_new_connection = Some(Box::new(f));
        self
    }

    pub fn connected(mut self, connected: bool) -> Self {
        self.connected = connected;
        self
    }
}

impl RenderOnce for Titlebar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors().clone();
        let height = cx.metrics().titlebar_height;
        let radius = cx.metrics().radius_sm;

        // One control. Not `ui::Button`, which tops out at a 14px glyph and
        // draws a filled box when latched; see the module note.
        let button = {
            let (hover, active) = (c.hover, c.active);
            move |id: &'static str,
                  icon: Icon,
                  tip: (SharedString, Option<&'static str>),
                  handler: Option<Handler>| {
                // Boxed because the two builders are two closure types and this
                // is one call site; nine of these are built per frame and each
                // is a pointer, which is not the thing to economise on.
                let (name, key) = tip;
                let tooltip: Box<dyn Fn(&mut Window, &mut App) -> AnyView> = match key {
                    Some(key) => Box::new(Tooltip::key(name, key)),
                    None => Box::new(Tooltip::text(name)),
                };
                h_flex()
                    .id(id)
                    .flex_none()
                    .size(px(HIT))
                    .justify_center()
                    .rounded(radius)
                    .hover(move |s| s.bg(hover))
                    .active(move |s| s.bg(active))
                    // A click on chrome must not also be read as a drag of the
                    // window, which is what the bar's own mouse-down does.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(icon.size(IconSize::Medium))
                    .tooltip(move |window, cx| tooltip(window, cx))
                    .when_some(handler, |el, f| {
                        el.on_click(move |e, window, cx| f(e, window, cx))
                    })
            }
        };
        let plain = |name: IconName| Icon::new(name).color(IconColor::Default);
        // The three toggles are one glyph each, not two: open shades the region
        // the panel occupies, closed leaves the same frame empty. A pair of
        // different icons would make the control look like it had moved.
        let shaded = c.text.opacity(0.32);
        let toggle = move |name: IconName, on: bool| match on {
            true => Icon::new(name)
                .color(IconColor::Default)
                .duo_color(IconColor::Custom(shaded)),
            false => Icon::new(name).color(IconColor::Subtle).flat(),
        };

        // The title is centred on the *window*, not on the space left over
        // between the two control groups — otherwise it slides sideways every
        // time a panel is toggled and the groups change width.
        let title = h_flex()
            .absolute()
            .inset_0()
            .justify_center()
            .items_center()
            .child(
                // A readout, not a control. What database a tab is on is the
                // tab's business and is changed from the breadcrumb inside it;
                // a second switcher up here changes a different tab's
                // connection from the one place in the window that is furthest
                // from the thing you are pointing at.
                h_flex()
                    .h(px(HIT))
                    .max_w(px(460.))
                    .px(px(8.))
                    .gap(px(7.))
                    // Nothing is shown while the connection is up: a green dot
                    // that is always green is a light nobody looks at. The dot
                    // appears when it has something to say.
                    .when(!self.connected, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .size(px(7.))
                                .rounded_full()
                                .bg(c.text_disabled),
                        )
                    })
                    // Connection and database are peers, in the same weight and
                    // colour, joined by the arrow: one path, read left to right.
                    // Making the database louder than the server it is on
                    // implies a hierarchy that is the wrong way round.
                    .child(Label::new(self.connection).medium())
                    .children(self.database.map(|database| {
                        h_flex()
                            .gap(px(7.))
                            .child(
                                Icon::new(IconName::ArrowTurn)
                                    .size(IconSize::Small)
                                    .color(IconColor::Disabled),
                            )
                            .child(Label::new(database).medium())
                    })),
            );

        // Painted rather than inherited from the window ground, because the
        // ground is nothing at all when the window is translucent — and this
        // band, unlike the regions below, has no fill of its own to fall back
        // on. Only the sidebar's stretch gives way. The rest of the band is the
        // top edge of the page, and the page is opaque; a toolbar that let the
        // desktop through above solid content would read as a gap in the
        // window rather than as glass.
        let panel = match self.vibrant {
            true => c.panel_vibrant(),
            false => c.panel,
        };
        // Two pieces that meet, not one with the other laid over it. Where the
        // window is see-through a tint over a tint saturates to opaque, so the
        // frame's stretch of the band starts where the sidebar's ends.
        let ground = div()
            .absolute()
            .inset_0()
            .flex()
            .children(self.sidebar.map(|width| {
                div()
                    .flex_none()
                    .w(width)
                    .h_full()
                    .bg(panel)
                    .border_r_1()
                    .border_color(c.seam)
            }))
            .child(div().flex_1().h_full().bg(c.background));

        h_flex()
            .id("titlebar")
            .relative()
            .h(height)
            .w_full()
            .flex_none()
            .child(ground)
            .pl(px(TRAFFIC_LIGHT_GUTTER))
            .pr(px(10.))
            // Dragging and double-click-to-zoom are ours to implement: the
            // window is opened with `app_owns_titlebar_drag`, which tells AppKit
            // to keep its hands off the whole view. Everything above that has a
            // job of its own stops the event before it arrives here, so the
            // gesture only fires on chrome nobody claimed.
            .on_mouse_down(MouseButton::Left, |e, window, _| {
                if e.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
            .child(title)
            // ---- new things, left ------------------------------------------
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(GAP))
                    .child(button(
                        "new-query",
                        plain(IconName::Plus),
                        ("New Query".into(), Some("⌘T")),
                        self.on_new_query,
                    ))
                    .child(button(
                        "new-connection",
                        plain(IconName::Database),
                        ("New Connection".into(), Some("⌘N")),
                        self.on_new_connection,
                    )),
            )
            // ---- layout switch, right --------------------------------------
            .child(div().flex_1())
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(GAP))
                    .child(button(
                        "toggle-left-panel",
                        toggle(IconName::PanelLeft, self.left_panel_open),
                        ("Database Tree".into(), Some("⌘1")),
                        self.on_toggle_left,
                    ))
                    .child(button(
                        "toggle-bottom-dock",
                        toggle(IconName::PanelBottom, self.bottom_dock_open),
                        ("Results".into(), Some("⌘2")),
                        self.on_toggle_bottom,
                    ))
                    .child(button(
                        "toggle-right-panel",
                        toggle(IconName::PanelRight, self.right_panel_open),
                        ("Inspector".into(), Some("⌘3")),
                        self.on_toggle_right,
                    )),
            )
    }
}
