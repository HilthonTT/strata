use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::Vfs;
use crate::{Entry, EntryKind};

/// The machine's own filesystem.
#[derive(Debug, Default)]
pub struct LocalVfs;

impl LocalVfs {
    pub fn entry_for(path: &Path) -> Result<Entry> {
        let link_meta =
            fs::symlink_metadata(path).with_context(|| format!("stat {}", path.display()))?;
        let (kind, meta) = if link_meta.file_type().is_symlink() {
            let target = fs::metadata(path).ok();
            let to_dir = target.as_ref().is_some_and(fs::Metadata::is_dir);
            (EntryKind::Symlink { to_dir }, target.unwrap_or(link_meta))
        } else if link_meta.is_dir() {
            (EntryKind::Dir, link_meta)
        } else if link_meta.is_file() {
            (EntryKind::File, link_meta)
        } else {
            (EntryKind::Other, link_meta)
        };
        Ok(Entry {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned()),
            path: path.to_path_buf(),
            kind,
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified: meta.modified().ok(),
            mode: mode_of(&meta),
        })
    }
}

#[cfg(unix)]
fn mode_of(meta: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(meta: &fs::Metadata) -> Option<u32> {
    Some(if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    })
}

impl Vfs for LocalVfs {
    fn scheme(&self) -> &'static str {
        "local"
    }

    fn label(&self) -> String {
        "local".into()
    }

    fn is_local(&self) -> bool {
        true
    }

    fn home(&self) -> PathBuf {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<Entry>> {
        let iter = fs::read_dir(path).with_context(|| format!("read {}", path.display()))?;
        // Entries that vanish or deny access mid-listing are skipped, not fatal.
        Ok(iter
            .filter_map(|e| e.ok())
            .filter_map(|e| Self::entry_for(&e.path()).ok())
            .collect())
    }

    fn stat(&self, path: &Path) -> Result<Entry> {
        Self::entry_for(path)
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))
    }

    fn create_file(&self, path: &Path) -> Result<()> {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("create {}", path.display()))?;
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        fs::remove_file(path).with_context(|| format!("remove {}", path.display()))
    }

    fn remove_dir(&self, path: &Path) -> Result<()> {
        fs::remove_dir(path).with_context(|| format!("remove {}", path.display()))
    }

    fn remove_all(&self, path: &Path) -> Result<()> {
        let meta = fs::symlink_metadata(path)?;
        if meta.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
        .with_context(|| format!("remove {}", path.display()))
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        fs::rename(from, to)
            .with_context(|| format!("rename {} -> {}", from.display(), to.display()))
    }

    fn reader(&self, path: &Path) -> Result<Box<dyn Read + Send>> {
        Ok(Box::new(
            fs::File::open(path).with_context(|| format!("open {}", path.display()))?,
        ))
    }

    fn writer(&self, path: &Path) -> Result<Box<dyn Write + Send>> {
        Ok(Box::new(
            fs::File::create(path).with_context(|| format!("create {}", path.display()))?,
        ))
    }

    fn join(&self, dir: &Path, name: &str) -> PathBuf {
        dir.join(name)
    }

    fn trash(&self, path: &Path) -> Result<()> {
        trash::delete(path).with_context(|| format!("trash {}", path.display()))
    }

    fn exists(&self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok()
    }

    fn local_path(&self, path: &Path) -> Option<PathBuf> {
        Some(path.to_path_buf())
    }
}
