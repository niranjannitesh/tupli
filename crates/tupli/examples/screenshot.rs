//! Renders the window to a PNG without putting it on screen.
//!
//! `cargo run -p tupli --example screenshot -- out/dir` writes `dark.png` and
//! `light.png` at 1440×900. Everything is real — the platform text system, the
//! Metal renderer, the same `Workspace` the binary builds — it simply never
//! reaches a display, which means the app can be inspected on a machine whose
//! screen is asleep and in CI.

use std::sync::Arc;

use gpui::{px, size, AppContext as _, Focusable as _, HeadlessAppContext};
use tupli::workspace::Workspace;
use ui::{Appearance, Assets, Theme, ThemeRegistry};

fn main() {
    // The workspace reports a connection it could not make through `log`, and
    // a screenshot that silently comes out empty is the one failure mode this
    // tool has. `RUST_LOG=warn` is enough to see it.
    env_logger::init();
    let out = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&out).expect("create output directory");

    // Read once, before the first pass moves it: each pass gets its own
    // `$HOME` below.
    let home = std::env::var("HOME").unwrap_or_default();

    for (appearance, name) in [(Appearance::Dark, "dark"), (Appearance::Light, "light")] {
        // A store per pass, because the first pass saves a session on the way
        // out and the second would restore it — which makes the light frame a
        // photograph of what the dark one left behind rather than a second
        // photograph of the same scenario.
        if !home.is_empty() {
            let home = std::path::PathBuf::from(&home).join(name);
            std::fs::create_dir_all(&home).expect("create the pass's home");
            std::env::set_var("HOME", &home);
        }
        // Both frames come out of one process, so the appearance is set here
        // rather than read from the environment. The workspace reads
        // `TUPLI_THEME` to decide whether to apply the machine's saved theme;
        // announcing the pass through it is what stops a theme saved by an
        // earlier run from turning the light frame dark.
        std::env::set_var(
            "TUPLI_THEME",
            if appearance.is_dark() {
                "dark"
            } else {
                "light"
            },
        );

        let mut cx = HeadlessAppContext::with_platform(
            gpui_platform::current_platform(true).text_system(),
            Arc::new(Assets),
            gpui_platform::current_headless_renderer,
        );

        cx.update(|cx| {
            // Before any theme is chosen: the registry is what a theme name resolves
            // through, and the first thing the workspace does is resolve one.
            ThemeRegistry::init(&[ThemeRegistry::user_dir(&store::paths::data_dir())], cx);
            Theme::set_global(Theme::of(appearance), cx);
            // The driver's runtime. Only needed when `TUPLI_CONNECT` is set,
            // but installing it unconditionally keeps the headless window and
            // the real one built the same way.
            gpui_tokio::init(cx);
        });

        let window = cx
            .open_window(size(px(1440.), px(900.)), |_window, cx| {
                cx.new(Workspace::new)
            })
            .expect("open headless window");

        // With `TUPLI_CONNECT` set the workspace opens a connection on its
        // first frame, and the interesting parts of the window — the tree, a
        // table's rows, its structure — only exist once the server has
        // answered. Draw, then pump until it has, with a ceiling so a server
        // that never replies fails in ten seconds rather than hanging.
        if std::env::var_os("TUPLI_CONNECT").is_some() {
            cx.update_window(window.into(), |_, window, cx| {
                let _ = window.draw(cx);
            })
            .expect("draw");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                cx.run_until_parked();
                let ready = cx.update(|cx| {
                    window
                        .read(cx)
                        .map(|workspace| {
                            workspace.is_connected(cx)
                                && !workspace.tree.is_empty()
                                && workspace.keys_settled(cx)
                        })
                        .unwrap_or(false)
                });
                // A browsed table needs its rows too, and those arrive on a
                // second round trip after the catalog.
                let opened = std::env::var_os("TUPLI_OPEN").is_none()
                    || cx.update(|cx| {
                        window
                            .read(cx)
                            .map(|workspace| {
                                // …unless there is nothing to ask for: a name
                                // this database does not have opens a tab that
                                // holds a notice and never runs a statement.
                                workspace.pane().elapsed.is_some()
                                    || workspace.absent_relation(cx).is_some()
                            })
                            .unwrap_or(false)
                    });
                // `TUPLI_FOLLOW` hops after those rows land, and the hop is
                // a second round trip of its own. Two finished statements is
                // what "the hop arrived" looks like from out here.
                let followed = std::env::var_os("TUPLI_FOLLOW").is_none()
                    || cx.update(|cx| {
                        window
                            .read(cx)
                            .map(|workspace| workspace.history_mine.len() >= 2)
                            .unwrap_or(false)
                    });
                if ready && opened && followed {
                    break;
                }
                if std::time::Instant::now() > deadline {
                    eprintln!("warning: gave up waiting for the connection");
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }

        // `TUPLI_ALSO=<database>` opens a second database on the same server,
        // which is the only way to photograph the sidebar holding two catalogs
        // at once — and the state that used to fold the first one up.
        if let Ok(database) = std::env::var("TUPLI_ALSO") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        workspace.open_database(&database, cx)
                    })
                    .expect("open a second database")
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                cx.run_until_parked();
                let ready = cx.update(|cx| {
                    window
                        .read(cx)
                        .map(|workspace| {
                            // Its own row appears the moment the session is
                            // made; what is being waited for is the catalog
                            // under it.
                            workspace.tree.iter().any(|node| {
                                node.depth > 1
                                    && node.origin.database.as_deref() == Some(&*database)
                            })
                        })
                        .unwrap_or(false)
                });
                if ready || std::time::Instant::now() > deadline {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }

        // `TUPLI_RUN=<sql>` types a statement into the console and runs it, so
        // a frame can show what a failure looks like — the squiggle under the
        // character the server named — without a hand on the keyboard. Needs
        // `TUPLI_CONNECT`, since only a server can reject anything.
        // `TUPLI_QUERY=1` opens an empty query tab first, which is what a
        // person does before typing. Without it a run lands in a window whose
        // centre is still the empty state, and the frame shows the console
        // nobody can see rather than the one the statement went into.
        if std::env::var("TUPLI_QUERY").as_deref() == Ok("1") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| workspace.new_query_tab(cx))
                    .expect("open a query tab")
            });
            cx.run_until_parked();
        }

        // `TUPLI_TABS=<n>` opens n more query tabs. The tab strip's own
        // behaviour — the menu below, a pin, the crowding that makes either
        // worth having — is invisible on a window holding one tab.
        if let Ok(count) = std::env::var("TUPLI_TABS") {
            let count: usize = count.parse().expect("TUPLI_TABS is a number of tabs");
            for _ in 0..count {
                cx.update(|cx| {
                    window
                        .update(cx, |workspace, _window, cx| workspace.new_query_tab(cx))
                        .expect("open a query tab")
                });
            }
            cx.run_until_parked();
        }

        // `TUPLI_PIN=<n>` pins that tab, which is the only way to see what a
        // pinned tab looks like without a hand on the mouse.
        if let Ok(index) = std::env::var("TUPLI_PIN") {
            let index: usize = index.parse().expect("TUPLI_PIN is a tab index");
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| workspace.toggle_pin(index, cx))
                    .expect("pin a tab")
            });
            cx.run_until_parked();
        }

        // `TUPLI_DOCK=<px>` sets the height of the results dock. A frame taken
        // for a README wants the rows to carry it, and the saved layout has no
        // opinion about that — a fresh profile opens on whatever the default
        // is, which splits the centre evenly between typing and reading.
        if let Ok(height) = std::env::var("TUPLI_DOCK") {
            let height: f32 = height.parse().expect("TUPLI_DOCK is a number of pixels");
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        workspace.dock_height = px(height);
                        cx.notify();
                    })
                    .expect("set the dock height")
            });
            cx.run_until_parked();
        }

        // `TUPLI_TYPE=<sql>` puts a statement in the console without sending
        // it, and `TUPLI_FORMAT=1` then presses Format. Together they are the
        // only way to photograph the editor's own behaviour, which owes
        // nothing to a server.
        if let Ok(sql) = std::env::var("TUPLI_TYPE") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        workspace
                            .pane()
                            .editor
                            .clone()
                            .update(cx, |editor, cx| editor.set_text(&sql, cx));
                        if std::env::var("TUPLI_FORMAT").as_deref() == Ok("1") {
                            workspace.format_query(cx);
                        }
                    })
                    .expect("type the statement")
            });
            cx.run_until_parked();
        }

        // `TUPLI_HOVER=<word>` rests the pointer on the first occurrence of a
        // word in the console. There is no mouse here, so the offset is found
        // in the text and handed to the editor directly; the clock then has to
        // be pushed past the panel's own delay.
        if let Ok(word) = std::env::var("TUPLI_HOVER") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        workspace.pane().editor.clone().update(cx, |editor, cx| {
                            let text = editor.text();
                            match text.find(&word) {
                                Some(at) => {
                                    let offset = text[..at].chars().count() + 1;
                                    editor.hover_at(Some(offset), cx);
                                }
                                None => eprintln!("warning: no {word:?} in the console"),
                            }
                        });
                    })
                    .expect("hover the word")
            });
            cx.background_executor
                .advance_clock(std::time::Duration::from_millis(500));
            cx.run_until_parked();
        }

        if let Ok(sql) = std::env::var("TUPLI_RUN") {
            // How many statements had finished before this one was sent. The
            // window may already be showing an answer — `TUPLI_OPEN` browses a
            // table, and the sample data is there from the first frame — so
            // "has a result" is not a signal that *this* statement has one.
            // Every finished run files a history row under this window, so
            // the count of those is.
            let before = cx.update(|cx| {
                window
                    .read(cx)
                    .map(|workspace| workspace.history_mine.len())
                    .unwrap_or(0)
            });
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        workspace
                            .pane()
                            .editor
                            .clone()
                            .update(cx, |editor, cx| editor.set_text(&sql, cx));
                        // `TUPLI_RUN_ALL=1` sends the whole script instead of
                        // the statement under the cursor, which is the only way
                        // to photograph a run of more than one.
                        match std::env::var("TUPLI_RUN_ALL").as_deref() {
                            Ok("1") => workspace.run_console_all(cx),
                            _ => workspace.run_console(cx),
                        }
                    })
                    .expect("run the statement")
            });
            // `TUPLI_CANCEL=1` presses Cancel on whatever `TUPLI_RUN` started,
            // which is the only way to photograph what a stopped statement
            // looks like. The request has to reach a backend that is already
            // running, so it goes out after the app has pumped a few times.
            if std::env::var("TUPLI_CANCEL").as_deref() == Ok("1") {
                for _ in 0..8 {
                    cx.run_until_parked();
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                cx.update(|cx| {
                    window
                        .update(cx, |workspace, _window, cx| workspace.cancel(cx))
                        .expect("cancel the statement")
                });
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                cx.run_until_parked();
                let landed = cx.update(|cx| {
                    window
                        .read(cx)
                        .map(|workspace| {
                            // A script is not done until the queue behind it
                            // is, or the failure that emptied it has landed.
                            workspace.history_mine.len() > before && !workspace.is_running(cx)
                        })
                        .unwrap_or(false)
                });
                if landed || std::time::Instant::now() > deadline {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }

        // `TUPLI_RESULT=<n>` picks one of a script's answers, the way clicking
        // its tab would. Zero-based, and ignored when the script produced only
        // the one result.
        if let Ok(index) = std::env::var("TUPLI_RESULT") {
            let index: usize = index.parse().expect("TUPLI_RESULT is a number");
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        workspace.show_result(index, cx)
                    })
                    .expect("show a result")
            });
        }

        // `TUPLI_FIND=<text>` opens the find bar and types into it, over the
        // console by default and over the rows with `TUPLI_FIND_ROWS=1`. The
        // surface is chosen by focus, which is a real focus here rather than a
        // flag, so the photograph exercises the same path ⌘F does.
        if let Ok(text) = std::env::var("TUPLI_FIND") {
            let rows = std::env::var("TUPLI_FIND_ROWS").as_deref() == Ok("1");
            cx.update(|cx| {
                window
                    .update(cx, |workspace, window, cx| {
                        let handle = match rows {
                            true => workspace.pane().grid.read(cx).focus_handle(cx),
                            false => workspace.pane().editor.read(cx).focus().clone(),
                        };
                        window.focus(&handle, cx);
                        workspace.open_find(Some(&*window), cx);
                        workspace
                            .pane()
                            .find
                            .clone()
                            .update(cx, |input, cx| input.set_text(&text, cx));
                    })
                    .expect("open the find bar")
            });
            cx.run_until_parked();
        }

        // `TUPLI_EDIT=1` stages one of each kind of change on the browsed
        // table, so the pending-state colours can be photographed without a
        // hand on the keyboard. Nothing is sent: staging is the whole point.
        if std::env::var("TUPLI_EDIT").as_deref() == Ok("1") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        let grid = workspace.pane().grid.clone();
                        grid.update(cx, |grid, cx| {
                            grid.set_cell(
                                1,
                                1,
                                db::Value::text(db::ValueKind::Text, "edited@example.com"),
                                cx,
                            );
                            grid.set_cursor(3, 0, false, cx);
                            grid.delete_rows(cx);
                            grid.add_row(cx);
                            grid.set_cell(
                                grid.row_count() - 1,
                                1,
                                db::Value::text(db::ValueKind::Text, "new@example.com"),
                                cx,
                            );
                            grid.set_cursor(6, 2, false, cx);
                        });
                    })
                    .expect("stage edits")
            });
        }

        // `TUPLI_EDIT=cell` opens the inline editor over a cell instead of
        // staging anything, which is the one part of the write path that has a
        // caret in it.
        if std::env::var("TUPLI_EDIT").as_deref() == Ok("cell") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        let grid = workspace.pane().grid.clone();
                        grid.update(cx, |grid, cx| {
                            grid.set_cursor(2, 2, false, cx);
                            grid.edit_cursor(None, cx);
                        });
                    })
                    .expect("open the cell editor")
            });
            cx.run_until_parked();
        }

        // `TUPLI_FIELD=<n>` opens one of the row inspector's fields, which is
        // the only way to photograph a laid-out document: collapsed, every
        // field is four lines at most.
        if let Ok(field) = std::env::var("TUPLI_FIELD") {
            let field: usize = field.parse().expect("TUPLI_FIELD is a column index");
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        workspace.inspector_tab = tupli::workspace::InspectorTab::Row;
                        workspace.expand_field(field, cx);
                    })
                    .expect("open a field")
            });
            cx.run_until_parked();
        }

        // `TUPLI_MENU=row` selects a few rows and opens the grid's context menu
        // over them. Driven through the workspace rather than through a
        // synthetic right click, because a mouse event in an offscreen window
        // has nowhere to land.
        // `TUPLI_MENU=tab:<n>` opens the tab strip's menu on that tab. The
        // point is the pointer's, and a right click lands inside the tab, so
        // it is measured from where the strip puts tab n rather than given.
        if let Some(index) = std::env::var("TUPLI_MENU")
            .ok()
            .and_then(|menu| menu.strip_prefix("tab:").map(str::to_string))
        {
            let index: usize = index.parse().expect("TUPLI_MENU=tab:<n> takes a tab index");
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        let pane = workspace.active_pane;
                        let at = gpui::point(px(300. + 150. * index as f32), px(96.));
                        workspace.open_tab_menu(at, pane, index, cx)
                    })
                    .expect("open the tab menu")
            });
            cx.run_until_parked();
        }

        if std::env::var("TUPLI_MENU").as_deref() == Ok("row") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        let grid = workspace.pane().grid.clone();
                        grid.update(cx, |grid, cx| {
                            grid.set_cursor(2, 1, false, cx);
                            grid.set_cursor(4, 1, true, cx);
                        });
                        workspace.open_row_menu(gpui::point(px(520.), px(560.)), 4, 1, cx);
                    })
                    .expect("open the row menu")
            });
            cx.run_until_parked();
        }

        // `TUPLI_COMMIT=1` sends the staged changes and waits for the server,
        // which is how the write path is exercised end to end without a hand
        // on the keyboard. Destructive by design — point it at a scratch
        // database.
        if std::env::var("TUPLI_COMMIT").as_deref() == Ok("1") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| workspace.commit_changes(cx))
                    .expect("commit")
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                cx.run_until_parked();
                let done = cx.update(|cx| {
                    window
                        .read(cx)
                        .map(|workspace| !workspace.is_running(cx))
                        .unwrap_or(true)
                });
                if done || std::time::Instant::now() > deadline {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            // The refresh a successful commit asks for is another round trip.
            for _ in 0..10 {
                cx.run_until_parked();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }

        // `TUPLI_SHEET=commit` puts the confirmation sheet up over the staged
        // changes, which is the last thing anybody sees before a write.
        if std::env::var("TUPLI_SHEET").as_deref() == Ok("commit") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| workspace.preview_commit(cx))
                    .expect("open the commit sheet")
            });
        }

        // `TUPLI_SHEET=save` puts the save-query sheet up over whatever else
        // was asked for, so the modal can be reviewed without a hand on the
        // keyboard. The editor holds the mock script either way.
        if std::env::var("TUPLI_SHEET").as_deref() == Ok("save") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| workspace.save_query_as(cx))
                    .expect("open the save sheet")
            });
        }

        // `TUPLI_SHEET=export` puts the export sheet up over the browsed rows
        // with a selection made first, because the row choice is only offered
        // when there is one. `export-all` skips the selection, which is the
        // other of the two states the sheet has.
        let sheet = std::env::var("TUPLI_SHEET");
        if matches!(sheet.as_deref(), Ok("export") | Ok("export-all")) {
            let select = sheet.as_deref() == Ok("export");
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        if select {
                            let grid = workspace.pane().grid.clone();
                            grid.update(cx, |grid, cx| {
                                grid.set_cursor(1, 0, false, cx);
                                grid.set_cursor(3, 0, true, cx);
                            });
                        }
                        workspace.open_export(cx);
                    })
                    .expect("open the export sheet")
            });
        }

        // `TUPLI_EXPORT=<format>:<path>` runs the write the sheet's Export
        // button would have run, with the save panel skipped — a native panel
        // has nothing to open onto in an offscreen window, and the file is the
        // only part of an export worth checking anyway.
        if let Ok(spec) = std::env::var("TUPLI_EXPORT") {
            let (format, path) = spec
                .split_once(':')
                .expect("TUPLI_EXPORT is <format>:<path>");
            let format = match format {
                "csv" => grid::Format::Csv,
                "tsv" => grid::Format::Tsv { headers: true },
                "json" => grid::Format::Json,
                "sql" => grid::Format::Sql,
                "markdown" => grid::Format::Markdown,
                other => panic!("no such export format: {other}"),
            };
            let path = std::path::PathBuf::from(path);
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        workspace.export_rows_to(path, format, grid::Rows::All, cx)
                    })
                    .expect("export the rows")
            });
            for _ in 0..10 {
                cx.run_until_parked();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }

        // `TUPLI_IMPORT=<path>` opens the import sheet on a file, with the open
        // panel skipped for the same reason `TUPLI_EXPORT` skips the save one.
        // `TUPLI_IMPORT_RUN` also presses Import, which writes to the server —
        // so it is a separate flag rather than the same one.
        if let Ok(path) = std::env::var("TUPLI_IMPORT") {
            let path = std::path::PathBuf::from(path);
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| workspace.import_file(path, cx))
                    .expect("open the import sheet")
            });
            cx.run_until_parked();
            if std::env::var("TUPLI_IMPORT_RUN").is_ok() {
                cx.update(|cx| {
                    window
                        .update(cx, |workspace, _window, cx| workspace.confirm_import(cx))
                        .expect("run the import")
                });
                for _ in 0..10 {
                    cx.run_until_parked();
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }

        // `TUPLI_SAVE_AS=<name>` runs the save the sheet would have run, so the
        // Queries tab can be captured with something actually in it. It writes
        // to the store under `$HOME`, so point that somewhere scratch.
        if let Ok(name) = std::env::var("TUPLI_SAVE_AS") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        workspace.save_query_named(name.clone(), cx)
                    })
                    .expect("save the query")
            });
            cx.run_until_parked();
        }

        // `TUPLI_PALETTE=<prefix>` opens the command palette in the mode that
        // prefix selects — empty for the mixed list, `>` `@` `#` `:` `?` for
        // the rest — and `TUPLI_PALETTE_QUERY=<text>` types into it, so the
        // match highlighting can be looked at and not just imagined.
        if let Ok(prefix) = std::env::var("TUPLI_PALETTE") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        workspace.open_palette(&prefix, cx);
                        if let Ok(query) = std::env::var("TUPLI_PALETTE_QUERY") {
                            if let Some(palette) = workspace.palette.clone() {
                                palette.update(cx, |palette, cx| palette.type_in(&query, cx));
                            }
                        }
                    })
                    .expect("open the palette")
            });
            cx.run_until_parked();
        }

        // `TUPLI_SPLIT=right|down|grid` splits the centre through the same
        // path ⌘D takes, so a frame shows what the pane tree actually lays out
        // rather than what it is supposed to.
        if let Ok(kind) = std::env::var("TUPLI_SPLIT") {
            use tupli::pane::Layout;
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| match kind.as_str() {
                        "right" => workspace.split_pane(Layout::Columns, cx),
                        "down" => workspace.split_pane(Layout::Rows, cx),
                        "grid" => {
                            workspace.split_pane(Layout::Columns, cx);
                            workspace.split_pane(Layout::Rows, cx);
                        }
                        other => eprintln!("warning: no split called {other:?}"),
                    })
                    .expect("split the centre")
            });
            cx.run_until_parked();
        }

        // `TUPLI_SETTINGS=<pane>` opens the Settings window and captures that
        // instead of the main one — it is a window of its own, so there is no
        // frame of the workspace it could appear in.
        let mut target: gpui::AnyWindowHandle = window.into();
        if let Ok(name) = std::env::var("TUPLI_SETTINGS") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| workspace.open_settings(cx))
                    .expect("open settings")
            });
            cx.run_until_parked();
            let settings = cx.update(|cx| {
                window
                    .read(cx)
                    .ok()
                    .and_then(|workspace| workspace.settings_window())
            });
            match settings {
                Some(handle) => {
                    if let Some(pane) = tupli::settings_window::Pane::named(&name) {
                        cx.update(|cx| {
                            handle
                                .update(cx, |view, _window, cx| view.show_pane(pane, cx))
                                .expect("show the pane")
                        });
                    } else if !name.is_empty() {
                        eprintln!("warning: no settings pane called {name:?}");
                    }
                    cx.run_until_parked();
                    target = handle.into();
                }
                None => eprintln!("warning: the settings window did not open"),
            }
        }

        // `TUPLI_CONN_MENU=1` photographs the connection row's own menu, and
        // `TUPLI_CONN_MENU=remove` the sheet its last item opens. Both are
        // about a *saved* connection, and `TUPLI_CONNECT` does not save — so
        // the spec is written into this run's throwaway store first, under the
        // id the live session already has, which is what keeps the sidebar at
        // one root rather than the same server twice.
        if let Ok(which) = std::env::var("TUPLI_CONN_MENU") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        let live = workspace.tree.first().map(|node| node.origin.connection);
                        if let (Some(id), Ok(spec)) = (live, std::env::var("TUPLI_CONNECT")) {
                            if let Ok(mut config) = db::ConnectionConfig::from_spec(&spec) {
                                config.id = id;
                                config.name = "Local".to_string();
                                workspace.save_connection(&config, None, cx);
                            }
                        }
                        let Some(id) = workspace.connections.first().map(|c| c.id) else {
                            eprintln!("warning: no saved connection to open a menu on");
                            return;
                        };
                        match which.as_str() {
                            "remove" => workspace.prompt_remove_connection(id, cx),
                            // What the menu's second item does, rather than
                            // the menu itself: the row it leaves behind is the
                            // thing worth photographing.
                            "disconnect" => workspace.close_sessions_on(id, cx),
                            // And the way back up, which is the same click a
                            // connection has always been opened with.
                            "reconnect" => {
                                workspace.close_sessions_on(id, cx);
                                let config = workspace.connections[0].clone();
                                workspace.open_connection(config, cx);
                            }
                            _ => workspace.open_connection_menu(
                                id,
                                gpui::point(px(150.), px(150.)),
                                cx,
                            ),
                        }
                    })
                    .expect("open the connection menu")
            });
            cx.run_until_parked();
            // A reconnect is a second handshake, and a handshake is a socket
            // rather than a task the executor can be run to the end of.
            if which == "reconnect" {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    cx.run_until_parked();
                    let ready = cx.update(|cx| {
                        window
                            .read(cx)
                            .map(|workspace| workspace.is_connected(cx))
                            .unwrap_or(false)
                    });
                    if ready {
                        break;
                    }
                    if std::time::Instant::now() > deadline {
                        eprintln!("warning: gave up waiting for the reconnect");
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }

        // `TUPLI_CONNECTION=new|<name>` opens the connection window and
        // captures that instead, for the same reason.
        if let Ok(which) = std::env::var("TUPLI_CONNECTION") {
            cx.update(|cx| {
                window
                    .update(cx, |workspace, _window, cx| {
                        match workspace
                            .connections
                            .iter()
                            .find(|c| c.name.eq_ignore_ascii_case(&which))
                            .cloned()
                        {
                            Some(config) => workspace.edit_connection(config, cx),
                            None => workspace.new_connection(cx),
                        }
                    })
                    .expect("open the connection window")
            });
            cx.run_until_parked();
            let connection = cx.update(|cx| {
                window
                    .read(cx)
                    .ok()
                    .and_then(|workspace| workspace.connection_window())
            });
            match connection {
                Some(handle) => target = handle.into(),
                None => eprintln!("warning: the connection window did not open"),
            }
        }

        // Assets load through the executor, and the first frame lays out the
        // grid's columns; parking between draws lets both settle before the
        // frame that gets captured.
        cx.run_until_parked();
        cx.update_window(target, |_, window, cx| {
            let _ = window.draw(cx);
        })
        .expect("draw");
        cx.run_until_parked();

        let image = cx.capture_screenshot(target).expect("capture");
        let path = out.join(format!("{name}.png"));
        image.save(&path).expect("write png");
        println!(
            "wrote {} ({}x{})",
            path.display(),
            image.width(),
            image.height()
        );
    }
}
