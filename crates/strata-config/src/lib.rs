//! User configuration for strata: `config.toml`, keymaps and themes.

pub mod config;
pub mod keymap;
pub mod theme;

pub use config::{
    config_dir, data_dir, BorderChars, BorderStyle, Config, General, OpenRule, PluginsConfig, SidebarSection,
};
pub use keymap::{Action, Binding, KeyPress, Keymap, KeymapPreset, Lookup};
pub use theme::{Palette, Theme, ThemeRegistry};
pub use toml;
