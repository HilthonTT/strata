//! Built-in actions triggered by key bindings.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use strata_config::Action;
use strata_core::elevate::Privileged;
use strata_core::jobs::JobState;
use strata_core::ops::{self, Conflict, CopyOptions, Transfer, TransferLog, TransferMode};
use strata_core::sort::SortKey;
use strata_core::throttle;
use strata_core::util::{human_size, posix};
use strata_core::VfsRef;
use strata_plugin::{Level, PluginValue};

use super::external::External;
use super::fileops::LinkKind;
use super::overlay::{Confirm, ConfirmState, InputPurpose, InputState, Overlay, PickerPurpose, PickerState};
use super::sidebar::SidebarItem;
use super::undo::PendingUndo;
use super::{App, Clipboard, Focus, View};

impl App {
    pub fn dispatch(&mut self, action: Action) {
        use Action::*;

        // `h`/`l` move across a grid and leave or open folders elsewhere.
        let in_grid = self.view == View::Files
            && self.focus == Focus::Panels
            && self.overlay.is_none()
            && self.panels.get(self.active).is_some_and(|p| p.is_grid());
        let action = match action {
            Left | Right if in_grid => return self.panel_mut().move_by(if action == Left { -1 } else { 1 }),
            Left => Parent,
            Right => Open,
            other => other,
        };

        // Actions that work everywhere.
        match action {
            ViewFiles => return self.set_view(View::Files),
            ViewDashboard => return self.set_view(View::Dashboard),
            ViewDocker => return self.set_view(View::Docker),
            ViewConnections => return self.set_view(View::Connections),
            ThemePicker => return self.open_theme_picker(),
            Help => return self.overlay = Some(Overlay::Help { scroll: 0 }),
            CommandPalette => {
                return self.overlay = Some(Overlay::Input(InputState::new(":", "", InputPurpose::Command)));
            }
            ToggleSidebar => return self.config.general.sidebar = !self.config.general.sidebar,
            ToggleFooter => return self.config.general.footer = !self.config.general.footer,
            TogglePreview => {
                self.config.general.preview = !self.config.general.preview;
                return self.invalidate_preview();
            }
            CancelJob => {
                if !self.jobs.cancel_latest() {
                    self.info("no running jobs");
                }
                return;
            }
            PauseJob => {
                let message = match self.jobs.toggle_pause_latest() {
                    Some(job) if job.progress.is_paused() => format!("paused {}", job.label),
                    Some(job) => format!("resumed {}", job.label),
                    None => "no running jobs".into(),
                };
                return self.info(message);
            }
            RetryJob => return self.retry_transfer(),
            Refresh => {
                self.dir_sizes.clear();
                self.reload_all();
                self.probe_connections();
                if self.view == View::Docker {
                    self.refresh_docker();
                }
                self.dashboard.usage = None;
                return self.info("refreshed");
            }
            QuitCd => {
                self.quit_and_cd();
                return self.dispatch(Quit);
            }
            Quit => {
                if self.jobs.running() > 0 {
                    let message = format!("{} job(s) still running. Quit and cancel them?", self.jobs.running());
                    self.overlay = Some(Overlay::Confirm(ConfirmState { message, action: Confirm::Quit }));
                } else {
                    self.quit();
                }
                return;
            }
            _ => {}
        }

        // Other views reuse navigation keys for their own lists.
        if self.view != View::Files {
            match action {
                Up => self.move_view_cursor(-1),
                Down => self.move_view_cursor(1),
                PageUp | HalfPageUp => self.move_view_cursor(-10),
                PageDown | HalfPageDown => self.move_view_cursor(10),
                Top => self.set_view_cursor(0),
                Bottom => self.set_view_cursor(usize::MAX),
                Open => self.open_in_view(),
                Clear | Parent => self.set_view(View::Files),
                _ => self.notify("press 1 to return to the files view", Level::Warn),
            }
            return;
        }

        if self.focus == Focus::Sidebar && self.on_sidebar_action(action) {
            return;
        }

        // Archives can be browsed and copied from, nothing else.
        let changes_panel = matches!(
            action,
            Cut | MoveToOther
                | Paste
                | Delete
                | DeletePermanent
                | Rename
                | BulkRename
                | RenamePattern
                | NewFile
                | NewDir
                | Duplicate
                | PasteSymlink
                | PasteRelativeSymlink
                | PasteHardlink
                | Chmod
        );
        if changes_panel && !self.ensure_writable() {
            return;
        }

        match action {
            Up => {
                let step = self.panel().row_step() as isize;
                self.panel_mut().move_by(-step);
            }
            Down => {
                let step = self.panel().row_step() as isize;
                self.panel_mut().move_by(step);
            }
            Top => self.panel_mut().move_to(0),
            Bottom => self.panel_mut().move_to(usize::MAX),
            PageUp => self.page(-1.0),
            PageDown => self.page(1.0),
            HalfPageUp => self.page(-0.5),
            HalfPageDown => self.page(0.5),
            Parent if self.panel().flat.is_some() => self.leave_flat(),
            Parent if self.panel().is_tree() && self.panel_mut().collapse_or_up() => {}
            Parent => {
                if !self.panel_mut().parent() {
                    self.leave_archive();
                }
            }
            Open => self.open_hovered(),
            Back => {
                self.panel_mut().go_back();
            }
            Forward => {
                self.panel_mut().go_forward();
            }
            Home => {
                let home = self.panel().vfs.home();
                self.cd(home);
            }
            Root => {
                let root = if self.panel().vfs.is_local() { super::sidebar::root_dir() } else { PathBuf::from("/") };
                self.cd(root);
            }
            NextPanel => self.focus_panel(self.active as isize + 1),
            PrevPanel => self.focus_panel(self.active as isize - 1),
            NewPanel => self.new_panel(),
            NewTab => self.new_tab(),
            CloseTab => self.close_tab(),
            NextTab => self.cycle_tab(1),
            PrevTab => self.cycle_tab(-1),
            Undo => self.undo(),
            Redo => self.redo(),
            PreviewDown => self.scroll_preview(self.half_preview()),
            PreviewUp => self.scroll_preview(-self.half_preview()),
            PreviewNext => self.next_preview_match(true, false),
            PreviewPrev => self.next_preview_match(false, false),
            PreviewFind => {
                let current = self.preview_query.clone().unwrap_or_default();
                self.overlay =
                    Some(Overlay::Input(InputState::new("Find in preview", current, InputPurpose::PreviewFind)));
            }
            ContentSearch => {
                if !self.panel().vfs.is_local() {
                    return self.notify("content search works on local directories", Level::Warn);
                }
                let input = InputState::new("Search contents", "", InputPurpose::Grep);
                self.overlay = Some(Overlay::Input(input));
            }
            ClosePanel => self.close_panel(),
            FocusSidebar => {
                // Only when it was drawn: it hides in narrow terminals.
                if self.config.general.sidebar && self.layout.sidebar.is_some() {
                    self.focus = Focus::Sidebar;
                }
            }
            FocusPanels => self.focus = Focus::Panels,
            ToggleSelect => {
                self.panel_mut().toggle_mark();
                self.panel_mut().move_by(1);
            }
            SelectAll => self.panel_mut().mark_all(),
            InvertSelection => self.panel_mut().invert_marks(),
            Clear => {
                let p = self.panel_mut();
                if !p.clear_marks() {
                    if !p.filter.is_empty() {
                        p.set_filter("");
                    } else if p.flat.is_some() {
                        self.leave_flat();
                    }
                }
            }
            VisualMode => self.panel_mut().toggle_visual(),
            SelectDown | SelectUp => {
                let p = self.panel_mut();
                p.toggle_mark();
                p.move_by(if action == SelectDown { 1 } else { -1 });
            }
            SortMenu => self.open_sort_menu(),
            RetryAsRoot => self.retry_as_root(),
            CopyCwd => {
                let cwd = self.panel().cwd.to_string_lossy().into_owned();
                self.set_clipboard(&cwd);
                self.info(format!("copied {cwd}"));
            }
            EditDir => self.edit_dir(),
            Copy => self.yank(TransferMode::Copy),
            Cut => self.yank(TransferMode::Move),
            Paste => self.paste(),
            CopyToOther => self.transfer_to_other(TransferMode::Copy),
            MoveToOther => self.transfer_to_other(TransferMode::Move),
            Delete => self.delete(false),
            DeletePermanent => self.delete(true),
            Rename => {
                if let Some(e) = self.panel().hovered() {
                    let input = InputState::new("Rename", e.name.clone(), InputPurpose::Rename(e.path.clone()));
                    self.overlay = Some(Overlay::Input(input.cursor_before_extension()));
                }
            }
            BulkRename => self.bulk_rename(),
            RenamePattern => self.rename_pattern(""),
            FindPrompt => self.find_prompt(),
            Flatten => self.toggle_flatten(),
            ToggleTree => {
                let on = !self.panel().tree;
                self.panel_mut().set_tree(on);
                self.info(if on { "tree view · l expands, h collapses, z M collapses all" } else { "list view" });
            }
            ToggleGrid => {
                let on = !self.panel().is_grid();
                self.panel_mut().set_grid(on);
                self.info(if on { "grid view · h j k l move, enter opens" } else { "list view" });
            }
            CollapseAll => self.panel_mut().collapse_all(),
            TagMenu => self.tag_menu(),
            GitMenu => self.git_menu(),
            NewFile => self.overlay = Some(Overlay::Input(InputState::new("New file", "", InputPurpose::NewFile))),
            NewDir => self.overlay = Some(Overlay::Input(InputState::new("New directory", "", InputPurpose::NewDir))),
            CopyPath => self.copy_path(),
            Filter => {
                let current = self.panel().filter.clone();
                let prompt = "Filter (fuzzy · *.{jpg,png} glob · re: regex)";
                self.overlay = Some(Overlay::Input(InputState::new(prompt, current, InputPurpose::Filter)));
            }
            FuzzyFind => self.start_find(),
            Jump => self.open_jump_picker(),
            ToggleHidden => {
                let show = !self.panel().show_hidden;
                self.config.general.show_hidden = show;
                for p in &mut self.panels {
                    p.show_hidden = show;
                    p.refilter();
                }
                self.info(if show { "showing hidden files" } else { "hiding hidden files" });
            }
            ToggleGitignored => {
                let hide = !self.config.general.hide_gitignored;
                self.config.general.hide_gitignored = hide;
                for p in &mut self.panels {
                    p.set_hide_gitignored(hide);
                }
                if hide && !strata_core::nas::which("git") {
                    self.notify("git is not installed: nothing can be hidden", Level::Warn);
                } else {
                    self.info(if hide { "hiding files ignored by git" } else { "showing files ignored by git" });
                }
            }
            CycleSort => {
                self.set_plugin_sort(None);
                let p = self.panel_mut();
                p.sort.key = p.sort.key.next();
                p.resort();
                let label = p.sort.key.label();
                self.info(format!("sort by {label}"));
            }
            ReverseSort => {
                let p = self.panel_mut();
                p.sort.reverse = !p.sort.reverse;
                p.resort();
            }
            ToggleDirSizes => {
                let on = !self.config.general.dir_sizes;
                self.config.general.dir_sizes = on;
                if !on {
                    // Drop measured sizes from the listings.
                    self.dir_sizes.stop();
                    self.reload_all();
                }
                self.info(if on { "measuring folder sizes" } else { "folder sizes off" });
            }
            Edit => self.edit_targets(),
            OpenWith => self.open_with_system(),
            Shell => self.open_shell(),
            Pin => self.toggle_pin(),
            Duplicate => self.duplicate(),
            PasteSymlink => self.paste_links(LinkKind::Absolute),
            PasteRelativeSymlink => self.paste_links(LinkKind::Relative),
            PasteHardlink => self.paste_links(LinkKind::Hard),
            Chmod => self.prompt_chmod(),
            Compare => self.compare(),
            Checksum => self.checksum(None),
            OpenTrash => self.open_trash(),
            _ => {}
        }
    }

