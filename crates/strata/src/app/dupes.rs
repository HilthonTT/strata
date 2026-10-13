//! `:dupes`: finds duplicate files below the focused directory and lists
//! them in a picker; `:dupes keep oldest|newest|shortest` trashes every
//! other copy.

use std::path::PathBuf;

use strata_core::dupes::{self, DupeGroup, DupeReport, Keep};
use strata_core::util::human_size;
use strata_plugin::Level;

use super::actions::short_path;
use super::overlay::{Confirm, ConfirmState, Overlay, PickerPurpose, PickerState};
use super::App;
use crate::event::AppEvent;

/// The result of the last scan.
pub struct DupeScan {
    root: PathBuf,
    groups: Vec<DupeGroup>,
}

impl App {
    /// `:dupes` scans; `:dupes keep <which>` trashes the extra copies.
    pub(super) fn dupes_command(&mut self, args: &str) {
        match args.split_whitespace().collect::<Vec<_>>().as_slice() {
            [] => self.find_dupes(),
            ["keep", which] => match Keep::parse(which) {
                Some(keep) => self.keep_dupes(keep),
                None => self.error("usage: dupes keep oldest|newest|shortest"),
            },
            _ => self.error("usage: dupes [keep oldest|newest|shortest]"),
        }
    }

    fn find_dupes(&mut self) {
        if !self.panel().vfs.is_local() {
            return self.notify("finding duplicates works on local folders", Level::Warn);
        }
        let root = self.panel().cwd.clone();
        let show_hidden = self.panel().show_hidden;
        let tx = self.tx.clone();
        self.jobs.spawn(format!("Find duplicates in {}", short_path(&root)), move |progress| {
            let report = dupes::find(&root, show_hidden, progress)?;
            let _ = tx.send(AppEvent::Dupes { root, report });
            Ok(())
        });
    }

    pub(super) fn on_dupes_found(&mut self, root: PathBuf, report: DupeReport) {
        let unreadable = match report.unreadable {
            0 => String::new(),
            n => format!(", {n} unreadable"),
        };
        if report.groups.is_empty() {
            self.dupes = None;
            let where_ = short_path(&root);
            return self.info(format!("no duplicates in {where_} ({} files checked{unreadable})", report.scanned));
        }
        if !unreadable.is_empty() {
            self.notify(format!("{} files checked{unreadable}", report.scanned), Level::Warn);
        }
        self.dupes = Some(DupeScan { root, groups: report.groups });
        self.show_dupes(0);
    }

    /// Opens the picker on the last scan, with the cursor on `row`.
    fn show_dupes(&mut self, row: usize) {
        let Some(scan) = &self.dupes else { return };
        let wasted: u64 = scan.groups.iter().map(DupeGroup::wasted).sum();
        let mut rows = Vec::new();
        let mut paths = Vec::new();
        for (i, group) in scan.groups.iter().enumerate() {
            for file in &group.files {
                let rel = file.path.strip_prefix(&scan.root).unwrap_or(&file.path);
                rows.push(format!("#{:<4} {:>10}  {}", i + 1, human_size(group.size), rel.display()));
                paths.push(file.path.clone());
            }
        }
        let title = format!(
            "Duplicates · {} groups, {} extra · ctrl+d trash · :dupes keep oldest|newest|shortest",
            scan.groups.len(),
            human_size(wasted)
        );
        let mut picker = PickerState::new(title, rows, PickerPurpose::Dupes { paths });
        picker.move_by(row.min(picker.matches.len().saturating_sub(1)) as isize);
        // Refreshing the open picker (after ctrl+d) replaces it.
        if let Some(Overlay::Picker(PickerState { purpose: PickerPurpose::Dupes { .. }, .. })) = &self.overlay {
            self.overlay = None;
        }
        self.show_overlay(Overlay::Picker(picker));
    }

