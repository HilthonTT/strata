//! File transfers and deletion that work across any pair of [`Vfs`] backends.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::inspect::{self, HashAlgo};
use crate::jobs::Progress;
use crate::throttle::Throttle;
use crate::util::unique_name;
use crate::vfs::{same_vfs, VfsRef};
use crate::{Entry, EntryKind, Vfs};

/// `(source, destination)` of each top-level item a transfer finished,
/// recorded so the transfer can be undone.
pub type TransferLog = Mutex<Vec<(PathBuf, PathBuf)>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferMode {
    Copy,
    Move,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Conflict {
    /// Keep both: the new item gets a ` (1)` suffix.
    #[default]
    KeepBoth,
    Overwrite,
    Skip,
    /// Overwrite when the source was modified after the existing item,
    /// otherwise skip.
    OverwriteIfNewer,
}

/// How a transfer settles name clashes beyond its batch policy.
#[derive(Debug, Clone, Default)]
pub struct Resolutions {
    /// Answers for single sources, decided before the transfer starts;
    /// sources not listed follow [`Transfer::conflict`].
    pub per_source: HashMap<PathBuf, Conflict>,
    /// Existing destinations replaced so far (trashed on local disks,
    /// deleted elsewhere), so undo can bring them back.
    pub replaced: Arc<Mutex<Vec<PathBuf>>>,
}

/// A top-level source whose name is already taken in the destination.
#[derive(Debug, Clone)]
pub struct Clash {
    pub source: Entry,
    pub existing: Entry,
}

impl Clash {
    /// Whether the source was modified after the existing item. Unknown
    /// times never count as newer.
    pub fn source_is_newer(&self) -> bool {
        is_newer(&self.source, &self.existing)
    }
}

fn is_newer(source: &Entry, existing: &Entry) -> bool {
    matches!((source.modified, existing.modified), (Some(s), Some(e)) if s > e)
}

/// What `policy` does about `source` landing on `existing`: always one of
/// [`Conflict::KeepBoth`], [`Conflict::Overwrite`] or [`Conflict::Skip`].
pub fn resolve(policy: Conflict, source: &Entry, existing: &Entry) -> Conflict {
    match policy {
        Conflict::OverwriteIfNewer if is_newer(source, existing) => Conflict::Overwrite,
        Conflict::OverwriteIfNewer => Conflict::Skip,
        other => other,
    }
}

/// Extra care taken while copying.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopyOptions {
    /// Check each copied file: the bytes read from the source are hashed
    /// (SHA-256) on their way through, the copy is read back and hashed,
    /// and a mismatch fails the job before a move deletes anything. Only
    /// copies onto the local disk are checked; other destinations would
    /// have to be downloaded again, so they are copied without a check.
    pub verify: bool,
    /// Bytes per second each job may move at most; `None` = unlimited.
    /// Copies between two rclone remotes hand it to rclone (`--bwlimit`).
    pub bandwidth_limit: Option<u64>,
}

impl CopyOptions {
    /// Whether copies onto `dst` are read back.
    fn verifies(&self, dst: &dyn Vfs) -> bool {
        self.verify && dst.is_local()
    }

    /// Bytes of progress per byte copied onto `dst`: reading a copy back
    /// counts as much as writing it.
    pub fn progress_weight(&self, dst: &dyn Vfs) -> u64 {
        if self.verifies(dst) {
            2
        } else {
            1
        }
    }
}

/// Copies or moves `sources` from `src` into `dest_dir` on `dst`.
#[derive(Clone)]
pub struct Transfer {
    pub mode: TransferMode,
    pub src: VfsRef,
    pub sources: Vec<PathBuf>,
    pub dst: VfsRef,
    pub dest_dir: PathBuf,
    pub conflict: Conflict,
    pub resolutions: Resolutions,
    /// `(source, destination)` of each top-level item, filled as items
    /// start. Items already listed go to the same destination, keeping
    /// files that arrived complete: that's how an interrupted transfer is
    /// resumed. Running the same `Transfer` twice therefore resumes it.
    pub targets: Arc<TransferLog>,
    pub options: CopyOptions,
}

const BUF_SIZE: usize = 256 * 1024;

impl Transfer {
    pub fn run(&self, progress: &Progress) -> Result<()> {
        self.run_logged(progress, &Mutex::default())
    }

