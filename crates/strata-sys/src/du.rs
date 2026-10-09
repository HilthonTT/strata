//! Disk usage of a directory's children, computed in parallel.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

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
    }
}
