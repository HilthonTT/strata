//! Sessions (`restore_session`): the tabs and panels open at quit, reopened
//! the next time strata starts without paths. Named layouts
//! ([`super::layouts`]) are sessions saved under a name.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use strata_core::sort::{SortKey, SortOptions};

use super::tabs::Tab;
use super::App;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    tab: usize,
    tabs: Vec<SavedTab>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SavedTab {
    active: usize,
    panels: Vec<SavedPanel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SavedPanel {
    cwd: PathBuf,
    sort: SortKey,
    reverse: bool,
    dirs_first: bool,
    /// The item under the cursor, by name (the top-level folder holding it
    /// in a tree). Missing in sessions saved by older versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    tree: bool,
}

impl Session {
    /// How many tabs and panels it holds.
    pub fn counts(&self) -> (usize, usize) {
        (self.tabs.len(), self.tabs.iter().map(|t| t.panels.len()).sum())
    }

    /// The folder of every panel, tab by tab.
    pub fn dirs(&self) -> impl Iterator<Item = &Path> {
        self.tabs.iter().flat_map(|t| t.panels.iter().map(|p| p.cwd.as_path()))
    }
}

fn session_file() -> PathBuf {
    strata_config::data_dir().join("session.json")
}

/// Writes `bytes` through a temporary file so a crash never leaves half of it.
pub(super) fn write_atomic(file: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, file))?;
    Ok(())
}

impl App {
    /// Writes the open tabs and local panels to the session file, when
    /// `restore_session` is on.
    pub fn save_session(&self) -> anyhow::Result<()> {
        if !self.config.general.restore_session || self.chooser {
            return Ok(());
        }
        let Some(session) = self.capture_session() else { return Ok(()) };
        let path = session_file();
        write_atomic(&path, &serde_json::to_vec_pretty(&session)?)
            .with_context(|| format!("saving the session to {}", path.display()))
    }

    /// The open tabs and their local panels; `None` when every panel is remote.
    pub(super) fn capture_session(&self) -> Option<Session> {
        let tab_of = |i: usize| {
            if i == self.tab {
                (&self.panels, self.active)
            } else {
                (&self.tabs[i].panels, self.tabs[i].active)
            }
        };
        let mut session = Session { tab: 0, tabs: Vec::new() };
        for i in 0..self.tabs.len() {
            let (panels, active) = tab_of(i);
            // Remote panels can't be reopened without reconnecting, so they're left out.
            let saved: Vec<SavedPanel> = panels
                .iter()
                .filter(|p| p.vfs.is_local())
                .map(|p| SavedPanel {
                    cwd: p.cwd.clone(),
                    sort: p.sort.key,
                    reverse: p.sort.reverse,
                    dirs_first: p.sort.dirs_first,
                    cursor: p.hovered().and_then(|e| top_level_name(&p.cwd, &e.path)),
                    tree: p.is_tree(),
                })
                .collect();
            if saved.is_empty() {
                continue;
            }
            if i == self.tab {
                session.tab = session.tabs.len();
            }
            let active = active.min(saved.len() - 1);
            session.tabs.push(SavedTab { active, panels: saved });
        }
        (!session.tabs.is_empty()).then_some(session)
    }

    /// Reopens the saved tabs and panels. Returns false when there is no
    /// usable session.
    pub(super) fn restore_session(&mut self) -> bool {
        let Some(session) = std::fs::read(session_file()).ok().and_then(|b| serde_json::from_slice::<Session>(&b).ok())
        else {
            return false;
        };
        self.apply_session(session)
    }

