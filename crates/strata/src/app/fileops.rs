//! File operations beyond copy, move and delete.

use std::path::{Component, Path, PathBuf};

use strata_core::ops::{Conflict, Transfer, TransferMode};
use strata_core::util::unique_name;
use strata_core::vfs::same_vfs;
use strata_plugin::Level;

use super::undo::UndoOp;
use super::App;

/// How `paste_links` links to the clipboard items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Absolute,
    Relative,
    Hard,
}

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

    /// Creates links in the focused directory to the clipboard items.
    pub(super) fn paste_links(&mut self, kind: LinkKind) {
        let Some(clip) = self.clipboard.as_ref() else {
            return self.notify("clipboard is empty — mark items and press y y first", Level::Warn);
        };
        let (vfs, cwd) = (self.panel().vfs.clone(), self.panel().cwd.clone());
        if !same_vfs(&clip.vfs, &vfs) {
            return self.error("links must be on the same filesystem as what they point to");
        }
        let sources = clip.paths.clone();
        let mut created = Vec::new();
        let mut errors = Vec::new();
        for source in &sources {
            let Some(name) = source.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
            let name = unique_name(&name, |n| vfs.exists(&vfs.join(&cwd, n)));
            let link = vfs.join(&cwd, &name);
            let result = match kind {
                LinkKind::Absolute => vfs.symlink(source, &link),
                LinkKind::Relative => vfs.symlink(&relative_path(source, &cwd), &link),
                LinkKind::Hard => vfs.hard_link(source, &link),
            };
            match result {
                Ok(()) => created.push((name, link)),
                Err(e) => errors.push(format!("{e:#}")),
            }
        }
        self.panel_mut().reload();
        if let Some((name, _)) = created.first() {
            let name = name.clone();
            self.panel_mut().focus_name(&name);
        }
        let count = created.len();
        let ops = created.into_iter().map(|(_, path)| UndoOp::Created { vfs: vfs.clone(), path }).collect();
        let what = if kind == LinkKind::Hard { "hard link" } else { "link" };
        self.push_undo(format!("{what} {count} item(s)"), ops);
        match errors.first() {
            Some(e) => self.error(e.clone()),
            None => self.info(format!("created {count} {what}(s)")),
        }
    }
}

/// `target` relative to the directory `base`, e.g. `../lib/x` for
/// `/a/lib/x` from `/a/bin`. Both must be absolute.
fn relative_path(target: &Path, base: &Path) -> PathBuf {
    let t: Vec<Component> = target.components().collect();
    let b: Vec<Component> = base.components().collect();
    let common = t.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if common == 0 {
        return target.to_path_buf();
    }
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for part in &t[common..] {
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_climb_to_the_common_parent() {
        assert_eq!(relative_path(Path::new("/a/lib/x"), Path::new("/a/bin")), PathBuf::from("../lib/x"));
        assert_eq!(relative_path(Path::new("/a/b/c"), Path::new("/a/b")), PathBuf::from("c"));
        assert_eq!(relative_path(Path::new("/x"), Path::new("/a/b")), PathBuf::from("../../x"));
    }
}
