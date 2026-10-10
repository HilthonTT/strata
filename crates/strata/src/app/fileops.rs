//! File operations beyond copy, move and delete: links, duplicates,
//! permissions and owners, comparing, checksums, the trash and archives.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::bail;
use strata_core::compare::{self, FileDiff};
use strata_core::inspect::{self, HashAlgo};
use strata_core::ops::{Conflict, Transfer, TransferMode};
use strata_core::perm::{self, ChangeLog, ModeSpec, Owner};
use strata_core::trash::{self, TrashedItem};
use strata_core::util::unique_name;
use strata_core::vfs::{same_vfs, ArchiveFormat, ArchiveVfs};
use strata_core::VfsRef;
use strata_plugin::Level;

use super::actions::short_path;
use super::external::{After, External, Wait};
use super::overlay::{Confirm, ConfirmState, InputPurpose, InputState, Overlay, PickerPurpose, PickerState};
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
    /// Warns and returns false when the focused panel cannot be changed.
    pub(super) fn ensure_writable(&mut self) -> bool {
        if self.panel().vfs.read_only() {
            self.notify("archives are read-only: copy items out of them instead", Level::Warn);
            return false;
        }
        true
    }

    // --- duplicate & links ----------------------------------------------------

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

    // --- permissions & owners -------------------------------------------------

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

    // --- checksums ------------------------------------------------------------

    /// Shows the checksums of the targets. With an algorithm (`:checksum
    /// sha256`) it also copies the hovered file's checksum to the clipboard.
    pub(super) fn checksum(&mut self, algo: Option<&str>) {
        let algo = match algo.map(str::trim).filter(|a| !a.is_empty()) {
            None => None,
            Some(name) => match parse_algo(name) {
                Some(a) => Some(a),
                None => return self.error(format!("unknown checksum '{name}' (md5, sha1, sha256, sha512)")),
            },
        };
        let vfs = self.panel().vfs.clone();
        let files: Vec<(String, PathBuf)> = match algo {
            Some(_) => self.panel().hovered().filter(|e| !e.is_dir()).map(|e| vec![(e.name.clone(), e.path.clone())]),
            None => Some(
                self.panel().target_entries().into_iter().filter(|e| !e.is_dir()).map(|e| (e.name, e.path)).collect(),
            ),
        }
        .unwrap_or_default();
        if files.is_empty() {
            return self.notify("checksums need a file", Level::Warn);
        }
        let tx = self.tx.clone();
        let what = describe(&files.iter().map(|(_, p)| p.clone()).collect::<Vec<_>>());
        let label = match algo {
            Some(algo) => format!("Copy the {} of {what}", algo.name()),
            None => format!("Checksum {what}"),
        };
        self.jobs.spawn(label, move |progress| {
            if let Some(algo) = algo {
                let (name, path) = &files[0];
                let sum = inspect::hash_file(&*vfs, path, &[algo], progress)?.remove(0);
                let message = format!("copied the {} of {name}", algo.name());
                let _ = tx.send(AppEvent::Clipboard { text: sum, message });
                return Ok(());
            }
            let algos = [HashAlgo::Md5, HashAlgo::Sha1, HashAlgo::Sha256];
            let mut lines = Vec::new();
            for (name, path) in &files {
                let sums = inspect::hash_file(&*vfs, path, &algos, progress)?;
                lines.push(name.clone());
                for (algo, sum) in algos.iter().zip(sums) {
                    lines.push(format!("  {:<8} {sum}", algo.name()));
                }
                lines.push(String::new());
            }
            lines.push("SHA-512: :checksum sha512 copies it · :verify checks against a hash or checksum file".into());
            let _ = tx.send(AppEvent::Report { title: "Checksums".into(), lines });
            Ok(())
        });
    }

    /// `:verify <hash>` checks the hovered file; `:verify` on a checksum
    /// file (`SHA256SUMS`, `*.sha256`…) checks every file it lists.
    pub(super) fn verify(&mut self, args: &str) {
        let Some(entry) = self.panel().hovered().filter(|e| !e.is_dir()).cloned() else {
            return self.notify("hover a file to verify", Level::Warn);
        };
        let (vfs, dir) = (self.panel().vfs.clone(), self.panel().cwd.clone());
        let tx = self.tx.clone();
        let expected = args.trim().to_ascii_lowercase();
        if !expected.is_empty() {
            let Some(algo) =
                HashAlgo::from_hex_len(expected.len()).filter(|_| expected.chars().all(|c| c.is_ascii_hexdigit()))
            else {
                return self.error("not an MD5, SHA-1, SHA-256 or SHA-512 hash");
            };
            // The job's own outcome is the verdict: "✓ …" or "… failed: …".
            let label = format!("Verify {} against the {} hash", entry.name, algo.name());
            self.jobs.spawn(label, move |progress| {
                let sum = inspect::hash_file(&*vfs, &entry.path, &[algo], progress)?.remove(0);
                if sum != expected {
                    bail!("{} does NOT match: its {} is {sum}", entry.name, algo.name());
                }
                Ok(())
            });
            return;
        }
        if !inspect::is_checksum_file(&entry.name) {
            return self.notify("hover a checksum file (SHA256SUMS, *.sha256…) or use :verify <hash>", Level::Warn);
        }
        self.jobs.spawn(format!("Verify {}", entry.name), move |progress| {
            let mut text = String::new();
            vfs.reader(&entry.path)?.take(4 * 1024 * 1024).read_to_string(&mut text)?;
            // `app.iso.sha256` holding just a digest is about `app.iso`.
            let default = entry.name.rsplit_once('.').map(|(stem, _)| stem.to_string());
            let sums = inspect::parse_sums(&text, default.as_deref());
            if sums.is_empty() {
                bail!("no checksums found in {}", entry.name);
            }
            let (mut ok, mut bad) = (0, 0);
            let mut lines = Vec::new();
            for line in &sums {
                progress.check()?;
                let path = line.file.split('/').fold(dir.clone(), |p, part| vfs.join(&p, part));
                match inspect::hash_file(&*vfs, &path, &[line.algo], progress) {
                    Ok(sum) if sum[0] == line.hash => {
                        ok += 1;
                        lines.push(format!("✓ {}", line.file));
                    }
                    Ok(_) => {
                        bad += 1;
                        lines.push(format!("✗ {}  ({} mismatch)", line.file, line.algo.name()));
                    }
                    Err(e) => {
                        bad += 1;
                        lines.push(format!("? {}  ({e:#})", line.file));
                    }
                }
            }
            let summary = if bad == 0 {
                format!("✓ all {ok} file(s) match")
            } else {
                format!("✗ {bad} of {} failed", ok + bad)
            };
            lines.insert(0, summary);
            lines.insert(1, String::new());
            let _ = tx.send(AppEvent::Report { title: format!("Verify · {}", entry.name), lines });
            Ok(())
        });
    }

    // --- trash ----------------------------------------------------------------

    pub(super) fn open_trash(&mut self) {
        if !trash::SUPPORTED {
            return self.notify("browsing the trash is not supported on this platform", Level::Warn);
        }
        let items = match trash::list() {
            Ok(items) => items,
            Err(e) => return self.error(format!("{e:#}")),
        };
        if items.is_empty() {
            return self.info("the trash is empty");
        }
        let width = items.iter().map(|i| i.name.chars().count()).max().unwrap_or(0).min(40);
        let format = &self.config.general.date_format;
        let rows = items
            .iter()
            .map(|i| {
                let when = chrono::DateTime::<chrono::Local>::from(i.deleted).format(format);
                format!("{:<width$}  {when}  {}", i.name, short_path(&i.original))
            })
            .collect();
        let title = format!("Trash ({}) · enter restore · ctrl+d delete · ctrl+e empty", items.len());
        self.overlay = Some(Overlay::Picker(PickerState::new(title, rows, PickerPurpose::Trash { items })));
    }

    pub(super) fn restore_trashed(&mut self, item: TrashedItem) {
        let path = item.original_path();
        match trash::restore(vec![item]) {
            Ok(()) => {
                let local = self.local.clone();
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.push_undo(format!("restore {name}"), vec![UndoOp::Created { vfs: local, path: path.clone() }]);
                self.reload_all();
                self.reveal(path.clone());
                self.info(format!("restored {}", short_path(&path)));
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    /// Picker keys beyond enter and esc: `ctrl+d` deletes the selected trash
    /// item for good and `ctrl+e` empties the trash.
    pub(super) fn trash_picker_key(&mut self, ctrl_char: char) -> bool {
        let Some(Overlay::Picker(picker)) = &self.overlay else { return false };
        let PickerPurpose::Trash { items } = &picker.purpose else { return false };
        let confirm = match ctrl_char {
            'd' => {
                let Some(item) = picker.selected_index().and_then(|i| items.get(i)).cloned() else { return true };
                let message = format!("Delete '{}' from the trash for good? This cannot be undone.", item.name);
                ConfirmState { message, action: Confirm::PurgeTrash(vec![item]) }
            }
            'e' => {
                let message = format!("Empty the trash ({} items)? This cannot be undone.", items.len());
                ConfirmState { message, action: Confirm::EmptyTrash }
            }
            _ => return false,
        };
        self.overlay = Some(Overlay::Confirm(confirm));
        true
    }

    pub(super) fn purge_trash(&mut self, items: Option<Vec<TrashedItem>>) {
        let result = match items {
            Some(items) => {
                let n = items.len();
                trash::purge(items).map(|()| n)
            }
            None => trash::empty(),
        };
        match result {
            Ok(n) => self.info(format!("deleted {n} item(s) from the trash")),
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    // --- archives -------------------------------------------------------------

    /// True if `name` is an archive that `open` should browse.
    pub(super) fn is_browsable_archive(&self, name: &str) -> bool {
        self.config.general.browse_archives && ArchiveFormat::detect(name).is_some()
    }

    /// Indexes an archive in the background, then shows it in the panel.
    pub(super) fn open_archive(&mut self, file: PathBuf) {
        if !self.panel().vfs.is_local() {
            return self.notify("copy the archive to a local folder to browse it", Level::Warn);
        }
        let (tx, panel) = (self.tx.clone(), self.panel().id);
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.info(format!("reading {name}…"));
        std::thread::spawn(move || {
            let result = ArchiveVfs::open(&file).map(|v| Arc::new(v) as VfsRef).map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::ArchiveOpened { panel, file, result });
        });
    }

    pub(super) fn on_archive_opened(&mut self, panel: u64, file: PathBuf, result: Result<VfsRef, String>) {
        let vfs = match result {
            Ok(vfs) => vfs,
            Err(e) => return self.error(e),
        };
        // Only if the panel is still where the archive was opened.
        let Some(p) = self.panels.iter_mut().find(|p| p.id == panel) else { return };
        if !p.vfs.is_local() || Some(p.cwd.as_path()) != file.parent() {
            return;
        }
        p.switch_vfs(vfs, PathBuf::from("/"));
        self.invalidate_preview();
    }

    /// Leaves an archive for the directory that holds it, with the cursor on
    /// it. False if the focused panel is not at an archive's root.
    pub(super) fn leave_archive(&mut self) -> bool {
        let Some(file) = self.panel().vfs.container() else { return false };
        let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else { return false };
        let name = name.to_string_lossy().into_owned();
        let local = self.local.clone();
        self.panel_mut().switch_vfs(local, dir.to_path_buf());
        self.panel_mut().focus_name(&name);
        true
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

fn parse_algo(name: &str) -> Option<HashAlgo> {
    match name.to_ascii_lowercase().replace('-', "").as_str() {
        "md5" => Some(HashAlgo::Md5),
        "sha1" => Some(HashAlgo::Sha1),
        "sha256" => Some(HashAlgo::Sha256),
        "sha512" => Some(HashAlgo::Sha512),
        _ => None,
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

    #[test]
    fn algorithms_parse_with_or_without_dashes() {
        assert_eq!(parse_algo("SHA-256"), Some(HashAlgo::Sha256));
        assert_eq!(parse_algo("md5"), Some(HashAlgo::Md5));
        assert_eq!(parse_algo("crc32"), None);
    }
}
