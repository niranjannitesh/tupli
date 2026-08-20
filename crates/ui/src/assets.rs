//! Bundled assets.
//!
//! Everything under `assets/` is embedded in the binary, so a release build is a
//! single file with no runtime lookup path. In debug builds `rust-embed` reads
//! from disk instead, which means an edited SVG shows up on the next frame.

use anyhow::Result;
use gpui::{AssetSource, SharedString};
use rust_embed::RustEmbed;
use std::borrow::Cow;

#[derive(RustEmbed)]
#[folder = "../../assets"]
#[include = "icons/**/*"]
#[include = "fonts/**/*"]
#[include = "themes/**/*"]
struct Embedded;

/// The [`AssetSource`] handed to `Application::with_assets`.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(Embedded::get(path).map(|f| f.data))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Embedded::iter()
            .filter(|p| p.starts_with(path))
            .map(|p| SharedString::from(p.to_string()))
            .collect())
    }
}

/// The theme files compiled into the binary, as (path, bytes).
///
/// `AssetSource` would do this through `list` plus `load`, but the registry is
/// built before there is an `App` to ask for one, and a theme is not an asset
/// the framework ever needs to resolve by path.
pub(crate) fn bundled_themes() -> Vec<(String, Cow<'static, [u8]>)> {
    Embedded::iter()
        .filter(|path| path.starts_with("themes/") && path.ends_with(".json"))
        .filter_map(|path| Embedded::get(&path).map(|file| (path.to_string(), file.data)))
        .collect()
}
