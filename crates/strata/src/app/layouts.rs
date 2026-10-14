//! Named layouts: the open tabs and panels saved under a name and opened
//! again later (`:layout save|load|list|delete|rename`, `strata --layout`,
//! `default_layout`). A layout is a [`Session`], kept in `layouts.json` in
//! the data directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use strata_plugin::{Level, PluginValue};

use super::overlay::{Confirm, ConfirmState, Overlay, PickerPurpose, PickerState};
use super::session::{write_atomic, Session};
use super::{short_path, App, Focus, View};

/// The subcommands of `:layout`.
pub const SUBCOMMANDS: &[&str] = &["save", "load", "list", "delete", "rename"];

/// Every saved layout, by name.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Layouts(BTreeMap<String, Session>);

impl Layouts {
    /// Reads `file`; a missing file is no layouts.
    pub fn load(file: &Path) -> Result<Self> {
        match std::fs::read(file) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("reading {}", file.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
        }
    }

    pub fn save(&self, file: &Path) -> Result<()> {
        write_atomic(file, &serde_json::to_vec_pretty(self)?).with_context(|| format!("saving {}", file.display()))
    }

    pub fn get(&self, name: &str) -> Option<&Session> {
        self.0.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    pub fn set(&mut self, name: &str, session: Session) {
        self.0.insert(name.to_string(), session);
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.0.remove(name).is_some()
    }

    /// Renames `from` to `to`, which must not be taken.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        if self.0.contains_key(to) {
            return Err(format!("a layout named '{to}' already exists"));
        }
        let session = self.0.remove(from).ok_or_else(|| format!("no layout named '{from}'"))?;
        self.0.insert(to.to_string(), session);
        Ok(())
    }
}

/// Checks a layout name: one word, so it can be typed after `:layout load`
/// and `--layout`.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a layout needs a name".into());
    }
    if name.chars().count() > 64 {
        return Err("layout names are at most 64 characters".into());
    }
    if name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("'{name}': layout names can't contain spaces"));
    }
    Ok(())
}

pub fn layouts_file() -> PathBuf {
    strata_config::data_dir().join("layouts.json")
}

/// Fails unless a layout named `name` is saved, so `--layout` can be
/// checked before the terminal is taken over.
pub fn check_exists(name: &str) -> Result<()> {
    validate_name(name).map_err(anyhow::Error::msg)?;
    if !Layouts::load(&layouts_file())?.contains(name) {
        anyhow::bail!("no layout named '{name}' (save one with :layout save {name})");
    }
    Ok(())
}

/// What strata opens at start.
#[derive(Debug, PartialEq, Eq)]
pub enum Startup<'a> {
    /// A named layout; `explicit` when asked for with `--layout`, so a
    /// missing one is an error rather than a warning.
    Layout { name: &'a str, explicit: bool },
    /// The last session (`restore_session`).
    Session,
    /// The given paths, or `start_dir`.
    Paths,
}

/// `--layout` beats paths, paths beat `default_layout`, and that beats
/// `restore_session`. The picker (`--chooser-file`) only takes an explicit
/// layout.
pub fn startup<'a>(
    flag: Option<&'a str>,
    has_paths: bool,
    default_layout: &'a str,
    restore_session: bool,
    chooser: bool,
) -> Startup<'a> {
    if let Some(name) = flag {
        return Startup::Layout { name, explicit: true };
    }
    if has_paths || chooser {
        return Startup::Paths;
    }
    if !default_layout.is_empty() {
        return Startup::Layout { name: default_layout, explicit: false };
    }
    if restore_session {
        Startup::Session
    } else {
        Startup::Paths
    }
}

