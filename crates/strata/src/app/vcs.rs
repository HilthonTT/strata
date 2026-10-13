//! Git status per file, refreshed in the background for every local panel,
//! and git actions on the marked files (the `;` menu and `:git`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use strata_core::git::{self, GitState, GitStatus};

use super::overlay::{Confirm, ConfirmState, InputPurpose, InputState, Overlay, PickerPurpose, PickerState, TextPopup};
use super::App;
use crate::event::AppEvent;

const MAX_AGE: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct GitCache {
    /// Status by panel directory; `None` when it is not in a repository.
    by_dir: HashMap<PathBuf, (Option<Arc<GitStatus>>, Instant)>,
    in_flight: HashSet<PathBuf>,
}

impl App {
    /// Starts a `git status` for panel directories with stale or no data.
    pub(super) fn update_git(&mut self) {
        if !self.config.general.git_status {
            return;
        }
        let dirs: Vec<PathBuf> = self.panels.iter().filter(|p| p.vfs.is_local()).map(|p| p.cwd.clone()).collect();
        for dir in dirs {
            let fresh = self.git.by_dir.get(&dir).is_some_and(|(_, at)| at.elapsed() < MAX_AGE);
            if fresh || !self.git.in_flight.insert(dir.clone()) {
                continue;
            }
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let status = git::status(&dir);
                let _ = tx.send(AppEvent::Git { dir, status });
            });
        }
    }

    pub(super) fn on_git_status(&mut self, dir: PathBuf, status: Option<GitStatus>) {
        self.git.in_flight.remove(&dir);
        self.git.by_dir.insert(dir, (status.map(Arc::new), Instant::now()));
    }

    /// Forces a refresh, e.g. after files changed.
    pub(super) fn invalidate_git(&mut self) {
        for (_, at) in self.git.by_dir.values_mut() {
            *at = Instant::now() - MAX_AGE;
        }
    }

    /// Status of `path` inside a panel showing `dir`.
    pub fn git_state(&self, dir: &Path, path: &Path) -> Option<GitState> {
        self.git.by_dir.get(dir)?.0.as_ref()?.get(path)
    }
}

/// Something the git menu or `:git` can do with the marked files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitOp {
    Diff,
    Stage,
    Unstage,
    Discard,
    Log,
    Blame,
    Commit,
}

impl GitOp {
    const MENU: [GitOp; 7] =
        [Self::Diff, Self::Stage, Self::Unstage, Self::Discard, Self::Commit, Self::Log, Self::Blame];

    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "diff" => Self::Diff,
            "stage" => Self::Stage,
            "unstage" => Self::Unstage,
            "discard" => Self::Discard,
            "log" => Self::Log,
            "blame" => Self::Blame,
            "commit" => Self::Commit,
            _ => return None,
        })
    }

    fn row(self) -> &'static str {
        match self {
            Self::Diff => "diff      show changes against the last commit",
            Self::Stage => "stage     git add",
            Self::Unstage => "unstage   take out of the next commit",
            Self::Discard => "discard   throw away unstaged changes",
            Self::Commit => "commit    commit what is staged…",
            Self::Log => "log       commits touching these items",
            Self::Blame => "blame     who changed each line",
        }
    }
}

/// What a finished git action hands back to the UI thread.
pub enum GitReply {
    /// Output to show in a popup (diff, log, blame).
    Show { title: String, body: String, colored: bool },
    /// The repository changed; the message goes in the status line.
    Changed(String),
    /// Nothing to do, and why.
    Warn(String),
    /// The repository of the marked items was found: open the menu
    /// (`op` is `None`) or run `op`, committing with `message` if given.
    Target { op: Option<GitOp>, target: GitTarget, branch: Option<String>, message: String },
    /// Ask for a commit message.
    CommitPrompt { root: PathBuf, staged: usize, branch: Option<String> },
    /// The commit failed; ask again with the message typed so far.
    CommitFailed { root: PathBuf, message: String, error: String },
}

