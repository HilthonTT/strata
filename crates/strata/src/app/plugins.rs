//! Bridge between the app and the Lua plugin host.

use std::time::Instant;

use strata_config::toml::Value as TomlValue;
use strata_config::{keymap::parse_sequence, Action, Binding};
use strata_core::util::expand_tilde;
use strata_core::Entry;
use strata_plugin::{Context, Level, PluginHost, PluginValue, Request, OFFICIAL_PLUGINS};

use super::external::{After, External, Wait};
use super::overlay::{InputPurpose, InputState, Overlay, PickerPurpose, PickerState, TextPopup};
use super::preview::PreviewContent;
use super::{App, View};

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
        let Some(host) = &self.plugins else { return };
        if !host.has_handlers(event) {
            return;
        }
        let errors = host.emit(event, &self.plugin_context());
        for e in errors {
            self.error(format!("plugin ({event}): {e}"));
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
                self.overlay = Some(Overlay::Picker(PickerState::new(title, items, PickerPurpose::Plugin(callback))));
            }
            Request::Input { prompt, default, callback } => {
                self.overlay = Some(Overlay::Input(InputState::new(prompt, default, InputPurpose::Plugin(callback))));
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
