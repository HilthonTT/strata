//! Folders ranked by how often and how recently they were visited, for
//! jumping back to them (`jump`, `:z`). Scoring follows zoxide: every
//! visit adds 1 to a folder's rank, and the rank is weighed by how long
//! ago the last visit was. When the ranks add up to more than
//! [`MAX_TOTAL`], all of them shrink and folders falling below 1 are
//! forgotten, so old habits fade.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

/// Sum of ranks above which every rank is aged (zoxide's `_ZO_MAXAGE`).
pub const MAX_TOTAL: f64 = 10_000.0;

/// Most folders kept; the lowest ranked go first.
pub const MAX_DIRS: usize = 2_000;

const HOUR: u64 = 60 * 60;
const DAY: u64 = 24 * HOUR;
const WEEK: u64 = 7 * DAY;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Dir {
    path: PathBuf,
    rank: f64,
    /// Last visit, in seconds since the Unix epoch.
    last: u64,
}

impl Dir {
    fn score(&self, now: u64) -> f64 {
        let age = now.saturating_sub(self.last);
        let weight = match age {
            a if a < HOUR => 4.0,
            a if a < DAY => 2.0,
            a if a < WEEK => 0.5,
            _ => 0.25,
        };
        self.rank * weight
    }
}

/// The visited folders.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Frecency {
    dirs: Vec<Dir>,
}

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Frecency {
    /// Reads the database; a missing or unreadable file is an empty one.
    pub fn load(file: &Path) -> Self {
        std::fs::read(file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    /// Writes the database, through a temporary file so a crash never
    /// leaves half of it.
    pub fn save(&self, file: &Path) -> Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)
            .and_then(|()| std::fs::rename(&tmp, file))
            .with_context(|| format!("saving visited folders to {}", file.display()))
    }

    pub fn len(&self) -> usize {
        self.dirs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }

    /// Records a visit to `dir` at `now`.
    pub fn add(&mut self, dir: &Path, now: u64) {
        match self.dirs.iter_mut().find(|d| d.path == dir) {
            Some(d) => {
                d.rank += 1.0;
                d.last = d.last.max(now);
            }
            None => self.dirs.push(Dir { path: dir.to_path_buf(), rank: 1.0, last: now }),
        }
        self.age();
    }

    /// Shrinks every rank once they add up to more than [`MAX_TOTAL`],
    /// forgetting folders that fall below 1, and keeps at most
    /// [`MAX_DIRS`] folders.
    fn age(&mut self) {
        let total: f64 = self.dirs.iter().map(|d| d.rank).sum();
        if total > MAX_TOTAL {
            let factor = 0.9 * MAX_TOTAL / total;
            for d in &mut self.dirs {
                d.rank *= factor;
            }
            self.dirs.retain(|d| d.rank >= 1.0);
        }
        if self.dirs.len() > MAX_DIRS {
            // Among equal ranks the latest visit stays, so a new folder is not dropped at once.
            self.dirs.sort_by(|a, b| b.rank.total_cmp(&a.rank).then(b.last.cmp(&a.last)));
            self.dirs.truncate(MAX_DIRS);
        }
    }

    pub fn remove(&mut self, dir: &Path) {
        self.dirs.retain(|d| d.path != dir);
    }

    /// Forgets folders for which `exists` is false. Returns how many.
    pub fn prune(&mut self, exists: impl Fn(&Path) -> bool) -> usize {
        let before = self.dirs.len();
        self.dirs.retain(|d| exists(&d.path));
        before - self.dirs.len()
    }

    /// Score of `dir` at `now`; 0 for folders never visited.
    pub fn score(&self, dir: &Path, now: u64) -> f64 {
        self.dirs.iter().find(|d| d.path == dir).map_or(0.0, |d| d.score(now))
    }

    /// Every folder, best first.
    pub fn ranked(&self, now: u64) -> Vec<PathBuf> {
        let mut dirs: Vec<(&Dir, f64)> = self.dirs.iter().map(|d| (d, d.score(now))).collect();
        dirs.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.path.cmp(&b.0.path)));
        dirs.into_iter().map(|(d, _)| d.path.clone()).collect()
    }

    /// The best folder matching every word of `query` (see
    /// [`matches_query`]), other than `except`.
    pub fn best(&self, query: &str, now: u64, except: Option<&Path>) -> Option<PathBuf> {
        let words: Vec<&str> = query.split_whitespace().collect();
        self.ranked(now).into_iter().find(|p| Some(p.as_path()) != except && matches_query(p, &words))
    }
}