/// The repository and items a git action works on.
pub struct GitTarget {
    root: PathBuf,
    paths: Vec<PathBuf>,
}

impl App {
    /// Finds the repository of the marked items (or the hovered one) off
    /// the UI thread, as git can be slow on big repositories or network
    /// drives, then opens the menu (`op` is `None`) or runs `op`.
    fn with_git_target(&mut self, op: Option<GitOp>, message: String) {
        let p = self.panel();
        if !p.vfs.is_local() {
            return self.notify("git works on local folders", strata_plugin::Level::Warn);
        }
        // An empty folder has nothing hovered: act on the folder, never on
        // the whole repository by accident.
        let mut paths = p.targets();
        if paths.is_empty() {
            paths.push(p.cwd.clone());
        }
        let cwd = p.cwd.clone();
        self.spawn_git(move || {
            let Some(root) = git::repo_root(&cwd) else {
                return Ok(GitReply::Warn("not inside a git repository".into()));
            };
            let branch = if op.is_none() { git::branch(&root) } else { None };
            Ok(GitReply::Target { op, target: GitTarget { root, paths }, branch, message })
        });
    }

    /// The git menu for the marked items.
    pub(super) fn git_menu(&mut self) {
        self.with_git_target(None, String::new());
    }

    fn open_git_menu(&mut self, target: GitTarget, branch: Option<String>) {
        let branch = branch.map(|b| format!(" · {b}")).unwrap_or_default();
        let title = format!("Git{branch} · {} item(s)", target.paths.len());
        let ops: Vec<GitOp> =
            GitOp::MENU.into_iter().filter(|op| *op != GitOp::Blame || self.blame_file(&target).is_some()).collect();
        let rows = ops.iter().map(|op| op.row().to_string()).collect();
        let purpose = PickerPurpose::Git { target, ops };
        self.show_overlay(Overlay::Picker(PickerState::new(title, rows, purpose)));
    }

    /// `:git <op> [message]` on the marked items; no op opens the menu.
    /// Anything else (`:git push`) runs as a git command in this folder.
    pub(super) fn git_command(&mut self, args: &str) {
        let (op, rest) = args.split_once(' ').map(|(o, r)| (o, r.trim())).unwrap_or((args, ""));
        if op.is_empty() {
            return self.git_menu();
        }
        let Some(op) = GitOp::from_name(op) else {
            if self.plugins.as_ref().is_some_and(|p| p.has_command("git")) {
                return self.run_plugin_command("git", args);
            }
            return self.run_shell_line(&format!("git {args}"));
        };
        self.with_git_target(Some(op), rest.to_string());
    }

    /// The single file `blame` would show.
    fn blame_file(&self, target: &GitTarget) -> Option<PathBuf> {
        match target.paths.as_slice() {
            [file] if file.is_file() => Some(file.clone()),
            _ => None,
        }
    }

    pub(super) fn run_git_op(&mut self, op: GitOp, target: GitTarget) {
        let GitTarget { root, paths } = target;
        match op {
            GitOp::Commit => self.spawn_git(move || {
                let staged = git::staged_count(&root)?;
                if staged == 0 {
                    return Ok(GitReply::Warn("nothing staged — stage items first".into()));
                }
                let branch = git::branch(&root);
                Ok(GitReply::CommitPrompt { root, staged, branch })
            }),
            GitOp::Discard => {
                let message = format!("Discard unstaged changes to {} item(s)? This cannot be undone.", paths.len());
                self.show_overlay(Overlay::Confirm(ConfirmState {
                    message,
                    action: Confirm::GitDiscard { root, paths },
                }));
            }
            GitOp::Blame => {
                let Some(file) = self.blame_file(&GitTarget { root: root.clone(), paths }) else {
                    return self.notify("blame works on one file", strata_plugin::Level::Warn);
                };
                let title = format!("blame {}", file.file_name().unwrap_or_default().to_string_lossy());
                self.spawn_git(move || {
                    git::blame(&root, &file).map(|body| GitReply::Show { title, body, colored: false })
                });
            }
            GitOp::Diff | GitOp::Log => {
                let what = describe(&root, &paths);
                self.spawn_git(move || {
                    let (title, body, colored) = if op == GitOp::Diff {
                        let body = git::diff(&root, &paths)?;
                        let body = if body.trim().is_empty() { "no changes".to_string() } else { body };
                        (format!("diff {what}"), body, true)
                    } else {
                        (format!("log {what}"), git::log(&root, &paths)?, false)
                    };
                    Ok(GitReply::Show { title, body, colored })
                });
            }
            GitOp::Stage | GitOp::Unstage => {
                let n = paths.len();
                self.spawn_git(move || {
                    if op == GitOp::Stage {
                        git::stage(&root, &paths).map(|()| GitReply::Changed(format!("staged {n} item(s)")))
                    } else {
                        git::unstage(&root, &paths).map(|()| GitReply::Changed(format!("unstaged {n} item(s)")))
                    }
                });
            }
        }
    }

