use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use anyhow::{anyhow, Context as _, Result};
use mlua::{Function, IntoLuaMulti, Lua, Table, Value};

use crate::api::{self, HostPaths, Registry, Shared};
use crate::types::{Context, PanelDef, PluginKey, PluginValue, Request};

/// Official plugins shipped inside the binary: `(name, source)`.
pub const OFFICIAL_PLUGINS: &[(&str, &str)] = &[
    ("git", include_str!("../../../plugins/git.lua")),
    ("bookmarks", include_str!("../../../plugins/bookmarks.lua")),
    ("archive", include_str!("../../../plugins/archive.lua")),
    ("zoxide", include_str!("../../../plugins/zoxide.lua")),
];

#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub name: String,
    pub official: bool,
}

/// Owns the Lua state and everything plugins registered.
pub struct PluginHost {
    lua: Lua,
    reg: Shared,
    loaded: Vec<PluginInfo>,
}

impl PluginHost {
    pub fn new(version: &str, config_dir: &Path, data_dir: &Path) -> Result<Self> {
        let lua = Lua::new();
        let reg: Shared = Rc::new(RefCell::new(Registry::default()));
        let paths = HostPaths {
            version: version.to_string(),
            config_dir: config_dir.to_string_lossy().into_owned(),
            data_dir: data_dir.to_string_lossy().into_owned(),
        };
        api::install(&lua, &reg, &paths).map_err(|e| anyhow!("installing plugin API: {e}"))?;
        // Let plugins `require` helper modules from the user plugin directory.
        let plugin_dir = config_dir.join("plugins");
        let package: Table = lua.globals().get("package").map_err(lua_err)?;
        let path: String = package.get("path").map_err(lua_err)?;
        let extra = format!(
            "{0}/?.lua;{0}/?/init.lua;{path}",
            plugin_dir.to_string_lossy().replace('\\', "/")
        );
        package.set("path", extra).map_err(lua_err)?;
        Ok(Self {
            lua,
            reg,
            loaded: Vec::new(),
        })
    }

    /// Runs a plugin's source. If it returns a table with `setup`, that is
    /// called with the plugin's options from the config.
    pub fn load(
        &mut self,
        name: &str,
        source: &str,
        official: bool,
        options: Option<PluginValue>,
    ) -> Result<()> {
        self.reg.borrow_mut().loading = name.to_string();
        let result = (|| -> mlua::Result<()> {
            let module: Value = self
                .lua
                .load(source)
                .set_name(format!("@{name}.lua"))
                .eval()?;
            if let Value::Table(t) = module {
                if let Ok(setup) = t.get::<Function>("setup") {
                    setup.call::<()>(options.unwrap_or(PluginValue::Map(Vec::new())))?;
                }
            }
            Ok(())
        })();
        self.reg.borrow_mut().loading.clear();
        result
            .map_err(lua_err)
            .with_context(|| format!("plugin '{name}'"))?;
        self.loaded.push(PluginInfo {
            name: name.to_string(),
            official,
        });
        Ok(())
    }

