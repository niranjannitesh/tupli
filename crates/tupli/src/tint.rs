//! Connection colours.
//!
//! The tint a connection carries is the app's cheapest and most effective
//! guardrail: production is red in the sidebar, in the titlebar and in the
//! status bar, and no amount of care substitutes for the window simply looking
//! different from the one you meant to type into.
//!
//! The palette is defined here rather than in `ui` because `ui` does not know
//! what a connection is, and the theme's semantic colours (`danger`, `success`)
//! mean something else — a red connection is not an error.

use db::ConnectionColor;
use gpui::{hsla, App, Hsla};

use ui::ActiveTheme;

/// The colour itself, tuned per appearance: the light theme needs more
/// saturation and less lightness to read against a white panel.
pub fn tint(color: ConnectionColor, cx: &App) -> Option<Hsla> {
    let dark = cx.theme().appearance.is_dark();
    let (hue, saturation, lightness) = match color {
        ConnectionColor::None => return None,
        ConnectionColor::Grey => (0., 0.00, if dark { 0.62 } else { 0.46 }),
        ConnectionColor::Red => (2., 0.72, if dark { 0.62 } else { 0.50 }),
        ConnectionColor::Orange => (26., 0.78, if dark { 0.60 } else { 0.47 }),
        ConnectionColor::Yellow => (44., 0.78, if dark { 0.58 } else { 0.42 }),
        ConnectionColor::Green => (142., 0.52, if dark { 0.50 } else { 0.38 }),
        ConnectionColor::Blue => (212., 0.72, if dark { 0.62 } else { 0.50 }),
        ConnectionColor::Purple => (266., 0.62, if dark { 0.66 } else { 0.54 }),
        ConnectionColor::Pink => (330., 0.68, if dark { 0.66 } else { 0.54 }),
    };
    Some(hsla(hue / 360., saturation, lightness, 1.))
}

/// The same colour at the weight a large filled area wants — a titlebar band, a
/// row background — where the full-strength version would shout.
pub fn tint_wash(color: ConnectionColor, cx: &App) -> Option<Hsla> {
    let mut base = tint(color, cx)?;
    base.a = if cx.theme().appearance.is_dark() {
        0.20
    } else {
        0.14
    };
    Some(base)
}

/// Every colour in palette order, for the picker.
pub const PALETTE: [ConnectionColor; 9] = [
    ConnectionColor::None,
    ConnectionColor::Grey,
    ConnectionColor::Red,
    ConnectionColor::Orange,
    ConnectionColor::Yellow,
    ConnectionColor::Green,
    ConnectionColor::Blue,
    ConnectionColor::Purple,
    ConnectionColor::Pink,
];