    pub fn set_view(&mut self, view: View) {
        self.view = view;
        match view {
            View::Docker => self.refresh_docker(),
            View::Connections => {
                self.probe_connections();
                self.refresh_keychain();
            }
            View::Files => self.invalidate_preview(),
            View::Dashboard => {}
        }
    }

    fn page(&mut self, fraction: f64) {
        let page = match self.config.general.page_size {
            0 => self.panel().page,
            rows => rows * self.panel().row_step(),
        } as f64;
        self.panel_mut().move_by((page * fraction).round() as isize);
    }

    /// Changes the focused panel's directory and returns to the files view.
    pub fn cd(&mut self, path: PathBuf) {
        self.view = View::Files;
        self.focus = Focus::Panels;
        if !self.panel_mut().cd(path) {
            // The listing was kept; only the message moves to the status line.
            let err = self.panel_mut().error.take().unwrap_or_default();
            self.error(err);
        }
    }

    /// Opens a local path, leaving any remote filesystem the panel is on.
    pub fn cd_local(&mut self, path: PathBuf) {
        if self.panel().vfs.is_local() {
            return self.cd(path);
        }
        let local = self.local.clone();
        self.view = View::Files;
        self.panel_mut().switch_vfs(local, path);
    }

    /// Goes to a file's directory and puts the cursor on it.
    pub fn reveal(&mut self, path: PathBuf) {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        let name = name.to_string_lossy().into_owned();
        self.cd(dir.to_path_buf());
        self.panel_mut().focus_name(&name);
    }

