//! Vim-style key sequences (`g g`, `y y`, `ctrl+d`) mapped to actions,
//! command-palette commands or plugin callbacks.

use std::collections::HashMap;
use std::fmt;

use anyhow::{bail, Result};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

macro_rules! actions {
    ($($variant:ident => $name:literal, $desc:literal;)*) => {
        /// Every built-in action a key can trigger.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Action { $($variant),* }

        impl Action {
            pub const ALL: &'static [Action] = &[$(Action::$variant),*];

            pub fn name(self) -> &'static str {
                match self { $(Action::$variant => $name),* }
            }

            pub fn description(self) -> &'static str {
                match self { $(Action::$variant => $desc),* }
            }

            pub fn from_name(name: &str) -> Option<Self> {
                match name { $($name => Some(Action::$variant),)* _ => None }
            }
        }
    };
}

actions! {
    Up => "up", "Move cursor up";
    Down => "down", "Move cursor down";
    Top => "top", "Jump to the first item";
    Bottom => "bottom", "Jump to the last item";
    PageUp => "page_up", "Scroll a page up";
    PageDown => "page_down", "Scroll a page down";
    HalfPageUp => "half_page_up", "Scroll half a page up";
    HalfPageDown => "half_page_down", "Scroll half a page down";
    Parent => "parent", "Go to the parent directory";
    Open => "open", "Enter directory / open file";
    Back => "back", "Go back in history";
    Forward => "forward", "Go forward in history";
    Home => "home", "Go to the home directory";
    Root => "root", "Go to the filesystem root";
    NextPanel => "next_panel", "Focus the next panel";
    PrevPanel => "prev_panel", "Focus the previous panel";
    NewPanel => "new_panel", "Open a new panel";
    NewTab => "new_tab", "Open a new tab";
    CloseTab => "close_tab", "Close the current tab";
    NextTab => "next_tab", "Go to the next tab";
    PrevTab => "prev_tab", "Go to the previous tab";
    ClosePanel => "close_panel", "Close the focused panel";
    FocusSidebar => "focus_sidebar", "Focus the sidebar";
    FocusPanels => "focus_panels", "Focus the file panels";
    ToggleSelect => "toggle_select", "Mark / unmark item";
    SelectAll => "select_all", "Mark every item";
    InvertSelection => "invert_selection", "Invert marks";
    Clear => "clear", "Clear marks, filter and pending keys";
    VisualMode => "visual_mode", "Range-select with movement";
    SelectDown => "select_down", "Mark item and move down";
    SelectUp => "select_up", "Mark item and move up";
    Copy => "copy", "Copy marked items to the clipboard";
    Cut => "cut", "Cut marked items to the clipboard";
    Paste => "paste", "Paste the clipboard here";
    CopyToOther => "copy_to_other", "Copy marked items to the next panel";
    MoveToOther => "move_to_other", "Move marked items to the next panel";
    Delete => "delete", "Move marked items to the trash";
    DeletePermanent => "delete_permanent", "Delete marked items permanently";
    Rename => "rename", "Rename item";
    BulkRename => "bulk_rename", "Rename marked items in $EDITOR";
    NewFile => "new_file", "Create a file";
    NewDir => "new_dir", "Create a directory";
    Duplicate => "duplicate", "Duplicate marked items here";
    PasteSymlink => "paste_symlink", "Paste the clipboard as symbolic links";
    PasteRelativeSymlink => "paste_relative_symlink", "Paste the clipboard as relative symbolic links";
    PasteHardlink => "paste_hardlink", "Paste the clipboard as hard links";
    CopyPath => "copy_path", "Copy path to the system clipboard";
    CopyCwd => "copy_cwd", "Copy the current directory's path";
    CancelJob => "cancel_job", "Cancel the latest running job";
    Filter => "filter", "Filter the current directory";
    FuzzyFind => "fuzzy_find", "Fuzzy-find files recursively";
    ContentSearch => "content_search", "Search file contents (ripgrep)";
    Undo => "undo", "Undo the last file operation";
    Redo => "redo", "Redo what was undone";
    PreviewDown => "preview_down", "Scroll the preview down";
    PreviewUp => "preview_up", "Scroll the preview up";
    PreviewFind => "preview_find", "Search inside the preview";
    PreviewNext => "preview_next", "Next match in the preview";
    PreviewPrev => "preview_prev", "Previous match in the preview";
    ToggleHidden => "toggle_hidden", "Show / hide dotfiles";
    TogglePreview => "toggle_preview", "Show / hide the preview";
    ToggleSidebar => "toggle_sidebar", "Show / hide the sidebar";
    ToggleFooter => "toggle_footer", "Show / hide the footer";
    CycleSort => "cycle_sort", "Cycle sort key";
    SortMenu => "sort_menu", "Choose how to sort";
    ReverseSort => "reverse_sort", "Reverse sort order";
    Refresh => "refresh", "Reload directories";
    ViewFiles => "view_files", "Files view";
    ViewDashboard => "view_dashboard", "Disk & memory dashboard";
    ViewDocker => "view_docker", "Docker containers";
    ViewConnections => "view_connections", "NAS connections";
    ThemePicker => "theme_picker", "Pick a theme";
    Help => "help", "Show key bindings";
    CommandPalette => "command_palette", "Run a command";
    Edit => "edit", "Open in $EDITOR";
    EditDir => "edit_dir", "Open the current directory in the editor";
    OpenWith => "open_with", "Open with the system default app";
    Shell => "shell", "Open a shell here";
    Pin => "pin", "Pin / unpin directory in the sidebar";
    Quit => "quit", "Quit strata";
    QuitCd => "quit_cd", "Quit and cd the shell to this directory";
}

