//! Bridge between the app and the Lua plugin host.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use strata_config::toml::Value as TomlValue;
use strata_config::{keymap::parse_sequence, Action, Binding};
use strata_core::ops::{Conflict, Transfer, TransferMode};
use strata_core::sort::natural_cmp;
use strata_core::util::expand_tilde;
use strata_core::Entry;
use strata_plugin::{
    Cell, ColumnDef, Context, EntryInfo, Level, PluginHost, PluginValue, Request, SortDef, SortValue, OFFICIAL_PLUGINS,
};

use super::external::{After, External, Wait};
use super::overlay::{InputPurpose, InputState, Overlay, PickerPurpose, PickerState, TextPopup};
use super::preview::PreviewContent;
use super::{App, View};

/// Columns and sort orders plugins added to the file list, with the
/// column cells worked out so far.
#[derive(Default)]
pub struct ListExtras {
    pub columns: Vec<ColumnDef>,
    pub sorts: Vec<SortDef>,
    /// Cells by column index and path. Only rows that have been on screen
    /// are worked out; a new listing in any panel starts over.
    cells: HashMap<(usize, PathBuf), Cell>,
    /// `(panel id, listing number)` the cells belong to.
    listings: Vec<(u64, u64)>,
    /// Columns whose function failed or was too slow; left blank.
    failed: HashSet<usize>,
    /// Plugin sort order per panel id, with what it was last applied to.
    sorted: HashMap<u64, (String, Option<SortStamp>)>,
}

/// `(listing number, reverse, folders first)` of a panel.
type SortStamp = (u64, bool, bool);

impl ListExtras {
    pub fn cell(&self, column: usize, path: &Path) -> Option<&Cell> {
        self.cells.get(&(column, path.to_path_buf()))
    }

    /// Name of the plugin sort order of a panel, if one is chosen.
    pub fn sort_of(&self, panel: u64) -> Option<&str> {
        self.sorted.get(&panel).map(|(name, _)| name.as_str())
    }
}

impl App {
    pub(super) fn load_plugins(&mut self) {
        let config_dir = strata_config::config_dir();
        let mut host = match PluginHost::new(env!("CARGO_PKG_VERSION"), &config_dir, &strata_config::data_dir()) {
            Ok(h) => h,
            Err(e) => return self.error(format!("plugins disabled: {e:#}")),
        };
        let cfg = self.config.plugins.clone();
        let options = |name: &str| cfg.options.get(name).map(to_plugin_value);

        for (name, source) in OFFICIAL_PLUGINS {
            if cfg.enabled.iter().any(|e| e == name) {
                if let Err(e) = host.load(name, source, true, options(name)) {
                    self.error(format!("{e:#}"));
                }
            }
        }
        for unknown in cfg.enabled.iter().filter(|n| !OFFICIAL_PLUGINS.iter().any(|(o, _)| o == n)) {
            self.notify(format!("unknown official plugin '{unknown}'"), Level::Warn);
        }
        for e in host.load_dir(&config_dir.join("plugins"), &cfg.disabled, options) {
            self.error(e);
        }
        for key in host.keys() {
            match parse_sequence(&key.keys) {
                Ok(seq) => {
                    self.keymap.bind(seq, Binding::Plugin { callback: key.callback, description: key.description })
                }
                Err(e) => self.error(format!("plugin {}: {e}", key.plugin)),
            }
        }
        self.plugin_list.columns = host.columns();
        self.plugin_list.sorts = host.sorts();
        self.plugins = Some(host);
        self.process_plugin_requests();
    }

