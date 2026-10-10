//! Browsing the system trash: list, restore, delete for good and empty.
//!
//! Supported on Linux, the BSDs and Windows; macOS offers no API to list
//! the trash.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;

/// One item in the trash.
#[derive(Debug, Clone)]
pub struct TrashedItem {
    pub name: String,
    /// Where it was deleted from.
    pub original: PathBuf,
    pub deleted: SystemTime,
    inner: Inner,
}

impl TrashedItem {
    /// Where restoring puts it back.
    pub fn original_path(&self) -> PathBuf {
        self.original.join(&self.name)
    }
}

pub const SUPPORTED: bool = cfg!(any(
    target_os = "windows",
    all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
));

#[cfg(any(
    target_os = "windows",
    all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
))]
mod imp {
    use super::*;
    use anyhow::{bail, Context};
    use trash::os_limited;

    pub type Inner = trash::TrashItem;

    pub fn list() -> Result<Vec<TrashedItem>> {
        let mut items: Vec<TrashedItem> = os_limited::list()
            .context("cannot read the trash")?
            .into_iter()
            .map(|item| TrashedItem {
                name: item.name.to_string_lossy().into_owned(),
                original: item.original_parent.clone(),
                deleted: UNIX_EPOCH + Duration::from_secs(item.time_deleted.max(0) as u64),
                inner: item,
            })
            .collect();
        items.sort_by_key(|i| std::cmp::Reverse(i.deleted));
        Ok(items)
    }

    pub fn restore(items: Vec<TrashedItem>) -> Result<()> {
        for item in &items {
            let target = item.original_path();
            if std::fs::symlink_metadata(&target).is_ok() {
                bail!("{} already exists", target.display());
            }
        }
        os_limited::restore_all(items.into_iter().map(|i| i.inner)).context("cannot restore from the trash")
    }

    pub fn purge(items: Vec<TrashedItem>) -> Result<()> {
        os_limited::purge_all(items.into_iter().map(|i| i.inner)).context("cannot delete from the trash")
    }
}

#[cfg(not(any(
    target_os = "windows",
    all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
)))]
mod imp {
    use super::*;
    use anyhow::bail;

    #[derive(Debug, Clone)]
    pub struct Inner;

    pub fn list() -> Result<Vec<TrashedItem>> {
        bail!("browsing the trash is not supported on this platform")
    }

    pub fn restore(_items: Vec<TrashedItem>) -> Result<()> {
        bail!("restoring from the trash is not supported on this platform")
    }

    pub fn purge(_items: Vec<TrashedItem>) -> Result<()> {
        bail!("deleting from the trash is not supported on this platform")
    }
}

use imp::Inner;

/// Everything in the trash, most recently deleted first.
pub fn list() -> Result<Vec<TrashedItem>> {
    imp::list()
}

/// Puts items back where they were deleted from. Refuses when something
/// already exists there.
pub fn restore(items: Vec<TrashedItem>) -> Result<()> {
    imp::restore(items)
}

/// Deletes items from the trash for good.
pub fn purge(items: Vec<TrashedItem>) -> Result<()> {
    imp::purge(items)
}

/// Deletes everything in the trash for good. Returns how many items.
pub fn empty() -> Result<usize> {
    let items = list()?;
    let count = items.len();
    purge(items)?;
    Ok(count)
}