    fn open_hovered(&mut self) {
        let Some(entry) = self.panel().hovered().cloned() else {
            return;
        };
        if entry.is_dir() {
            if self.panel().is_tree() {
                self.panel_mut().toggle_expand();
                return;
            }
            return self.cd(entry.path);
        }
        if self.chooser {
            return self.choose();
        }
        self.emit_plugin_event_with(
            "open",
            PluginValue::map([("path", PluginValue::str(entry.path.to_string_lossy()))]),
        );
        if self.is_browsable_archive(&entry.name) && self.panel().vfs.is_local() {
            return self.open_archive(entry.path);
        }
        if !self.panel().vfs.is_local() {
            if super::external::prefers_system_open(&entry.extension()) {
                self.notify("copy media files to a local folder to open them", Level::Warn);
            } else {
                self.edit_remote(&entry);
            }
        } else if self.open_with_rule(&entry) {
            // Handled by an `[open_with]` rule.
        } else if super::external::prefers_system_open(&entry.extension()) {
            self.open_with_system();
        } else {
            self.external = Some(External::Edit(vec![entry.path]));
        }
    }

    /// Picks the marked items, or the hovered file, and quits.
    fn choose(&mut self) {
        if !self.panel().vfs.is_local() {
            return self.notify("only local files can be picked", Level::Warn);
        }
        self.chosen = self.panel().targets();
        self.quit();
    }

