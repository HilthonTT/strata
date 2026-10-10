//! Built-in `:compress` and `:extract`.
//!
//! Extracting opens the archive like browsing it does, then copies its
//! contents out as an ordinary transfer, so it gets progress, pause,
//! resume, undo and conflict handling for free.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use strata_core::compress;
use strata_core::ops::{Conflict, Transfer, TransferMode};
use strata_core::util::unique_name;
use strata_core::vfs::{ArchiveFormat, ArchiveVfs};
use strata_core::VfsRef;
use strata_plugin::Level;

use super::external::{After, External, Wait};
use super::overlay::{InputPurpose, InputState, Overlay};
use super::App;
use crate::event::AppEvent;

impl App {
    /// `:compress [name]`: packs the marked items (or the hovered one) into
    /// an archive in the focused directory, asking for a name if not given.
    pub(super) fn compress_command(&mut self, name: &str) {
        let items = self.panel().targets();
        if items.is_empty() {
            return self.notify("compress: nothing to compress", Level::Warn);
        }
        if !self.panel().vfs.is_local() {
            return self.notify("compress: only local files can be compressed", Level::Warn);
        }
        if !name.is_empty() {
            return self.compress(items, name);
        }
        let stem = match items.as_slice() {
            [one] => file_name(one),
            _ => file_name(&self.panel().cwd),
        };
        let input = InputState::new("Archive name", format!("{stem}.zip"), InputPurpose::Compress(items));
        self.overlay = Some(Overlay::Input(input));
    }

    pub(super) fn compress(&mut self, items: Vec<PathBuf>, name: &str) {
        let cwd = self.panel().cwd.clone();
        let target = cwd.join(name);
        if target.exists() {
            return self.error(format!("{name} already exists"));
        }
        if compress::can_write(name) {
            self.panel_mut().clear_marks();
            let label = format!("Compress {name}");
            self.jobs.spawn(label, move |progress| compress::create_archive(&items, &target, progress));
            return;
        }
        // xz and zstd: the system tar compresses these far better.
        if ArchiveFormat::detect(name).is_some() && strata_core::nas::which("tar") {
            self.panel_mut().clear_marks();
            let mut argv = vec!["tar".to_string(), "-caf".into(), name.to_string(), "--".into()];
            argv.extend(items.iter().map(|p| file_name(p)));
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
        let dest = self.panel().cwd.clone();
        if ArchiveFormat::detect(&entry.name).is_some() {
            self.info(format!("reading {}…", entry.name));
            let (tx, file) = (self.tx.clone(), entry.path);
            std::thread::spawn(move || {
                let result = ArchiveVfs::open(&file).map(|v| Arc::new(v) as VfsRef).map_err(|e| format!("{e:#}"));
                let _ = tx.send(AppEvent::ArchiveToExtract { file, dest, result });
            });
            return;
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
            targets: Default::default(),
        };
        self.start_transfer(transfer);
    }
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
    use super::archive_stem;

    #[test]
    fn stems_drop_archive_extensions() {
        assert_eq!(archive_stem("photos.tar.gz"), "photos");
        assert_eq!(archive_stem("Backup.TAR.BZ2"), "Backup");
        assert_eq!(archive_stem("app.zip"), "app");
        assert_eq!(archive_stem("v1.2.7z"), "v1.2");
        assert_eq!(archive_stem("README"), "README (extracted)");
    }
}