    /// Like [`Transfer::run`], recording each finished item in `log`. The
    /// log is filled even when the transfer fails or is cancelled midway.
    pub fn run_logged(&self, progress: &Progress, log: &TransferLog) -> Result<()> {
        let record = |src: &Path, dst: &Path| {
            log.lock().unwrap_or_else(|e| e.into_inner()).push((src.to_path_buf(), dst.to_path_buf()));
        };
        progress.set_current("scanning…");
        let (bytes, items) = self.measure(progress)?;
        let weight = self.options.progress_weight(&*self.dst);
        progress.total_bytes.store(bytes * weight, Ordering::Relaxed);
        progress.total_items.store(items, Ordering::Relaxed);

        let same = same_vfs(&self.src, &self.dst);
        let earlier: Vec<(PathBuf, PathBuf)> = self.targets.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let copier = Copier::new(&*self.src, &*self.dst, progress, &self.options);
        for source in &self.sources {
            progress.check()?;
            let entry = self.src.stat(source)?;
            if same && self.dest_dir.starts_with(source) && entry.is_dir() {
                bail!("cannot {} a directory into itself", self.verb());
            }
            let resumed = earlier.iter().find(|(s, _)| s == source).map(|(_, t)| t.clone());
            let target = match resumed.clone() {
                Some(target) => target,
                None => {
                    let Some(target) = self.target_for(&entry)? else {
                        continue;
                    };
                    self.targets.lock().unwrap_or_else(|e| e.into_inner()).push((source.clone(), target.clone()));
                    target
                }
            };
            if same && self.mode == TransferMode::Move && self.src.rename(source, &target).is_ok() {
                progress.add_bytes(tree_size(&*self.src, &entry, progress).unwrap_or(0) * weight);
                record(source, &target);
                progress.item_done();
                continue;
            }
            copier.tree(&entry, &target, resumed.is_some())?;
            if self.mode == TransferMode::Move {
                progress.set_current(format!("removing {}", entry.name));
                self.src.remove_all(source)?;
            }
            record(source, &target);
            progress.item_done();
        }
        Ok(())
    }

    fn verb(&self) -> &'static str {
        match self.mode {
            TransferMode::Copy => "copy",
            TransferMode::Move => "move",
        }
    }

    fn measure(&self, progress: &Progress) -> Result<(u64, usize)> {
        let mut bytes = 0;
        for source in &self.sources {
            let entry = self.src.stat(source)?;
            bytes += tree_size(&*self.src, &entry, progress)?;
        }
        Ok((bytes, self.sources.len()))
    }

    /// Names of top-level sources that already exist in the destination
    /// (a move onto itself does not count).
    pub fn conflicts(&self) -> Vec<String> {
        self.clashes().into_iter().map(|c| c.source.name).collect()
    }

    /// The top-level sources that already exist in the destination, with
    /// both sides' details (a move onto itself does not count).
    pub fn clashes(&self) -> Vec<Clash> {
        self.sources
            .iter()
            .filter_map(|source| {
                let name = source.file_name()?.to_string_lossy().into_owned();
                let target = self.dst.join(&self.dest_dir, &name);
                if self.onto_itself(source, &target) {
                    return None;
                }
                let existing = self.dst.stat(&target).ok()?;
                let source = self.src.stat(source).ok()?;
                Some(Clash { source, existing })
            })
            .collect()
    }

    fn onto_itself(&self, source: &Path, target: &Path) -> bool {
        same_vfs(&self.src, &self.dst) && target == source && self.mode == TransferMode::Move
    }

    /// What to do about `source` clashing with `existing`: its own answer
    /// if it has one, else the batch policy.
    pub fn decision(&self, source: &Entry, existing: &Entry) -> Conflict {
        let policy = self.resolutions.per_source.get(&source.path).copied().unwrap_or(self.conflict);
        resolve(policy, source, existing)
    }

    /// `(source, destination)` of the top-level items not in `finished`,
    /// without changing anything: items that already started keep their
    /// destination, others get one from the conflict policy (an existing
    /// destination is replaced only with [`Conflict::Overwrite`]).
    pub fn remaining(&self, finished: &TransferLog) -> Vec<(PathBuf, PathBuf)> {
        let finished = finished.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let started = self.targets.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.sources
            .iter()
            .filter(|s| !finished.iter().any(|(done, _)| done == *s))
            .filter_map(|source| {
                if let Some((_, target)) = started.iter().find(|(s, _)| s == source) {
                    return Some((source.clone(), target.clone()));
                }
                let name = source.file_name()?.to_string_lossy().into_owned();
                let target = self.dst.join(&self.dest_dir, &name);
                let Ok(existing) = self.dst.stat(&target) else {
                    return Some((source.clone(), target));
                };
                if self.onto_itself(source, &target) {
                    return None;
                }
                let entry = self.src.stat(source).ok()?;
                match self.decision(&entry, &existing) {
                    Conflict::Skip | Conflict::OverwriteIfNewer => None,
                    Conflict::Overwrite => Some((source.clone(), target)),
                    Conflict::KeepBoth => {
                        let name = unique_name(&name, |n| self.dst.exists(&self.dst.join(&self.dest_dir, n)));
                        Some((source.clone(), self.dst.join(&self.dest_dir, &name)))
                    }
                }
            })
            .collect()
    }

    /// Resolves the destination path for a top-level source, honouring the
    /// conflict policy. `None` means skip.
    fn target_for(&self, entry: &Entry) -> Result<Option<PathBuf>> {
        let target = self.dst.join(&self.dest_dir, &entry.name);
        let Ok(existing) = self.dst.stat(&target) else {
            return Ok(Some(target));
        };
        if self.onto_itself(&entry.path, &target) {
            return Ok(None);
        }
        match self.decision(entry, &existing) {
            Conflict::Skip | Conflict::OverwriteIfNewer => Ok(None),
            // Replaced items go to the trash where there is one.
            Conflict::Overwrite => {
                if self.dst.is_local() {
                    self.dst.trash(&target)?;
                } else {
                    self.dst.remove_all(&target)?;
                }
                self.resolutions.replaced.lock().unwrap_or_else(|e| e.into_inner()).push(target.clone());
                Ok(Some(target))
            }
            Conflict::KeepBoth => {
                let name = unique_name(&entry.name, |n| self.dst.exists(&self.dst.join(&self.dest_dir, n)));
                Ok(Some(self.dst.join(&self.dest_dir, &name)))
            }
        }
    }
}