    /// Snapshot of the UI handed to plugin callbacks.
    pub fn plugin_context(&self) -> Context {
        let p = self.panel();
        let hovered = p.hovered();
        Context {
            cwd: p.cwd.to_string_lossy().into_owned(),
            hovered: hovered.map(|e| e.path.to_string_lossy().into_owned()),
            hovered_is_dir: hovered.is_some_and(Entry::is_dir),
            selected: p.marked.iter().map(|m| m.to_string_lossy().into_owned()).collect(),
            panel: self.active,
            view: self.view.title().to_lowercase(),
            theme: self.theme.name.clone(),
            scheme: p.vfs.scheme().to_string(),
            icons: self.config.general.icons,
            tags: hovered.map(|e| self.tags.get(&e.path).to_vec()).unwrap_or_default(),
            filter: p.filter.clone(),
            listing: p.flat.as_ref().map(|f| f.title.clone()),
            tab: self.tab,
        }
    }

    /// Calls a plugin callback. `value` is `Some(..)` for picker/prompt
    /// callbacks (the chosen text or nil); key callbacks get the context.
    pub(super) fn call_plugin(&mut self, callback: usize, value: Option<Option<String>>) {
        let ctx = self.plugin_context();
        let Some(host) = &self.plugins else { return };
        let result = match value {
            Some(v) => host.call(callback, (v, ctx)),
            None => host.call(callback, ctx),
        };
        if let Err(e) = result {
            self.error(format!("plugin: {e:#}"));
        }
        self.process_plugin_requests();
    }

    pub(super) fn run_plugin_command(&mut self, name: &str, args: &str) {
        let ctx = self.plugin_context();
        let Some(host) = &self.plugins else { return };
        if let Err(e) = host.run_command(name, args, ctx) {
            self.error(format!("{name}: {e:#}"));
        }
        self.process_plugin_requests();
    }

    pub(super) fn emit_plugin_event(&mut self, event: &str) {
        self.emit_plugin_event_with(event, PluginValue::Nil);
    }

    /// Fires `event` with a data table: handlers get `(ctx, data)`.
    pub(super) fn emit_plugin_event_with(&mut self, event: &str, data: PluginValue) {
        if event == "tag" {
            // Plugin columns often show tags.
            self.plugin_list.cells.clear();
        }
        let Some(host) = &self.plugins else { return };
        if !host.has_handlers(event) {
            return;
        }
        let errors = host.emit(event, &self.plugin_context(), data);
        for e in errors {
            self.error(format!("plugin ({event}): {e}"));
        }
    }

    /// Asks plugins whether an operation on `paths` may go ahead, by firing
    /// a `before_*` event with `paths`, the tags of those items and the
    /// tagged items inside them, and `extra`. Shows the reason and returns
    /// true when a plugin refuses.
    pub(super) fn plugin_vetoes(
        &mut self,
        event: &str,
        paths: &[PathBuf],
        extra: impl IntoIterator<Item = (&'static str, PluginValue)>,
    ) -> bool {
        if !self.plugins.as_ref().is_some_and(|h| h.has_handlers(event)) {
            return false;
        }
        let tags = self
            .tags
            .with_all(&[])
            .into_iter()
            .filter(|tagged| paths.iter().any(|p| tagged.starts_with(p)))
            .map(|tagged| {
                let tags = PluginValue::strings(self.tags.get(&tagged).iter().cloned());
                (tagged.to_string_lossy().into_owned(), tags)
            })
            .collect();
        let mut data = vec![
            ("paths", PluginValue::strings(paths.iter().map(|p| p.to_string_lossy().into_owned()))),
            ("tags", PluginValue::Map(tags)),
        ];
        data.extend(extra);
        let ctx = self.plugin_context();
        let Some(host) = &self.plugins else { return false };
        let (reason, errors) = host.veto(event, &ctx, PluginValue::map(data));
        for e in errors {
            self.error(format!("plugin ({event}): {e}"));
        }
        self.process_plugin_requests();
        match reason {
            Some(reason) => {
                self.notify(reason, Level::Warn);
                true
            }
            None => false,
        }
    }

    /// `before_copy` or `before_move` for a transfer, then
    /// `before_overwrite` for the items it would replace.
    pub(super) fn plugins_veto_transfer(&mut self, transfer: &Transfer) -> bool {
        let event = match transfer.mode {
            TransferMode::Copy => "before_copy",
            TransferMode::Move => "before_move",
        };
        let dest = ("dest", PluginValue::str(transfer.dest_dir.to_string_lossy()));
        if self.plugin_vetoes(event, &transfer.sources, [dest.clone()]) {
            return true;
        }
        if !self.plugins.as_ref().is_some_and(|h| h.has_handlers("before_overwrite")) {
            return false;
        }
        // Per-file answers from the conflict dialog can overwrite even when
        // the batch policy doesn't.
        let replaced: Vec<PathBuf> = transfer
            .clashes()
            .into_iter()
            .filter(|c| transfer.decision(&c.source, &c.existing) == Conflict::Overwrite)
            .map(|c| c.existing.path)
            .collect();
        if replaced.is_empty() {
            return false;
        }
        let sources = PluginValue::strings(transfer.sources.iter().map(|p| p.to_string_lossy().into_owned()));
        self.plugin_vetoes("before_overwrite", &replaced, [dest, ("sources", sources)])
    }

    /// `before_rename` for `(from, to)` pairs: `paths` and a parallel `to`.
    pub(super) fn plugins_veto_renames(&mut self, moves: &[(PathBuf, PathBuf)]) -> bool {
        let (from, to): (Vec<PathBuf>, Vec<PathBuf>) = moves.iter().cloned().unzip();
        let to = PluginValue::strings(to.iter().map(|p| p.to_string_lossy().into_owned()));
        self.plugin_vetoes("before_rename", &from, [("to", to)])
    }

    /// An entry as plugin column and sort functions see it.
    fn entry_info(&self, entry: &Entry) -> EntryInfo {
        EntryInfo {
            path: entry.path.to_string_lossy().into_owned(),
            name: entry.name.clone(),
            is_dir: entry.is_dir(),
            size: entry.size,
            modified: entry.modified.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64),
            ext: entry.extension(),
            tags: self.tags.get(&entry.path).to_vec(),
        }
    }

