//! Creating zip and tar archives (plain, gz, bz2) from local files, and
//! AES-256 encrypted zips.
//!
//! xz and zstd archives are left to the system `tar`: the pure-Rust
//! encoders available barely compress (xz) or can panic mid-stream (zstd).

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use anyhow::{bail, Context, Result};

use crate::jobs::Progress;
use crate::vfs::{ArchiveFormat, Compression};

/// Whether strata can write archives named like `name` itself.
pub fn can_write(name: &str) -> bool {
    matches!(
        ArchiveFormat::detect(name),
        Some(ArchiveFormat::Zip | ArchiveFormat::Tar(Compression::None | Compression::Gzip | Compression::Bzip2))
    )
}

/// Packs `items` (files or folders, stored under their own names) into
/// `target`. The archive is written next to `target` and renamed into
/// place once complete, so a failed or cancelled run leaves nothing behind.
pub fn create_archive(items: &[PathBuf], target: &Path, progress: &Progress) -> Result<()> {
    create_archive_with_password(items, target, None, progress)
}

/// Like [`create_archive`]; with a password, the contents of a zip are
/// encrypted with AES-256 (names stay readable, as zip stores them).
pub fn create_archive_with_password(
    items: &[PathBuf],
    target: &Path,
    password: Option<&str>,
    progress: &Progress,
) -> Result<()> {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let format = ArchiveFormat::detect(&name).filter(|_| can_write(&name));
    let Some(format) = format else {
        bail!("strata can't write {name}: use .zip, .tar, .tar.gz or .tar.bz2");
    };
    if password.is_some() && format != ArchiveFormat::Zip {
        bail!("only zip archives can have a password");
    }
    if target.exists() {
        bail!("{} already exists", target.display());
    }

    progress.set_current("scanning…");
    let mut total = 0;
    for item in items {
        total += local_size(item, progress)?;
    }
    progress.total_bytes.store(total, Ordering::Relaxed);
    progress.total_items.store(items.len(), Ordering::Relaxed);

    let partial = target.with_file_name(format!(".{name}.part"));
    let result = File::create(&partial)
        .with_context(|| format!("creating {}", partial.display()))
        .and_then(|file| write(format, BufWriter::new(file), items, password, progress));
    match result {
        Ok(()) => fs::rename(&partial, target).with_context(|| format!("creating {}", target.display())),
        Err(e) => {
            let _ = fs::remove_file(&partial);
            Err(e)
        }
    }
}

fn write(
    format: ArchiveFormat,
    out: BufWriter<File>,
    items: &[PathBuf],
    password: Option<&str>,
    progress: &Progress,
) -> Result<()> {
    match format {
        ArchiveFormat::Zip => {
            let mut zip = zip::ZipWriter::new(out);
            for item in items {
                add_zip(&mut zip, item, &file_name(item)?, password, progress)?;
                progress.item_done();
            }
            zip.finish()?.flush()?;
        }
        ArchiveFormat::Tar(Compression::None) => write_tar(out, items, progress)?.flush()?,
        ArchiveFormat::Tar(Compression::Gzip) => {
            write_tar(flate2::write::GzEncoder::new(out, flate2::Compression::default()), items, progress)?
                .finish()?
                .flush()?
        }
        ArchiveFormat::Tar(Compression::Bzip2) => {
            write_tar(bzip2::write::BzEncoder::new(out, bzip2::Compression::default()), items, progress)?
                .finish()?
                .flush()?
        }
        ArchiveFormat::Tar(_) => unreachable!("checked by can_write"),
    }
    Ok(())
}

fn file_name(path: &Path) -> Result<String> {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).with_context(|| format!("no name: {}", path.display()))
}

fn write_tar<W: Write>(out: W, items: &[PathBuf], progress: &Progress) -> Result<W> {
    let mut tar = tar::Builder::new(out);
    tar.follow_symlinks(false);
    for item in items {
        add_tar(&mut tar, item, Path::new(&file_name(item)?), progress)?;
        progress.item_done();
    }
    Ok(tar.into_inner()?)
}

