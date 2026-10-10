//! File operations beyond copy, move and delete.

use strata_core::ops::{Conflict, Transfer, TransferMode};

use super::App;

impl App {
    /// Copies the targets next to themselves as `name (1).ext`.
    pub(super) fn duplicate(&mut self) {
        let sources = self.panel().targets();
        if sources.is_empty() {
            return;
        }
        let vfs = self.panel().vfs.clone();
        let transfer = Transfer {
            mode: TransferMode::Copy,
            src: vfs.clone(),
            sources,
            dst: vfs,
            dest_dir: self.panel().cwd.clone(),
            conflict: Conflict::KeepBoth,
        };
        self.panel_mut().clear_marks();
        self.start_transfer(transfer);
    }
}