/// Like zoxide: the words appear in the path in order, and the last one in
/// the folder's own name. Case is ignored unless a word has a capital.
pub fn matches_query(path: &Path, words: &[&str]) -> bool {
    let Some(last) = words.last() else {
        return true;
    };
    let ignore_case = !words.iter().any(|w| w.chars().any(char::is_uppercase));
    let fold = |s: &str| if ignore_case { s.to_lowercase() } else { s.to_string() };
    let full = fold(&crate::util::posix(path));
    let mut from = 0;
    for word in words {
        let word = fold(word);
        match full[from..].find(&word) {
            Some(i) => from += i + word.len(),
            None => return false,
        }
    }
    let name = path.file_name().map(|n| fold(&n.to_string_lossy())).unwrap_or_default();
    name.contains(&fold(last))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000_000;

    #[test]
    fn frequent_and_recent_folders_rank_first() {
        let mut db = Frecency::default();
        for _ in 0..3 {
            db.add(Path::new("/old/often"), NOW - 2 * WEEK);
        }
        db.add(Path::new("/new/once"), NOW - 60);
        db.add(Path::new("/day/twice"), NOW - 2 * HOUR);
        db.add(Path::new("/day/twice"), NOW - 2 * HOUR);
        // 1 × 4 (this hour) and 2 × 2 (today) beat 3 × 0.25 (weeks ago).
        assert_eq!(db.score(Path::new("/new/once"), NOW), 4.0);
        assert_eq!(db.score(Path::new("/day/twice"), NOW), 4.0);
        assert_eq!(db.score(Path::new("/old/often"), NOW), 0.75);
        assert_eq!(db.score(Path::new("/never"), NOW), 0.0);
        let ranked = db.ranked(NOW);
        assert_eq!(ranked.last().unwrap(), Path::new("/old/often"));

        // Visiting it again makes it recent.
        db.add(Path::new("/old/often"), NOW);
        assert_eq!(db.ranked(NOW)[0], Path::new("/old/often"));
    }

    #[test]
    fn ranks_age_once_they_add_up() {
        let mut db = Frecency::default();
        db.add(Path::new("/rare"), NOW);
        for _ in 0..MAX_TOTAL as usize {
            db.add(Path::new("/busy"), NOW);
        }
        let total: f64 = db.dirs.iter().map(|d| d.rank).sum();
        assert!(total <= MAX_TOTAL, "{total}");
        assert_eq!(db.len(), 1, "a folder visited once fades away");
        assert_eq!(db.ranked(NOW), [PathBuf::from("/busy")]);
    }

    #[test]
    fn keeps_at_most_max_dirs() {
        let mut db = Frecency::default();
        db.add(Path::new("/keep"), NOW);
        db.add(Path::new("/keep"), NOW);
        for i in 0..MAX_DIRS + 10 {
            db.add(&PathBuf::from(format!("/d{i}")), NOW);
        }
        assert_eq!(db.len(), MAX_DIRS);
        assert!(db.score(Path::new("/keep"), NOW) > 0.0, "the highest ranked survive");
    }

    #[test]
    fn queries_match_words_in_order_ending_in_the_name() {
        let p = Path::new("/home/me/projects/strata");
        assert!(matches_query(p, &["strata"]));
        assert!(matches_query(p, &["pro", "str"]));
        assert!(!matches_query(p, &["STR"]), "capitals make it case-sensitive");
        assert!(!matches_query(p, &["projects"]), "the last word must be in the folder's name");
        assert!(!matches_query(p, &["strata", "pro"]), "words in order");
        assert!(!matches_query(p, &["Strata"]));
        assert!(matches_query(p, &[]));

        let mut db = Frecency::default();
        db.add(Path::new("/home/me/projects/strata"), NOW);
        db.add(Path::new("/home/me/projects/strata"), NOW);
        db.add(Path::new("/home/me/old/strata"), NOW);
        db.add(Path::new("/home/me/projects"), NOW);
        assert_eq!(db.best("strata", NOW, None).unwrap(), Path::new("/home/me/projects/strata"));
        assert_eq!(db.best("old str", NOW, None).unwrap(), Path::new("/home/me/old/strata"));
        let here = Path::new("/home/me/projects/strata");
        assert_eq!(db.best("strata", NOW, Some(here)).unwrap(), Path::new("/home/me/old/strata"), "not where we are");
        assert!(db.best("nothing", NOW, None).is_none());
    }

    #[test]
    fn persists_and_prunes_missing_folders() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/jumps.json");
        let mut db = Frecency::default();
        db.add(dir.path(), NOW);
        db.add(&dir.path().join("gone"), NOW);
        db.save(&file).unwrap();
        let mut loaded = Frecency::load(&file);
        assert_eq!(loaded, db);
        assert_eq!(loaded.prune(Path::is_dir), 1);
        assert_eq!(loaded.ranked(NOW), [dir.path().to_path_buf()]);
        loaded.remove(dir.path());
        assert!(loaded.is_empty());

        std::fs::write(&file, "not json").unwrap();
        assert!(Frecency::load(&file).is_empty(), "a damaged file starts afresh");
        assert!(Frecency::load(&dir.path().join("missing.json")).is_empty());
    }
}