/// Copies a file or directory tree from one backend to another.
pub fn copy_tree(src: &dyn Vfs, entry: &Entry, dst: &dyn Vfs, target: &Path, progress: &Progress) -> Result<()> {
    copy_tree_with(src, entry, dst, target, progress, &CopyOptions::default())
}

/// [`copy_tree`] with extra care, such as verifying each file.
pub fn copy_tree_with(
    src: &dyn Vfs,
    entry: &Entry,
    dst: &dyn Vfs,
    target: &Path,
    progress: &Progress,
    options: &CopyOptions,
) -> Result<()> {
    Copier::new(src, dst, progress, options).tree(entry, target, false)
}

/// One copy from `src` to `dst`.
struct Copier<'a> {
    src: &'a dyn Vfs,
    dst: &'a dyn Vfs,
    progress: &'a Progress,
    options: &'a CopyOptions,
    /// Shared by every file, so the limit holds for the whole job.
    throttle: RefCell<Option<Throttle>>,
}

impl<'a> Copier<'a> {
    fn new(src: &'a dyn Vfs, dst: &'a dyn Vfs, progress: &'a Progress, options: &'a CopyOptions) -> Self {
        let throttle = RefCell::new(options.bandwidth_limit.map(Throttle::new));
        Self { src, dst, progress, options, throttle }
    }

    /// Copies `entry` to `target`; with `resume`, into a partial earlier
    /// copy: existing folders and links are kept, and files already there
    /// with the right size are skipped (when verifying, only if their
    /// contents match too). Anything else is copied again from the start.
    fn tree(&self, entry: &Entry, target: &Path, resume: bool) -> Result<()> {
        let (src, dst, progress) = (self.src, self.dst, self.progress);
        progress.check()?;
        // Between two rclone remotes, rclone copies by itself: on the
        // server when it can, without passing through here otherwise.
        let remotes = src.as_rclone().zip(dst.as_rclone()).filter(|_| entry.kind != EntryKind::Other);
        if let Some((from, to)) = remotes {
            progress.set_current(entry.name.clone());
            if from.copy_to(entry, to, target, self.options.bandwidth_limit, progress)? {
                return Ok(());
            }
        }
        let existing = if resume { dst.stat(target).ok() } else { None };
        match entry.kind {
            EntryKind::Dir => {
                if !existing.as_ref().is_some_and(|e| e.kind == EntryKind::Dir) {
                    dst.create_dir(target)?;
                }
                for child in src.read_dir(&entry.path)? {
                    let child_target = dst.join(target, &child.name);
                    self.tree(&child, &child_target, resume)?;
                }
            }
            EntryKind::Symlink { .. } if existing.as_ref().is_some_and(Entry::is_symlink) => {}
            EntryKind::Symlink { .. } if src.is_local() && dst.is_local() => {
                copy_local_symlink(&entry.path, target)?;
            }
            EntryKind::File
                if existing.as_ref().is_some_and(|e| e.kind == EntryKind::File && e.size == entry.size)
                    && self.already_copied(entry, target)? => {}
            // Directory links on remote backends are skipped to avoid cycles.
            EntryKind::Symlink { to_dir: true } => {}
            EntryKind::File | EntryKind::Symlink { .. } => {
                progress.set_current(entry.name.clone());
                let mut reader = src.reader(&entry.path)?;
                let mut writer = dst.writer(target)?;
                let mut hasher = self.options.verifies(dst).then(Sha256::new);
                let mut throttle = self.throttle.borrow_mut();
                stream(&mut *reader, &mut *writer, progress, hasher.as_mut(), throttle.as_mut())
                    .with_context(|| format!("copy {}", entry.path.display()))?;
                drop(writer);
                if let Some(hasher) = hasher {
                    self.check_copy(entry, target, &hex(&hasher.finalize()))?;
                }
            }
            EntryKind::Other => {}
        }
        Ok(())
    }

