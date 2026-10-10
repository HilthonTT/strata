//! Read-only browsing of zip and tar archives (plain, gz, bz2, xz, zst).
//!
//! Opening an archive reads its index once. Each archive then gets one
//! background thread that serves file contents in archive order, so copying
//! a whole `.tar.gz` out decompresses it once rather than once per file.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use super::Vfs;
use crate::{Entry, EntryKind};

/// The supported archive formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    Tar(Compression),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
    Bzip2,
    Xz,
    Zstd,
}

impl ArchiveFormat {
    /// The format of a file, judged by its name.
    pub fn detect(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        let ends = |suffixes: &[&str]| suffixes.iter().any(|s| lower.ends_with(s));
        Some(if ends(&[".zip", ".jar", ".war", ".ear", ".apk", ".aar", ".whl", ".cbz", ".xpi", ".nupkg", ".vsix"]) {
            ArchiveFormat::Zip
        } else if ends(&[".tar"]) {
            ArchiveFormat::Tar(Compression::None)
        } else if ends(&[".tar.gz", ".tgz"]) {
            ArchiveFormat::Tar(Compression::Gzip)
        } else if ends(&[".tar.bz2", ".tbz", ".tbz2"]) {
            ArchiveFormat::Tar(Compression::Bzip2)
        } else if ends(&[".tar.xz", ".txz"]) {
            ArchiveFormat::Tar(Compression::Xz)
        } else if ends(&[".tar.zst", ".tzst"]) {
            ArchiveFormat::Tar(Compression::Zstd)
        } else {
            return None;
        })
    }
}

/// Where an item's data lives inside the archive.
#[derive(Debug, Clone, Copy)]
enum Source {
    /// Index of the item in archive order.
    Item(usize),
    /// A symbolic link whose target is not a file in the archive.
    Dangling,
    /// Directories have no data.
    None,
}

/// A request for one item's data, answered through `reply`: chunks of
/// data, then an empty chunk once all of it has been sent.
type Request = (usize, SyncSender<io::Result<Vec<u8>>>);

/// Links in an archive and the paths they point to.
type Links = HashMap<PathBuf, PathBuf>;

/// A zip or tar archive on the local disk, browsed as a filesystem.
pub struct ArchiveVfs {
    file: PathBuf,
    format: ArchiveFormat,
    entries: HashMap<PathBuf, (Entry, Source)>,
    children: HashMap<PathBuf, Vec<PathBuf>>,
    /// Symbolic links to directories inside the archive, and their targets.
    dir_links: HashMap<PathBuf, PathBuf>,
    server: Mutex<Option<Sender<Request>>>,
}

impl std::fmt::Debug for ArchiveVfs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveVfs").field("file", &self.file).finish()
    }
}

impl ArchiveVfs {
    /// Reads the archive's index. This decompresses tar archives once, so
    /// call it off the UI thread.
    pub fn open(file: &Path) -> Result<Self> {
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let format = ArchiveFormat::detect(&name).with_context(|| format!("{name} is not a supported archive"))?;
        let mut vfs = Self {
            file: file.to_path_buf(),
            format,
            entries: HashMap::new(),
            children: HashMap::new(),
            dir_links: HashMap::new(),
            server: Mutex::new(None),
        };
        let root = Entry {
            name: "/".into(),
            path: PathBuf::from("/"),
            kind: EntryKind::Dir,
            size: 0,
            modified: None,
            mode: Some(0o755),
        };
        vfs.entries.insert(root.path.clone(), (root, Source::None));
        match format {
            ArchiveFormat::Zip => vfs.index_zip()?,
            ArchiveFormat::Tar(compression) => vfs.index_tar(compression)?,
        }
        Ok(vfs)
    }

    /// The archive file this filesystem reads from.
    pub fn file(&self) -> &Path {
        &self.file
    }