/// A single key with modifiers, normalised so `G` and `shift+g` are equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyPress {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeyPress {
    pub fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        let mut mods = mods & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let code = match code {
            KeyCode::Char(c) => {
                // The character already carries case; shift is implicit.
                mods.remove(KeyModifiers::SHIFT);
                if mods.contains(KeyModifiers::CONTROL) {
                    KeyCode::Char(c.to_ascii_lowercase())
                } else {
                    KeyCode::Char(c)
                }
            }
            KeyCode::BackTab => {
                mods.remove(KeyModifiers::SHIFT);
                KeyCode::BackTab
            }
            other => other,
        };
        Self { code, mods }
    }

    pub fn from_event(ev: &KeyEvent) -> Self {
        Self::new(ev.code, ev.modifiers)
    }

    /// Parses `ctrl+d`, `G`, `space`, `f5`, `alt+enter`...
    pub fn parse(s: &str) -> Result<Self> {
        let mut mods = KeyModifiers::NONE;
        let mut rest = s;
        loop {
            let lower = rest.to_ascii_lowercase();
            let (prefix_len, m) = if lower.starts_with("ctrl+") {
                (5, KeyModifiers::CONTROL)
            } else if lower.starts_with("alt+") {
                (4, KeyModifiers::ALT)
            } else if lower.starts_with("shift+") {
                (6, KeyModifiers::SHIFT)
            } else {
                break;
            };
            if rest.len() == prefix_len {
                break; // a literal like "ctrl+" is not a key
            }
            mods |= m;
            rest = &rest[prefix_len..];
        }
        let code = match rest.to_ascii_lowercase().as_str() {
            "enter" | "return" | "cr" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "space" => KeyCode::Char(' '),
            "backspace" | "bs" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            f if f.len() > 1 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
                KeyCode::F(f[1..].parse().unwrap_or(1))
            }
            _ => {
                let mut chars = rest.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => {
                        if mods.contains(KeyModifiers::SHIFT) && c.is_ascii_lowercase() {
                            KeyCode::Char(c.to_ascii_uppercase())
                        } else {
                            KeyCode::Char(c)
                        }
                    }
                    _ => bail!("unknown key '{s}'"),
                }
            }
        };
        Ok(Self::new(code, mods))
    }
}

