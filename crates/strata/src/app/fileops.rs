//! File operations beyond copy, move and delete.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::bail;
use strata_core::compare::{self, FileDiff};
use strata_core::ops::{Conflict, Transfer, TransferMode};
use strata_core::perm::{self, ChangeLog, ModeSpec, Owner};
use strata_core::util::unique_name;
use strata_core::vfs::same_vfs;
use strata_core::VfsRef;
use strata_plugin::Level;

use super::actions::short_path;
use super::external::{After, External, Wait};
use super::overlay::{InputPurpose, InputState, Overlay};
use super::undo::{PendingUndo, UndoOp};
use super::App;
use crate::event::AppEvent;

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

    pub(super) fn prompt_chmod(&mut self) {
        let paths = self.panel().targets();
        let Some(first) = self.panel().hovered().filter(|_| !paths.is_empty()) else { return };
        let current = first.mode.map(|m| format!("{m:o}")).unwrap_or_default();
        let what = match paths.as_slice() {
            [_] => format!("'{}'", first.name),
            many => format!("{} items", many.len()),
        };
        let prompt = format!("Permissions for {what} (644, u+x, go-w; -R for subfolders)");
        let purpose = InputPurpose::Chmod { vfs: self.panel().vfs.clone(), paths };
        self.overlay = Some(Overlay::Input(InputState::new(prompt, current, purpose)));
    }

    /// Applies a mode like `755`, `u+x` or `-R go-w` to `paths`.
    pub(super) fn start_chmod(&mut self, vfs: VfsRef, paths: Vec<PathBuf>, input: &str) {
        let (recursive, spec) = split_recursive(input);
        let spec = match ModeSpec::parse(spec) {
            Ok(spec) => spec,
            Err(e) => return self.error(format!("{e:#}")),
        };
        let label = format!("chmod {} {}", spec_text(input), describe(&paths));
        let log = Arc::new(ChangeLog::default());
        let job_log = log.clone();
        let job_vfs = vfs.clone();
        let job = self.jobs.spawn(label.clone(), move |progress| {
            perm::change_mode(&*job_vfs, &paths, &spec, recursive, progress, &job_log)
        });
        self.panel_mut().clear_marks();
        self.track_job(job, label, PendingUndo::Mode { vfs, log });
    }

    /// `:chown user[:group]` on the marked local items.
    pub(super) fn chown_command(&mut self, args: &str) {
        if !self.panel().vfs.is_local() {
            return self.notify("chown works on local files", Level::Warn);
        }
        let (recursive, spec) = split_recursive(args);
        let owner = match Owner::parse(spec) {
            Ok(owner) => owner,
            Err(e) => return self.error(format!("{e:#}")),
        };
        let paths = self.panel().targets();
        if paths.is_empty() {
            return;
        }
        let label = format!("chown {} {}", spec_text(args), describe(&paths));
        let log = Arc::new(ChangeLog::default());
        let job_log = log.clone();
        let job = self
            .jobs
            .spawn(label.clone(), move |progress| perm::change_owner(&paths, owner, recursive, progress, &job_log));
        self.panel_mut().clear_marks();
        self.track_job(job, label, PendingUndo::Owner { log });
    }

    // --- compare --------------------------------------------------------------

    /// Compares two marked items, or the hovered item with its namesake (or
    /// the hovered item) in the next panel.
    pub(super) fn compare(&mut self) {
        let p = self.panel();
        let marked = if p.marked.is_empty() { Vec::new() } else { p.target_entries() };
        let (left, right) = match marked.as_slice() {
            [a, b] => ((p.vfs.clone(), a.path.clone()), (p.vfs.clone(), b.path.clone())),
            [] | [_] if self.panels.len() > 1 => {
                let Some(entry) = marked.first().or(p.hovered()) else { return };
                let other = &self.panels[(self.active + 1) % self.panels.len()];
                let namesake = other.vfs.join(&other.cwd, &entry.name);
                let right = if other.hovered().is_some_and(|e| e.name == entry.name) || other.vfs.exists(&namesake) {
                    namesake
                } else if let Some(e) = other.hovered() {
                    e.path.clone()
                } else {
                    return self.notify("nothing to compare with in the next panel", Level::Warn);
                };
                ((p.vfs.clone(), entry.path.clone()), (other.vfs.clone(), right))
            }
            _ => return self.notify("mark two items, or open a second panel to compare with", Level::Warn),
        };
        self.compare_paths(left, right);
    }

    fn compare_paths(&mut self, (av, a): (VfsRef, PathBuf), (bv, b): (VfsRef, PathBuf)) {
        let tool = strata_core::util::split_command(&self.config.general.diff_tool);
        let both_files = av.is_local() && bv.is_local() && a.is_file() && b.is_file();
        if !tool.is_empty() && both_files {
            let mut argv = tool;
            argv.push(a.to_string_lossy().into_owned());
            argv.push(b.to_string_lossy().into_owned());
            let cwd = a.parent().map(Path::to_path_buf);
            return self.queue_external(External::Run { argv, cwd, wait: Wait::OnFailure, after: After::Reload });
        }
        let (la, lb) = (location(&av, &a), location(&bv, &b));
        let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
        let label = format!("Compare {} ↔ {}", name(&a), name(&b));
        let tx = self.tx.clone();
        self.jobs.spawn(label, move |progress| {
            let (ea, eb) = (av.stat(&a)?, bv.stat(&b)?);
            let lines = match (ea.is_dir(), eb.is_dir()) {
                (true, true) => {
                    let report = compare::compare_dirs((&*av, &a), (&*bv, &b), progress)?;
                    dir_report_lines(&la, &lb, &report)
                }
                (false, false) => match compare::diff_files((&*av, &a), (&*bv, &b), (&la, &lb), progress)? {
                    FileDiff::Identical => {
                        vec![format!("✓ {la}"), format!("✓ {lb}"), String::new(), "The files are identical.".into()]
                    }
                    FileDiff::Differ { reason } => {
                        vec![format!("- {la}"), format!("+ {lb}"), String::new(), format!("~ {reason}")]
                    }
                    FileDiff::Text(lines) => lines,
                },
                _ => bail!("cannot compare a file with a directory"),
            };
            let title = format!("Compare · {} ↔ {}", name(&a), name(&b));
            let _ = tx.send(AppEvent::Report { title, lines });
            Ok(())
        });
    }
}

