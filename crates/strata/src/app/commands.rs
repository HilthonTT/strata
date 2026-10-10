//! The `:` command palette, prompt submission and completion.

use std::path::PathBuf;

use strata_config::Action;
use strata_core::nas::Connection;
use strata_core::sort::SortKey;
use strata_core::util::expand_tilde;
use strata_plugin::Level;

use super::external::{After, External, Wait};
use super::overlay::{InputPurpose, InputState, Overlay, PickerPurpose, TextPopup};
use super::App;

/// Built-in commands with a one-line description, for help and completion.
pub const COMMANDS: &[(&str, &str)] = &[
    ("cd", "cd <path> — change directory"),
    ("local", "local [path] — leave a remote filesystem"),
    ("mkdir", "mkdir <name> — create a directory"),
    ("touch", "touch <name> — create a file"),
    ("rename", "rename <name> — rename the hovered item"),
    ("select", "select <glob> — mark matching items"),
    ("duplicate", "duplicate — copy marked items next to themselves"),
    ("link", "link [-r] — paste the clipboard as (relative) symbolic links"),
    ("hardlink", "hardlink — paste the clipboard as hard links"),
    ("chmod", "chmod [-R] <mode> — set permissions (755, u+x, go-w)"),
    ("chown", "chown [-R] <user[:group]> — change the owner"),
    ("compare", "compare — diff two marked items, or the hovered one with the next panel"),
    ("checksum", "checksum [md5|sha1|sha256|sha512] — show checksums, or copy one"),
    ("verify", "verify [hash] — check a file against a hash, or a checksum file's list"),
    ("trash", "trash — browse, restore and delete trashed items"),
    ("empty-trash", "empty-trash — delete everything in the trash for good"),
    ("theme", "theme <name> — switch theme"),
    ("sort", "sort name|size|modified|ext [rev]"),
    ("set", "set hidden|preview|sidebar|footer [on|off]"),
    ("connect", "connect <name|url> — open a NAS connection"),
    ("disconnect", "disconnect <name> — unmount a share"),
    ("diagnose", "diagnose <name> — test a connection step by step"),
    ("sh", "sh <command> — run a shell command here"),
    ("grep", "grep <pattern> — search file contents"),
    ("tab", "tab <n> — go to tab n (or open a new one)"),
    ("undo", "undo — undo the last file operation"),
    ("password", "password <name> — save a NAS password in the keychain"),
    ("forget", "forget <name> — remove a saved NAS password"),
    ("config", "config — edit config.toml"),
    ("plugins", "plugins — list loaded plugins"),
    ("messages", "messages — notification history"),
    ("files", "files — files view"),
    ("dashboard", "dashboard — disks, I/O and memory"),
    ("docker", "docker — containers"),
    ("connections", "connections — NAS connections"),
    ("help", "help — key bindings"),
    ("quit", "quit — exit strata"),
];