    /// `ctrl+d` in the duplicates picker trashes the highlighted copy.
    pub(super) fn dupes_picker_key(&mut self, ctrl_char: char) -> bool {
        let Some(Overlay::Picker(picker)) = &self.overlay else { return false };
        let PickerPurpose::Dupes { paths } = &picker.purpose else { return false };
        if ctrl_char != 'd' {
            return false;
        }
        let Some(path) = picker.selected_index().and_then(|i| paths.get(i)).cloned() else { return true };
        if !self.config.general.confirm_delete {
            self.trash_dupe(path);
            return true;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let message = if self.config.general.use_trash {
            format!("Move '{name}' to the trash?")
        } else {
            format!("Permanently delete '{name}'? This cannot be undone.")
        };
        self.overlay = Some(Overlay::Confirm(ConfirmState { message, action: Confirm::TrashDupe(path) }));
        true
    }

    /// Deletes one copy, then shows the remaining duplicates again.
    pub(super) fn trash_dupe(&mut self, path: PathBuf) {
        let row = self.dupes_row(&path).unwrap_or(0);
        self.start_delete(self.local.clone(), vec![path.clone()], !self.config.general.use_trash);
        if let Some(scan) = &mut self.dupes {
            scan.groups.iter_mut().for_each(|g| g.files.retain(|f| f.path != path));
            scan.groups.retain(|g| g.files.len() > 1);
            if scan.groups.is_empty() {
                self.dupes = None;
                return;
            }
        }
        self.show_dupes(row);
    }

    fn dupes_row(&self, path: &PathBuf) -> Option<usize> {
        self.dupes.as_ref()?.groups.iter().flat_map(|g| &g.files).position(|f| &f.path == path)
    }

    /// Trashes all but one copy in each group of the last scan. Files that
    /// changed or disappeared since the scan are left alone.
    fn keep_dupes(&mut self, keep: Keep) {
        let Some(scan) = &self.dupes else {
            return self.notify("no duplicates found yet: run :dupes first", Level::Warn);
        };
        let (groups, changed) = unchanged_groups(&scan.groups);
        let paths = keep.others(&groups);
        if paths.is_empty() {
            return self.info("nothing to delete: the files changed since the scan, run :dupes again");
        }
        let bytes: u64 = groups.iter().map(DupeGroup::wasted).sum();
        let which = match keep {
            Keep::Oldest => "the oldest copy",
            Keep::Newest => "the newest copy",
            Keep::Shortest => "the copy with the shortest path",
        };
        let permanent = !self.config.general.use_trash;
        let verb = if permanent { "Permanently delete" } else { "Trash" };
        let mut message = format!(
            "{verb} {} duplicate(s) ({}), keeping {which} in each of {} groups?",
            paths.len(),
            human_size(bytes),
            groups.len()
        );
        if changed > 0 {
            message.push_str(&format!(" {changed} file(s) changed since the scan and are left alone."));
        }
        let action = Confirm::Delete { vfs: self.local.clone(), paths, permanent };
        self.overlay = Some(Overlay::Confirm(ConfirmState { message, action }));
    }
}

/// The groups without the files that changed or disappeared since the
/// scan (dropping groups left with one file), and how many files that was.
fn unchanged_groups(groups: &[DupeGroup]) -> (Vec<DupeGroup>, usize) {
    let mut kept = Vec::new();
    let mut changed = 0;
    for group in groups {
        let files: Vec<_> = group
            .files
            .iter()
            .filter(|f| {
                std::fs::symlink_metadata(&f.path)
                    .is_ok_and(|m| m.is_file() && m.len() == group.size && m.modified().ok() == f.modified)
            })
            .cloned()
            .collect();
        changed += group.files.len() - files.len();
        if files.len() > 1 {
            kept.push(DupeGroup { size: group.size, files });
        }
    }
    (kept, changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use strata_core::dupes::DupeFile;

    #[test]
    fn changed_groups_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, "same").unwrap();
        std::fs::write(&b, "same").unwrap();
        let file = |p: &PathBuf| DupeFile { path: p.clone(), modified: std::fs::metadata(p).unwrap().modified().ok() };
        let groups = vec![DupeGroup { size: 4, files: vec![file(&a), file(&b)] }];
        assert_eq!(unchanged_groups(&groups).0.len(), 1);
        std::fs::write(&b, "different").unwrap();
        assert_eq!(unchanged_groups(&groups), (Vec::new(), 1));
        let c = dir.path().join("c");
        std::fs::write(&c, "same").unwrap();
        let groups = vec![DupeGroup { size: 4, files: vec![file(&a), file(&c), groups[0].files[1].clone()] }];
        let (kept, changed) = unchanged_groups(&groups);
        assert_eq!((kept[0].files.len(), changed), (2, 1));
    }
}
