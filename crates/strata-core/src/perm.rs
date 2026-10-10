//! Permission and ownership changes: `chmod` specs (`755`, `u+x,go-w`)
//! and `chown` on the local disk.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use anyhow::{bail, Result};

use crate::jobs::Progress;
use crate::{EntryKind, Vfs};

/// `(path, old, new)` of every changed item, recorded for undo.
pub type Changes<T> = Vec<(PathBuf, T, T)>;
pub type ChangeLog<T> = Mutex<Changes<T>>;

/// A parsed `chmod` mode: absolute (`755`) or symbolic (`u+x,go-w`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeSpec {
    Absolute(u32),
    Symbolic(Vec<Clause>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    /// Bits of `u`, `g` and `o` the clause applies to (`0o7` per class).
    who: u32,
    /// `+`, `-` or `=` followed by the permission letters.
    ops: Vec<(char, String)>,
}

impl ModeSpec {
    pub fn parse(spec: &str) -> Result<Self> {
        let spec = spec.trim();
        if spec.is_empty() {
            bail!("empty mode");
        }
        if spec.chars().all(|c| c.is_ascii_digit()) {
            return match u32::from_str_radix(spec, 8) {
                Ok(mode) if mode <= 0o7777 => Ok(Self::Absolute(mode)),
                _ => bail!("'{spec}' is not an octal mode like 755"),
            };
        }
        let mut clauses = Vec::new();
        for part in spec.split(',') {
            let mut chars = part.chars().peekable();
            let mut who = 0;
            while let Some(&c) = chars.peek() {
                who |= match c {
                    'u' => 0o700,
                    'g' => 0o070,
                    'o' => 0o007,
                    'a' => 0o777,
                    _ => break,
                };
                chars.next();
            }
            let mut ops = Vec::new();
            while let Some(op) = chars.next() {
                if !matches!(op, '+' | '-' | '=') {
                    bail!("expected +, - or = in '{part}'");
                }
                let mut perms = String::new();
                while let Some(&c) = chars.peek() {
                    if !matches!(c, 'r' | 'w' | 'x' | 'X' | 's' | 't') {
                        break;
                    }
                    perms.push(c);
                    chars.next();
                }
                ops.push((op, perms));
            }
            if ops.is_empty() {
                bail!("'{part}' has no +, - or =");
            }
            clauses.push(Clause { who: if who == 0 { 0o777 } else { who }, ops });
        }
        Ok(Self::Symbolic(clauses))
    }

    /// The mode a file with `mode` gets under this spec.
    pub fn apply(&self, mode: u32, is_dir: bool) -> u32 {
        let clauses = match self {
            Self::Absolute(m) => return *m,
            Self::Symbolic(clauses) => clauses,
        };
        let mut mode = mode & 0o7777;
        for clause in clauses {
            for (op, perms) in &clause.ops {
                let mut bits = 0;
                for p in perms.chars() {
                    bits |= match p {
                        'r' => 0o444 & clause.who,
                        'w' => 0o222 & clause.who,
                        'x' => 0o111 & clause.who,
                        // Execute only for directories and files someone can already run.
                        'X' if is_dir || mode & 0o111 != 0 => 0o111 & clause.who,
                        's' => {
                            (if clause.who & 0o700 != 0 { 0o4000 } else { 0 })
                                | (if clause.who & 0o070 != 0 { 0o2000 } else { 0 })
                        }
                        't' => 0o1000,
                        _ => 0,
                    };
                }
                match op {
                    '+' => mode |= bits,
                    '-' => mode &= !bits,
                    _ => {
                        let mut cleared = clause.who;
                        if clause.who & 0o700 != 0 {
                            cleared |= 0o4000;
                        }
                        if clause.who & 0o070 != 0 {
                            cleared |= 0o2000;
                        }
                        mode = (mode & !cleared) | bits;
                    }
                }
            }
        }
        mode
    }
}

