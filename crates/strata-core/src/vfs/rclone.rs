use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::child::{last_line, ChildReader, ChildWriter};
use super::Vfs;
use crate::util::posix;
use crate::{Entry, EntryKind};

/// Any storage rclone can reach (S3, WebDAV, FTP, Google Drive, …), browsed
/// by running the `rclone` command line tool.
///
/// `remote` is what rclone puts before a path: a configured remote such as
/// `gdrive:` or an inline one such as `:webdav,url='https://host/dav':`.
#[derive(Debug)]
pub struct RcloneVfs {
    scheme: &'static str,
    label: String,
    remote: String,
    /// Extra environment, used to hand rclone secrets off the command line.
    env: Vec<(String, String)>,
    home: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ListItem {
    name: String,
    #[serde(default)]
    size: i64,
    #[serde(default)]
    mod_time: Option<String>,
    #[serde(default)]
    is_dir: bool,
}

impl RcloneVfs {
    pub fn new(
        scheme: &'static str,
        label: impl Into<String>,
        remote: impl Into<String>,
        env: Vec<(String, String)>,
        home: PathBuf,
    ) -> Self {
        Self { scheme, label: label.into(), remote: remote.into(), env, home }
    }

    /// The rclone location of `path`.
    fn target(&self, path: &Path) -> String {
        format!("{}{}", self.remote, posix(path).trim_start_matches('/'))
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("rclone");
        // Never stop to ask for a config password, keep retries short so a
        // dead server fails fast, and only print errors.
        cmd.args(["--ask-password=false", "--log-level", "ERROR", "--contimeout", "10s", "--low-level-retries", "2"]);
        if self.scheme == "s3" {
            // Listing would otherwise read each object's metadata to get its time.
            cmd.arg("--use-server-modtime");
        }
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        cmd
    }

    fn run(&self, args: &[&str]) -> Result<Output> {
        let out = self.command().args(args).stdin(Stdio::null()).output().map_err(spawn_error)?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("{}", last_line(&stderr).unwrap_or_else(|| format!("rclone {} failed", args[0])));
        }
        Ok(out)
    }

    fn entry(&self, dir: &Path, item: ListItem) -> Entry {
        let kind = if item.is_dir { EntryKind::Dir } else { EntryKind::File };
        Entry {
            path: self.join(dir, &item.name),
            name: item.name,
            kind,
            size: if item.is_dir { 0 } else { item.size.max(0) as u64 },
            modified: item.mod_time.as_deref().and_then(parse_time),
            mode: None,
        }
    }
}

fn parse_time(s: &str) -> Option<SystemTime> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(SystemTime::from)
}

fn spawn_error(e: std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        anyhow::anyhow!("rclone is not installed (see https://rclone.org/install/)")
    } else {
        anyhow::Error::new(e).context("failed to run rclone")
    }
}