    fn focus_panel(&mut self, index: isize) {
        let n = self.panels.len() as isize;
        self.active = index.rem_euclid(n) as usize;
        self.focus = Focus::Panels;
    }

    fn new_panel(&mut self) {
        if self.panels.len() >= 6 {
            return self.notify("at most 6 panels", Level::Warn);
        }
        let p = self.panel();
        let panel = self.make_panel(p.vfs.clone(), p.cwd.clone(), p.sort, p.show_hidden);
        self.panels.insert(self.active + 1, panel);
        self.active += 1;
    }

    fn close_panel(&mut self) {
        if self.panels.len() == 1 {
            return self.notify("cannot close the last panel", Level::Warn);
        }
        self.panels.remove(self.active);
        self.active = self.active.min(self.panels.len() - 1);
    }

    fn on_sidebar_action(&mut self, action: Action) -> bool {
        match action {
            Action::Up => self.sidebar.move_by(-1),
            Action::Down => self.sidebar.move_by(1),
            Action::Top => self.sidebar.first(),
            Action::Bottom => self.sidebar.last(),
            Action::Open => self.open_sidebar_item(),
            Action::Parent | Action::FocusPanels | Action::Clear => self.focus = Focus::Panels,
            Action::Pin => {
                if let Some(SidebarItem::Pinned(path)) = self.sidebar.selected().cloned() {
                    self.pinned.retain(|p| *p != path);
                    self.save_pins();
                    self.rebuild_sidebar();
                }
            }
            Action::Delete | Action::DeletePermanent => match self.sidebar.selected().cloned() {
                Some(SidebarItem::Disk { mount, removable: true, .. }) => {
                    if !self.eject_sidebar_disk(&mount) {
                        self.notify("this disk cannot be ejected from here", Level::Warn);
                    }
                }
                _ => self.notify("only removable disks can be ejected from the sidebar", Level::Warn),
            },
            _ => return false,
        }
        true
    }