impl fmt::Display for KeyPress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            f.write_str("ctrl+")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            f.write_str("alt+")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            f.write_str("shift+")?;
        }
        match self.code {
            KeyCode::Char(' ') => f.write_str("space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Enter => f.write_str("enter"),
            KeyCode::Esc => f.write_str("esc"),
            KeyCode::Tab => f.write_str("tab"),
            KeyCode::BackTab => f.write_str("shift+tab"),
            KeyCode::Backspace => f.write_str("backspace"),
            KeyCode::Delete => f.write_str("delete"),
            KeyCode::Up => f.write_str("↑"),
            KeyCode::Down => f.write_str("↓"),
            KeyCode::Left => f.write_str("←"),
            KeyCode::Right => f.write_str("→"),
            KeyCode::Home => f.write_str("home"),
            KeyCode::End => f.write_str("end"),
            KeyCode::PageUp => f.write_str("pageup"),
            KeyCode::PageDown => f.write_str("pagedown"),
            KeyCode::F(n) => write!(f, "f{n}"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// Parses a space-separated key sequence such as `g g`.
pub fn parse_sequence(s: &str) -> Result<Vec<KeyPress>> {
    let keys: Vec<KeyPress> = s.split_whitespace().map(KeyPress::parse).collect::<Result<_>>()?;
    if keys.is_empty() {
        bail!("empty key sequence");
    }
    Ok(keys)
}

pub fn format_sequence(keys: &[KeyPress]) -> String {
    keys.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")
}

/// What a key sequence does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    Action(Action),
    /// A command-palette line, e.g. `cd ~/projects`.
    Command(String),
    /// A Lua callback registered by a plugin.
    Plugin {
        callback: usize,
        description: String,
    },
}

impl Binding {
    /// `"copy"` → action, `":cd ~"` → command.
    pub fn parse(value: &str) -> Result<Self> {
        if let Some(cmd) = value.strip_prefix(':') {
            return Ok(Self::Command(cmd.trim().to_string()));
        }
        Action::from_name(value.trim()).map(Self::Action).ok_or_else(|| anyhow::anyhow!("unknown action '{value}'"))
    }

    pub fn description(&self) -> String {
        match self {
            Self::Action(a) => a.description().to_string(),
            Self::Command(c) => format!(":{c}"),
            Self::Plugin { description, .. } => description.clone(),
        }
    }
}

/// Result of looking up a (possibly partial) key sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// Bound, and no longer sequence starts with it.
    Exact(Binding),
    /// More keys could follow; carries the binding if this prefix is also bound.
    Prefix(Option<Binding>),
    None,
}

#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bindings: HashMap<Vec<KeyPress>, Binding>,
}

/// Which built-in key layout to start from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeymapPreset {
    /// Vim-style: `y y`, `d d`, `g g`, `h j k l`.
    #[default]
    Vim,
    /// superfile-style: `ctrl+c`, `ctrl+x`, `ctrl+v`, `ctrl+d`...
    Default,
}

/// Keys shared by both presets.
const COMMON: &[(&str, &str)] = &[
    ("up", "up"),
    ("down", "down"),
    ("home", "top"),
    ("end", "bottom"),
    ("pageup", "page_up"),
    ("pagedown", "page_down"),
    ("left", "parent"),
    ("backspace", "parent"),
    ("right", "open"),
    ("enter", "open"),
    ("tab", "next_panel"),
    ("backtab", "prev_panel"),
    ("esc", "clear"),
    ("v", "visual_mode"),
    ("J", "select_down"),
    ("K", "select_up"),
    ("/", "filter"),
    (".", "toggle_hidden"),
    ("F", "toggle_footer"),
    ("1", "view_files"),
    ("2", "view_dashboard"),
    ("3", "view_docker"),
    ("4", "view_connections"),
    ("T", "theme_picker"),
    ("?", "help"),
    (":", "command_palette"),
    ("e", "edit"),
    ("E", "edit_dir"),
    ("P", "pin"),
    ("D", "delete_permanent"),
    ("Y", "duplicate"),
    ("q", "quit"),
    ("Q", "quit_cd"),
    ("ctrl+g", "content_search"),
    ("f5", "refresh"),
    ("alt+j", "preview_down"),
    ("alt+down", "preview_down"),
    ("alt+k", "preview_up"),
    ("alt+up", "preview_up"),
    ("alt+/", "preview_find"),
    ("alt+n", "preview_next"),
    ("alt+N", "preview_prev"),
    ("alt+1", ":tab 1"),
    ("alt+2", ":tab 2"),
    ("alt+3", ":tab 3"),
    ("alt+4", ":tab 4"),
    ("alt+5", ":tab 5"),
    ("alt+6", ":tab 6"),
    ("alt+7", ":tab 7"),
    ("alt+8", ":tab 8"),
    ("alt+9", ":tab 9"),
];

