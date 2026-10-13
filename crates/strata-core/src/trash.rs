//! Browsing the system trash: list, restore, delete for good and empty.
//!
//! Linux, the BSDs and Windows go through the `trash` crate. macOS has no
//! API to list the trash, so there `~/.Trash` is read directly, with the
//! original locations taken from the "put back" records Finder keeps in its
//! `.DS_Store`. Items trashed without Finder have no such record; their
//! original location is unknown.

#[cfg(any(target_os = "macos", test))]
mod dsstore;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;

/// One item in the trash.
#[derive(Debug, Clone)]
pub struct TrashedItem {
    pub name: String,
    /// Where it was deleted from. Empty when unknown (only on macOS); set
    /// it to choose where [`restore`] puts the item.
    pub original: PathBuf,
    pub deleted: SystemTime,
    // Only read where the trash can be browsed.
    #[cfg_attr(
        not(any(
            target_os = "windows",
            target_os = "macos",
            all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
        )),
        allow(dead_code)
    )]
    inner: Inner,
}

impl TrashedItem {
    /// Where restoring puts it back.
    pub fn original_path(&self) -> PathBuf {
        self.original.join(&self.name)
    }

    /// Whether the trash recorded where the item was deleted from.
    pub fn location_known(&self) -> bool {
        !self.original.as_os_str().is_empty()
    }
}

pub const SUPPORTED: bool = cfg!(any(
    target_os = "windows",
    target_os = "macos",
    all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
));

#[cfg(any(
    target_os = "windows",
    all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
))]
mod imp {
    use super::*;
    use anyhow::{bail, Context};
    use std::time::{Duration, UNIX_EPOCH};
    use trash::os_limited;

    pub type Inner = trash::TrashItem;

    pub fn list() -> Result<Vec<TrashedItem>> {
        Ok(os_limited::list()
            .context("cannot read the trash")?
            .into_iter()
            .map(|item| TrashedItem {
                name: item.name.to_string_lossy().into_owned(),
                original: item.original_parent.clone(),
                deleted: UNIX_EPOCH + Duration::from_secs(item.time_deleted.max(0) as u64),
                inner: item,
            })
            .collect())
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

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use anyhow::{bail, Context};
    use std::fs;
    use std::io::ErrorKind;
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, UNIX_EPOCH};

    /// Where the item sits in the trash.
    pub type Inner = PathBuf;

    fn trash_dir() -> Result<PathBuf> {
        Ok(dirs::home_dir().context("no home directory")?.join(".Trash"))
    }

    pub fn list() -> Result<Vec<TrashedItem>> {
        let dir = trash_dir()?;
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) if e.kind() == ErrorKind::PermissionDenied => bail!(
                "cannot read the trash: give your terminal Full Disk Access in \
                 System Settings › Privacy & Security"
            ),
            Err(e) => return Err(e).context("cannot read the trash"),
        };
        let put_back = fs::read(dir.join(".DS_Store")).map(|data| dsstore::put_back(&data)).unwrap_or_default();
        let mut items = Vec::new();
        for entry in entries {
            let entry = entry.context("cannot read the trash")?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if file_name == ".DS_Store" || file_name == ".localized" {
                continue;
            }
            let record = put_back.get(&file_name);
            // Moving an item into the trash changes its inode, so the change
            // time is (nearly always) when it was deleted.
            let deleted = entry
                .metadata()
                .map(|m| {
                    UNIX_EPOCH + Duration::new(m.ctime().max(0) as u64, m.ctime_nsec().clamp(0, 999_999_999) as u32)
                })
                .unwrap_or(UNIX_EPOCH);
            items.push(TrashedItem {
                name: record.and_then(|r| r.name.clone()).unwrap_or_else(|| file_name.clone()),
                // `ptbL` is relative to the volume root; ~/.Trash only holds
                // items from the boot volume.
                original: record.and_then(|r| r.parent.as_deref()).map(|p| Path::new("/").join(p)).unwrap_or_default(),
                deleted,
                inner: entry.path(),
            });
        }
        Ok(items)
    }

    pub fn restore(items: Vec<TrashedItem>) -> Result<()> {
        for item in &items {
            if !item.location_known() {
                bail!("the original location of {} is unknown", item.name);
            }
            let target = item.original_path();
            if fs::symlink_metadata(&target).is_ok() {
                bail!("{} already exists", target.display());
            }
        }
        for item in items {
            let target = item.original_path();
            fs::create_dir_all(&item.original).with_context(|| format!("create {}", item.original.display()))?;
            fs::rename(&item.inner, &target).with_context(|| format!("cannot restore {}", item.name))?;
        }
        Ok(())
    }

    pub fn purge(items: Vec<TrashedItem>) -> Result<()> {
        for item in items {
            let is_dir = fs::symlink_metadata(&item.inner).map(|m| m.is_dir()).unwrap_or(false);
            match is_dir {
                true => fs::remove_dir_all(&item.inner),
                false => fs::remove_file(&item.inner),
            }
            .with_context(|| format!("cannot delete {} from the trash", item.name))?;
        }
        Ok(())
    }
}