    pub(super) fn open_sidebar_item(&mut self) {
        let Some(item) = self.sidebar.selected().cloned() else {
            return;
        };
        self.focus = Focus::Panels;
        match item {
            SidebarItem::Place { path, .. } | SidebarItem::Pinned(path) => self.cd_local(path),
            SidebarItem::Disk { mount, .. } => self.cd_local(mount),
            SidebarItem::Tag { name, .. } => self.show_tagged(&name),
            SidebarItem::Connection(name) => {
                if let Some(conn) = self.config.connections.iter().find(|c| c.name == name).cloned() {
                    self.open_connection(conn);
                }
            }
            SidebarItem::Header(_) => {}
        }
    }

    pub fn rebuild_sidebar(&mut self) {
        self.sidebar.rebuild(
            &self.config.general.sidebar_sections,
            &self.pinned,
            &self.metrics.disks,
            &self.config.connections,
            &self.tags.all(),
        );
    }

    fn toggle_pin(&mut self) {
        if !self.panel().vfs.is_local() {
            return self.notify("only local directories can be pinned", Level::Warn);
        }
        let cwd = self.panel().cwd.clone();
        if let Some(i) = self.pinned.iter().position(|p| *p == cwd) {
            self.pinned.remove(i);
            self.info(format!("unpinned {}", cwd.display()));
        } else {
            self.pinned.push(cwd.clone());
            self.info(format!("pinned {}", cwd.display()));
        }
        self.save_pins();
        self.rebuild_sidebar();
    }