    /// Works out plugin column cells for the rows of panel `index` about to
    /// be drawn (`rows` from its scroll offset).
    pub fn prepare_plugin_cells(&mut self, index: usize, rows: usize) {
        if self.plugin_list.columns.is_empty() {
            return;
        }
        let p = &self.panels[index];
        let stamp = (p.id, p.listed);
        if !self.plugin_list.listings.contains(&stamp) {
            self.plugin_list.listings.retain(|(id, _)| *id != p.id);
            self.plugin_list.listings.push(stamp);
            self.plugin_list.cells.clear();
        }
        let shown: Vec<&Entry> = (p.offset..(p.offset + rows).min(p.len())).filter_map(|row| p.entry_at(row)).collect();
        let mut failures = Vec::new();
        for (i, column) in self.plugin_list.columns.iter().enumerate() {
            if self.plugin_list.failed.contains(&i) {
                continue;
            }
            let missing: Vec<&Entry> =
                shown.iter().copied().filter(|e| !self.plugin_list.cells.contains_key(&(i, e.path.clone()))).collect();
            if missing.is_empty() {
                continue;
            }
            let Some(host) = &self.plugins else { return };
            let infos = missing.iter().map(|e| self.entry_info(e)).collect();
            match host.column_cells(column.callback, infos) {
                Ok(cells) => {
                    let paths = missing.iter().map(|e| (i, e.path.clone()));
                    self.plugin_list.cells.extend(paths.zip(cells));
                }
                Err(e) => failures.push((i, format!("plugin column '{}': {e:#}", column.name))),
            }
        }
        for (i, message) in failures {
            self.plugin_list.failed.insert(i);
            self.error(message);
        }
    }

    /// Chooses a plugin sort order for the focused panel (`None` goes back
    /// to the built-in ones).
    pub(super) fn set_plugin_sort(&mut self, name: Option<String>) {
        let id = self.panel().id;
        match name {
            Some(name) => {
                self.plugin_list.sorted.insert(id, (name, None));
                self.apply_plugin_sort(self.active);
            }
            None => {
                self.plugin_list.sorted.remove(&id);
            }
        }
    }

