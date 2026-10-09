//! Live refresh: panels showing a local folder reload when something else
//! changes it. Bursts of changes are coalesced so a large copy or a
//! `git checkout` triggers one reload, not thousands.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use strata_plugin::Level;

use super::App;
use crate::event::AppEvent;

/// Quiet time before a changed folder is reloaded.
const SETTLE: Duration = Duration::from_millis(150);
/// Reload anyway during a long burst of changes.
const MAX_WAIT: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct FsWatch {
    watcher: Option<RecommendedWatcher>,
    watched: HashSet<PathBuf>,
    /// Changed folder → (first change, last change, changed paths).
    dirty: HashMap<PathBuf, (Instant, Instant, HashSet<PathBuf>)>,
}

impl App {
    pub(super) fn start_watcher(&mut self) {
        let tx = self.tx.clone();
        let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            if let Ok(event) = result {
                if !event.kind.is_access() {
                    let _ = tx.send(AppEvent::FsChanged(event.paths));
                }
            }
        });
        match watcher {
            Ok(w) => self.watch.watcher = Some(w),
            Err(e) => self.notify(format!("live refresh is off: {e}"), Level::Warn),
        }
    }

    /// Watches exactly the local folders shown in the current tab.
    pub(super) fn sync_watches(&mut self) {
        let Some(watcher) = self.watch.watcher.as_mut() else { return };
        let wanted: HashSet<PathBuf> = self.panels.iter().filter(|p| p.vfs.is_local()).map(|p| p.cwd.clone()).collect();
        if wanted == self.watch.watched {
            return;
        }
        for gone in self.watch.watched.difference(&wanted) {
            let _ = watcher.unwatch(gone);
        }
        let mut watched = HashSet::new();
        for dir in wanted {
            let ok = self.watch.watched.contains(&dir) || watcher.watch(&dir, RecursiveMode::NonRecursive).is_ok();
            if ok {
                watched.insert(dir);
            }
        }
        self.watch.watched = watched;
    }

    pub(super) fn on_fs_changed(&mut self, paths: Vec<PathBuf>) {
        let now = Instant::now();
        for path in paths {
            // An event names the changed entry; its folder is what to reload.
            let dirs = [Some(path.clone()), path.parent().map(PathBuf::from)];
            for dir in dirs.into_iter().flatten().filter(|d| self.watch.watched.contains(d)) {
                let slot = self.watch.dirty.entry(dir).or_insert_with(|| (now, now, HashSet::new()));
                slot.1 = now;
                slot.2.insert(path.clone());
            }
        }
    }

    /// Reloads folders whose changes have settled.
    pub(super) fn flush_fs_changes(&mut self) {
        let now = Instant::now();
        let ready: Vec<PathBuf> = self
            .watch
            .dirty
            .iter()
            .filter(|(_, (first, last, _))| {
                now.duration_since(*last) >= SETTLE || now.duration_since(*first) >= MAX_WAIT
            })
            .map(|(dir, _)| dir.clone())
            .collect();
        if ready.is_empty() {
            return;
        }
        let hovered = self.panel().hovered().map(|e| e.path.clone());
        for dir in ready {
            let Some((_, _, changed)) = self.watch.dirty.remove(&dir) else { continue };
            for panel in self.panels.iter_mut().filter(|p| p.vfs.is_local() && p.cwd == dir) {
                panel.reload();
            }
            if hovered.as_ref().is_some_and(|h| changed.contains(h)) {
                self.invalidate_preview();
            }
        }
        self.invalidate_git();
    }
}
