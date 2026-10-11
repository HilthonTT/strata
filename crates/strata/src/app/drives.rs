//! Removable drives: `:drives` lists them, enter mounts and opens one,
//! `:eject` unmounts and powers off the drive holding the focused folder
//! (or the disk under the sidebar cursor).

use std::path::Path;

use strata_core::util::human_size;
use strata_plugin::{Level, PluginValue};
use strata_sys::drives::{self, Drive};

use super::external::{After, External, Wait};
use super::overlay::{Overlay, PickerPurpose, PickerState};
use super::sidebar::SidebarItem;
use super::{App, Focus};

impl App {
    /// `:drives` picks a drive to open; `:mount <name>` opens one directly.
    pub(super) fn drives_command(&mut self, args: &str) {
        let drives = match drives::list() {
            Ok(d) => d,
            Err(e) => return self.error(format!("drives: {e:#}")),
        };
        if !args.is_empty() {
            return match find_drive(&drives, args) {
                Some(drive) => self.open_drive(drive.clone()),
                None => self.error(format!("no removable drive named '{args}'")),
            };
        }
        if drives.is_empty() {
            return self.info("no removable drives found");
        }
        let rows = drives
            .iter()
            .map(|d| {
                let place = match &d.mount {
                    Some(m) => m.display().to_string(),
                    None => "not mounted · enter mounts it".into(),
                };
                let dot = if d.mount.is_some() { "●" } else { "○" };
                format!("{dot} {:<16} {:>10}  {:<6} {:<12} {place}", d.name(), human_size(d.size), d.fs, d.device)
            })
            .collect();
        let title = "Removable drives · enter opens · ctrl+e ejects";
        self.overlay = Some(Overlay::Picker(PickerState::new(title, rows, PickerPurpose::Drives { drives })));
    }

    /// Goes to a mounted drive, or mounts it first.
    pub(super) fn open_drive(&mut self, drive: Drive) {
        if let Some(mount) = drive.mount {
            return self.cd_local(mount);
        }
        self.queue_external(External::Run {
            argv: drives::mount_command(&drive),
            cwd: None,
            wait: Wait::OnFailure,
            after: After::DriveMounted(drive.device),
        });
    }

    /// `ctrl+e` in the drives picker ejects the highlighted drive.
    pub(super) fn drives_picker_key(&mut self, ctrl_char: char) -> bool {
        let Some(Overlay::Picker(picker)) = &self.overlay else { return false };
        let PickerPurpose::Drives { drives } = &picker.purpose else { return false };
        if ctrl_char != 'e' {
            return false;
        }
        if let Some(drive) = picker.selected_index().and_then(|i| drives.get(i)).cloned() {
            self.overlay = None;
            self.eject(drive);
        }
        true
    }

    /// `:eject [name|path]`: the named drive, the disk under the sidebar
    /// cursor, or the drive holding the focused folder.
    pub(super) fn eject_command(&mut self, args: &str) {
        let drives = match drives::list() {
            Ok(d) => d,
            Err(e) => return self.error(format!("eject: {e:#}")),
        };
        let sidebar_disk = match (self.focus, self.sidebar.selected()) {
            (Focus::Sidebar, Some(SidebarItem::Disk { mount, .. })) => Some(mount.clone()),
            _ => None,
        };
        let cwd = self.panel().vfs.is_local().then(|| self.panel().cwd.clone());
        let drive = if !args.is_empty() {
            find_drive(&drives, args).or_else(|| drives::containing(&drives, &strata_core::util::expand_tilde(args)))
        } else {
            sidebar_disk.or(cwd).and_then(|path| drives::containing(&drives, &path))
        };
        match drive {
            Some(drive) => self.eject(drive.clone()),
            None if args.is_empty() => {
                self.notify("not on a removable drive: run :eject <name> or :drives", Level::Warn)
            }
            None => self.error(format!("no mounted removable drive '{args}'")),
        }
    }

    /// `d d` on a removable disk in the sidebar ejects it.
    pub(super) fn eject_sidebar_disk(&mut self, mount: &Path) -> bool {
        let Ok(drives) = drives::list() else { return false };
        match drives.iter().find(|d| d.mount.as_deref() == Some(mount)) {
            Some(drive) => {
                self.eject(drive.clone());
                true
            }
            None => false,
        }
    }

    fn eject(&mut self, drive: Drive) {
        // Step out of the drive first, or unmounting fails with "busy".
        if let Some(mount) = &drive.mount {
            let home = dirs::home_dir().unwrap_or_default();
            for p in self.panels.iter_mut().chain(self.tabs.iter_mut().flat_map(|t| t.panels.iter_mut())) {
                if p.vfs.is_local() && p.cwd.starts_with(mount) {
                    p.cd(home.clone());
                }
            }
        }
        let eject = drive.mount.is_some() || cfg!(target_os = "macos");
        self.queue_external(External::Run {
            argv: drives::unmount_command(&drive, eject),
            cwd: dirs::home_dir(),
            wait: Wait::OnFailure,
            after: After::DriveEjected(drive.name().to_string()),
        });
    }

    /// After the mount command: opens the drive where it was mounted.
    pub(super) fn drive_mounted(&mut self, device: &str) {
        let drive = drives::list().ok().and_then(|d| d.into_iter().find(|d| d.device == device));
        match drive.and_then(|d| d.mount.map(|m| (d.label, m))) {
            Some((label, mount)) => {
                self.info(format!("mounted {} at {}", label.as_deref().unwrap_or(device), mount.display()));
                let data = PluginValue::map([
                    ("device", PluginValue::str(device)),
                    ("path", PluginValue::str(mount.to_string_lossy())),
                ]);
                self.cd_local(mount);
                self.emit_plugin_event_with("mount", data);
            }
            None => self.notify(format!("{device}: the mount finished but the drive was not found"), Level::Warn),
        }
    }

    pub(super) fn drive_ejected(&mut self, name: &str) {
        self.info(format!("{name} can be removed safely"));
        self.emit_plugin_event_with("unmount", PluginValue::map([("device", PluginValue::str(name))]));
    }
}

/// A drive by label, device (`/dev/sdb1` or `sdb1`) or mount point.
fn find_drive<'a>(drives: &'a [Drive], name: &str) -> Option<&'a Drive> {
    drives.iter().find(|d| {
        d.label.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(name))
            || d.device == name
            || d.device.strip_prefix("/dev/") == Some(name)
            || d.mount.as_deref() == Some(Path::new(name))
    })
}
