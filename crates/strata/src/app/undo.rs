//! Undo and redo for file operations: renames, creations, copies, moves
//! and trashing. Permanent deletes cannot be undone.
//!
//! Undoing an entry runs its steps backwards; the inverse of those steps
//! becomes a redo entry once the undo job succeeds, and the other way round.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{bail, Result};
use strata_core::bulk::{self, Rename};
use strata_core::jobs::{JobState, Progress};
use strata_core::ops::{self, TransferLog, TransferMode};
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
    },
    Trash {
        paths: Vec<PathBuf>,
        since: SystemTime,
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
            PendingUndo::Transfer { mode, src, dst, log } => {
                let done = log.lock().unwrap_or_else(|e| e.into_inner()).clone();
                done.into_iter()
                    .map(|(original, current)| match mode {
                        TransferMode::Copy => UndoOp::Created { vfs: dst.clone(), path: current },
                        TransferMode::Move => {
                            UndoOp::Moved { from_vfs: src.clone(), original, to_vfs: dst.clone(), current }
                        }
                    })
                    .collect()
            }
            PendingUndo::Trash { paths, since } if *state == JobState::Done => vec![UndoOp::Trashed { paths, since }],
            PendingUndo::Trash { .. } => Vec::new(),
            PendingUndo::Inverse { entry, to_redo } => {
                if *state == JobState::Done && !entry.ops.is_empty() {
                    let stack = if to_redo { &mut self.undo.redo } else { &mut self.undo.entries };
                    push_capped(stack, entry);
                }
                return;
            }
        };
        self.push_undo(label, ops);
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

fn push_capped(stack: &mut Vec<UndoEntry>, entry: UndoEntry) {
    stack.push(entry);
    if stack.len() > MAX_HISTORY {
        stack.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
