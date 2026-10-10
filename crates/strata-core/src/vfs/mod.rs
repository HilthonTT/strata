//! A small virtual filesystem abstraction so panels can browse local disks,
//! SFTP servers and Docker containers through one interface.

mod docker;
mod local;
#[cfg(feature = "sftp")]
mod sftp;

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Result};

use crate::Entry;

pub use docker::DockerVfs;
pub use local::LocalVfs;
#[cfg(feature = "sftp")]
pub use sftp::{SftpAuth, SftpVfs};

pub type VfsRef = Arc<dyn Vfs>;

/// Operations every filesystem backend provides.
///
/// Implementations must be cheap to share across threads: file operations
/// run on background job threads while the UI keeps browsing.
pub trait Vfs: Send + Sync + fmt::Debug {
    /// Short scheme such as `local`, `sftp` or `docker`.
    fn scheme(&self) -> &'static str;
    /// Human readable location, shown in panel titles.
    fn label(&self) -> String;
    fn is_local(&self) -> bool {
        false
    }
    /// Directory a fresh panel opens at.
    fn home(&self) -> PathBuf;

    fn read_dir(&self, path: &Path) -> Result<Vec<Entry>>;
    fn stat(&self, path: &Path) -> Result<Entry>;
    fn create_dir(&self, path: &Path) -> Result<()>;
    fn create_file(&self, path: &Path) -> Result<()>;
    fn remove_file(&self, path: &Path) -> Result<()>;
    /// Removes an empty directory.
    fn remove_dir(&self, path: &Path) -> Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> Result<()>;
    fn reader(&self, path: &Path) -> Result<Box<dyn Read + Send>>;
    fn writer(&self, path: &Path) -> Result<Box<dyn Write + Send>>;

    /// Joins a child name onto a directory path using the backend's rules.
    fn join(&self, dir: &Path, name: &str) -> PathBuf {
        crate::util::posix_join(dir, name)
    }

    fn parent(&self, path: &Path) -> Option<PathBuf> {
        path.parent().map(Path::to_path_buf)
    }

    /// Recursively removes a file or directory tree.
    fn remove_all(&self, path: &Path) -> Result<()> {
        let entry = self.stat(path)?;
        if entry.kind == crate::EntryKind::Dir {
            for child in self.read_dir(path)? {
                self.remove_all(&child.path)?;
            }
            self.remove_dir(path)
        } else {
            self.remove_file(path)
        }
    }

    /// Moves a path to the system trash, where the backend supports it.
    fn trash(&self, _path: &Path) -> Result<()> {
        bail!("{} does not support the trash", self.scheme())
    }

    fn exists(&self, path: &Path) -> bool {
        self.stat(path).is_ok()
    }

    /// Creates a symbolic link at `link` that points to `target`.
    fn symlink(&self, _target: &Path, _link: &Path) -> Result<()> {
        bail!("{} does not support symbolic links", self.scheme())
    }

    /// Creates a hard link at `link` to the existing file `original`.
    fn hard_link(&self, _original: &Path, _link: &Path) -> Result<()> {
        bail!("{} does not support hard links", self.scheme())
    }

    /// Path on the local disk, if this backend exposes one (used for
    /// previews, editors and shells).
    fn local_path(&self, _path: &Path) -> Option<PathBuf> {
        None
    }

    /// Command line that opens an interactive shell at `path`, for remote
    /// backends (`docker exec -it`, `ssh -t`). `None` for the local disk.
    fn shell_command(&self, _path: &Path) -> Option<Vec<String>> {
        None
    }
}

/// Identity check used to decide whether a move can be a cheap rename.
pub fn same_vfs(a: &VfsRef, b: &VfsRef) -> bool {
    Arc::ptr_eq(a, b) || (a.is_local() && b.is_local())
}
