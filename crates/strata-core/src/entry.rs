use std::path::PathBuf;
use std::time::SystemTime;

/// What kind of filesystem object an [`Entry`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    /// A symbolic link. `to_dir` is true when it resolves to a directory.
    Symlink {
        to_dir: bool,
    },
    Other,
}

/// A single item inside a directory, independent of the backing filesystem.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Unix permission bits, when the backend knows them.
    pub mode: Option<u32>,
}

impl Entry {
    /// True for directories and symlinks pointing at directories.
    pub fn is_dir(&self) -> bool {
        matches!(
            self.kind,
            EntryKind::Dir | EntryKind::Symlink { to_dir: true }
        )
    }

    pub fn is_symlink(&self) -> bool {
        matches!(self.kind, EntryKind::Symlink { .. })
    }

    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }

    pub fn is_executable(&self) -> bool {
        !self.is_dir() && self.mode.is_some_and(|m| m & 0o111 != 0)
    }

    /// Lower-cased extension without the dot, or an empty string.
    pub fn extension(&self) -> String {
        match self.name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
            _ => String::new(),
        }
    }
}
