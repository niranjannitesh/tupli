//! Tupli design system.
//!
//! A hand-rolled widget layer on top of gpui. Nothing here knows about databases;
//! it knows about panels, tabs, rows and text. The rule of the crate is that a
//! component takes tokens from [`Theme`] and never invents a colour or a height —
//! consistency comes from the token set being small enough to hold in your head.

mod assets;
mod badge;
mod button;
mod chrome;
mod elide;
mod icon;
mod icon_name;
mod label;
mod list;
mod menu;
mod select;
mod sheet;
mod spinner;
mod styled_ext;
mod tab;
mod theme;
mod theme_registry;
mod toggle;
mod tooltip;
mod zed_theme;

pub use assets::Assets;
pub use badge::{Badge, BadgeStyle, BadgeTone};
pub use button::{Button, ButtonSize, ButtonVariant};
pub use chrome::{
    page, region, Axis, Divider, EmptyState, ResizeHandle, SectionHeader, StatusBar, Toolbar,
};
pub use elide::ElidedLabel;
pub use icon::{Icon, IconColor, IconSize, IconSlot};
pub use icon_name::IconName;
pub use label::{Label, LabelSize};
pub use list::{Disclosure, ListItem};
pub use menu::{ContextMenu, MenuItem};
pub use select::{Popup, Segmented};
pub use sheet::{FormRow, Notice, NoticeTone, Sheet};
pub use spinner::Spinner;
pub use styled_ext::{h_flex, v_flex, StyledExt};
pub use tab::{Tab, TabBar};
pub use theme::{
    ActiveTheme, Appearance, Metrics, SyntaxTheme, Theme, ThemeColors,
    Typography,
};
pub use theme_registry::ThemeRegistry;
pub use toggle::{Checkbox, Switch};
pub use tooltip::Tooltip;
pub use zed_theme::{ThemeFamily, ThemeVariant};

pub mod prelude {
    pub use crate::{
        h_flex, v_flex, ActiveTheme, Button, ButtonSize, ButtonVariant, Icon, IconColor, IconName,
        IconSize, Label, LabelSize, StyledExt,
    };
    pub use gpui::prelude::*;
}
