//! Editing files on SFTP servers and in Docker containers: download a
//! copy, open it in the editor, and upload it back if it changed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use strata_core::jobs::JobState;
use strata_core::ops;
use strata_core::vfs::LocalVfs;
use strata_core::{Entry, VfsRef};
use strata_plugin::Level;

use super::external::External;
use super::App;
use crate::tui::{self, Term};

/// A remote file checked out into a private temporary folder.
pub struct RemoteEdit {
    vfs: VfsRef,
    remote: PathBuf,
    local: PathBuf,
    /// Deleted when the edit is finished (or abandoned).
    _dir: tempfile::TempDir,
}

#[derive(Default)]
pub struct RemoteEdits {
    /// Downloads in progress, by job id.
    downloading: HashMap<u64, RemoteEdit>,
}

impl App {
    /// Downloads a remote file, then opens it in the editor.
    pub(super) fn edit_remote(&mut self, entry: &Entry) {
        if entry.is_dir() {
            return;
        }
        let dir = match tempfile::Builder::new().prefix("strata-edit-").tempdir() {
            Ok(d) => d,
            Err(e) => return self.error(format!("cannot create a temporary folder: {e}")),
        };
        let local = dir.path().join(&entry.name);
        let edit =
            RemoteEdit { vfs: self.panel().vfs.clone(), remote: entry.path.clone(), local: local.clone(), _dir: dir };
        let (vfs, remote) = (edit.vfs.clone(), entry.clone());
        let job = self.jobs.spawn(format!("Download {}", entry.name), move |progress| {
            ops::copy_tree(&*vfs, &remote, &LocalVfs, &local, progress)
        });
        self.remote_edits.downloading.insert(job, edit);
    }

    /// Opens the editor once a download finished.
    pub(super) fn remote_download_finished(&mut self, job: u64, state: &JobState) {
        let Some(edit) = self.remote_edits.downloading.remove(&job) else { return };
        if *state == JobState::Done {
            self.queue_external(External::EditRemote(edit));
        }
    }

    pub(super) fn run_remote_edit(&mut self, terminal: &mut Term, edit: RemoteEdit) {
        let before = std::fs::read(&edit.local).ok();
        let mut editor = self.editor();
        let program = editor.remove(0);
        if let Err(e) = tui::run_external(terminal, Command::new(&program).args(editor).arg(&edit.local)) {
            return self.error(format!("{program}: {e:#}"));
        }
        let after = std::fs::read(&edit.local).ok();
        if after.is_none() || after == before {
            return self.notify("no changes to upload", Level::Info);
        }
        if edit.vfs.read_only() {
            return self.notify("archives are read-only: your changes were not saved", Level::Warn);
        }
        let name = edit.remote.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.jobs.spawn(format!("Upload {name}"), move |progress| {
            let entry = LocalVfs::entry_for(&edit.local)?;
            // `edit` (and its temporary folder) lives until the upload ends.
            ops::copy_tree(&LocalVfs, &entry, &*edit.vfs, &edit.remote, progress)
        });
    }
}