fn add_tar<W: Write>(tar: &mut tar::Builder<W>, path: &Path, name: &Path, progress: &Progress) -> Result<()> {
    progress.check()?;
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        tar.append_dir(name, path)?;
        let mut children: Vec<_> = fs::read_dir(path)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        children.sort();
        for child in children {
            add_tar(tar, &child, &name.join(file_name(&child)?), progress)?;
        }
    } else if meta.is_file() {
        progress.set_current(name.display().to_string());
        let mut header = tar::Header::new_gnu();
        header.set_metadata(&meta);
        let reader = Counted { inner: File::open(path)?, progress };
        tar.append_data(&mut header, name, reader).with_context(|| format!("adding {}", path.display()))?;
    } else {
        // Links and special files are stored as they are.
        tar.append_path_with_name(path, name)?;
    }
    Ok(())
}

fn add_zip<W: Write + io::Seek>(
    zip: &mut zip::ZipWriter<W>,
    path: &Path,
    name: &str,
    password: Option<&str>,
    progress: &Progress,
) -> Result<()> {
    progress.check()?;
    let meta = fs::symlink_metadata(path)?;
    let mut options = zip::write::SimpleFileOptions::default().large_file(meta.len() >= u32::MAX as u64);
    if let Some(time) = meta.modified().ok().and_then(zip_time) {
        options = options.last_modified_time(time);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        options = options.unix_permissions(meta.permissions().mode() & 0o7777);
    }
    // Folders have no data to encrypt.
    let data_options = match password {
        Some(password) => options.with_aes_encryption(zip::AesMode::Aes256, password),
        None => options,
    };
    if meta.is_dir() {
        zip.add_directory(format!("{name}/"), options)?;
        let mut children: Vec<_> = fs::read_dir(path)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        children.sort();
        for child in children {
            add_zip(zip, &child, &format!("{name}/{}", file_name(&child)?), password, progress)?;
        }
    } else if meta.file_type().is_symlink() {
        let target = fs::read_link(path)?;
        zip.add_symlink(name, target.to_string_lossy(), data_options)?;
    } else if meta.is_file() {
        progress.set_current(name.to_string());
        zip.start_file(name, data_options)?;
        io::copy(&mut Counted { inner: File::open(path)?, progress }, zip)
            .with_context(|| format!("adding {}", path.display()))?;
    }
    Ok(())
}

/// A file time as zip stores it: local time, 1980 to 2107.
fn zip_time(time: std::time::SystemTime) -> Option<zip::DateTime> {
    use chrono::{Datelike, Local, Timelike};
    let t = chrono::DateTime::<Local>::from(time);
    let narrow = |n: u32| u8::try_from(n).ok();
    zip::DateTime::from_date_and_time(
        u16::try_from(t.year()).ok()?,
        narrow(t.month())?,
        narrow(t.day())?,
        narrow(t.hour())?,
        narrow(t.minute())?,
        narrow(t.second())?,
    )
    .ok()
}

/// Bytes of the regular files below `path`.
fn local_size(path: &Path, progress: &Progress) -> Result<u64> {
    progress.check()?;
    let meta = fs::symlink_metadata(path).with_context(|| format!("reading {}", path.display()))?;
    if !meta.is_dir() {
        return Ok(if meta.is_file() { meta.len() } else { 0 });
    }
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        total += local_size(&entry?.path(), progress)?;
    }
    Ok(total)
}

