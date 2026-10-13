//! Built-in `:compress` and `:extract`, and passwords of encrypted zips.
//!
//! Extracting opens the archive like browsing it does, then copies its
//! contents out as an ordinary transfer, so it gets progress, pause,
//! resume, undo and conflict handling for free.
//!
//! An encrypted zip is opened with the password saved for it in the
//! keychain, or else one asked for (masked). The password is checked when
//! the archive opens, so a wrong one is asked again rather than producing
//! garbage files; one that worked can then be remembered in the keychain.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use strata_core::compress;
use strata_core::ops::{Conflict, Transfer, TransferMode};
use strata_core::util::unique_name;
use strata_core::vfs::{ArchiveFormat, ArchiveVfs, Locked};
use strata_core::VfsRef;
use strata_plugin::Level;

use super::external::{After, External, Wait};
use super::overlay::{Confirm, ConfirmState, InputPurpose, InputState, Overlay};
use super::App;
use crate::event::AppEvent;

/// What an archive is being opened for.
#[derive(Debug, Clone)]
pub enum ArchiveUse {
    /// Browsing in the panel with this id.
    Browse(u64),
    /// `:extract` into this folder.
    Extract(PathBuf),
}

impl App {
    /// `:compress [--password] [name]`: packs the marked items (or the
    /// hovered one) into an archive in the focused directory, asking for a
    /// name if not given. With `--password` (or `-p`), the zip is encrypted
    /// with AES-256 and the password is asked for twice.
    pub(super) fn compress_command(&mut self, args: &str) {
        let items = self.panel().targets();
        if items.is_empty() {
            return self.notify("compress: nothing to compress", Level::Warn);
        }
        if !self.panel().vfs.is_local() {
            return self.notify("compress: only local files can be compressed", Level::Warn);
        }
        let (encrypt, name) = split_password_flag(args);
        if !name.is_empty() {
            return self.compress_or_ask(items, name, encrypt);
        }
        let stem = match items.as_slice() {
            [one] => file_name(one),
            _ => file_name(&self.panel().cwd),
        };
        let purpose = InputPurpose::Compress { items, encrypt };
        let input = InputState::new("Archive name", format!("{stem}.zip"), purpose);
        self.overlay = Some(Overlay::Input(input));
    }

    /// Compresses, after asking for a password if the archive gets one.
    pub(super) fn compress_or_ask(&mut self, items: Vec<PathBuf>, name: &str, encrypt: bool) {
        if !encrypt {
            return self.compress(items, name, None);
        }
        if ArchiveFormat::detect(name) != Some(ArchiveFormat::Zip) || !compress::can_write(name) {
            return self.error(format!("{name}: only .zip archives can have a password"));
        }
        self.ask_compress_password(items, name.to_string(), None);
    }

    /// Asks for the new archive's password, then (with `first`) to repeat it.
    fn ask_compress_password(&mut self, items: Vec<PathBuf>, name: String, first: Option<String>) {
        let prompt = match first {
            None => format!("Password for {name} (AES-256)"),
            Some(_) => format!("Repeat the password for {name}"),
        };
        let purpose = InputPurpose::CompressPassword { items, name, first };
        self.overlay = Some(Overlay::Input(InputState::new(prompt, "", purpose).masked()));
    }

    /// A password typed for a new archive: ask again, or compress if both match.
    pub(super) fn on_compress_password(
        &mut self,
        items: Vec<PathBuf>,
        name: String,
        first: Option<String>,
        password: String,
    ) {
        match first {
            None => self.ask_compress_password(items, name, Some(password)),
            Some(first) if first == password => self.compress(items, &name, Some(password)),
            Some(_) => {
                self.error("the passwords don't match");
                self.ask_compress_password(items, name, None);
            }
        }
    }

    pub(super) fn compress(&mut self, items: Vec<PathBuf>, name: &str, password: Option<String>) {
        // Beside the items when they share a folder (in a tree, maybe not
        // the panel's own), so tar can name them relative to it.
        let cwd = match items.first().and_then(|p| p.parent()) {
            Some(dir) if items.iter().all(|p| p.parent() == Some(dir)) => dir.to_path_buf(),
            _ => self.panel().cwd.clone(),
        };
        let target = cwd.join(name);
        if target.exists() {
            return self.error(format!("{name} already exists"));
        }
        if compress::can_write(name) {
            self.panel_mut().clear_marks();
            let label = format!("Compress {name}");
            self.jobs.spawn(label, move |progress| {
                compress::create_archive_with_password(&items, &target, password.as_deref(), progress)
            });
            return;
        }
        if password.is_some() {
            return self.error(format!("{name}: only .zip archives can have a password"));
        }
        // xz and zstd: the system tar compresses these far better.
        if ArchiveFormat::detect(name).is_some() && strata_core::nas::which("tar") {
            self.panel_mut().clear_marks();
            let mut argv = vec!["tar".to_string(), "-caf".into(), name.to_string(), "--".into()];
            argv.extend(
                items
                    .iter()
                    .map(|p| p.strip_prefix(&cwd).map(strata_core::util::posix).unwrap_or_else(|_| file_name(p))),
            );
            return self.queue_external(External::Run {
                argv,
                cwd: Some(cwd),
                wait: Wait::OnFailure,
                after: After::Reload,
            });
        }
        self.error(format!("can't create {name}: use .zip, .tar, .tar.gz, .tar.bz2, .tar.xz or .tar.zst"));
    }

