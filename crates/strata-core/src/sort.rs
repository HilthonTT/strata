use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::Entry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Modified,
    Extension,
}

impl SortKey {
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::Size,
            Self::Size => Self::Modified,
            Self::Modified => Self::Extension,
            Self::Extension => Self::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Size => "size",
            Self::Modified => "modified",
            Self::Extension => "ext",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortOptions {
    pub key: SortKey,
    pub reverse: bool,
    pub dirs_first: bool,
}

impl Default for SortOptions {
    fn default() -> Self {
        Self {
            key: SortKey::Name,
            reverse: false,
            dirs_first: true,
        }
    }
}

/// Natural, case-insensitive comparison so `file2` sorts before `file10`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut n = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        n.push(c);
                        it.next();
                    }
                    n
                };
                let (na, nb) = (take(&mut a), take(&mut b));
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

pub fn sort_entries(entries: &mut [Entry], opts: SortOptions) {
    entries.sort_by(|a, b| {
        if opts.dirs_first && a.is_dir() != b.is_dir() {
            return b.is_dir().cmp(&a.is_dir());
        }
        let ord = match opts.key {
            SortKey::Name => natural_cmp(&a.name, &b.name),
            SortKey::Size => a
                .size
                .cmp(&b.size)
                .then_with(|| natural_cmp(&a.name, &b.name)),
            SortKey::Modified => a
                .modified
                .cmp(&b.modified)
                .then_with(|| natural_cmp(&a.name, &b.name)),
            SortKey::Extension => a
                .extension()
                .cmp(&b.extension())
                .then_with(|| natural_cmp(&a.name, &b.name)),
        };
        if opts.reverse {
            ord.reverse()
        } else {
            ord
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut names = vec!["file10", "File2", "file1", "a"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["a", "file1", "File2", "file10"]);
    }
}
