//! tupli — a native database client.

use gpui::{
    point, px, size, App, AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions,
};
use gpui_platform::application;
use ui::{Appearance, Assets, Theme, ThemeRegistry};

use tupli::workspace::Workspace;

fn main() {
    env_logger::init();

    application().with_assets(Assets).run(|cx: &mut App| {
        // `TUPLI_THEME=light` starts in the light appearance. There is a
        // switch in the titlebar too; this exists so a screenshot of either
        // appearance can be taken without a human clicking anything.
        let appearance = match std::env::var("TUPLI_THEME").as_deref() {
            Ok("light") => Appearance::Light,
            _ => Appearance::Dark,
        };
        // Before any theme is chosen: the registry is what a theme name resolves
        // through, and the first thing the workspace does is resolve one.
        ThemeRegistry::init(&[ThemeRegistry::user_dir(&store::paths::data_dir())], cx);
        Theme::set_global(Theme::of(appearance), cx);

        // The Tokio runtime the database driver runs on. GPUI owns the main
        // thread; this owns two worker threads, and `gpui_tokio::Tokio::spawn`
        // is the only bridge between them.
        gpui_tokio::init(cx);

        // Before the window: an app with no `NSMainMenu` shows the *previous*
        // app's menu bar, live and clickable, for as long as tupli is
        // frontmost. See `tupli::menu`.
        tupli::menu::init(cx);

        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("tupli".into()),
                    appears_transparent: true,
                    // The y is the top of the button's 14px frame, which the
                    // light fills: 9 + 7 puts its centre on 16, the middle of
                    // the 32px bar, which is where every control beside it is
                    // centred too. gpui sizes the titlebar container from this
                    // same y (height = 14 + 2y), so 9 is also what keeps the
                    // buttons' own band exactly as tall as our bar.
                    traffic_light_position: Some(point(px(12.), px(9.))),
                }),
                // The bar at the top of this window is ours, all of it. Left
                // false, AppKit keeps ownership of the band the system titlebar
                // would have occupied and acts on it behind our back: a quick
                // second click on the sidebar toggle — which sits inside that
                // band — is a double-click on the titlebar as far as AppKit is
                // concerned, and it zooms the window. Our own handlers never
                // ran; there was nothing for them to stop. Claiming the view
                // also drops the click delay AppKit otherwise inserts while it
                // waits to see whether a titlebar click becomes a double one.
                app_owns_titlebar_drag: true,
                ..Default::default()
            },
            |_window, cx| cx.new(Workspace::new),
        )
        .unwrap();

        cx.activate(true);
    });
}