    /// `:extract`: unpacks the hovered archive into the focused directory,
    /// inside a folder named after it unless it holds a single item.
    pub(super) fn extract_command(&mut self) {
        let Some(entry) = self.panel().hovered().cloned() else {
            return self.notify("extract: hover an archive", Level::Warn);
        };
        if !self.panel().vfs.is_local() || entry.is_dir() {
            return self.notify("extract: hover a local archive", Level::Warn);
        }
        // Next to the archive, which in a tree can be below the panel's folder.
        let dest = entry.path.parent().map(PathBuf::from).unwrap_or_else(|| self.panel().cwd.clone());
        if ArchiveFormat::detect(&entry.name).is_some() {
            self.info(format!("reading {}…", entry.name));
            return self.open_archive_for(entry.path, ArchiveUse::Extract(dest), None);
        }
        // 7z, rar and others: 7-Zip handles most of them.
        if strata_core::nas::which("7z") {
            let folder = unique_name(&archive_stem(&entry.name), |n| dest.join(n).exists());
            let argv = vec!["7z".into(), "x".into(), entry.path.display().to_string(), format!("-o{folder}")];
            return self.queue_external(External::Run {
                argv,
                cwd: Some(dest),
                wait: Wait::OnFailure,
                after: After::Reload,
            });
        }
        self.error(format!("can't extract {}: install 7-Zip (7z) for this format", entry.name));
    }

    pub(super) fn on_archive_to_extract(&mut self, file: PathBuf, dest: PathBuf, result: Result<VfsRef, String>) {
        let archive = match result {
            Ok(vfs) => vfs,
            Err(e) => return self.error(format!("extract {}: {e}", file.display())),
        };
        self.offer_to_remember_password(&file);
        let top = match archive.read_dir(Path::new("/")) {
            Ok(entries) => entries,
            Err(e) => return self.error(format!("extract {}: {e:#}", file.display())),
        };
        if top.is_empty() {
            return self.info("the archive is empty");
        }
        let dest_dir = if top.len() == 1 {
            dest
        } else {
            let name = unique_name(&archive_stem(&file_name(&file)), |n| dest.join(n).exists());
            let folder = dest.join(name);
            if let Err(e) = std::fs::create_dir(&folder) {
                return self.error(format!("creating {}: {e}", folder.display()));
            }
            folder
        };
        let transfer = Transfer {
            mode: TransferMode::Copy,
            src: archive,
            sources: top.into_iter().map(|e| e.path).collect(),
            dst: self.local.clone(),
            dest_dir,
            conflict: Conflict::KeepBoth,
            resolutions: Default::default(),
            targets: Default::default(),
            options: Default::default(),
        };
        self.start_transfer(transfer);
    }
}