/// Scrambles a password the way rclone expects it in its config and
/// `RCLONE_*_PASS` variables. The password goes in on stdin.
pub fn obscure(password: &str) -> Result<String> {
    let mut child = Command::new("rclone")
        .args(["obscure", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(spawn_error)?;
    child.stdin.take().context("no stdin")?.write_all(password.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("rclone obscure failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Quotes a value for an inline rclone remote (`:ftp,host='x':`), where
/// commas and colons would otherwise end it.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

impl Vfs for RcloneVfs {
    fn scheme(&self) -> &'static str {
        self.scheme
    }

    fn label(&self) -> String {
        format!("{}:{}", self.scheme, self.label)
    }

    fn home(&self) -> PathBuf {
        self.home.clone()
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<Entry>> {
        let out = self.run(&["lsjson", "--no-mimetype", &self.target(path)])?;
        let items: Vec<ListItem> = serde_json::from_slice(&out.stdout).context("unexpected rclone output")?;
        Ok(items.into_iter().map(|item| self.entry(path, item)).collect())
    }

    fn stat(&self, path: &Path) -> Result<Entry> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let Some(name) = name else {
            // The root of the remote always exists and is a directory.
            return Ok(Entry {
                name: "/".into(),
                path: path.to_path_buf(),
                kind: EntryKind::Dir,
                size: 0,
                modified: None,
                mode: None,
            });
        };
        let out = self.run(&["lsjson", "--stat", "--no-mimetype", &self.target(path)])?;
        let mut item: ListItem = serde_json::from_slice(&out.stdout).context("unexpected rclone output")?;
        item.name = name;
        let dir = self.parent(path).unwrap_or_else(|| PathBuf::from("/"));
        Ok(self.entry(&dir, item))
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        self.run(&["mkdir", &self.target(path)]).map(drop)
    }

    fn create_file(&self, path: &Path) -> Result<()> {
        self.run(&["touch", &self.target(path)]).map(drop)
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        self.run(&["deletefile", &self.target(path)]).map(drop)
    }

    fn remove_dir(&self, path: &Path) -> Result<()> {
        self.run(&["rmdir", &self.target(path)]).map(drop)
    }

    fn remove_all(&self, path: &Path) -> Result<()> {
        if self.stat(path)?.is_dir() {
            self.run(&["purge", &self.target(path)]).map(drop)
        } else {
            self.remove_file(path)
        }
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        self.run(&["moveto", &self.target(from), &self.target(to)]).map(drop)
    }

    fn reader(&self, path: &Path) -> Result<Box<dyn Read + Send>> {
        let child = self
            .command()
            .args(["cat", &self.target(path)])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(spawn_error)?;
        Ok(Box::new(ChildReader::new(child, "rclone")))
    }

    fn writer(&self, path: &Path) -> Result<Box<dyn Write + Send>> {
        let child = self
            .command()
            .args(["rcat", &self.target(path)])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(spawn_error)?;
        Ok(Box::new(ChildWriter::new(child, "rclone")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vfs() -> RcloneVfs {
        RcloneVfs::new("s3", "backups", ":s3,env_auth=true:", Vec::new(), PathBuf::from("/bucket"))
    }

    #[test]
    fn targets_are_relative_to_the_remote() {
        let v = vfs();
        assert_eq!(v.target(Path::new("/")), ":s3,env_auth=true:");
        assert_eq!(v.target(Path::new("/bucket/a b.txt")), ":s3,env_auth=true:bucket/a b.txt");
        assert_eq!(v.label(), "s3:backups");
    }

    #[test]
    fn parses_lsjson_output() {
        let json = br#"[
            {"Path":"docs","Name":"docs","Size":-1,"ModTime":"2024-03-01T10:00:00Z","IsDir":true},
            {"Path":"a.txt","Name":"a.txt","Size":12,"ModTime":"2024-03-01T10:00:00.5+01:00","IsDir":false}
        ]"#;
        let items: Vec<ListItem> = serde_json::from_slice(json).unwrap();
        let v = vfs();
        let entries: Vec<Entry> = items.into_iter().map(|i| v.entry(Path::new("/bucket"), i)).collect();
        assert_eq!(entries[0].kind, EntryKind::Dir);
        assert_eq!(entries[0].size, 0);
        assert_eq!(entries[0].path, PathBuf::from("/bucket/docs"));
        assert_eq!(entries[1].size, 12);
        let t = entries[1].modified.unwrap().duration_since(SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(t.as_millis(), 1_709_283_600_500);
    }

    #[test]
    fn quotes_inline_values() {
        assert_eq!(quote("https://h/dav"), "'https://h/dav'");
        assert_eq!(quote("it's"), "'it''s'");
    }

    /// Exercises a backend end to end. Skipped when rclone is not installed.
    fn round_trip(vfs: &RcloneVfs, base: &Path) {
        let dir = base.join("new dir");
        vfs.create_dir(&dir).unwrap();
        let file = dir.join("a, b's.txt");
        let mut w = vfs.writer(&file).unwrap();
        w.write_all(b"hello rclone").unwrap();
        w.flush().unwrap();
        drop(w);
        let mut text = String::new();
        vfs.reader(&file).unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "hello rclone");

        let listed = vfs.read_dir(&dir).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!((listed[0].name.as_str(), listed[0].size, listed[0].kind), ("a, b's.txt", 12, EntryKind::File));
        assert!(listed[0].modified.is_some());
        assert!(vfs.stat(&dir).unwrap().is_dir());

        let moved = dir.join("moved.txt");
        vfs.rename(&file, &moved).unwrap();
        assert!(!vfs.exists(&file) && vfs.exists(&moved));
        vfs.create_file(&dir.join("empty")).unwrap();
        assert_eq!(vfs.stat(&dir.join("empty")).unwrap().size, 0);

        let err = vfs.reader(&dir.join("missing")).unwrap().read_to_string(&mut text).unwrap_err();
        assert!(!err.to_string().is_empty());
        vfs.remove_all(&dir).unwrap();
        assert!(!vfs.exists(&dir));
    }

    #[test]
    fn works_against_a_local_remote() {
        if !crate::nas::which("rclone") {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let base = PathBuf::from(posix(tmp.path()));
        let vfs = RcloneVfs::new("rclone", "local", ":local:", Vec::new(), base.clone());
        round_trip(&vfs, &base);
    }

    #[test]
    fn works_against_a_password_protected_webdav_server() {
        use crate::nas::Connection;
        if !crate::nas::which("rclone") {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut server = Command::new("rclone")
            .args(["serve", "webdav", "--addr", &format!("127.0.0.1:{port}"), "--user", "me", "--pass", "s3cret"])
            .arg(tmp.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let up = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
        });
        let conn = Connection::from_url("dav", &format!("dav://me@127.0.0.1:{port}/")).unwrap();
        let result = std::panic::catch_unwind(|| {
            assert!(up, "rclone serve webdav did not start");
            let remote = conn.rclone_remote(Some("s3cret")).unwrap();
            assert!(!remote.remote.contains("s3cret"), "the password stays off the command line");
            let vfs = RcloneVfs::new("webdav", "dav", remote.remote, remote.env, remote.home.clone());
            round_trip(&vfs, &remote.home);

            let wrong = conn.rclone_remote(Some("nope")).unwrap();
            let vfs = RcloneVfs::new("webdav", "dav", wrong.remote, wrong.env, wrong.home);
            let err = vfs.read_dir(Path::new("/")).unwrap_err().to_string();
            assert!(err.contains("401"), "{err}");
        });
        let _ = server.kill();
        let _ = server.wait();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn the_root_needs_no_rclone() {
        assert!(vfs().stat(Path::new("/")).unwrap().is_dir());
    }
}
