//! Lua plugin host for strata.
//!
//! Plugins are plain Lua files that talk to strata through the global
//! `strata` table. They can bind keys, add commands, react to events,
//! contribute status-line segments, draw their own panels and provide
//! previewers. Side effects on the app (changing directory, prompting the
//! user...) are queued as [`Request`]s that the UI drains after each call,
//! which keeps this crate independent of the UI.

mod api;
mod host;
mod types;

pub use host::{PluginHost, PluginInfo, OFFICIAL_PLUGINS};
pub use types::{Context, Level, PanelDef, PluginKey, PluginValue, Request};
