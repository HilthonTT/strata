//! Sessions (`restore_session`): the tabs and panels open at quit, reopened
//! the next time strata starts without paths.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use strata_core::sort::{SortKey, SortOptions};

use super::tabs::Tab;
use super::App;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Session {
    tab: usize,
    tabs: Vec<SavedTab>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct SavedTab {
    active: usize,
    panels: Vec<SavedPanel>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct SavedPanel {
    cwd: PathBuf,
    sort: SortKey,
    reverse: bool,
    dirs_first: bool,
}

fn session_file() -> PathBuf {
    strata_config::data_dir().join("session.json")
}

impl App {
    /// Writes the open tabs and local panels to the session file, when
    /// `restore_session` is on.
    pub fn save_session(&self) -> anyhow::Result<()> {
        if !self.config.general.restore_session || self.chooser {
            return Ok(());
        }
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
        if session.tabs.is_empty() {
            return Ok(());
        }
        let path = session_file();
        std::fs::create_dir_all(path.parent().unwrap_or(&path))?;
        std::fs::write(&path, serde_json::to_vec_pretty(&session)?)
            .with_context(|| format!("saving the session to {}", path.display()))?;
        Ok(())
    }

    /// Reopens the saved tabs and panels. Returns false when there is no
    /// usable session.
    pub(super) fn restore_session(&mut self) -> bool {
        let Some(session) = std::fs::read(session_file()).ok().and_then(|b| serde_json::from_slice::<Session>(&b).ok())
        else {
            return false;
        };
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
                        self.make_panel(self.local.clone(), existing_dir(&p.cwd), sort, show_hidden)
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

/// `dir`, or its nearest ancestor that still exists.
fn existing_dir(dir: &Path) -> PathBuf {
    dir.ancestors().find(|d| d.is_dir()).map(Path::to_path_buf).unwrap_or_else(super::sidebar::root_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_an_existing_parent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(existing_dir(&dir.path().join("gone/deeper")), dir.path());
        assert_eq!(existing_dir(dir.path()), dir.path());
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
                    }],
                },
                SavedTab { active: 0, panels: vec![] },
            ],
        };
        let json = serde_json::to_vec(&session).unwrap();
        assert_eq!(serde_json::from_slice::<Session>(&json).unwrap(), session);
    }
}