    /// Whether a file found at `target` when resuming (with the right size)
    /// can stay: always, unless verifying, when it must match the source.
    fn already_copied(&self, entry: &Entry, target: &Path) -> Result<bool> {
        if !self.options.verifies(self.dst) {
            self.progress.add_bytes(entry.size);
            return Ok(true);
        }
        self.progress.set_current(format!("verifying {}", entry.name));
        let source = inspect::hash_file(self.src, &entry.path, &[HashAlgo::Sha256], self.progress)?.remove(0);
        let copy = inspect::hash_file(self.dst, target, &[HashAlgo::Sha256], self.progress)?.remove(0);
        if source == copy {
            return Ok(true);
        }
        // Copied again from the start: take back what was counted.
        self.progress.done_bytes.fetch_sub(entry.size * 2, Ordering::Relaxed);
        Ok(false)
    }

    /// Reads the copy back and compares its hash with the source's. A copy
    /// that differs is removed, so it is not mistaken for a good one.
    fn check_copy(&self, entry: &Entry, target: &Path, expected: &str) -> Result<()> {
        self.progress.set_current(format!("verifying {}", entry.name));
        let actual = inspect::hash_file(self.dst, target, &[HashAlgo::Sha256], self.progress)
            .with_context(|| format!("verify {}", target.display()))?
            .remove(0);
        if actual != expected {
            let _ = self.dst.remove_file(target);
            bail!(
                "verification failed: the copy of {} does not match the original (SHA-256 {} ≠ {})",
                entry.path.display(),
                short_hash(&actual),
                short_hash(expected)
            );
        }
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn short_hash(hash: &str) -> &str {
    &hash[..hash.len().min(12)]
}

/// Moves `from` (on `src`) to exactly `to` (on `dst`): a rename when
/// possible, otherwise copy then delete. Used to undo moves.
pub fn relocate(src: &VfsRef, from: &Path, dst: &VfsRef, to: &Path, progress: &Progress) -> Result<()> {
    if dst.exists(to) {
        bail!("{} already exists", to.display());
    }
    if same_vfs(src, dst) && src.rename(from, to).is_ok() {
        return Ok(());
    }
    let entry = src.stat(from)?;
    copy_tree(&**src, &entry, &**dst, to, progress)?;
    src.remove_all(from)
}

/// Copies `reader` into `writer`, feeding the bytes to `hasher` if given
/// and keeping to `throttle`'s rate if given.
fn stream(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    progress: &Progress,
    mut hasher: Option<&mut Sha256>,
    mut throttle: Option<&mut Throttle>,
) -> Result<()> {
    let mut buf = vec![0u8; BUF_SIZE];
    let chunk = throttle.as_ref().map_or(BUF_SIZE, |t| t.chunk(BUF_SIZE));
    loop {
        progress.check()?;
        let n = reader.read(&mut buf[..chunk])?;
        if n == 0 {
            break;
        }
        if let Some(hasher) = hasher.as_mut() {
            hasher.update(&buf[..n]);
        }
        writer.write_all(&buf[..n])?;
        progress.add_bytes(n as u64);
        if let Some(throttle) = throttle.as_mut() {
            progress.sleep(throttle.delay(n as u64, std::time::Instant::now()));
        }
    }
    writer.flush()?;
    Ok(())
}

#[cfg(unix)]
fn copy_local_symlink(src: &Path, dst: &Path) -> Result<()> {
    let target = std::fs::read_link(src)?;
    std::os::unix::fs::symlink(target, dst)?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_local_symlink(src: &Path, dst: &Path) -> Result<()> {
    std::fs::copy(src, dst)?;
    Ok(())
}

/// Total size in bytes of a file or directory tree.
pub fn tree_size(vfs: &dyn crate::Vfs, entry: &Entry, progress: &Progress) -> Result<u64> {
    progress.check()?;
    Ok(match entry.kind {
        EntryKind::Dir => {
            let mut total = 0;
            for child in vfs.read_dir(&entry.path)? {
                total += tree_size(vfs, &child, progress)?;
            }
            total
        }
        EntryKind::File => entry.size,
        _ => 0,
    })
}

/// Restores items trashed since `since` to their original locations.
/// Returns how many were restored.
pub fn restore_from_trash(paths: &[PathBuf], since: std::time::SystemTime) -> Result<usize> {
    let chosen = latest_trashed(paths, since)?;
    if chosen.is_empty() {
        bail!("the items are no longer in the trash");
    }
    restore_items(chosen)
}

/// Swaps local `paths` with their versions trashed since `since`: the
/// current items go to the trash and the trashed ones come back. The
/// versions to restore are picked before anything is trashed, so a swap
/// within the same second as the trashing still restores the right ones.
pub fn swap_with_trashed(vfs: &dyn Vfs, paths: &[PathBuf], since: std::time::SystemTime) -> Result<usize> {
    let chosen = latest_trashed(paths, since)?;
    if chosen.is_empty() {
        bail!("the items are no longer in the trash");
    }
    for path in paths.iter().filter(|p| vfs.exists(p)) {
        vfs.trash(path)?;
    }
    restore_items(chosen)
}

/// The latest version of each of `paths` trashed since `since`.
#[cfg(any(
    target_os = "windows",
    all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
))]
fn latest_trashed(paths: &[PathBuf], since: std::time::SystemTime) -> Result<Vec<trash::TrashItem>> {
    let since = since.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) - 2;
    let mut chosen: Vec<trash::TrashItem> = Vec::new();
    for item in trash::os_limited::list().context("cannot read the trash")? {
        if item.time_deleted < since || !paths.contains(&item.original_path()) {
            continue;
        }
        match chosen.iter_mut().find(|c| c.original_path() == item.original_path()) {
            Some(existing) if existing.time_deleted < item.time_deleted => *existing = item,
            Some(_) => {}
            None => chosen.push(item),
        }
    }
    Ok(chosen)
}

