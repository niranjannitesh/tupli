use gpui::{
    div, px, rgb, size, App, AppContext as _, Bounds, Context, IntoElement, ParentElement, Render,
    Styled, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;

struct Probe;

impl Render for Probe {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x101014))
            .text_color(rgb(0xffffff))
            .child(div().text_size(px(24.)).child("Hello default font"))
            .child(
                div()
                    .font_family(".SystemUIFont")
                    .text_size(px(20.))
                    .child("SystemUIFont 20px"),
            )
            .child(
                div()
                    .font_family("Helvetica")
                    .text_size(px(20.))
                    .child("Helvetica 20px"),
            )
            .child(
                div()
                    .font_family("SF Mono")
                    .text_size(px(20.))
                    .child("SF Mono 20px"),
            )
            .child(
                div()
                    .font_family("Menlo")
                    .text_size(px(20.))
                    .child("Menlo 20px"),
            )
    }
}

fn main() {
    env_logger::init();
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(600.), px(400.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_w, cx| cx.new(|_| Probe),
        )
        .unwrap();
        cx.activate(true);
    });
}
