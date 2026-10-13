//! Undo and redo for file operations: renames, creations, copies, moves,
//! trashing and permission or owner changes. Permanent deletes cannot be
//! undone.
//!
//! Undoing an entry runs its steps backwards; the inverse of those steps
//! becomes a redo entry once the undo job succeeds, and the other way round.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{bail, Result};
use strata_core::bulk::{self, Rename};
use strata_core::jobs::{JobState, Progress};
use strata_core::ops::{self, TransferLog, TransferMode};
use strata_core::perm::{self, ChangeLog, Changes};
use strata_core::sync::SyncLog;
use strata_core::VfsRef;
use strata_plugin::Level;

use super::App;

const MAX_HISTORY: usize = 50;

/// One step needed to reverse an operation.
pub enum UndoOp {
    /// `from` was renamed to `to`.
    Rename {
        vfs: VfsRef,
        from: PathBuf,
        to: PathBuf,
    },
    BulkRename {
        vfs: VfsRef,
        dir: PathBuf,
        plan: Vec<Rename>,
    },
    /// `path` was created (new file, directory or copy).
    Created {
        vfs: VfsRef,
        path: PathBuf,
    },
    /// An item was moved from `original` to `current`.
    Moved {
        from_vfs: VfsRef,
        original: PathBuf,
        to_vfs: VfsRef,
        current: PathBuf,
    },
    Trashed {
        paths: Vec<PathBuf>,
        since: SystemTime,
    },
    /// Local items replaced by new versions; the old versions went to the
    /// trash after `since`.
    Replaced {
        vfs: VfsRef,
        paths: Vec<PathBuf>,
        since: SystemTime,
    },
    /// Permissions changed from `old` to `new`.
    Mode {
        vfs: VfsRef,
        changes: Changes<u32>,
    },
    /// Local owners changed from `old` to `new` (`(uid, gid)`).
    Owner {
        changes: Changes<(u32, u32)>,
    },
}

pub struct UndoEntry {
    pub label: String,
    pub ops: Vec<UndoOp>,
}

impl UndoOp {
    /// The step that reverses this step once it has been undone. `None`
    /// when that is impossible (remote items deleted without a trash).
    fn inverse(&self, local: &VfsRef, now: SystemTime) -> Vec<UndoOp> {
        match self {
            UndoOp::Rename { vfs, from, to } => {
                vec![UndoOp::Rename { vfs: vfs.clone(), from: to.clone(), to: from.clone() }]
            }
            UndoOp::BulkRename { vfs, dir, plan } => {
                vec![UndoOp::BulkRename { vfs: vfs.clone(), dir: dir.clone(), plan: bulk::reverse(plan) }]
            }
            // Undo trashed it, so redo restores it.
            UndoOp::Created { vfs, path } if vfs.is_local() => {
                vec![UndoOp::Trashed { paths: vec![path.clone()], since: now }]
            }
            UndoOp::Created { .. } => Vec::new(),
            UndoOp::Moved { from_vfs, original, to_vfs, current } => vec![UndoOp::Moved {
                from_vfs: to_vfs.clone(),
                original: current.clone(),
                to_vfs: from_vfs.clone(),
                current: original.clone(),
            }],
            // Undo restored them, so redo trashes them again.
            UndoOp::Trashed { paths, .. } => {
                paths.iter().map(|p| UndoOp::Created { vfs: local.clone(), path: p.clone() }).collect()
            }
            // Undo trashed the new versions, so redo swaps them back.
            UndoOp::Replaced { vfs, paths, .. } => {
                vec![UndoOp::Replaced { vfs: vfs.clone(), paths: paths.clone(), since: now }]
            }
            UndoOp::Mode { vfs, changes } => vec![UndoOp::Mode {
                vfs: vfs.clone(),
                changes: changes.iter().map(|(p, old, new)| (p.clone(), *new, *old)).collect(),
            }],
            UndoOp::Owner { changes } => {
                vec![UndoOp::Owner { changes: changes.iter().map(|(p, old, new)| (p.clone(), *new, *old)).collect() }]
            }
        }
    }
}