const VIM: &[(&str, &str)] = &[
    ("k", "up"),
    ("j", "down"),
    ("g g", "top"),
    ("G", "bottom"),
    ("ctrl+b", "page_up"),
    ("ctrl+f", "page_down"),
    ("ctrl+u", "half_page_up"),
    ("ctrl+d", "half_page_down"),
    ("h", "parent"),
    ("l", "open"),
    ("H", "back"),
    ("L", "forward"),
    ("~", "home"),
    ("g h", "home"),
    ("g /", "root"),
    ("n", "new_panel"),
    ("ctrl+w", "close_panel"),
    ("ctrl+h", "focus_sidebar"),
    ("ctrl+l", "focus_panels"),
    ("space", "toggle_select"),
    ("ctrl+a", "select_all"),
    ("*", "invert_selection"),
    ("y y", "copy"),
    ("x", "cut"),
    ("p", "paste"),
    ("c", "copy_to_other"),
    ("m", "move_to_other"),
    ("d d", "delete"),
    ("r", "rename"),
    ("R", "bulk_rename"),
    ("a", "new_file"),
    ("A", "new_dir"),
    ("y p", "copy_path"),
    ("y d", "copy_cwd"),
    ("ctrl+x", "cancel_job"),
    ("f", "fuzzy_find"),
    ("w", "toggle_preview"),
    ("b", "toggle_sidebar"),
    ("s", "sort_menu"),
    ("S", "reverse_sort"),
    ("ctrl+r", "redo"),
    ("g f", "view_files"),
    ("g d", "view_dashboard"),
    ("g k", "view_docker"),
    ("g c", "view_connections"),
    ("o", "open_with"),
    ("!", "shell"),
    ("ctrl+c", "quit"),
    ("u", "undo"),
    ("t", "new_tab"),
    ("g t", "next_tab"),
    ("g T", "prev_tab"),
    ("g q", "close_tab"),
    ("g l", "paste_symlink"),
    ("g L", "paste_relative_symlink"),
];

/// Mirrors superfile's default hotkeys where strata has the feature.
const STANDARD: &[(&str, &str)] = &[
    ("k", "up"),
    ("j", "down"),
    ("h", "parent"),
    ("l", "open"),
    ("L", "next_panel"),
    ("H", "prev_panel"),
    ("shift+left", "prev_panel"),
    ("n", "new_panel"),
    ("N", "new_panel"),
    ("w", "close_panel"),
    ("s", "focus_sidebar"),
    ("o", "sort_menu"),
    ("R", "reverse_sort"),
    ("f", "toggle_preview"),
    ("ctrl+n", "new_file"),
    ("ctrl+r", "rename"),
    ("alt+r", "bulk_rename"),
    ("ctrl+c", "copy"),
    ("ctrl+x", "cut"),
    ("ctrl+v", "paste"),
    ("ctrl+d", "delete"),
    ("delete", "delete"),
    ("ctrl+a", ":compress"),
    ("ctrl+e", ":extract"),
    ("ctrl+p", "copy_path"),
    ("c", "copy_cwd"),
    ("A", "select_all"),
    ("space", "toggle_select"),
    ("ctrl+f", "fuzzy_find"),
    (">", "copy_to_other"),
    ("<", "move_to_other"),
    ("alt+c", "cancel_job"),
    ("b", "toggle_sidebar"),
    ("ctrl+l", "focus_panels"),
    ("O", "open_with"),
    ("!", "shell"),
    ("ctrl+z", "undo"),
    ("ctrl+y", "redo"),
    ("ctrl+t", "new_tab"),
    ("alt+w", "close_tab"),
    ("alt+right", "next_tab"),
    ("alt+left", "prev_tab"),
];

impl Keymap {
    /// The vim preset.
    pub fn with_defaults() -> Self {
        Self::preset(KeymapPreset::Vim)
    }

    pub fn preset(preset: KeymapPreset) -> Self {
        let mut map = Self::default();
        let specific = match preset {
            KeymapPreset::Vim => VIM,
            KeymapPreset::Default => STANDARD,
        };
        for (keys, value) in COMMON.iter().chain(specific) {
            let seq = parse_sequence(keys).expect("preset keys are valid");
            map.bindings.insert(seq, Binding::parse(value).expect("preset bindings are valid"));
        }
        map
    }

    /// Applies `[keys]` overrides; invalid entries are returned as errors.
    pub fn apply_overrides(&mut self, overrides: &HashMap<String, String>) -> Vec<String> {
        let mut errors = Vec::new();
        for (keys, value) in overrides {
            match (parse_sequence(keys), value.as_str()) {
                (Ok(seq), "none" | "") => {
                    self.bindings.remove(&seq);
                }
                (Ok(seq), _) => match Binding::parse(value) {
                    Ok(b) => {
                        self.bindings.insert(seq, b);
                    }
                    Err(e) => errors.push(format!("keys.\"{keys}\": {e}")),
                },
                (Err(e), _) => errors.push(format!("keys.\"{keys}\": {e}")),
            }
        }
        errors
    }

