//! Comparing two files (a unified diff) or two directory trees, on any
//! pair of backends.

use std::io::Read;
use std::path::Path;

use anyhow::Result;

use crate::jobs::Progress;
use crate::util::human_size;
use crate::{Entry, EntryKind, Vfs};

/// Files larger than this are compared byte for byte, not diffed.
pub const MAX_DIFF_BYTES: u64 = 4 * 1024 * 1024;

/// Result of comparing two files.
#[derive(Debug, PartialEq, Eq)]
pub enum FileDiff {
    Identical,
    /// Binary or too large to diff: only whether the bytes differ.
    Differ {
        reason: String,
    },
    /// Unified diff lines (`---`, `+++`, `@@`, ` `, `-`, `+`).
    Text(Vec<String>),
}

/// Diffs two files. `labels` name them in the diff header.
pub fn diff_files(
    a: (&dyn Vfs, &Path),
    b: (&dyn Vfs, &Path),
    labels: (&str, &str),
    progress: &Progress,
) -> Result<FileDiff> {
    let (ea, eb) = (a.0.stat(a.1)?, b.0.stat(b.1)?);
    if ea.size > MAX_DIFF_BYTES || eb.size > MAX_DIFF_BYTES {
        return Ok(if files_equal(a, b, progress)? {
            FileDiff::Identical
        } else {
            FileDiff::Differ { reason: format!("files differ ({} vs {})", human_size(ea.size), human_size(eb.size)) }
        });
    }
    let (left, right) = (read_all(a.0, a.1)?, read_all(b.0, b.1)?);
    if left == right {
        return Ok(FileDiff::Identical);
    }
    let binary = |data: &[u8]| data.iter().take(8192).any(|&b| b == 0);
    let (Ok(left), Ok(right)) = (std::str::from_utf8(&left), std::str::from_utf8(&right)) else {
        return Ok(FileDiff::Differ { reason: "binary files differ".into() });
    };
    if binary(left.as_bytes()) || binary(right.as_bytes()) {
        return Ok(FileDiff::Differ { reason: "binary files differ".into() });
    }
    let diff = similar::TextDiff::from_lines(left, right);
    let text = diff.unified_diff().context_radius(3).header(labels.0, labels.1).to_string();
    Ok(FileDiff::Text(text.lines().map(str::to_string).collect()))
}

fn read_all(vfs: &dyn Vfs, path: &Path) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    vfs.reader(path)?.take(MAX_DIFF_BYTES + 1).read_to_end(&mut data)?;
    Ok(data)
}

/// True when both files have the same bytes.
pub fn files_equal(a: (&dyn Vfs, &Path), b: (&dyn Vfs, &Path), progress: &Progress) -> Result<bool> {
    if a.0.stat(a.1)?.size != b.0.stat(b.1)?.size {
        return Ok(false);
    }
    let (mut ra, mut rb) = (a.0.reader(a.1)?, b.0.reader(b.1)?);
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        progress.check()?;
        let n = read_full(&mut *ra, &mut ba)?;
        let m = read_full(&mut *rb, &mut bb)?;
        if n != m || ba[..n] != bb[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
        progress.add_bytes(n as u64);
    }
}

