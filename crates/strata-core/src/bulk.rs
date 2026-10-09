//! Bulk rename: edit a list of names in `$EDITOR`, then apply the diff.

use std::collections::HashSet;

use std::path::Path;

use anyhow::{bail, Result};

use crate::VfsRef;

/// One rename to perform, by name inside the same directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    pub from: String,
    pub to: String,
}

/// The text shown in the editor: one name per line.
pub fn render(names: &[String]) -> String {
    let mut out = names.join("\n");
    out.push('\n');
    out
}

/// Compares the edited text with the original names and returns the
/// renames to perform, rejecting anything ambiguous.
pub fn plan(original: &[String], edited: &str) -> Result<Vec<Rename>> {
    let new: Vec<&str> = edited.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
    if new.len() != original.len() {
        bail!("expected {} names but found {} — lines must not be added or removed", original.len(), new.len());
    }
    let mut seen = HashSet::new();
    for name in &new {
        if name.contains('/') || name.contains('\\') {
            bail!("'{name}' contains a path separator");
        }
        if !seen.insert(*name) {
            bail!("'{name}' appears more than once");
        }
    }
    Ok(original
        .iter()
        .zip(new)
        .filter(|(from, to)| from.as_str() != *to)
        .map(|(from, to)| Rename { from: from.clone(), to: to.to_string() })
        .collect())
}

/// Applies renames inside `dir`, going through temporary names so that
/// swaps (`a` ↔ `b`) work. Refuses to overwrite files outside the plan.
pub fn apply(vfs: &VfsRef, dir: &Path, plan: &[Rename]) -> Result<()> {
    let sources: HashSet<&str> = plan.iter().map(|r| r.from.as_str()).collect();
    for r in plan {
        if !sources.contains(r.to.as_str()) && vfs.exists(&vfs.join(dir, &r.to)) {
            bail!("'{}' already exists", r.to);
        }
    }
    let staged: Vec<(String, &Rename)> =
        plan.iter().enumerate().map(|(i, r)| (format!(".strata-rename-{}-{i}", std::process::id()), r)).collect();
    for (tmp, r) in &staged {
        vfs.rename(&vfs.join(dir, &r.from), &vfs.join(dir, tmp))?;
    }
    for (tmp, r) in &staged {
        vfs.rename(&vfs.join(dir, tmp), &vfs.join(dir, &r.to))?;
    }
    Ok(())
}

/// The renames that undo `plan`.
pub fn reverse(plan: &[Rename]) -> Vec<Rename> {
    plan.iter().map(|r| Rename { from: r.to.clone(), to: r.from.clone() }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plans_only_changed_names() {
        let orig = names(&["a.txt", "b.txt", "c.txt"]);
        let plan = plan(&orig, "a.txt\nbee.txt\nc.md\n").unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0], Rename { from: "b.txt".into(), to: "bee.txt".into() });
    }

    #[test]
    fn applies_swaps_and_reverses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "A").unwrap();
        std::fs::write(dir.path().join("b"), "B").unwrap();
        let vfs: VfsRef = std::sync::Arc::new(crate::vfs::LocalVfs);
        let p = plan(&names(&["a", "b"]), "b\na\n").unwrap();
        apply(&vfs, dir.path(), &p).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a")).unwrap(), "B");
        apply(&vfs, dir.path(), &reverse(&p)).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a")).unwrap(), "A");
    }

    #[test]
    fn refuses_to_overwrite_untouched_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "").unwrap();
        std::fs::write(dir.path().join("c"), "").unwrap();
        let vfs: VfsRef = std::sync::Arc::new(crate::vfs::LocalVfs);
        let p = vec![Rename { from: "a".into(), to: "c".into() }];
        assert!(apply(&vfs, dir.path(), &p).is_err());
    }

    #[test]
    fn rejects_bad_edits() {
        let orig = names(&["a", "b"]);
        assert!(plan(&orig, "a\n").is_err());
        assert!(plan(&orig, "x\nx\n").is_err());
        assert!(plan(&orig, "a\nsub/b\n").is_err());
    }
}
