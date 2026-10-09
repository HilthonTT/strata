use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use super::Vfs;
use crate::util::{posix, shell_quote};
use crate::{Entry, EntryKind};

/// Browses the filesystem of a running Docker container through `docker exec`.
///
/// Only POSIX `sh` and `stat` are required inside the container, so it works
/// with busybox-based images too.
#[derive(Debug)]
pub struct DockerVfs {
    container: String,
    name: String,
}

impl DockerVfs {
    pub fn new(container: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            container: container.into(),
            name: name.into(),
        }
    }

    fn sh(&self, script: &str) -> Result<String> {
        let out = Command::new("docker")
            .args(["exec", &self.container, "sh", "-c", script])
            .stdin(Stdio::null())
            .output()
            .context("failed to run docker")?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn parse_stat_line(line: &str, path: PathBuf) -> Option<Entry> {
        // Format: type|size|mtime|mode(hex)|is_dir_target
        let mut parts = line.splitn(5, '|');
        let kind = parts.next()?;
        let size = parts.next()?.parse().unwrap_or(0);
        let mtime: u64 = parts.next()?.parse().unwrap_or(0);
        let mode = u32::from_str_radix(parts.next()?, 16)
            .ok()
            .map(|m| m & 0o7777);
        let to_dir = parts.next() == Some("1");
        let kind = match kind {
            k if k.contains("symbolic link") => EntryKind::Symlink { to_dir },
            k if k.contains("directory") => EntryKind::Dir,
            k if k.contains("file") => EntryKind::File,
            _ => EntryKind::Other,
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".into());
        Some(Entry {
            name,
            path,
            kind,
            size: if kind == EntryKind::Dir { 0 } else { size },
            modified: Some(UNIX_EPOCH + Duration::from_secs(mtime)),
            mode,
        })
    }

    const STAT_FN: &'static str = r#"st() { printf '%s|' "$(stat -c '%F|%s|%Y|%f' "$1" 2>/dev/null)"; if [ -d "$1" ]; then echo 1; else echo 0; fi; printf '%s\n' "$1"; }"#;
}

impl Vfs for DockerVfs {
    fn scheme(&self) -> &'static str {
        "docker"
    }

    fn label(&self) -> String {
        format!("docker:{}", self.name)
    }

    fn home(&self) -> PathBuf {
        PathBuf::from("/")
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<Entry>> {
        let dir = posix(path);
        let script = format!(
            "{} ; cd {} || exit 1; for f in * .*; do case \"$f\" in .|..) continue;; esac; [ -e \"$f\" ] || [ -L \"$f\" ] || continue; st \"$f\"; done",
            Self::STAT_FN,
            shell_quote(&dir)
        );
        let out = self.sh(&script)?;
        let mut lines = out.lines();
        let mut entries = Vec::new();
        while let (Some(meta), Some(name)) = (lines.next(), lines.next()) {
            if let Some(e) = Self::parse_stat_line(meta, self.join(path, name)) {
                entries.push(e);
            }
        }
        Ok(entries)
    }

    fn stat(&self, path: &Path) -> Result<Entry> {
        let p = posix(path);
        let out = self.sh(&format!(
            "{} ; [ -e {q} ] || [ -L {q} ] || exit 1; st {q}",
            Self::STAT_FN,
            q = shell_quote(&p)
        ))?;
        let line = out.lines().next().context("empty stat output")?;
        Self::parse_stat_line(line, path.to_path_buf()).context("unparsable stat output")
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        self.sh(&format!("mkdir -p {}", shell_quote(&posix(path))))
            .map(drop)
    }

    fn create_file(&self, path: &Path) -> Result<()> {
        self.sh(&format!("touch {}", shell_quote(&posix(path))))
            .map(drop)
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        self.sh(&format!("rm -f {}", shell_quote(&posix(path))))
            .map(drop)
    }

    fn remove_dir(&self, path: &Path) -> Result<()> {
        self.sh(&format!("rmdir {}", shell_quote(&posix(path))))
            .map(drop)
    }

    fn remove_all(&self, path: &Path) -> Result<()> {
        self.sh(&format!("rm -rf {}", shell_quote(&posix(path))))
            .map(drop)
    }

    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        self.sh(&format!(
            "mv {} {}",
            shell_quote(&posix(from)),
            shell_quote(&posix(to))
        ))
        .map(drop)
    }

    fn reader(&self, path: &Path) -> Result<Box<dyn Read + Send>> {
        let out = Command::new("docker")
            .args(["exec", &self.container, "cat", &posix(path)])
            .stdin(Stdio::null())
            .output()
            .context("failed to run docker")?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(Box::new(Cursor::new(out.stdout)))
    }

    fn shell_command(&self, path: &Path) -> Option<Vec<String>> {
        let script = "command -v bash >/dev/null && exec bash || exec sh";
        Some(
            [
                "docker",
                "exec",
                "-it",
                "-w",
                &posix(path),
                &self.container,
                "sh",
                "-c",
                script,
            ]
            .map(str::to_string)
            .to_vec(),
        )
    }

    fn writer(&self, path: &Path) -> Result<Box<dyn Write + Send>> {
        let child = Command::new("docker")
            .args([
                "exec",
                "-i",
                &self.container,
                "sh",
                "-c",
                &format!("cat > {}", shell_quote(&posix(path))),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to run docker")?;
        Ok(Box::new(ChildWriter { child: Some(child) }))
    }
}

/// Streams into a child's stdin and reports its exit status when dropped.
struct ChildWriter {
    child: Option<Child>,
}

impl Write for ChildWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.child.as_mut().and_then(|c| c.stdin.as_mut()) {
            Some(stdin) => stdin.write(buf),
            None => Err(std::io::Error::other("writer closed")),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(mut child) = self.child.take() {
            drop(child.stdin.take());
            let status = child.wait()?;
            if !status.success() {
                return Err(std::io::Error::other("docker exec failed to write file"));
            }
        }
        Ok(())
    }
}

impl Drop for ChildWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_busybox_stat_output() {
        let e = DockerVfs::parse_stat_line(
            "regular file|42|1700000000|81a4|0",
            PathBuf::from("/etc/hosts"),
        )
        .unwrap();
        assert_eq!(e.name, "hosts");
        assert_eq!(e.kind, EntryKind::File);
        assert_eq!(e.size, 42);
        assert_eq!(e.mode, Some(0o644));

        let d =
            DockerVfs::parse_stat_line("symbolic link|7|0|a1ff|1", PathBuf::from("/lib")).unwrap();
        assert_eq!(d.kind, EntryKind::Symlink { to_dir: true });
    }
}