/// `-R spec` → `(true, spec)`.
fn split_recursive(input: &str) -> (bool, &str) {
    let input = input.trim();
    match input.strip_prefix("-R").or_else(|| input.strip_prefix("-r")) {
        Some(rest) if rest.starts_with(char::is_whitespace) => (true, rest.trim()),
        _ => (false, input),
    }
}

fn spec_text(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn describe(paths: &[PathBuf]) -> String {
    match paths {
        [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        many => format!("{} items", many.len()),
    }
}

/// `~/dir/file` for local paths, `sftp:me@nas:/dir/file` for others.
fn location(vfs: &VfsRef, path: &Path) -> String {
    if vfs.is_local() {
        short_path(path)
    } else {
        format!("{}:{}", vfs.label(), strata_core::util::posix(path))
    }
}

fn dir_report_lines(left: &str, right: &str, report: &compare::DirReport) -> Vec<String> {
    let mut lines = vec![
        format!("- {left}"),
        format!("+ {right}"),
        String::new(),
        format!(
            "{} identical · {} differ · {} only left · {} only right",
            report.same,
            report.differ.len(),
            report.only_left.len(),
            report.only_right.len()
        ),
        String::new(),
    ];
    if report.is_identical() {
        lines.push("✓ The directories are identical.".into());
    }
    lines.extend(report.differ.iter().map(|(name, why)| format!("~ {name}  ({why})")));
    lines.extend(report.only_left.iter().map(|name| format!("- {name}")));
    lines.extend(report.only_right.iter().map(|name| format!("+ {name}")));
    lines
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

    #[test]
    fn recursive_flag_is_split_off() {
        assert_eq!(split_recursive("-R go-w"), (true, "go-w"));
        assert_eq!(split_recursive("  755 "), (false, "755"));
        assert_eq!(split_recursive("-Rx"), (false, "-Rx"));
    }
}