#[cfg(any(
    target_os = "windows",
    all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
))]
fn restore_items(chosen: Vec<trash::TrashItem>) -> Result<usize> {
    let count = chosen.len();
    trash::os_limited::restore_all(chosen).context("cannot restore from the trash")?;
    Ok(count)
}

#[cfg(not(any(
    target_os = "windows",
    all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
)))]
fn latest_trashed(_paths: &[PathBuf], _since: std::time::SystemTime) -> Result<Vec<()>> {
    bail!("restoring from the trash is not supported on this platform")
}

#[cfg(not(any(
    target_os = "windows",
    all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
)))]
fn restore_items(_chosen: Vec<()>) -> Result<usize> {
    bail!("restoring from the trash is not supported on this platform")
}

/// Deletes items, using the trash when requested and supported.
pub fn delete(vfs: &VfsRef, paths: &[PathBuf], use_trash: bool, progress: &Progress) -> Result<()> {
    progress.total_items.store(paths.len(), Ordering::Relaxed);
    for path in paths {
        progress.check()?;
        progress.set_current(path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        if use_trash {
            vfs.trash(path)?;
        } else {
            vfs.remove_all(path)?;
        }
        progress.item_done();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::LocalVfs;
    use std::fs;
    use std::sync::Arc;

    fn local() -> VfsRef {
        Arc::new(LocalVfs)
    }

    #[test]
    fn copies_trees_and_keeps_both_on_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("nested")).unwrap();
        fs::write(src.join("nested/a.txt"), "hello").unwrap();
        let dest = dir.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let vfs = local();
        let copy = || Transfer {
            mode: TransferMode::Copy,
            src: vfs.clone(),
            sources: vec![src.clone()],
            dst: vfs.clone(),
            dest_dir: dest.clone(),
            conflict: Conflict::KeepBoth,
            resolutions: Default::default(),
            targets: Default::default(),
            options: Default::default(),
        };
        let p = Progress::default();
        copy().run(&p).unwrap();
        copy().run(&p).unwrap();
        assert_eq!(fs::read_to_string(dest.join("src/nested/a.txt")).unwrap(), "hello");
        assert!(dest.join("src (1)/nested/a.txt").exists());
        assert!(src.exists());
    }

    #[test]
    fn remaining_skips_finished_items_and_keeps_both_on_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("b.txt"), "old").unwrap();
        let vfs = local();
        let transfer = Transfer {
            mode: TransferMode::Copy,
            src: vfs.clone(),
            sources: vec![a.clone(), b.clone()],
            dst: vfs,
            dest_dir: dest.clone(),
            conflict: Conflict::KeepBoth,
            resolutions: Default::default(),
            targets: Default::default(),
            options: Default::default(),
        };
        let finished: TransferLog = Mutex::new(vec![(a.clone(), dest.join("a.txt"))]);
        assert_eq!(transfer.remaining(&finished), vec![(b, dest.join("b (1).txt"))]);
        assert!(!dest.join("b (1).txt").exists(), "nothing is changed");
    }

    #[test]
    fn moves_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.bin");
        fs::write(&file, [1u8; 10]).unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();
        let vfs = local();
        Transfer {
            mode: TransferMode::Move,
            src: vfs.clone(),
            sources: vec![file.clone()],
            dst: vfs,
            dest_dir: dest.clone(),
            conflict: Conflict::KeepBoth,
            resolutions: Default::default(),
            targets: Default::default(),
            options: Default::default(),
        }
        .run(&Progress::default())
        .unwrap();
        assert!(!file.exists());
        assert_eq!(fs::read(dest.join("f.bin")).unwrap().len(), 10);
    }

    #[test]
    fn refuses_copy_into_itself() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a");
        fs::create_dir_all(src.join("b")).unwrap();
        let vfs = local();
        let err = Transfer {
            mode: TransferMode::Copy,
            src: vfs.clone(),
            sources: vec![src.clone()],
            dst: vfs,
            dest_dir: src.join("b"),
            conflict: Conflict::KeepBoth,
            resolutions: Default::default(),
            targets: Default::default(),
            options: Default::default(),
        }
        .run(&Progress::default());
        assert!(err.is_err());
    }

    #[test]
    fn logs_transfers_and_relocates_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        fs::write(&file, "x").unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();
        let vfs = local();
        let log = TransferLog::default();
        Transfer {
            mode: TransferMode::Move,
            src: vfs.clone(),
            sources: vec![file.clone()],
            dst: vfs.clone(),
            dest_dir: dest.clone(),
            conflict: Conflict::KeepBoth,
            resolutions: Default::default(),
            targets: Default::default(),
            options: Default::default(),
        }
        .run_logged(&Progress::default(), &log)
        .unwrap();
        let log = log.into_inner().unwrap();
        assert_eq!(log, vec![(file.clone(), dest.join("f.txt"))]);
        relocate(&vfs, &log[0].1, &vfs, &log[0].0, &Progress::default()).unwrap();
        assert!(file.exists());
        assert!(!dest.join("f.txt").exists());
    }

    #[test]
    fn conflicts_are_reported_and_skip_keeps_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dest) = (dir.path().join("src"), dir.path().join("dest"));
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(src.join("a.txt"), "new").unwrap();
        fs::write(src.join("b.txt"), "new").unwrap();
        fs::write(dest.join("a.txt"), "old").unwrap();
        let vfs = local();
        let mut t = Transfer {
            mode: TransferMode::Copy,
            src: vfs.clone(),
            sources: vec![src.join("a.txt"), src.join("b.txt")],
            dst: vfs,
            dest_dir: dest.clone(),
            conflict: Conflict::Skip,
            resolutions: Default::default(),
            targets: Default::default(),
            options: Default::default(),
        };
        assert_eq!(t.conflicts(), vec!["a.txt".to_string()]);
        t.run(&Progress::default()).unwrap();
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "old");
        assert!(dest.join("b.txt").exists());
        t.conflict = Conflict::KeepBoth;
        t.sources.truncate(1);
        t.run(&Progress::default()).unwrap();
        assert!(dest.join("a (1).txt").exists());
    }

    #[test]
    fn resumes_into_the_earlier_destination() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("done.txt"), "complete").unwrap();
        fs::write(src.join("partial.txt"), "the whole file").unwrap();
        let dest = dir.path().join("dest");
        // The first run copied into `src (1)` and stopped midway.
        let earlier = dest.join("src (1)");
        fs::create_dir_all(&earlier).unwrap();
        fs::create_dir(dest.join("src")).unwrap();
        fs::write(earlier.join("done.txt"), "complete").unwrap();
        fs::write(earlier.join("partial.txt"), "the wh").unwrap();

        let vfs = local();
        let t = Transfer {
            mode: TransferMode::Copy,
            src: vfs.clone(),
            sources: vec![src.clone()],
            dst: vfs,
            dest_dir: dest.clone(),
            conflict: Conflict::KeepBoth,
            resolutions: Default::default(),
            targets: Arc::new(Mutex::new(vec![(src.clone(), earlier.clone())])),
            options: Default::default(),
        };
        let p = Progress::default();
        t.run(&p).unwrap();
        assert_eq!(fs::read_to_string(earlier.join("partial.txt")).unwrap(), "the whole file");
        assert_eq!(fs::read_to_string(earlier.join("done.txt")).unwrap(), "complete");
        assert!(!dest.join("src (2)").exists(), "no new copy");
        assert_eq!(fs::read_dir(dest.join("src")).unwrap().count(), 0, "the other folder is untouched");
        assert_eq!(p.done_bytes.load(Ordering::Relaxed), 22);
    }

    fn set_age(path: &Path, secs_ago: u64) {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        fs::File::options().write(true).open(path).unwrap().set_modified(when).unwrap();
    }

    #[test]
    fn overwrite_if_newer_compares_modification_times() {
        let dir = tempfile::tempdir().unwrap();
        let (new, old) = (dir.path().join("new"), dir.path().join("old"));
        fs::write(&new, "n").unwrap();
        fs::write(&old, "o").unwrap();
        set_age(&new, 10);
        set_age(&old, 1000);
        let (new, mut old) = (LocalVfs::entry_for(&new).unwrap(), LocalVfs::entry_for(&old).unwrap());
        assert_eq!(resolve(Conflict::OverwriteIfNewer, &new, &old), Conflict::Overwrite);
        assert_eq!(resolve(Conflict::OverwriteIfNewer, &old, &new), Conflict::Skip);
        assert_eq!(resolve(Conflict::KeepBoth, &old, &new), Conflict::KeepBoth);
        old.modified = None;
        assert_eq!(resolve(Conflict::OverwriteIfNewer, &new, &old), Conflict::Skip, "unknown times are not newer");
    }

    #[test]
    fn per_source_answers_override_the_batch_policy() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dest) = (dir.path().join("src"), dir.path().join("dest"));
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dest).unwrap();
        for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"] {
            fs::write(src.join(name), "new").unwrap();
            fs::write(dest.join(name), "old").unwrap();
        }
        fs::write(dest.join("b (1).txt"), "taken").unwrap();
        fs::write(src.join("fresh.txt"), "new").unwrap();
        set_age(&src.join("c.txt"), 1000);
        set_age(&src.join("d.txt"), 0);
        set_age(&dest.join("d.txt"), 1000);
        let vfs = local();
        let names = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt", "fresh.txt"];
        let mut transfer = Transfer {
            mode: TransferMode::Copy,
            src: vfs.clone(),
            sources: names.iter().map(|n| src.join(n)).collect(),
            dst: vfs,
            dest_dir: dest.clone(),
            conflict: Conflict::Skip,
            resolutions: Default::default(),
            targets: Default::default(),
            options: Default::default(),
        };
        assert_eq!(transfer.clashes().len(), 5);
        let answers = &mut transfer.resolutions.per_source;
        answers.insert(src.join("a.txt"), Conflict::Overwrite);
        answers.insert(src.join("b.txt"), Conflict::KeepBoth);
        answers.insert(src.join("c.txt"), Conflict::OverwriteIfNewer);
        answers.insert(src.join("d.txt"), Conflict::OverwriteIfNewer);
        // e.txt follows the batch policy (skip); fresh.txt has no clash.
        assert_eq!(
            transfer.remaining(&TransferLog::default()),
            vec![
                (src.join("a.txt"), dest.join("a.txt")),
                (src.join("b.txt"), dest.join("b (2).txt")),
                (src.join("d.txt"), dest.join("d.txt")),
                (src.join("fresh.txt"), dest.join("fresh.txt")),
            ]
        );

        // Running it without overwrites keeps both, skips and copies.
        transfer.resolutions.per_source.remove(&src.join("a.txt"));
        transfer.resolutions.per_source.remove(&src.join("d.txt"));
        transfer.run(&Progress::default()).unwrap();
        assert_eq!(fs::read_to_string(dest.join("b (2).txt")).unwrap(), "new");
        for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"] {
            assert_eq!(fs::read_to_string(dest.join(name)).unwrap(), "old", "{name}");
        }
        assert!(dest.join("fresh.txt").exists());
        assert!(transfer.resolutions.replaced.lock().unwrap().is_empty());
    }

    /// The local disk, but every file written comes out with its first
    /// byte changed: a copy that verification must catch.
    #[derive(Debug)]
    struct Corrupting;

    struct FlipFirst(Box<dyn Write + Send>, bool);

    impl Write for FlipFirst {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.1 || buf.is_empty() {
                return self.0.write(buf);
            }
            self.1 = true;
            let mut first = buf.to_vec();
            first[0] ^= 0xff;
            self.0.write_all(&first)?;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.flush()
        }
    }

    impl Vfs for Corrupting {
        fn scheme(&self) -> &'static str {
            "local"
        }
        fn label(&self) -> String {
            "corrupting".into()
        }
        fn is_local(&self) -> bool {
            true
        }
        fn home(&self) -> PathBuf {
            LocalVfs.home()
        }
        fn read_dir(&self, path: &Path) -> Result<Vec<Entry>> {
            LocalVfs.read_dir(path)
        }
        fn stat(&self, path: &Path) -> Result<Entry> {
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
        fn rename(&self, _from: &Path, _to: &Path) -> Result<()> {
            bail!("another filesystem")
        }
        fn reader(&self, path: &Path) -> Result<Box<dyn Read + Send>> {
            LocalVfs.reader(path)
        }
        fn writer(&self, path: &Path) -> Result<Box<dyn Write + Send>> {
            Ok(Box::new(FlipFirst(LocalVfs.writer(path)?, false)))
        }
    }

    fn verified(mode: TransferMode, src: VfsRef, sources: Vec<PathBuf>, dst: VfsRef, dest_dir: &Path) -> Transfer {
        Transfer {
            mode,
            src,
            sources,
            dst,
            dest_dir: dest_dir.to_path_buf(),
            conflict: Conflict::KeepBoth,
            resolutions: Default::default(),
            targets: Default::default(),
            options: CopyOptions { verify: true, ..Default::default() },
        }
    }

    #[test]
    fn verified_copies_read_the_copy_back() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), "hello").unwrap();
        fs::write(src.join("sub/b.txt"), "world!").unwrap();
        let dest = dir.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let p = Progress::default();
        verified(TransferMode::Copy, local(), vec![src.clone()], local(), &dest).run(&p).unwrap();
        assert_eq!(fs::read_to_string(dest.join("src/sub/b.txt")).unwrap(), "world!");
        assert_eq!(p.total_bytes.load(Ordering::Relaxed), 22, "reading back counts as much as copying");
        assert_eq!(p.done_bytes.load(Ordering::Relaxed), 22);
    }

    #[test]
    fn a_bad_copy_fails_the_move_and_keeps_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.bin");
        fs::write(&file, b"precious").unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();
        // A move to another filesystem (the rename fails): copy, then delete.
        let bad: VfsRef = Arc::new(Corrupting);
        let err = verified(TransferMode::Move, bad.clone(), vec![file.clone()], bad.clone(), &dest)
            .run(&Progress::default())
            .unwrap_err();
        assert!(format!("{err:#}").contains("verification failed"), "{err:#}");
        assert_eq!(fs::read(&file).unwrap(), b"precious", "the source is kept");
        assert!(!dest.join("f.bin").exists(), "the bad copy is removed");

        // Without verification the same copy goes through unnoticed.
        let mut unchecked = verified(TransferMode::Copy, local(), vec![file.clone()], bad, &dest);
        unchecked.options.verify = false;
        unchecked.run(&Progress::default()).unwrap();
        assert_ne!(fs::read(dest.join("f.bin")).unwrap(), b"precious");
    }

    #[test]
    fn verified_resumes_copy_again_files_that_differ() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("same.txt"), "complete").unwrap();
        fs::write(src.join("bad.txt"), "original").unwrap();
        let dest = dir.path().join("dest");
        let earlier = dest.join("src");
        fs::create_dir_all(&earlier).unwrap();
        fs::write(earlier.join("same.txt"), "complete").unwrap();
        fs::write(earlier.join("bad.txt"), "garbled!").unwrap();
        let t = verified(TransferMode::Copy, local(), vec![src.clone()], local(), &dest);
        t.targets.lock().unwrap().push((src.clone(), earlier.clone()));
        let p = Progress::default();
        t.run(&p).unwrap();
        assert_eq!(fs::read_to_string(earlier.join("bad.txt")).unwrap(), "original");
        assert_eq!(p.done_bytes.load(Ordering::Relaxed), p.total_bytes.load(Ordering::Relaxed));
    }

    #[test]
    fn bandwidth_limits_slow_copies_down() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.bin");
        fs::write(&file, vec![7u8; 300 * 1024]).unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();
        let mut t = verified(TransferMode::Copy, local(), vec![file], local(), &dest);
        t.options = CopyOptions { bandwidth_limit: Some(1 << 20), ..Default::default() };
        let started = std::time::Instant::now();
        t.run(&Progress::default()).unwrap();
        // 300 KiB at 1 MiB/s: about 0.3 s (the last chunk's wait included).
        assert!(started.elapsed() >= std::time::Duration::from_millis(250), "{:?}", started.elapsed());
        assert_eq!(fs::read(dest.join("f.bin")).unwrap().len(), 300 * 1024);
    }

    #[test]
    fn deletes_permanently() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("x");
        fs::create_dir_all(sub.join("y")).unwrap();
        delete(&local(), std::slice::from_ref(&sub), false, &Progress::default()).unwrap();
        assert!(!sub.exists());
    }
}