/// Applies `spec` to `paths` (and everything below them when `recursive`).
/// Symbolic links met while recursing are left alone.
pub fn change_mode(
    vfs: &dyn Vfs,
    paths: &[PathBuf],
    spec: &ModeSpec,
    recursive: bool,
    progress: &Progress,
    log: &ChangeLog<u32>,
) -> Result<()> {
    progress.total_items.store(paths.len(), Ordering::Relaxed);
    for path in paths {
        chmod_one(vfs, path, spec, recursive, progress, log)?;
        progress.item_done();
    }
    Ok(())
}

fn chmod_one(
    vfs: &dyn Vfs,
    path: &Path,
    spec: &ModeSpec,
    recursive: bool,
    progress: &Progress,
    log: &ChangeLog<u32>,
) -> Result<()> {
    progress.check()?;
    let entry = vfs.stat(path)?;
    let Some(old) = entry.mode else {
        bail!("{} does not report permissions", vfs.scheme());
    };
    let new = spec.apply(old, entry.is_dir());
    if new != old {
        progress.set_current(entry.name.clone());
        vfs.set_mode(path, new)?;
        log.lock().unwrap_or_else(|e| e.into_inner()).push((path.to_path_buf(), old, new));
    }
    if recursive && entry.kind == EntryKind::Dir {
        for child in vfs.read_dir(path)? {
            if !child.is_symlink() {
                chmod_one(vfs, &child.path, spec, recursive, progress, log)?;
            }
        }
    }
    Ok(())
}

/// A `chown` target: user and/or group ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owner {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl Owner {
    /// Parses `user`, `user:group` or `:group`, by name or number.
    pub fn parse(spec: &str) -> Result<Self> {
        let (user, group) = match spec.trim().split_once([':', '.']) {
            Some((u, g)) => (u, g),
            None => (spec.trim(), ""),
        };
        if user.is_empty() && group.is_empty() {
            bail!("usage: chown user[:group] or :group");
        }
        let uid = (!user.is_empty()).then(|| lookup_id(user, "/etc/passwd", &["id", "-u"])).transpose()?;
        let gid = (!group.is_empty()).then(|| lookup_id(group, "/etc/group", &["getent", "group"])).transpose()?;
        Ok(Self { uid, gid })
    }
}

/// Resolves a user or group name: a number, a line of `file`, or what the
/// `query` command prints (covers LDAP and other name services).
fn lookup_id(name: &str, file: &str, query: &[&str]) -> Result<u32> {
    if let Ok(id) = name.parse() {
        return Ok(id);
    }
    if let Ok(text) = std::fs::read_to_string(file) {
        for line in text.lines() {
            let mut fields = line.split(':');
            if fields.next() == Some(name) {
                if let Some(id) = fields.nth(1).and_then(|id| id.parse().ok()) {
                    return Ok(id);
                }
            }
        }
    }
    let out = std::process::Command::new(query[0]).args(&query[1..]).arg(name).output();
    if let Some(out) = out.ok().filter(|o| o.status.success()) {
        let text = String::from_utf8_lossy(&out.stdout);
        // `id -u` prints the id; `getent group` prints `name:x:id:members`.
        let id = text.trim().split(':').nth(if query[0] == "id" { 0 } else { 2 });
        if let Some(id) = id.and_then(|id| id.parse().ok()) {
            return Ok(id);
        }
    }
    bail!("no such {}: {name}", if file.ends_with("passwd") { "user" } else { "group" })
}

/// `(uid, gid)` of a local path, not following symlinks.
#[cfg(unix)]
pub fn owner_of(path: &Path) -> Result<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path)?;
    Ok((meta.uid(), meta.gid()))
}

/// Sets the owner of a local path, not following symlinks.
#[cfg(unix)]
pub fn set_owner(path: &Path, uid: u32, gid: u32) -> Result<()> {
    use anyhow::Context;
    std::os::unix::fs::lchown(path, Some(uid), Some(gid)).with_context(|| format!("chown {}", path.display()))
}