    fn save_pins(&mut self) {
        let from_config: Vec<PathBuf> = self.config.pinned.iter().map(|p| strata_core::util::expand_tilde(p)).collect();
        let text: String =
            self.pinned.iter().filter(|p| !from_config.contains(p)).map(|p| format!("{}\n", p.display())).collect();
        let file = super::pins_file();
        let result = file.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::write(&file, text));
        if let Err(e) = result {
            self.error(format!("saving pins: {e}"));
        }
    }

    // --- clipboard & transfers ------------------------------------------------

    fn yank(&mut self, mode: TransferMode) {
        let paths = self.panel().targets();
        if paths.is_empty() {
            return;
        }
        let verb = if mode == TransferMode::Copy { "copied" } else { "cut" };
        self.info(format!("{verb} {} item(s)", paths.len()));
        self.clipboard = Some(Clipboard { vfs: self.panel().vfs.clone(), paths, mode });
        self.panel_mut().clear_marks();
    }

    fn paste(&mut self) {
        self.paste_with(CopyOptions::default());
    }

    /// Pastes the clipboard here, copying with `options`.
    pub(super) fn paste_with(&mut self, options: CopyOptions) {
        let Some(clip) = self.clipboard.as_ref() else {
            return self.notify("clipboard is empty — mark items and press y y or x", Level::Warn);
        };
        let transfer = Transfer {
            mode: clip.mode,
            src: clip.vfs.clone(),
            sources: clip.paths.clone(),
            dst: self.panel().vfs.clone(),
            dest_dir: self.panel().target_dir(),
            conflict: Conflict::KeepBoth,
            resolutions: Default::default(),
            targets: Default::default(),
            options,
        };
        if clip.mode == TransferMode::Move {
            self.clipboard = None;
        }
        self.begin_transfer(transfer);
        self.emit_plugin_event("paste");
    }

    fn transfer_to_other(&mut self, mode: TransferMode) {
        if self.panels.len() < 2 {
            return self.notify("open a second panel first (n)", Level::Warn);
        }
        let sources = self.panel().targets();
        if sources.is_empty() {
            return;
        }
        let other = &self.panels[(self.active + 1) % self.panels.len()];
        if other.vfs.read_only() {
            return self.notify("the next panel is an archive, which is read-only", Level::Warn);
        }
        let transfer = Transfer {
            mode,
            src: self.panel().vfs.clone(),
            sources,
            dst: other.vfs.clone(),
            dest_dir: other.cwd.clone(),
            conflict: Conflict::KeepBoth,
            resolutions: Default::default(),
            targets: Default::default(),
            options: Default::default(),
        };
        self.panel_mut().clear_marks();
        self.begin_transfer(transfer);
    }

    /// Starts a transfer, first asking about each name that already exists
    /// in the destination.
    pub(super) fn begin_transfer(&mut self, transfer: Transfer) {
        let clashes = transfer.clashes();
        if clashes.is_empty() {
            return self.start_transfer(transfer);
        }
        self.overlay = Some(Overlay::Conflict(super::overlay::ConflictState::new(transfer, clashes)));
    }

    pub(super) fn start_transfer(&mut self, mut transfer: Transfer) {
        if self.plugins_veto_transfer(&transfer) {
            return;
        }
        transfer.options.verify |= self.config.general.verify_copies;
        transfer.options.bandwidth_limit = self.bandwidth_limit();
        let verb = if transfer.mode == TransferMode::Copy { "Copy" } else { "Move" };
        let what = match transfer.sources.as_slice() {
            [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            many => format!("{} items", many.len()),
        };
        let label = format!("{verb} {what} → {}", short_path(&transfer.dest_dir));
        let log = Arc::new(TransferLog::default());
        let pending = PendingUndo::Transfer {
            mode: transfer.mode,
            src: transfer.src.clone(),
            dst: transfer.dst.clone(),
            log: log.clone(),
            replaced: transfer.resolutions.replaced.clone(),
            since: std::time::SystemTime::now(),
        };
        let undo_label = format!("{} {what}", verb.to_lowercase());
        let retry = (transfer.clone(), log.clone());
        let job = self.jobs.spawn(label, move |progress| transfer.run_logged(progress, &log));
        self.track_job(job, undo_label, pending);
        self.retryable.insert(job, retry);
    }

    /// The bandwidth limit for new jobs: `bandwidth_limit` from the config,
    /// or what `:limit` set since.
    pub(super) fn bandwidth_limit(&mut self) -> Option<u64> {
        match throttle::parse_rate(&self.config.general.bandwidth_limit) {
            Ok(limit) => limit,
            Err(e) => {
                self.error(format!("bandwidth_limit: {e:#}; copying without a limit"));
                None
            }
        }
    }

    /// `:limit [rate|off]`
    pub(super) fn limit_command(&mut self, args: &str) {
        if !args.is_empty() {
            if let Err(e) = throttle::parse_rate(args) {
                return self.error(format!("{e:#}"));
            }
            self.config.general.bandwidth_limit = args.to_string();
        }
        match self.bandwidth_limit() {
            Some(rate) => self.info(format!("new copies are limited to {}/s", human_size(rate))),
            None => self.info("new copies run at full speed"),
        }
    }

    /// Runs the latest failed or cancelled copy or move again, without the
    /// items it finished, into the same destinations.
    fn retry_transfer(&mut self) {
        let failed = self
            .jobs
            .jobs()
            .iter()
            .rev()
            .find(|j| {
                matches!(j.state(), JobState::Failed(_) | JobState::Cancelled) && self.retryable.contains_key(&j.id)
            })
            .map(|j| j.id);
        let Some((mut transfer, finished)) = failed.and_then(|id| self.retryable.remove(&id)) else {
            return self.info("no failed or cancelled copy to resume");
        };
        let finished: Vec<PathBuf> =
            finished.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(src, _)| src.clone()).collect();
        transfer.sources.retain(|s| !finished.contains(s));
        if transfer.sources.is_empty() {
            return self.info("everything was already copied");
        }
        self.start_transfer(transfer);
    }

    /// Forgets how to retry jobs that succeeded, keeping a few failed ones.
    pub(super) fn prune_retryable(&mut self, finished: u64, state: &JobState) {
        if *state == JobState::Done {
            self.retryable.remove(&finished);
        }
        while self.retryable.len() > 10 {
            let Some(&oldest) = self.retryable.keys().min() else { break };
            self.retryable.remove(&oldest);
        }
    }

    fn delete(&mut self, permanent: bool) {
        let paths = self.panel().targets();
        if paths.is_empty() {
            return;
        }
        let vfs = self.panel().vfs.clone();
        let permanent = permanent || !self.config.general.use_trash || !vfs.is_local();
        if !self.config.general.confirm_delete {
            return self.start_delete(vfs, paths, permanent);
        }
        let what = match paths.as_slice() {
            [one] => format!("'{}'", one.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()),
            many => format!("{} items", many.len()),
        };
        let message = if permanent {
            format!("Permanently delete {what}? This cannot be undone.")
        } else {
            format!("Move {what} to the trash?")
        };
        self.overlay =
            Some(Overlay::Confirm(ConfirmState { message, action: Confirm::Delete { vfs, paths, permanent } }));
    }

    pub(super) fn start_delete(&mut self, vfs: VfsRef, paths: Vec<PathBuf>, permanent: bool) {
        if self.plugin_vetoes("before_delete", &paths, [("permanent", PluginValue::Bool(permanent))]) {
            return;
        }
        let label = format!("{} {} item(s)", if permanent { "Delete" } else { "Trash" }, paths.len());
        self.panel_mut().clear_marks();
        let undo =
            (!permanent).then(|| PendingUndo::Trash { paths: paths.clone(), since: std::time::SystemTime::now() });
        let undo_label = format!("trash {} item(s)", paths.len());
        let data = PluginValue::map([
            ("paths", PluginValue::strings(paths.iter().map(|p| p.to_string_lossy().into_owned()))),
            ("permanent", PluginValue::Bool(permanent)),
        ]);
        self.emit_plugin_event_with("delete", data);
        let plan = Privileged::Delete(paths.clone());
        let job_vfs = vfs.clone();
        let job = self.jobs.spawn(label, move |progress| ops::delete(&job_vfs, &paths, !permanent, progress));
        self.offer_elevation(job, &[&vfs], plan);
        if let Some(pending) = undo {
            self.track_job(job, undo_label, pending);
        }
    }

    // --- misc -----------------------------------------------------------------

    fn copy_path(&mut self) {
        let paths: Vec<String> = self.panel().targets().iter().map(|p| p.to_string_lossy().into_owned()).collect();
        if paths.is_empty() {
            return;
        }
        self.set_clipboard(&paths.join("\n"));
        self.info(format!("copied {} path(s) to the clipboard", paths.len()));
    }

    fn start_find(&mut self) {
        if !self.panel().vfs.is_local() {
            return self.notify("fuzzy find works on local directories; use / to filter", Level::Warn);
        }
        self.find_cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.find_cancel = cancel.clone();
        let root = self.panel().cwd.clone();
        // The picker opens at once, so keys typed while indexing go into
        // its query instead of running as commands.
        let purpose = PickerPurpose::Find { vfs: self.panel().vfs.clone(), root: root.clone() };
        let title = format!("Find in {} · indexing…", short_path(&root));
        self.overlay = Some(Overlay::Picker(PickerState::new(title, Vec::new(), purpose)));
        super::workers::find_files(self.tx.clone(), root, self.panel().show_hidden, cancel);
    }

    /// Fills the find picker once indexing is done, keeping the query.
    pub(super) fn open_find_picker(&mut self, root: PathBuf, files: Vec<PathBuf>) {
        let Some(Overlay::Picker(picker)) = &mut self.overlay else { return };
        if !matches!(&picker.purpose, PickerPurpose::Find { root: r, .. } if *r == root) {
            return;
        }
        picker.title = format!("Find in {} · {} files", short_path(&root), files.len());
        picker.items = files.iter().map(|p| posix(p)).collect();
        picker.refilter();
    }

    pub fn open_theme_picker(&mut self) {
        let original = self.theme.name.clone();
        let mut picker =
            PickerState::new("Theme", self.themes.names(), PickerPurpose::Theme { original: original.clone() });
        picker.focus(&original);
        self.overlay = Some(Overlay::Picker(picker));
    }

    /// Switches theme; `persist_name` records it as the configured theme.
    pub fn apply_theme(&mut self, name: &str, persist_name: bool) -> bool {
        let Some(theme) = self.themes.get(name).cloned() else {
            self.error(format!("unknown theme '{name}'"));
            return false;
        };
        self.theme = if self.config.general.transparent { theme.transparent() } else { theme };
        if persist_name {
            self.config.general.theme = name.to_string();
        }
        self.on_theme_changed();
        if persist_name {
            self.emit_plugin_event_with("theme", PluginValue::map([("name", PluginValue::str(name))]));
        }
        true
    }

    const SORT_KEYS: [(SortKey, &'static str); 4] = [
        (SortKey::Name, "Name"),
        (SortKey::Size, "Size"),
        (SortKey::Modified, "Date modified"),
        (SortKey::Extension, "Type (extension)"),
    ];

    /// The sort menu: built-in keys, then plugin sort orders, then the
    /// reverse and folders-first switches.
    fn open_sort_menu(&mut self) {
        let sort = self.panel().sort;
        let plugin_sort = self.plugin_list.sort_of(self.panel().id);
        let check = |on: bool| if on { "●" } else { "○" };
        let mut items: Vec<String> = Self::SORT_KEYS
            .iter()
            .map(|(key, label)| format!("{} {label}", check(plugin_sort.is_none() && sort.key == *key)))
            .collect();
        items.extend(
            self.plugin_list
                .sorts
                .iter()
                .map(|s| format!("{} {}", check(plugin_sort == Some(s.name.as_str())), s.title)),
        );
        items.push(format!("{} Reverse order", check(sort.reverse)));
        items.push(format!("{} Directories first", check(sort.dirs_first)));
        let current = match plugin_sort {
            Some(name) => self.plugin_list.sorts.iter().position(|s| s.name == name).map(|i| Self::SORT_KEYS.len() + i),
            None => Self::SORT_KEYS.iter().position(|(k, _)| *k == sort.key),
        };
        let mut picker = PickerState::new("Sort by", items, PickerPurpose::Sort);
        if let Some(i) = current {
            picker.cursor = i;
        }
        self.overlay = Some(Overlay::Picker(picker));
    }

    pub(super) fn apply_sort_choice(&mut self, index: usize) {
        let keys = Self::SORT_KEYS.len();
        let plugin_sorts = self.plugin_list.sorts.len();
        if (keys..keys + plugin_sorts).contains(&index) {
            let name = self.plugin_list.sorts[index - keys].name.clone();
            return self.set_plugin_sort(Some(name));
        }
        if index < keys {
            self.set_plugin_sort(None);
        }
        let p = self.panel_mut();
        match index {
            i if i < keys => p.sort.key = Self::SORT_KEYS[i].0,
            i if i == keys + plugin_sorts => p.sort.reverse = !p.sort.reverse,
            _ => p.sort.dirs_first = !p.sort.dirs_first,
        }
        p.resort();
    }
}

/// `~/projects/x` instead of `/home/me/projects/x`.
pub fn short_path(path: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", posix(rest)) };
        }
    }
    path.to_string_lossy().into_owned()
}
