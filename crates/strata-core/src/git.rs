//! Per-file git status for the file panels.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Status of a path, ordered by how much it matters when summarising a
/// directory (a directory shows its most important child).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GitState {
    Ignored,
    Untracked,
    Deleted,
    Renamed,
    Added,
    Modified,
    Conflicted,
}

impl GitState {
    /// Single-character marker shown next to the file name.
    pub fn marker(self) -> char {
        match self {
            Self::Ignored => '!',
            Self::Untracked => '?',
            Self::Deleted => 'D',
            Self::Renamed => 'R',
            Self::Added => 'A',
            Self::Modified => 'M',
            Self::Conflicted => 'U',
        }
    }
}

/// Status of every changed path in a repository.
#[derive(Debug, Clone, Default)]
pub struct GitStatus {
    pub root: PathBuf,
    /// Absolute paths of changed files, plus every directory containing one.
    states: HashMap<PathBuf, GitState>,
    /// Untracked or ignored directories: everything inside inherits the state.
    inherited: HashMap<PathBuf, GitState>,
}

impl GitStatus {
    pub fn get(&self, path: &Path) -> Option<GitState> {
        if let Some(state) = self.states.get(path) {
            return Some(*state);
        }
        path.ancestors().skip(1).take_while(|a| a.starts_with(&self.root)).find_map(|a| self.inherited.get(a).copied())
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty() && self.inherited.is_empty()
    }
}

/// Runs `git status` for the repository containing `dir`. `None` when
/// `dir` is not in a repository or git is not installed.
pub fn status(dir: &Path) -> Option<GitStatus> {
    let root = git(dir, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(String::from_utf8_lossy(&root).trim());
    let out = git(&root, &["status", "--porcelain=v1", "-z", "--ignored=matching", "--untracked-files=normal"])?;
    Some(parse(&root, &out))
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// Parses `git status --porcelain=v1 -z` output.
pub fn parse(root: &Path, output: &[u8]) -> GitStatus {
    let mut status = GitStatus { root: root.to_path_buf(), ..Default::default() };
    let mut records = output.split(|b| *b == 0).filter(|r| r.len() > 3);
    while let Some(record) = records.next() {
        let (x, y) = (record[0], record[1]);
        let rel = String::from_utf8_lossy(&record[3..]).into_owned();
        // Renames are followed by a second record holding the old path.
        if x == b'R' || x == b'C' {
            records.next();
        }
        let state = match (x, y) {
            (b'!', b'!') => GitState::Ignored,
            (b'?', b'?') => GitState::Untracked,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => GitState::Conflicted,
            (_, b'M') | (b'M', _) | (_, b'T') | (b'T', _) => GitState::Modified,
            (b'A', _) => GitState::Added,
            (b'R', _) | (b'C', _) => GitState::Renamed,
            (b'D', _) | (_, b'D') => GitState::Deleted,
            _ => continue,
        };
        let is_dir = rel.ends_with('/');
        let path = root.join(rel.trim_end_matches('/'));
        if is_dir && matches!(state, GitState::Untracked | GitState::Ignored) {
            status.inherited.insert(path.clone(), state);
        }
        status.states.insert(path.clone(), state);
        if state == GitState::Ignored {
            continue;
        }
        for dir in path.ancestors().skip(1).take_while(|a| a.starts_with(root) && *a != root) {
            let slot = status.states.entry(dir.to_path_buf()).or_insert(state);
            *slot = (*slot).max(state);
        }
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_and_summarises_directories() {
        let root = Path::new("/repo");
        let out = b" M src/main.rs\0A  src/new.rs\0?? notes/\0!! target/\0R  b.txt\0a.txt\0UU conflict.rs\0";
        let s = parse(root, out);
        assert_eq!(s.get(Path::new("/repo/src/main.rs")), Some(GitState::Modified));
        assert_eq!(s.get(Path::new("/repo/src/new.rs")), Some(GitState::Added));
        assert_eq!(s.get(Path::new("/repo/src")), Some(GitState::Modified), "dir shows its most important child");
        assert_eq!(s.get(Path::new("/repo/notes/todo.md")), Some(GitState::Untracked), "inherited from untracked dir");
        assert_eq!(s.get(Path::new("/repo/target/debug/x")), Some(GitState::Ignored));
        assert_eq!(s.get(Path::new("/repo/b.txt")), Some(GitState::Renamed));
        assert_eq!(s.get(Path::new("/repo/a.txt")), None, "old name of a rename is skipped");
        assert_eq!(s.get(Path::new("/repo/conflict.rs")), Some(GitState::Conflicted));
        assert_eq!(s.get(Path::new("/repo/clean.rs")), None);
    }

    #[test]
    fn reads_a_real_repository() {
        if !crate::nas::which("git") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git").arg("-C").arg(dir.path()).args(args).output().unwrap();
        };
        run(&["init", "-q"]);
        std::fs::write(dir.path().join("new.txt"), "x").unwrap();
        let s = status(dir.path()).unwrap();
        let root = s.root.clone();
        assert_eq!(s.get(&root.join("new.txt")), Some(GitState::Untracked));
        assert!(status(&std::env::temp_dir().join("definitely-not-a-repo-xyz")).is_none());
    }
}