impl App {
    pub fn run_command(&mut self, line: &str) {
        let line = line.trim().trim_start_matches(':');
        if line.is_empty() {
            return;
        }
        if let Some(rest) = line.strip_prefix('!') {
            return self.run_shell_line(rest.trim());
        }
        let (cmd, args) = line.split_once(char::is_whitespace).map(|(c, a)| (c, a.trim())).unwrap_or((line, ""));
        match cmd {
            "cd" => {
                let target = if args.is_empty() { "~" } else { args };
                let path = self.resolve(target);
                self.cd(path);
            }
            "local" => {
                let path = if args.is_empty() { dirs::home_dir().unwrap_or_default() } else { expand_tilde(args) };
                self.cd_local(path);
            }
            "mkdir" if !args.is_empty() => self.create(args, true),
            "touch" if !args.is_empty() => self.create(args, false),
            "rename" if !args.is_empty() => {
                if let Some(path) = self.panel().hovered().map(|e| e.path.clone()) {
                    self.rename(path, args);
                }
            }
            "select" => {
                let n = self.panel_mut().mark_glob(if args.is_empty() { "*" } else { args });
                self.info(format!("marked {n} item(s)"));
            }
            "duplicate" | "dup" => self.dispatch(Action::Duplicate),
            "link" | "symlink" | "ln" => match args {
                "" => self.dispatch(Action::PasteSymlink),
                "-r" | "--relative" => self.dispatch(Action::PasteRelativeSymlink),
                _ => self.error("usage: link [-r]"),
            },
            "hardlink" => self.dispatch(Action::PasteHardlink),
            "chmod" if !args.is_empty() => {
                let (vfs, paths) = (self.panel().vfs.clone(), self.panel().targets());
                if !paths.is_empty() {
                    self.start_chmod(vfs, paths, args);
                }
            }
            "chown" if !args.is_empty() => self.chown_command(args),
            "chown" => self.error("usage: chown [-R] user[:group]"),
            "compare" | "diff" => self.dispatch(Action::Compare),
            "checksum" | "hash" => self.checksum(Some(args)),
            "verify" => self.verify(args),
            "trash" => self.dispatch(Action::OpenTrash),
            "empty-trash" => match strata_core::trash::list() {
                Ok(items) if items.is_empty() => self.info("the trash is empty"),
                Ok(items) => {
                    let message = format!("Empty the trash ({} items)? This cannot be undone.", items.len());
                    self.overlay = Some(Overlay::Confirm(super::overlay::ConfirmState {
                        message,
                        action: super::overlay::Confirm::EmptyTrash,
                    }));
                }
                Err(e) => self.error(format!("{e:#}")),
            },
            "theme" if args.is_empty() => self.open_theme_picker(),
            "theme" => {
                if self.apply_theme(args, true) {
                    self.info(format!("theme: {args}"));
                }
            }
            "sort" => self.sort_command(args),
            "set" => self.set_command(args),
            "connect" => self.connect_command(args),
            "disconnect" | "unmount" => match self.connection(args) {
                Some(conn) => self.unmount(conn),
                None => self.error(format!("no connection named '{args}'")),
            },
            "diagnose" | "test" => match self.connection(args) {
                Some(conn) => self.start_diagnose(conn),
                None => self.error(format!("no connection named '{args}'")),
            },
            "sh" | "shell" if !args.is_empty() => self.run_shell_line(args),
            "grep" | "rg" | "search" => {
                if args.is_empty() {
                    self.dispatch(Action::ContentSearch);
                } else {
                    self.start_content_search(args);
                }
            }
            "tab" => match args.parse::<usize>() {
                Ok(n) if n >= 1 => self.switch_tab(n - 1, false),
                _ => self.dispatch(Action::NewTab),
            },
            "undo" => self.dispatch(Action::Undo),
            "password" => match self.connection(args) {
                Some(conn) => self.prompt_save_password(conn.name),
                None => self.error(format!("no connection named '{args}'")),
            },
            "forget" => match self.connection(args) {
                Some(conn) => self.forget_password(&conn.name),
                None => self.error(format!("no connection named '{args}'")),
            },
            "config" => {
                let path = self.config_path.clone();
                if !path.exists() {
                    let _ = path.parent().map(std::fs::create_dir_all);
                    let _ = std::fs::write(&path, strata_config::Config::template());
                }
                self.queue_external(External::Edit(vec![path]));
                self.notify("restart strata to apply config changes", Level::Info);
            }
            "plugins" => self.show_plugins(),
            "messages" | "log" => {
                let lines = self
                    .notifications
                    .iter()
                    .map(|n| format!("{:>5}  {}", format!("{:?}", n.level).to_lowercase(), n.message))
                    .collect::<Vec<_>>();
                let scroll = lines.len().saturating_sub(10);
                self.overlay =
                    Some(Overlay::Text(TextPopup { title: "Messages".into(), lines, scroll, colored: false }));
            }
            "files" => self.dispatch(Action::ViewFiles),
            "dashboard" => self.dispatch(Action::ViewDashboard),
            "docker" => self.dispatch(Action::ViewDocker),
            "connections" | "nas" => self.dispatch(Action::ViewConnections),
            "help" => self.dispatch(Action::Help),
            "q" | "quit" | "exit" => self.dispatch(Action::Quit),
            "q!" => self.quit(),
            _ => {
                if let Some(action) = Action::from_name(cmd) {
                    self.dispatch(action);
                } else if self.plugins.as_ref().is_some_and(|p| p.has_command(cmd)) {
                    self.run_plugin_command(cmd, args);
                } else {
                    self.error(format!("unknown command: {cmd}"));
                }
            }
        }
    }

