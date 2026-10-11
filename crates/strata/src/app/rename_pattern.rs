//! Renaming the marked items with one pattern (`g r` / `:rename-pattern`),
//! with a live preview of the new names. See [`strata_core::pattern`].

use std::path::PathBuf;
use std::time::SystemTime;

use strata_core::bulk;
use strata_core::pattern::{self, Item, Pattern};
use strata_core::VfsRef;
use strata_plugin::Level;

use super::overlay::{InputPurpose, InputState, Overlay, RenamePreview};
use super::undo::UndoOp;
use super::App;

/// Rows of the preview worked out per keystroke; the rest is counted.
const PREVIEW_ROWS: usize = 200;

/// The items being renamed, in the order they are listed.
pub struct PatternTargets {
    pub vfs: VfsRef,
    pub items: Vec<(PathBuf, Option<SystemTime>)>,
}

impl PatternTargets {
    fn items(&self) -> Vec<Item<'_>> {
        self.items.iter().map(|(path, modified)| Item { path, modified: *modified }).collect()
    }
}

impl App {
    /// Opens the pattern prompt, optionally pre-filled.
    pub(super) fn rename_pattern(&mut self, initial: &str) {
        if !self.ensure_writable() {
            return;
        }
        let entries = self.panel().targets_in_order();
        if entries.is_empty() {
            return;
        }
        let targets = PatternTargets {
            vfs: self.panel().vfs.clone(),
            items: entries.into_iter().map(|e| (e.path, e.modified)).collect(),
        };
        let prompt = format!(
            "Rename {} item(s) · {{name}} {{.ext}} {{n:3}} {{date}} {{name|lower}} · s/find/repl/gi",
            targets.items.len()
        );
        let mut input = InputState::new(prompt, initial, InputPurpose::RenamePattern(targets));
        update_preview(&mut input);
        self.overlay = Some(Overlay::Input(input));
    }

    /// Enter in the pattern prompt: checks the plan against the disk, then
    /// renames everything, folder by folder, as one undo step.
    pub(super) fn apply_rename_pattern(&mut self, targets: PatternTargets, value: &str) {
        let result = Pattern::parse(value).and_then(|p| {
            let vfs = targets.vfs.clone();
            pattern::plan(&p, &targets.items(), |path| vfs.exists(path))
        });
        let plan = match result {
            Ok(plan) => plan,
            Err(e) => return self.error(format!("rename: {e:#}")),
        };
        let count: usize = plan.values().map(Vec::len).sum();
        if count == 0 {
            return self.info("nothing to rename: every name stays the same");
        }
        let vfs = targets.vfs;
        let mut ops = Vec::new();
        let mut failed = None;
        // Deepest folders first: renaming a folder before what is inside
        // it would leave those paths stale.
        for (dir, renames) in plan.into_iter().rev() {
            if let Err(e) = bulk::apply(&vfs, &dir, &renames) {
                failed = Some(format!("{}: {e:#}", dir.display()));
                break;
            }
            ops.push(UndoOp::BulkRename { vfs: vfs.clone(), dir, plan: renames });
        }
        let done: usize = ops
            .iter()
            .map(|op| match op {
                UndoOp::BulkRename { plan, .. } => plan.len(),
                _ => 0,
            })
            .sum();
        self.push_undo(format!("rename {done} item(s)"), ops);
        match failed {
            Some(e) if done > 0 => self.error(format!("renamed {done} of {count} item(s), then failed: {e}")),
            Some(e) => self.error(format!("rename: {e}")),
            None => self.info(format!("renamed {done} item(s)")),
        }
        self.panel_mut().clear_marks();
        self.reload_all();
    }

    /// `:rename-pattern <pattern>` renames without the prompt.
    pub(super) fn rename_pattern_command(&mut self, args: &str) {
        if args.is_empty() {
            return self.rename_pattern("");
        }
        if !self.ensure_writable() {
            return;
        }
        let entries = self.panel().targets_in_order();
        if entries.is_empty() {
            return self.notify("nothing to rename", Level::Warn);
        }
        let targets = PatternTargets {
            vfs: self.panel().vfs.clone(),
            items: entries.into_iter().map(|e| (e.path, e.modified)).collect(),
        };
        self.apply_rename_pattern(targets, args);
    }
}

/// Recomputes the preview under the pattern prompt after an edit.
pub(super) fn update_preview(input: &mut InputState) {
    let InputPurpose::RenamePattern(targets) = &input.purpose else { return };
    let total = targets.items.len();
    let preview = match Pattern::parse(&input.value) {
        Err(e) => RenamePreview { rows: Vec::new(), error: Some(format!("{e:#}")), total },
        Ok(p) => {
            let items = targets.items();
            let rows = items
                .iter()
                .take(PREVIEW_ROWS)
                .enumerate()
                .map(|(i, item)| {
                    let old = item.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    (old, p.apply(item, i))
                })
                .collect();
            // Collisions inside the set are checked here; the disk only on enter.
            let error = pattern::plan(&p, &items, |_| false).err().map(|e| format!("{e:#}"));
            RenamePreview { rows, error, total }
        }
    };
    input.preview = Some(preview);
}
