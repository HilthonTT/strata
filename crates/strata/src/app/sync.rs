//! `:sync`: one-way sync of the focused panel's directory into the next
//! panel's. The plan is worked out in the background, then confirmed (or
//! shown, with `--dry-run`) before anything changes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use strata_core::sync::{self, SyncLog, SyncOptions, SyncPlan, SyncStep};
use strata_core::util::human_size;
use strata_core::vfs::same_vfs;
use strata_core::VfsRef;
use strata_plugin::Level;

use super::fileops::location;
use super::overlay::{Confirm, ConfirmState, Overlay, TextPopup};
use super::undo::PendingUndo;
use super::App;
use crate::event::AppEvent;

/// A planned sync from `src_dir` into `dst_dir`.
pub struct SyncRequest {
    pub src: VfsRef,
    pub src_dir: PathBuf,
    pub dst: VfsRef,
    pub dst_dir: PathBuf,
    pub opts: SyncOptions,
    pub plan: SyncPlan,
    pub dry_run: bool,
}

impl SyncRequest {
    fn title(&self) -> String {
        format!("{} → {}", location(&self.src, &self.src_dir), location(&self.dst, &self.dst_dir))
    }

    /// The command that applies this plan.
    fn command(&self) -> String {
        let mut cmd = String::from(":sync");
        if self.opts.mirror {
            cmd.push_str(" --mirror");
        }
        if self.opts.newer_only {
            cmd.push_str(" --newer");
        }
        if self.opts.copy.verify {
            cmd.push_str(" --verify");
        }
        cmd
    }
}

impl App {
    /// `:sync [--mirror] [--newer] [--verify] [--dry-run]`
    pub(super) fn sync_command(&mut self, args: &str) {
        let mut opts = SyncOptions::default();
        opts.copy.verify = self.config.general.verify_copies;
        opts.copy.bandwidth_limit = self.bandwidth_limit();
        let mut dry_run = false;
        for arg in args.split_whitespace() {
            match arg {
                "--mirror" | "-m" => opts.mirror = true,
                "--newer" | "-n" => opts.newer_only = true,
                "--verify" | "-v" => opts.copy.verify = true,
                "--dry-run" | "-d" => dry_run = true,
                _ => return self.error("usage: sync [--mirror] [--newer] [--verify] [--dry-run]"),
            }
        }
        if self.panels.len() < 2 {
            return self.notify("open a second panel (n) to sync into", Level::Warn);
        }
        let other = &self.panels[(self.active + 1) % self.panels.len()];
        let (src, src_dir) = (self.panel().vfs.clone(), self.panel().cwd.clone());
        let (dst, dst_dir) = (other.vfs.clone(), other.cwd.clone());
        if dst.read_only() {
            return self.notify("archives are read-only: sync into a folder instead", Level::Warn);
        }
        if same_vfs(&src, &dst) && (src_dir.starts_with(&dst_dir) || dst_dir.starts_with(&src_dir)) {
            return self.error("cannot sync a folder with itself, or with a folder inside it");
        }
        let tx = self.tx.clone();
        let label = format!("Plan sync {} → {}", name(&src_dir), name(&dst_dir));
        self.jobs.spawn(label, move |progress| {
            let plan = sync::plan((&*src, &src_dir), (&*dst, &dst_dir), opts, progress)?;
            let request = SyncRequest { src, src_dir, dst, dst_dir, opts, plan, dry_run };
            let _ = tx.send(AppEvent::SyncPlanned(Box::new(request)));
            Ok(())
        });
    }

    pub(super) fn on_sync_planned(&mut self, request: SyncRequest) {
        let kept = request.plan.kept_newer.len();
        if request.plan.is_empty() {
            let newer = if kept > 0 { format!(" ({kept} newer file(s) left alone)") } else { String::new() };
            return self.info(format!("already in sync: {}{newer}", request.title()));
        }
        if request.dry_run {
            let title = format!("Sync plan · {}", request.title());
            let lines = plan_lines(&request);
            return self.show_overlay(Overlay::Text(TextPopup { title, lines, scroll: 0, colored: true }));
        }
        let mut message = format!("Sync {}? {}.", request.title(), summary(&request.plan));
        let (_, replace, delete) = request.plan.counts();
        if replace + delete > 0 {
            message.push_str(if request.dst.is_local() {
                " Replaced and deleted items go to the trash."
            } else {
                " Replaced and deleted items are removed for good."
            });
        }
        self.show_overlay(Overlay::Confirm(ConfirmState { message, action: Confirm::Sync(Box::new(request)) }));
    }

    pub(super) fn start_sync(&mut self, request: SyncRequest) {
        let label = format!("Sync {} → {}", name(&request.src_dir), name(&request.dst_dir));
        let undo_label = format!("sync {} → {}", name(&request.src_dir), name(&request.dst_dir));
        let log = Arc::new(Mutex::new(SyncLog::default()));
        let pending = PendingUndo::Sync { vfs: request.dst.clone(), since: SystemTime::now(), log: log.clone() };
        let SyncRequest { src, src_dir, dst, dst_dir, plan, opts, .. } = request;
        let job = self.jobs.spawn(label, move |progress| {
            sync::apply(&plan, (&*src, &src_dir), (&*dst, &dst_dir), opts, progress, &log)
        });
        self.track_job(job, undo_label, pending);
    }
}

/// `copy 3, replace 1 and delete 2 (12 MiB)`
fn summary(plan: &SyncPlan) -> String {
    let (copy, replace, delete) = plan.counts();
    let parts: Vec<String> = [(copy, "copy"), (replace, "replace"), (delete, "delete")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, verb)| format!("{verb} {n}"))
        .collect();
    let mut text = match parts.as_slice() {
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
        [] => String::new(),
    };
    if plan.bytes > 0 {
        text.push_str(&format!(" ({})", human_size(plan.bytes)));
    }
    text
}

/// The plan as report lines: `+` copy, `~` replace, `-` delete.
fn plan_lines(request: &SyncRequest) -> Vec<String> {
    let plan = &request.plan;
    let mut lines = vec![
        format!("from {}", location(&request.src, &request.src_dir)),
        format!("into {}", location(&request.dst, &request.dst_dir)),
        String::new(),
        format!("{}. Nothing has changed yet: run {} to apply.", capitalize(&summary(plan)), request.command()),
        String::new(),
    ];
    lines.extend(plan.steps.iter().map(|step| match step {
        SyncStep::Copy(rel) => format!("+ {rel}"),
        SyncStep::Replace(rel) => format!("~ {rel}"),
        SyncStep::Delete(rel) => format!("- {rel}"),
    }));
    if !plan.kept_newer.is_empty() {
        lines.push(String::new());
        lines.push(format!("Left alone, newer in the destination ({}):", plan.kept_newer.len()));
        lines.extend(plan.kept_newer.iter().map(|rel| format!("  {rel}")));
    }
    lines
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_list_only_what_happens() {
        let plan = SyncPlan {
            steps: vec![SyncStep::Copy("a".into()), SyncStep::Copy("b".into()), SyncStep::Delete("c".into())],
            bytes: 2048,
            kept_newer: Vec::new(),
        };
        assert_eq!(summary(&plan), "copy 2 and delete 1 (2.0 KiB)");
        let plan = SyncPlan { steps: vec![SyncStep::Replace("a".into())], ..Default::default() };
        assert_eq!(summary(&plan), "replace 1");
    }
}