    /// Resolves a path typed by the user against the focused panel.
    fn resolve(&self, input: &str) -> PathBuf {
        let p = self.panel();
        if p.vfs.is_local() {
            let path = expand_tilde(input);
            if path.is_absolute() {
                path
            } else {
                p.cwd.join(path)
            }
        } else if input.starts_with('/') {
            PathBuf::from(input)
        } else if input == "~" {
            p.vfs.home()
        } else {
            p.vfs.join(&p.cwd, input)
        }
    }

    fn sort_command(&mut self, args: &str) {
        let mut parts = args.split_whitespace();
        let key = match parts.next() {
            Some("name") => SortKey::Name,
            Some("size") => SortKey::Size,
            Some("modified") | Some("time") | Some("mtime") => SortKey::Modified,
            Some("ext") | Some("extension") => SortKey::Extension,
            _ => return self.error("usage: sort name|size|modified|ext [rev]"),
        };
        let reverse = matches!(parts.next(), Some("rev") | Some("reverse") | Some("desc"));
        let p = self.panel_mut();
        p.sort.key = key;
        p.sort.reverse = reverse;
        p.resort();
    }

    fn set_command(&mut self, args: &str) {
        let mut parts = args.split_whitespace();
        let (Some(name), value) = (parts.next(), parts.next()) else {
            return self.error("usage: set hidden|preview|sidebar|footer [on|off]");
        };
        let g = &self.config.general;
        let current = match name {
            "hidden" => g.show_hidden,
            "preview" => g.preview,
            "sidebar" => g.sidebar,
            "footer" => g.footer,
            "icons" => g.icons,
            _ => return self.error(format!("unknown setting '{name}'")),
        };
        let wanted = match value {
            Some("on" | "true" | "yes" | "1") => true,
            Some("off" | "false" | "no" | "0") => false,
            _ => !current,
        };
        if wanted == current {
            return;
        }
        match name {
            "hidden" => self.dispatch(Action::ToggleHidden),
            "preview" => self.dispatch(Action::TogglePreview),
            "sidebar" => self.dispatch(Action::ToggleSidebar),
            "footer" => self.dispatch(Action::ToggleFooter),
            _ => self.config.general.icons = wanted,
        }
    }

    fn connect_command(&mut self, args: &str) {
        if args.is_empty() {
            return self.dispatch(Action::ViewConnections);
        }
        if let Some(conn) = self.connection(args) {
            return self.open_connection(conn);
        }
        match Connection::from_url("", args) {
            Some(conn) => self.open_connection(conn),
            None => self.error(format!("not a saved connection or URL: {args}")),
        }
    }

    pub(super) fn connection(&self, name: &str) -> Option<Connection> {
        self.config.connections.iter().find(|c| c.name == name).cloned()
    }

