//! The `strata` global table exposed to Lua.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::process::Command;
use std::rc::Rc;

use mlua::{Function, Lua, Table, Value, Variadic};

use crate::types::{Level, PanelDef, PluginKey, Request};

#[derive(Default)]
pub(crate) struct Registry {
    pub callbacks: Vec<Function>,
    pub keys: Vec<PluginKey>,
    pub commands: BTreeMap<String, (usize, String)>,
    pub handlers: BTreeMap<String, Vec<usize>>,
    pub statuslines: Vec<usize>,
    pub panels: Vec<PanelDef>,
    pub previewers: Vec<(Vec<String>, usize)>,
    pub requests: Vec<Request>,
    /// Plugin currently being loaded, for attribution.
    pub loading: String,
}

impl Registry {
    fn store(&mut self, f: Function) -> usize {
        self.callbacks.push(f);
        self.callbacks.len() - 1
    }
}

pub(crate) type Shared = Rc<RefCell<Registry>>;

pub(crate) struct HostPaths {
    pub version: String,
    pub config_dir: String,
    pub data_dir: String,
}

pub(crate) fn install(lua: &Lua, reg: &Shared, paths: &HostPaths) -> mlua::Result<()> {
    let strata = lua.create_table()?;
    strata.set("version", paths.version.clone())?;
    strata.set("platform", std::env::consts::OS)?;
    let config_dir = paths.config_dir.clone();
    strata.set(
        "config_dir",
        lua.create_function(move |_, ()| Ok(config_dir.clone()))?,
    )?;
    let data_dir = paths.data_dir.clone();
    strata.set(
        "data_dir",
        lua.create_function(move |_, ()| Ok(data_dir.clone()))?,
    )?;

    // --- registration -----------------------------------------------------
    let r = reg.clone();
    strata.set(
        "map",
        lua.create_function(
            move |_, (keys, f, desc): (String, Function, Option<String>)| {
                let mut r = r.borrow_mut();
                let callback = r.store(f);
                let plugin = r.loading.clone();
                let description = desc.unwrap_or_else(|| format!("{plugin}: {keys}"));
                r.keys.push(PluginKey {
                    keys,
                    callback,
                    description,
                    plugin,
                });
                Ok(())
            },
        )?,
    )?;

    let r = reg.clone();
    strata.set(
        "command",
        lua.create_function(
            move |_, (name, f, desc): (String, Function, Option<String>)| {
                let mut r = r.borrow_mut();
                let id = r.store(f);
                r.commands.insert(name, (id, desc.unwrap_or_default()));
                Ok(())
            },
        )?,
    )?;

    let r = reg.clone();
    strata.set(
        "on",
        lua.create_function(move |_, (event, f): (String, Function)| {
            let mut r = r.borrow_mut();
            let id = r.store(f);
            r.handlers.entry(event).or_default().push(id);
            Ok(())
        })?,
    )?;

    let r = reg.clone();
    strata.set(
        "statusline",
        lua.create_function(move |_, f: Function| {
            let mut r = r.borrow_mut();
            let id = r.store(f);
            r.statuslines.push(id);
            Ok(())
        })?,
    )?;

    let r = reg.clone();
    strata.set(
        "panel",
        lua.create_function(move |_, spec: Table| {
            let name: String = spec.get("name")?;
            let title: Option<String> = spec.get("title")?;
            let render: Function = spec.get("render")?;
            let mut r = r.borrow_mut();
            let callback = r.store(render);
            let plugin = r.loading.clone();
            r.panels.retain(|p| p.name != name);
            r.panels.push(PanelDef {
                title: title.unwrap_or_else(|| name.clone()),
                name,
                callback,
                plugin,
            });
            Ok(())
        })?,
    )?;

    let r = reg.clone();
    strata.set(
        "previewer",
        lua.create_function(move |_, spec: Table| {
            let exts: Vec<String> = spec.get("ext")?;
            let f: Function = spec.get("fn")?;
            let mut r = r.borrow_mut();
            let id = r.store(f);
            r.previewers.push((
                exts.into_iter().map(|e| e.to_ascii_lowercase()).collect(),
                id,
            ));
            Ok(())
        })?,
    )?;

    // --- requests to the app -------------------------------------------------
    let push = |reg: &Shared, req: Request| reg.borrow_mut().requests.push(req);

    let r = reg.clone();
    strata.set(
        "notify",
        lua.create_function(move |_, (msg, level): (String, Option<String>)| {
            let level = match level.as_deref() {
                Some("warn") | Some("warning") => Level::Warn,
                Some("error") => Level::Error,
                _ => Level::Info,
            };
            push(
                &r,
                Request::Notify {
                    message: msg,
                    level,
                },
            );
            Ok(())
        })?,
    )?;

    let r = reg.clone();
    strata.set(
        "cd",
        lua.create_function(move |_, path: String| {
            push(&r, Request::Cd(path));
            Ok(())
        })?,
    )?;
    let r = reg.clone();
    strata.set(
        "action",
        lua.create_function(move |_, name: String| {
            push(&r, Request::Action(name));
            Ok(())
        })?,
    )?;
    let r = reg.clone();
    strata.set(
        "run",
        lua.create_function(move |_, line: String| {
            push(&r, Request::Command(line));
            Ok(())
        })?,
    )?;
    let r = reg.clone();
    strata.set(
        "toggle_panel",
        lua.create_function(move |_, name: String| {
            push(&r, Request::TogglePanel(name));
            Ok(())
        })?,
    )?;
    let r = reg.clone();
    strata.set(
        "exec",
        lua.create_function(move |_, cmd: String| {
            push(&r, Request::Exec(cmd));
            Ok(())
        })?,
    )?;
    let r = reg.clone();
    strata.set(
        "refresh",
        lua.create_function(move |_, ()| {
            push(&r, Request::Refresh);
            Ok(())
        })?,
    )?;

    let r = reg.clone();
    strata.set(
        "select",
        lua.create_function(
            move |_, (title, items, f): (String, Vec<String>, Function)| {
                let callback = r.borrow_mut().store(f);
                push(
                    &r,
                    Request::Select {
                        title,
                        items,
                        callback,
                    },
                );
                Ok(())
            },
        )?,
    )?;

    let r = reg.clone();
    strata.set(
        "input",
        lua.create_function(
            move |_, (prompt, default, f): (String, Option<String>, Function)| {
                let callback = r.borrow_mut().store(f);
                push(
                    &r,
                    Request::Input {
                        prompt,
                        default: default.unwrap_or_default(),
                        callback,
                    },
                );
                Ok(())
            },
        )?,
    )?;

    // --- synchronous helpers ------------------------------------------------
    strata.set(
        "shell",
        lua.create_function(|lua, (cmd, cwd): (String, Option<String>)| {
            let mut command = if cfg!(windows) {
                let mut c = Command::new("cmd");
                c.args(["/C", &cmd]);
                c
            } else {
                let mut c = Command::new("sh");
                c.args(["-c", &cmd]);
                c
            };
            if let Some(cwd) = cwd {
                command.current_dir(cwd);
            }
            match command.output() {
                Ok(out) => Ok(Variadic::from_iter([
                    Value::String(lua.create_string(&out.stdout)?),
                    Value::Integer(out.status.code().unwrap_or(-1) as i64),
                    Value::String(lua.create_string(&out.stderr)?),
                ])),
                Err(e) => Ok(Variadic::from_iter([
                    Value::Nil,
                    Value::Integer(-1),
                    Value::String(lua.create_string(e.to_string())?),
                ])),
            }
        })?,
    )?;

    strata.set(
        "mkdir",
        lua.create_function(|_, path: String| Ok(std::fs::create_dir_all(path).is_ok()))?,
    )?;

    strata.set(
        "which",
        lua.create_function(|_, program: String| Ok(strata_which(&program)))?,
    )?;

    lua.globals().set("strata", strata)?;
    Ok(())
}

fn strata_which(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(program);
            candidate.is_file() || candidate.with_extension("exe").is_file()
        })
    })
}
