//! Built-in actions triggered by key bindings.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use strata_config::Action;
use strata_core::ops::{self, Conflict, Transfer, TransferMode};
use strata_core::sort::SortKey;
use strata_core::util::posix;
use strata_core::VfsRef;
use strata_plugin::Level;

use super::external::External;
use super::overlay::{
    Confirm, ConfirmState, InputPurpose, InputState, Overlay, PickerPurpose, PickerState,
};
use super::panel::Panel;
use super::sidebar::SidebarItem;
use super::{App, Clipboard, Focus, View};

impl App {
    pub fn dispatch(&mut self, action: Action) {
        use Action::*;

        // Actions that work everywhere.
        match action {
            ViewFiles => return self.set_view(View::Files),
            ViewDashboard => return self.set_view(View::Dashboard),
            ViewDocker => return self.set_view(View::Docker),
            ViewConnections => return self.set_view(View::Connections),
            ThemePicker => return self.open_theme_picker(),
            Help => return self.overlay = Some(Overlay::Help { scroll: 0 }),
            CommandPalette => {
                return self.overlay = Some(Overlay::Input(InputState::new(
                    ":",
                    "",
                    InputPurpose::Command,
                )));
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
            Refresh => {
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
                    let message = format!(
                        "{} job(s) still running. Quit and cancel them?",
                        self.jobs.running()
                    );
                    self.overlay = Some(Overlay::Confirm(ConfirmState {
                        message,
                        action: Confirm::Quit,
                    }));
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

        match action {
            Up => self.panel_mut().move_by(-1),
            Down => self.panel_mut().move_by(1),
            Top => self.panel_mut().move_to(0),
            Bottom => self.panel_mut().move_to(usize::MAX),
            PageUp => self.page(-1.0),
            PageDown => self.page(1.0),
            HalfPageUp => self.page(-0.5),
            HalfPageDown => self.page(0.5),
            Parent => {
                self.panel_mut().parent();
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
                let root = if self.panel().vfs.is_local() {
                    super::sidebar::root_dir()
                } else {
                    PathBuf::from("/")
                };
                self.cd(root);
            }
            NextPanel => self.focus_panel(self.active as isize + 1),
            PrevPanel => self.focus_panel(self.active as isize - 1),
            NewPanel => self.new_panel(),
            ClosePanel => self.close_panel(),
            FocusSidebar => {
                if self.config.general.sidebar {
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
                if !p.clear_marks() && !p.filter.is_empty() {
                    p.set_filter("");
                }
            }
            VisualMode => self.panel_mut().toggle_visual(),
            SelectDown | SelectUp => {
                let p = self.panel_mut();
                p.toggle_mark();
                p.move_by(if action == SelectDown { 1 } else { -1 });
            }
            SortMenu => self.open_sort_menu(),
            CopyCwd => {
                let cwd = self.panel().cwd.to_string_lossy().into_owned();
                copy_to_clipboard(&cwd);
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
                    let input = InputState::new(
                        "Rename",
                        e.name.clone(),
                        InputPurpose::Rename(e.path.clone()),
                    );
                    self.overlay = Some(Overlay::Input(input.cursor_before_extension()));
                }
            }
            BulkRename => self.bulk_rename(),
            NewFile => {
                self.overlay = Some(Overlay::Input(InputState::new(
                    "New file",
                    "",
                    InputPurpose::NewFile,
                )))
            }
            NewDir => {
                self.overlay = Some(Overlay::Input(InputState::new(
                    "New directory",
                    "",
                    InputPurpose::NewDir,
                )))
            }
            CopyPath => self.copy_path(),
            Filter => {
                let current = self.panel().filter.clone();
                self.overlay = Some(Overlay::Input(InputState::new(
                    "Filter",
                    current,
                    InputPurpose::Filter,
                )));
            }
            FuzzyFind => self.start_find(),
            ToggleHidden => {
                let show = !self.panel().show_hidden;
                self.config.general.show_hidden = show;
                for p in &mut self.panels {
                    p.show_hidden = show;
                    p.refilter();
                }
                self.info(if show {
                    "showing hidden files"
                } else {
                    "hiding hidden files"
                });
            }
            CycleSort => {
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
            Edit => self.edit_targets(),
            OpenWith => self.open_with_system(),
            Shell => self.open_shell(),
            Pin => self.toggle_pin(),
            _ => {}
        }
    }

    pub fn set_view(&mut self, view: View) {
        self.view = view;
        match view {
            View::Docker => self.refresh_docker(),
            View::Connections => self.probe_connections(),
            View::Files => self.invalidate_preview(),
            View::Dashboard => {}
        }
    }

    fn page(&mut self, fraction: f64) {
        let page = self.panel().page as f64;
        self.panel_mut().move_by((page * fraction).round() as isize);
    }

    /// Changes the focused panel's directory and returns to the files view.
    pub fn cd(&mut self, path: PathBuf) {
        self.view = View::Files;
        self.focus = Focus::Panels;
        if !self.panel_mut().cd(path) {
            let err = self.panel().error.clone().unwrap_or_default();
            self.panel_mut().reload();
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
            return self.cd(entry.path);
        }
        if !self.panel().vfs.is_local() {
            self.notify(
                "remote files open in the preview only; copy them locally to edit",
                Level::Warn,
            );
        } else if self.open_with_rule(&entry) {
            // Handled by an `[open_with]` rule.
        } else if super::external::prefers_system_open(&entry.extension()) {
            self.open_with_system();
        } else {
            self.external = Some(External::Edit(vec![entry.path]));
        }
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
        let panel = Panel::new(p.vfs.clone(), p.cwd.clone(), p.sort, p.show_hidden);
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
            SidebarItem::Connection(name) => {
                if let Some(conn) = self
                    .config
                    .connections
                    .iter()
                    .find(|c| c.name == name)
                    .cloned()
                {
                    self.open_connection(conn);
                }
            }
            SidebarItem::Header(_) => {}
        }
    }

    pub fn rebuild_sidebar(&mut self) {
        self.sidebar
            .rebuild(&self.pinned, &self.metrics.disks, &self.config.connections);
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
        let from_config: Vec<PathBuf> = self
            .config
            .pinned
            .iter()
            .map(|p| strata_core::util::expand_tilde(p))
            .collect();
        let text: String = self
            .pinned
            .iter()
            .filter(|p| !from_config.contains(p))
            .map(|p| format!("{}\n", p.display()))
            .collect();
        let file = super::pins_file();
        let result = file
            .parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .and_then(|_| std::fs::write(&file, text));
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
        let verb = if mode == TransferMode::Copy {
            "copied"
        } else {
            "cut"
        };
        self.info(format!("{verb} {} item(s)", paths.len()));
        self.clipboard = Some(Clipboard {
            vfs: self.panel().vfs.clone(),
            paths,
            mode,
        });
        self.panel_mut().clear_marks();
    }

    fn paste(&mut self) {
        let Some(clip) = self.clipboard.as_ref() else {
            return self.notify(
                "clipboard is empty — mark items and press y y or x",
                Level::Warn,
            );
        };
        let transfer = Transfer {
            mode: clip.mode,
            src: clip.vfs.clone(),
            sources: clip.paths.clone(),
            dst: self.panel().vfs.clone(),
            dest_dir: self.panel().cwd.clone(),
            conflict: Conflict::KeepBoth,
        };
        if clip.mode == TransferMode::Move {
            self.clipboard = None;
        }
        self.start_transfer(transfer);
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
        let transfer = Transfer {
            mode,
            src: self.panel().vfs.clone(),
            sources,
            dst: other.vfs.clone(),
            dest_dir: other.cwd.clone(),
            conflict: Conflict::KeepBoth,
        };
        self.panel_mut().clear_marks();
        self.start_transfer(transfer);
    }

    fn start_transfer(&mut self, transfer: Transfer) {
        let verb = if transfer.mode == TransferMode::Copy {
            "Copy"
        } else {
            "Move"
        };
        let what = match transfer.sources.as_slice() {
            [one] => one
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            many => format!("{} items", many.len()),
        };
        let label = format!("{verb} {what} → {}", short_path(&transfer.dest_dir));
        self.jobs
            .spawn(label, move |progress| transfer.run(progress));
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
            [one] => format!(
                "'{}'",
                one.file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            ),
            many => format!("{} items", many.len()),
        };
        let message = if permanent {
            format!("Permanently delete {what}? This cannot be undone.")
        } else {
            format!("Move {what} to the trash?")
        };
        self.overlay = Some(Overlay::Confirm(ConfirmState {
            message,
            action: Confirm::Delete {
                vfs,
                paths,
                permanent,
            },
        }));
    }

    pub(super) fn start_delete(&mut self, vfs: VfsRef, paths: Vec<PathBuf>, permanent: bool) {
        let label = format!(
            "{} {} item(s)",
            if permanent { "Delete" } else { "Trash" },
            paths.len()
        );
        self.panel_mut().clear_marks();
        self.jobs.spawn(label, move |progress| {
            ops::delete(&vfs, &paths, !permanent, progress)
        });
    }

    // --- misc -----------------------------------------------------------------

    fn copy_path(&mut self) {
        let paths: Vec<String> = self
            .panel()
            .targets()
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        if paths.is_empty() {
            return;
        }
        copy_to_clipboard(&paths.join("\n"));
        self.info(format!("copied {} path(s) to the clipboard", paths.len()));
    }

    fn start_find(&mut self) {
        if !self.panel().vfs.is_local() {
            return self.notify(
                "fuzzy find works on local directories; use / to filter",
                Level::Warn,
            );
        }
        self.find_cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.find_cancel = cancel.clone();
        self.info("indexing…");
        super::workers::find_files(
            self.tx.clone(),
            self.panel().cwd.clone(),
            self.panel().show_hidden,
            cancel,
        );
    }

    pub(super) fn open_find_picker(&mut self, root: PathBuf, files: Vec<PathBuf>) {
        if self.overlay.is_some() || root != self.panel().cwd {
            return;
        }
        let items = files.iter().map(|p| posix(p)).collect();
        let purpose = PickerPurpose::Find {
            vfs: self.panel().vfs.clone(),
            root: root.clone(),
        };
        self.overlay = Some(Overlay::Picker(PickerState::new(
            format!("Find in {}", short_path(&root)),
            items,
            purpose,
        )));
    }

    pub fn open_theme_picker(&mut self) {
        let original = self.theme.name.clone();
        let mut picker = PickerState::new(
            "Theme",
            self.themes.names(),
            PickerPurpose::Theme {
                original: original.clone(),
            },
        );
        picker.focus(&original);
        self.overlay = Some(Overlay::Picker(picker));
    }

    /// Switches theme; `persist_name` records it as the configured theme.
    pub fn apply_theme(&mut self, name: &str, persist_name: bool) -> bool {
        let Some(theme) = self.themes.get(name).cloned() else {
            self.error(format!("unknown theme '{name}'"));
            return false;
        };
        self.theme = if self.config.general.transparent {
            theme.transparent()
        } else {
            theme
        };
        if persist_name {
            self.config.general.theme = name.to_string();
        }
        self.on_theme_changed();
        true
    }

    const SORT_KEYS: [(SortKey, &'static str); 4] = [
        (SortKey::Name, "Name"),
        (SortKey::Size, "Size"),
        (SortKey::Modified, "Date modified"),
        (SortKey::Extension, "Type (extension)"),
    ];

    fn open_sort_menu(&mut self) {
        let sort = self.panel().sort;
        let check = |on: bool| if on { "●" } else { "○" };
        let mut items: Vec<String> = Self::SORT_KEYS
            .iter()
            .map(|(key, label)| format!("{} {label}", check(sort.key == *key)))
            .collect();
        items.push(format!("{} Reverse order", check(sort.reverse)));
        items.push(format!("{} Directories first", check(sort.dirs_first)));
        let mut picker = PickerState::new("Sort by", items, PickerPurpose::Sort);
        if let Some(i) = Self::SORT_KEYS.iter().position(|(k, _)| *k == sort.key) {
            picker.cursor = i;
        }
        self.overlay = Some(Overlay::Picker(picker));
    }

    pub(super) fn apply_sort_choice(&mut self, index: usize) {
        let p = self.panel_mut();
        match index {
            i if i < Self::SORT_KEYS.len() => p.sort.key = Self::SORT_KEYS[i].0,
            4 => p.sort.reverse = !p.sort.reverse,
            _ => p.sort.dirs_first = !p.sort.dirs_first,
        }
        p.resort();
    }
}

/// `~/projects/x` instead of `/home/me/projects/x`.
pub fn short_path(path: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", posix(rest))
            };
        }
    }
    path.to_string_lossy().into_owned()
}

/// Sets the system clipboard through OSC 52, which works locally and over
/// SSH in most modern terminals.
fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    let osc = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let mut out = std::io::stdout();
    let _ = out.write_all(osc.as_bytes()).and_then(|_| out.flush());
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
