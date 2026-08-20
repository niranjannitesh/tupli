//! The new/edit connection sheet.
//!
//! The only place in the app that takes a password. It never writes one to the
//! SQLite store — [`ConnectionSheet`] hands the secret to whoever opened it, and
//! the workspace puts it in the Keychain — and it never keeps one after the
//! sheet closes.
//!
//! "Test" opens a real connection and throws it away. That is the point: a
//! connection sheet that validates the shape of a hostname tells you nothing,
//! and the only question anyone has when filling this in is whether it works.

use db::{ConnectionColor, ConnectionConfig, SafetyLevel, SslMode};
use gpui::{
    div, prelude::*, px, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Render, SharedString, Styled, Subscription, Window,
};
use gpui_tokio::Tokio;
use ui::{
    h_flex, ActiveTheme, Button, ButtonSize, ButtonVariant, FormRow, Notice, NoticeTone, Segmented,
    Sheet,
};

use editor::{Input, InputSize};

use crate::tint::{tint, PALETTE};

/// How the last "Test" went.
enum Test {
    Untried,
    Running,
    Reached(SharedString),
    Failed(SharedString),
}

pub enum SheetEvent {
    Dismissed,
    /// Save it. The password is `None` when the field was left alone on an
    /// existing connection, which means "keep whatever is in the Keychain" —
    /// distinct from `Some("")`, which means "there is no password".
    Saved {
        config: ConnectionConfig,
        password: Option<String>,
    },
}

pub struct ConnectionSheet {
    /// The identity being edited. Only the id and the fields with no control of
    /// their own survive from this; everything else is read out of the inputs.
    base: ConnectionConfig,
    editing: bool,
    focus: FocusHandle,

    name: Entity<Input>,
    host: Entity<Input>,
    port: Entity<Input>,
    database: Entity<Input>,
    user: Entity<Input>,
    password: Entity<Input>,
    /// Whether the password field has been touched, which is what distinguishes
    /// "unchanged" from "cleared".
    password_edited: bool,

    ssl: SslMode,
    color: ConnectionColor,
    safety: SafetyLevel,
    test: Test,

    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SheetEvent> for ConnectionSheet {}

impl Focusable for ConnectionSheet {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ConnectionSheet {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::for_config(ConnectionConfig::default(), false, cx)
    }

    pub fn editing(config: ConnectionConfig, cx: &mut Context<Self>) -> Self {
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
        let port = field(&config.port.to_string(), "5432", cx);
        let database = field(&config.database, "postgres", cx);
        let user = field(&config.user, &whoami_or_postgres(), cx);
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
            ssl: config.ssl_mode,
            color: config.color,
            safety: config.safety,
            base: config,
            editing,
            focus: cx.focus_handle(),
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
    fn collect(&self, cx: &gpui::App) -> ConnectionConfig {
        let text = |input: &Entity<Input>| input.read(cx).text(cx).trim().to_string();
        let host = text(&self.host);
        let name = text(&self.name);
        ConnectionConfig {
            id: self.base.id,
            // An unnamed connection is named after where it goes, which is what
            // the person would have typed anyway.
            name: if name.is_empty() { host.clone() } else { name },
            group: self.base.group.clone(),
            host: if host.is_empty() {
                "localhost".into()
            } else {
                host
            },
            port: text(&self.port).parse().unwrap_or(5432),
            database: text(&self.database),
            user: text(&self.user),
            ssl_mode: self.ssl,
            ssl_cert: self.base.ssl_cert.clone(),
            ssl_key: self.base.ssl_key.clone(),
            ssl_root_cert: self.base.ssl_root_cert.clone(),
            color: self.color,
            safety: self.safety,
            keep_alive: self.base.keep_alive,
        }
    }

    /// `None` means "leave the Keychain alone".
    fn password(&self, cx: &gpui::App) -> Option<String> {
        if self.editing && !self.password_edited {
            return None;
        }
        Some(self.password.read(cx).text(cx))
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
            let connection = db_pg::PgConnection::connect(&config, password.as_deref()).await?;
            let version = connection.scalar("select version()").await?;
            Ok::<_, db::DbError>(version)
        });

        cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| {
                this.test = match outcome {
                    Ok(Ok(version)) => Test::Reached(short_version(&version).into()),
                    Ok(Err(error)) => Test::Failed(error.message.to_string().into()),
                    Err(error) => Test::Failed(error.to_string().into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let config = self.collect(cx);
        let password = self.password(cx);
        cx.emit(SheetEvent::Saved { config, password });
    }
}

impl Render for ConnectionSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors().clone();
        let config = self.collect(cx);
        let problems = config.problems();
        let ssl = self.ssl;
        let safety = self.safety;
        let color = self.color;

        Sheet::new(
            "connection-sheet",
            if self.editing {
                "Edit Connection"
            } else {
                "New Connection"
            },
        )
        .subtitle(config.endpoint())
        .width(px(520.))
        .on_dismiss(cx.listener(|_, _, _, cx| cx.emit(SheetEvent::Dismissed)))
        .child(FormRow::new("Name").child(self.name.clone()))
        .child(
            FormRow::new("Host")
                .child(self.host.clone())
                .trailing(div().w(px(72.)).child(self.port.clone())),
        )
        .child(FormRow::new("Database").child(self.database.clone()))
        .child(FormRow::new("User").child(self.user.clone()))
        .child(
            FormRow::new("Password")
                .child(self.password.clone())
                .hint("Stored in the macOS Keychain, never in tupli's own database."),
        )
        .child(
            FormRow::new("SSL")
                .child(
                    Segmented::new("ssl-mode", SslMode::ALL.map(|mode| mode.as_str()))
                        .selected(SslMode::ALL.iter().position(|m| *m == ssl).unwrap_or(3))
                        .on_select({
                            let sheet = cx.entity();
                            move |index, _, cx| {
                                sheet.update(cx, |this, cx| {
                                    this.ssl = SslMode::ALL[index];
                                    this.test = Test::Untried;
                                    cx.notify();
                                });
                            }
                        }),
                )
                .hint(ssl_hint(ssl)),
        )
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
                            let sheet = cx.entity();
                            move |index, _, cx| {
                                sheet.update(cx, |this, cx| {
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
            Test::Running => Some(Notice::new(NoticeTone::Info, "Connecting…")),
            Test::Reached(version) => {
                Some(Notice::new(NoticeTone::Success, "Connected").detail(version.clone()))
            }
            Test::Failed(message) => {
                Some(Notice::new(NoticeTone::Danger, "Could not connect").detail(message.clone()))
            }
        })
        .children(
            problems
                .first()
                .map(|problem| Notice::new(NoticeTone::Warning, problem.to_string())),
        )
        .footer_start(
            Button::new("test", "Test")
                .size(ButtonSize::Small)
                .on_click(cx.listener(|this, _, _, cx| this.test_connection(cx))),
        )
        .footer_end(
            Button::new("cancel", "Cancel")
                .size(ButtonSize::Small)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(SheetEvent::Dismissed))),
        )
        .footer_end(
            Button::new("save", if self.editing { "Save" } else { "Connect" })
                .variant(ButtonVariant::Accent)
                .size(ButtonSize::Small)
                .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
        )
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

/// `PostgreSQL 16.4 (Homebrew) on aarch64-apple-darwin…` → the first six words.
/// The full banner is three lines of build flags nobody asked for.
fn short_version(banner: &str) -> String {
    banner
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

fn whoami_or_postgres() -> String {
    std::env::var("USER").unwrap_or_else(|_| "postgres".into())
}