    /// Orders panel `index` by its plugin sort order, if it has one and the
    /// listing or the sort options changed since the last time.
    pub fn apply_plugin_sort(&mut self, index: usize) {
        let p = &self.panels[index];
        let Some((name, applied)) = self.plugin_list.sorted.get(&p.id) else { return };
        let now = (p.listed, p.sort.reverse, p.sort.dirs_first);
        if *applied == Some(now) {
            return;
        }
        let (id, name) = (p.id, name.clone());
        let def = self.plugin_list.sorts.iter().find(|s| s.name == name);
        let (Some(def), Some(host)) = (def, &self.plugins) else {
            self.plugin_list.sorted.remove(&id);
            return;
        };
        let infos = p.entries().iter().map(|e| self.entry_info(e)).collect();
        match host.sort_keys(def.callback, infos) {
            Ok(keys) => {
                let keys: HashMap<PathBuf, SortValue> = p.entries().iter().map(|e| e.path.clone()).zip(keys).collect();
                self.panels[index].sort_with(|a, b| {
                    let key = |e: &Entry| keys.get(&e.path).unwrap_or(&SortValue::None);
                    key(a).compare(key(b)).then_with(|| natural_cmp(&a.name, &b.name))
                });
                self.plugin_list.sorted.insert(id, (name, Some(now)));
            }
            Err(e) => {
                self.plugin_list.sorted.remove(&id);
                self.panels[index].resort();
                self.error(format!("plugin sort '{name}': {e:#}"));
            }
        }
    }

    /// Applies everything plugins asked for. Requests can trigger more
    /// plugin calls, so this loops a bounded number of times.
    pub(super) fn process_plugin_requests(&mut self) {
        for _ in 0..8 {
            let requests = match &self.plugins {
                Some(host) => host.drain(),
                None => return,
            };
            if requests.is_empty() {
                return;
            }
            for request in requests {
                self.apply_request(request);
            }
        }
    }

    fn apply_request(&mut self, request: Request) {
        match request {
            Request::Notify { message, level } => self.notify(message, level),
            Request::Cd(path) => {
                if self.panel().vfs.is_local() {
                    self.cd(expand_tilde(&path));
                } else {
                    self.cd(path.into());
                }
            }
            Request::Action(name) => match Action::from_name(&name) {
                Some(action) => self.dispatch(action),
                None => self.error(format!("plugin asked for unknown action '{name}'")),
            },
            Request::Command(line) => self.run_command(&line),
            Request::Select { title, items, callback } => {
                self.show_overlay(Overlay::Picker(PickerState::new(title, items, PickerPurpose::Plugin(callback))));
            }
            Request::Input { prompt, default, callback } => {
                self.show_overlay(Overlay::Input(InputState::new(prompt, default, InputPurpose::Plugin(callback))));
            }
            Request::TogglePanel(name) => {
                if let Some(i) = self.open_plugin_panels.iter().position(|n| *n == name) {
                    self.open_plugin_panels.remove(i);
                } else if self.plugins.as_ref().is_some_and(|h| h.panels().iter().any(|p| p.name == name)) {
                    self.open_plugin_panels.push(name);
                    self.refresh_plugin_ui();
                } else {
                    self.error(format!("no plugin panel named '{name}'"));
                }
            }
            Request::Exec(cmd) => {
                let argv = if cfg!(windows) {
                    vec!["cmd".into(), "/C".into(), cmd]
                } else {
                    vec!["sh".into(), "-c".into(), cmd]
                };
                let cwd = self.panel().vfs.is_local().then(|| self.panel().cwd.clone());
                self.queue_external(External::Run { argv, cwd, wait: Wait::Always, after: After::Reload });
            }
            Request::Refresh => self.reload_all(),
            Request::Mark { paths, clear } => {
                let p = self.panel_mut();
                if clear {
                    p.clear_marks();
                }
                p.mark_paths(&paths.into_iter().map(|p| expand_tilde(&p)).collect());
            }
            Request::Show { title, paths } => {
                self.show_paths(title, paths.into_iter().map(|p| expand_tilde(&p)).collect());
            }
            Request::Tag { paths, tag, add } => {
                self.apply_tag(paths.into_iter().map(|p| expand_tilde(&p)).collect(), &tag, add);
            }
        }
    }

