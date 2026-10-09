//! Fuzzy matching for the in-panel filter and recursive file finder.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// A scored match: index into the input, score and matched char positions.
#[derive(Debug, Clone)]
pub struct Match {
    pub index: usize,
    pub score: u32,
    pub positions: Vec<u32>,
}

pub struct Fuzzy {
    matcher: Matcher,
}

impl Default for Fuzzy {
    fn default() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
        }
    }
}

impl Fuzzy {
    /// Returns the items matching `query`, best first. An empty query keeps
    /// every item in its original order.
    pub fn filter<'a>(
        &mut self,
        query: &str,
        items: impl IntoIterator<Item = &'a str>,
    ) -> Vec<Match> {
        let items = items.into_iter();
        if query.trim().is_empty() {
            return items
                .enumerate()
                .map(|(index, _)| Match {
                    index,
                    score: 0,
                    positions: Vec::new(),
                })
                .collect();
        }
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut matches: Vec<Match> = items
            .enumerate()
            .filter_map(|(index, item)| {
                let mut positions = Vec::new();
                let score = pattern.indices(
                    Utf32Str::new(item, &mut buf),
                    &mut self.matcher,
                    &mut positions,
                )?;
                positions.sort_unstable();
                positions.dedup();
                Some(Match {
                    index,
                    score,
                    positions,
                })
            })
            .collect();
        matches.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
        matches
    }
}

/// Directories never worth descending into when searching.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".cache",
    "__pycache__",
    ".venv",
];

/// Recursively lists files below `root` (relative paths), for fuzzy finding.
pub fn walk_files(
    root: &Path,
    limit: usize,
    show_hidden: bool,
    cancel: &AtomicBool,
) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            e.depth() == 0
                || !(SKIP_DIRS.contains(&name.as_ref()) || (!show_hidden && name.starts_with('.')))
        })
        .filter_map(Result::ok)
        .take_while(|_| !cancel.load(Ordering::Relaxed))
        .filter(|e| e.depth() > 0)
        .filter_map(|e| e.path().strip_prefix(root).ok().map(Path::to_path_buf))
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_ranks_better_matches_first() {
        let mut f = Fuzzy::default();
        let items = ["readme.md", "src/main.rs", "Cargo.toml", "src/app/mod.rs"];
        let m = f.filter("main", items.iter().copied());
        assert_eq!(items[m[0].index], "src/main.rs");
        assert!(f.filter("zzz", items.iter().copied()).is_empty());
        assert_eq!(f.filter("", items.iter().copied()).len(), 4);
    }

    #[test]
    fn walks_and_skips_ignored_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        std::fs::write(dir.path().join(".git/objects/x"), "").unwrap();
        let files = walk_files(dir.path(), 100, true, &AtomicBool::new(false));
        assert!(files.contains(&PathBuf::from("src/lib.rs")));
        assert!(!files.iter().any(|p| p.starts_with(".git")));
    }
}
