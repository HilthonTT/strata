//! One file panel: a directory listing with cursor, marks, filter and history.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use strata_core::find::{self, Query};
use strata_core::search::Fuzzy;
use strata_core::sort::{sort_entries, SortKey, SortOptions};
use strata_core::{Entry, EntryKind, VfsRef};

/// A directory listing produced off the UI thread (remote locations).
pub struct Listing {
    pub panel: u64,
    generation: u64,
    path: PathBuf,
    result: Result<Vec<Entry>, String>,
    /// A flat listing stopped at its limit.
    truncated: bool,
}

/// What a flat listing shows instead of the directory's own entries.
#[derive(Clone)]
pub enum FlatSource {
    /// Everything below the directory matching a query (`:find`, `:flatten`).
    Walk(Query),
}

/// A recursive listing shown in place of the directory.
#[derive(Clone)]
pub struct Flat {
    /// Shown in the panel title, e.g. `find *.log --size +1M`.
    pub title: String,
    pub source: FlatSource,
    /// The listing stopped at [`find::LIMIT`] entries.
    pub truncated: bool,
    /// The result was reported (no matches, truncated) after it arrived.
    pub announced: bool,
}

/// Where background listings are delivered.
pub type Lister = Sender<Listing>;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct Panel {
    /// Identifies the panel for background listings.
    pub id: u64,
    pub vfs: VfsRef,
    pub cwd: PathBuf,
    /// Every entry in the directory, sorted (hidden ones included).
    entries: Vec<Entry>,
    /// Indices into `entries` that are shown, with fuzzy-match positions.
    pub visible: Vec<(usize, Vec<u32>)>,
    pub cursor: usize,
    pub offset: usize,
    pub marked: HashSet<PathBuf>,
    pub filter: String,
    pub sort: SortOptions,
    pub show_hidden: bool,
    pub visual_anchor: Option<usize>,
    pub error: Option<String>,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    /// Rows available in the last render, for paging.
    pub page: usize,
    /// Remote listings run in the background when this is set.
    lister: Option<Lister>,
    generation: u64,
    /// A background listing is in flight.
    pub loading: bool,
    /// Name to put the cursor on once the listing arrives.
    pending_focus: Option<String>,
    /// Counts listings, so folder sizes are copied into each new one.
    pub listed: u64,
    /// `(listed, sizes version)` last given folder sizes.
    pub sizes_seen: (u64, u64),
    /// A flat listing replacing the directory's entries, if any.
    pub flat: Option<Flat>,
    /// Stops the walk of the flat listing being built.
    walk_cancel: Arc<AtomicBool>,
}

