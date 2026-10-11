//! Tags and colour labels on files and folders.
//!
//! Tags live in one file under the data directory (one `path<TAB>tags`
//! line per item), so they work on every filesystem and can be listed
//! without scanning the disk. On Linux they are also written to the
//! `user.xdg.tags` extended attribute, which other file managers (such as
//! Dolphin) read, so they survive moves made outside strata.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};

/// Tags shown as coloured dots instead of by name.
pub const COLORS: &[&str] = &["red", "orange", "yellow", "green", "blue", "purple", "gray"];

#[derive(Debug, Clone, Default)]
pub struct TagDb {
    file: Option<PathBuf>,
    tags: HashMap<PathBuf, Vec<String>>,
}

impl TagDb {
    /// Reads the tag file; a missing file is an empty database.
    pub fn load(file: PathBuf) -> Result<Self> {
        let mut db = Self { file: Some(file.clone()), tags: HashMap::new() };
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(db),
            Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
        };
        for line in text.lines() {
            let Some((path, tags)) = line.split_once('\t') else { continue };
            let tags: Vec<String> = tags.split(',').filter(|t| !t.is_empty()).map(String::from).collect();
            if !tags.is_empty() {
                db.tags.insert(PathBuf::from(path), tags);
            }
        }
        Ok(db)
    }

    /// An in-memory database that is never saved, for tests.
    pub fn in_memory() -> Self {
        Self::default()
    }

    fn save(&self) -> Result<()> {
        let Some(file) = &self.file else { return Ok(()) };
        let mut lines: Vec<String> =
            self.tags.iter().map(|(p, t)| format!("{}\t{}", p.to_string_lossy(), t.join(","))).collect();
        lines.sort();
        let mut text = lines.join("\n");
        text.push('\n');
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Write then rename, so a crash never leaves half a file.
        let tmp = file.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, file).with_context(|| format!("writing {}", file.display()))
    }

    pub fn get(&self, path: &Path) -> &[String] {
        self.tags.get(path).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    /// Adds `tag` to every path. Returns how many did not have it yet.
    pub fn add(&mut self, paths: &[PathBuf], tag: &str) -> Result<usize> {
        let tag = check_tag(tag)?;
        let mut added = 0;
        for path in paths {
            check_path(path)?;
            let tags = self.tags.entry(path.clone()).or_default();
            if !tags.contains(&tag) {
                tags.push(tag.clone());
                added += 1;
                write_xattr(path, tags);
            }
        }
        self.save()?;
        Ok(added)
    }

    /// Removes `tag` (or every tag) from the paths. Returns how many changed.
    pub fn remove(&mut self, paths: &[PathBuf], tag: Option<&str>) -> Result<usize> {
        let mut removed = 0;
        for path in paths {
            let Some(tags) = self.tags.get_mut(path) else { continue };
            let before = tags.len();
            match tag {
                Some(tag) => tags.retain(|t| t != tag),
                None => tags.clear(),
            }
            if tags.len() != before {
                removed += 1;
                write_xattr(path, tags);
            }
            if tags.is_empty() {
                self.tags.remove(path);
            }
        }
        if removed > 0 {
            self.save()?;
        }
        Ok(removed)
    }

    /// Carries tags along when `from` is renamed or moved to `to`,
    /// including everything inside it.
    pub fn moved(&mut self, from: &Path, to: &Path) {
        self.moved_all(&[(from.to_path_buf(), to.to_path_buf())]);
    }

    /// Like [`TagDb::moved`] for renames that happen together, so swaps
    /// (`a` -> `b`, `b` -> `a`) keep each item's tags.
    pub fn moved_all(&mut self, moves: &[(PathBuf, PathBuf)]) {
        let mut taken = Vec::new();
        for (from, to) in moves {
            let affected: Vec<PathBuf> = self.tags.keys().filter(|p| p.starts_with(from)).cloned().collect();
            for old in affected {
                let rest = old.strip_prefix(from).unwrap_or(Path::new(""));
                let new = if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) };
                if let Some(tags) = self.tags.remove(&old) {
                    taken.push((new, tags));
                }
            }
        }
        if taken.is_empty() {
            return;
        }
        self.tags.extend(taken);
        let _ = self.save();
    }

    /// Every tag with how many items carry it.
    pub fn all(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for tag in self.tags.values().flatten() {
            *out.entry(tag.clone()).or_insert(0) += 1;
        }
        out
    }

    /// Paths carrying every one of `tags`.
    pub fn with_all(&self, tags: &[String]) -> HashSet<PathBuf> {
        self.tags.iter().filter(|(_, t)| tags.iter().all(|want| t.contains(want))).map(|(p, _)| p.clone()).collect()
    }

    /// Forgets tags of items that no longer exist. Returns how many.
    pub fn prune(&mut self) -> Result<usize> {
        let before = self.tags.len();
        self.tags.retain(|p, _| std::fs::symlink_metadata(p).is_ok());
        let pruned = before - self.tags.len();
        if pruned > 0 {
            self.save()?;
        }
        Ok(pruned)
    }
}

