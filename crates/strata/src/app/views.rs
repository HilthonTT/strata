//! Logic for the dashboard, Docker and NAS connection views.

use std::path::PathBuf;
use std::sync::Arc;

use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use strata_config::KeyPress;
use strata_core::nas::{Connection, Protocol, Step, StepStatus};
use strata_core::vfs::DockerVfs;
use strata_core::{Vfs, VfsRef};
use strata_plugin::Level;
use strata_sys::docker::ContainerAction;

use super::external::{After, External};
use super::overlay::{Confirm, ConfirmState, InputPurpose, InputState, Overlay, TextPopup};
use super::{workers, App, View};

impl App {
    /// View-specific single keys, checked before the global keymap.
    pub(super) fn on_view_key(&mut self, key: KeyPress) -> bool {
        if key.mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return false;
        }
        let KeyCode::Char(c) = key.code else {
            return false;
        };
        match self.view {
            View::Docker => self.docker_key(c),
            View::Connections => self.connections_key(c),
            View::Dashboard | View::Files => false,
        }
    }

    fn docker_key(&mut self, c: char) -> bool {
        let Some(container) = self.docker.containers.get(self.docker.cursor).cloned() else {
            return false;
        };
        let (id, name) = (container.id.clone(), container.name.clone());
        let action = match c {
            's' => ContainerAction::Start,
            'S' => ContainerAction::Stop,
            'r' => ContainerAction::Restart,
            'p' if container.state == "paused" => ContainerAction::Unpause,
            'p' => ContainerAction::Pause,
            'x' => {
                let message = format!("Remove container '{name}'?");
                self.overlay =
                    Some(Overlay::Confirm(ConfirmState { message, action: Confirm::DockerRemove { id, name } }));
                return true;
            }
            'L' => {
                workers::container_logs(self.tx.clone(), id, name);
                return true;
            }
            'e' if container.is_running() => {
                let shell = DockerVfs::new(id, name).shell_command(std::path::Path::new("/"));
                self.queue_external(External::Shell { cwd: PathBuf::from("/"), remote: shell });
                return true;
            }
            _ => return false,
        };
        self.info(format!("{} {name}…", action.verb()));
        workers::container_action(self.tx.clone(), id, name, action);
        true
    }

    fn connections_key(&mut self, c: char) -> bool {
        match c {
            'a' => {
                let input = InputState::new("URL (smb://, nfs://, sftp://)", "", InputPurpose::AddConnectionUrl);
                self.overlay = Some(Overlay::Input(input));
            }
            't' | 'u' => {
                let Some(conn) = self.config.connections.get(self.nas.cursor).cloned() else {
                    return false;
                };
                if c == 't' {
                    self.start_diagnose(conn);
                } else {
                    self.unmount(conn);
                }
            }
            'e' => self.run_command("config"),
            'p' | 'f' => {
                let Some(conn) = self.config.connections.get(self.nas.cursor).cloned() else { return false };
                if c == 'p' {
                    self.prompt_save_password(conn.name);
                } else {
                    self.forget_password(&conn.name);
                }
            }
            'r' => {
                self.probe_connections();
                self.info("checking connections…");
            }
            _ => return false,
        }
        true
    }

    fn view_len(&self) -> usize {
        match self.view {
            View::Docker => self.docker.containers.len(),
            View::Connections => self.config.connections.len(),
            View::Dashboard => self.dashboard.usage.as_ref().map_or(0, |u| u.items.len()),
            View::Files => 0,
        }
    }

    fn view_cursor(&mut self) -> Option<&mut usize> {
        match self.view {
            View::Docker => Some(&mut self.docker.cursor),
            View::Connections => Some(&mut self.nas.cursor),
            View::Dashboard => Some(&mut self.dashboard.cursor),
            View::Files => None,
        }
    }

    pub(super) fn move_view_cursor(&mut self, delta: isize) {
        let max = self.view_len().saturating_sub(1) as isize;
        if let Some(cursor) = self.view_cursor() {
            *cursor = (*cursor as isize + delta).clamp(0, max.max(0)) as usize;
        }
    }

    pub(super) fn set_view_cursor(&mut self, row: usize) {
        let max = self.view_len().saturating_sub(1);
        if let Some(cursor) = self.view_cursor() {
            *cursor = row.min(max);
        }
    }

    pub(super) fn open_in_view(&mut self) {
        match self.view {
            View::Docker => {
                let Some(c) = self.docker.containers.get(self.docker.cursor).cloned() else {
                    return;
                };
                if !c.is_running() {
                    return self.notify(format!("{} is not running — press s to start it", c.name), Level::Warn);
                }
                let vfs: VfsRef = Arc::new(DockerVfs::new(c.id, c.name.clone()));
                self.view = View::Files;
                self.panel_mut().switch_vfs(vfs, PathBuf::from("/"));
                self.info(format!("browsing container {}", c.name));
            }
            View::Connections => {
                if let Some(conn) = self.config.connections.get(self.nas.cursor).cloned() {
                    self.open_connection(conn);
                }
            }
            View::Dashboard => {
                let item = self.dashboard.usage.as_ref().and_then(|u| u.items.get(self.dashboard.cursor)).cloned();
                if let Some(item) = item {
                    if item.is_dir {
                        self.cd_local(item.path);
                    } else {
                        self.view = View::Files;
                        self.reveal(item.path);
                    }
                }
            }
            View::Files => {}
        }
    }

    /// Opens a NAS connection: SFTP in-app, SMB/NFS by mounting (or jumping
    /// to the existing mount).
    pub fn open_connection(&mut self, conn: Connection) {
        if conn.protocol == Protocol::Sftp {
            if let Some(vfs) = self.nas.sessions.get(&conn.name).cloned() {
                let path = if conn.share.is_empty() { vfs.home() } else { PathBuf::from(&conn.share) };
                self.view = View::Files;
                self.panel_mut().switch_vfs(vfs, path);
                return;
            }
            return self.connect_sftp(conn, None);
        }
        if let Some(path) = conn.mounted_at() {
            return self.cd_local(path);
        }
        let password = if conn.protocol == Protocol::Smb { strata_core::secrets::get(&conn.name) } else { None };
        let Some(plan) = conn.mount_plan(password.as_deref()) else {
            return;
        };
        if conn.needs_mount_dir() {
            if let Err(e) = std::fs::create_dir_all(conn.mount_point()) {
                return self.error(format!("cannot create {}: {e}", conn.mount_point().display()));
            }
        }
        self.info(format!("mounting {}…", conn.name));
        self.queue_external(External::Mount { plan, after: After::Mounted(conn.name.clone()) });
    }

    pub(super) fn prompt_save_password(&mut self, name: String) {
        let prompt = format!("Password for {name} (saved in the system keychain)");
        self.overlay = Some(Overlay::Input(InputState::new(prompt, "", InputPurpose::SavePassword(name)).masked()));
    }

    pub(super) fn forget_password(&mut self, name: &str) {
        match strata_core::secrets::delete(name) {
            Ok(()) => {
                self.nas.saved_passwords.remove(name);
                self.info(format!("removed the saved password for {name}"));
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    /// Checks which connections have a keychain password, off the UI thread.
    pub(super) fn refresh_keychain(&mut self) {
        let names: Vec<String> = self.config.connections.iter().map(|c| c.name.clone()).collect();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let saved = names.into_iter().filter(|n| strata_core::secrets::get(n).is_some()).collect();
            let _ = tx.send(crate::event::AppEvent::Keychain(saved));
        });
    }

    pub(super) fn connect_sftp(&mut self, conn: Connection, password: Option<String>) {
        #[cfg(feature = "sftp")]
        {
            self.info(format!("connecting to {}…", conn.host));
            workers::connect_sftp(self.tx.clone(), conn, password);
        }
        #[cfg(not(feature = "sftp"))]
        {
            let _ = (conn, password);
            self.error("this build of strata has no SFTP support");
        }
    }

    pub(super) fn on_connected(&mut self, name: String, result: Result<VfsRef, String>, path: PathBuf) {
        match result {
            Ok(vfs) => {
                self.nas.sessions.insert(name.clone(), vfs.clone());
                self.view = View::Files;
                self.panel_mut().switch_vfs(vfs, path);
                self.info(format!("connected to {name}"));
                // Offer to remember a password the user just typed.
                if let Some((pending, password)) = self.pending_password.take() {
                    if pending == name && self.connection(&name).is_some() {
                        let message = format!("Save the password for {name} in the system keychain?");
                        let action = Confirm::SavePassword { name, password };
                        self.overlay = Some(Overlay::Confirm(ConfirmState { message, action }));
                    }
                }
            }
            Err(e) if e.contains("authentication failed") => {
                let conn = self
                    .connection(&name)
                    .or_else(|| Connection::from_url(&name, &format!("sftp://{name}")))
                    .filter(|c| c.protocol == Protocol::Sftp);
                match conn {
                    Some(conn) => {
                        let prompt = format!("Password for {}@{}", conn.user.clone().unwrap_or_default(), conn.host);
                        self.overlay = Some(Overlay::Input(
                            InputState::new(prompt, "", InputPurpose::SftpPassword(conn)).masked(),
                        ));
                    }
                    None => self.error(format!("{name}: {e}")),
                }
            }
            Err(e) => {
                self.pending_password = None;
                self.error(format!("{name}: {e}"));
            }
        }
    }

    pub(super) fn start_diagnose(&mut self, conn: Connection) {
        self.info(format!("checking {}…", conn.name));
        workers::diagnose(self.tx.clone(), conn);
    }

    pub(super) fn show_diagnosis(&mut self, name: &str, steps: Vec<Step>) {
        let mut lines = vec![format!("Connection check for {name}"), String::new()];
        for step in steps {
            let mark = match step.status {
                StepStatus::Ok => "✓",
                StepStatus::Warn => "!",
                StepStatus::Fail => "✗",
                StepStatus::Skipped => "–",
            };
            lines.push(format!(" {mark} {:<14} {}", step.name, step.detail));
        }
        lines.push(String::new());
        lines.push("✓ ok   ! warning   ✗ failed   – skipped (an earlier step failed)".into());
        self.overlay = Some(Overlay::Text(TextPopup { title: "Diagnose".into(), lines, scroll: 0, colored: false }));
    }

    /// Mounts SMB shares marked `auto_connect` through GVFS, which needs no
    /// terminal. Mounts that may prompt for a password (sudo, SFTP) are
    /// left for the user to open.
    pub(super) fn auto_connect(&mut self) {
        let pending: Vec<Connection> = self.config.connections.iter().filter(|c| c.auto_connect).cloned().collect();
        for conn in pending {
            match conn.mount_command() {
                Some(argv) if argv[0] == "gio" && conn.mounted_at().is_none() => {
                    std::thread::spawn(move || {
                        let _ = std::process::Command::new(&argv[0])
                            .args(&argv[1..])
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .status();
                    });
                }
                _ => {}
            }
        }
    }
}