    /// Loads every `*.lua` and `*/init.lua` in `dir`, skipping `disabled`.
    /// Returns one message per plugin that failed.
    pub fn load_dir(
        &mut self,
        dir: &Path,
        disabled: &[String],
        options: impl Fn(&str) -> Option<PluginValue>,
    ) -> Vec<String> {
        let Ok(read) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut candidates: Vec<(String, std::path::PathBuf)> = read
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter_map(|p| {
                if p.is_dir() {
                    let init = p.join("init.lua");
                    let name = p.file_name()?.to_string_lossy().into_owned();
                    init.exists().then_some((name, init))
                } else if p.extension().is_some_and(|e| e == "lua") {
                    Some((p.file_stem()?.to_string_lossy().into_owned(), p))
                } else {
                    None
                }
            })
            .collect();
        candidates.sort();
        let mut errors = Vec::new();
        for (name, path) in candidates {
            if disabled.contains(&name) || self.loaded.iter().any(|p| p.name == name) {
                continue;
            }
            let result = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))
                .and_then(|src| self.load(&name, &src, false, options(&name)));
            if let Err(e) = result {
                errors.push(format!("{e:#}"));
            }
        }
        errors
    }

    pub fn loaded(&self) -> &[PluginInfo] {
        &self.loaded
    }

    pub fn keys(&self) -> Vec<PluginKey> {
        self.reg.borrow().keys.clone()
    }

    pub fn commands(&self) -> Vec<(String, String)> {
        self.reg
            .borrow()
            .commands
            .iter()
            .map(|(n, (_, d))| (n.clone(), d.clone()))
            .collect()
    }

    pub fn has_command(&self, name: &str) -> bool {
        self.reg.borrow().commands.contains_key(name)
    }

    pub fn panels(&self) -> Vec<PanelDef> {
        self.reg.borrow().panels.clone()
    }

    /// Runs a plugin command with its argument string.
    pub fn run_command(&self, name: &str, args: &str, ctx: Context) -> Result<()> {
        let id = self.reg.borrow().commands.get(name).map(|(id, _)| *id);
        let id = id.ok_or_else(|| anyhow!("unknown command '{name}'"))?;
        self.call(id, (ctx, args.to_string()))
    }

    /// Calls a stored callback with arbitrary arguments.
    pub fn call(&self, callback: usize, args: impl IntoLuaMulti) -> Result<()> {
        self.call_ret::<()>(callback, args)
    }

    fn call_ret<R: mlua::FromLuaMulti>(
        &self,
        callback: usize,
        args: impl IntoLuaMulti,
    ) -> Result<R> {
        // Clone the function out so the callback may register things itself.
        let f = self.reg.borrow().callbacks.get(callback).cloned();
        let f = f.ok_or_else(|| anyhow!("stale plugin callback"))?;
        f.call::<R>(args).map_err(lua_err)
    }

    /// Fires an event (`cd`, `hover`, `startup`, `paste`...) at every handler.
    pub fn emit(&self, event: &str, ctx: &Context) -> Vec<String> {
        let ids = self
            .reg
            .borrow()
            .handlers
            .get(event)
            .cloned()
            .unwrap_or_default();
        ids.into_iter()
            .filter_map(|id| self.call(id, ctx.clone()).err().map(|e| format!("{e:#}")))
            .collect()
    }

    pub fn has_handlers(&self, event: &str) -> bool {
        self.reg
            .borrow()
            .handlers
            .get(event)
            .is_some_and(|h| !h.is_empty())
    }

    /// Status-line segments from every plugin; empty results are dropped.
    pub fn statusline(&self, ctx: &Context) -> Vec<String> {
        let ids = self.reg.borrow().statuslines.clone();
        ids.into_iter()
            .filter_map(|id| {
                self.call_ret::<Option<String>>(id, ctx.clone())
                    .ok()
                    .flatten()
            })
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Lines for a plugin panel.
    pub fn render_panel(
        &self,
        name: &str,
        ctx: &Context,
        width: u16,
        height: u16,
    ) -> Result<Vec<String>> {
        let id = self
            .reg
            .borrow()
            .panels
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.callback);
        let id = id.ok_or_else(|| anyhow!("no panel '{name}'"))?;
        let value: Value = self.call_ret(id, (ctx.clone(), width, height))?;
        Ok(lines_from(value))
    }

    /// Text preview from the first previewer claiming this extension.
    pub fn preview(
        &self,
        path: &str,
        ext: &str,
        width: u16,
        height: u16,
    ) -> Option<Result<Vec<String>>> {
        let ext = ext.to_ascii_lowercase();
        let ids: Vec<usize> = self
            .reg
            .borrow()
            .previewers
            .iter()
            .filter(|(exts, _)| exts.iter().any(|e| e == &ext || e == "*"))
            .map(|(_, id)| *id)
            .collect();
        for id in ids {
            match self.call_ret::<Value>(id, (path.to_string(), width, height)) {
                Ok(Value::Nil) => continue,
                Ok(v) => return Some(Ok(lines_from(v))),
                Err(e) => return Some(Err(e)),
            }
        }
        None
    }

    pub fn has_previewer(&self, ext: &str) -> bool {
        let ext = ext.to_ascii_lowercase();
        self.reg
            .borrow()
            .previewers
            .iter()
            .any(|(exts, _)| exts.iter().any(|e| e == &ext || e == "*"))
    }

    /// Takes the requests queued by plugins since the last drain.
    pub fn drain(&self) -> Vec<Request> {
        std::mem::take(&mut self.reg.borrow_mut().requests)
    }
}

