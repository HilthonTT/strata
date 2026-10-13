//! Per-file git status for the file panels, and the git actions run on
//! marked files: stage, unstage, discard, diff, log, blame and commit.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

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

/// Top-level directory of the repository containing `dir`.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    let root = git(dir, &["rev-parse", "--show-toplevel"])?;
    Some(PathBuf::from(String::from_utf8_lossy(&root).trim()))
}

/// Current branch name, or the short commit hash when detached.
pub fn branch(root: &Path) -> Option<String> {
    let name = git(root, &["symbolic-ref", "--short", "-q", "HEAD"])
        .or_else(|| git(root, &["rev-parse", "--short", "HEAD"]))?;
    Some(String::from_utf8_lossy(&name).trim().to_string()).filter(|n| !n.is_empty())
}

/// Runs git in `root` with `args`, then `--` and `paths`. Returns stdout,
/// or git's own error message.
fn run(root: &Path, args: &[&str], paths: &[PathBuf]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).args(args);
    if !paths.is_empty() {
        cmd.arg("--").args(paths);
    }
    let out = cmd
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .stdin(Stdio::null())
        .output()
        .context("failed to run git")?;
    if !out.status.success() {
        // Some failures (`commit` with nothing staged) only explain themselves on stdout.
        let (err, msg) = (String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
        let line = err.lines().chain(msg.lines()).map(str::trim).find(|l| !l.is_empty() && !l.starts_with("hint:"));
        bail!("{}", line.unwrap_or("git failed").trim_start_matches("fatal: ").trim_start_matches("error: "));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// True when the repository has at least one commit.
fn has_head(root: &Path) -> bool {
    git(root, &["rev-parse", "--verify", "-q", "HEAD"]).is_some()
}

/// Stages new, changed and deleted files (and everything below folders).
pub fn stage(root: &Path, paths: &[PathBuf]) -> Result<()> {
    run(root, &["add", "-A"], paths).map(drop)
}

/// Takes files out of the index, keeping the changes in the work tree.
pub fn unstage(root: &Path, paths: &[PathBuf]) -> Result<()> {
    if has_head(root) {
        run(root, &["reset", "-q", "HEAD"], paths).map(drop)
    } else {
        // Nothing committed yet: there is no HEAD to reset to.
        run(root, &["rm", "-r", "-q", "--cached"], paths).map(drop)
    }
}

/// Throws away unstaged changes to tracked files. Untracked files are left
/// alone (they can be deleted, which the trash can undo).
pub fn discard(root: &Path, paths: &[PathBuf]) -> Result<()> {
    let listed = run(root, &["ls-files", "-z"], paths)?;
    let listed: Vec<PathBuf> = listed.split('\0').filter(|r| !r.is_empty()).map(|r| root.join(r)).collect();
    // Pass on the items that hold tracked files rather than every file
    // inside them, which could overflow the command line.
    let tracked: Vec<PathBuf> = paths
        .iter()
        .filter(|p| {
            let p = canonical_in(root, p);
            listed.iter().any(|t| t.starts_with(&p))
        })
        .cloned()
        .collect();
    if tracked.is_empty() {
        bail!("nothing tracked to discard");
    }
    run(root, &["checkout"], &tracked).map(drop)
}

/// `path` spelled the way git spells paths under `root`, which git gives
/// with symlinks resolved and, on Windows, with forward slashes.
fn canonical_in(root: &Path, path: &Path) -> PathBuf {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let root_real = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    match path.strip_prefix(&root_real) {
        Ok(rel) => root.join(rel),
        Err(_) => path,
    }
}

/// How many files are staged for the next commit.
pub fn staged_count(root: &Path) -> Result<usize> {
    let names = run(root, &["diff", "--cached", "--name-only", "-z"], &[])?;
    Ok(names.split('\0').filter(|n| !n.is_empty()).count())
}

/// Staged and unstaged changes against the last commit, plus untracked
/// files shown as wholly added.
pub fn diff(root: &Path, paths: &[PathBuf]) -> Result<String> {
    let base: &[&str] =
        if has_head(root) { &["diff", "--no-color", "HEAD"] } else { &["diff", "--no-color", "--cached"] };
    let mut out = run(root, base, paths)?;
    let untracked = run(root, &["ls-files", "--others", "--exclude-standard", "-z"], paths)?;
    for rel in untracked.split('\0').filter(|r| !r.is_empty()) {
        // `--no-index` exits with 1 when the files differ, which they do.
        let added = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["diff", "--no-color", "--no-index", "--", "/dev/null", rel])
            .stdin(Stdio::null())
            .output()
            .context("failed to run git")?;
        out.push_str(&String::from_utf8_lossy(&added.stdout));
    }
    Ok(out)
}

/// Recent commits touching `paths` (the whole repository when empty).
pub fn log(root: &Path, paths: &[PathBuf]) -> Result<String> {
    run(root, &["log", "--no-color", "-n", "300", "--date=short", "--format=%h  %ad  %<(16,trunc)%an  %s"], paths)
}

/// Who last changed each line of `file`.
pub fn blame(root: &Path, file: &Path) -> Result<String> {
    run(root, &["blame", "--date=short", "-w"], &[file.to_path_buf()])
}

/// Commits what is staged. Returns git's one-line summary.
pub fn commit(root: &Path, message: &str) -> Result<String> {
    if message.trim().is_empty() {
        bail!("the commit message is empty");
    }
    let out = run(root, &["commit", "-q", "-m", message], &[]).map_err(|e| {
        let text = e.to_string();
        if text.contains("identity unknown") || text.contains("auto-detect email") {
            anyhow::anyhow!("{text}: set user.name and user.email with `git config --global`")
        } else {
            e
        }
    })?;
    let summary = run(root, &["log", "-1", "--format=%h %s"], &[]).unwrap_or(out);
    Ok(summary.trim().to_string())
}

/// Which of `paths` git ignores, through `.gitignore` files,
/// `.git/info/exclude` and the global excludes file. Paths are grouped by
/// the repository holding them and each group is checked by one
/// `git check-ignore --stdin`, so a listing of thousands of items costs a
/// single process per repository. Tracked files are never ignored, and
/// paths outside a repository (or without git installed) are kept.
pub fn ignored<'a>(paths: impl IntoIterator<Item = &'a Path>) -> HashSet<PathBuf> {
    let mut roots: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    let mut by_repo: HashMap<PathBuf, Vec<(&Path, String)>> = HashMap::new();
    for path in paths {
        let Some(root) = path.parent().and_then(|dir| repo_containing(dir, &mut roots)) else {
            continue;
        };
        let Ok(rel) = path.strip_prefix(&root) else { continue };
        // Git refuses to answer for its own directory, failing the batch.
        if rel.as_os_str().is_empty() || rel.components().any(|c| c.as_os_str() == ".git") {
            continue;
        }
        let rel = crate::util::posix(rel);
        by_repo.entry(root).or_default().push((path, rel));
    }
    let mut ignored = HashSet::new();
    for (root, items) in by_repo {
        let found = check_ignore(&root, items.iter().map(|(_, rel)| rel.as_str()));
        ignored.extend(items.into_iter().filter(|(_, rel)| found.contains(rel)).map(|(p, _)| p.to_path_buf()));
    }
    ignored
}

