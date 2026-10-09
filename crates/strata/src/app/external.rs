//! Programs that take over the terminal: editors, shells, mount commands.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use strata_core::bulk;
use strata_core::VfsRef;
use strata_plugin::Level;

use super::App;
use crate::tui::{self, Term};

/// When to pause for Enter after an external command, so output stays visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    OnFailure,
    Always,
}

/// What to do once an external command finished successfully.
#[derive(Debug, Clone)]
pub enum After {
    Reload,
    Mounted(String),
    Unmounted,
}

pub enum External {
    Edit(Vec<PathBuf>),
    Shell {
        cwd: PathBuf,
        remote: Option<Vec<String>>,
    },
    BulkRename {
        vfs: VfsRef,
        dir: PathBuf,
        names: Vec<String>,
    },
    Run {
        argv: Vec<String>,
        cwd: Option<PathBuf>,
        wait: Wait,
        after: After,
    },
    /// A NAS mount, which may need a password on stdin or in a file.
    Mount {
        plan: strata_core::nas::MountPlan,
        after: After,
    },
}

/// File types better handled by the desktop than by a text editor.
const SYSTEM_OPEN: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg", "tiff", "heic", "mp4", "mkv", "webm", "mov", "avi",
    "mp3", "flac", "wav", "ogg", "m4a", "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp",
    "epub", "zip", "7z", "rar", "iso", "dmg", "exe", "msi", "appimage", "deb", "rpm",
];

pub fn prefers_system_open(ext: &str) -> bool {
    SYSTEM_OPEN.contains(&ext)
}