impl App {
    /// `:layout save|load|list|delete|rename …`.
    pub(super) fn layout_command(&mut self, args: &str) {
        let (sub, rest) = args.split_once(char::is_whitespace).map(|(s, n)| (s, n.trim())).unwrap_or((args, ""));
        if sub.is_empty() || sub == "list" {
            return self.list_layouts();
        }
        if !SUBCOMMANDS.contains(&sub) {
            return self.error(format!("usage: layout {}", SUBCOMMANDS.join("|")));
        }
        let file = layouts_file();
        let mut all = match Layouts::load(&file) {
            Ok(all) => all,
            Err(e) => return self.error(format!("layouts: {e:#}")),
        };
        if rest.is_empty() {
            let usage = if sub == "rename" { "<name> <new name>" } else { "<name>" };
            return self.error(format!("usage: layout {sub} {usage}"));
        }
        match sub {
            "save" => {
                if let Err(e) = validate_name(rest) {
                    return self.error(e);
                }
                let Some(session) = self.capture_session() else {
                    return self.notify("nothing to save: only local panels go in a layout", Level::Warn);
                };
                if all.contains(rest) {
                    let message = format!("Replace the layout '{rest}' with the open tabs?");
                    let action = Confirm::ReplaceLayout { name: rest.to_string(), session: Box::new(session) };
                    self.overlay = Some(Overlay::Confirm(ConfirmState { message, action }));
                    return;
                }
                self.save_layout(rest.to_string(), session);
            }
            "load" => {
                if let Err(e) = self.load_layout(rest) {
                    self.error(e);
                }
            }
            "delete" => {
                if !all.remove(rest) {
                    return self.error(format!("no layout named '{rest}'"));
                }
                match all.save(&file) {
                    Ok(()) => self.info(format!("deleted layout '{rest}'")),
                    Err(e) => self.error(format!("layouts: {e:#}")),
                }
            }
            _ => {
                let Some((from, to)) = rest.split_once(char::is_whitespace).map(|(f, t)| (f, t.trim())) else {
                    return self.error("usage: layout rename <name> <new name>");
                };
                if let Err(e) = validate_name(to).and_then(|()| all.rename(from, to)) {
                    return self.error(e);
                }
                match all.save(&file) {
                    Ok(()) => self.info(format!("renamed layout '{from}' to '{to}'")),
                    Err(e) => self.error(format!("layouts: {e:#}")),
                }
            }
        }
    }

    /// Saves `session` as `name`, replacing a layout of that name.
    pub(super) fn save_layout(&mut self, name: String, session: Session) {
        let file = layouts_file();
        let result = Layouts::load(&file).and_then(|mut all| {
            all.set(&name, session.clone());
            all.save(&file)
        });
        if let Err(e) = result {
            return self.error(format!("layouts: {e:#}"));
        }
        let (tabs, panels) = session.counts();
        let open: usize = (0..self.tabs.len())
            .map(|i| if i == self.tab { self.panels.len() } else { self.tabs[i].panels.len() })
            .sum();
        let skipped = open.saturating_sub(panels);
        let remote = if skipped > 0 { format!("; {skipped} remote panel(s) left out") } else { String::new() };
        self.info(format!("saved layout '{name}': {tabs} tab(s), {panels} panel(s){remote}"));
        self.emit_plugin_event_with("layout_save", PluginValue::map([("name", PluginValue::str(&name))]));
    }

    /// Replaces the open tabs with the layout `name`.
    pub(super) fn load_layout(&mut self, name: &str) -> Result<(), String> {
        let all = Layouts::load(&layouts_file()).map_err(|e| format!("layouts: {e:#}"))?;
        let session = all.get(name).cloned().ok_or_else(|| format!("no layout named '{name}'"))?;
        let (tabs, panels) = session.counts();
        if !self.apply_session(session) {
            return Err(format!("layout '{name}' has no panels"));
        }
        self.invalidate_preview();
        self.focus = Focus::Panels;
        if self.view != View::Files {
            self.set_view(View::Files);
        }
        self.info(format!("layout '{name}': {tabs} tab(s), {panels} panel(s)"));
        self.emit_plugin_event_with("layout_load", PluginValue::map([("name", PluginValue::str(name))]));
        Ok(())
    }

    /// Lists the layouts in a picker: enter opens one, ctrl+d deletes it.
    fn list_layouts(&mut self) {
        let all = match Layouts::load(&layouts_file()) {
            Ok(all) => all,
            Err(e) => return self.error(format!("layouts: {e:#}")),
        };
        let names: Vec<String> = all.names().map(str::to_string).collect();
        if names.is_empty() {
            return self.info("no saved layouts: open some tabs and run :layout save <name>");
        }
        let width = names.iter().map(|n| n.chars().count()).max().unwrap_or(0).min(24);
        let rows = names
            .iter()
            .filter_map(|n| all.get(n).map(|s| (n, s)))
            .map(|(name, session)| {
                let (tabs, panels) = session.counts();
                let dirs: Vec<String> = session.dirs().take(3).map(short_path).collect();
                let more = if panels > 3 { ", …" } else { "" };
                format!("{name:<width$}  {tabs} tab(s), {panels} panel(s)  {}{more}", dirs.join(", "))
            })
            .collect();
        let title = format!("Layouts ({}) · enter opens · ctrl+d deletes", names.len());
        self.overlay = Some(Overlay::Picker(PickerState::new(title, rows, PickerPurpose::Layouts { names })));
    }