fn check_tag(tag: &str) -> Result<String> {
    let tag = tag.trim().to_lowercase();
    if tag.is_empty() || tag.contains([',', '\t', '\n', ' ']) {
        bail!("a tag is one word without commas");
    }
    Ok(tag)
}

fn check_path(path: &Path) -> Result<()> {
    if path.to_string_lossy().contains(['\t', '\n']) {
        bail!("cannot tag '{}': its name contains a tab or newline", path.display());
    }
    Ok(())
}

/// Mirrors our tags into `user.xdg.tags`, keeping tags other programs set.
/// Best effort: filesystems without extended attributes just skip it.
#[cfg(target_os = "linux")]
fn write_xattr(path: &Path, ours: &[String]) {
    const ATTR: &str = "user.xdg.tags";
    let known: HashSet<&str> = COLORS.iter().copied().chain(ours.iter().map(String::as_str)).collect();
    let mut tags: Vec<String> = xattr::get(path, ATTR)
        .ok()
        .flatten()
        .map(|v| String::from_utf8_lossy(&v).split(',').map(|t| t.trim().to_string()).collect())
        .unwrap_or_default();
    // Drop what we may have written before, then add the current set.
    tags.retain(|t| !t.is_empty() && !known.contains(t.as_str()));
    tags.extend(ours.iter().cloned());
    let _ = if tags.is_empty() { xattr::remove(path, ATTR) } else { xattr::set(path, ATTR, tags.join(",").as_bytes()) };
}

#[cfg(not(target_os = "linux"))]
fn write_xattr(_path: &Path, _ours: &[String]) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_removes_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tags.tsv");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        let mut db = TagDb::load(file.clone()).unwrap();
        assert_eq!(db.add(&[a.clone(), b.clone()], "Red").unwrap(), 2);
        assert_eq!(db.add(std::slice::from_ref(&a), "red").unwrap(), 0, "already tagged");
        db.add(std::slice::from_ref(&a), "todo").unwrap();
        assert!(db.add(std::slice::from_ref(&a), "two words").is_err());

        let db2 = TagDb::load(file.clone()).unwrap();
        assert_eq!(db2.get(&a), ["red", "todo"]);
        assert_eq!(db2.all().get("red"), Some(&2));
        assert_eq!(db2.with_all(&["red".into(), "todo".into()]), [a.clone()].into_iter().collect());

        assert_eq!(db.remove(std::slice::from_ref(&a), Some("red")).unwrap(), 1);
        assert_eq!(db.remove(std::slice::from_ref(&b), None).unwrap(), 1);
        assert!(db.get(&b).is_empty());
        assert_eq!(TagDb::load(file).unwrap().all().len(), 1);
    }

    #[test]
    fn tags_follow_moves() {
        let mut db = TagDb::in_memory();
        db.add(&[PathBuf::from("/x/dir"), PathBuf::from("/x/dir/inner.txt"), PathBuf::from("/x/dirt")], "blue")
            .unwrap();
        db.moved(Path::new("/x/dir"), Path::new("/y/renamed"));
        assert_eq!(db.get(Path::new("/y/renamed")), ["blue"]);
        assert_eq!(db.get(Path::new("/y/renamed/inner.txt")), ["blue"]);
        assert_eq!(db.get(Path::new("/x/dirt")), ["blue"], "a sibling with a common prefix stays");
    }

    #[test]
    fn swapped_names_keep_their_tags() {
        let mut db = TagDb::in_memory();
        db.add(&[PathBuf::from("/d/a")], "red").unwrap();
        db.add(&[PathBuf::from("/d/b")], "blue").unwrap();
        db.moved_all(&[(PathBuf::from("/d/a"), PathBuf::from("/d/b")), (PathBuf::from("/d/b"), PathBuf::from("/d/a"))]);
        assert_eq!(db.get(Path::new("/d/b")), ["red"]);
        assert_eq!(db.get(Path::new("/d/a")), ["blue"]);
    }

    #[test]
    fn prune_drops_missing_items() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::write(&real, "").unwrap();
        let mut db = TagDb::in_memory();
        db.add(&[real.clone(), dir.path().join("gone")], "green").unwrap();
        assert_eq!(db.prune().unwrap(), 1);
        assert_eq!(db.get(&real), ["green"]);
    }
}
