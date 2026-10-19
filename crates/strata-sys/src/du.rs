//! Disk usage of a directory's children, computed in parallel, either one
//! level deep ([`scan`]) or as a whole tree to browse ([`scan_tree`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Debug, Clone)]
pub struct UsageItem {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub is_dir: bool,
    pub files: u64,
}

#[derive(Debug, Clone, Default)]
pub struct UsageReport {
    pub root: PathBuf,
    pub total: u64,
    pub files: u64,
    /// Children sorted by size, largest first.
    pub items: Vec<UsageItem>,
}

/// Sizes every direct child of `root`, recursing into directories without
/// crossing filesystem boundaries. Returns `None` if cancelled.
pub fn scan(root: &Path, cancel: &AtomicBool) -> Option<UsageReport> {
    let children: Vec<PathBuf> = std::fs::read_dir(root).ok()?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
    let chunk = children.len().div_ceil(threads).max(1);

    let mut items: Vec<UsageItem> = std::thread::scope(|s| {
        let handles: Vec<_> = children
            .chunks(chunk)
            .map(|paths| s.spawn(move || paths.iter().filter_map(|p| measure(p, cancel)).collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    items.sort_by_key(|i| std::cmp::Reverse(i.size));
    Some(UsageReport {
        root: root.to_path_buf(),
        total: items.iter().map(|i| i.size).sum(),
        files: items.iter().map(|i| i.files).sum(),
        items,
    })
}

/// A file or folder of a [`UsageTree`], with everything below it.
#[derive(Debug, Clone, Default)]
pub struct UsageNode {
    pub name: String,
    pub size: u64,
    pub files: u64,
    pub is_dir: bool,
    /// Largest first; empty for files.
    pub children: Vec<UsageNode>,
}

/// Every file and folder below `root`, measured once so it can be browsed
/// without scanning again.
#[derive(Debug, Clone)]
pub struct UsageTree {
    pub root: PathBuf,
    pub node: UsageNode,
}

impl UsageNode {
    pub fn child(&self, name: &str) -> Option<&UsageNode> {
        self.children.iter().find(|c| c.name == name)
    }

    /// The folder `trail` (names from here down), if it is in the tree.
    pub fn at(&self, trail: &[String]) -> Option<&UsageNode> {
        trail.iter().try_fold(self, |node, name| node.child(name))
    }

    /// Takes out the item at `trail`, taking its size and files off every
    /// folder above it. Returns it, or `None` if it is not in the tree.
    pub fn remove(&mut self, trail: &[String]) -> Option<UsageNode> {
        let (name, rest) = trail.split_first()?;
        let i = self.children.iter().position(|c| &c.name == name)?;
        let removed = if rest.is_empty() { self.children.remove(i) } else { self.children[i].remove(rest)? };
        self.size -= removed.size;
        self.files -= removed.files;
        Some(removed)
    }

    /// Puts `node` in place of the item at `trail` (the same item scanned
    /// again), updating the folders above it. False if it is not in the tree.
    pub fn replace(&mut self, trail: &[String], node: UsageNode) -> bool {
        let Some((name, rest)) = trail.split_first() else {
            *self = node;
            return true;
        };
        let Some(i) = self.children.iter().position(|c| &c.name == name) else { return false };
        let (old_size, old_files) = (self.children[i].size, self.children[i].files);
        if !self.children[i].replace(rest, node) {
            return false;
        }
        self.size = self.size - old_size + self.children[i].size;
        self.files = self.files - old_files + self.children[i].files;
        self.children.sort_by_key(|c| std::cmp::Reverse(c.size));
        true
    }
}

/// Measures everything below `root` into a tree, without crossing
/// filesystem boundaries or following links; the top-level items are
/// measured in parallel. `counted` goes up by one per file and folder seen,
/// for progress. Returns `None` if cancelled or `root` cannot be read.
pub fn scan_tree(root: &Path, cancel: &AtomicBool, counted: &AtomicU64) -> Option<UsageTree> {
    let device = device_of(root);
    let children: Vec<PathBuf> = std::fs::read_dir(root).ok()?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
    let chunk = children.len().div_ceil(threads).max(1);
    let children: Vec<UsageNode> = std::thread::scope(|s| {
        let handles: Vec<_> = children
            .chunks(chunk)
            .map(|paths| {
                s.spawn(move || paths.iter().filter_map(|p| build(p, device, cancel, counted)).collect::<Vec<_>>())
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
    Some(UsageTree { root: root.to_path_buf(), node: folder(name, children) })
}

fn build(path: &Path, device: Option<u64>, cancel: &AtomicBool, counted: &AtomicU64) -> Option<UsageNode> {
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let meta = std::fs::symlink_metadata(path).ok()?;
    counted.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name()?.to_string_lossy().into_owned();
    if !meta.is_dir() {
        // Links count as nothing, like in `measure`.
        let size = if meta.is_file() { meta.len() } else { 0 };
        return Some(UsageNode { name, size, files: meta.is_file() as u64, is_dir: false, children: Vec::new() });
    }
    let mut children = Vec::new();
    // Another filesystem mounted here is listed, but not entered.
    if device.is_none() || device_of(path) == device {
        if let Ok(rd) = std::fs::read_dir(path) {
            for entry in rd.filter_map(Result::ok) {
                children.extend(build(&entry.path(), device, cancel, counted));
            }
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    Some(folder(name, children))
}

fn folder(name: String, mut children: Vec<UsageNode>) -> UsageNode {
    children.sort_by_key(|c| std::cmp::Reverse(c.size));
    UsageNode {
        name,
        size: children.iter().map(|c| c.size).sum(),
        files: children.iter().map(|c| c.files).sum(),
        is_dir: true,
        children,
    }
}

/// Total size of the files below `dir`, without crossing filesystem
/// boundaries or following links. Returns `None` if cancelled.
pub fn dir_size(dir: &Path, cancel: &AtomicBool) -> Option<u64> {
    measure(dir, cancel).map(|i| i.size)
}

fn measure(path: &Path, cancel: &AtomicBool) -> Option<UsageItem> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    if !meta.is_dir() {
        return Some(UsageItem { name, path: path.to_path_buf(), size: meta.len(), is_dir: false, files: 1 });
    }
    let (mut size, mut files) = (0, 0);
    for entry in walkdir(path) {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        if let Ok(m) = entry.metadata() {
            if m.is_file() {
                size += m.len();
                files += 1;
            }
        }
    }
    Some(UsageItem { name, path: path.to_path_buf(), size, is_dir: true, files })
}

/// Minimal recursive walker that stays on one filesystem and never follows links.
fn walkdir(root: &Path) -> impl Iterator<Item = std::fs::DirEntry> {
    let device = device_of(root);
    let mut stack = vec![root.to_path_buf()];
    let mut pending: Vec<std::fs::DirEntry> = Vec::new();
    std::iter::from_fn(move || loop {
        if let Some(e) = pending.pop() {
            if let Ok(ft) = e.file_type() {
                if ft.is_dir() && device_of(&e.path()) == device {
                    stack.push(e.path());
                }
            }
            return Some(e);
        }
        let dir = stack.pop()?;
        if let Ok(rd) = std::fs::read_dir(&dir) {
            pending.extend(rd.filter_map(Result::ok));
        }
    })
}

#[cfg(unix)]
fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).ok().map(|m| m.dev())
}

#[cfg(not(unix))]
fn device_of(_: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_children() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("big/inner")).unwrap();
        std::fs::write(dir.path().join("big/inner/a"), vec![0u8; 3000]).unwrap();
        std::fs::write(dir.path().join("big/b"), vec![0u8; 1000]).unwrap();
        std::fs::write(dir.path().join("small"), vec![0u8; 10]).unwrap();
        let r = scan(dir.path(), &AtomicBool::new(false)).unwrap();
        assert_eq!(r.total, 4010);
        assert_eq!(r.items[0].name, "big");
        assert_eq!(r.items[0].files, 2);
        assert_eq!(dir_size(&dir.path().join("big"), &AtomicBool::new(false)), Some(4000));
        assert_eq!(dir_size(&dir.path().join("big"), &AtomicBool::new(true)), None);
    }

    #[test]
    fn trees_are_browsable_and_stay_consistent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("big/inner")).unwrap();
        std::fs::write(dir.path().join("big/inner/a"), vec![0u8; 3000]).unwrap();
        std::fs::write(dir.path().join("big/b"), vec![0u8; 1000]).unwrap();
        std::fs::write(dir.path().join("small"), vec![0u8; 10]).unwrap();
        let counted = AtomicU64::new(0);
        let mut tree = scan_tree(dir.path(), &AtomicBool::new(false), &counted).unwrap();
        assert_eq!(counted.load(Ordering::Relaxed), 5);
        let root = &mut tree.node;
        assert_eq!((root.size, root.files), (4010, 3));
        let trail = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let inner = root.at(&trail(&["big", "inner"])).unwrap();
        assert_eq!((inner.size, inner.children[0].name.as_str()), (3000, "a"));
        assert_eq!(root.children[0].name, "big", "largest first");

        let removed = root.remove(&trail(&["big", "inner", "a"])).unwrap();
        assert_eq!(removed.size, 3000);
        assert_eq!((root.size, root.files), (1010, 2), "sizes above go down");
        assert_eq!(root.at(&trail(&["big"])).unwrap().size, 1000);
        assert!(root.remove(&trail(&["missing"])).is_none());

        let rescanned = UsageNode { name: "big".into(), size: 5, files: 1, is_dir: true, children: Vec::new() };
        assert!(root.replace(&trail(&["big"]), rescanned));
        assert_eq!((root.size, root.files), (15, 2));
        assert_eq!(root.children[0].name, "small", "resorted after the rescan");

        assert!(scan_tree(dir.path(), &AtomicBool::new(true), &counted).is_none());
    }
}
