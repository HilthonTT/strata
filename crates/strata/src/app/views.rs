//! Logic for the dashboard, Docker and NAS connection views (the disk
//! usage view is in `usage`).

use std::path::PathBuf;
use std::sync::Arc;

use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use strata_config::KeyPress;
use strata_core::nas::{Connection, Protocol, Step, StepStatus};
use strata_core::ssh::{HostKey, HostKeyError};
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
            View::Usage => self.usage_key(c),
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
            'e' if !container.is_running() => {
                self.notify(format!("{name} is not running — press s to start it"), Level::Warn);
                return true;
            }
            'e' => {
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
                let prompt = "URL (smb://, nfs://, sftp://, s3://, davs://, ftp://, rclone://)";
                let input = InputState::new(prompt, "", InputPurpose::AddConnectionUrl);
                self.overlay = Some(Overlay::Input(input));
            }
            't' | 'u' => {
                let Some(conn) = self.listed_connections().get(self.nas.cursor).cloned() else {
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
                let Some(conn) = self.listed_connections().get(self.nas.cursor).cloned() else { return false };
                if c == 'p' {
                    self.prompt_save_password(conn.name);
                } else {
                    self.forget_password(&conn.name);
                }
            }
            // Saves a ~/.ssh/config host to config.toml, under its alias.
            'i' => {
                let row = self.nas.cursor.checked_sub(self.config.connections.len());
                let host = row.and_then(|row| self.ssh_hosts().nth(row)).map(|h| h.to_connection());
                match (row, host) {
                    (_, Some(conn)) => self.save_connection(conn),
                    (Some(_), None) => return false,
                    (None, _) => {
                        self.info("this connection is already in config.toml; i imports hosts from ~/.ssh/config")
                    }
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

    pub(super) fn view_len(&self) -> usize {
        match self.view {
            View::Docker => self.docker.containers.len(),
            View::Connections => self.listed_connections().len(),
            View::Dashboard => self.dashboard.usage.as_ref().map_or(0, |u| u.items.len()),
            View::Usage => self.usage.listed().len(),
            View::Files => 0,
        }
    }

    fn view_cursor(&mut self) -> Option<&mut usize> {
        match self.view {
            View::Docker => Some(&mut self.docker.cursor),
            View::Connections => Some(&mut self.nas.cursor),
            View::Dashboard => Some(&mut self.dashboard.cursor),
            View::Usage => Some(&mut self.usage.cursor),
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
                if let Some(conn) = self.listed_connections().get(self.nas.cursor).cloned() {
                    self.open_connection(conn);
                }
            }
            View::Dashboard => {
                let item = self.dashboard.usage.as_ref().and_then(|u| u.items.get(self.dashboard.cursor)).cloned();
                if let Some(item) = item {
                    if item.is_dir {
                        self.open_usage(Some(item.path));
                    } else {
                        self.view = View::Files;
                        self.reveal(item.path);
                    }
                }
            }
            View::Usage | View::Files => {}
        }
    }

    /// Opens a connection: SFTP, S3, WebDAV, FTP and rclone remotes in-app,
    /// SMB/NFS by mounting (or jumping to the existing mount).
    pub fn open_connection(&mut self, conn: Connection) {
        if conn.protocol.in_app() {
            if let Some(vfs) = self.nas.sessions.get(&conn.name).cloned() {
                let path = match conn.protocol {
                    Protocol::Sftp if !conn.share.is_empty() => PathBuf::from(&conn.share),
                    _ => vfs.home(),
                };
                self.view = View::Files;
                self.panel_mut().switch_vfs(vfs, path);
                return;
            }
            return self.connect_remote(conn, None);
        }
        if let Some(path) = conn.mounted_at() {
            return self.cd_local(path);
        }
        // The keychain talks D-Bus, which can wait on an unlock prompt.
        if conn.protocol == Protocol::Smb {
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let password = strata_core::secrets::get(&conn.name);
                let _ = tx.send(crate::event::AppEvent::MountReady { conn, password });
            });
            return;
        }
        self.mount_connection(conn, None);
    }

    /// Mounts an SMB or NFS share, with the SMB password from the keychain.
    pub(super) fn mount_connection(&mut self, conn: Connection, password: Option<String>) {
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

    pub(super) fn save_password(&mut self, name: String, password: String) {
        let done = format!("saved the password for {name} in the system keychain");
        self.keychain_task(done, move || strata_core::secrets::set(&name, &password));
    }

    pub(super) fn forget_password(&mut self, name: &str) {
        let done = format!("removed the saved password for {name}");
        let name = name.to_string();
        self.keychain_task(done, move || strata_core::secrets::delete(&name));
    }

    /// Changes the keychain off the UI thread (D-Bus can wait on an unlock
    /// prompt), then reports and refreshes which connections have a password.
    fn keychain_task(&mut self, done: String, op: impl FnOnce() -> anyhow::Result<()> + Send + 'static) {
        let names: Vec<String> = self.listed_connections().into_iter().map(|c| c.name).collect();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let (message, level) = match op() {
                Ok(()) => (done, Level::Info),
                Err(e) => (format!("{e:#}"), Level::Error),
            };
            let _ = tx.send(crate::event::AppEvent::Notify { message, level });
            let saved = names.into_iter().filter(|n| strata_core::secrets::get(n).is_some()).collect();
            let _ = tx.send(crate::event::AppEvent::Keychain(saved));
        });
    }

    /// Checks which connections have a keychain password, off the UI thread.
    pub(super) fn refresh_keychain(&mut self) {
        let names: Vec<String> = self.listed_connections().into_iter().map(|c| c.name).collect();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let saved = names.into_iter().filter(|n| strata_core::secrets::get(n).is_some()).collect();
            let _ = tx.send(crate::event::AppEvent::Keychain(saved));
        });
    }

    /// Connects to an in-app connection, with `password` if one was typed.
    pub(super) fn connect_remote(&mut self, conn: Connection, password: Option<String>) {
        if conn.protocol == Protocol::Sftp {
            return self.connect_sftp(conn, password, None);
        }
        self.info(format!("connecting to {}…", conn.name));
        workers::connect_rclone(self.tx.clone(), conn, password);
    }

    /// `accepted` is the host key fingerprint the user has just trusted.
    fn connect_sftp(&mut self, conn: Connection, password: Option<String>, accepted: Option<String>) {
        #[cfg(feature = "sftp")]
        {
            self.info(format!("connecting to {}…", conn.host));
            workers::connect_sftp(self.tx.clone(), conn, password, accepted);
        }
        #[cfg(not(feature = "sftp"))]
        {
            let _ = (conn, password, accepted);
            self.error("this build of strata has no SFTP support");
        }
    }

    /// The server's host key was not trusted, so nothing was sent to it.
    /// A new server is the user's call; a changed or revoked key is refused.
    pub(super) fn on_host_key(&mut self, conn: Connection, password: Option<String>, problem: HostKeyError) {
        let name = conn.name.clone();
        let key = match problem {
            HostKeyError::Unknown(key) => key,
            problem => {
                self.error(format!("{name}: {problem}"));
                return self.show_overlay(Overlay::Text(host_key_warning(&name, &problem)));
            }
        };
        let message = format!(
            "{name}: {} is not a known host. Its {} key fingerprint is {} — trust it and add it to \
             ~/.ssh/known_hosts? Only say yes if it matches the server's own fingerprint.",
            key.host_entry(),
            key.key_type,
            key.fingerprint
        );
        let action = Confirm::TrustHostKey { conn, password, key: Box::new(key) };
        self.show_overlay(Overlay::Confirm(ConfirmState { message, action }));
    }

    /// Saves a host key the user trusted, then connects with it.
    pub(super) fn trust_host_key(&mut self, conn: Connection, password: Option<String>, key: HostKey) {
        match strata_core::ssh::remember_host_key(&key) {
            Ok(path) => self.info(format!("added {} to {}", key.host_entry(), path.display())),
            Err(e) => self.error(format!("could not save the host key ({e}); trusting it for this connection only")),
        }
        self.connect_sftp(conn, password, Some(key.fingerprint));
    }

    pub(super) fn on_connected(&mut self, conn: Connection, result: Result<VfsRef, String>, path: PathBuf) {
        let name = conn.name.clone();
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
                        self.show_overlay(Overlay::Confirm(ConfirmState { message, action }));
                    }
                }
            }
            // rclone remotes keep their own credentials in rclone's config.
            Err(e) if e.contains("authentication failed") && conn.protocol != Protocol::Rclone => {
                if self.pending_password.take().is_some() {
                    self.error(format!("{name}: {e}"));
                }
                let prompt = match (conn.protocol, &conn.user) {
                    (Protocol::S3, Some(key)) => format!("Secret key for {key} on {}", conn.host),
                    (Protocol::S3, None) => {
                        return self.error(format!(
                            "{name}: {e} · set AWS credentials, or put the access key in the URL \
                             (s3+https://KEY@host/bucket)"
                        ));
                    }
                    (_, user) => format!("Password for {}@{}", user.clone().unwrap_or_default(), conn.host),
                };
                self.show_overlay(Overlay::Input(
                    InputState::new(prompt, "", InputPurpose::RemotePassword(conn)).masked(),
                ));
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
        self.show_overlay(Overlay::Text(TextPopup { title: "Diagnose".into(), lines, scroll: 0, colored: false }));
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

/// The warning shown when a server's key does not match `known_hosts`.
/// There is no way past it from strata: the fix is the user's to make.
fn host_key_warning(name: &str, problem: &HostKeyError) -> TextPopup {
    let key = problem.key();
    let host = key.host_entry();
    let mut lines = vec![format!("✗ {name}: connection refused; nothing was sent to the server."), String::new()];
    match problem {
        HostKeyError::Changed { known_types, .. } => {
            lines.push("✗ WARNING: THE SERVER'S HOST KEY DOES NOT MATCH ~/.ssh/known_hosts".into());
            lines.push("  Someone could be intercepting the connection (a man-in-the-middle attack).".into());
            lines.push(String::new());
            lines.push(format!("  Server         {host}"));
            lines.push(format!("  Offered key    {} {}", key.key_type, key.fingerprint));
            lines.push(format!("  On record      {}", known_types.join(", ")));
            lines.push(String::new());
            if !known_types.contains(&key.key_type) {
                lines.push(format!("  The server sent a {} key, but known_hosts only has other types.", key.key_type));
                lines.push("  It may have stopped offering the recorded key, or this build cannot use it.".into());
            }
            lines.push("  If the server's keys were changed on purpose (say it was reinstalled), check the".into());
            lines.push("  fingerprint above with its administrator, remove the old entry and connect again:".into());
            lines.push(String::new());
            lines.push(format!("      ssh-keygen -R '{host}'"));
        }
        HostKeyError::Revoked(_) => {
            lines.push("✗ WARNING: THE SERVER'S HOST KEY HAS BEEN REVOKED".into());
            lines.push(String::new());
            lines.push(format!("  Server         {host}"));
            lines.push(format!("  Offered key    {} {}", key.key_type, key.fingerprint));
            lines.push(String::new());
            lines.push("  known_hosts marks this key @revoked. Ask the server's administrator for its new key.".into());
        }
        HostKeyError::Unknown(_) => lines.push(problem.to_string()),
    }
    TextPopup { title: "Host key warning".into(), lines, scroll: 0, colored: true }
}
