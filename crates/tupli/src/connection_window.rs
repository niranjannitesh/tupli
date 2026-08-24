//! The connection window (§5.10).
//!
//! A window of its own rather than a sheet over the workspace, for the same
//! reason Settings is one: filling in a connection is not about the query
//! behind it, it takes long enough that you want to be able to look something
//! up while it is open, and it wants room for the list of what you already
//! have. The sheet could only ever show one connection at a time.
//!
//! It is also the only place in the app that takes a password. The secret is
//! never written to the SQLite store — the window hands it to the workspace,
//! which puts it in the Keychain — and it is never kept after the window
//! closes.
//!
//! "Test" opens a real connection and throws it away. That is the point: a
//! form that validates the shape of a hostname tells you nothing, and the only
//! question anyone has when filling this in is whether it works.

use db::{ConnectionColor, ConnectionConfig, Engine, SafetyLevel, SslMode};
use gpui::{
    div, point, prelude::*, px, size, App, Bounds, Context, Entity, FocusHandle, Focusable,
    IntoElement, KeyDownEvent, ParentElement, Render, SharedString, Styled, Subscription,
    TitlebarOptions, WeakEntity, Window, WindowBounds, WindowHandle, WindowOptions,
};
use gpui_tokio::Tokio;
use ui::{
    h_flex, v_flex, ActiveTheme, Button, ButtonSize, ButtonVariant, Divider, FormRow, IconColor,
    IconName, Label, LabelSize, ListItem, Notice, NoticeTone, SectionHeader, Segmented,
};

use editor::{Input, InputSize};

use crate::tint::{tint, PALETTE};
use crate::workspace::Workspace;

/// Wide enough for the form's labels and its widest control — the five SSL
/// modes in a row — with the saved list beside it.
const WINDOW_SIZE: (f32, f32) = (800., 600.);
/// The saved-connection list. Fits a name and its endpoint underneath.
const SIDEBAR_WIDTH: gpui::Pixels = px(216.);
/// Both footers, pinned rather than grown from their contents.
///
/// The two sit side by side with a rule above each, and padding around a
/// button gives the taller one a higher rule — a seam across the bottom of the
/// window that reads as a mistake because it is one.
const FOOTER_HEIGHT: gpui::Pixels = px(44.);

/// Open the connection window, on `config` if there is one and on a blank form
/// if there is not.
pub fn open(
    workspace: WeakEntity<Workspace>,
    config: Option<ConnectionConfig>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<ConnectionWindow>> {
    let bounds = Bounds::centered(None, size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Connections".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.), px(9.))),
            }),
            ..Default::default()
        },
        |_window, cx| cx.new(|cx| ConnectionWindow::new(workspace, config, cx)),
    )
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

pub struct ConnectionWindow {
    /// Weak: the main window owns the app's state, and this window must not be
    /// the reason it stays alive.
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    booted: bool,
    /// The connection being filled in. Replaced outright when another one is
    /// picked out of the list, so a half-typed password can never leak from
    /// one form into the next.
    form: Entity<ConnectionForm>,
    /// Armed by the first click on Delete and read by the second. A two-step
    /// button rather than a dialogue: deleting a connection closes every tab
    /// on it, which is worth a second click but not a modal.
    confirming_delete: Option<uuid::Uuid>,
}

