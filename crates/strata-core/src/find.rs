//! Recursive listings filtered by name, size, age, type and extension,
//! for `:find` and `:flatten`.
//!
//! A query is a list of words: `*.log --size +100M --mtime -7d --type f`.
//! Words that are not flags are name globs; a name matching any of them
//! passes. Every flag must match.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context as _, Result};

use crate::search::SKIP_DIRS;
use crate::util::glob_match;
use crate::vfs::LocalVfs;
use crate::{Entry, EntryKind};

/// Entries a walk returns at most, so a query at `/` cannot exhaust memory.
pub const LIMIT: usize = 50_000;

/// Above or below a bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound<T> {
    Above(T),
    Below(T),
}

impl<T: PartialOrd> Bound<T> {
    fn admits(&self, value: T) -> bool {
        match self {
            Bound::Above(b) => value >= *b,
            Bound::Below(b) => value < *b,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindFilter {
    File,
    Dir,
    Link,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    /// Name globs; any may match. Empty matches every name.
    pub names: Vec<String>,
    pub size: Option<Bound<u64>>,
    /// Age since the last modification: `Below` is "changed within".
    pub age: Option<Bound<Duration>>,
    pub kind: Option<KindFilter>,
    /// Lower-case extensions without the dot.
    pub exts: Vec<String>,
    /// Levels below the root to descend; `None` is unlimited.
    pub max_depth: Option<usize>,
    /// Also descend into `.git`, `node_modules`, `target`...
    pub all: bool,
    /// Only empty files and directories.
    pub empty: bool,
}

impl Query {
    /// Everything below the root that is a file: what `:flatten` shows.
    pub fn files() -> Self {
        Self { kind: Some(KindFilter::File), ..Self::default() }
    }

    pub fn parse(input: &str) -> Result<Self> {
        let mut q = Query::default();
        let words = crate::util::split_command(input);
        let mut words = words.iter().map(String::as_str);
        while let Some(word) = words.next() {
            let flag = word.trim_start_matches('-');
            if !word.starts_with('-') || flag.is_empty() {
                q.names.push(word.to_string());
                continue;
            }
            // `--size=+1M` and `--size +1M` both work.
            let (flag, inline) = match flag.split_once('=') {
                Some((f, v)) => (f, Some(v)),
                None => (flag, None),
            };
            let mut value = |name: &str| -> Result<String> {
                inline
                    .map(str::to_string)
                    .or_else(|| words.next().map(str::to_string))
                    .with_context(|| format!("--{name} needs a value"))
            };
            match flag {
                "size" => q.size = Some(parse_bound(&value("size")?, parse_size)?),
                "mtime" | "age" | "changed" | "modified" => {
                    q.age = Some(parse_bound(&value("mtime")?, parse_age)?);
                }
                "newer" => q.age = Some(Bound::Below(parse_age(&value("newer")?)?)),
                "older" => q.age = Some(Bound::Above(parse_age(&value("older")?)?)),
                "type" => {
                    q.kind = Some(match value("type")?.as_str() {
                        "f" | "file" => KindFilter::File,
                        "d" | "dir" | "directory" => KindFilter::Dir,
                        "l" | "link" | "symlink" => KindFilter::Link,
                        other => bail!("unknown type '{other}': use f, d or l"),
                    })
                }
                "ext" | "extension" => q.exts.extend(
                    value("ext")?
                        .split(',')
                        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
                        .filter(|e| !e.is_empty()),
                ),
                "name" => q.names.push(value("name")?),
                "depth" | "maxdepth" => {
                    let v = value("depth")?;
                    q.max_depth = Some(v.parse().with_context(|| format!("--depth: '{v}' is not a number"))?);
                }
                "all" | "a" => q.all = true,
                "empty" => q.empty = true,
                other => bail!("unknown option --{other}"),
            }
        }
        Ok(q)
    }

    /// Whether `entry` passes, given the current time.
    pub fn matches(&self, entry: &Entry, now: SystemTime) -> bool {
        if !self.names.is_empty() && !self.names.iter().any(|g| name_matches(g, &entry.name)) {
            return false;
        }
        if let Some(kind) = self.kind {
            let ok = match kind {
                KindFilter::File => entry.kind == EntryKind::File,
                KindFilter::Dir => entry.kind == EntryKind::Dir,
                KindFilter::Link => entry.is_symlink(),
            };
            if !ok {
                return false;
            }
        }
        if let Some(size) = self.size {
            if entry.is_dir() || !size.admits(entry.size) {
                return false;
            }
        }
        if let Some(age) = self.age {
            let Some(age_now) = entry.modified.map(|m| now.duration_since(m).unwrap_or_default()) else {
                return false;
            };
            if !age.admits(age_now) {
                return false;
            }
        }
        if !self.exts.is_empty() && !self.exts.contains(&entry.extension()) {
            return false;
        }
        if self.empty && !is_empty(entry) {
            return false;
        }
        true
    }
}

/// Globs match the whole name; a plain word matches anywhere in it.
fn name_matches(pattern: &str, name: &str) -> bool {
    if pattern.contains(['*', '?']) {
        glob_match(pattern, name)
    } else {
        name.to_lowercase().contains(&pattern.to_lowercase())
    }
}

fn is_empty(entry: &Entry) -> bool {
    match entry.kind {
        EntryKind::Dir => std::fs::read_dir(&entry.path).is_ok_and(|mut d| d.next().is_none()),
        EntryKind::File => entry.size == 0,
        _ => false,
    }
}

/// `+X` is "at least X", `-X` is "less than X", a bare `X` means `+X`.
fn parse_bound<T>(input: &str, parse: impl Fn(&str) -> Result<T>) -> Result<Bound<T>> {
    Ok(match input.strip_prefix('-') {
        Some(rest) => Bound::Below(parse(rest)?),
        None => Bound::Above(parse(input.strip_prefix('+').unwrap_or(input))?),
    })
}

/// `500`, `10k`, `1.5M`, `2G`, `1T` (powers of 1024; a trailing `B`/`iB` is
/// allowed).
pub fn parse_size(input: &str) -> Result<u64> {
    let s = input.trim().to_ascii_lowercase();
    let s = s.trim_end_matches("ib").trim_end_matches('b');
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (number, unit) = s.split_at(split);
    let number: f64 = number.parse().ok().with_context(|| format!("'{input}' is not a size like 500K, 10M or 2G"))?;
    let scale: u64 = match unit {
        "" => 1,
        "k" => 1 << 10,
        "m" => 1 << 20,
        "g" => 1 << 30,
        "t" => 1 << 40,
        _ => bail!("'{input}' is not a size like 500K, 10M or 2G"),
    };
    Ok((number * scale as f64) as u64)
}

/// `30s`, `15m`, `12h`, `7d`, `2w`, `6mo`, `1y`; a bare number is days.
pub fn parse_age(input: &str) -> Result<Duration> {
    let s = input.trim().to_ascii_lowercase();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (number, unit) = s.split_at(split);
    let number: f64 = number.parse().ok().with_context(|| format!("'{input}' is not an age like 12h, 7d or 1y"))?;
    let seconds = match unit {
        "s" => 1.0,
        "m" | "min" => 60.0,
        "h" => 3600.0,
        "" | "d" => 86_400.0,
        "w" => 7.0 * 86_400.0,
        "mo" => 30.0 * 86_400.0,
        "y" => 365.0 * 86_400.0,
        _ => bail!("'{input}' is not an age like 12h, 7d or 1y"),
    };
    Ok(Duration::from_secs_f64(number * seconds))
}

/// Entries found by [`walk`].
#[derive(Debug, Default)]
pub struct Found {
    pub entries: Vec<Entry>,
    /// The walk stopped at [`LIMIT`].
    pub truncated: bool,
}

/// Lists everything below `root` that matches `query`, without `root`.
/// Hidden items are skipped unless `show_hidden`.
pub fn walk(root: &Path, query: &Query, show_hidden: bool, cancel: &AtomicBool) -> Found {
    let now = SystemTime::now();
    let mut walker = walkdir::WalkDir::new(root).follow_links(false).min_depth(1);
    if let Some(depth) = query.max_depth {
        walker = walker.max_depth(depth.max(1));
    }
    let mut found = Found::default();
    let entries = walker.into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        let skipped = !query.all && e.file_type().is_dir() && SKIP_DIRS.contains(&name.as_ref());
        !skipped && (show_hidden || !name.starts_with('.'))
    });
    for e in entries.filter_map(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entry) = LocalVfs::entry_for(e.path()) else { continue };
        if query.matches(&entry, now) {
            if found.entries.len() == LIMIT {
                found.truncated = true;
                break;
            }
            found.entries.push(entry);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags_and_globs() {
        let q = Query::parse("*.log --size +100M --mtime -7d --type f --ext=gz,.ZIP").unwrap();
        assert_eq!(q.names, vec!["*.log"]);
        assert_eq!(q.size, Some(Bound::Above(100 << 20)));
        assert_eq!(q.age, Some(Bound::Below(Duration::from_secs(7 * 86_400))));
        assert_eq!(q.kind, Some(KindFilter::File));
        assert_eq!(q.exts, vec!["gz", "zip"]);
        assert!(Query::parse("--size").is_err());
        assert!(Query::parse("--type x").is_err());
        assert!(Query::parse("--bogus 1").is_err());
    }

    #[test]
    fn sizes_and_ages() {
        assert_eq!(parse_size("1.5k").unwrap(), 1536);
        assert_eq!(parse_size("2GiB").unwrap(), 2 << 30);
        assert_eq!(parse_size("10").unwrap(), 10);
        assert!(parse_size("ten").is_err());
        assert_eq!(parse_age("2w").unwrap(), Duration::from_secs(14 * 86_400));
        assert_eq!(parse_age("3").unwrap(), Duration::from_secs(3 * 86_400));
        assert!(parse_age("1x").is_err());
    }

    #[test]
    fn walks_with_filters() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("a/big.log"), vec![0u8; 4096]).unwrap();
        std::fs::write(root.join("a/b/small.log"), "x").unwrap();
        std::fs::write(root.join("a/b/notes.txt"), "").unwrap();
        std::fs::write(root.join("node_modules/dep.log"), "x").unwrap();
        std::fs::write(root.join(".hidden.log"), "x").unwrap();
        let cancel = AtomicBool::new(false);
        let names = |q: &str, hidden: bool| {
            let mut v: Vec<String> =
                walk(root, &Query::parse(q).unwrap(), hidden, &cancel).entries.into_iter().map(|e| e.name).collect();
            v.sort();
            v
        };
        assert_eq!(names("*.log", false), vec!["big.log", "small.log"]);
        assert_eq!(names("*.log", true), vec![".hidden.log", "big.log", "small.log"]);
        assert_eq!(names("*.log --all", false), vec!["big.log", "dep.log", "small.log"]);
        assert_eq!(names("--size +1k", false), vec!["big.log"]);
        assert_eq!(names("--type d", false), vec!["a", "b"]);
        assert_eq!(names("--empty --type f", false), vec!["notes.txt"]);
        assert_eq!(names("--depth 2 --type f", false), vec!["big.log"]);
        assert_eq!(names("note", false), vec!["notes.txt"]);
        assert_eq!(names("--mtime +1d", false), Vec::<String>::new());
    }
}