    pub fn bind(&mut self, seq: Vec<KeyPress>, binding: Binding) {
        self.bindings.insert(seq, binding);
    }

    pub fn lookup(&self, seq: &[KeyPress]) -> Lookup {
        let exact = self.bindings.get(seq).cloned();
        let longer = self.bindings.keys().any(|k| k.len() > seq.len() && k.starts_with(seq));
        match (exact, longer) {
            (Some(b), false) => Lookup::Exact(b),
            (b, true) => Lookup::Prefix(b),
            (None, false) => Lookup::None,
        }
    }

    /// Bindings that continue `prefix`, for the which-key hint.
    pub fn continuations(&self, prefix: &[KeyPress]) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .bindings
            .iter()
            .filter(|(k, _)| k.len() > prefix.len() && k.starts_with(prefix))
            .map(|(k, b)| (format_sequence(&k[prefix.len()..]), b.description()))
            .collect();
        out.sort();
        out
    }

    /// All bindings, grouped by description, for the help screen.
    pub fn describe(&self) -> Vec<(String, String)> {
        let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
        for (keys, binding) in &self.bindings {
            grouped.entry(binding.description()).or_default().push(format_sequence(keys));
        }
        let mut out: Vec<(String, String)> = grouped
            .into_iter()
            .map(|(desc, mut keys)| {
                keys.sort_by_key(|k| (k.chars().count(), k.clone()));
                (keys.join(", "), desc)
            })
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1));
        out
    }

    /// Keys bound to an action, for hints in the UI.
    pub fn keys_for(&self, action: Action) -> Option<String> {
        let mut keys: Vec<String> = self
            .bindings
            .iter()
            .filter(|(_, b)| **b == Binding::Action(action))
            .map(|(k, _)| format_sequence(k))
            .collect();
        keys.sort_by_key(|k| (k.chars().count(), k.clone()));
        keys.into_iter().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> KeyPress {
        KeyPress::parse(s).unwrap()
    }

    #[test]
    fn parses_keys() {
        assert_eq!(key("ctrl+D"), KeyPress::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(key("G"), KeyPress::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
        assert_eq!(key("shift+g"), key("G"));
        assert_eq!(key("space").code, KeyCode::Char(' '));
        assert_eq!(key("f5").code, KeyCode::F(5));
        assert_eq!(key("+").code, KeyCode::Char('+'));
        assert!(KeyPress::parse("nope").is_err());
    }

    #[test]
    fn sequences_resolve() {
        let map = Keymap::with_defaults();
        assert_eq!(map.lookup(&[key("g")]), Lookup::Prefix(None));
        assert_eq!(map.lookup(&[key("g"), key("g")]), Lookup::Exact(Binding::Action(Action::Top)));
        assert_eq!(map.lookup(&[key("j")]), Lookup::Exact(Binding::Action(Action::Down)));
        assert_eq!(map.lookup(&[key("Z")]), Lookup::None);
    }

    #[test]
    fn overrides_apply_and_report_errors() {
        let mut map = Keymap::with_defaults();
        let mut o = HashMap::new();
        o.insert("g p".to_string(), ":cd ~/projects".to_string());
        o.insert("j".to_string(), "none".to_string());
        o.insert("z".to_string(), "not_an_action".to_string());
        let errors = map.apply_overrides(&o);
        assert_eq!(errors.len(), 1);
        assert_eq!(map.lookup(&[key("g"), key("p")]), Lookup::Exact(Binding::Command("cd ~/projects".into())));
        assert_eq!(map.lookup(&[key("j")]), Lookup::None);
    }

    #[test]
    fn default_preset_matches_superfile() {
        let map = Keymap::preset(KeymapPreset::Default);
        assert_eq!(map.lookup(&[key("ctrl+c")]), Lookup::Exact(Binding::Action(Action::Copy)));
        assert_eq!(map.lookup(&[key("ctrl+v")]), Lookup::Exact(Binding::Action(Action::Paste)));
        assert_eq!(map.lookup(&[key("ctrl+a")]), Lookup::Exact(Binding::Command("compress".into())));
        assert_eq!(map.lookup(&[key("Q")]), Lookup::Exact(Binding::Action(Action::QuitCd)));
        // No multi-key sequences, so nothing waits for a second key.
        assert_eq!(map.lookup(&[key("g")]), Lookup::None);
    }

    #[test]
    fn every_action_has_a_unique_name() {
        let mut names: Vec<_> = Action::ALL.iter().map(|a| a.name()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), Action::ALL.len());
    }
}
