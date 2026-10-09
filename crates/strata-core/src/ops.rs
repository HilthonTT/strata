//! File transfers and deletion that work across any pair of [`Vfs`] backends.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use anyhow::{bail, Context, Result};

use crate::jobs::Progress;
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
}

/// Copies or moves `sources` from `src` into `dest_dir` on `dst`.
pub struct Transfer {
    pub mode: TransferMode,
    pub src: VfsRef,
    pub sources: Vec<PathBuf>,
    pub dst: VfsRef,
    pub dest_dir: PathBuf,
    pub conflict: Conflict,
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
        progress.total_bytes.store(bytes, Ordering::Relaxed);
        progress.total_items.store(items, Ordering::Relaxed);

        let same = same_vfs(&self.src, &self.dst);
        for source in &self.sources {
            progress.check()?;
            let entry = self.src.stat(source)?;
            if same && self.dest_dir.starts_with(source) && entry.is_dir() {
                bail!("cannot {} a directory into itself", self.verb());
            }
            let Some(target) = self.target_for(&entry)? else {
                continue;
            };
            if same && self.mode == TransferMode::Move && self.src.rename(source, &target).is_ok() {
                progress.add_bytes(tree_size(&*self.src, &entry, progress).unwrap_or(0));
                record(source, &target);
                progress.item_done();
                continue;
            }
            copy_tree(&*self.src, &entry, &*self.dst, &target, progress)?;
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
        self.sources
            .iter()
            .filter_map(|source| {
                let name = source.file_name()?.to_string_lossy().into_owned();
                let target = self.dst.join(&self.dest_dir, &name);
                let onto_itself =
                    same_vfs(&self.src, &self.dst) && target == *source && self.mode == TransferMode::Move;
                (!onto_itself && self.dst.exists(&target)).then_some(name)
            })
            .collect()
    }

    /// Resolves the destination path for a top-level source, honouring the
    /// conflict policy. `None` means skip.
    fn target_for(&self, entry: &Entry) -> Result<Option<PathBuf>> {
        let target = self.dst.join(&self.dest_dir, &entry.name);
        if !self.dst.exists(&target) {
            return Ok(Some(target));
        }
        if same_vfs(&self.src, &self.dst) && target == entry.path && self.mode == TransferMode::Move {
            return Ok(None);
        }
        match self.conflict {
            Conflict::Skip => Ok(None),
            // Replaced items go to the trash where there is one.
            Conflict::Overwrite => {
                if self.dst.is_local() {
                    self.dst.trash(&target)?;
                } else {
                    self.dst.remove_all(&target)?;
                }
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
    progress.check()?;
    match entry.kind {
        EntryKind::Dir => {
            dst.create_dir(target)?;
            for child in src.read_dir(&entry.path)? {
                let child_target = dst.join(target, &child.name);
                copy_tree(src, &child, dst, &child_target, progress)?;
            }
        }
        EntryKind::Symlink { .. } if src.is_local() && dst.is_local() => {
            copy_local_symlink(&entry.path, target)?;
        }
        // Directory links on remote backends are skipped to avoid cycles.
        EntryKind::Symlink { to_dir: true } => {}
        EntryKind::File | EntryKind::Symlink { .. } => {
            progress.set_current(entry.name.clone());
            let mut reader = src.reader(&entry.path)?;
            let mut writer = dst.writer(target)?;
            stream(&mut *reader, &mut *writer, progress).with_context(|| format!("copy {}", entry.path.display()))?;
        }
        EntryKind::Other => {}
    }
    Ok(())
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

fn stream(reader: &mut dyn Read, writer: &mut dyn Write, progress: &Progress) -> Result<()> {
    let mut buf = vec![0u8; BUF_SIZE];
    loop {
        progress.check()?;
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        progress.add_bytes(n as u64);
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
#[cfg(any(
    target_os = "windows",
    all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
))]
pub fn restore_from_trash(paths: &[PathBuf], since: std::time::SystemTime) -> Result<usize> {
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
    if chosen.is_empty() {
        bail!("the items are no longer in the trash");
    }
    let count = chosen.len();
    trash::os_limited::restore_all(chosen).context("cannot restore from the trash")?;
    Ok(count)
}

#[cfg(not(any(
    target_os = "windows",
    all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
)))]
pub fn restore_from_trash(_paths: &[PathBuf], _since: std::time::SystemTime) -> Result<usize> {
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
        let t = Transfer {
            mode: TransferMode::Copy,
            src: vfs.clone(),
            sources: vec![src.clone()],
            dst: vfs,
            dest_dir: dest.clone(),
            conflict: Conflict::KeepBoth,
        };
        let p = Progress::default();
        t.run(&p).unwrap();
        t.run(&p).unwrap();
        assert_eq!(fs::read_to_string(dest.join("src/nested/a.txt")).unwrap(), "hello");
        assert!(dest.join("src (1)/nested/a.txt").exists());
        assert!(src.exists());
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
    fn deletes_permanently() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("x");
        fs::create_dir_all(sub.join("y")).unwrap();
        delete(&local(), std::slice::from_ref(&sub), false, &Progress::default()).unwrap();
        assert!(!sub.exists());
    }
}
