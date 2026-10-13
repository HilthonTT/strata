//! Saved selections: named sets of marked items, kept across sessions and
//! folders (`:select save|load|add|list|delete <name>`).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use strata_plugin::Level;

use super::overlay::{Overlay, TextPopup};
use super::App;

/// The subcommands of `:select`; anything else is a glob.
pub const SUBCOMMANDS: &[&str] = &["save", "load", "add", "list", "delete"];

/// Every saved selection, by name.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Selections(BTreeMap<String, Vec<PathBuf>>);

impl Selections {
    /// Reads `file`; a missing file is an empty set of selections.
    pub fn load(file: &Path) -> Result<Self> {
        match std::fs::read(file) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("reading {}", file.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
        }
    }

    pub fn save(&self, file: &Path) -> Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(file, serde_json::to_vec_pretty(self)?).with_context(|| format!("saving {}", file.display()))
    }

    pub fn get(&self, name: &str) -> Option<&[PathBuf]> {
        self.0.get(name).map(Vec::as_slice)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// Replaces (or creates) `name` with `paths`.
    pub fn set(&mut self, name: &str, paths: Vec<PathBuf>) {
        let mut paths = paths;
        paths.sort();
        paths.dedup();
        self.0.insert(name.to_string(), paths);
    }

    /// Adds `paths` to `name`, creating it if needed. Returns how many were new.
    pub fn add(&mut self, name: &str, paths: Vec<PathBuf>) -> usize {
        let set = self.0.entry(name.to_string()).or_default();
        let before = set.len();
        set.extend(paths);
        set.sort();
        set.dedup();
        set.len() - before
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.0.remove(name).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[PathBuf])> {
        self.0.iter().map(|(n, p)| (n.as_str(), p.as_slice()))
    }
}

fn selections_file() -> PathBuf {
    strata_config::data_dir().join("selections.json")
}

impl App {
    /// `:select save|load|add|list|delete …`. Returns false when `args`
    /// is not a subcommand, so it is taken as a glob.
    pub(super) fn selection_command(&mut self, args: &str) -> bool {
        let (sub, name) = args.split_once(char::is_whitespace).map(|(s, n)| (s, n.trim())).unwrap_or((args, ""));
        if !SUBCOMMANDS.contains(&sub) {
            return false;
        }
        let file = selections_file();
        let mut all = match Selections::load(&file) {
            Ok(all) => all,
            Err(e) => {
                self.error(format!("selections: {e:#}"));
                return true;
            }
        };
        if sub == "list" {
            self.list_selections(&all);
            return true;
        }
        if name.is_empty() {
            self.error(format!("usage: select {sub} <name>"));
            return true;
        }
        let changed = match sub {
            "save" | "add" => {
                if !self.panel().vfs.is_local() {
                    self.notify("only local items can be saved in a selection", Level::Warn);
                    return true;
                }
                let paths = self.panel().targets();
                if paths.is_empty() {
                    self.notify("nothing to save: mark some items first", Level::Warn);
                    return true;
                }
                if sub == "save" {
                    let n = paths.len();
                    all.set(name, paths);
                    self.info(format!("saved {n} item(s) as selection '{name}'"));
                } else {
                    let added = all.add(name, paths);
                    let total = all.get(name).map_or(0, <[PathBuf]>::len);
                    self.info(format!("added {added} item(s) to selection '{name}' ({total} in all)"));
                }
                true
            }
            "delete" => {
                if !all.remove(name) {
                    self.error(format!("no selection named '{name}'"));
                    return true;
                }
                self.info(format!("deleted selection '{name}'"));
                true
            }
            _ => {
                match all.get(name) {
                    Some(paths) => self.load_selection(name, paths.to_vec()),
                    None => self.error(format!("no selection named '{name}'")),
                }
                false
            }
        };
        if changed {
            if let Err(e) = all.save(&file) {
                self.error(format!("selections: {e:#}"));
            }
        }
        true
    }

