//! User configuration for strata: `config.toml`, keymaps and themes.

pub mod config;
pub mod keymap;
pub mod theme;

pub use config::{config_dir, data_dir, BorderStyle, Config, General, OpenRule, PluginsConfig};
pub use keymap::{Action, Binding, KeyPress, Keymap, KeymapPreset, Lookup};
pub use theme::{Palette, Theme, ThemeRegistry};
pub use toml;