/// Fills `buf` unless the reader ends first.
fn read_full(reader: &mut dyn Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Differences between two directory trees, as paths relative to them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DirReport {
    pub only_left: Vec<String>,
    pub only_right: Vec<String>,
    /// Present on both sides with different contents or types.
    pub differ: Vec<(String, String)>,
    /// Files that are identical on both sides.
    pub same: usize,
}

impl DirReport {
    pub fn is_identical(&self) -> bool {
        self.only_left.is_empty() && self.only_right.is_empty() && self.differ.is_empty()
    }
}

/// Compares two directory trees recursively. Files of equal size are
/// compared byte for byte.
pub fn compare_dirs(a: (&dyn Vfs, &Path), b: (&dyn Vfs, &Path), progress: &Progress) -> Result<DirReport> {
    let mut report = DirReport::default();
    walk(a, b, "", &mut report, progress)?;
    Ok(report)
}

fn walk(
    a: (&dyn Vfs, &Path),
    b: (&dyn Vfs, &Path),
    rel: &str,
    report: &mut DirReport,
    progress: &Progress,
) -> Result<()> {
    progress.check()?;
    let sorted = |vfs: &dyn Vfs, dir: &Path| -> Result<Vec<Entry>> {
        let mut entries = vfs.read_dir(dir)?;
        entries.sort_by(|x, y| x.name.cmp(&y.name));
        Ok(entries)
    };
    let (left, right) = (sorted(a.0, a.1)?, sorted(b.0, b.1)?);
    let name_of = |rel: &str, name: &str| if rel.is_empty() { name.to_string() } else { format!("{rel}/{name}") };
    let (mut i, mut j) = (0, 0);
    while i < left.len() || j < right.len() {
        let order = match (left.get(i), right.get(j)) {
            (Some(l), Some(r)) => l.name.cmp(&r.name),
            (Some(_), None) => std::cmp::Ordering::Less,
            _ => std::cmp::Ordering::Greater,
        };
        match order {
            std::cmp::Ordering::Less => {
                report.only_left.push(name_of(rel, &left[i].name));
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                report.only_right.push(name_of(rel, &right[j].name));
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                let (l, r) = (&left[i], &right[j]);
                let name = name_of(rel, &l.name);
                progress.set_current(name.clone());
                match (kind_of(l), kind_of(r)) {
                    (Kind::Dir, Kind::Dir) => walk((a.0, &l.path), (b.0, &r.path), &name, report, progress)?,
                    (Kind::File, Kind::File) => {
                        if l.size != r.size {
                            let why = format!("{} vs {}", human_size(l.size), human_size(r.size));
                            report.differ.push((name, why));
                        } else if files_equal((a.0, &l.path), (b.0, &r.path), progress)? {
                            report.same += 1;
                        } else {
                            report.differ.push((name, "contents differ".into()));
                        }
                    }
                    // Links to directories and special files: names only.
                    (Kind::Other, Kind::Other) => report.same += 1,
                    (x, y) => report.differ.push((name, format!("{} vs {}", x.label(), y.label()))),
                }
                i += 1;
                j += 1;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Other,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Dir => "directory",
            Kind::File => "file",
            Kind::Other => "special file",
        }
    }
}

fn kind_of(entry: &Entry) -> Kind {
    match entry.kind {
        EntryKind::Dir => Kind::Dir,
        EntryKind::File | EntryKind::Symlink { to_dir: false } => Kind::File,
        EntryKind::Symlink { to_dir: true } | EntryKind::Other => Kind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::LocalVfs;
    use std::fs;

    #[test]
    fn diffs_text_files() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "one\ntwo\nthree\n").unwrap();
        fs::write(&b, "one\n2\nthree\n").unwrap();
        let p = Progress::default();
        let FileDiff::Text(lines) = diff_files((&LocalVfs, &a), (&LocalVfs, &b), ("a", "b"), &p).unwrap() else {
            panic!("expected a text diff");
        };
        assert_eq!(lines[0], "--- a");
        assert!(lines.contains(&"-two".to_string()) && lines.contains(&"+2".to_string()));
        assert_eq!(diff_files((&LocalVfs, &a), (&LocalVfs, &a), ("a", "a"), &p).unwrap(), FileDiff::Identical);
    }

    #[test]
    fn binary_files_only_report_a_difference() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, [0u8, 1, 2]).unwrap();
        fs::write(&b, [0u8, 1, 3]).unwrap();
        let diff = diff_files((&LocalVfs, &a), (&LocalVfs, &b), ("a", "b"), &Progress::default()).unwrap();
        assert!(matches!(diff, FileDiff::Differ { .. }));
    }

    #[test]
    fn compares_directory_trees() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        for root in [&a, &b] {
            fs::create_dir_all(root.join("sub")).unwrap();
            fs::write(root.join("same.txt"), "x").unwrap();
        }
        fs::write(a.join("sub/changed"), "abc").unwrap();
        fs::write(b.join("sub/changed"), "abd").unwrap();
        fs::write(a.join("left-only"), "").unwrap();
        fs::create_dir(b.join("right-only")).unwrap();
        fs::write(a.join("kind"), "").unwrap();
        fs::create_dir(b.join("kind")).unwrap();
        let report = compare_dirs((&LocalVfs, &a), (&LocalVfs, &b), &Progress::default()).unwrap();
        assert_eq!(report.only_left, ["left-only"]);
        assert_eq!(report.only_right, ["right-only"]);
        assert_eq!(
            report.differ,
            [("kind".to_string(), "file vs directory".to_string()), ("sub/changed".into(), "contents differ".into())]
        );
        assert_eq!(report.same, 1);
        assert!(!report.is_identical());
    }
}
