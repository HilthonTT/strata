//! Lua plugin host for strata.
//!
//! Plugins are plain Lua files that talk to strata through the global
//! `strata` table. They can bind keys, add commands, react to events,
//! contribute status-line segments, draw their own panels, provide
//! previewers, add file-list columns and sort orders, and veto operations
//! through `before_*` events. Side effects on the app (changing directory, prompting the
//! user...) are queued as [`Request`]s that the UI drains after each call,
//! which keeps this crate independent of the UI.

mod api;
mod host;
mod types;

pub use host::{PluginHost, PluginInfo, OFFICIAL_PLUGINS};
pub use types::{
    Cell, ColumnDef, Context, EntryInfo, Level, PanelDef, PluginKey, PluginValue, Request, SortDef, SortValue,
};