impl ConnectionWindow {
    fn new(
        workspace: WeakEntity<Workspace>,
        config: Option<ConnectionConfig>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            form: Self::build_form(config, cx),
            workspace,
            focus: cx.focus_handle(),
            booted: false,
            confirming_delete: None,
        }
    }

    fn build_form(
        config: Option<ConnectionConfig>,
        cx: &mut Context<Self>,
    ) -> Entity<ConnectionForm> {
        let form = cx.new(|cx| match config {
            Some(config) => ConnectionForm::editing(config, cx),
            None => ConnectionForm::new(cx),
        });
        // A keystroke anywhere in the form changes what the footer says, so the
        // window repaints with it.
        cx.observe(&form, |_, _, cx| cx.notify()).detach();
        form
    }

    /// Show `config`, or a blank form when it is `None`. The path a second
    /// "New Connection…" takes while the window is already up.
    pub fn show(
        &mut self,
        config: Option<ConnectionConfig>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.form = Self::build_form(config, cx);
        self.confirming_delete = None;
        window.focus(&self.form.read(cx).name.focus_handle(cx), cx);
        cx.notify();
    }

    fn connections(&self, cx: &App) -> Vec<ConnectionConfig> {
        self.workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).connections.clone())
            .unwrap_or_default()
    }

    /// Which saved connection the form is on, if it is on one.
    fn selected(&self, cx: &App) -> Option<uuid::Uuid> {
        let form = self.form.read(cx);
        form.editing.then_some(form.base.id)
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (config, password) = {
            let form = self.form.read(cx);
            (form.collect(cx), form.password(cx))
        };
        let saved = self
            .workspace
            .update(cx, |workspace, cx| {
                workspace.save_connection(&config, password, cx);
                workspace.raise(cx);
            })
            .is_ok();
        // Only get out of the way once the connection has somewhere to go. A
        // workspace that has gone away leaves the form up rather than throwing
        // away what was typed into it.
        if saved {
            window.remove_window();
        }
    }

    fn delete(&mut self, id: uuid::Uuid, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirming_delete != Some(id) {
            self.confirming_delete = Some(id);
            cx.notify();
            return;
        }
        let _ = self
            .workspace
            .update(cx, |workspace, cx| workspace.delete_connection(id, cx));
        self.show(None, window, cx);
    }

    /// ⎋ and ⌘W both mean "I am done here". Nothing is committed until Connect,
    /// so there is nothing to ask about on the way out.
    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, _cx: &mut Context<Self>) {
        let k = &event.keystroke;
        if k.key == "escape" || (k.key == "w" && k.modifiers.platform) {
            window.remove_window();
        }
    }
}

impl Focusable for ConnectionWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ConnectionWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.booted {
            self.booted = true;
            // Straight into the first field: the window is only ever opened to
            // type into it.
            window.focus(&self.form.read(cx).name.focus_handle(cx), cx);
        }

        let c = cx.colors().clone();
        let m = cx.metrics().clone();
        let ty = cx.typography().clone();

        v_flex()
            .id("connections")
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
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(
                                v_flex()
                                    .id("connection-form")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    // The traffic lights sit over the top-left
                                    // of the window; the form starts below them.
                                    .pt(m.titlebar_height)
                                    .px(px(24.))
                                    .pb(px(16.))
                                    .child(self.form.clone()),
                            )
                            .child(Divider::horizontal())
                            .child(self.footer(cx)),
                    ),
            )
    }
}

impl ConnectionWindow {
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors().clone();
        let m = cx.metrics().clone();
        let connections = self.connections(cx);
        let selected = self.selected(cx);
        let confirming = self.confirming_delete;

        v_flex()
            .w(SIDEBAR_WIDTH)
            .flex_none()
            .h_full()
            .bg(c.chrome)
            .child(
                v_flex()
                    .id("saved-connections")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    // Clear of the traffic lights, like the main window's
                    // sidebar.
                    .pt(m.titlebar_height)
                    .px(px(6.))
                    .gap(px(1.))
                    .child(
                        div().px(px(6.)).pb(px(4.)).child(
                            SectionHeader::new("Saved").end_child(
                                Label::new(connections.len().to_string())
                                    .size(LabelSize::Small)
                                    .color(IconColor::Subtle),
                            ),
                        ),
                    )
                    .when(connections.is_empty(), |el| {
                        el.child(
                            div().px(px(6.)).py(px(4.)).child(
                                Label::new("Nothing saved yet.")
                                    .size(LabelSize::Small)
                                    .color(IconColor::Subtle),
                            ),
                        )
                    })
                    .children(connections.iter().enumerate().map(|(index, config)| {
                        let config = config.clone();
                        let id = config.id;
                        let tint = crate::tint::tint(config.color, cx);
                        // Name only. The endpoint is a line of its own in the
                        // form header, and putting it here too costs the name
                        // the width it needs to be a name.
                        ListItem::new(("connection", index), config.display_name())
                            .icon(IconName::Plug)
                            .icon_color(match tint {
                                Some(tint) => IconColor::Custom(tint),
                                None => IconColor::Muted,
                            })
                            .selected(selected == Some(id))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show(Some(config.clone()), window, cx)
                            }))
                    })),
            )
            .child(Divider::horizontal())
            .child(
                h_flex()
                    .h(FOOTER_HEIGHT)
                    .flex_none()
                    .px(px(8.))
                    .gap(px(6.))
                    .child(
                        Button::new("new", "New")
                            .start_icon(IconName::Plus)
                            .size(ButtonSize::Small)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.show(None, window, cx)),
                            ),
                    )
                    .children(selected.map(|id| {
                        let armed = confirming == Some(id);
                        Button::new("delete", if armed { "Delete?" } else { "Delete" })
                            .size(ButtonSize::Small)
                            .when(armed, |button| button.variant(ButtonVariant::Danger))
                            .on_click(
                                cx.listener(move |this, _, window, cx| this.delete(id, window, cx)),
                            )
                    })),
            )
    }

    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = self.form.read(cx).editing;
        h_flex()
            .h(FOOTER_HEIGHT)
            .flex_none()
            .px(px(16.))
            .gap(px(8.))
            .justify_between()
            .child(
                Button::new("test", "Test")
                    .size(ButtonSize::Small)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.form.update(cx, |form, cx| form.test_connection(cx))
                    })),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        Button::new("close", "Close")
                            .size(ButtonSize::Small)
                            .on_click(|_, window, _| window.remove_window()),
                    )
                    .child(
                        Button::new("save", if editing { "Save" } else { "Connect" })
                            .variant(ButtonVariant::Accent)
                            .size(ButtonSize::Small)
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}

