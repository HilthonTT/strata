//! Finding duplicate files below a local directory: files of equal size
//! are compared by their first bytes, then by a SHA-256 of everything.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::SystemTime;

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::jobs::Progress;

/// Bytes hashed first, to split same-size files cheaply.
const HEAD_BYTES: u64 = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupeFile {
    pub path: PathBuf,
    pub modified: Option<SystemTime>,
}

/// Files with identical contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupeGroup {
    pub size: u64,
    /// Sorted by path.
    pub files: Vec<DupeFile>,
}

impl DupeGroup {
    /// Bytes freed by keeping only one copy.
    pub fn wasted(&self) -> u64 {
        self.size * (self.files.len() as u64 - 1)
    }
}

#[derive(Debug, Default)]
pub struct DupeReport {
    /// Most wasted space first.
    pub groups: Vec<DupeGroup>,
    pub scanned: usize,
    /// Files that could not be read.
    pub unreadable: usize,
}

impl DupeReport {
    pub fn wasted(&self) -> u64 {
        self.groups.iter().map(DupeGroup::wasted).sum()
    }
}

/// Which copy of each group to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    Oldest,
    Newest,
    /// The shortest path: usually the original, not the `copy (2)`.
    Shortest,
}

impl Keep {
    pub fn parse(s: &str) -> Option<Keep> {
        match s.trim() {
            "oldest" => Some(Keep::Oldest),
            "newest" => Some(Keep::Newest),
            "shortest" => Some(Keep::Shortest),
            _ => None,
        }
    }

    /// Every file but the one kept in each group.
    pub fn others(self, groups: &[DupeGroup]) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for group in groups {
            let path_len = |f: &DupeFile| f.path.as_os_str().len();
            let keep = match self {
                // Unknown times count as newest; ties go to the first path.
                Keep::Oldest => group.files.iter().min_by_key(|f| (f.modified.is_none(), f.modified)),
                Keep::Newest => group.files.iter().rev().max_by_key(|f| (f.modified.is_some(), f.modified)),
                Keep::Shortest => group.files.iter().min_by_key(|f| path_len(f)),
            };
            let Some(keep) = keep else { continue };
            out.extend(group.files.iter().filter(|f| f.path != keep.path).map(|f| f.path.clone()));
        }
        out
    }
}

/// Finds groups of identical files below `root`. Symbolic links, empty
/// files and extra hard links to the same file are left out.
pub fn find(root: &Path, show_hidden: bool, progress: &Progress) -> Result<DupeReport> {
    let mut report = DupeReport::default();
    let mut by_size: HashMap<u64, Vec<DupeFile>> = HashMap::new();
    let mut inodes = std::collections::HashSet::new();
    progress.set_current("scanning…");
    let walk = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || show_hidden || !e.file_name().to_string_lossy().starts_with('.'));
    for entry in walk {
        progress.check()?;
        let Ok(entry) = entry else {
            report.unreadable += 1;
            continue;
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            report.unreadable += 1;
            continue;
        };
        report.scanned += 1;
        if report.scanned % 1000 == 0 {
            progress.set_current(format!("scanning… {} files", report.scanned));
        }
        if meta.len() == 0 || !inodes.insert(inode(&meta, entry.path())) {
            continue;
        }
        let file = DupeFile { path: entry.into_path(), modified: meta.modified().ok() };
        by_size.entry(meta.len()).or_default().push(file);
    }
    by_size.retain(|_, files| files.len() > 1);

    let mut candidates: Vec<(u64, Vec<DupeFile>)> = Vec::new();
    for (size, files) in by_size {
        candidates.extend(split_by(files, size, |path| digest(path, HEAD_BYTES, progress), &mut report)?);
    }
    let full: Vec<_> = candidates.iter().filter(|(size, _)| *size > HEAD_BYTES).collect();
    let total = full.iter().map(|(size, files)| size * files.len() as u64).sum();
    progress.total_bytes.store(total, Ordering::Relaxed);
    let mut groups = Vec::new();
    for (size, files) in candidates {
        if size <= HEAD_BYTES {
            groups.push((size, files));
            continue;
        }
        groups.extend(split_by(files, size, |path| digest(path, u64::MAX, progress), &mut report)?);
    }

    report.groups = groups
        .into_iter()
        .map(|(size, mut files)| {
            files.sort_by(|a, b| a.path.cmp(&b.path));
            DupeGroup { size, files }
        })
        .collect();
    report.groups.sort_by(|a, b| b.wasted().cmp(&a.wasted()).then_with(|| a.files[0].path.cmp(&b.files[0].path)));
    Ok(report)
}