    /// Marks the saved items that still exist: in place when the panel
    /// already lists them all, otherwise in a flat listing of them (like
    /// `:find` results), so items from several folders can be marked at
    /// once. Missing items are reported.
    fn load_selection(&mut self, name: &str, paths: Vec<PathBuf>) {
        let (found, missing): (Vec<PathBuf>, Vec<PathBuf>) =
            paths.into_iter().partition(|p| std::fs::symlink_metadata(p).is_ok());
        if found.is_empty() {
            return self
                .notify(format!("none of the {} item(s) in '{name}' exist any more", missing.len()), Level::Warn);
        }
        let wanted: HashSet<PathBuf> = found.iter().cloned().collect();
        let p = self.panel();
        let shown = (0..p.len()).filter_map(|row| p.entry_at(row)).filter(|e| wanted.contains(&e.path)).count();
        if p.vfs.is_local() && shown == wanted.len() {
            self.panel_mut().mark_paths(&wanted);
        } else {
            self.show_paths(format!("selection {name}"), found.clone());
            // Marks outlive the listing arriving; items it lacks are dropped then.
            self.panel_mut().marked.extend(found.iter().cloned());
        }
        let n = found.len();
        match missing.as_slice() {
            [] => self.info(format!("marked {n} item(s) from '{name}'")),
            [one] => self.notify(format!("marked {n} item(s) from '{name}'; missing: {}", one.display()), Level::Warn),
            many => self.notify(
                format!("marked {n} item(s) from '{name}'; {} no longer exist (see :messages)", many.len()),
                Level::Warn,
            ),
        }
        if missing.len() > 1 {
            for path in missing.iter().take(20) {
                self.notify(format!("missing from '{name}': {}", path.display()), Level::Warn);
            }
        }
    }

    fn list_selections(&mut self, all: &Selections) {
        if all.names().next().is_none() {
            return self.info("no saved selections: mark items and run :select save <name>");
        }
        let mut lines = Vec::new();
        for (name, paths) in all.iter() {
            let missing = paths.iter().filter(|p| std::fs::symlink_metadata(p).is_err()).count();
            let gone = if missing > 0 { format!(", {missing} missing") } else { String::new() };
            lines.push(format!("{name} ({} item(s){gone})", paths.len()));
            lines.extend(paths.iter().take(5).map(|p| format!("    {}", super::short_path(p))));
            if paths.len() > 5 {
                lines.push(format!("    … and {} more", paths.len() - 5));
            }
        }
        lines.push(String::new());
        lines.push(":select load <name> marks them again".into());
        self.overlay =
            Some(Overlay::Text(TextPopup { title: "Saved selections".into(), lines, scroll: 0, colored: false }));
    }

    /// Completions for `:select <arg>`, as whole command lines.
    pub(super) fn complete_selection(&self, arg: &str) -> Vec<String> {
        match arg.split_once(' ') {
            None => SUBCOMMANDS.iter().filter(|s| s.starts_with(arg)).map(|s| format!("select {s} ")).collect(),
            Some((sub @ ("load" | "add" | "delete" | "save"), prefix)) => {
                let all = Selections::load(&selections_file()).unwrap_or_default();
                all.names().filter(|n| n.starts_with(prefix)).map(|n| format!("select {sub} {n}")).collect()
            }
            Some(_) => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selections_round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/selections.json");
        assert_eq!(Selections::load(&file).unwrap(), Selections::default(), "no file yet");

        let mut all = Selections::default();
        all.set("photos", vec!["/b/2.jpg".into(), "/a/1.jpg".into(), "/a/1.jpg".into()]);
        assert_eq!(all.add("photos", vec!["/a/1.jpg".into(), "/c/3.jpg".into()]), 1);
        all.set("docs", vec!["/d/x.md".into()]);
        all.save(&file).unwrap();

        let back = Selections::load(&file).unwrap();
        assert_eq!(back, all);
        let photos: Vec<PathBuf> = vec!["/a/1.jpg".into(), "/b/2.jpg".into(), "/c/3.jpg".into()];
        assert_eq!(back.get("photos").unwrap(), photos.as_slice());
        assert_eq!(back.names().collect::<Vec<_>>(), ["docs", "photos"]);

        let mut back = back;
        assert!(back.remove("docs"));
        assert!(!back.remove("docs"));
        back.save(&file).unwrap();
        assert_eq!(Selections::load(&file).unwrap().names().collect::<Vec<_>>(), ["photos"]);
    }

    #[test]
    fn a_broken_file_is_an_error_not_a_reset() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("selections.json");
        std::fs::write(&file, "not json").unwrap();
        assert!(Selections::load(&file).is_err());
    }
}