impl Panel {
    pub fn new(vfs: VfsRef, cwd: PathBuf, sort: SortOptions, show_hidden: bool, lister: Option<Lister>) -> Self {
        let mut panel = Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            vfs,
            cwd,
            entries: Vec::new(),
            visible: Vec::new(),
            cursor: 0,
            offset: 0,
            marked: HashSet::new(),
            filter: String::new(),
            sort,
            show_hidden,
            visual_anchor: None,
            error: None,
            back: Vec::new(),
            forward: Vec::new(),
            page: 20,
            lister,
            generation: 0,
            loading: false,
            pending_focus: None,
            listed: 0,
            sizes_seen: (0, 0),
            flat: None,
            walk_cancel: Arc::new(AtomicBool::new(false)),
        };
        panel.reload();
        panel
    }

    pub fn len(&self) -> usize {
        self.visible.len()
    }

    pub fn entry_at(&self, row: usize) -> Option<&Entry> {
        self.visible.get(row).and_then(|(i, _)| self.entries.get(*i))
    }

    pub fn hovered(&self) -> Option<&Entry> {
        self.entry_at(self.cursor)
    }

    /// How an entry is listed: its name, or in a flat listing its path
    /// relative to the directory.
    pub fn label<'a>(&self, entry: &'a Entry) -> Cow<'a, str> {
        if self.flat.is_none() {
            return Cow::Borrowed(&entry.name);
        }
        match entry.path.strip_prefix(&self.cwd) {
            Ok(rel) => Cow::Owned(strata_core::util::posix(rel)),
            Err(_) => Cow::Owned(super::short_path(&entry.path)),
        }
    }

    /// Replaces the listing with a flat one; `None` goes back to the
    /// directory's own entries.
    pub fn set_flat(&mut self, flat: Option<Flat>) {
        self.flat = flat;
        self.filter.clear();
        self.marked.clear();
        self.visual_anchor = None;
        self.cursor = 0;
        self.offset = 0;
        self.entries.clear();
        self.visible.clear();
        self.reload();
    }

    /// Re-reads the directory, keeping the cursor on the same name if
    /// possible. Remote locations are listed in the background so a slow
    /// server never freezes the UI.
    pub fn reload(&mut self) {
        if self.pending_focus.is_none() {
            self.pending_focus = self.hovered().map(|e| self.label(e).into_owned());
        }
        if let Some(flat) = &self.flat {
            return self.reload_flat(flat.source.clone());
        }
        match self.lister.clone().filter(|_| !self.vfs.is_local()) {
            Some(tx) => {
                self.generation += 1;
                self.loading = true;
                let (vfs, path, panel, generation) = (self.vfs.clone(), self.cwd.clone(), self.id, self.generation);
                std::thread::spawn(move || {
                    let result = vfs.read_dir(&path).map_err(|e| format!("{e:#}"));
                    let _ = tx.send(Listing { panel, generation, path, result, truncated: false });
                });
            }
            None => {
                let result = self.vfs.read_dir(&self.cwd).map_err(|e| format!("{e:#}"));
                self.apply_entries(result);
            }
        }
    }

    /// Builds a flat listing, in the background when there is a lister.
    fn reload_flat(&mut self, source: FlatSource) {
        self.walk_cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.walk_cancel = cancel.clone();
        let (vfs, root, show_hidden) = (self.vfs.clone(), self.cwd.clone(), self.show_hidden);
        let build = move || -> (Result<Vec<Entry>, String>, bool) {
            match source {
                FlatSource::Walk(query) if vfs.is_local() => {
                    let found = find::walk(&root, &query, show_hidden, &cancel);
                    (Ok(found.entries), found.truncated)
                }
                FlatSource::Walk(_) => (Err("searching works on local folders".into()), false),
            }
        };
        match self.lister.clone() {
            Some(tx) => {
                self.generation += 1;
                self.loading = true;
                let (path, panel, generation) = (self.cwd.clone(), self.id, self.generation);
                std::thread::spawn(move || {
                    let (result, truncated) = build();
                    let _ = tx.send(Listing { panel, generation, path, result, truncated });
                });
            }
            None => {
                let (result, truncated) = build();
                if let Some(flat) = &mut self.flat {
                    flat.truncated = truncated;
                }
                self.apply_entries(result);
            }
        }
    }

    /// Applies a background listing. Returns false if it is stale.
    pub fn apply_listing(&mut self, listing: Listing) -> bool {
        if listing.panel != self.id || listing.generation != self.generation || listing.path != self.cwd {
            return false;
        }
        self.loading = false;
        if let Some(flat) = &mut self.flat {
            flat.truncated = listing.truncated;
        }
        self.apply_entries(listing.result);
        true
    }

    fn apply_entries(&mut self, result: Result<Vec<Entry>, String>) {
        self.listed += 1;
        match result {
            Ok(mut entries) => {
                sort_entries(&mut entries, self.sort);
                self.entries = entries;
                self.error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(e);
            }
        }
        let names: HashSet<&PathBuf> = self.entries.iter().map(|e| &e.path).collect();
        self.marked.retain(|p| names.contains(p));
        self.refilter();
        if let Some(name) = self.pending_focus.take() {
            self.focus_name(&name);
        }
    }

    /// Folders listed here (not links to folders).
    pub fn dirs(&self) -> impl Iterator<Item = &Path> {
        self.entries.iter().filter(|e| e.kind == EntryKind::Dir).map(|e| e.path.as_path())
    }

    /// Uses measured folder sizes in place of the folders' own sizes,
    /// re-sorting when sorted by size.
    pub fn apply_dir_sizes(&mut self, sizes: &HashMap<PathBuf, u64>) {
        let mut changed = false;
        for e in self.entries.iter_mut().filter(|e| e.kind == EntryKind::Dir) {
            if let Some(&size) = sizes.get(&e.path) {
                changed |= e.size != size;
                e.size = size;
            }
        }
        if changed && self.sort.key == SortKey::Size {
            self.resort();
        }
    }

    pub fn resort(&mut self) {
        let keep = self.hovered().map(|e| e.name.clone());
        sort_entries(&mut self.entries, self.sort);
        self.refilter();
        if let Some(name) = keep {
            self.focus_name(&name);
        }
    }

    pub fn refilter(&mut self) {
        let candidates: Vec<usize> =
            (0..self.entries.len()).filter(|&i| self.show_hidden || !self.entries[i].is_hidden()).collect();
        self.visible = if self.filter.is_empty() {
            candidates.into_iter().map(|i| (i, Vec::new())).collect()
        } else {
            let mut fuzzy = Fuzzy::default();
            let labels: Vec<Cow<str>> = candidates.iter().map(|&i| self.label(&self.entries[i])).collect();
            let matches = fuzzy.filter(&self.filter, labels.iter().map(|l| l.as_ref()));
            matches.into_iter().map(|m| (candidates[m.index], m.positions)).collect()
        };
        self.clamp();
    }

    pub fn set_filter(&mut self, filter: &str) {
        self.filter = filter.to_string();
        self.cursor = 0;
        self.refilter();
    }

    /// Changes directory, recording history. Returns false on failure.
    pub fn cd(&mut self, path: PathBuf) -> bool {
        if path == self.cwd {
            self.reload();
            return true;
        }
        // Local folders are checked first so a failed cd keeps the listing;
        // remote ones report errors when their listing arrives.
        if self.vfs.is_local() {
            if let Err(e) = self.vfs.read_dir(&path) {
                self.error = Some(format!("{e:#}"));
                return false;
            }
        }
        let previous = std::mem::replace(&mut self.cwd, path);
        self.back.push(previous.clone());
        self.forward.clear();
        self.after_cd(Some(&previous));
        true
    }

    /// Switches to another filesystem (SFTP, Docker...).
    pub fn switch_vfs(&mut self, vfs: VfsRef, path: PathBuf) {
        self.vfs = vfs;
        self.cwd = path;
        self.back.clear();
        self.forward.clear();
        self.after_cd(None);
    }

    fn after_cd(&mut self, previous: Option<&Path>) {
        self.flat = None;
        self.walk_cancel.store(true, Ordering::Relaxed);
        self.filter.clear();
        self.marked.clear();
        self.visual_anchor = None;
        self.cursor = 0;
        self.offset = 0;
        self.entries.clear();
        self.visible.clear();
        self.error = None;
        // Coming back up from a child: put the cursor on that child.
        self.pending_focus = previous
            .filter(|prev| prev.parent() == Some(self.cwd.as_path()))
            .and_then(|prev| prev.file_name())
            .map(|name| name.to_string_lossy().into_owned());
        self.reload();
    }

    pub fn parent(&mut self) -> bool {
        match self.vfs.parent(&self.cwd) {
            Some(p) if p != self.cwd => self.cd(p),
            _ => false,
        }
    }

    pub fn go_back(&mut self) -> bool {
        let Some(path) = self.back.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.cwd, path);
        self.forward.push(current.clone());
        self.after_cd(Some(&current));
        true
    }

    pub fn go_forward(&mut self) -> bool {
        let Some(path) = self.forward.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.cwd, path);
        self.back.push(current.clone());
        self.after_cd(Some(&current));
        true
    }

    /// Puts the cursor on the entry listed as `name` (see [`Panel::label`]).
    pub fn focus_name(&mut self, name: &str) {
        if let Some(row) = self.visible.iter().position(|(i, _)| self.label(&self.entries[*i]) == name) {
            self.cursor = row;
            self.clamp();
        }
    }

    pub fn focus_path(&mut self, path: &Path) {
        if let Some(row) = self.visible.iter().position(|(i, _)| self.entries[*i].path == path) {
            self.cursor = row;
            self.clamp();
        }
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let max = self.visible.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
        self.update_visual();
    }

    pub fn move_to(&mut self, row: usize) {
        self.cursor = row.min(self.visible.len().saturating_sub(1));
        self.update_visual();
    }

    fn clamp(&mut self) {
        self.cursor = self.cursor.min(self.visible.len().saturating_sub(1));
    }

    /// Keeps the cursor on screen for a list of `height` rows.
    pub fn scroll_into_view(&mut self, height: usize) {
        self.page = height.max(1);
        if self.cursor < self.offset {
            self.offset = self.cursor;
        } else if self.cursor >= self.offset + height {
            self.offset = self.cursor + 1 - height;
        }
        let max_offset = self.visible.len().saturating_sub(height);
        self.offset = self.offset.min(max_offset);
    }

    pub fn toggle_mark(&mut self) {
        if let Some(path) = self.hovered().map(|e| e.path.clone()) {
            if !self.marked.remove(&path) {
                self.marked.insert(path);
            }
        }
    }

    pub fn mark_all(&mut self) {
        let paths: Vec<PathBuf> = (0..self.len()).filter_map(|r| self.entry_at(r).map(|e| e.path.clone())).collect();
        self.marked.extend(paths);
    }

    pub fn invert_marks(&mut self) {
        let paths: Vec<PathBuf> = (0..self.len()).filter_map(|r| self.entry_at(r).map(|e| e.path.clone())).collect();
        for p in paths {
            if !self.marked.remove(&p) {
                self.marked.insert(p);
            }
        }
    }

    /// Marks entries whose names match a `*`/`?` glob. Returns the count.
    pub fn mark_glob(&mut self, pattern: &str) -> usize {
        let paths: Vec<PathBuf> = (0..self.len())
            .filter_map(|r| self.entry_at(r))
            .filter(|e| glob_match(pattern, &e.name))
            .map(|e| e.path.clone())
            .collect();
        let n = paths.len();
        self.marked.extend(paths);
        n
    }

    pub fn toggle_visual(&mut self) {
        if self.visual_anchor.take().is_none() {
            self.visual_anchor = Some(self.cursor);
            self.update_visual();
        }
    }

    fn update_visual(&mut self) {
        let Some(anchor) = self.visual_anchor else {
            return;
        };
        let (lo, hi) = (anchor.min(self.cursor), anchor.max(self.cursor));
        let paths: Vec<PathBuf> = (lo..=hi).filter_map(|r| self.entry_at(r).map(|e| e.path.clone())).collect();
        self.marked = paths.into_iter().collect();
    }

    /// Marked items, or the hovered one when nothing is marked.
    pub fn targets(&self) -> Vec<PathBuf> {
        if self.marked.is_empty() {
            self.hovered().map(|e| vec![e.path.clone()]).unwrap_or_default()
        } else {
            let mut v: Vec<PathBuf> =
                self.entries.iter().filter(|e| self.marked.contains(&e.path)).map(|e| e.path.clone()).collect();
            v.sort();
            v
        }
    }

    /// Like [`Panel::targets`], with the full entries.
    pub fn target_entries(&self) -> Vec<Entry> {
        if self.marked.is_empty() {
            return self.hovered().cloned().into_iter().collect();
        }
        let mut v: Vec<Entry> = self.entries.iter().filter(|e| self.marked.contains(&e.path)).cloned().collect();
        v.sort_by(|a, b| a.path.cmp(&b.path));
        v
    }

    pub fn is_marked(&self, entry: &Entry) -> bool {
        self.marked.contains(&entry.path)
    }

    pub fn clear_marks(&mut self) -> bool {
        let had = !self.marked.is_empty() || self.visual_anchor.is_some();
        self.marked.clear();
        self.visual_anchor = None;
        had
    }
}

