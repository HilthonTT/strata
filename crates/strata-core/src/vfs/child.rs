//! Streams into and out of helper processes (`docker exec`, `rclone`),
//! turning their exit status into an I/O error.

use std::io::{Read, Write};
use std::process::Child;

/// Streams into a child's stdin and reports its exit status on flush or drop.
pub(super) struct ChildWriter {
    child: Option<Child>,
    /// Named in the error when the child fails without saying why.
    program: &'static str,
}

impl ChildWriter {
    pub(super) fn new(child: Child, program: &'static str) -> Self {
        Self { child: Some(child), program }
    }
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
                return Err(std::io::Error::other(failure(&mut child, self.program, "write the file")));
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

/// Reads a child's stdout and reports its exit status at the end.
pub(super) struct ChildReader {
    child: Child,
    program: &'static str,
    done: bool,
}

impl ChildReader {
    pub(super) fn new(child: Child, program: &'static str) -> Self {
        Self { child, program, done: false }
    }
}

impl Read for ChildReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        let n = match self.child.stdout.as_mut() {
            Some(out) => out.read(buf)?,
            None => 0,
        };
        if n == 0 && !buf.is_empty() {
            self.done = true;
            let status = self.child.wait()?;
            if !status.success() {
                return Err(std::io::Error::other(failure(&mut self.child, self.program, "read the file")));
            }
        }
        Ok(n)
    }
}

impl Drop for ChildReader {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The child's last stderr line, or a generic message.
fn failure(child: &mut Child, program: &str, what: &str) -> String {
    let mut stderr = String::new();
    if let Some(err) = child.stderr.as_mut() {
        let _ = err.read_to_string(&mut stderr);
    }
    match last_line(&stderr) {
        Some(line) => line,
        None => format!("{program} failed to {what}"),
    }
}

/// Last non-empty line of a tool's error output, without rclone's
/// `2024/01/02 03:04:05 ERROR : ` prefix or the inline remote it was
/// given (which can be long).
pub(crate) fn last_line(stderr: &str) -> Option<String> {
    let mut line = stderr.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    let dated = line.len() > 20 && line.as_bytes()[4] == b'/' && line.as_bytes()[13] == b':';
    if dated {
        line = &line[20..];
        // The level: `ERROR : `, `CRITICAL: `, `NOTICE: `.
        if let Some((level, rest)) = line.split_once(": ") {
            if level.trim().chars().all(|c| c.is_ascii_uppercase()) {
                line = rest;
            }
        }
    }
    if let Some(rest) = line.strip_prefix("Failed to create file system for ") {
        line = rest.split_once("\": ").map(|(_, msg)| msg).unwrap_or(rest);
    }
    Some(line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_useful_part_of_errors() {
        let err = "2024/05/01 10:00:00 NOTICE : x\n2024/05/01 10:00:01 ERROR : dir: directory not found\n\n";
        assert_eq!(last_line(err).as_deref(), Some("dir: directory not found"));
        let critical =
            "2026/10/12 17:50:49 CRITICAL: Failed to create file system for \":ftp,host='h':pub\": NewFs: 530 no";
        assert_eq!(last_line(critical).as_deref(), Some("NewFs: 530 no"));
        assert_eq!(last_line("Error: oops").as_deref(), Some("Error: oops"));
        assert_eq!(last_line("  \n"), None);
    }

    #[cfg(unix)]
    #[test]
    fn reports_child_failures() {
        use std::process::{Command, Stdio};
        let spawn = |script: &str| {
            Command::new("sh")
                .args(["-c", script])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        };
        let mut out = String::new();
        ChildReader::new(spawn("printf hello"), "sh").read_to_string(&mut out).unwrap();
        assert_eq!(out, "hello");

        let err = ChildReader::new(spawn("echo nope >&2; exit 3"), "sh").read_to_string(&mut out).unwrap_err();
        assert_eq!(err.to_string(), "nope");

        let mut w = ChildWriter::new(spawn("cat >/dev/null; exit 1"), "sh");
        w.write_all(b"data").unwrap();
        assert_eq!(w.flush().unwrap_err().to_string(), "sh failed to write the file");
    }
}