/// Changes the owner of local `paths` (recursively when asked).
#[cfg(unix)]
pub fn change_owner(
    paths: &[PathBuf],
    owner: Owner,
    recursive: bool,
    progress: &Progress,
    log: &ChangeLog<(u32, u32)>,
) -> Result<()> {
    fn one(path: &Path, owner: Owner, recursive: bool, progress: &Progress, log: &ChangeLog<(u32, u32)>) -> Result<()> {
        progress.check()?;
        let old = owner_of(path)?;
        let new = (owner.uid.unwrap_or(old.0), owner.gid.unwrap_or(old.1));
        if new != old {
            set_owner(path, new.0, new.1)?;
            log.lock().unwrap_or_else(|e| e.into_inner()).push((path.to_path_buf(), old, new));
        }
        let meta = std::fs::symlink_metadata(path)?;
        if recursive && meta.is_dir() {
            for child in std::fs::read_dir(path)? {
                one(&child?.path(), owner, recursive, progress, log)?;
            }
        }
        Ok(())
    }
    progress.total_items.store(paths.len(), Ordering::Relaxed);
    for path in paths {
        one(path, owner, recursive, progress, log)?;
        progress.item_done();
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn change_owner(
    _paths: &[PathBuf],
    _owner: Owner,
    _recursive: bool,
    _progress: &Progress,
    _log: &ChangeLog<(u32, u32)>,
) -> Result<()> {
    bail!("changing owners is not supported on this platform")
}

#[cfg(not(unix))]
pub fn set_owner(_path: &Path, _uid: u32, _gid: u32) -> Result<()> {
    bail!("changing owners is not supported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(spec: &str, mode: u32, dir: bool) -> u32 {
        ModeSpec::parse(spec).unwrap().apply(mode, dir)
    }

    #[test]
    fn parses_octal_modes() {
        assert_eq!(ModeSpec::parse("755").unwrap(), ModeSpec::Absolute(0o755));
        assert_eq!(ModeSpec::parse("0644").unwrap(), ModeSpec::Absolute(0o644));
        assert!(ModeSpec::parse("789").is_err());
        assert!(ModeSpec::parse("77777").is_err());
    }

    #[test]
    fn applies_symbolic_modes() {
        assert_eq!(apply("u+x", 0o644, false), 0o744);
        assert_eq!(apply("+x", 0o644, false), 0o755);
        assert_eq!(apply("go-w", 0o666, false), 0o644);
        assert_eq!(apply("a=r", 0o755, false), 0o444);
        assert_eq!(apply("u=rwx,go=rx", 0o600, false), 0o755);
        assert_eq!(apply("u+x-w", 0o644, false), 0o544);
        assert_eq!(apply("g+s", 0o755, true), 0o2755);
        assert_eq!(apply("+t", 0o777, true), 0o1777);
    }

    #[test]
    fn capital_x_only_marks_directories_and_executables() {
        assert_eq!(apply("a+X", 0o644, false), 0o644);
        assert_eq!(apply("a+X", 0o644, true), 0o755);
        assert_eq!(apply("a+X", 0o744, false), 0o755);
    }

    #[test]
    fn rejects_bad_symbolic_modes() {
        assert!(ModeSpec::parse("u").is_err());
        assert!(ModeSpec::parse("z+x").is_err());
        assert!(ModeSpec::parse("").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn changes_modes_recursively_and_logs_them() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("f"), "x").unwrap();
        std::fs::set_permissions(sub.join("f"), std::fs::Permissions::from_mode(0o600)).unwrap();
        let log = ChangeLog::default();
        let spec = ModeSpec::parse("go+r").unwrap();
        change_mode(&crate::vfs::LocalVfs, std::slice::from_ref(&sub), &spec, true, &Progress::default(), &log)
            .unwrap();
        let mode = std::fs::metadata(sub.join("f")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        assert!(log
            .into_inner()
            .unwrap()
            .iter()
            .any(|(p, old, new)| *p == sub.join("f") && *old == 0o600 && *new == 0o644));
    }

    #[test]
    fn parses_numeric_owners() {
        assert_eq!(Owner::parse("1000:100").unwrap(), Owner { uid: Some(1000), gid: Some(100) });
        assert_eq!(Owner::parse(":100").unwrap(), Owner { uid: None, gid: Some(100) });
        assert_eq!(Owner::parse("0").unwrap(), Owner { uid: Some(0), gid: None });
        assert!(Owner::parse(":").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resolves_root_by_name() {
        assert_eq!(Owner::parse("root").unwrap().uid, Some(0));
    }
}