    /// Re-renders plugin status segments and open plugin panels.
    pub(super) fn refresh_plugin_ui(&mut self) {
        self.plugin_refreshed = Instant::now();
        let Some(host) = &self.plugins else { return };
        let ctx = self.plugin_context();
        self.plugin_status = host.statusline(&ctx);
        let mut lines = std::collections::HashMap::new();
        for name in &self.open_plugin_panels {
            let rendered = host.render_panel(name, &ctx, 40, 50).unwrap_or_else(|e| vec![format!("error: {e:#}")]);
            lines.insert(name.clone(), rendered);
        }
        self.plugin_panel_lines = lines;
    }

    pub(super) fn plugin_preview(&mut self, entry: &Entry) -> Option<PreviewContent> {
        if entry.is_dir() || !self.panel().vfs.is_local() {
            return None;
        }
        let host = self.plugins.as_ref()?;
        let ext = entry.extension();
        if !host.has_previewer(&ext) {
            return None;
        }
        let size = self.preview_area;
        match host.preview(&entry.path.to_string_lossy(), &ext, size.width, size.height)? {
            Ok(lines) => Some(PreviewContent::Text(lines)),
            Err(e) => Some(PreviewContent::Error(format!("{e:#}"))),
        }
    }

    pub(super) fn show_plugins(&mut self) {
        let mut lines =
            vec![format!("Plugin directory: {}", strata_config::config_dir().join("plugins").display()), String::new()];
        match &self.plugins {
            None => lines.push("plugins are disabled".into()),
            Some(host) => {
                for p in host.loaded() {
                    lines.push(format!("  {} {}", if p.official { "●" } else { "○" }, p.name));
                }
                lines.push(String::new());
                lines.push("● official   ○ user".into());
                let commands = host.commands();
                if !commands.is_empty() {
                    lines.push(String::new());
                    lines.push("Commands:".into());
                    lines.extend(commands.into_iter().map(|(n, d)| format!("  :{n:<18} {d}")));
                }
                let panels = host.panels();
                if !panels.is_empty() {
                    lines.push(String::new());
                    lines.push("Panels:".into());
                    lines.extend(panels.into_iter().map(|p| format!("  {:<19} from {}", p.name, p.plugin)));
                }
            }
        }
        self.overlay = Some(Overlay::Text(TextPopup { title: "Plugins".into(), lines, scroll: 0, colored: false }));
    }

    /// Plugin panels shown next to the file panels.
    pub fn visible_plugin_panels(&self) -> Vec<(String, &[String])> {
        if self.view != View::Files {
            return Vec::new();
        }
        let titles = self.plugins.as_ref().map(|h| h.panels()).unwrap_or_default();
        self.open_plugin_panels
            .iter()
            .map(|name| {
                let title =
                    titles.iter().find(|p| p.name == *name).map(|p| p.title.clone()).unwrap_or_else(|| name.clone());
                let lines = self.plugin_panel_lines.get(name).map(Vec::as_slice).unwrap_or(&[]);
                (title, lines)
            })
            .collect()
    }
}

fn to_plugin_value(v: &TomlValue) -> PluginValue {
    match v {
        TomlValue::String(s) => PluginValue::Str(s.clone()),
        TomlValue::Integer(i) => PluginValue::Int(*i),
        TomlValue::Float(f) => PluginValue::Float(*f),
        TomlValue::Boolean(b) => PluginValue::Bool(*b),
        TomlValue::Datetime(d) => PluginValue::Str(d.to_string()),
        TomlValue::Array(items) => PluginValue::List(items.iter().map(to_plugin_value).collect()),
        TomlValue::Table(t) => PluginValue::Map(t.iter().map(|(k, v)| (k.clone(), to_plugin_value(v))).collect()),
    }
}