    fn index_zip(&mut self) -> Result<()> {
        let mut zip = zip::ZipArchive::new(BufReader::new(open(&self.file)?)).context("cannot read the zip index")?;
        let mut links = Links::new();
        for i in 0..zip.len() {
            let item = zip.by_index_raw(i)?;
            let Some(path) = clean_path(item.name()) else { continue };
            let mode = item.unix_mode();
            let kind = if item.is_dir() {
                EntryKind::Dir
            } else if mode.is_some_and(|m| m & 0o170000 == 0o120000) {
                EntryKind::Symlink { to_dir: false }
            } else {
                EntryKind::File
            };
            let modified = item.last_modified().and_then(|t| {
                civil_to_system(t.year().into(), t.month().into(), t.day().into(), t.hour(), t.minute(), t.second())
            });
            let size = item.size();
            drop(item);
            // A zip symlink stores its target as its data.
            links.remove(&path);
            if matches!(kind, EntryKind::Symlink { .. }) {
                let mut target = String::new();
                zip.by_index(i)?.take(4096).read_to_string(&mut target).context("cannot read a zip symlink")?;
                links.insert(path.clone(), resolve_symlink(&path, &target));
            }
            let source = if kind == EntryKind::Dir { Source::None } else { Source::Item(i) };
            self.insert(path, kind, size, modified, mode.map(|m| m & 0o7777), source);
        }
        self.resolve_links(links);
        Ok(())
    }

    fn index_tar(&mut self, compression: Compression) -> Result<()> {
        let mut archive = tar::Archive::new(decoder(&self.file, compression)?);
        let mut links = Links::new();
        for (i, item) in archive.entries().context("cannot read the tar archive")?.enumerate() {
            let item = item.context("cannot read the tar archive")?;
            let header = item.header();
            let Some(path) = clean_path(&item.path()?.to_string_lossy()) else { continue };
            let kind = match header.entry_type() {
                tar::EntryType::Directory => EntryKind::Dir,
                tar::EntryType::Symlink => EntryKind::Symlink { to_dir: false },
                tar::EntryType::Link => EntryKind::File,
                t if t.is_file() || t.is_contiguous() || t.is_gnu_sparse() => EntryKind::File,
                _ => EntryKind::Other,
            };
            let modified = header.mtime().ok().and_then(|s| UNIX_EPOCH.checked_add(Duration::from_secs(s)));
            let mode = header.mode().ok().map(|m| m & 0o7777);
            // Links take their data from their target, resolved below.
            let link_target = match header.entry_type() {
                tar::EntryType::Link => item.link_name()?.and_then(|t| clean_path(&t.to_string_lossy())),
                tar::EntryType::Symlink => item.link_name()?.map(|t| resolve_symlink(&path, &t.to_string_lossy())),
                _ => None,
            };
            // The last entry for a path wins, as when extracting.
            links.remove(&path);
            if let Some(target) = link_target {
                links.insert(path.clone(), target);
            }
            let source = if kind == EntryKind::Dir { Source::None } else { Source::Item(i) };
            self.insert(path, kind, item.size(), modified, mode, source);
        }
        self.resolve_links(links);
        Ok(())
    }

    /// Points links at the data of what they finally lead to, following
    /// links to links.
    fn resolve_links(&mut self, links: Links) {
        let mut resolved = Vec::new();
        for (link, target) in &links {
            let mut target = target;
            // Follow chains like `a -> b -> f`; a cycle ends up dangling.
            let mut hops = 0;
            while let Some(next) = links.get(target).filter(|_| hops < 40) {
                target = next;
                hops += 1;
            }
            let found = (hops < 40).then(|| self.entries.get(target)).flatten();
            resolved.push((link.clone(), target.clone(), found.map(|(e, s)| (e.size, e.is_dir(), *s))));
        }
        for (link, target, found) in resolved {
            let Some((entry, source)) = self.entries.get_mut(&link) else { continue };
            match found {
                Some((_, true, _)) if entry.is_symlink() => {
                    entry.kind = EntryKind::Symlink { to_dir: true };
                    entry.size = 0;
                    *source = Source::None;
                    self.dir_links.insert(link, target);
                }
                Some((size, false, data)) => {
                    *source = data;
                    entry.size = size;
                }
                _ => *source = Source::Dangling,
            }
        }
    }

