use gpui::App;
use gpui_platform::application;

fn main() {
    application().run(|cx: &mut App| {
        let mut names = cx.text_system().all_font_names();
        names.sort();
        for n in names {
            let l = n.to_lowercase();
            if [
                "geist",
                "berkeley",
                "mono",
                "sf ",
                "system",
                "menlo",
                "jetbrains",
                "iosevka",
            ]
            .iter()
            .any(|k| l.contains(k))
            {
                println!("{n}");
            }
        }
        cx.quit();
    });
}