/// Splits same-size `files` by `key`, keeping groups of two or more.
fn split_by(
    files: Vec<DupeFile>,
    size: u64,
    mut key: impl FnMut(&Path) -> Result<Option<[u8; 32]>>,
    report: &mut DupeReport,
) -> Result<Vec<(u64, Vec<DupeFile>)>> {
    let mut groups: HashMap<[u8; 32], Vec<DupeFile>> = HashMap::new();
    for file in files {
        match key(&file.path)? {
            Some(k) => groups.entry(k).or_default().push(file),
            None => report.unreadable += 1,
        }
    }
    Ok(groups.into_values().filter(|g| g.len() > 1).map(|g| (size, g)).collect())
}

/// SHA-256 of the first `limit` bytes; `None` if the file is unreadable.
/// Only cancelling is an error.
fn digest(path: &Path, limit: u64, progress: &Progress) -> Result<Option<[u8; 32]>> {
    progress.check()?;
    let Ok(file) = File::open(path) else { return Ok(None) };
    let full = limit == u64::MAX;
    if full {
        progress.set_current(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    }
    let mut reader = file.take(limit);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        progress.check()?;
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => return Ok(None),
        };
        hasher.update(&buf[..n]);
        if full {
            progress.add_bytes(n as u64);
        }
    }
    Ok(Some(hasher.finalize().into()))
}

#[cfg(unix)]
fn inode(meta: &std::fs::Metadata, _path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

/// Without inode numbers every path counts as its own file.
#[cfg(not(unix))]
fn inode(_meta: &std::fs::Metadata, path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    #[test]
    fn groups_identical_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("sub")).unwrap();
        let big: Vec<u8> = (0..20_000u32).map(|i| i as u8).collect();
        let mut other = big.clone();
        *other.last_mut().unwrap() ^= 1;
        fs::write(root.join("a.bin"), &big).unwrap();
        fs::write(root.join("sub/b.bin"), &big).unwrap();
        fs::write(root.join("c.bin"), &other).unwrap(); // same size and head, different tail
        fs::write(root.join("x.txt"), "hello").unwrap();
        fs::write(root.join("sub/y.txt"), "hello").unwrap();
        fs::write(root.join("e1"), "").unwrap();
        fs::write(root.join("e2"), "").unwrap();
        fs::write(root.join(".hidden.txt"), "hello").unwrap();
        #[cfg(unix)]
        {
            fs::hard_link(root.join("a.bin"), root.join("a-link.bin")).unwrap();
            std::os::unix::fs::symlink(root.join("x.txt"), root.join("x-link.txt")).unwrap();
        }

        let report = find(root, false, &Progress::default()).unwrap();
        assert_eq!(report.groups.len(), 2);
        let first = &report.groups[0];
        assert_eq!(first.size, 20_000);
        assert_eq!(first.files.len(), 2, "hard links count once, c.bin differs: {first:?}");
        assert_eq!(first.files[1].path, root.join("sub/b.bin"));
        let paths: Vec<_> = report.groups[1].files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(paths, [root.join("sub/y.txt"), root.join("x.txt")]);
        assert_eq!(report.wasted(), 20_005);

        let with_hidden = find(root, true, &Progress::default()).unwrap();
        assert_eq!(with_hidden.groups[1].files.len(), 3);
    }

    #[test]
    fn keeps_one_file_per_group() {
        let now = SystemTime::now();
        let file =
            |p: &str, age: u64| DupeFile { path: PathBuf::from(p), modified: Some(now - Duration::from_secs(age)) };
        let groups =
            [DupeGroup { size: 1, files: vec![file("/a/copy (1).txt", 5), file("/a/x.txt", 10), file("/b/x.txt", 1)] }];
        assert_eq!(Keep::Oldest.others(&groups), [PathBuf::from("/a/copy (1).txt"), "/b/x.txt".into()]);
        assert_eq!(Keep::Newest.others(&groups), [PathBuf::from("/a/copy (1).txt"), "/a/x.txt".into()]);
        assert_eq!(Keep::Shortest.others(&groups), [PathBuf::from("/a/copy (1).txt"), "/b/x.txt".into()]);
        assert_eq!(Keep::parse("newest"), Some(Keep::Newest));
        assert_eq!(Keep::parse("biggest"), None);
    }
}