    /// Replaces every tab with the saved ones. Folders that are gone open
    /// their nearest existing parent. Returns false when nothing is left
    /// to open, leaving the tabs as they were.
    pub(super) fn apply_session(&mut self, session: Session) -> bool {
        let defaults = self.panel().sort;
        let show_hidden = self.config.general.show_hidden;
        let mut tabs: Vec<Tab> = session
            .tabs
            .into_iter()
            .filter(|t| !t.panels.is_empty())
            .take(9)
            .map(|t| {
                let panels: Vec<_> = t
                    .panels
                    .into_iter()
                    .take(6)
                    .map(|p| {
                        let sort =
                            SortOptions { key: p.sort, reverse: p.reverse, dirs_first: p.dirs_first, ..defaults };
                        let cwd = existing_dir(&p.cwd);
                        let moved = cwd != p.cwd;
                        let mut panel = self.make_panel(self.local.clone(), cwd, sort, show_hidden);
                        if p.tree {
                            panel.set_tree(true);
                        }
                        // In a parent folder the saved item isn't listed.
                        if let Some(name) = p.cursor.filter(|_| !moved) {
                            panel.focus_name(&name);
                        }
                        panel
                    })
                    .collect();
                Tab { active: t.active.min(panels.len() - 1), panels }
            })
            .collect();
        if tabs.is_empty() {
            return false;
        }
        let current = session.tab.min(tabs.len() - 1);
        let tab = std::mem::take(&mut tabs[current]);
        self.panels = tab.panels;
        self.active = tab.active;
        self.tabs = tabs;
        self.tab = current;
        true
    }
}

/// The name of the entry directly in `cwd` that is or holds `path`.
fn top_level_name(cwd: &Path, path: &Path) -> Option<String> {
    let first = path.strip_prefix(cwd).ok()?.components().next()?;
    Some(first.as_os_str().to_string_lossy().into_owned())
}

/// `dir`, or its nearest ancestor that still exists.
fn existing_dir(dir: &Path) -> PathBuf {
    dir.ancestors().find(|d| d.is_dir()).map(Path::to_path_buf).unwrap_or_else(super::sidebar::root_dir)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A session of one tab per folder, one panel each.
    pub fn session_of(dirs: &[&str]) -> Session {
        let tabs = dirs
            .iter()
            .map(|d| SavedTab {
                active: 0,
                panels: vec![SavedPanel {
                    cwd: d.into(),
                    sort: SortKey::Name,
                    reverse: false,
                    dirs_first: true,
                    cursor: None,
                    tree: false,
                }],
            })
            .collect();
        Session { tab: 0, tabs }
    }

    #[test]
    fn falls_back_to_an_existing_parent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(existing_dir(&dir.path().join("gone/deeper")), dir.path());
        assert_eq!(existing_dir(dir.path()), dir.path());
    }

    #[test]
    fn the_cursor_is_kept_as_a_top_level_name() {
        let cwd = Path::new("/p");
        assert_eq!(top_level_name(cwd, Path::new("/p/a.txt")).as_deref(), Some("a.txt"));
        assert_eq!(top_level_name(cwd, Path::new("/p/src/lib.rs")).as_deref(), Some("src"));
        assert_eq!(top_level_name(cwd, Path::new("/elsewhere/x")), None);
    }

    #[test]
    fn session_round_trips() {
        let session = Session {
            tab: 1,
            tabs: vec![
                SavedTab {
                    active: 0,
                    panels: vec![SavedPanel {
                        cwd: "/a".into(),
                        sort: SortKey::Size,
                        reverse: true,
                        dirs_first: false,
                        cursor: Some("notes.md".into()),
                        tree: true,
                    }],
                },
                SavedTab { active: 0, panels: vec![] },
            ],
        };
        let json = serde_json::to_vec(&session).unwrap();
        assert_eq!(serde_json::from_slice::<Session>(&json).unwrap(), session);
        assert_eq!(session.counts(), (2, 1));
    }

    #[test]
    fn sessions_from_older_versions_still_load() {
        let old = serde_json::to_string(&session_of(&["/a"])).unwrap();
        assert!(!old.contains("cursor") && !old.contains("tree"), "defaults aren't written: {old}");
        let json = r#"{"tab":0,"tabs":[{"active":0,"panels":[{"cwd":"/a","sort":"Name","reverse":false,"dirs_first":true}]}]}"#;
        let sort = serde_json::to_string(&SortKey::Name).unwrap();
        let json = json.replace("\"Name\"", &sort);
        assert_eq!(serde_json::from_str::<Session>(&json).unwrap(), session_of(&["/a"]));
    }
}