// ---------------------------------------------------------------------------
// The form
// ---------------------------------------------------------------------------

/// How the last "Test" went.
enum Test {
    Untried,
    Running,
    Reached(SharedString),
    Failed(SharedString),
}

pub struct ConnectionForm {
    /// The identity being edited. Only the id and the fields with no control of
    /// their own survive from this; everything else is read out of the inputs.
    base: ConnectionConfig,
    editing: bool,

    name: Entity<Input>,
    host: Entity<Input>,
    port: Entity<Input>,
    database: Entity<Input>,
    user: Entity<Input>,
    password: Entity<Input>,
    /// Whether the password field has been touched, which is what distinguishes
    /// "unchanged" from "cleared".
    password_edited: bool,

    engine: Engine,
    ssl: SslMode,
    color: ConnectionColor,
    safety: SafetyLevel,
    test: Test,

    _subscriptions: Vec<Subscription>,
}

impl ConnectionForm {
    fn new(cx: &mut Context<Self>) -> Self {
        Self::for_config(ConnectionConfig::default(), false, cx)
    }

    fn editing(config: ConnectionConfig, cx: &mut Context<Self>) -> Self {
        Self::for_config(config, true, cx)
    }

    fn for_config(config: ConnectionConfig, editing: bool, cx: &mut Context<Self>) -> Self {
        let field = |value: &str, placeholder: &str, cx: &mut Context<Self>| {
            let value = value.to_string();
            let placeholder = SharedString::from(placeholder.to_string());
            cx.new(|cx| {
                let input = Input::new(cx)
                    .size(InputSize::Medium)
                    .placeholder(placeholder, cx);
                input.set_text(&value, cx);
                input
            })
        };

        let name = field(&config.name, "Production", cx);
        let host = field(&config.host, "localhost", cx);
        let port = field(
            &config.port.to_string(),
            &config.engine.default_port().to_string(),
            cx,
        );
        let (database_hint, user_hint) = engine_placeholder(config.engine);
        let database = field(&config.database, &database_hint, cx);
        let user = field(&config.user, &user_hint, cx);
        let password = cx.new(|cx| {
            Input::new(cx)
                .size(InputSize::Medium)
                .placeholder(if editing { "unchanged" } else { "" }, cx)
                .masked(cx)
        });

        // A change anywhere invalidates the test result: a green "Reached" over
        // a hostname that has since been edited is a lie.
        let mut subscriptions = Vec::new();
        for input in [&name, &host, &port, &database, &user] {
            subscriptions.push(
                cx.subscribe(input, |this, _, event: &editor::EditorEvent, cx| {
                    if matches!(event, editor::EditorEvent::Changed) {
                        this.test = Test::Untried;
                        cx.notify();
                    }
                }),
            );
        }
        subscriptions.push(
            cx.subscribe(&password, |this, _, event: &editor::EditorEvent, cx| {
                if matches!(event, editor::EditorEvent::Changed) {
                    this.password_edited = true;
                    this.test = Test::Untried;
                    cx.notify();
                }
            }),
        );

        Self {
            engine: config.engine,
            ssl: config.ssl_mode,
            color: config.color,
            safety: config.safety,
            base: config,
            editing,
            name,
            host,
            port,
            database,
            user,
            password,
            password_edited: false,
            test: Test::Untried,
            _subscriptions: subscriptions,
        }
    }

