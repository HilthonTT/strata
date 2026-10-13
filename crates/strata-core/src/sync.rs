//! One-way sync of a directory tree into another, on any pair of backends:
//! copy what is missing, replace what differs and, when mirroring, remove
//! what only the destination has.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use anyhow::{bail, Result};

use crate::compare;
use crate::jobs::{Cancelled, Progress};
use crate::ops::{copy_tree_with, tree_size, CopyOptions};
use crate::Vfs;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncOptions {
    /// Also remove items that only the destination has.
    pub mirror: bool,
    /// Replace a differing file only when the source copy is newer.
    pub newer_only: bool,
    /// Verification and bandwidth limit for the copies.
    pub copy: CopyOptions,
}

/// What to do with one path, relative to both roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncStep {
    Copy(String),
    Replace(String),
    Delete(String),
}

impl SyncStep {
    pub fn rel(&self) -> &str {
        match self {
            SyncStep::Copy(rel) | SyncStep::Replace(rel) | SyncStep::Delete(rel) => rel,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncPlan {
    pub steps: Vec<SyncStep>,
    /// Bytes to copy for the copy and replace steps.
    pub bytes: u64,
    /// Differing files left alone because the destination is newer.
    pub kept_newer: Vec<String>,
}

impl SyncPlan {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// `(copy, replace, delete)` counts.
    pub fn counts(&self) -> (usize, usize, usize) {
        self.steps.iter().fold((0, 0, 0), |(c, r, d), step| match step {
            SyncStep::Copy(_) => (c + 1, r, d),
            SyncStep::Replace(_) => (c, r + 1, d),
            SyncStep::Delete(_) => (c, r, d + 1),
        })
    }
}

/// Destinations a sync changed, so it can be undone.
#[derive(Debug, Default)]
pub struct SyncLog {
    /// Items that did not exist before.
    pub created: Vec<PathBuf>,
    /// Items whose old version went to the trash and got a new one.
    pub replaced: Vec<PathBuf>,
    /// Items moved to the trash.
    pub trashed: Vec<PathBuf>,
}

/// Works out how to make `dst` match `src`.
pub fn plan(
    src: (&dyn Vfs, &Path),
    dst: (&dyn Vfs, &Path),
    opts: SyncOptions,
    progress: &Progress,
) -> Result<SyncPlan> {
    let report = compare::compare_dirs(src, dst, progress)?;
    let mut plan = SyncPlan::default();
    let size_of = |rel: &str, progress: &Progress| -> Result<u64> {
        let entry = src.0.stat(&join_rel(src.0, src.1, rel))?;
        tree_size(src.0, &entry, progress)
    };
    for rel in report.only_left {
        plan.bytes += size_of(&rel, progress)?;
        plan.steps.push(SyncStep::Copy(rel));
    }
    for (rel, _) in report.differ {
        if opts.newer_only {
            let modified = |vfs: &dyn Vfs, root: &Path| vfs.stat(&join_rel(vfs, root, &rel)).ok()?.modified;
            if let (Some(theirs), Some(ours)) = (modified(dst.0, dst.1), modified(src.0, src.1)) {
                if theirs >= ours {
                    plan.kept_newer.push(rel);
                    continue;
                }
            }
        }
        plan.bytes += size_of(&rel, progress)?;
        plan.steps.push(SyncStep::Replace(rel));
    }
    if opts.mirror {
        plan.steps.extend(report.only_right.into_iter().map(SyncStep::Delete));
    }
    Ok(plan)
}

/// Carries out `plan`. Replaced and deleted items go to the trash when
/// `dst` has one; otherwise they are removed for good. One failing item
/// does not stop the rest; the job fails at the end if any did.
pub fn apply(
    plan: &SyncPlan,
    src: (&dyn Vfs, &Path),
    dst: (&dyn Vfs, &Path),
    opts: SyncOptions,
    progress: &Progress,
    log: &Mutex<SyncLog>,
) -> Result<()> {
    let copy = opts.copy;
    progress.total_bytes.store(plan.bytes * copy.progress_weight(dst.0), Ordering::Relaxed);
    progress.total_items.store(plan.steps.len(), Ordering::Relaxed);
    let record = |f: fn(&mut SyncLog) -> &mut Vec<PathBuf>, path: PathBuf| {
        f(&mut log.lock().unwrap_or_else(|e| e.into_inner())).push(path);
    };
    let remove = |path: &Path| if dst.0.is_local() { dst.0.trash(path) } else { dst.0.remove_all(path) };
    let mut errors = Vec::new();
    for step in &plan.steps {
        progress.check()?;
        progress.set_current(step.rel().to_string());
        let (from, to) = (join_rel(src.0, src.1, step.rel()), join_rel(dst.0, dst.1, step.rel()));
        let result = match step {
            SyncStep::Copy(_) => src.0.stat(&from).and_then(|entry| {
                copy_tree_with(src.0, &entry, dst.0, &to, progress, &copy)?;
                record(|l| &mut l.created, to.clone());
                Ok(())
            }),
            SyncStep::Replace(_) => src.0.stat(&from).and_then(|entry| {
                remove(&to)?;
                // Recorded before copying: undo must bring the old version
                // back even when the new one arrived only in part.
                record(|l| &mut l.replaced, to.clone());
                copy_tree_with(src.0, &entry, dst.0, &to, progress, &copy)
            }),
            SyncStep::Delete(_) => remove(&to).map(|()| record(|l| &mut l.trashed, to.clone())),
        };
        match result {
            Err(e) if e.is::<Cancelled>() => return Err(e),
            Err(e) => errors.push(format!("{}: {e:#}", step.rel())),
            Ok(()) => {}
        }
        progress.item_done();
    }
    match errors.as_slice() {
        [] => Ok(()),
        [one] => bail!("{one}"),
        [first, ..] => bail!("{} of {} items failed, first: {first}", errors.len(), plan.steps.len()),
    }
}

/// `root/a/b` from `a/b`, using the backend's separators.
fn join_rel(vfs: &dyn Vfs, root: &Path, rel: &str) -> PathBuf {
    rel.split('/').filter(|part| !part.is_empty()).fold(root.to_path_buf(), |path, part| vfs.join(&path, part))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::LocalVfs;
    use std::fs;
    use std::time::{Duration, SystemTime};

    fn set_mtime(path: &Path, when: SystemTime) {
        fs::File::options().write(true).open(path).unwrap().set_modified(when).unwrap();
    }

    fn tree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        for root in [&a, &b] {
            fs::create_dir_all(root.join("sub")).unwrap();
            fs::write(root.join("same"), "x").unwrap();
        }
        fs::write(a.join("sub/new"), "new").unwrap();
        fs::write(a.join("changed"), "left").unwrap();
        fs::write(b.join("changed"), "right!").unwrap();
        fs::write(b.join("extra"), "").unwrap();
        (dir, a, b)
    }

    #[test]
    fn plans_copies_replacements_and_mirror_deletes() {
        let (_dir, a, b) = tree();
        let p = Progress::default();
        let plan = plan((&LocalVfs, &a), (&LocalVfs, &b), SyncOptions::default(), &p).unwrap();
        assert_eq!(plan.steps, [SyncStep::Copy("sub/new".into()), SyncStep::Replace("changed".into())]);
        assert_eq!(plan.bytes, 7);
        let mirror = SyncOptions { mirror: true, ..Default::default() };
        let plan = super::plan((&LocalVfs, &a), (&LocalVfs, &b), mirror, &p).unwrap();
        assert_eq!(plan.counts(), (1, 1, 1));
    }

    #[test]
    fn newer_only_keeps_newer_destination_files() {
        let (_dir, a, b) = tree();
        let now = SystemTime::now();
        set_mtime(&a.join("changed"), now - Duration::from_secs(60));
        set_mtime(&b.join("changed"), now);
        let opts = SyncOptions { newer_only: true, ..Default::default() };
        let plan = plan((&LocalVfs, &a), (&LocalVfs, &b), opts, &Progress::default()).unwrap();
        assert_eq!(plan.steps, [SyncStep::Copy("sub/new".into())]);
        assert_eq!(plan.kept_newer, ["changed"]);
    }

    /// The local disk without a trash, so tests remove items for good
    /// instead of filling the real trash.
    #[derive(Debug)]
    struct NoTrash;

    impl Vfs for NoTrash {
        fn scheme(&self) -> &'static str {
            "test"
        }
        fn label(&self) -> String {
            "test".into()
        }
        fn home(&self) -> PathBuf {
            LocalVfs.home()
        }
        fn read_dir(&self, path: &Path) -> Result<Vec<crate::Entry>> {
            LocalVfs.read_dir(path)
        }
        fn stat(&self, path: &Path) -> Result<crate::Entry> {
            LocalVfs.stat(path)
        }
        fn create_dir(&self, path: &Path) -> Result<()> {
            LocalVfs.create_dir(path)
        }
        fn create_file(&self, path: &Path) -> Result<()> {
            LocalVfs.create_file(path)
        }
        fn remove_file(&self, path: &Path) -> Result<()> {
            LocalVfs.remove_file(path)
        }
        fn remove_dir(&self, path: &Path) -> Result<()> {
            LocalVfs.remove_dir(path)
        }
        fn rename(&self, from: &Path, to: &Path) -> Result<()> {
            LocalVfs.rename(from, to)
        }
        fn reader(&self, path: &Path) -> Result<Box<dyn std::io::Read + Send>> {
            LocalVfs.reader(path)
        }
        fn writer(&self, path: &Path) -> Result<Box<dyn std::io::Write + Send>> {
            LocalVfs.writer(path)
        }
    }

    #[test]
    fn applying_a_plan_makes_the_trees_match() {
        let (_dir, a, b) = tree();
        fs::create_dir(b.join("kind")).unwrap();
        fs::write(a.join("kind"), "file now").unwrap();
        let p = Progress::default();
        let opts = SyncOptions { mirror: true, ..Default::default() };
        let plan = plan((&LocalVfs, &a), (&NoTrash, &b), opts, &p).unwrap();
        let log = Mutex::default();
        apply(&plan, (&LocalVfs, &a), (&NoTrash, &b), opts, &p, &log).unwrap();
        assert!(compare::compare_dirs((&LocalVfs, &a), (&LocalVfs, &b), &p).unwrap().is_identical());
        let log = log.into_inner().unwrap();
        assert_eq!(log.created, [b.join("sub/new")]);
        assert_eq!(log.replaced, [b.join("changed"), b.join("kind")]);
        assert_eq!(log.trashed, [b.join("extra")]);
    }

    #[test]
    fn a_failing_item_does_not_stop_the_rest() {
        let (_dir, a, b) = tree();
        let plan = SyncPlan {
            steps: vec![SyncStep::Copy("missing".into()), SyncStep::Copy("sub/new".into())],
            ..Default::default()
        };
        let err = apply(
            &plan,
            (&LocalVfs, &a),
            (&NoTrash, &b),
            SyncOptions::default(),
            &Progress::default(),
            &Mutex::default(),
        );
        assert!(err.unwrap_err().to_string().starts_with("missing:"));
        assert!(b.join("sub/new").exists());
    }
}
