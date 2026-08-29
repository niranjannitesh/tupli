//! Renders candidate icons at the sizes the app draws them, for choosing between.
//!
//! `TUPLI_SHEET=<manifest.json> cargo run -p tupli --example icon_sheet -- out/dir`
//! writes `dark.png` and `light.png`. The point of doing this through gpui rather
//! than an SVG rasteriser is that gpui is the rasteriser the app ships: an icon is
//! an alpha mask tinted by the theme, hinted at 16 physical pixels, and a candidate
//! that looks fine in a browser can still turn to mud there.

use std::sync::Arc;

use gpui::{div, prelude::*, px, size, svg, HeadlessAppContext, SharedString, Window};
use ui::{ActiveTheme, Appearance, Assets, Theme, ThemeRegistry};

struct Sheet {
    rows: Vec<Row>,
}

struct Row {
    slot: String,
    note: String,
    icons: Vec<(String, SharedString)>,
}

impl Render for Sheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors();
        div()
            .size_full()
            .bg(c.panel)
            .font_family(cx.typography().ui_family.clone())
            .text_color(c.text)
            .p_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(self.rows.iter().map(|row| {
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(180.))
                                    .flex_none()
                                    .child(div().text_size(px(13.)).child(row.slot.clone()))
                                    .child(
                                        div()
                                            .text_size(px(10.))
                                            .text_color(c.text_subtle)
                                            .child(row.note.clone()),
                                    ),
                            )
                            .children(row.icons.iter().enumerate().map(|(i, (caption, path))| {
                                div()
                                    .w(px(132.))
                                    .flex_none()
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .gap_1()
                                    .p_1()
                                    .rounded_md()
                                    .when(i == 0, |d| d.bg(c.hover))
                                    .child(
                                        div()
                                            .flex()
                                            .items_end()
                                            .gap_2()
                                            .h(px(34.))
                                            .child(
                                                svg()
                                                    .path(path.clone())
                                                    .size(px(32.))
                                                    .text_color(c.text),
                                            )
                                            .child(
                                                svg()
                                                    .path(path.clone())
                                                    .size(px(16.))
                                                    .text_color(c.text_muted),
                                            )
                                            .child(
                                                svg()
                                                    .path(path.clone())
                                                    .size(px(14.))
                                                    .text_color(c.text_muted),
                                            )
                                            .child(
                                                svg()
                                                    .path(path.clone())
                                                    .size(px(12.))
                                                    .text_color(c.text_muted),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(9.))
                                            .text_color(c.text_subtle)
                                            .child(caption.clone()),
                                    )
                            }))
                    })),
            )
    }
}

fn main() {
    env_logger::init();
    let out = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&out).expect("create output directory");

    let manifest = std::env::var("TUPLI_SHEET").expect("TUPLI_SHEET is the manifest path");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).expect("read the manifest"))
            .expect("parse the manifest");
    let rows: Vec<Row> = manifest
        .as_array()
        .expect("the manifest is an array of rows")
        .iter()
        .map(|row| Row {
            slot: row["slot"].as_str().unwrap_or_default().to_string(),
            note: row["note"].as_str().unwrap_or_default().to_string(),
            icons: row["icons"]
                .as_array()
                .expect("a row has icons")
                .iter()
                .map(|icon| {
                    (
                        icon["label"].as_str().unwrap_or_default().to_string(),
                        SharedString::from(
                            icon["path"]
                                .as_str()
                                .expect("an icon has a path")
                                .to_string(),
                        ),
                    )
                })
                .collect(),
        })
        .collect();

    let widest = rows.iter().map(|r| r.icons.len()).max().unwrap_or(1);
    let width = 210. + widest as f32 * 140.;
    let height = 40. + rows.len() as f32 * 60.;

    for (appearance, name) in [(Appearance::Dark, "dark"), (Appearance::Light, "light")] {
        let mut cx = HeadlessAppContext::with_platform(
            gpui_platform::current_platform(true).text_system(),
            Arc::new(Assets),
            gpui_platform::current_headless_renderer,
        );
        cx.update(|cx| {
            ThemeRegistry::init(&[], cx);
            Theme::set_global(Theme::of(appearance), cx);
        });
        let rows = rows
            .iter()
            .map(|r| Row {
                slot: r.slot.clone(),
                note: r.note.clone(),
                icons: r.icons.clone(),
            })
            .collect();
        let window = cx
            .open_window(size(px(width), px(height)), |_window, cx| {
                cx.new(|_| Sheet { rows })
            })
            .expect("open headless window");
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
        })
        .expect("draw");
        cx.run_until_parked();
        let image = cx.capture_screenshot(window.into()).expect("capture");
        let path = out.join(format!("{name}.png"));
        image.save(&path).expect("write png");
        println!("wrote {}", path.display());
    }
}
