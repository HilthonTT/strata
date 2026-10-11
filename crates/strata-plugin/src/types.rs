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
    /// Mark these paths in the focused panel (`clear` drops other marks).
    Mark {
        paths: Vec<String>,
        clear: bool,
    },
    /// Show these paths as a flat list in the focused panel.
    Show {
        title: String,
        paths: Vec<String>,
    },
    /// Add or remove a tag on these paths.
    Tag {
        paths: Vec<String>,
        tag: String,
        add: bool,
    },
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
    /// Whether Nerd Font icons are enabled.
    pub icons: bool,
    /// Tags of the hovered item.
    pub tags: Vec<String>,
    /// The panel's filter text.
    pub filter: String,
    /// What a flat listing (`:find`, `:flatten`) shows, if one is open.
    pub listing: Option<String>,
    /// Tab number (0-based here, 1-based in Lua).
    pub tab: usize,
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
        t.set("icons", self.icons)?;
        t.set("tags", self.tags)?;
        t.set("filter", self.filter)?;
        t.set("listing", self.listing)?;
        t.set("tab", self.tab + 1)?;
        Ok(Value::Table(t))
    }
}

/// Plugin options from `config.toml`, converted without depending on toml;
/// also the data passed to event handlers.
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

impl PluginValue {
    /// A table from `(key, value)` pairs.
    pub fn map<K: Into<String>>(pairs: impl IntoIterator<Item = (K, PluginValue)>) -> Self {
        Self::Map(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    pub fn str(s: impl Into<String>) -> Self {
        Self::Str(s.into())
    }

    /// A list of strings, e.g. paths.
    pub fn strings<S: Into<String>>(items: impl IntoIterator<Item = S>) -> Self {
        Self::List(items.into_iter().map(|s| Self::Str(s.into())).collect())
    }
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
