//! Git status per file, refreshed in the background for every local panel.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use strata_core::git::{self, GitState, GitStatus};

use super::App;
use crate::event::AppEvent;

const MAX_AGE: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct GitCache {
    /// Status by panel directory; `None` when it is not in a repository.
    by_dir: HashMap<PathBuf, (Option<Arc<GitStatus>>, Instant)>,
    in_flight: HashSet<PathBuf>,
}

impl App {
    /// Starts a `git status` for panel directories with stale or no data.
    pub(super) fn update_git(&mut self) {
        if !self.config.general.git_status {
            return;
        }
        let dirs: Vec<PathBuf> = self.panels.iter().filter(|p| p.vfs.is_local()).map(|p| p.cwd.clone()).collect();
        for dir in dirs {
            let fresh = self.git.by_dir.get(&dir).is_some_and(|(_, at)| at.elapsed() < MAX_AGE);
            if fresh || !self.git.in_flight.insert(dir.clone()) {
                continue;
            }
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let status = git::status(&dir);
                let _ = tx.send(AppEvent::Git { dir, status });
            });
        }
    }

    pub(super) fn on_git_status(&mut self, dir: PathBuf, status: Option<GitStatus>) {
        self.git.in_flight.remove(&dir);
        self.git.by_dir.insert(dir, (status.map(Arc::new), Instant::now()));
    }

    /// Forces a refresh, e.g. after files changed.
    pub(super) fn invalidate_git(&mut self) {
        for (_, at) in self.git.by_dir.values_mut() {
            *at = Instant::now() - MAX_AGE;
        }
    }

    /// Status of `path` inside a panel showing `dir`.
    pub fn git_state(&self, dir: &Path, path: &Path) -> Option<GitState> {
        self.git.by_dir.get(dir)?.0.as_ref()?.get(path)
    }
}