    pub(super) fn git_discard(&mut self, root: PathBuf, paths: Vec<PathBuf>) {
        let n = paths.len();
        self.spawn_git(move || {
            git::discard(&root, &paths).map(|()| GitReply::Changed(format!("discarded changes to {n} item(s)")))
        });
    }

    pub(super) fn git_commit(&mut self, root: PathBuf, message: String) {
        self.spawn_git(move || {
            Ok(match git::commit(&root, &message) {
                Ok(summary) => GitReply::Changed(format!("committed {summary}")),
                Err(e) => GitReply::CommitFailed { root, message, error: format!("git: {e:#}") },
            })
        });
    }

    fn spawn_git(&self, work: impl FnOnce() -> anyhow::Result<GitReply> + Send + 'static) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(AppEvent::GitDone(work().map_err(|e| format!("git: {e:#}"))));
        });
    }

    pub(super) fn on_git_done(&mut self, result: Result<GitReply, String>) {
        match result {
            Ok(GitReply::Show { title, body, colored }) => {
                let lines = body.lines().map(str::to_string).collect();
                self.show_overlay(Overlay::Text(TextPopup { title, lines, scroll: 0, colored }));
            }
            Ok(GitReply::Changed(message)) => {
                self.info(message);
                self.invalidate_git();
                self.update_git();
                for panel in self.panels.iter_mut().filter(|p| p.vfs.is_local()) {
                    panel.reload();
                }
                self.invalidate_preview();
            }
            Ok(GitReply::Warn(message)) => self.notify(message, strata_plugin::Level::Warn),
            Ok(GitReply::Target { op: None, target, branch, .. }) => self.open_git_menu(target, branch),
            Ok(GitReply::Target { op: Some(GitOp::Commit), target, message, .. }) if !message.is_empty() => {
                self.git_commit(target.root, message)
            }
            Ok(GitReply::Target { op: Some(op), target, .. }) => self.run_git_op(op, target),
            Ok(GitReply::CommitPrompt { root, staged, branch }) => {
                let branch = branch.map(|b| format!(" on {b}")).unwrap_or_default();
                let title = format!("Commit {staged} staged file(s){branch}");
                self.show_overlay(Overlay::Input(InputState::new(title, "", InputPurpose::GitCommit(root))));
            }
            Ok(GitReply::CommitFailed { root, message, error }) => {
                self.error(error);
                let input =
                    InputState::new("Commit message (the commit failed)", message, InputPurpose::GitCommit(root));
                self.show_overlay(Overlay::Input(input));
            }
            Err(e) => self.error(e),
        }
    }
}

/// `a.txt`, `src/` or `3 items`, for popup titles.
fn describe(root: &Path, paths: &[PathBuf]) -> String {
    match paths {
        [] => "repository".into(),
        [one] if one == root => "repository".into(),
        [one] => one.strip_prefix(root).map(strata_core::util::posix).unwrap_or_else(|_| one.display().to_string()),
        many => format!("{} items", many.len()),
    }
}