    /// `ctrl+d` in the layouts picker asks to delete the highlighted layout.
    pub(super) fn layouts_picker_key(&mut self, ctrl_char: char) -> bool {
        let Some(Overlay::Picker(picker)) = &self.overlay else { return false };
        let PickerPurpose::Layouts { names } = &picker.purpose else { return false };
        if ctrl_char != 'd' {
            return false;
        }
        if let Some(name) = picker.selected_index().and_then(|i| names.get(i)).cloned() {
            let message = format!("Delete the layout '{name}'?");
            self.overlay = Some(Overlay::Confirm(ConfirmState { message, action: Confirm::DeleteLayout(name) }));
        }
        true
    }

    /// Deletes a layout confirmed from the picker, then lists the rest.
    pub(super) fn delete_layout(&mut self, name: String) {
        self.layout_command(&format!("delete {name}"));
        if Layouts::load(&layouts_file()).is_ok_and(|all| all.names().next().is_some()) {
            self.list_layouts();
        }
    }

    /// Completions for `:layout <arg>`, as whole command lines.
    pub(super) fn complete_layout(&self, arg: &str) -> Vec<String> {
        match arg.split_once(' ') {
            None => SUBCOMMANDS.iter().filter(|s| s.starts_with(arg)).map(|s| format!("layout {s} ")).collect(),
            Some((sub @ ("load" | "delete" | "save" | "rename"), prefix)) if !prefix.contains(' ') => {
                let all = Layouts::load(&layouts_file()).unwrap_or_default();
                let space = if sub == "rename" { " " } else { "" };
                all.names().filter(|n| n.starts_with(prefix)).map(|n| format!("layout {sub} {n}{space}")).collect()
            }
            Some(_) => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::session::tests::session_of;
    use super::*;

    #[test]
    fn layouts_round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/layouts.json");
        assert_eq!(Layouts::load(&file).unwrap(), Layouts::default(), "no file yet");

        let mut all = Layouts::default();
        all.set("work", session_of(&["/a", "/b"]));
        all.set("home", session_of(&["/c"]));
        all.save(&file).unwrap();
        assert!(!file.with_extension("json.tmp").exists(), "the temporary file is renamed into place");

        let mut back = Layouts::load(&file).unwrap();
        assert_eq!(back, all);
        assert_eq!(back.names().collect::<Vec<_>>(), ["home", "work"]);
        assert_eq!(back.get("work").unwrap().counts(), (2, 2));

        assert_eq!(back.rename("home", "work"), Err("a layout named 'work' already exists".into()));
        assert_eq!(back.rename("gone", "x"), Err("no layout named 'gone'".into()));
        back.rename("home", "house").unwrap();
        assert!(back.remove("work"));
        assert!(!back.remove("work"));
        back.save(&file).unwrap();
        assert_eq!(Layouts::load(&file).unwrap().names().collect::<Vec<_>>(), ["house"]);
    }

    #[test]
    fn a_broken_file_is_an_error_not_a_reset() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("layouts.json");
        std::fs::write(&file, "not json").unwrap();
        assert!(Layouts::load(&file).is_err());
    }

    #[test]
    fn names_are_one_word() {
        assert!(validate_name("work").is_ok());
        assert!(validate_name("photos-2026_q3.v2").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("two words").is_err());
        assert!(validate_name("tab\there").is_err());
        assert!(validate_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn startup_precedence() {
        use Startup::*;
        // --layout wins over everything.
        assert_eq!(startup(Some("a"), false, "b", true, false), Layout { name: "a", explicit: true });
        assert_eq!(startup(Some("a"), false, "", false, true), Layout { name: "a", explicit: true });
        // Paths beat default_layout and restore_session.
        assert_eq!(startup(None, true, "b", true, false), Paths);
        // default_layout beats restore_session.
        assert_eq!(startup(None, false, "b", true, false), Layout { name: "b", explicit: false });
        assert_eq!(startup(None, false, "", true, false), Session);
        assert_eq!(startup(None, false, "", false, false), Paths);
        // The picker ignores default_layout and the session.
        assert_eq!(startup(None, false, "b", true, true), Paths);
    }
}