/// Reports bytes read to the job, and stops when it is cancelled.
struct Counted<'a, R> {
    inner: R,
    progress: &'a Progress,
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.progress.check().map_err(io::Error::other)?;
        let n = self.inner.read(buf)?;
        self.progress.add_bytes(n as u64);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::ArchiveVfs;
    use crate::Vfs;

    fn sample() -> (tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("folder/inner")).unwrap();
        fs::write(dir.path().join("folder/inner/a.txt"), "alpha").unwrap();
        fs::write(dir.path().join("b.txt"), "bravo!").unwrap();
        let items = vec![dir.path().join("folder"), dir.path().join("b.txt")];
        (dir, items)
    }

    #[test]
    fn written_archives_read_back() {
        for name in ["out.zip", "out.tar", "out.tar.gz", "out.tar.bz2"] {
            let (dir, items) = sample();
            let target = dir.path().join(name);
            let p = Progress::default();
            create_archive(&items, &target, &p).unwrap();
            assert_eq!(p.done_bytes.load(Ordering::Relaxed), 11, "{name}");

            let archive = ArchiveVfs::open(&target).unwrap();
            let mut text = String::new();
            archive.reader(Path::new("/folder/inner/a.txt")).unwrap().read_to_string(&mut text).unwrap();
            assert_eq!(text, "alpha", "{name}");
            assert!(archive.exists(Path::new("/b.txt")), "{name}");
            assert!(!dir.path().join(format!(".{name}.part")).exists());
            let modified = archive.stat(Path::new("/b.txt")).unwrap().modified.unwrap();
            let age = std::time::SystemTime::now().duration_since(modified).unwrap_or_default();
            assert!(age.as_secs() < 120, "{name} keeps file times");
        }
    }

    #[test]
    fn refuses_unsupported_and_existing_targets() {
        let (dir, items) = sample();
        let p = Progress::default();
        assert!(create_archive(&items, &dir.path().join("out.tar.xz"), &p).is_err());
        assert!(create_archive(&items, &dir.path().join("b.txt"), &p).is_err());
        assert!(!can_write("x.7z"));
        assert!(can_write("x.TGZ"));
    }

    #[test]
    fn encrypted_zips_round_trip() {
        use crate::ops::{Conflict, Transfer, TransferMode};
        use crate::vfs::Locked;
        use std::sync::Arc;

        let (dir, mut items) = sample();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("b.txt", dir.path().join("link")).unwrap();
            items.push(dir.path().join("link"));
        }
        let target = dir.path().join("secret.zip");
        create_archive_with_password(&items, &target, Some("correct horse"), &Progress::default()).unwrap();
        // The contents are not stored in the clear.
        let raw = fs::read(&target).unwrap();
        assert!(!raw.windows(5).any(|w| w == b"alpha"));

        let locked = |password| ArchiveVfs::open_with_password(&target, password).unwrap_err();
        assert_eq!(ArchiveVfs::open(&target).unwrap_err().downcast_ref(), Some(&Locked::NeedsPassword));
        assert_eq!(locked(Some("wrong")).downcast_ref(), Some(&Locked::WrongPassword));
        assert_eq!(locked(Some("")).downcast_ref(), Some(&Locked::WrongPassword));

        let archive = ArchiveVfs::open_with_password(&target, Some("correct horse")).unwrap();
        #[cfg(unix)]
        assert!(archive.stat(Path::new("/link")).unwrap().is_symlink());
        let vfs: crate::VfsRef = Arc::new(archive);
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        Transfer {
            mode: TransferMode::Copy,
            src: vfs.clone(),
            sources: vfs.read_dir(Path::new("/")).unwrap().into_iter().map(|e| e.path).collect(),
            dst: Arc::new(crate::vfs::LocalVfs),
            dest_dir: out.clone(),
            conflict: Conflict::KeepBoth,
            resolutions: Default::default(),
            targets: Default::default(),
            options: Default::default(),
        }
        .run(&Progress::default())
        .unwrap();
        assert_eq!(fs::read_to_string(out.join("folder/inner/a.txt")).unwrap(), "alpha");
        assert_eq!(fs::read_to_string(out.join("b.txt")).unwrap(), "bravo!");
    }

    #[test]
    fn only_zips_take_a_password() {
        let (dir, items) = sample();
        let target = dir.path().join("out.tar.gz");
        let err = create_archive_with_password(&items, &target, Some("pw"), &Progress::default()).unwrap_err();
        assert!(err.to_string().contains("only zip"));
        assert!(!target.exists());
    }

    #[test]
    fn cancelling_leaves_nothing_behind() {
        let (dir, items) = sample();
        let target = dir.path().join("out.zip");
        let p = Progress::default();
        p.cancel();
        assert!(create_archive(&items, &target, &p).is_err());
        assert!(!target.exists());
        assert!(!dir.path().join(".out.zip.part").exists());
    }
}