/// Nearest folder at or above `dir` holding a `.git` (a directory, or the
/// file a worktree or submodule has). Answers are cached for every folder
/// looked at, so a listing spread over many folders checks each only once.
fn repo_containing(dir: &Path, cache: &mut HashMap<PathBuf, Option<PathBuf>>) -> Option<PathBuf> {
    let mut walked = Vec::new();
    let mut found = None;
    for a in dir.ancestors() {
        if let Some(known) = cache.get(a) {
            found = known.clone();
            break;
        }
        walked.push(a.to_path_buf());
        if a.join(".git").exists() {
            found = Some(a.to_path_buf());
            break;
        }
    }
    for a in walked {
        cache.insert(a, found.clone());
    }
    found
}

/// Runs `git check-ignore` in `root` on paths relative to it. Returns
/// those that are ignored; none when git fails.
fn check_ignore<'a>(root: &Path, rels: impl Iterator<Item = &'a str>) -> HashSet<String> {
    let input: Vec<u8> = rels.flat_map(|r| r.bytes().chain([0])).collect();
    let child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "--stdin", "-z"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return HashSet::new() };
    let Some(mut stdin) = child.stdin.take() else { return HashSet::new() };
    // Written from another thread so a full stdout pipe cannot deadlock.
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output();
    let _ = writer.join();
    // Exit code 1 means nothing is ignored; anything else but 0 is an error.
    match out {
        Ok(out) if out.status.success() => out
            .stdout
            .split(|b| *b == 0)
            .filter(|r| !r.is_empty())
            .map(|r| String::from_utf8_lossy(r).into_owned())
            .collect(),
        _ => HashSet::new(),
    }
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

    #[test]
    fn finds_ignored_paths_in_one_batch_per_repository() {
        if !crate::nas::which("git") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        Command::new("git").arg("-C").arg(root).args(["init", "-q"]).output().unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n!keep.log\n").unwrap();
        std::fs::create_dir_all(root.join(".git/info")).unwrap();
        std::fs::write(root.join(".git/info/exclude"), "secret.txt\n").unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // `target/` only matches folders: a file named `target` is kept.
        for f in ["src/main.rs", "src/app.log", "keep.log", "secret.txt", "target/debug/x", "src/target"] {
            std::fs::write(root.join(f), "").unwrap();
        }
        // A nested repository answers for its own files.
        let nested = root.join("vendor");
        std::fs::create_dir(&nested).unwrap();
        Command::new("git").arg("-C").arg(&nested).args(["init", "-q"]).output().unwrap();
        std::fs::write(nested.join(".gitignore"), "*.tmp\n").unwrap();
        std::fs::write(nested.join("a.tmp"), "").unwrap();
        std::fs::write(root.join("b.tmp"), "").unwrap();

        let paths: Vec<PathBuf> = [
            "target",
            "target/debug/x",
            "src",
            "src/main.rs",
            "src/app.log",
            "src/target",
            "keep.log",
            "secret.txt",
            ".git",
            ".git/config",
            "vendor/a.tmp",
            "b.tmp",
        ]
        .iter()
        .map(|p| root.join(p))
        .collect();
        let found = ignored(paths.iter().map(PathBuf::as_path));
        let mut found: Vec<String> = found.iter().map(|p| crate::util::posix(p.strip_prefix(root).unwrap())).collect();
        found.sort();
        assert_eq!(found, ["secret.txt", "src/app.log", "target", "target/debug/x", "vendor/a.tmp"]);

        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("x.log"), "").unwrap();
        assert!(ignored([outside.path().join("x.log").as_path()]).is_empty(), "not in a repository");
    }

    #[test]
    fn stages_diffs_commits_and_discards() {
        if !crate::nas::which("git") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            Command::new("git").arg("-C").arg(root).args(args).output().unwrap();
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "Test"]);
        // Windows runners default to autocrlf, which would check files out with CRLF.
        git(&["config", "core.autocrlf", "false"]);
        let root = &repo_root(root).unwrap();
        let file = root.join("a.txt");
        std::fs::write(&file, "one\n").unwrap();

        // Before the first commit, untracked files show as added.
        assert!(diff(root, std::slice::from_ref(&file)).unwrap().contains("+one"));
        stage(root, std::slice::from_ref(&file)).unwrap();
        assert_eq!(status(root).unwrap().get(&file), Some(GitState::Added));
        unstage(root, std::slice::from_ref(&file)).unwrap();
        assert_eq!(status(root).unwrap().get(&file), Some(GitState::Untracked), "unstaged with no HEAD yet");

        stage(root, std::slice::from_ref(&file)).unwrap();
        assert!(commit(root, "  ").is_err());
        assert_eq!(staged_count(root).unwrap(), 1);
        let summary = commit(root, "first").unwrap();
        assert!(summary.ends_with(" first"), "{summary}");
        assert_eq!(branch(root).as_deref(), Some("main"));

        assert_eq!(staged_count(root).unwrap(), 0);
        let nothing = commit(root, "again").unwrap_err().to_string();
        assert!(!nothing.is_empty() && nothing != "git failed", "{nothing}");

        std::fs::write(&file, "two\n").unwrap();
        let d = diff(root, std::slice::from_ref(&file)).unwrap();
        assert!(d.contains("-one") && d.contains("+two"), "{d}");
        stage(root, std::slice::from_ref(&file)).unwrap();
        unstage(root, std::slice::from_ref(&file)).unwrap();
        std::fs::write(root.join("new.txt"), "x").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/b.txt"), "b\n").unwrap();
        stage(root, &[root.join("sub")]).unwrap();
        commit(root, "sub").unwrap();
        std::fs::write(root.join("sub/b.txt"), "changed\n").unwrap();
        discard(root, &[root.join("sub")]).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("sub/b.txt")).unwrap(), "b\n", "folders discard what is inside");
        discard(root, &[file.clone(), root.join("new.txt")]).unwrap();
        assert!(root.join("new.txt").exists(), "untracked files are kept");
        assert!(discard(root, &[root.join("new.txt")]).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "one\n");
        assert!(log(root, std::slice::from_ref(&file)).unwrap().contains("first"));
        assert!(blame(root, &file).unwrap().contains("one"));
        assert!(stage(root, &[root.join("missing")]).unwrap_err().to_string().contains("missing"));
    }
}