    /// Adds an item, creating any parent directories the archive omits.
    fn insert(
        &mut self,
        path: PathBuf,
        kind: EntryKind,
        size: u64,
        modified: Option<SystemTime>,
        mode: Option<u32>,
        source: Source,
    ) {
        if let Some(parent) = path.parent().map(Path::to_path_buf) {
            if !self.entries.contains_key(&parent) {
                self.insert(parent.clone(), EntryKind::Dir, 0, modified, Some(0o755), Source::None);
            }
            let siblings = self.children.entry(parent).or_default();
            if !siblings.contains(&path) {
                siblings.push(path.clone());
            }
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let entry = Entry {
            name,
            path: path.clone(),
            kind,
            size: if kind == EntryKind::Dir { 0 } else { size },
            modified,
            mode,
        };
        self.entries.insert(path, (entry, source));
    }

    fn lookup(&self, path: &Path) -> Result<&(Entry, Source)> {
        self.entries.get(path).with_context(|| format!("{} is not in the archive", path.display()))
    }

    /// Sends a request to the archive's reader thread, starting it if needed.
    fn request(&self, index: usize) -> Result<PipeReader> {
        let mut server = self.server.lock().unwrap_or_else(|e| e.into_inner());
        for _ in 0..2 {
            let tx = server.get_or_insert_with(|| spawn_server(self.file.clone(), self.format));
            let (reply, rx) = mpsc::sync_channel(8);
            if tx.send((index, reply)).is_ok() {
                return Ok(PipeReader::new(rx));
            }
            // The thread stopped after an error: start a fresh one.
            *server = None;
        }
        bail!("cannot read {}", self.file.display())
    }
}

impl Vfs for ArchiveVfs {
    fn scheme(&self) -> &'static str {
        "archive"
    }

    fn label(&self) -> String {
        format!("archive:{}", self.file.file_name().map(|n| n.to_string_lossy()).unwrap_or_default())
    }

    fn read_only(&self) -> bool {
        true
    }

    fn home(&self) -> PathBuf {
        PathBuf::from("/")
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<Entry>> {
        let (entry, _) = self.lookup(path)?;
        if !entry.is_dir() {
            bail!("{} is not a directory", path.display());
        }
        let dir = self.dir_links.get(path).map(PathBuf::as_path).unwrap_or(path);
        let children = self.children.get(dir).map(Vec::as_slice).unwrap_or_default();
        Ok(children.iter().filter_map(|p| self.entries.get(p)).map(|(e, _)| e.clone()).collect())
    }

    fn stat(&self, path: &Path) -> Result<Entry> {
        Ok(self.lookup(path)?.0.clone())
    }

    fn create_dir(&self, _path: &Path) -> Result<()> {
        bail!(READ_ONLY)
    }

    fn create_file(&self, _path: &Path) -> Result<()> {
        bail!(READ_ONLY)
    }

    fn remove_file(&self, _path: &Path) -> Result<()> {
        bail!(READ_ONLY)
    }

    fn remove_dir(&self, _path: &Path) -> Result<()> {
        bail!(READ_ONLY)
    }

    fn rename(&self, _from: &Path, _to: &Path) -> Result<()> {
        bail!(READ_ONLY)
    }

    fn reader(&self, path: &Path) -> Result<Box<dyn Read + Send>> {
        match self.lookup(path)?.1 {
            Source::Item(index) => Ok(Box::new(self.request(index)?)),
            Source::Dangling => bail!("{} links outside the archive", path.display()),
            Source::None => bail!("{} is a directory", path.display()),
        }
    }

    fn writer(&self, _path: &Path) -> Result<Box<dyn Write + Send>> {
        bail!(READ_ONLY)
    }

    fn container(&self) -> Option<PathBuf> {
        Some(self.file.clone())
    }
}

const READ_ONLY: &str = "archives are read-only: copy items out of them instead";

fn open(file: &Path) -> Result<File> {
    File::open(file).with_context(|| format!("open {}", file.display()))
}

/// A decompressing reader for a tar archive.
fn decoder(file: &Path, compression: Compression) -> Result<Box<dyn Read + Send>> {
    let raw = BufReader::new(open(file)?);
    Ok(match compression {
        Compression::None => Box::new(raw),
        Compression::Gzip => Box::new(flate2::read::MultiGzDecoder::new(raw)),
        Compression::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(raw)),
        Compression::Zstd => Box::new(ruzstd::decoding::StreamingDecoder::new(raw).context("not a zstd stream")?),
        // lzma-rs only decodes into a writer: run it on a thread into a pipe.
        Compression::Xz => {
            let (tx, rx) = mpsc::sync_channel(8);
            thread::spawn(move || {
                let mut raw = raw;
                let mut out = PipeWriter(tx.clone());
                let _ = tx.send(match lzma_rs::xz_decompress(&mut raw, &mut out) {
                    Ok(()) => Ok(Vec::new()),
                    Err(e) => Err(io::Error::other(format!("xz: {e}"))),
                });
            });
            Box::new(PipeReader::new(rx))
        }
    })
}