    /// The config as the fields currently read.
    fn collect(&self, cx: &App) -> ConnectionConfig {
        let text = |input: &Entity<Input>| input.read(cx).text(cx).trim().to_string();
        let host = text(&self.host);
        let name = text(&self.name);
        let file = self.engine.is_file();
        ConnectionConfig {
            id: self.base.id,
            // An unnamed connection is named after where it goes, which is what
            // the person would have typed anyway. A file names itself, and it
            // does so out of a path the form has not collected yet, so the name
            // is left empty for `display_name` to answer.
            name: if name.is_empty() && !file {
                host.clone()
            } else {
                name
            },
            group: self.base.group.clone(),
            host: if host.is_empty() {
                "localhost".into()
            } else {
                host
            },
            port: text(&self.port)
                .parse()
                .unwrap_or_else(|_| self.engine.default_port()),
            database: text(&self.database),
            user: text(&self.user),
            ssl_mode: self.ssl,
            ssl_cert: self.base.ssl_cert.clone(),
            ssl_key: self.base.ssl_key.clone(),
            ssl_root_cert: self.base.ssl_root_cert.clone(),
            color: self.color,
            safety: self.safety,
            keep_alive: self.base.keep_alive,
            engine: self.engine,
        }
    }

    /// Switching engines carries the port with it, but only a port nobody
    /// chose: a typed 55432 is a tunnel, and moving it would be an edit the
    /// person did not make.
    fn set_engine(&mut self, engine: Engine, cx: &mut Context<Self>) {
        if engine == self.engine {
            return;
        }
        let port = self.port.read(cx).text(cx);
        if port.trim().parse() == Ok(self.engine.default_port()) {
            let next = engine.default_port().to_string();
            self.port.update(cx, |input, cx| input.set_text(&next, cx));
        }
        for (input, text) in [
            (&self.database, engine_placeholder(engine).0),
            (&self.user, engine_placeholder(engine).1),
            (&self.port, engine.default_port().to_string()),
        ] {
            input.update(cx, |input, cx| input.set_placeholder(text, cx));
        }
        // Same rule as the port: a TLS mode nobody chose follows the engine,
        // and one somebody chose does not move in either direction.
        if self.ssl == self.engine.default_ssl_mode() {
            self.ssl = engine.default_ssl_mode();
        }
        self.engine = engine;
        self.test = Test::Untried;
        cx.notify();
    }

    /// `None` means "leave the Keychain alone".
    fn password(&self, cx: &App) -> Option<String> {
        if self.editing && !self.password_edited {
            return None;
        }
        Some(self.password.read(cx).text(cx))
    }