pub use strata_core::util::glob_match;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use strata_core::vfs::LocalVfs;

    fn panel_in(dir: &Path) -> Panel {
        Panel::new(Arc::new(LocalVfs), dir.to_path_buf(), SortOptions::default(), false, None)
    }

    #[test]
    fn globs() {
        assert!(glob_match("*.rs", "main.RS"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("*.rs", "main.rsx"));
    }

    #[test]
    fn navigation_and_marks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::write(dir.path().join(".hidden"), "").unwrap();
        let mut p = panel_in(dir.path());
        assert_eq!(p.len(), 2, "hidden files are not shown");
        assert_eq!(p.hovered().unwrap().name, "sub", "directories first");

        assert!(p.cd(dir.path().join("sub")));
        assert!(p.parent());
        assert_eq!(p.hovered().unwrap().name, "sub", "cursor returns to the child");

        p.move_by(1);
        p.toggle_mark();
        assert_eq!(p.targets(), vec![dir.path().join("b.txt")]);
        p.set_filter("bt");
        assert_eq!(p.len(), 1);
        assert!(p.go_back());
    }

    #[test]
    fn visual_mode_marks_a_range() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["a", "b", "c", "d"] {
            std::fs::write(dir.path().join(n), "").unwrap();
        }
        let mut p = panel_in(dir.path());
        p.toggle_visual();
        p.move_by(2);
        assert_eq!(p.marked.len(), 3);
        p.toggle_visual();
        assert_eq!(p.marked.len(), 3, "leaving visual mode keeps marks");
    }

    #[test]
    fn flat_listings_show_relative_paths_and_leave_on_cd() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/b/deep.txt"), "").unwrap();
        std::fs::write(dir.path().join("top.txt"), "").unwrap();
        let mut p = panel_in(dir.path());
        p.set_flat(Some(Flat {
            title: "flatten".into(),
            source: FlatSource::Walk(Query::files()),
            truncated: false,
            announced: false,
        }));
        let labels: Vec<String> = (0..p.len()).map(|r| p.label(p.entry_at(r).unwrap()).into_owned()).collect();
        assert_eq!(labels, ["a/b/deep.txt", "top.txt"]);
        p.set_filter("deep");
        assert_eq!(p.len(), 1);
        assert_eq!(p.hovered().unwrap().name, "deep.txt", "entries keep their real names");
        p.reload();
        assert_eq!(p.hovered().unwrap().name, "deep.txt", "the cursor stays on the same item");
        assert!(p.cd(dir.path().join("a")));
        assert!(p.flat.is_none());
        assert_eq!(p.hovered().unwrap().name, "b");
    }

    /// Pretends to be remote so listings go through the background path.
    #[derive(Debug)]
    struct Remote(LocalVfs);

    impl strata_core::Vfs for Remote {
        fn scheme(&self) -> &'static str {
            "test"
        }
        fn label(&self) -> String {
            "test".into()
        }
        fn home(&self) -> PathBuf {
            self.0.home()
        }
        fn read_dir(&self, path: &Path) -> anyhow::Result<Vec<Entry>> {
            self.0.read_dir(path)
        }
        fn stat(&self, path: &Path) -> anyhow::Result<Entry> {
            self.0.stat(path)
        }
        fn create_dir(&self, path: &Path) -> anyhow::Result<()> {
            self.0.create_dir(path)
        }
        fn create_file(&self, path: &Path) -> anyhow::Result<()> {
            self.0.create_file(path)
        }
        fn remove_file(&self, path: &Path) -> anyhow::Result<()> {
            self.0.remove_file(path)
        }
        fn remove_dir(&self, path: &Path) -> anyhow::Result<()> {
            self.0.remove_dir(path)
        }
        fn rename(&self, from: &Path, to: &Path) -> anyhow::Result<()> {
            self.0.rename(from, to)
        }
        fn reader(&self, path: &Path) -> anyhow::Result<Box<dyn std::io::Read + Send>> {
            self.0.reader(path)
        }
        fn writer(&self, path: &Path) -> anyhow::Result<Box<dyn std::io::Write + Send>> {
            self.0.writer(path)
        }
    }

    #[test]
    fn remote_listings_arrive_in_the_background_and_stale_ones_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/inner.txt"), "").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let vfs: VfsRef = Arc::new(Remote(LocalVfs));
        let mut p = Panel::new(vfs, dir.path().to_path_buf(), SortOptions::default(), false, Some(tx));
        assert!(p.loading);
        assert_eq!(p.len(), 0, "nothing is listed synchronously");

        let first = rx.recv().unwrap();
        // Navigate before the first listing is applied: it becomes stale.
        assert!(p.cd(dir.path().join("sub")));
        assert!(!p.apply_listing(first));
        assert!(p.apply_listing(rx.recv().unwrap()));
        assert!(!p.loading);
        assert_eq!(p.hovered().unwrap().name, "inner.txt");

        // Going back up focuses the folder we came from once it is listed.
        assert!(p.parent());
        assert!(p.apply_listing(rx.recv().unwrap()));
        assert_eq!(p.hovered().unwrap().name, "sub");
    }
}