/// Serves item data in archive order. Requests for items further on keep
/// reading the same stream; earlier ones restart it.
fn spawn_server(file: PathBuf, format: ArchiveFormat) -> Sender<Request> {
    let (tx, rx) = mpsc::channel::<Request>();
    thread::spawn(move || {
        let result = match format {
            ArchiveFormat::Zip => serve_zip(&file, &rx),
            ArchiveFormat::Tar(compression) => serve_tar(&file, compression, &rx),
        };
        // The error goes to the request that was being served, if any.
        let _ = result;
    });
    tx
}

fn serve_zip(file: &Path, rx: &Receiver<Request>) -> Result<()> {
    let mut zip = zip::ZipArchive::new(BufReader::new(open(file)?))?;
    while let Ok((index, reply)) = rx.recv() {
        match zip.by_index(index) {
            Ok(mut item) => {
                let size = item.size();
                send_all(&mut item, size, &reply)
            }
            Err(e) => {
                let _ = reply.send(Err(io::Error::other(e)));
            }
        }
    }
    Ok(())
}

fn serve_tar(file: &Path, compression: Compression, rx: &Receiver<Request>) -> Result<()> {
    let mut pending: Option<Request> = None;
    loop {
        let mut archive = tar::Archive::new(decoder(file, compression)?);
        let mut entries = archive.entries()?.enumerate();
        let mut next = 0;
        loop {
            let (index, reply) = match pending.take() {
                Some(request) => request,
                None => match rx.recv() {
                    Ok(request) => request,
                    Err(_) => return Ok(()),
                },
            };
            if index < next {
                pending = Some((index, reply));
                break;
            }
            loop {
                match entries.next() {
                    Some((i, Ok(mut item))) => {
                        next = i + 1;
                        if i == index {
                            let size = item.size();
                            send_all(&mut item, size, &reply);
                            break;
                        }
                    }
                    Some((i, Err(e))) => {
                        next = i + 1;
                        if i >= index {
                            let _ = reply.send(Err(e));
                            break;
                        }
                    }
                    None => {
                        let _ = reply.send(Err(io::Error::other("item not found in the archive")));
                        next = usize::MAX;
                        break;
                    }
                }
            }
        }
    }
}

