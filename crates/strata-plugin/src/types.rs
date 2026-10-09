use mlua::{IntoLua, Lua, Value};

/// Severity of a plugin notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// Something a plugin asked the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Notify {
        message: String,
        level: Level,
    },
    Cd(String),
    /// A built-in action by name, e.g. `toggle_hidden`.
    Action(String),
    /// A command-palette line, e.g. `theme nord`.
    Command(String),
    /// Show a picker; the callback receives the chosen item (or nil).
    Select {
        title: String,
        items: Vec<String>,
        callback: usize,
    },
    /// Show a text prompt; the callback receives the text (or nil).
    Input {
        prompt: String,
        default: String,
        callback: usize,
    },
    TogglePanel(String),
    /// Run a command with the terminal handed over (editors, pagers...).
    Exec(String),
    Refresh,
}

/// Snapshot of the UI passed to every Lua callback as a table.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub cwd: String,
    pub hovered: Option<String>,
    pub hovered_is_dir: bool,
    pub selected: Vec<String>,
    pub panel: usize,
    pub view: String,
    pub theme: String,
    /// `local`, `sftp` or `docker`.
    pub scheme: String,
}

impl IntoLua for Context {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        let t = lua.create_table()?;
        t.set("cwd", self.cwd)?;
        t.set("hovered", self.hovered)?;
        t.set("hovered_is_dir", self.hovered_is_dir)?;
        t.set("selected", self.selected)?;
        t.set("panel", self.panel + 1)?;
        t.set("view", self.view)?;
        t.set("theme", self.theme)?;
        t.set("scheme", self.scheme)?;
        Ok(Value::Table(t))
    }
}

/// Plugin options from `config.toml`, converted without depending on toml.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginValue {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<PluginValue>),
    Map(Vec<(String, PluginValue)>),
}

impl IntoLua for PluginValue {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        Ok(match self {
            Self::Nil => Value::Nil,
            Self::Bool(b) => Value::Boolean(b),
            Self::Int(i) => Value::Integer(i),
            Self::Float(f) => Value::Number(f),
            Self::Str(s) => Value::String(lua.create_string(&s)?),
            Self::List(items) => Value::Table(lua.create_sequence_from(items)?),
            Self::Map(pairs) => Value::Table(lua.create_table_from(pairs)?),
        })
    }
}

/// A key sequence bound by a plugin.
#[derive(Debug, Clone)]
pub struct PluginKey {
    pub keys: String,
    pub callback: usize,
    pub description: String,
    pub plugin: String,
}

/// A UI panel drawn by a plugin.
#[derive(Debug, Clone)]
pub struct PanelDef {
    pub name: String,
    pub title: String,
    pub callback: usize,
    pub plugin: String,
}