impl App {
    /// Indexes an archive in the background, with the password typed, or
    /// else any saved in the keychain, then sends it on for `purpose`.
    pub(super) fn open_archive_for(&mut self, file: PathBuf, purpose: ArchiveUse, password: Option<String>) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let typed = password.is_some();
            let mut result = ArchiveVfs::open_with_password(&file, password.as_deref());
            // The keychain talks D-Bus, so only ask it for archives that need it.
            if !typed && locked(&result) == Some(Locked::NeedsPassword) {
                if let Some(saved) = strata_core::secrets::get_archive(&file) {
                    result = ArchiveVfs::open_with_password(&file, Some(&saved));
                }
            }
            if let Some(reason) = locked(&result) {
                let _ = tx.send(AppEvent::ArchiveLocked { file, purpose, reason });
                return;
            }
            let result = result.map(|v| Arc::new(v) as VfsRef).map_err(|e| format!("{e:#}"));
            let _ = tx.send(match purpose {
                ArchiveUse::Browse(panel) => AppEvent::ArchiveOpened { panel, file, result },
                ArchiveUse::Extract(dest) => AppEvent::ArchiveToExtract { file, dest, result },
            });
        });
    }

    /// An encrypted archive had no password, or the wrong one: ask for it.
    pub(super) fn on_archive_locked(&mut self, file: PathBuf, purpose: ArchiveUse, reason: Locked) {
        let name = file_name(&file);
        if reason == Locked::WrongPassword {
            let typed = self.pending_archive_password.take().is_some_and(|(f, _)| f == file);
            self.error(if typed {
                format!("wrong password for {name}")
            } else {
                format!("the saved password for {name} no longer works")
            });
        }
        let prompt = format!("Password for {name}");
        let input = InputState::new(prompt, "", InputPurpose::ArchivePassword { file, purpose }).masked();
        self.show_overlay(Overlay::Input(input));
    }

    /// A password was typed for an encrypted archive: try it.
    pub(super) fn on_archive_password(&mut self, file: PathBuf, purpose: ArchiveUse, password: String) {
        self.info(format!("reading {}…", file_name(&file)));
        self.pending_archive_password = Some((file.clone(), password.clone()));
        self.open_archive_for(file, purpose, Some(password));
    }

    /// Offers to save a password that just opened `file` in the keychain.
    pub(super) fn offer_to_remember_password(&mut self, file: &Path) {
        let Some((pending, password)) = self.pending_archive_password.take() else { return };
        if pending == file {
            let message = format!("Remember the password for {} in the system keychain?", file_name(file));
            let action = Confirm::SaveArchivePassword { file: pending, password };
            self.show_overlay(Overlay::Confirm(ConfirmState { message, action }));
        }
    }

    /// Saves or (without a password) forgets an archive's password, off the
    /// UI thread: D-Bus can wait on an unlock prompt.
    pub(super) fn archive_keychain(&mut self, file: PathBuf, password: Option<String>) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let name = file_name(&file);
            let result = match &password {
                Some(password) => strata_core::secrets::set_archive(&file, password)
                    .map(|()| format!("saved the password for {name} in the system keychain")),
                None => strata_core::secrets::delete_archive(&file)
                    .map(|()| format!("removed the saved password for {name}")),
            };
            let (message, level) = match result {
                Ok(message) => (message, Level::Info),
                Err(e) => (format!("{e:#}"), Level::Error),
            };
            let _ = tx.send(AppEvent::Notify { message, level });
        });
    }

    /// `:forget` without a connection name: the hovered archive's password.
    pub(super) fn forget_archive_password(&mut self) {
        match self.panel().hovered().filter(|e| ArchiveFormat::detect(&e.name).is_some()) {
            Some(entry) if self.panel().vfs.is_local() => {
                let file = entry.path.clone();
                self.archive_keychain(file, None);
            }
            _ => self.notify("forget: give a connection name, or hover a local archive", Level::Warn),
        }
    }
}

fn locked<T>(result: &anyhow::Result<T>) -> Option<Locked> {
    result.as_ref().err().and_then(|e| e.downcast_ref::<Locked>()).copied()
}

/// `--password photos.zip` → `(true, "photos.zip")`.
fn split_password_flag(args: &str) -> (bool, &str) {
    let args = args.trim();
    for flag in ["--password", "-p"] {
        if let Some(rest) = args.strip_prefix(flag) {
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                return (true, rest.trim());
            }
        }
        if let Some(rest) = args.strip_suffix(flag) {
            if rest.ends_with(char::is_whitespace) {
                return (true, rest.trim());
            }
        }
    }
    (false, args)
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "archive".into())
}

/// `photos.tar.gz` → `photos`.
fn archive_stem(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for ext in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst"] {
        if lower.ends_with(ext) && name.len() > ext.len() {
            return name[..name.len() - ext.len()].to_string();
        }
    }
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => format!("{name} (extracted)"),
    }
}

#[cfg(test)]
mod tests {
    use super::{archive_stem, split_password_flag};

    #[test]
    fn password_flag_goes_either_side() {
        assert_eq!(split_password_flag("--password"), (true, ""));
        assert_eq!(split_password_flag("--password secret.zip"), (true, "secret.zip"));
        assert_eq!(split_password_flag("secret.zip -p"), (true, "secret.zip"));
        assert_eq!(split_password_flag(" photos.zip "), (false, "photos.zip"));
        assert_eq!(split_password_flag("-photos.zip"), (false, "-photos.zip"));
    }

    #[test]
    fn stems_drop_archive_extensions() {
        assert_eq!(archive_stem("photos.tar.gz"), "photos");
        assert_eq!(archive_stem("Backup.TAR.BZ2"), "Backup");
        assert_eq!(archive_stem("app.zip"), "app");
        assert_eq!(archive_stem("v1.2.7z"), "v1.2");
        assert_eq!(archive_stem("README"), "README (extracted)");
    }
}
