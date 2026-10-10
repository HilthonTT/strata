//! Folder sizes in the file list (`dir_sizes`): measured in the background,
//! one folder at a time, and cached until something changes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::App;
use crate::event::AppEvent;

#[derive(Default)]
pub struct DirSizes {
    /// Measured folders.
    pub sizes: HashMap<PathBuf, u64>,
    /// Bumped whenever `sizes` changes, so panels know to pick them up.
    version: u64,
    /// Folders whose children are being measured, with their cancel flags.
    scanning: HashMap<PathBuf, Arc<AtomicBool>>,
    /// Folders whose children have all been measured.
    done: HashSet<PathBuf>,
}

impl DirSizes {
    /// Forgets every size, e.g. after a file operation.
    pub fn clear(&mut self) {
        self.sizes.clear();
        self.done.clear();
        self.version += 1;
    }

    /// Stops every measurement and forgets every size.
    pub fn stop(&mut self) {
        for cancel in self.scanning.values() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.scanning.clear();
        self.clear();
    }

    /// Forgets the sizes of the folders containing `path`.
    pub fn invalidate(&mut self, path: &Path) {
        for dir in path.ancestors() {
            if self.sizes.remove(dir).is_some() {
                self.version += 1;
                if let Some(parent) = dir.parent() {
                    self.done.remove(parent);
                }
            }
        }
    }
}

impl App {
    /// Measures the folders shown in local panels and puts known sizes into
    /// their listings. Runs every tick; cheap when nothing changed.
    pub(super) fn update_dir_sizes(&mut self) {
        if !self.config.general.dir_sizes {
            return;
        }
        let shown: HashSet<PathBuf> = self.panels.iter().filter(|p| p.vfs.is_local()).map(|p| p.cwd.clone()).collect();
        self.dir_sizes.scanning.retain(|root, cancel| {
            let keep = shown.contains(root);
            if !keep {
                cancel.store(true, Ordering::Relaxed);
            }
            keep
        });

        let ds = &mut self.dir_sizes;
        for p in self.panels.iter_mut().filter(|p| p.vfs.is_local()) {
            if p.sizes_seen != (p.listed, ds.version) {
                p.sizes_seen = (p.listed, ds.version);
                p.apply_dir_sizes(&ds.sizes);
            }
            if ds.done.contains(&p.cwd) || ds.scanning.contains_key(&p.cwd) {
                continue;
            }
            let missing: Vec<PathBuf> =
                p.dirs().filter(|d| !ds.sizes.contains_key(*d)).map(Path::to_path_buf).collect();
            if missing.is_empty() {
                ds.done.insert(p.cwd.clone());
                continue;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            ds.scanning.insert(p.cwd.clone(), cancel.clone());
            spawn_measure(self.tx.clone(), p.cwd.clone(), missing, cancel);
        }
    }

    pub(super) fn on_dir_size(&mut self, path: PathBuf, size: u64) {
        if !self.config.general.dir_sizes {
            return;
        }
        if self.dir_sizes.sizes.insert(path, size) != Some(size) {
            self.dir_sizes.version += 1;
        }
    }

    pub(super) fn on_dir_sizes_done(&mut self, root: PathBuf) {
        if self.dir_sizes.scanning.remove(&root).is_none() {
            return; // stopped meanwhile
        }
        self.dir_sizes.done.insert(root);
    }

    /// The measured size of a folder, when `dir_sizes` is on and it is known.
    pub fn dir_size(&self, path: &Path) -> Option<u64> {
        self.dir_sizes.sizes.get(path).copied()
    }
}

/// Measures `dirs` one by one, reporting each as it finishes.
fn spawn_measure(tx: std::sync::mpsc::Sender<AppEvent>, root: PathBuf, dirs: Vec<PathBuf>, cancel: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        for path in dirs {
            let Some(size) = strata_sys::du::dir_size(&path, &cancel) else {
                return;
            };
            if tx.send(AppEvent::DirSize { path, size }).is_err() {
                return;
            }
        }
        let _ = tx.send(AppEvent::DirSizesDone { root });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidates_containing_folders() {
        let mut ds = DirSizes::default();
        ds.sizes.insert(PathBuf::from("/a"), 10);
        ds.sizes.insert(PathBuf::from("/a/b"), 5);
        ds.sizes.insert(PathBuf::from("/c"), 1);
        ds.done.insert(PathBuf::from("/"));
        ds.done.insert(PathBuf::from("/a"));
        ds.invalidate(Path::new("/a/b/file"));
        assert_eq!(ds.sizes.len(), 1);
        assert!(ds.done.is_empty(), "both parents measure again");
    }
}
