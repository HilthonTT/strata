use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _, Result};
use mlua::{Function, HookTriggers, IntoLuaMulti, Lua, MultiValue, Table, Value, VmState};

use crate::api::{self, HostPaths, Registry, Shared};
use crate::types::{
    Cell, ColumnDef, Context, EntryInfo, PanelDef, PluginKey, PluginValue, Request, SortDef, SortValue,
};

/// Official plugins shipped inside the binary: `(name, source)`.
pub const OFFICIAL_PLUGINS: &[(&str, &str)] = &[
    ("git", include_str!("../../../plugins/git.lua")),
    ("bookmarks", include_str!("../../../plugins/bookmarks.lua")),
    ("archive", include_str!("../../../plugins/archive.lua")),
    ("zoxide", include_str!("../../../plugins/zoxide.lua")),
    ("protect", include_str!("../../../plugins/protect.lua")),
];

/// How long each `before_*` handler may run before it is stopped.
const VETO_LIMIT: Duration = Duration::from_millis(500);
/// How long a plugin column may take for the rows on screen.
const COLUMN_LIMIT: Duration = Duration::from_millis(200);
/// How long a plugin sort may take for a whole listing.
const SORT_LIMIT: Duration = Duration::from_secs(2);

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
        let extra = format!("{0}/?.lua;{0}/?/init.lua;{path}", plugin_dir.to_string_lossy().replace('\\', "/"));
        package.set("path", extra).map_err(lua_err)?;
        Ok(Self { lua, reg, loaded: Vec::new() })
    }

    /// Runs a plugin's source. If it returns a table with `setup`, that is
    /// called with the plugin's options from the config.
    pub fn load(&mut self, name: &str, source: &str, official: bool, options: Option<PluginValue>) -> Result<()> {
        self.reg.borrow_mut().loading = name.to_string();
        let result = (|| -> mlua::Result<()> {
            let module: Value = self.lua.load(source).set_name(format!("@{name}.lua")).eval()?;
            if let Value::Table(t) = module {
                if let Ok(setup) = t.get::<Function>("setup") {
                    setup.call::<()>(options.unwrap_or(PluginValue::Map(Vec::new())))?;
                }
            }
            Ok(())
        })();
        self.reg.borrow_mut().loading.clear();
        result.map_err(lua_err).with_context(|| format!("plugin '{name}'"))?;
        self.loaded.push(PluginInfo { name: name.to_string(), official });
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
        self.reg.borrow().commands.iter().map(|(n, (_, d))| (n.clone(), d.clone())).collect()
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

    fn call_ret<R: mlua::FromLuaMulti>(&self, callback: usize, args: impl IntoLuaMulti) -> Result<R> {
        // Clone the function out so the callback may register things itself.
        let f = self.reg.borrow().callbacks.get(callback).cloned();
        let f = f.ok_or_else(|| anyhow!("stale plugin callback"))?;
        f.call::<R>(args).map_err(lua_err)
    }

    /// Fires an event (`cd`, `hover`, `startup`, `paste`...) at every
    /// handler, as `handler(ctx, data)`. Returns the errors.
    pub fn emit(&self, event: &str, ctx: &Context, data: PluginValue) -> Vec<String> {
        let ids = self.reg.borrow().handlers.get(event).cloned().unwrap_or_default();
        ids.into_iter()
            .filter_map(|id| self.call(id, (ctx.clone(), data.clone())).err().map(|e| format!("{e:#}")))
            .collect()
    }

    pub fn has_handlers(&self, event: &str) -> bool {
        self.reg.borrow().handlers.get(event).is_some_and(|h| !h.is_empty())
    }

    /// Status-line segments from every plugin; empty results are dropped.
    pub fn statusline(&self, ctx: &Context) -> Vec<String> {
        let ids = self.reg.borrow().statuslines.clone();
        ids.into_iter()
            .filter_map(|id| self.call_ret::<Option<String>>(id, ctx.clone()).ok().flatten())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Lines for a plugin panel.
    pub fn render_panel(&self, name: &str, ctx: &Context, width: u16, height: u16) -> Result<Vec<String>> {
        let id = self.reg.borrow().panels.iter().find(|p| p.name == name).map(|p| p.callback);
        let id = id.ok_or_else(|| anyhow!("no panel '{name}'"))?;
        let value: Value = self.call_ret(id, (ctx.clone(), width, height))?;
        Ok(lines_from(value))
    }

    /// Text preview from the first previewer claiming this extension.
    pub fn preview(&self, path: &str, ext: &str, width: u16, height: u16) -> Option<Result<Vec<String>>> {
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
        self.reg.borrow().previewers.iter().any(|(exts, _)| exts.iter().any(|e| e == &ext || e == "*"))
    }

    /// Fires a `before_*` event and asks whether to go ahead. A handler
    /// refuses by returning `false` (optionally followed by a reason) or
    /// `{ cancel = true, reason = "..." }`; the first refusal wins and its
    /// reason is returned. Handlers that fail or run past [`VETO_LIMIT`]
    /// are reported in the errors and do not refuse.
    pub fn veto(&self, event: &str, ctx: &Context, data: PluginValue) -> (Option<String>, Vec<String>) {
        let ids = self.reg.borrow().handlers.get(event).cloned().unwrap_or_default();
        let mut errors = Vec::new();
        for id in ids {
            let result =
                self.with_deadline(VETO_LIMIT, || self.call_ret::<MultiValue>(id, (ctx.clone(), data.clone())));
            match result {
                Ok(values) => {
                    if let Some(reason) = refusal(values) {
                        let owner = self.reg.borrow().owners.get(id).cloned().unwrap_or_default();
                        let reason = reason.unwrap_or_else(|| match owner.as_str() {
                            "" => "cancelled by a plugin".into(),
                            name => format!("cancelled by plugin '{name}'"),
                        });
                        return (Some(reason), errors);
                    }
                }
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
        (None, errors)
    }

    pub fn columns(&self) -> Vec<ColumnDef> {
        self.reg.borrow().columns.clone()
    }

    /// One plugin column's cells for `entries`, from `fn(entry) -> text?, style?`.
    pub fn column_cells(&self, callback: usize, entries: Vec<EntryInfo>) -> Result<Vec<Cell>> {
        self.with_deadline(COLUMN_LIMIT, || {
            entries
                .into_iter()
                .map(|entry| {
                    let (text, style): (Option<String>, Option<String>) = self.call_ret(callback, entry)?;
                    Ok(Cell { text: text.unwrap_or_default(), style })
                })
                .collect()
        })
    }

    pub fn sorts(&self) -> Vec<SortDef> {
        self.reg.borrow().sorts.clone()
    }

    /// Sort keys for `entries` from a plugin sort's `fn(entry) -> key`.
    pub fn sort_keys(&self, callback: usize, entries: Vec<EntryInfo>) -> Result<Vec<SortValue>> {
        self.with_deadline(SORT_LIMIT, || {
            entries.into_iter().map(|entry| self.call_ret::<Value>(callback, entry).map(SortValue::from_lua)).collect()
        })
    }

    /// Runs `f`, stopping the Lua code it runs once `limit` has passed.
    /// Time spent outside Lua (in `strata.shell`, say) is not interrupted
    /// but counts: the Lua code after it fails, and so does the result.
    fn with_deadline<R>(&self, limit: Duration, f: impl FnOnce() -> Result<R>) -> Result<R> {
        let start = Instant::now();
        let timed_out = move || start.elapsed() > limit;
        let message = format!("timed out after {} ms", limit.as_millis());
        let hook_message = message.clone();
        let hooked = self
            .lua
            .set_hook(HookTriggers::new().on_calls().every_nth_instruction(1000), move |_, _| match timed_out() {
                true => Err(mlua::Error::runtime(&hook_message)),
                false => Ok(VmState::Continue),
            })
            .is_ok();
        let result = f();
        if hooked {
            self.lua.remove_hook();
        }
        match result {
            Ok(_) if timed_out() => Err(anyhow!(message)),
            other => other,
        }
    }

    /// Takes the requests queued by plugins since the last drain.
    pub fn drain(&self) -> Vec<Request> {
        std::mem::take(&mut self.reg.borrow_mut().requests)
    }
}

/// `Some(reason)` when a `before_*` handler's return values refuse.
fn refusal(values: MultiValue) -> Option<Option<String>> {
    let mut values = values.into_iter();
    match values.next()? {
        Value::Boolean(false) => Some(match values.next() {
            Some(Value::String(s)) => Some(s.to_string_lossy()),
            _ => None,
        }),
        Value::Table(t) => {
            let cancel: bool = t.get("cancel").unwrap_or(false);
            cancel.then(|| t.get::<Option<String>>("reason").ok().flatten())
        }
        _ => None,
    }
}

fn lines_from(value: Value) -> Vec<String> {
    match value {
        Value::String(s) => s.to_string_lossy().lines().map(str::to_string).collect(),
        Value::Table(t) => t.sequence_values::<String>().filter_map(Result::ok).collect(),
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

        let ctx = Context { cwd: "/tmp".into(), ..Default::default() };
        h.call(keys[0].callback, ctx.clone()).unwrap();
        h.run_command("hello", "world", ctx.clone()).unwrap();
        assert_eq!(
            h.drain(),
            vec![
                Request::Cd("/tmp/sub".into()),
                Request::Notify { message: "hi world".into(), level: crate::Level::Info }
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
            Some(PluginValue::Map(vec![("greeting".into(), PluginValue::Str("hey".into()))])),
        )
        .unwrap();
        let lines = h.render_panel("p", &Context::default(), 10, 5).unwrap();
        assert_eq!(lines, vec!["greeting: hey", "size 10x5"]);
    }

    #[test]
    fn errors_are_reported_not_fatal() {
        let mut h = host();
        assert!(h.load("bad", "this is not lua", false, None).is_err());
        h.load("ok", "strata.on('cd', function() error('boom') end)", false, None).unwrap();
        let errors = h.emit("cd", &Context::default(), PluginValue::Nil);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("boom"));
    }

    #[test]
    fn events_carry_data_and_new_requests_queue() {
        let mut h = host();
        h.load(
            "ev",
            r#"
            strata.on("rename", function(ctx, e)
              strata.notify(e.from .. " -> " .. e.to .. " in tab " .. ctx.tab)
              strata.mark({ e.to })
              strata.tag(e.to, "red")
              strata.untag({ e.from })
              strata.show("Renamed", { e.to })
            end)
            "#,
            false,
            None,
        )
        .unwrap();
        let data = PluginValue::map([("from", PluginValue::str("/a")), ("to", PluginValue::str("/b"))]);
        assert!(h.emit("rename", &Context::default(), data).is_empty());
        assert_eq!(
            h.drain(),
            vec![
                Request::Notify { message: "/a -> /b in tab 1".into(), level: crate::Level::Info },
                Request::Mark { paths: vec!["/b".into()], clear: false },
                Request::Tag { paths: vec!["/b".into()], tag: "red".into(), add: true },
                Request::Tag { paths: vec!["/a".into()], tag: String::new(), add: false },
                Request::Show { title: "Renamed".into(), paths: vec!["/b".into()] },
            ]
        );
    }

    #[test]
    fn official_plugins_load() {
        let mut h = host();
        for (name, src) in OFFICIAL_PLUGINS {
            h.load(name, src, true, None).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        }
        assert_eq!(h.loaded().len(), OFFICIAL_PLUGINS.len());
    }

    fn paths(paths: &[&str]) -> PluginValue {
        PluginValue::map([("paths", PluginValue::strings(paths.iter().copied())), ("tags", PluginValue::Map(vec![]))])
    }

    #[test]
    fn before_hooks_can_refuse() {
        let mut h = host();
        h.load(
            "guard",
            r#"
            strata.on("before_delete", function(ctx, e)
              if e.paths[1] == "/plain" then return false end
              if e.paths[1] == "/reason" then return false, "not this one" end
              if e.paths[1] == "/table" then return { cancel = true, reason = "nope" } end
              if e.paths[1] == "/broken" then error("boom") end
              return true
            end)
            "#,
            false,
            None,
        )
        .unwrap();
        let ctx = Context::default();
        let veto = |p: &str| h.veto("before_delete", &ctx, paths(&[p]));
        assert_eq!(veto("/ok"), (None, vec![]));
        assert_eq!(veto("/plain").0.as_deref(), Some("cancelled by plugin 'guard'"));
        assert_eq!(veto("/reason").0.as_deref(), Some("not this one"));
        assert_eq!(veto("/table").0.as_deref(), Some("nope"));
        let (reason, errors) = veto("/broken");
        assert_eq!(reason, None, "a failing handler does not refuse");
        assert!(errors[0].contains("boom"));
        assert_eq!(h.veto("before_rename", &ctx, paths(&["/plain"])), (None, vec![]), "other events are unaffected");
    }

    #[test]
    fn slow_hooks_are_stopped() {
        let mut h = host();
        h.load("slow", r#"strata.on("before_move", function() while true do end end)"#, false, None).unwrap();
        let started = std::time::Instant::now();
        let (reason, errors) = h.veto("before_move", &Context::default(), paths(&["/a"]));
        assert_eq!(reason, None);
        assert!(errors[0].contains("timed out"), "{errors:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        // The hook is gone afterwards: other calls run normally.
        h.load("fine", r#"strata.command("x", function() strata.refresh() end)"#, false, None).unwrap();
        h.run_command("x", "", Context::default()).unwrap();
        assert_eq!(h.drain(), vec![Request::Refresh]);
    }

    #[test]
    fn protect_refuses_tagged_items() {
        let mut h = host();
        let (_, src) = OFFICIAL_PLUGINS.iter().find(|(n, _)| *n == "protect").unwrap();
        h.load("protect", src, true, None).unwrap();
        let data = |tags: Vec<(String, PluginValue)>| {
            PluginValue::map([("paths", PluginValue::strings(["/d"])), ("tags", PluginValue::Map(tags))])
        };
        let ctx = Context::default();
        let red = vec![("/d/a".to_string(), PluginValue::strings(["red"]))];
        assert_eq!(h.veto("before_delete", &ctx, data(red)).0, None);
        let keep = vec![("/d/a".to_string(), PluginValue::strings(["red", "keep"]))];
        let reason = h.veto("before_delete", &ctx, data(keep)).0.unwrap();
        assert!(reason.contains("'a' is tagged keep"), "{reason}");

        let column = &h.columns()[0];
        let entry =
            |tags: &[&str]| EntryInfo { tags: tags.iter().map(|t| t.to_string()).collect(), ..Default::default() };
        let cells = h.column_cells(column.callback, vec![entry(&["keep"]), entry(&[])]).unwrap();
        assert_eq!(cells[0].text, "🔒");
        assert_eq!(cells[1], Cell::default());
    }

    #[test]
    fn columns_compute_cells() {
        let mut h = host();
        h.load(
            "cols",
            r#"
            strata.column({ name = "size", width = 6, fn = function(e) return "old" end })
            strata.column({ name = "kind", width = 4, style = "muted", fn = function(e)
              if e.is_dir then return "dir", "accent" end
              return e.ext
            end })
            strata.column({ name = "size", width = 50, fn = function(e) return e.size end })
            "#,
            false,
            None,
        )
        .unwrap();
        let columns = h.columns();
        let names: Vec<_> = columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["kind", "size"], "a column registered again replaces the first");
        assert_eq!(columns[0].style.as_deref(), Some("muted"));
        assert_eq!(columns[1].width, 40, "widths are capped");
        let file = EntryInfo { name: "a.rs".into(), ext: "rs".into(), size: 12, ..Default::default() };
        let dir = EntryInfo { name: "d".into(), is_dir: true, ..Default::default() };
        let cells = h.column_cells(columns[0].callback, vec![file.clone(), dir.clone()]).unwrap();
        assert_eq!(
            cells,
            vec![Cell { text: "rs".into(), style: None }, Cell { text: "dir".into(), style: Some("accent".into()) }]
        );
        let cells = h.column_cells(columns[1].callback, vec![file, dir]).unwrap();
        assert_eq!(cells, vec![Cell { text: "12".into(), style: None }, Cell { text: "0".into(), style: None }]);

        h.load("bad", r#"strata.column({ name = "bad", fn = function() error("nope") end })"#, false, None).unwrap();
        let bad = h.columns().into_iter().find(|c| c.name == "bad").unwrap();
        assert_eq!(bad.width, 8);
        assert!(h.column_cells(bad.callback, vec![EntryInfo::default()]).is_err());
    }

    #[test]
    fn sorts_produce_comparable_keys() {
        let mut h = host();
        h.load(
            "sorts",
            r#"
            strata.sort({ name = "len", title = "Name length", fn = function(e) return #e.name end })
            strata.sort({ name = "tag", fn = function(e) return e.tags[1] end })
            "#,
            false,
            None,
        )
        .unwrap();
        let sorts = h.sorts();
        assert_eq!((sorts[0].name.as_str(), sorts[0].title.as_str()), ("len", "Name length"));
        assert_eq!(sorts[1].title, "tag", "the title defaults to the name");
        let entry = |name: &str, tag: Option<&str>| EntryInfo {
            name: name.into(),
            tags: tag.into_iter().map(String::from).collect(),
            ..Default::default()
        };
        let entries = vec![entry("abc", Some("Blue")), entry("a", None), entry("ab", Some("apple"))];
        let keys = h.sort_keys(sorts[0].callback, entries.clone()).unwrap();
        assert_eq!(keys, vec![SortValue::Num(3.0), SortValue::Num(1.0), SortValue::Num(2.0)]);
        let mut keys = h.sort_keys(sorts[1].callback, entries).unwrap();
        keys.sort_by(SortValue::compare);
        assert_eq!(keys, vec![SortValue::Str("apple".into()), SortValue::Str("blue".into()), SortValue::None]);
        assert!(SortValue::Num(10.0).compare(&SortValue::Str("a".into())).is_lt(), "numbers before text");
    }

    #[test]
    fn previewers_match_extensions() {
        let mut h = host();
        h.load(
            "pv",
            r#"strata.previewer({ ext = { "md" }, fn = function(path) return "preview of " .. path end })"#,
            false,
            None,
        )
        .unwrap();
        assert!(h.has_previewer("MD"));
        let lines = h.preview("/a.md", "md", 10, 10).unwrap().unwrap();
        assert_eq!(lines, vec!["preview of /a.md"]);
        assert!(h.preview("/a.rs", "rs", 10, 10).is_none());
    }
}
