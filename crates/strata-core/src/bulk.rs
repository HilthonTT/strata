//! Bulk rename: edit a list of names in `$EDITOR`, then apply the diff.

use std::collections::HashSet;

use anyhow::{bail, Result};

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
    let new: Vec<&str> = edited
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect();
    if new.len() != original.len() {
        bail!(
            "expected {} names but found {} — lines must not be added or removed",
            original.len(),
            new.len()
        );
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
        .map(|(from, to)| Rename {
            from: from.clone(),
            to: to.to_string(),
        })
        .collect())
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
        assert_eq!(
            plan[0],
            Rename {
                from: "b.txt".into(),
                to: "bee.txt".into()
            }
        );
    }

    #[test]
    fn rejects_bad_edits() {
        let orig = names(&["a", "b"]);
        assert!(plan(&orig, "a\n").is_err());
        assert!(plan(&orig, "x\nx\n").is_err());
        assert!(plan(&orig, "a\nsub/b\n").is_err());
    }
}