    /// The open panel, for the engines whose database is a path.
    ///
    /// Typing the path works too — the field is a field — but nobody knows
    /// where an application put its SQLite file off the top of their head.
    fn choose_file(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            let path = match prompt.await {
                Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                    Some(path) => path,
                    None => return,
                },
                Ok(Ok(None)) => return,
                Ok(Err(error)) => {
                    log::warn!("the open panel did not open: {error:#}");
                    return;
                }
                Err(_) => return,
            };
            this.update(cx, |this, cx| {
                let path = path.to_string_lossy().into_owned();
                this.database
                    .update(cx, |input, cx| input.set_text(&path, cx));
                this.test = Test::Untried;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Open a connection, ask it one question, and drop it.
    fn test_connection(&mut self, cx: &mut Context<Self>) {
        if matches!(self.test, Test::Running) {
            return;
        }
        let config = self.collect(cx);
        // The typed password, not the stored one — testing an edit has to test
        // what is on screen.
        let typed = self.password(cx);
        self.test = Test::Running;
        cx.notify();

        let work = Tokio::spawn(cx, async move {
            let password = match typed {
                Some(password) => Some(password),
                None => store::secrets::password(config.id).ok().flatten(),
            };
            // Through the registry, so that testing a Redis connection tests
            // Redis. The version comes off the handshake every driver already
            // does, which is why this asks the server nothing further.
            let connection = drivers::connect(&config, password.as_deref()).await?;
            Ok::<_, db::DbError>(version_label(config.engine, &connection.server_version()))
        });

        cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| {
                this.test = match outcome {
                    Ok(Ok(version)) => Test::Reached(version.into()),
                    Ok(Err(error)) => Test::Failed(error.message.to_string().into()),
                    Err(error) => Test::Failed(error.to_string().into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for ConnectionForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors().clone();
        let config = self.collect(cx);
        let problems = config.problems();
        let ssl = self.ssl;
        let safety = self.safety;
        let engine = self.engine;
        let color = self.color;

        v_flex()
            .gap(px(14.))
            .child(
                SectionHeader::new(if self.editing {
                    "Edit connection"
                } else {
                    "New connection"
                })
                .flush()
                .end_child(
                    Label::new(config.endpoint())
                        .size(LabelSize::Small)
                        .color(IconColor::Subtle),
                ),
            )
            .child(
                FormRow::new("Engine")
                    .child(
                        Segmented::new("engine", Engine::ALL.map(|engine| engine.label()))
                            .selected(
                                Engine::ALL
                                    .iter()
                                    .position(|e| *e == engine)
                                    .unwrap_or_default(),
                            )
                            .on_select({
                                let form = cx.entity();
                                move |index, _, cx| {
                                    form.update(cx, |this, cx| {
                                        this.set_engine(Engine::ALL[index], cx)
                                    });
                                }
                            }),
                    )
                    .hint(engine_hint(engine)),
            )
            .child(FormRow::new("Name").child(self.name.clone()))
            // A file has no server, so the half of the form that describes one
            // is not hidden as a shortcut — there is nothing behind it to fill
            // in, and a greyed-out `Host` would suggest otherwise.
            .when(!engine.is_file(), |el| {
                el.child(
                    FormRow::new("Host")
                        .child(self.host.clone())
                        .trailing(div().w(px(72.)).child(self.port.clone())),
                )
            })
            .child(if engine.is_file() {
                FormRow::new("File")
                    .child(self.database.clone())
                    .trailing(
                        Button::new("choose-file", "Choose…")
                            .size(ButtonSize::Small)
                            .on_click(cx.listener(|this, _, _, cx| this.choose_file(cx))),
                    )
                    .hint("The file has to exist already: opening one never creates it.")
            } else {
                FormRow::new("Database").child(self.database.clone())
            })
            .when(!engine.is_file(), |el| {
                el.child(FormRow::new("User").child(self.user.clone()))
                    .child(
                        FormRow::new("Password")
                            .child(self.password.clone())
                            .hint("Stored in the macOS Keychain, never in tupli's own database."),
                    )
                    .child(
                        FormRow::new("SSL")
                            .child(
                                Segmented::new("ssl-mode", SslMode::ALL.map(|mode| mode.as_str()))
                                    .selected(
                                        SslMode::ALL.iter().position(|m| *m == ssl).unwrap_or(3),
                                    )
                                    .on_select({
                                        let form = cx.entity();
                                        move |index, _, cx| {
                                            form.update(cx, |this, cx| {
                                                this.ssl = SslMode::ALL[index];
                                                this.test = Test::Untried;
                                                cx.notify();
                                            });
                                        }
                                    }),
                            )
                            .hint(ssl_hint(ssl)),
                    )
            })
            .child(
                FormRow::new("Colour").child(h_flex().gap(px(6.)).children(
                    PALETTE.into_iter().enumerate().map(|(index, option)| {
                        let selected = option == color;
                        let fill = tint(option, cx);
                        div()
                            .id(("swatch", index))
                            .size(px(18.))
                            .rounded_full()
                            .cursor_pointer()
                            .border_2()
                            .border_color(if selected {
                                c.text
                            } else {
                                gpui::transparent_black()
                            })
                            .child(
                                div()
                                    .size_full()
                                    .rounded_full()
                                    .when_some(fill, |el, fill| el.bg(fill))
                                    // "No colour" is a ring, not a blank:
                                    // an empty slot reads as a bug.
                                    .when(fill.is_none(), |el| {
                                        el.border_1().border_color(c.border_strong)
                                    }),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.color = option;
                                cx.notify();
                            }))
                    }),
                )),
            )
            .child(
                FormRow::new("Safety")
                    .child(
                        Segmented::new("safety", ["Normal", "Confirm writes", "Read-only"])
                            .selected(match safety {
                                SafetyLevel::Normal => 0,
                                SafetyLevel::Confirm => 1,
                                SafetyLevel::ReadOnly => 2,
                            })
                            .on_select({
                                let form = cx.entity();
                                move |index, _, cx| {
                                    form.update(cx, |this, cx| {
                                        this.safety = match index {
                                            1 => SafetyLevel::Confirm,
                                            2 => SafetyLevel::ReadOnly,
                                            _ => SafetyLevel::Normal,
                                        };
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .hint(match safety {
                        SafetyLevel::Normal => "Edits commit like any other client.",
                        SafetyLevel::Confirm => "Every write is previewed and confirmed first.",
                        SafetyLevel::ReadOnly => "No writes at all: the grid stays read-only.",
                    }),
            )
            .children(match &self.test {
                Test::Untried => None,
                Test::Running => Some(Notice::new(NoticeTone::Info, "Connecting…").busy()),
                Test::Reached(version) => {
                    Some(Notice::new(NoticeTone::Success, "Connected").detail(version.clone()))
                }
                Test::Failed(message) => Some(
                    Notice::new(NoticeTone::Danger, "Could not connect").detail(message.clone()),
                ),
            })
            .children(
                problems
                    .first()
                    .map(|problem| Notice::new(NoticeTone::Warning, problem.to_string())),
            )
    }
}

/// What the `Database` and `User` fields fall back to, which is not the same
/// question on every engine: one is asking for a name, another for an index,
/// and a file engine for a path.
fn engine_placeholder(engine: Engine) -> (String, String) {
    match engine {
        Engine::Postgres => ("postgres".into(), whoami_or_postgres()),
        // Redis numbers its databases from zero, and the user it ships with is
        // literally called `default`.
        Engine::Redis => ("0".into(), "default".into()),
        // Where a bare table name resolves, rather than a boundary: a
        // ClickHouse session can read every database on the server.
        Engine::ClickHouse => ("default".into(), "default".into()),
        // No user to fall back to, and the field is not on screen anyway.
        Engine::Sqlite => ("~/database.sqlite".into(), String::new()),
    }
}

fn engine_hint(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "Tables, a SQL editor, and editable results.",
        Engine::Redis => "Keys by pattern, and a command line instead of SQL.",
        Engine::ClickHouse => "Tables and a SQL editor. Results are read-only.",
        Engine::Sqlite => "A file on disk. Tables, a SQL editor, editable results.",
    }
}

/// One line about what the chosen mode actually promises, because the libpq
/// names are famously misleading — `require` encrypts but verifies nothing.
fn ssl_hint(mode: SslMode) -> &'static str {
    match mode {
        SslMode::Disable => "No encryption. Only for a server on this machine.",
        SslMode::Allow | SslMode::Prefer => "Falls back to plaintext if the server refuses TLS.",
        SslMode::Require => "Encrypted, but the server's certificate is not checked.",
        SslMode::VerifyCa => "Encrypted, and the certificate chain is verified.",
        SslMode::VerifyFull => "Encrypted, chain verified, and the hostname must match.",
    }
}

/// `PostgreSQL 16.4` — what the driver said, named.
///
/// The drivers report a bare number (`16.4 (Homebrew)`, `7.2.4`) because the
/// status bar already knows which connection it is showing. Here nobody does:
/// this line is the answer to "did it reach the thing I chose", so it says
/// which thing. The trailing build details are dropped — three lines of
/// compiler flags is not a version.
fn version_label(engine: Engine, version: &str) -> String {
    match version.split_whitespace().next() {
        Some(number) => format!("{} {number}", engine.label()),
        None => engine.label().to_string(),
    }
}

fn whoami_or_postgres() -> String {
    std::env::var("USER").unwrap_or_else(|_| "postgres".into())
}