impl UndoEntry {
    /// The entry that reverses this one after it has run.
    fn inverse(&self, local: &VfsRef) -> UndoEntry {
        let now = SystemTime::now();
        // Steps run last-to-first, so the inverse lists them reversed.
        let ops = self.ops.iter().rev().flat_map(|op| op.inverse(local, now)).collect();
        UndoEntry { label: self.label.clone(), ops }
    }

    fn run(self, progress: &Progress) -> Result<()> {
        for op in self.ops.into_iter().rev() {
            progress.check()?;
            match op {
                UndoOp::Rename { vfs, from, to } => {
                    if vfs.exists(&from) {
                        bail!("{} already exists", from.display());
                    }
                    vfs.rename(&to, &from)?;
                }
                UndoOp::BulkRename { vfs, dir, plan } => bulk::apply(&vfs, &dir, &bulk::reverse(&plan))?,
                // Remove what was created, through the trash where possible.
                UndoOp::Created { vfs, path } => match vfs.is_local() {
                    true => vfs.trash(&path)?,
                    false => vfs.remove_all(&path)?,
                },
                UndoOp::Moved { from_vfs, original, to_vfs, current } => {
                    ops::relocate(&to_vfs, &current, &from_vfs, &original, progress)?
                }
                UndoOp::Trashed { paths, since } => {
                    ops::restore_from_trash(&paths, since)?;
                }
                UndoOp::Replaced { vfs, paths, since } => {
                    ops::swap_with_trashed(&*vfs, &paths, since)?;
                }
                UndoOp::Mode { vfs, changes } => {
                    let modes: Vec<_> = changes.iter().map(|(path, old, _)| (path.clone(), *old)).collect();
                    perm::set_modes(&*vfs, &modes)?;
                }
                UndoOp::Owner { changes } => {
                    for (path, (uid, gid), _) in changes.iter().rev() {
                        perm::set_owner(path, *uid, *gid)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// An operation running as a job; becomes an undo entry when it ends.
pub enum PendingUndo {
    Transfer {
        mode: TransferMode,
        src: VfsRef,
        dst: VfsRef,
        log: Arc<TransferLog>,
        /// Destinations that were overwritten, and when the job started.
        replaced: Arc<Mutex<Vec<PathBuf>>>,
        since: SystemTime,
    },
    Trash {
        paths: Vec<PathBuf>,
        since: SystemTime,
    },
    Mode {
        vfs: VfsRef,
        log: Arc<ChangeLog<u32>>,
    },
    Owner {
        log: Arc<ChangeLog<(u32, u32)>>,
    },
    /// A sync into `vfs`, started at `since`.
    Sync {
        vfs: VfsRef,
        since: SystemTime,
        log: Arc<Mutex<SyncLog>>,
    },
    /// An undo (or redo) job; on success its inverse goes on the other stack.
    Inverse {
        entry: UndoEntry,
        to_redo: bool,
    },
}

#[derive(Default)]
pub struct UndoHistory {
    entries: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    pending: HashMap<u64, (String, PendingUndo)>,
}

impl App {
    /// Records a new operation. Anything that could be redone is dropped,
    /// as it no longer follows from the current state.
    pub(super) fn push_undo(&mut self, label: impl Into<String>, ops: Vec<UndoOp>) {
        if ops.is_empty() {
            return;
        }
        self.undo.redo.clear();
        push_capped(&mut self.undo.entries, UndoEntry { label: label.into(), ops });
    }

    /// Remembers a job so it can be undone once it finishes.
    pub(super) fn track_job(&mut self, job: u64, label: String, pending: PendingUndo) {
        self.undo.pending.insert(job, (label, pending));
    }

    /// Turns a finished job into an undo entry, including the part of a
    /// failed or cancelled job that did happen.
    pub(super) fn job_finished_for_undo(&mut self, job: u64, state: &JobState) {
        let Some((label, pending)) = self.undo.pending.remove(&job) else { return };
        let ops = match pending {
            PendingUndo::Transfer { mode, src, dst, log, replaced, since } => {
                let done = log.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let replaced = replaced.lock().unwrap_or_else(|e| e.into_inner()).clone();
                transfer_ops(mode, &src, &dst, done, &replaced, since)
            }
            PendingUndo::Trash { paths, since } if *state == JobState::Done => vec![UndoOp::Trashed { paths, since }],
            PendingUndo::Trash { .. } => Vec::new(),
            // Like transfers, whatever part of the job happened is undoable.
            PendingUndo::Mode { vfs, log } => {
                let changes = std::mem::take(&mut *log.lock().unwrap_or_else(|e| e.into_inner()));
                (!changes.is_empty()).then(|| UndoOp::Mode { vfs, changes }).into_iter().collect()
            }
            PendingUndo::Owner { log } => {
                let changes = std::mem::take(&mut *log.lock().unwrap_or_else(|e| e.into_inner()));
                (!changes.is_empty()).then(|| UndoOp::Owner { changes }).into_iter().collect()
            }
            // Replaced and deleted items are only recoverable from a trash.
            PendingUndo::Sync { vfs, since, log } => {
                let log = std::mem::take(&mut *log.lock().unwrap_or_else(|e| e.into_inner()));
                let mut ops: Vec<UndoOp> =
                    log.created.into_iter().map(|path| UndoOp::Created { vfs: vfs.clone(), path }).collect();
                if vfs.is_local() && !log.trashed.is_empty() {
                    ops.push(UndoOp::Trashed { paths: log.trashed, since });
                }
                if vfs.is_local() && !log.replaced.is_empty() {
                    ops.push(UndoOp::Replaced { vfs, paths: log.replaced, since });
                }
                ops
            }
            PendingUndo::Inverse { entry, to_redo } => {
                if *state == JobState::Done {
                    // The inverse describes the state undo or redo left behind.
                    self.follow_tags(&entry.ops);
                }
                if *state == JobState::Done && !entry.ops.is_empty() {
                    let stack = if to_redo { &mut self.undo.redo } else { &mut self.undo.entries };
                    push_capped(stack, entry);
                }
                return;
            }
        };
        self.follow_tags(&ops);
        self.push_undo(label, ops);
    }

    /// Moves tags along with items that were renamed or moved locally.
    fn follow_tags(&mut self, ops: &[UndoOp]) {
        for op in ops {
            match op {
                UndoOp::Rename { vfs, from, to } if vfs.is_local() => self.tags.moved(from, to),
                UndoOp::BulkRename { vfs, dir, plan } if vfs.is_local() => {
                    let moves: Vec<_> = plan.iter().map(|r| (dir.join(&r.from), dir.join(&r.to))).collect();
                    self.tags.moved_all(&moves);
                }
                UndoOp::Moved { from_vfs, original, to_vfs, current } if from_vfs.is_local() && to_vfs.is_local() => {
                    self.tags.moved(original, current)
                }
                _ => {}
            }
        }
    }

    pub(super) fn undo(&mut self) {
        let Some(entry) = self.undo.entries.pop() else {
            return self.notify("nothing to undo", Level::Warn);
        };
        let inverse = entry.inverse(&self.local);
        let label = format!("Undo {}", entry.label);
        let job = self.jobs.spawn(label.clone(), move |progress| entry.run(progress));
        self.track_job(job, label, PendingUndo::Inverse { entry: inverse, to_redo: true });
    }

    pub(super) fn redo(&mut self) {
        let Some(entry) = self.undo.redo.pop() else {
            return self.notify("nothing to redo", Level::Warn);
        };
        let inverse = entry.inverse(&self.local);
        let label = format!("Redo {}", entry.label);
        let job = self.jobs.spawn(label.clone(), move |progress| entry.run(progress));
        self.track_job(job, label, PendingUndo::Inverse { entry: inverse, to_redo: false });
    }
}

/// The steps undoing a transfer that finished `done`. Destinations it
/// overwrote on a local disk went to the trash and come back from there.
fn transfer_ops(
    mode: TransferMode,
    src: &VfsRef,
    dst: &VfsRef,
    done: Vec<(PathBuf, PathBuf)>,
    replaced: &[PathBuf],
    since: SystemTime,
) -> Vec<UndoOp> {
    let mut ops = Vec::new();
    for (original, current) in done {
        let overwrote = dst.is_local() && replaced.contains(&current);
        match mode {
            TransferMode::Copy if overwrote => {
                ops.push(UndoOp::Replaced { vfs: dst.clone(), paths: vec![current], since })
            }
            TransferMode::Copy => ops.push(UndoOp::Created { vfs: dst.clone(), path: current }),
            TransferMode::Move => {
                // Steps run last-to-first: the item moves back, then the
                // version it replaced comes out of the trash.
                if overwrote {
                    ops.push(UndoOp::Trashed { paths: vec![current.clone()], since });
                }
                ops.push(UndoOp::Moved { from_vfs: src.clone(), original, to_vfs: dst.clone(), current });
            }
        }
    }
    ops
}

fn push_capped(stack: &mut Vec<UndoEntry>, entry: UndoEntry) {
    stack.push(entry);
    if stack.len() > MAX_HISTORY {
        stack.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use strata_core::vfs::LocalVfs;

    #[test]
    fn rename_and_move_inverses_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, "x").unwrap();
        std::fs::rename(&a, &b).unwrap();
        let vfs: VfsRef = Arc::new(LocalVfs);
        let entry = UndoEntry {
            label: "rename".into(),
            ops: vec![UndoOp::Rename { vfs: vfs.clone(), from: a.clone(), to: b.clone() }],
        };
        let redo = entry.inverse(&vfs);
        entry.run(&Progress::default()).unwrap();
        assert!(a.exists() && !b.exists(), "undo renames back");
        redo.run(&Progress::default()).unwrap();
        assert!(b.exists() && !a.exists(), "redo renames again");
    }

    #[test]
    fn moves_invert_both_ways() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("out")).unwrap();
        let (orig, moved) = (dir.path().join("f"), dir.path().join("out/f"));
        std::fs::write(&moved, "x").unwrap();
        let vfs: VfsRef = Arc::new(LocalVfs);
        let op = UndoOp::Moved {
            from_vfs: vfs.clone(),
            original: orig.clone(),
            to_vfs: vfs.clone(),
            current: moved.clone(),
        };
        let entry = UndoEntry { label: "move".into(), ops: vec![op] };
        let redo = entry.inverse(&vfs);
        entry.run(&Progress::default()).unwrap();
        assert!(orig.exists() && !moved.exists());
        redo.run(&Progress::default()).unwrap();
        assert!(moved.exists() && !orig.exists());
    }

    #[test]
    fn overwritten_destinations_come_back_from_the_trash() {
        let vfs: VfsRef = Arc::new(LocalVfs);
        let done = vec![("/s/a".into(), "/d/a".into()), ("/s/b".into(), "/d/b".into())];
        let replaced = [PathBuf::from("/d/b")];
        let now = SystemTime::now();
        let copy = transfer_ops(TransferMode::Copy, &vfs, &vfs, done.clone(), &replaced, now);
        assert!(matches!(&copy[0], UndoOp::Created { path, .. } if path == Path::new("/d/a")));
        assert!(matches!(&copy[1], UndoOp::Replaced { paths, .. } if paths == &replaced));
        let moved = transfer_ops(TransferMode::Move, &vfs, &vfs, done, &replaced, now);
        assert_eq!(moved.len(), 3);
        // Run last-to-first: b moves back before its old version is restored.
        assert!(matches!(&moved[1], UndoOp::Trashed { paths, .. } if paths == &replaced));
        assert!(matches!(&moved[2], UndoOp::Moved { current, .. } if current == Path::new("/d/b")));
    }

    #[cfg(unix)]
    #[test]
    fn mode_changes_invert_both_ways() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        std::fs::write(&f, "x").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        let vfs: VfsRef = Arc::new(LocalVfs);
        let mode = || std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        let entry = UndoEntry {
            label: "chmod".into(),
            ops: vec![UndoOp::Mode { vfs: vfs.clone(), changes: vec![(f.clone(), 0o644, 0o755)] }],
        };
        let redo = entry.inverse(&vfs);
        entry.run(&Progress::default()).unwrap();
        assert_eq!(mode(), 0o644, "undo restores the old mode");
        redo.run(&Progress::default()).unwrap();
        assert_eq!(mode(), 0o755, "redo applies the new mode again");
    }
}