#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
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
    let mut items = imp::list()?;
    items.sort_by_key(|i| std::cmp::Reverse(i.deleted));
    Ok(items)
}

/// Puts items back where they were deleted from (see
/// [`TrashedItem::original`]). Refuses when something already exists there.
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

/// The latest version of each of `paths` trashed since `since`, for undo.
///
/// An item whose original location is unknown matches a path with the same
/// name, and takes that path's folder as its original location: Finder may
/// not have written its put-back record yet.
pub fn trashed_since(paths: &[PathBuf], since: SystemTime) -> Result<Vec<TrashedItem>> {
    // Trash times have a granularity of a second on some platforms.
    let since = since.checked_sub(std::time::Duration::from_secs(2)).unwrap_or(since);
    Ok(pick_latest(list()?, paths, since))
}

fn pick_latest(items: Vec<TrashedItem>, paths: &[PathBuf], since: SystemTime) -> Vec<TrashedItem> {
    let mut chosen: Vec<TrashedItem> = Vec::new();
    // `items` is newest first, so the first match for a path is its latest version.
    for mut item in items.into_iter().filter(|i| i.deleted >= since) {
        let path = match item.location_known() {
            true => paths.iter().find(|p| **p == item.original_path()),
            false => paths.iter().find(|p| p.file_name().is_some_and(|n| *n == *item.name)),
        };
        let Some(path) = path else { continue };
        if chosen.iter().any(|c| c.original_path() == *path) {
            continue;
        }
        if !item.location_known() {
            item.original = path.parent().map(Path::to_path_buf).unwrap_or_default();
        }
        chosen.push(item);
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn item(original: &str, name: &str, secs: u64) -> TrashedItem {
        TrashedItem {
            name: name.into(),
            original: original.into(),
            deleted: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            inner: test_inner(),
        }
    }

    #[cfg(any(
        target_os = "windows",
        all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
    ))]
    fn test_inner() -> Inner {
        trash::TrashItem {
            id: Default::default(),
            name: Default::default(),
            original_parent: Default::default(),
            time_deleted: 0,
        }
    }

    #[cfg(target_os = "macos")]
    fn test_inner() -> Inner {
        PathBuf::new()
    }

    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))
    )))]
    fn test_inner() -> Inner {
        Inner
    }

    #[test]
    fn picks_the_latest_trashed_version_of_each_path() {
        let items = vec![
            item("/d", "a", 300),
            item("/d", "a", 200),
            item("/d", "b", 250),
            item("/elsewhere", "a", 260),
            item("/d", "c", 50),
        ];
        let paths = [PathBuf::from("/d/a"), PathBuf::from("/d/b"), PathBuf::from("/d/c")];
        let since = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let chosen = pick_latest(items, &paths, since);
        let picked: Vec<_> = chosen.iter().map(|i| (i.original_path(), i.deleted)).collect();
        assert_eq!(
            picked,
            [
                (PathBuf::from("/d/a"), SystemTime::UNIX_EPOCH + Duration::from_secs(300)),
                (PathBuf::from("/d/b"), SystemTime::UNIX_EPOCH + Duration::from_secs(250)),
            ]
        );
    }

    #[test]
    fn unknown_locations_match_by_name() {
        let items = vec![item("", "a", 300), item("", "zzz", 300)];
        let chosen = pick_latest(items, &[PathBuf::from("/d/a")], SystemTime::UNIX_EPOCH);
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].original_path(), PathBuf::from("/d/a"));
        assert!(chosen[0].location_known());
    }
}
