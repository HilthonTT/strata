//! Undo for file operations: renames, creations, copies, moves and trashing.
//! Permanent deletes cannot be undone.

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

impl UndoEntry {
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
    Transfer { mode: TransferMode, src: VfsRef, dst: VfsRef, log: Arc<TransferLog> },
    Trash { paths: Vec<PathBuf>, since: SystemTime },
}

#[derive(Default)]
pub struct UndoHistory {
    entries: Vec<UndoEntry>,
    pending: HashMap<u64, (String, PendingUndo)>,
}

impl App {
    pub(super) fn push_undo(&mut self, label: impl Into<String>, ops: Vec<UndoOp>) {
        if ops.is_empty() {
            return;
        }
        let history = &mut self.undo.entries;
        history.push(UndoEntry { label: label.into(), ops });
        if history.len() > MAX_HISTORY {
            history.remove(0);
        }
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
        };
        self.push_undo(label, ops);
    }

    pub(super) fn undo(&mut self) {
        let Some(entry) = self.undo.entries.pop() else {
            return self.notify("nothing to undo", Level::Warn);
        };
        let label = format!("Undo {}", entry.label);
        self.jobs.spawn(label, move |progress| entry.run(progress));
    }
}