/// Streams `size` bytes of a reader into a reply channel; stops when the
/// receiver is gone. Data that ends early is an error, not a short file.
fn send_all(reader: &mut dyn Read, size: u64, reply: &SyncSender<io::Result<Vec<u8>>>) {
    let mut buf = vec![0u8; 64 * 1024];
    let mut sent = 0u64;
    loop {
        match reader.read(&mut buf) {
            Ok(0) if sent < size => {
                let _ = reply.send(Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the archive is truncated")));
                return;
            }
            Ok(0) => {
                let _ = reply.send(Ok(Vec::new()));
                return;
            }
            Ok(n) => {
                sent += n as u64;
                if reply.send(Ok(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = reply.send(Err(e));
                return;
            }
        }
    }
}

/// The reading end of a chunk channel. Ends at an empty chunk; a sender
/// that goes away before sending one stopped on an error.
struct PipeReader {
    rx: Receiver<io::Result<Vec<u8>>>,
    buf: Vec<u8>,
    pos: usize,
    done: bool,
}

impl PipeReader {
    fn new(rx: Receiver<io::Result<Vec<u8>>>) -> Self {
        Self { rx, buf: Vec::new(), pos: 0, done: false }
    }
}

impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.buf.len() {
            if self.done {
                return Ok(0);
            }
            match self.rx.recv() {
                Ok(Ok(chunk)) => {
                    self.done = chunk.is_empty();
                    self.buf = chunk;
                    self.pos = 0;
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the archive could not be read"));
                }
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

struct PipeWriter(SyncSender<io::Result<Vec<u8>>>);

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // An empty chunk would read as the end of the stream.
        if buf.is_empty() {
            return Ok(0);
        }
        self.0.send(Ok(buf.to_vec())).map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// `/a/b` for an archive name like `./a/b/` or `a//b`. `None` for names
/// that are empty or climb out of the archive.
fn clean_path(name: &str) -> Option<PathBuf> {
    let mut parts = Vec::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return None,
            // `C:` would make the name absolute when copied out on Windows.
            p if cfg!(windows) && p.contains(':') => return None,
            p => parts.push(p),
        }
    }
    (!parts.is_empty()).then(|| PathBuf::from(format!("/{}", parts.join("/"))))
}

/// Resolves a symlink target relative to the link's directory.
fn resolve_symlink(link: &Path, target: &str) -> PathBuf {
    let base = if target.starts_with('/') {
        String::new()
    } else {
        crate::util::posix(link.parent().unwrap_or(Path::new("/")))
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in base.split('/').chain(target.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    PathBuf::from(format!("/{}", parts.join("/")))
}

/// Converts a calendar date and time (UTC) to a `SystemTime`.
fn civil_to_system(year: i64, month: u32, day: u32, hour: u8, minute: u8, second: u8) -> Option<SystemTime> {
    if !(1..=12).contains(&month) || day == 0 {
        return None;
    }
    // Days since 1970-01-01, from Howard Hinnant's `days_from_civil`.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64;
    u64::try_from(secs).ok().map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::Progress;
    use crate::ops::{Conflict, Transfer, TransferMode};
    use std::sync::Arc;

    fn tar_bytes() -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in [("top.txt", "top"), ("dir/a.txt", "alpha"), ("dir/sub/b.txt", "beta")] {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(1_700_000_000);
            header.set_cksum();
            builder.append_data(&mut header, name, data.as_bytes()).unwrap();
        }
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_cksum();
        builder.append_link(&mut link, "dir/link", "a.txt").unwrap();
        builder.append_link(&mut link, "up", "dir/sub").unwrap();
        builder.into_inner().unwrap()
    }

    /// A zstd frame of raw (stored) blocks: valid zstd without an encoder.
    fn zstd_stored(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x28, 0xB5, 0x2F, 0xFD];
        out.push(0xA0); // single segment, 4-byte content size
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        let chunks: Vec<&[u8]> = data.chunks(100_000).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            let last = (i + 1 == chunks.len()) as u32;
            let header = last | (chunk.len() as u32) << 3; // block type 0 = raw
            out.extend_from_slice(&header.to_le_bytes()[..3]);
            out.extend_from_slice(chunk);
        }
        out
    }

    fn make_tar(path: &Path, compression: Compression) {
        let tar = tar_bytes();
        let data = match compression {
            Compression::None => tar,
            Compression::Gzip => {
                let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                e.write_all(&tar).unwrap();
                e.finish().unwrap()
            }
            Compression::Bzip2 => {
                let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
                e.write_all(&tar).unwrap();
                e.finish().unwrap()
            }
            Compression::Xz => {
                let mut out = Vec::new();
                lzma_rs::xz_compress(&mut io::Cursor::new(tar), &mut out).unwrap();
                out
            }
            Compression::Zstd => zstd_stored(&tar),
        };
        std::fs::write(path, data).unwrap();
    }

    fn read(vfs: &ArchiveVfs, path: &str) -> String {
        let mut s = String::new();
        vfs.reader(Path::new(path)).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    #[test]
    fn browses_and_reads_tar_gz() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("t.tar.gz");
        make_tar(&file, Compression::Gzip);
        let vfs = ArchiveVfs::open(&file).unwrap();
        let mut names: Vec<String> = vfs.read_dir(Path::new("/")).unwrap().into_iter().map(|e| e.name).collect();
        names.sort();
        assert_eq!(names, ["dir", "top.txt", "up"]);
        assert!(vfs.stat(Path::new("/dir/sub")).unwrap().is_dir(), "implied directories exist");
        assert_eq!(vfs.stat(Path::new("/dir/a.txt")).unwrap().mode, Some(0o644));
        // Out of order reads restart the stream.
        assert_eq!(read(&vfs, "/dir/sub/b.txt"), "beta");
        assert_eq!(read(&vfs, "/top.txt"), "top");
        assert_eq!(read(&vfs, "/dir/link"), "alpha", "symlinks read their target");
        let linked = vfs.read_dir(Path::new("/up")).unwrap();
        assert_eq!(linked.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["b.txt"]);
        assert!(vfs.create_file(Path::new("/new")).is_err());
        assert!(vfs.read_only());
    }

    #[test]
    fn reads_every_tar_compression() {
        let dir = tempfile::tempdir().unwrap();
        for (name, compression) in [
            ("t.tar", Compression::None),
            ("t.tgz", Compression::Gzip),
            ("t.tar.bz2", Compression::Bzip2),
            ("t.tar.xz", Compression::Xz),
            ("t.tar.zst", Compression::Zstd),
        ] {
            let file = dir.path().join(name);
            make_tar(&file, compression);
            let vfs = ArchiveVfs::open(&file).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert_eq!(read(&vfs, "/dir/a.txt"), "alpha", "{name}");
            assert_eq!(read(&vfs, "/top.txt"), "top", "{name}");
        }
    }

    #[test]
    fn reads_zip_and_copies_out() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("z.zip");
        let mut zip = zip::ZipWriter::new(File::create(&file).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        zip.add_directory("empty/", opts).unwrap();
        zip.start_file("docs/readme.md", opts).unwrap();
        zip.write_all(b"# hi").unwrap();
        zip.finish().unwrap();

        let vfs: crate::VfsRef = Arc::new(ArchiveVfs::open(&file).unwrap());
        assert_eq!(vfs.read_dir(Path::new("/")).unwrap().len(), 2);
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        Transfer {
            mode: TransferMode::Copy,
            src: vfs,
            sources: vec![PathBuf::from("/docs")],
            dst: Arc::new(crate::vfs::LocalVfs),
            dest_dir: out.clone(),
            conflict: Conflict::KeepBoth,
        }
        .run(&Progress::default())
        .unwrap();
        assert_eq!(std::fs::read_to_string(out.join("docs/readme.md")).unwrap(), "# hi");
    }

    fn append(builder: &mut tar::Builder<Vec<u8>>, name: &str, data: &str) {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, data.as_bytes()).unwrap();
    }

    fn symlink(builder: &mut tar::Builder<Vec<u8>>, name: &str, target: &str) {
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_cksum();
        builder.append_link(&mut link, name, target).unwrap();
    }

    #[test]
    fn follows_chained_and_replaced_links() {
        let mut b = tar::Builder::new(Vec::new());
        symlink(&mut b, "a", "b");
        symlink(&mut b, "b", "f");
        append(&mut b, "f", "hello");
        append(&mut b, "sub/x", "x");
        symlink(&mut b, "d", "sub");
        symlink(&mut b, "c", "d");
        append(&mut b, "t", "old");
        symlink(&mut b, "r", "t");
        append(&mut b, "r", "new");
        symlink(&mut b, "loop1", "loop2");
        symlink(&mut b, "loop2", "loop1");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("links.tar");
        std::fs::write(&file, b.into_inner().unwrap()).unwrap();
        let vfs = ArchiveVfs::open(&file).unwrap();
        assert_eq!(read(&vfs, "/a"), "hello");
        assert_eq!(vfs.stat(Path::new("/a")).unwrap().size, 5);
        assert!(vfs.stat(Path::new("/c")).unwrap().is_dir(), "a link to a directory link is a directory");
        assert_eq!(vfs.read_dir(Path::new("/c")).unwrap()[0].name, "x");
        assert!(vfs.reader(Path::new("/d")).is_err(), "directory links have no data");
        assert_eq!(read(&vfs, "/r"), "new", "a later entry replaces a link");
        assert!(vfs.reader(Path::new("/loop1")).is_err());
    }

    #[test]
    fn truncated_or_missing_archives_fail_reads() {
        let mut b = tar::Builder::new(Vec::new());
        append(&mut b, "big", &"x".repeat(2000));
        let tar = b.into_inner().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("t.tar");
        std::fs::write(&file, &tar).unwrap();
        let vfs = ArchiveVfs::open(&file).unwrap();
        std::fs::write(&file, &tar[..512 + 1000]).unwrap();
        assert!(vfs.reader(Path::new("/big")).unwrap().read_to_end(&mut Vec::new()).is_err());
        std::fs::remove_file(&file).unwrap();
        let vfs = ArchiveVfs { server: Mutex::new(None), ..vfs };
        assert!(vfs.reader(Path::new("/big")).unwrap().read_to_end(&mut Vec::new()).is_err());
    }

    #[test]
    fn huge_tar_times_do_not_panic() {
        let mut b = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mtime(u64::MAX);
        header.set_cksum();
        b.append_data(&mut header, "f", io::empty()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("t.tar");
        std::fs::write(&file, b.into_inner().unwrap()).unwrap();
        assert_eq!(ArchiveVfs::open(&file).unwrap().stat(Path::new("/f")).unwrap().modified, None);
    }

    #[cfg(unix)]
    #[test]
    fn zip_symlinks_read_their_target() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("z.zip");
        let mut zip = zip::ZipWriter::new(File::create(&file).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("real.txt", opts).unwrap();
        zip.write_all(b"content").unwrap();
        zip.add_directory("sub/", opts).unwrap();
        zip.add_symlink("ln", "real.txt", opts).unwrap();
        zip.add_symlink("dl", "sub", opts).unwrap();
        zip.finish().unwrap();
        let vfs = ArchiveVfs::open(&file).unwrap();
        assert_eq!(read(&vfs, "/ln"), "content");
        assert!(vfs.stat(Path::new("/dl")).unwrap().is_dir());
    }

    #[test]
    fn cleans_and_resolves_paths() {
        assert_eq!(clean_path("./a//b/"), Some(PathBuf::from("/a/b")));
        assert_eq!(clean_path("../evil"), None);
        assert_eq!(clean_path("./"), None);
        assert_eq!(resolve_symlink(Path::new("/a/b/link"), "../c"), PathBuf::from("/a/c"));
        assert_eq!(resolve_symlink(Path::new("/a/link"), "/x"), PathBuf::from("/x"));
    }

    #[test]
    fn converts_dates() {
        let t = civil_to_system(2024, 2, 29, 12, 0, 0).unwrap();
        assert_eq!(t.duration_since(UNIX_EPOCH).unwrap().as_secs(), 1_709_208_000);
    }

    #[test]
    fn detects_formats_by_name() {
        assert_eq!(ArchiveFormat::detect("a.TGZ"), Some(ArchiveFormat::Tar(Compression::Gzip)));
        assert_eq!(ArchiveFormat::detect("a.tar.zst"), Some(ArchiveFormat::Tar(Compression::Zstd)));
        assert_eq!(ArchiveFormat::detect("a.jar"), Some(ArchiveFormat::Zip));
        assert_eq!(ArchiveFormat::detect("log.gz"), None);
    }
}
