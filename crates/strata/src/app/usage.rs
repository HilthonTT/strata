//! The disk usage view: one scan measures everything below a folder, then
//! folders open and close instantly (like ncdu). Items can be trashed from
//! here, and a folder can be scanned again on its own.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use strata_config::Action;
use strata_plugin::Level;
use strata_sys::du::{UsageNode, UsageTree};

use super::overlay::{Confirm, ConfirmState, Overlay};
use super::{short_path, App, View};
use crate::event::AppEvent;

/// How the folder's items are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageSort {
    #[default]
    Size,
    Files,
    Name,
}

impl UsageSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Size => "size",
            Self::Files => "files",
            Self::Name => "name",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Size => Self::Files,
            Self::Files => Self::Name,
            Self::Name => Self::Size,
        }
    }
}

/// A scan in flight.
struct Scan {
    root: PathBuf,
    /// Where the result goes in the current tree (a folder scanned again);
    /// `None` replaces the tree.
    graft: Option<Vec<String>>,
    /// The item to put the cursor on once it arrives.
    focus: Option<String>,
    cancel: Arc<AtomicBool>,
    counted: Arc<AtomicU64>,
}

#[derive(Default)]
pub struct UsageView {
    pub tree: Option<UsageTree>,
    /// The folder shown: names from the tree's root down.
    pub trail: Vec<String>,
    pub cursor: usize,
    pub sort: UsageSort,
    /// When `tree` was scanned.
    pub scanned: Option<Instant>,
    scan: Option<Scan>,
}

impl UsageView {
    /// The folder shown.
    pub fn node(&self) -> Option<&UsageNode> {
        self.tree.as_ref()?.node.at(&self.trail)
    }

    /// Its path on disk.
    pub fn path(&self) -> Option<PathBuf> {
        let tree = self.tree.as_ref()?;
        Some(self.trail.iter().fold(tree.root.clone(), |p, name| p.join(name)))
    }

    /// The folder's items in the chosen order.
    pub fn listed(&self) -> Vec<&UsageNode> {
        let Some(node) = self.node() else { return Vec::new() };
        let mut items: Vec<&UsageNode> = node.children.iter().collect();
        match self.sort {
            UsageSort::Size => {}
            UsageSort::Files => items.sort_by_key(|n| std::cmp::Reverse(n.files)),
            UsageSort::Name => items.sort_by_key(|n| n.name.to_lowercase()),
        }
        items
    }

    pub fn hovered(&self) -> Option<&UsageNode> {
        self.listed().get(self.cursor).copied()
    }

    /// The folder being scanned and how many items were counted so far.
    pub fn scanning(&self) -> Option<(&Path, u64)> {
        self.scan.as_ref().map(|s| (s.root.as_path(), s.counted.load(Ordering::Relaxed)))
    }

    /// The scan replaces what is shown (rather than refreshing one folder).
    pub fn scanning_anew(&self) -> bool {
        self.scan.as_ref().is_some_and(|s| s.graft.is_none())
    }

    fn focus(&mut self, name: &str) {
        self.cursor = self.listed().iter().position(|n| n.name == name).unwrap_or(0);
    }

    fn clamp(&mut self) {
        self.cursor = self.cursor.min(self.listed().len().saturating_sub(1));
    }