impl App {
    pub(super) fn editor(&self) -> Vec<String> {
        let configured = Some(self.config.general.editor.clone()).filter(|e| !e.trim().is_empty());
        let editor = configured
            .or_else(|| std::env::var("VISUAL").ok())
            .or_else(|| std::env::var("EDITOR").ok())
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| if cfg!(windows) { "notepad".into() } else { "vi".into() });
        strata_core::util::split_command(&editor)
    }

    pub(super) fn edit_targets(&mut self) {
        if !self.panel().vfs.is_local() {
            return self.notify("editing works on local files", Level::Warn);
        }
        let files: Vec<PathBuf> = self.panel().targets();
        if !files.is_empty() {
            self.external = Some(External::Edit(files));
        }
    }

    /// Opens `entry` with its `[open_with]` rule. Returns false if none applies.
    pub(super) fn open_with_rule(&mut self, entry: &strata_core::Entry) -> bool {
        let Some(rule) = self.config.open_with.get(&entry.extension()).cloned() else {
            return false;
        };
        let mut argv: Vec<String> = strata_core::util::split_command(rule.command());
        if argv.is_empty() {
            return false;
        }
        argv.push(entry.path.to_string_lossy().into_owned());
        let cwd = entry.path.parent().map(PathBuf::from);
        if rule.detach() {
            let mut cmd = Command::new(&argv[0]);
            cmd.args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            if let Some(cwd) = cwd {
                cmd.current_dir(cwd);
            }
            match cmd.spawn() {
                Ok(_) => self.info(format!("opened {} with {}", entry.name, argv[0])),
                Err(e) => self.error(format!("{}: {e}", argv[0])),
            }
        } else {
            self.external = Some(External::Run { argv, cwd, wait: Wait::OnFailure, after: After::Reload });
        }
        true
    }

    /// Opens the focused directory in `dir_editor` (or the editor).
    pub(super) fn edit_dir(&mut self) {
        if !self.panel().vfs.is_local() {
            return self.notify("editing works on local directories", Level::Warn);
        }
        let configured = strata_core::util::split_command(&self.config.general.dir_editor);
        let mut argv = if configured.is_empty() { self.editor() } else { configured };
        let cwd = self.panel().cwd.clone();
        argv.push(cwd.to_string_lossy().into_owned());
        self.external = Some(External::Run { argv, cwd: Some(cwd), wait: Wait::OnFailure, after: After::Reload });
    }

    pub(super) fn open_with_system(&mut self) {
        let Some(entry) = self.panel().hovered().cloned() else {
            return;
        };
        if !self.panel().vfs.is_local() {
            return self.notify("only local files can be opened with the system", Level::Warn);
        }
        if self.open_with_rule(&entry) {
            return;
        }
        let opener = Some(self.config.general.opener.clone()).filter(|o| !o.trim().is_empty());
        let mut parts: Vec<String> = match opener {
            Some(o) => strata_core::util::split_command(&o),
            None if cfg!(target_os = "macos") => vec!["open".into()],
            None if cfg!(windows) => vec!["cmd".into(), "/C".into(), "start".into(), String::new()],
            None => vec!["xdg-open".into()],
        };
        let program = parts.remove(0);
        let spawned = Command::new(&program)
            .args(parts)
            .arg(&entry.path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(_) => self.info(format!("opened {}", entry.name)),
            Err(e) => self.error(format!("{program}: {e}")),
        }
    }

    pub(super) fn open_shell(&mut self) {
        let p = self.panel();
        let remote = p.vfs.shell_command(&p.cwd);
        if !p.vfs.is_local() && remote.is_none() {
            return self.notify("no shell available for this location", Level::Warn);
        }
        self.external = Some(External::Shell { cwd: p.cwd.clone(), remote });
    }

    pub(super) fn bulk_rename(&mut self) {
        let p = self.panel();
        let names: Vec<String> =
            p.targets().iter().filter_map(|t| t.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
        if names.is_empty() {
            return;
        }
        self.external = Some(External::BulkRename { vfs: p.vfs.clone(), dir: p.cwd.clone(), names });
    }

    /// Runs a shell command line in the focused directory.
    pub(super) fn run_shell_line(&mut self, line: &str) {
        let argv = if cfg!(windows) {
            vec!["cmd".into(), "/C".into(), line.to_string()]
        } else {
            vec!["sh".into(), "-c".into(), line.to_string()]
        };
        let cwd = self.panel().vfs.is_local().then(|| self.panel().cwd.clone());
        self.external = Some(External::Run { argv, cwd, wait: Wait::Always, after: After::Reload });
    }

    pub(super) fn queue_external(&mut self, ext: External) {
        self.external = Some(ext);
    }

    pub(super) fn run_external(&mut self, terminal: &mut Term, ext: External) {
        match ext {
            External::Edit(files) => {
                let mut editor = self.editor();
                let program = editor.remove(0);
                let mut cmd = Command::new(&program);
                cmd.args(editor).args(&files);
                if let Some(dir) = files.first().and_then(|f| f.parent()) {
                    cmd.current_dir(dir);
                }
                if let Err(e) = tui::run_external(terminal, &mut cmd) {
                    self.error(format!("{program}: {e:#}"));
                }
                self.reload_all();
            }
            External::Shell { cwd, remote } => {
                let mut cmd = match remote {
                    Some(argv) => {
                        let mut c = Command::new(&argv[0]);
                        c.args(&argv[1..]);
                        c
                    }
                    None => {
                        let shell = std::env::var("SHELL").unwrap_or_else(|_| {
                            if cfg!(windows) {
                                "cmd".into()
                            } else {
                                "sh".into()
                            }
                        });
                        let mut c = Command::new(shell);
                        c.current_dir(&cwd).env("STRATA_SHELL", "1");
                        c
                    }
                };
                if let Err(e) = tui::run_external(terminal, &mut cmd) {
                    self.error(format!("shell: {e:#}"));
                }
                self.reload_all();
            }
            External::BulkRename { vfs, dir, names } => self.run_bulk_rename(terminal, vfs, dir, names),
            External::Mount { plan, after } => self.run_mount(terminal, plan, after),
            External::Run { argv, cwd, wait, after } => {
                let Some((program, args)) = argv.split_first() else {
                    return;
                };
                let mut cmd = Command::new(program);
                cmd.args(args);
                if let Some(cwd) = cwd {
                    cmd.current_dir(cwd);
                }
                let result = match wait {
                    Wait::Always => tui::run_external_and_wait(terminal, &mut cmd),
                    Wait::OnFailure => tui::run_external(terminal, &mut cmd),
                };
                match result {
                    Ok(status) if status.success() => self.after_external(after),
                    Ok(status) => {
                        self.error(format!("`{}` exited with {status}", argv.join(" ")));
                        if wait == Wait::OnFailure {
                            self.notify("see :messages for details", Level::Warn);
                        }
                    }
                    Err(e) => self.error(format!("{program}: {e:#}")),
                }
                self.reload_all();
            }
        }
    }

    fn run_mount(&mut self, terminal: &mut Term, plan: strata_core::nas::MountPlan, after: After) {
        let result = (|| -> anyhow::Result<std::process::ExitStatus> {
            // A private (0600) file that is deleted when this scope ends.
            let credentials = plan
                .credentials
                .as_ref()
                .map(|contents| -> anyhow::Result<tempfile::NamedTempFile> {
                    let mut file = tempfile::Builder::new().prefix("strata-cred-").tempfile()?;
                    use std::io::Write;
                    file.write_all(contents.as_bytes())?;
                    file.flush()?;
                    Ok(file)
                })
                .transpose()?;
            let argv: Vec<String> = plan
                .argv
                .iter()
                .map(|a| match &credentials {
                    Some(f) => a.replace(strata_core::nas::CREDENTIALS_FILE, &f.path().to_string_lossy()),
                    None => a.clone(),
                })
                .collect();
            let mut cmd = Command::new(&argv[0]);
            cmd.args(&argv[1..]);
            tui::run_external_with_input(terminal, &mut cmd, plan.stdin.as_deref())
        })();
        match result {
            Ok(status) if status.success() => self.after_external(after),
            Ok(status) => self.error(format!("mount exited with {status}")),
            Err(e) => self.error(format!("mount: {e:#}")),
        }
        self.reload_all();
    }

    fn after_external(&mut self, after: After) {
        match after {
            After::Reload => {}
            After::Mounted(name) => {
                self.probe_connections();
                let conn = self.config.connections.iter().find(|c| c.name == name).cloned();
                match conn.as_ref().and_then(|c| c.mounted_at()) {
                    Some(path) => {
                        self.info(format!("mounted {name}"));
                        self.cd_local(path);
                    }
                    None => self.notify(format!("{name}: mount finished but the share was not found"), Level::Warn),
                }
            }
            After::Unmounted => {
                self.probe_connections();
                self.info("unmounted");
            }
        }
    }

    fn run_bulk_rename(&mut self, terminal: &mut Term, vfs: VfsRef, dir: PathBuf, names: Vec<String>) {
        let result = (|| -> anyhow::Result<Vec<bulk::Rename>> {
            let mut file = tempfile::Builder::new().prefix("strata-rename-").suffix(".txt").tempfile()?;
            use std::io::Write;
            file.write_all(bulk::render(&names).as_bytes())?;
            file.flush()?;
            let mut editor = self.editor();
            let program = editor.remove(0);
            let status = tui::run_external(terminal, Command::new(program).args(editor).arg(file.path()))?;
            if !status.success() {
                anyhow::bail!("editor exited with {status}");
            }
            let edited = std::fs::read_to_string(file.path())?;
            let plan = bulk::plan(&names, &edited)?;
            bulk::apply(&vfs, &dir, &plan)?;
            Ok(plan)
        })();
        match result {
            Ok(plan) if plan.is_empty() => self.info("nothing renamed"),
            Ok(plan) => {
                self.info(format!("renamed {} item(s)", plan.len()));
                let label = format!("rename {} items", plan.len());
                self.push_undo(label, vec![super::undo::UndoOp::BulkRename { vfs, dir, plan }]);
            }
            Err(e) => self.error(format!("bulk rename: {e:#}")),
        }
        self.reload_all();
    }
}