fn lines_from(value: Value) -> Vec<String> {
    match value {
        Value::String(s) => s.to_string_lossy().lines().map(str::to_string).collect(),
        Value::Table(t) => t
            .sequence_values::<String>()
            .filter_map(Result::ok)
            .collect(),
        _ => Vec::new(),
    }
}

fn lua_err(e: mlua::Error) -> anyhow::Error {
    anyhow!("{e}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> PluginHost {
        let dir = std::env::temp_dir();
        PluginHost::new("test", &dir, &dir).unwrap()
    }

    #[test]
    fn registers_keys_commands_and_requests() {
        let mut h = host();
        h.load(
            "demo",
            r#"
            strata.map("g x", function(ctx) strata.cd(ctx.cwd .. "/sub") end, "Go to sub")
            strata.command("hello", function(ctx, args) strata.notify("hi " .. args) end, "Say hi")
            strata.statusline(function(ctx) return "on " .. ctx.cwd end)
            "#,
            false,
            None,
        )
        .unwrap();
        let keys = h.keys();
        assert_eq!(keys[0].keys, "g x");
        assert_eq!(keys[0].plugin, "demo");

        let ctx = Context {
            cwd: "/tmp".into(),
            ..Default::default()
        };
        h.call(keys[0].callback, ctx.clone()).unwrap();
        h.run_command("hello", "world", ctx.clone()).unwrap();
        assert_eq!(
            h.drain(),
            vec![
                Request::Cd("/tmp/sub".into()),
                Request::Notify {
                    message: "hi world".into(),
                    level: crate::Level::Info
                }
            ]
        );
        assert_eq!(h.statusline(&ctx), vec!["on /tmp".to_string()]);
    }

    #[test]
    fn setup_receives_options_and_panels_render() {
        let mut h = host();
        h.load(
            "opts",
            r#"
            local M = {}
            function M.setup(opts)
              strata.panel({ name = "p", title = "Panel", render = function(ctx, w, h)
                return { "greeting: " .. opts.greeting, "size " .. w .. "x" .. h }
              end })
            end
            return M
            "#,
            false,
            Some(PluginValue::Map(vec![(
                "greeting".into(),
                PluginValue::Str("hey".into()),
            )])),
        )
        .unwrap();
        let lines = h.render_panel("p", &Context::default(), 10, 5).unwrap();
        assert_eq!(lines, vec!["greeting: hey", "size 10x5"]);
    }

    #[test]
    fn errors_are_reported_not_fatal() {
        let mut h = host();
        assert!(h.load("bad", "this is not lua", false, None).is_err());
        h.load(
            "ok",
            "strata.on('cd', function() error('boom') end)",
            false,
            None,
        )
        .unwrap();
        let errors = h.emit("cd", &Context::default());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("boom"));
    }

    #[test]
    fn official_plugins_load() {
        let mut h = host();
        for (name, src) in OFFICIAL_PLUGINS {
            h.load(name, src, true, None)
                .unwrap_or_else(|e| panic!("{name}: {e:#}"));
        }
        assert_eq!(h.loaded().len(), OFFICIAL_PLUGINS.len());
    }

    #[test]
    fn previewers_match_extensions() {
        let mut h = host();
        h.load("pv", r#"strata.previewer({ ext = { "md" }, fn = function(path) return "preview of " .. path end })"#, false, None)
            .unwrap();
        assert!(h.has_previewer("MD"));
        let lines = h.preview("/a.md", "md", 10, 10).unwrap().unwrap();
        assert_eq!(lines, vec!["preview of /a.md"]);
        assert!(h.preview("/a.rs", "rs", 10, 10).is_none());
    }
}