    fn cancel_scan(&mut self) {
        if let Some(scan) = self.scan.take() {
            scan.cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl App {
    /// Opens the disk usage view on `dir` (the focused panel's folder if
    /// `None`), showing it from the last scan when that covers it.
    pub(super) fn open_usage(&mut self, dir: Option<PathBuf>) {
        let dir = match dir {
            Some(dir) => dir,
            None if self.panel().vfs.is_local() => self.panel().cwd.clone(),
            None if self.usage.tree.is_some() || self.usage.scan.is_some() => {
                self.view = View::Usage;
                return;
            }
            None => return self.notify("disk usage works on local folders", Level::Warn),
        };
        self.view = View::Usage;
        let inside = self.usage.tree.as_ref().and_then(|t| trail_to(&t.root, &dir));
        match inside {
            Some(trail) if self.usage.tree.as_ref().is_some_and(|t| t.node.at(&trail).is_some()) => {
                if trail != self.usage.trail {
                    self.usage.trail = trail;
                    self.usage.cursor = 0;
                }
            }
            _ if self.usage.scan.as_ref().is_some_and(|s| s.root == dir) => {}
            _ => self.scan_usage(dir, None, None),
        }
    }

    /// `:du [path]`.
    pub(super) fn du_command(&mut self, args: &str) {
        if args.is_empty() {
            return self.open_usage(None);
        }
        if !self.panel().vfs.is_local() && !args.starts_with(['/', '~']) {
            return self.notify("disk usage works on local folders", Level::Warn);
        }
        let dir = strata_core::util::expand_tilde(args);
        let dir = if dir.is_absolute() { dir } else { self.panel().cwd.join(dir) };
        if !dir.is_dir() {
            return self.error(format!("du: {} is not a folder", dir.display()));
        }
        self.open_usage(Some(dir));
    }

    fn scan_usage(&mut self, root: PathBuf, graft: Option<Vec<String>>, focus: Option<String>) {
        self.usage.cancel_scan();
        let cancel = Arc::new(AtomicBool::new(false));
        let counted = Arc::new(AtomicU64::new(0));
        let (tx, dir) = (self.tx.clone(), root.clone());
        let (stop, count) = (cancel.clone(), counted.clone());
        std::thread::spawn(move || {
            let tree = strata_sys::du::scan_tree(&dir, &stop, &count);
            if !stop.load(Ordering::Relaxed) {
                let _ = tx.send(AppEvent::UsageTree { root: dir, tree });
            }
        });
        self.usage.scan = Some(Scan { root, graft, focus, cancel, counted });
    }

    pub(super) fn on_usage_tree(&mut self, root: PathBuf, tree: Option<UsageTree>) {
        let Some(scan) = self.usage.scan.take_if(|s| s.root == root) else { return };
        let Some(tree) = tree else {
            return self.error(format!("cannot read {}", short_path(&root)));
        };
        let u = &mut self.usage;
        u.scanned = Some(Instant::now());
        // A folder scanned again goes into the tree, unless the tree changed.
        let grafted = match (&scan.graft, &mut u.tree) {
            (Some(trail), Some(current)) => current.node.replace(trail, tree.node.clone()),
            _ => false,
        };
        if !grafted {
            u.tree = Some(tree);
            u.trail.clear();
        }
        match scan.focus {
            Some(name) => u.focus(&name),
            None => u.clamp(),
        }
    }

    /// Actions the disk usage view handles itself. True when handled.
    pub(super) fn usage_action(&mut self, action: Action) -> bool {
        match action {
            Action::Open => self.usage_open(),
            Action::Parent => self.usage_up(),
            Action::Delete | Action::DeletePermanent => self.usage_delete(action == Action::DeletePermanent),
            Action::Refresh => self.usage_rescan(),
            _ => return false,
        }
        true
    }

    /// Single keys of the view: `r` scans the folder again, `s` changes the
    /// order, `o` shows the item in the files view.
    pub(super) fn usage_key(&mut self, c: char) -> bool {
        match c {
            'r' => self.usage_rescan(),
            's' => {
                let name = self.usage.hovered().map(|n| n.name.clone());
                self.usage.sort = self.usage.sort.next();
                if let Some(name) = name {
                    self.usage.focus(&name);
                }
                self.info(format!("sorted by {}", self.usage.sort.label()));
            }
            'o' => match (self.usage.path(), self.usage.hovered().map(|n| n.name.clone())) {
                (Some(dir), Some(name)) => self.reveal_local(dir.join(name)),
                (Some(dir), None) => self.cd_local(dir),
                _ => return false,
            },
            _ => return false,
        }
        true
    }

    /// Opens the hovered folder, or shows the hovered file in the files view.
    fn usage_open(&mut self) {
        let (Some(dir), Some(item)) = (self.usage.path(), self.usage.hovered()) else { return };
        if item.is_dir {
            let name = item.name.clone();
            self.usage.trail.push(name);
            self.usage.cursor = 0;
        } else {
            self.reveal_local(dir.join(&item.name));
        }
    }

    /// Shows a local file in the files view, leaving a remote filesystem.
    fn reveal_local(&mut self, path: PathBuf) {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return };
        let name = name.to_string_lossy().into_owned();
        self.cd_local(dir.to_path_buf());
        self.panel_mut().focus_name(&name);
    }

    /// Goes to the parent folder, scanning it when it is above the scan.
    fn usage_up(&mut self) {
        if let Some(name) = self.usage.trail.pop() {
            return self.usage.focus(&name);
        }
        let Some(root) = self.usage.tree.as_ref().map(|t| t.root.clone()) else {
            return self.set_view(View::Files);
        };
        let (Some(parent), Some(name)) = (root.parent(), root.file_name()) else { return };
        let name = name.to_string_lossy().into_owned();
        self.scan_usage(parent.to_path_buf(), None, Some(name));
    }

    fn usage_rescan(&mut self) {
        let Some(dir) = self.usage.path() else { return };
        let focus = self.usage.hovered().map(|n| n.name.clone());
        let graft = Some(self.usage.trail.clone()).filter(|t| !t.is_empty());
        self.scan_usage(dir, graft, focus);
    }

    fn usage_delete(&mut self, permanent: bool) {
        let (Some(dir), Some(item)) = (self.usage.path(), self.usage.hovered()) else { return };
        let path = dir.join(&item.name);
        let mut trail = self.usage.trail.clone();
        trail.push(item.name.clone());
        let permanent = permanent || !self.config.general.use_trash;
        if !self.config.general.confirm_delete {
            return self.delete_from_usage(path, trail, permanent);
        }
        let what = format!("'{}' ({})", item.name, strata_core::util::human_size(item.size));
        let message = if permanent {
            format!("Permanently delete {what}? This cannot be undone.")
        } else {
            format!("Move {what} to the trash?")
        };
        self.overlay =
            Some(Overlay::Confirm(ConfirmState { message, action: Confirm::UsageDelete { path, trail, permanent } }));
    }

    /// Deletes an item and takes it out of the tree (`r` brings it back if
    /// the deletion fails).
    pub(super) fn delete_from_usage(&mut self, path: PathBuf, trail: Vec<String>, permanent: bool) {
        self.start_delete(self.local.clone(), vec![path], permanent);
        if let Some(tree) = &mut self.usage.tree {
            tree.node.remove(&trail);
        }
        self.usage.clamp();
    }
}

/// The names leading from `root` down to `dir`, if `dir` is below it.
fn trail_to(root: &Path, dir: &Path) -> Option<Vec<String>> {
    let rel = dir.strip_prefix(root).ok()?;
    Some(rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, size: u64, files: u64, children: Vec<UsageNode>) -> UsageNode {
        UsageNode { name: name.into(), size, files, is_dir: !children.is_empty(), children }
    }

    #[test]
    fn folders_list_in_the_chosen_order() {
        let mut u = UsageView::default();
        let docs = node("docs", 10, 1, vec![node("a", 10, 1, vec![])]);
        let root = node("r", 120, 9, vec![node("Big", 100, 2, vec![]), node("many", 10, 6, vec![]), docs]);
        u.tree = Some(UsageTree { root: PathBuf::from("/r"), node: root });
        let names = |u: &UsageView| u.listed().iter().map(|n| n.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&u), ["Big", "many", "docs"]);
        u.sort = UsageSort::Files;
        assert_eq!(names(&u), ["many", "Big", "docs"]);
        u.sort = UsageSort::Name;
        assert_eq!(names(&u), ["Big", "docs", "many"]);
        u.focus("many");
        assert_eq!(u.cursor, 2);

        u.trail = vec!["docs".into()];
        assert_eq!(u.path(), Some(PathBuf::from("/r/docs")));
        assert_eq!(names(&u), ["a"]);
        assert_eq!(trail_to(Path::new("/r"), Path::new("/r/docs/a")), Some(vec!["docs".into(), "a".into()]));
        assert_eq!(trail_to(Path::new("/r"), Path::new("/other")), None);
    }
}