    fn create(&mut self, name: &str, dir: bool) {
        let p = self.panel();
        let path = p.vfs.join(&p.cwd, name);
        let vfs = p.vfs.clone();
        // The outermost path that does not exist yet is what undo removes.
        let first = name.split(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(name);
        let created = vfs.join(&p.cwd, first);
        let created = (!vfs.exists(&created)).then_some(created);
        let result = if dir {
            vfs.create_dir(&path)
        } else {
            let parent = vfs.parent(&path).filter(|parent| !vfs.exists(parent));
            parent.map(|parent| vfs.create_dir(&parent)).transpose().and_then(|_| vfs.create_file(&path))
        };
        match result {
            Ok(()) => {
                self.panel_mut().reload();
                let first = name.split(['/', '\\']).next().unwrap_or(name).to_string();
                self.panel_mut().focus_name(&first);
                if let Some(path) = created {
                    self.push_undo(format!("create {first}"), vec![super::undo::UndoOp::Created { vfs, path }]);
                }
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn rename(&mut self, from: PathBuf, new_name: &str) {
        if new_name.contains('/') || new_name.contains('\\') {
            return self.error("names cannot contain path separators");
        }
        let vfs = self.panel().vfs.clone();
        let Some(dir) = vfs.parent(&from) else { return };
        let to = vfs.join(&dir, new_name);
        if to == from {
            return;
        }
        if vfs.exists(&to) {
            return self.error(format!("'{new_name}' already exists"));
        }
        match vfs.rename(&from, &to) {
            Ok(()) => {
                self.panel_mut().reload();
                self.panel_mut().focus_name(new_name);
                self.push_undo(format!("rename to {new_name}"), vec![super::undo::UndoOp::Rename { vfs, from, to }]);
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    pub(super) fn submit_input(&mut self) {
        let Some(Overlay::Input(input)) = self.overlay.take() else {
            return;
        };
        let value = input.value.trim().to_string();
        match input.purpose {
            InputPurpose::Filter => {}
            InputPurpose::Command => self.run_command(&input.value),
            InputPurpose::Rename(path) if !value.is_empty() => self.rename(path, &value),
            InputPurpose::NewFile if !value.is_empty() => {
                self.create(value.trim_end_matches('/'), value.ends_with('/'))
            }
            InputPurpose::NewDir if !value.is_empty() => self.create(&value, true),
            InputPurpose::Plugin(callback) => self.call_plugin(callback, Some(Some(input.value))),
            InputPurpose::SftpPassword(conn) => {
                self.pending_password = Some((conn.name.clone(), input.value.clone()));
                self.connect_sftp(conn, Some(input.value));
            }
            InputPurpose::Grep => self.start_content_search(&value),
            InputPurpose::PreviewFind => self.find_in_preview(&value),
            InputPurpose::SavePassword(name) if !input.value.is_empty() => {
                self.confirmed(super::overlay::Confirm::SavePassword { name, password: input.value });
            }
            InputPurpose::AddConnectionUrl if !value.is_empty() => match Connection::from_url("", &value) {
                Some(conn) => {
                    let name = conn.name.clone();
                    self.overlay = Some(Overlay::Input(InputState::new(
                        "Connection name",
                        name,
                        InputPurpose::AddConnectionName(conn),
                    )));
                }
                None => self.error("use smb://user@host/share, nfs://host/export or sftp://user@host:port/path"),
            },
            InputPurpose::Chmod { vfs, paths } if !value.is_empty() => self.start_chmod(vfs, paths, &value),
            InputPurpose::AddConnectionName(mut conn) if !value.is_empty() => {
                conn.name = value;
                self.save_connection(conn);
            }
            _ => {}
        }
    }

    pub(super) fn submit_picker(&mut self) {
        let Some(Overlay::Picker(picker)) = self.overlay.take() else {
            return;
        };
        let selected = picker.selected().map(str::to_string);
        let selected_index = picker.selected_index();
        match picker.purpose {
            PickerPurpose::Theme { original } => match selected {
                Some(name) => {
                    self.apply_theme(&name, true);
                    self.info(format!("theme: {name} (set general.theme to keep it)"));
                }
                None => {
                    self.apply_theme(&original, false);
                }
            },
            PickerPurpose::Find { vfs, root } => {
                if let Some(rel) = selected {
                    let path = vfs.join(&root, &rel);
                    if vfs.stat(&path).is_ok_and(|e| e.is_dir()) {
                        self.cd(path);
                    } else {
                        self.reveal(path);
                    }
                }
            }
            PickerPurpose::Plugin(callback) => self.call_plugin(callback, Some(selected)),
            PickerPurpose::Grep { root, hits } => {
                if let Some(hit) = selected_index.and_then(|i| hits.get(i)) {
                    self.open_hit(&root, hit);
                }
            }
            PickerPurpose::Sort => {
                if let Some(index) = picker.selected_index() {
                    self.apply_sort_choice(index);
                }
            }
            PickerPurpose::Trash { mut items } => {
                if let Some(i) = selected_index.filter(|&i| i < items.len()) {
                    self.restore_trashed(items.swap_remove(i));
                }
            }
        }
    }

    /// Tab completion for the command palette and path prompts.
    pub(super) fn complete_input(&mut self) {
        let Some(Overlay::Input(input)) = &self.overlay else {
            return;
        };
        let value = input.value.clone();
        let candidates: Vec<String> = match input.purpose {
            InputPurpose::Command => match value.split_once(' ') {
                None => {
                    let mut names: Vec<String> = COMMANDS.iter().map(|(c, _)| c.to_string()).collect();
                    names.extend(Action::ALL.iter().map(|a| a.name().to_string()));
                    if let Some(p) = &self.plugins {
                        names.extend(p.commands().into_iter().map(|(n, _)| n));
                    }
                    names.into_iter().filter(|n| n.starts_with(&value)).map(|n| format!("{n} ")).collect()
                }
                Some(("theme", arg)) => self
                    .themes
                    .names()
                    .into_iter()
                    .filter(|n| n.starts_with(arg))
                    .map(|n| format!("theme {n}"))
                    .collect(),
                Some((cmd @ ("connect" | "disconnect" | "diagnose" | "password" | "forget"), arg)) => self
                    .config
                    .connections
                    .iter()
                    .filter(|c| c.name.starts_with(arg))
                    .map(|c| format!("{cmd} {}", c.name))
                    .collect(),
                Some((cmd @ ("cd" | "local"), arg)) => {
                    self.complete_path(arg).into_iter().map(|p| format!("{cmd} {p}")).collect()
                }
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        let completion = match candidates.as_slice() {
            [] => return,
            [one] => one.clone(),
            many => common_prefix(many),
        };
        if let Some(Overlay::Input(input)) = &mut self.overlay {
            if completion.len() >= input.value.len() {
                input.set(completion);
            }
        }
    }

    fn complete_path(&self, partial: &str) -> Vec<String> {
        let p = self.panel();
        let (dir_part, name_part) = match partial.rfind('/') {
            Some(i) => (&partial[..=i], &partial[i + 1..]),
            None => ("", partial),
        };
        let dir = if dir_part.is_empty() { p.cwd.clone() } else { self.resolve(dir_part) };
        let Ok(entries) = p.vfs.read_dir(&dir) else {
            return Vec::new();
        };
        let mut out: Vec<String> = entries
            .into_iter()
            .filter(|e| e.is_dir() && e.name.starts_with(name_part) && (name_part.starts_with('.') || !e.is_hidden()))
            .map(|e| format!("{dir_part}{}/", e.name))
            .collect();
        out.sort();
        out
    }

    pub(super) fn save_connection(&mut self, conn: Connection) {
        if self.config.connections.iter().any(|c| c.name == conn.name) {
            return self.error(format!("a connection named '{}' already exists", conn.name));
        }
        match strata_config::Config::append_connection(&self.config_path, &conn) {
            Ok(()) => {
                self.info(format!("saved connection '{}' to {}", conn.name, self.config_path.display()));
                self.config.connections.push(conn);
                self.rebuild_sidebar();
                self.probe_connections();
            }
            Err(e) => self.error(format!("saving connection: {e:#}")),
        }
    }

    pub(super) fn unmount(&mut self, conn: Connection) {
        if let Some(vfs) = self.nas.sessions.remove(&conn.name) {
            for i in 0..self.panels.len() {
                if std::sync::Arc::ptr_eq(&self.panels[i].vfs, &vfs) {
                    let local = self.local.clone();
                    self.panels[i].switch_vfs(local, dirs::home_dir().unwrap_or_default());
                }
            }
            return self.info(format!("disconnected {}", conn.name));
        }
        let Some(mounted) = conn.mounted_at() else {
            return self.notify(format!("{} is not mounted", conn.name), Level::Warn);
        };
        // Step out of the mount first, or unmounting fails with "busy".
        let home = dirs::home_dir().unwrap_or_default();
        for p in &mut self.panels {
            if p.vfs.is_local() && p.cwd.starts_with(&mounted) {
                p.cd(home.clone());
            }
        }
        self.queue_external(External::Run {
            argv: conn.unmount_command(&mounted),
            cwd: Some(home),
            wait: Wait::OnFailure,
            after: After::Unmounted,
        });
    }
}

fn common_prefix(items: &[String]) -> String {
    let first = &items[0];
    let mut len = first.len();
    for item in &items[1..] {
        len = len.min(first.bytes().zip(item.bytes()).take_while(|(a, b)| a == b).count());
    }
    while !first.is_char_boundary(len) {
        len -= 1;
    }
    first[..len].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_prefix_of_candidates() {
        let items = vec!["theme nord".to_string(), "theme nightfox".to_string()];
        assert_eq!(common_prefix(&items), "theme n");
    }
}
