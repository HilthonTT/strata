//! One file panel: a directory listing with cursor, marks, filter and
//! history, shown flat or as a tree of expandable folders.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use strata_core::filter::Matcher;
use strata_core::find::{self, Query};
use strata_core::search::Fuzzy;
use strata_core::sort::{sort_entries, SortKey, SortOptions};
use strata_core::{Entry, EntryKind, Vfs, VfsRef};

/// Most entries a tree listing shows, so expanding a huge folder cannot
/// stall the panel.
const TREE_LIMIT: usize = 20_000;

/// How long the UI waits for a local listing before showing the panel as
/// loading and taking the listing whenever it arrives. Most folders list in
/// a few milliseconds; a huge folder, a slow disk or a stale network mount
/// must not freeze the UI.
const LOCAL_WAIT: Duration = Duration::from_millis(100);

/// A directory listing produced off the UI thread.
pub struct Listing {
    pub panel: u64,
    generation: u64,
    path: PathBuf,
    result: Result<Vec<Entry>, String>,
    /// Depth of each entry in a tree listing; empty otherwise.
    depths: Vec<u16>,
    /// A flat or tree listing stopped at its limit.
    truncated: bool,
    /// Entries ignored by git, when they are being hidden.
    ignored: HashSet<PathBuf>,
}

/// What a flat listing shows instead of the directory's own entries.
#[derive(Clone)]
pub enum FlatSource {
    /// Everything below the directory matching a query (`:find`, `:flatten`).
    Walk(Query),
    /// A fixed set of paths (from a plugin).
    Paths(Vec<PathBuf>),
}

/// A recursive or hand-picked listing shown in place of the directory.
#[derive(Clone)]
pub struct Flat {
    /// Shown in the panel title, e.g. `find *.log --size +1M`.
    pub title: String,
    pub source: FlatSource,
    /// The listing stopped at [`find::LIMIT`] entries.
    pub truncated: bool,
    /// The result was reported (no matches, truncated, plugin event) after it arrived.
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
    /// Why `filter` is not a valid regex or glob; the last valid query
    /// keeps filtering meanwhile.
    pub filter_error: Option<String>,
    /// The last valid parse of `filter`.
    matcher: Option<Matcher>,
    pub sort: SortOptions,
    pub show_hidden: bool,
    /// Leave out what git ignores (local folders in a repository).
    pub hide_gitignored: bool,
    /// Entries of the listing that git ignores; filled only while
    /// `hide_gitignored` is on.
    ignored: HashSet<PathBuf>,
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
    /// A local listing that took longer than [`LOCAL_WAIT`], picked up by
    /// [`Panel::poll_listing`].
    inflight: Option<Receiver<Listing>>,
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
    /// Tree view: folders expand in place instead of being entered.
    pub tree: bool,
    /// Folders expanded in the tree view.
    pub expanded: HashSet<PathBuf>,
    /// Depth below `cwd` of each entry in the tree view (parallel to
    /// `entries`; empty outside it).
    depths: Vec<u16>,
    /// The tree listing stopped at [`TREE_LIMIT`] entries.
    pub tree_truncated: bool,
    /// Local folders changed to since last asked, for the jump list.
    visits: Vec<PathBuf>,
    /// Folders that are mostly images or videos show as a grid.
    auto_grid: bool,
    /// The grid or list chosen with `toggle_grid`; `None` follows
    /// `auto_grid`. Forgotten on `cd` while `auto_grid` is on.
    grid_choice: Option<bool>,
    /// The listing is mostly images and videos (see [`super::thumbs::is_media_dir`]).
    media_dir: bool,
    /// Tiles per row in the last grid render; 1 outside the grid.
    pub grid_cols: usize,
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
            filter_error: None,
            matcher: None,
            sort,
            show_hidden,
            hide_gitignored: false,
            ignored: HashSet::new(),
            visual_anchor: None,
            error: None,
            back: Vec::new(),
            forward: Vec::new(),
            page: 20,
            lister,
            generation: 0,
            loading: false,
            inflight: None,
            pending_focus: None,
            listed: 0,
            sizes_seen: (0, 0),
            flat: None,
            walk_cancel: Arc::new(AtomicBool::new(false)),
            tree: false,
            expanded: HashSet::new(),
            depths: Vec::new(),
            tree_truncated: false,
            visits: Vec::new(),
            auto_grid: false,
            grid_choice: None,
            media_dir: false,
            grid_cols: 1,
        };
        panel.reload();
        panel
    }

    /// Hides what git ignores from the start (lists the folder again).
    pub fn with_gitignored_hidden(mut self, hide: bool) -> Self {
        self.set_hide_gitignored(hide);
        self
    }

    /// Shows folders of mostly images or videos as a grid of thumbnails.
    pub fn with_auto_grid(mut self, on: bool) -> Self {
        self.auto_grid = on;
        self
    }

    /// The grid of thumbnails is shown (never with a tree or flat listing).
    pub fn is_grid(&self) -> bool {
        self.flat.is_none() && !self.tree && self.grid_choice.unwrap_or(self.auto_grid && self.media_dir)
    }

    /// Switches between the grid and the list, leaving the tree view.
    pub fn set_grid(&mut self, on: bool) {
        self.grid_choice = Some(on);
        if on {
            self.set_tree(false);
            if self.flat.is_some() {
                self.set_flat(None);
            }
        }
    }

    /// How far `up`/`down` move: a row of tiles in the grid.
    pub fn row_step(&self) -> usize {
        if self.is_grid() {
            self.grid_cols.max(1)
        } else {
            1
        }
    }

    /// Hides or shows what git ignores. Ignored items are only looked up
    /// while hidden, so turning it on lists the folder again.
    pub fn set_hide_gitignored(&mut self, hide: bool) {
        if self.hide_gitignored == hide {
            return;
        }
        self.hide_gitignored = hide;
        if hide {
            self.reload();
        } else {
            self.ignored.clear();
            self.refilter();
        }
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

    /// Depth below the panel's directory of the entry on `row` (tree view).
    pub fn depth_at(&self, row: usize) -> u16 {
        self.visible.get(row).and_then(|(i, _)| self.depths.get(*i)).copied().unwrap_or(0)
    }

    /// The tree view is on and shows folders nested (not a flat listing).
    pub fn is_tree(&self) -> bool {
        self.tree && self.flat.is_none()
    }

    pub fn is_expanded(&self, entry: &Entry) -> bool {
        self.is_tree() && self.expanded.contains(&entry.path)
    }

    /// How an entry is listed: its name, or in a flat listing or tree its
    /// path relative to the directory (unique, so the cursor can find it
    /// again, and what the filter matches).
    pub fn label<'a>(&self, entry: &'a Entry) -> Cow<'a, str> {
        if self.flat.is_none() && !self.tree {
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
    /// possible. Listings run off the UI thread so a slow server or mount
    /// never freezes it (see [`Panel::start_listing`]).
    pub fn reload(&mut self) {
        if self.pending_focus.is_none() {
            self.pending_focus = self.hovered().map(|e| self.label(e).into_owned());
        }
        if let Some(flat) = &self.flat {
            return self.reload_flat(flat.source.clone());
        }
        let (vfs, root) = (self.vfs.clone(), self.cwd.clone());
        let remote = !vfs.is_local();
        if self.tree {
            let (expanded, sort) = (self.expanded.clone(), self.sort);
            return self.start_listing(remote, move || build_tree(&*vfs, &root, &expanded, sort));
        }
        self.start_listing(remote, move || (vfs.read_dir(&root).map_err(|e| format!("{e:#}")), Vec::new(), false));
    }

    /// Lists on a worker thread. With `background` (and a lister) the panel
    /// shows as loading until the listing arrives; otherwise it waits up to
    /// [`LOCAL_WAIT`] so a quick listing shows at once, without flicker.
    fn start_listing<F>(&mut self, background: bool, build: F)
    where
        F: FnOnce() -> (Result<Vec<Entry>, String>, Vec<u16>, bool) + Send + 'static,
    {
        self.generation += 1;
        self.inflight = None;
        let (panel, generation, path) = (self.id, self.generation, self.cwd.clone());
        let gitignored = self.hide_gitignored && self.vfs.is_local();
        let run = move || {
            let (result, depths, truncated) = build();
            // Asked of git here, on the worker, so a big repository never stalls the UI.
            let ignored = match &result {
                Ok(entries) if gitignored => strata_core::git::ignored(entries.iter().map(|e| e.path.as_path())),
                _ => HashSet::new(),
            };
            Listing { panel, generation, path, result, depths, truncated, ignored }
        };
        if let Some(tx) = self.lister.clone().filter(|_| background) {
            self.loading = true;
            std::thread::spawn(move || {
                let _ = tx.send(run());
            });
            return;
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(run());
        });
        match rx.recv_timeout(LOCAL_WAIT) {
            Ok(listing) => {
                self.apply_listing(listing);
            }
            Err(_) => {
                self.loading = true;
                self.inflight = Some(rx);
            }
        }
    }

    /// Applies a slow local listing if it has arrived. Returns whether the
    /// entries changed.
    pub fn poll_listing(&mut self) -> bool {
        let Some(rx) = &self.inflight else {
            return false;
        };
        match rx.try_recv() {
            Ok(listing) => {
                self.inflight = None;
                self.apply_listing(listing)
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.inflight = None;
                self.loading = false;
                false
            }
        }
    }

    /// Turns the tree view on or off, keeping the cursor on the same item
    /// (or, leaving the tree, on the top-level folder holding it).
    pub fn set_tree(&mut self, on: bool) {
        if self.tree == on {
            return;
        }
        let keep = self.hovered().map(|e| e.path.clone());
        if self.flat.is_some() {
            self.flat = None;
            self.walk_cancel.store(true, Ordering::Relaxed);
        }
        self.tree = on;
        self.expanded.clear();
        self.tree_truncated = false;
        self.filter.clear();
        self.visual_anchor = None;
        self.pending_focus = keep
            .as_deref()
            .and_then(|p| p.strip_prefix(&self.cwd).ok())
            .and_then(|rel| rel.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned());
        self.reload();
    }

    /// Expands or collapses the hovered folder. False when it is not one.
    pub fn toggle_expand(&mut self) -> bool {
        let Some(entry) = self.hovered().filter(|e| e.is_dir()) else {
            return false;
        };
        let path = entry.path.clone();
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        self.reload();
        true
    }

    /// Tree view `h`: collapses the hovered folder, or moves to the folder
    /// holding the hovered item. False at the top level.
    pub fn collapse_or_up(&mut self) -> bool {
        let Some(entry) = self.hovered() else {
            return false;
        };
        let path = entry.path.clone();
        if self.expanded.remove(&path) {
            self.reload();
            return true;
        }
        if self.depth_at(self.cursor) == 0 {
            return false;
        }
        match self.vfs.parent(&path) {
            Some(parent) => {
                self.focus_path(&parent);
                true
            }
            None => false,
        }
    }

    /// Collapses every folder, keeping the cursor on the top-level one
    /// holding the hovered item.
    pub fn collapse_all(&mut self) {
        let top = self
            .hovered()
            .and_then(|e| e.path.strip_prefix(&self.cwd).ok())
            .and_then(|rel| rel.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned());
        self.expanded.clear();
        self.pending_focus = top;
        self.reload();
    }

    /// Where new and pasted items go: in the tree view, inside the hovered
    /// folder when it is expanded, else next to the hovered item.
    pub fn target_dir(&self) -> PathBuf {
        if self.is_tree() {
            if let Some(entry) = self.hovered() {
                if self.expanded.contains(&entry.path) {
                    return entry.path.clone();
                }
                if let Some(parent) = self.vfs.parent(&entry.path) {
                    return parent;
                }
            }
        }
        self.cwd.clone()
    }

    /// Whether this panel lists the contents of `dir` (its own directory
    /// or an expanded folder).
    pub fn shows_dir(&self, dir: &Path) -> bool {
        self.cwd == dir || (self.is_tree() && self.expanded.contains(dir))
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
                FlatSource::Paths(paths) => (Ok(paths.iter().filter_map(|p| vfs.stat(p).ok()).collect()), false),
            }
        };
        self.start_listing(true, move || {
            let (result, truncated) = build();
            (result, Vec::new(), truncated)
        });
    }

    /// Applies a background listing. Returns false if it is stale.
    pub fn apply_listing(&mut self, listing: Listing) -> bool {
        if listing.panel != self.id || listing.generation != self.generation || listing.path != self.cwd {
            return false;
        }
        self.loading = false;
        match &mut self.flat {
            Some(flat) => flat.truncated = listing.truncated,
            None => self.tree_truncated = listing.truncated,
        }
        self.ignored = listing.ignored;
        self.apply_entries(listing.result, listing.depths);
        true
    }

    /// Takes a new listing; `depths` is empty unless it is a tree.
    fn apply_entries(&mut self, result: Result<Vec<Entry>, String>, depths: Vec<u16>) {
        self.listed += 1;
        self.depths.clear();
        match result {
            Ok(entries) if depths.len() == entries.len() && !depths.is_empty() => {
                (self.entries, self.depths) =
                    sort_tree(entries.into_iter().zip(depths).collect(), self.sort).into_iter().unzip();
                self.error = None;
            }
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
        // Folders that are gone (or now listed elsewhere) stay collapsed.
        if self.is_tree() && self.error.is_none() {
            let dirs: HashSet<&PathBuf> = self.entries.iter().filter(|e| e.is_dir()).map(|e| &e.path).collect();
            self.expanded.retain(|p| dirs.contains(p));
        }
        let names: HashSet<&PathBuf> = self.entries.iter().map(|e| &e.path).collect();
        self.marked.retain(|p| names.contains(p));
        if self.flat.is_none() && !self.tree {
            self.media_dir = super::thumbs::is_media_dir(self.entries.iter().filter(|e| !e.is_hidden()));
        }
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
        let keep = self.hovered().map(|e| e.path.clone());
        if self.depths.len() == self.entries.len() && !self.depths.is_empty() {
            let items = std::mem::take(&mut self.entries).into_iter().zip(std::mem::take(&mut self.depths)).collect();
            (self.entries, self.depths) = sort_tree(items, self.sort).into_iter().unzip();
        } else {
            sort_entries(&mut self.entries, self.sort);
        }
        self.refilter();
        if let Some(path) = keep {
            self.focus_path(&path);
        }
    }

    pub fn refilter(&mut self) {
        // Hidden and git-ignored entries are left out, and in a tree
        // everything inside them.
        let mut candidates = Vec::with_capacity(self.entries.len());
        let mut hidden_depth: Option<u16> = None;
        for (i, entry) in self.entries.iter().enumerate() {
            let depth = self.depths.get(i).copied().unwrap_or(0);
            if hidden_depth.is_some_and(|d| depth > d) {
                continue;
            }
            hidden_depth = None;
            let ignored = self.hide_gitignored && self.ignored.contains(&entry.path);
            if ignored || (!self.show_hidden && entry.is_hidden()) {
                hidden_depth = Some(depth);
                continue;
            }
            candidates.push(i);
        }
        self.filter_error = None;
        if self.filter.is_empty() {
            self.matcher = None;
        } else {
            match Matcher::parse(&self.filter) {
                Ok(m) => self.matcher = Some(m),
                // Half-typed regexes keep the last valid one's matches.
                Err(e) => self.filter_error = Some(e),
            }
        }
        self.visible = match &self.matcher {
            None => candidates.into_iter().map(|i| (i, Vec::new())).collect(),
            Some(matcher) => {
                let labels: Vec<Cow<str>> = candidates.iter().map(|&i| self.label(&self.entries[i])).collect();
                let matches = matcher.filter(&mut Fuzzy::default(), labels.iter().map(|l| l.as_ref()));
                matches.into_iter().map(|m| (candidates[m.index], m.positions)).collect()
            }
        };
        self.clamp();
    }

    /// Typing a filter puts the cursor on the best match; clearing it
    /// keeps the cursor on the item found with it.
    pub fn set_filter(&mut self, filter: &str) {
        let keep = filter.is_empty().then(|| self.hovered().map(|e| e.path.clone())).flatten();
        self.filter = filter.to_string();
        self.cursor = 0;
        self.refilter();
        if let Some(path) = keep {
            self.focus_path(&path);
        }
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
            if let Some(Err(e)) = can_open_dir(&path) {
                self.error = Some(e);
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
        self.media_dir = false;
        if self.auto_grid {
            self.grid_choice = None;
        }
        self.expanded.clear();
        self.depths.clear();
        self.tree_truncated = false;
        self.walk_cancel.store(true, Ordering::Relaxed);
        self.filter.clear();
        self.marked.clear();
        self.visual_anchor = None;
        self.cursor = 0;
        self.offset = 0;
        self.entries.clear();
        self.visible.clear();
        self.error = None;
        if self.vfs.is_local() {
            self.visits.push(self.cwd.clone());
        }
        // Coming back up from a child: put the cursor on that child.
        self.pending_focus = previous
            .filter(|prev| prev.parent() == Some(self.cwd.as_path()))
            .and_then(|prev| prev.file_name())
            .map(|name| name.to_string_lossy().into_owned());
        self.reload();
    }

    /// Local folders this panel changed to since the last call.
    pub fn take_visits(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.visits)
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
    /// While a listing is in flight, the cursor goes there once it arrives.
    pub fn focus_name(&mut self, name: &str) {
        if self.loading {
            self.pending_focus = Some(name.to_string());
        }
        if let Some(row) = self.visible.iter().position(|(i, _)| self.label(&self.entries[*i]) == name) {
            self.cursor = row;
            self.clamp();
        }
    }

    pub fn focus_path(&mut self, path: &Path) {
        if self.loading {
            let name = match path.strip_prefix(&self.cwd) {
                Ok(rel) if self.flat.is_some() || self.tree => Some(strata_core::util::posix(rel)),
                _ if self.flat.is_some() || self.tree => Some(super::short_path(path)),
                _ => path.file_name().map(|n| n.to_string_lossy().into_owned()),
            };
            self.pending_focus = name.or(self.pending_focus.take());
        }
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

    /// Keeps the cursor on screen in a grid of `rows` rows of `cols` tiles;
    /// `offset` is then the first tile shown, always at the start of a row.
    pub fn scroll_grid_into_view(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        self.grid_cols = cols;
        self.page = cols * rows;
        let (cursor_row, mut top) = (self.cursor / cols, self.offset / cols);
        if cursor_row < top {
            top = cursor_row;
        } else if cursor_row >= top + rows {
            top = cursor_row + 1 - rows;
        }
        let last_top = self.visible.len().div_ceil(cols).saturating_sub(rows);
        self.offset = top.min(last_top) * cols;
    }

    /// Keeps the cursor on screen for a list of `height` rows.
    pub fn scroll_into_view(&mut self, height: usize) {
        self.grid_cols = 1;
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

    /// Marks the shown entries matching a `:select` pattern: a glob such as
    /// `*.{jpg,png}`, or `re:<regex>` (see [`Matcher`]). Returns the count.
    pub fn mark_matching(&mut self, pattern: &str) -> Result<usize, String> {
        let matcher = Matcher::parse_selection(pattern)?;
        let entries: Vec<&Entry> = (0..self.len()).filter_map(|r| self.entry_at(r)).collect();
        let labels: Vec<Cow<str>> = entries.iter().map(|e| self.label(e)).collect();
        let found = matcher.filter(&mut Fuzzy::default(), labels.iter().map(|l| l.as_ref()));
        let paths: Vec<PathBuf> = found.into_iter().map(|m| entries[m.index].path.clone()).collect();
        let n = paths.len();
        self.marked.extend(paths);
        Ok(n)
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

    /// Marked items (or the hovered one) in the order they are listed.
    pub fn targets_in_order(&self) -> Vec<Entry> {
        (0..self.len())
            .filter_map(|r| self.entry_at(r))
            .filter(|e| {
                if self.marked.is_empty() {
                    self.hovered().is_some_and(|h| h.path == e.path)
                } else {
                    self.marked.contains(&e.path)
                }
            })
            .cloned()
            .collect()
    }

    /// Marks the listed entries among `paths`. Returns how many.
    pub fn mark_paths(&mut self, paths: &HashSet<PathBuf>) -> usize {
        let found: Vec<PathBuf> =
            self.entries.iter().filter(|e| paths.contains(&e.path)).map(|e| e.path.clone()).collect();
        let n = found.len();
        self.marked.extend(found);
        n
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

    /// Every entry in the listing, hidden and filtered-out ones included.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Orders a flat listing by `cmp` instead of the sort key (for plugin
    /// sort orders), still putting folders first and reversing as the
    /// sort options say. A tree keeps its order.
    pub fn sort_with(&mut self, mut cmp: impl FnMut(&Entry, &Entry) -> std::cmp::Ordering) {
        if !self.depths.is_empty() {
            return;
        }
        let keep = self.hovered().map(|e| e.path.clone());
        let sort = self.sort;
        self.entries.sort_by(|a, b| {
            if sort.dirs_first && a.is_dir() != b.is_dir() {
                return b.is_dir().cmp(&a.is_dir());
            }
            let ord = cmp(a, b);
            if sort.reverse {
                ord.reverse()
            } else {
                ord
            }
        });
        self.refilter();
        if let Some(path) = keep {
            self.focus_path(&path);
        }
    }
}

/// Whether a local folder can be opened, without listing it. `None` when
/// the check takes longer than [`LOCAL_WAIT`] (a slow or stale mount): the
/// listing then reports any error when it arrives.
fn can_open_dir(path: &Path) -> Option<Result<(), String>> {
    let (tx, rx) = mpsc::channel();
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let result = std::fs::read_dir(&path).map(drop).map_err(|e| format!("read {}: {e}", path.display()));
        let _ = tx.send(result);
    });
    rx.recv_timeout(LOCAL_WAIT).ok()
}

/// Lists `root` and, depth first, every expanded folder below it. Folders
/// that cannot be read are shown collapsed. Returns the entries in tree
/// order, their depths and whether the listing stopped at [`TREE_LIMIT`].
/// Each folder is sorted as it is read, so a cut-off folder shows its
/// first items, and the items after it at the levels above stay listed.
fn build_tree(
    vfs: &dyn Vfs,
    root: &Path,
    expanded: &HashSet<PathBuf>,
    sort: SortOptions,
) -> (Result<Vec<Entry>, String>, Vec<u16>, bool) {
    let sorted = |mut entries: Vec<Entry>| {
        sort_entries(&mut entries, sort);
        entries
    };
    let top = match vfs.read_dir(root) {
        Ok(top) => sorted(top),
        Err(e) => return (Err(format!("{e:#}")), Vec::new(), false),
    };
    let (mut entries, mut depths) = (Vec::new(), Vec::new());
    let mut stack: Vec<(Entry, u16)> = top.into_iter().rev().map(|e| (e, 0)).collect();
    while let Some((entry, depth)) = stack.pop() {
        if entries.len() >= TREE_LIMIT {
            // Skip the rest of this folder, but not what follows it.
            stack.push((entry, depth));
            while let Some((entry, d)) = stack.pop() {
                if d < depth {
                    entries.push(entry);
                    depths.push(d);
                }
            }
            return (Ok(entries), depths, true);
        }
        // Only folders the user opened are read, one level per expansion,
        // so links that loop back cannot recurse on their own.
        let open = entry.is_dir() && expanded.contains(&entry.path);
        let path = entry.path.clone();
        entries.push(entry);
        depths.push(depth);
        if open {
            if let Ok(children) = vfs.read_dir(&path) {
                stack.extend(sorted(children).into_iter().rev().map(|c| (c, depth + 1)));
            }
        }
    }
    (Ok(entries), depths, false)
}

/// Sorts a tree listing within each folder, keeping children under their
/// parent.
fn sort_tree(items: Vec<(Entry, u16)>, sort: SortOptions) -> Vec<(Entry, u16)> {
    let Some(base) = items.first().map(|(_, d)| *d) else {
        return items;
    };
    // Each group is one sibling followed by everything nested in it.
    let mut groups: Vec<Vec<(Entry, u16)>> = Vec::new();
    for item in items {
        match groups.last_mut() {
            Some(group) if item.1 > base => group.push(item),
            _ => groups.push(vec![item]),
        }
    }
    let mut heads: Vec<Entry> = groups.iter().map(|g| g[0].0.clone()).collect();
    sort_entries(&mut heads, sort);
    let order: HashMap<PathBuf, usize> = heads.into_iter().enumerate().map(|(i, e)| (e.path, i)).collect();
    groups.sort_by_key(|g| order.get(&g[0].0.path).copied().unwrap_or(usize::MAX));
    let mut out = Vec::new();
    for mut group in groups {
        let children = group.split_off(1);
        out.extend(group);
        out.extend(sort_tree(children, sort));
    }
    out
}

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
        assert!(strata_core::util::glob_match("*.rs", "main.RS"));
        assert!(strata_core::util::glob_match("a?c", "abc"));
        assert!(!strata_core::util::glob_match("*.rs", "main.rsx"));
    }

    #[test]
    fn photo_folders_open_as_a_grid() {
        let dir = tempfile::tempdir().unwrap();
        let photos = dir.path().join("photos");
        std::fs::create_dir(&photos).unwrap();
        for name in ["a.jpg", "b.png", "c.mp4", "notes.txt"] {
            std::fs::write(photos.join(name), "").unwrap();
        }
        let mut p = panel_in(dir.path()).with_auto_grid(true);
        assert!(!p.is_grid(), "a folder of folders stays a list");
        assert!(p.cd(photos.clone()));
        assert!(p.is_grid());

        p.set_grid(false);
        assert!(!p.is_grid(), "toggling wins over the guess");
        assert!(p.parent() && p.cd(photos.clone()));
        assert!(p.is_grid(), "the choice is forgotten on leaving");

        p.set_tree(true);
        assert!(!p.is_grid(), "the tree replaces the grid");
        p.set_grid(true);
        assert!(p.is_grid() && !p.tree);

        let mut manual = panel_in(dir.path());
        manual.set_grid(true);
        assert!(manual.cd(photos) && manual.is_grid(), "without auto_grid the choice stays");
        assert!(manual.parent() && manual.is_grid());
    }

    #[test]
    fn grids_scroll_by_whole_rows() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..23 {
            std::fs::write(dir.path().join(format!("{i:02}.jpg")), "").unwrap();
        }
        let mut p = panel_in(dir.path()).with_auto_grid(true);
        assert!(p.is_grid());
        p.scroll_grid_into_view(4, 2);
        assert_eq!((p.offset, p.page, p.row_step()), (0, 8, 4));
        p.move_by(p.row_step() as isize * 2);
        p.scroll_grid_into_view(4, 2);
        assert_eq!((p.cursor, p.offset), (8, 4), "the row with the cursor comes into view");
        p.move_to(usize::MAX);
        p.scroll_grid_into_view(4, 2);
        assert_eq!((p.cursor, p.offset), (22, 16), "the last two rows fill the grid");
        p.move_to(1);
        p.scroll_grid_into_view(4, 2);
        assert_eq!(p.offset, 0);
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
        p.set_filter("");
        assert_eq!(p.hovered().unwrap().name, "b.txt", "clearing the filter keeps the cursor");
        assert!(p.go_back());
    }

    #[test]
    fn filters_take_globs_and_regexes_and_survive_bad_ones() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["a.jpg", "b.png", "c.txt", "v1.md", "v22.md"] {
            std::fs::write(dir.path().join(n), "").unwrap();
        }
        let mut p = panel_in(dir.path());
        let names = |p: &Panel| (0..p.len()).map(|r| p.entry_at(r).unwrap().name.clone()).collect::<Vec<_>>();
        p.set_filter("*.{jpg,png}");
        assert_eq!(names(&p), ["a.jpg", "b.png"]);
        p.set_filter(r"re:^v\d{2}");
        assert_eq!(names(&p), ["v22.md"]);
        assert_eq!(p.visible[0].1, [0, 1, 2], "the regex match is highlighted");
        p.set_filter(r"re:^v\d{2}(");
        assert!(p.filter_error.as_deref().is_some_and(|e| e.starts_with("bad regex")));
        assert_eq!(names(&p), ["v22.md"], "a bad regex keeps the last good matches");
        p.set_filter("");
        assert!(p.filter_error.is_none());
        p.set_filter("re:(");
        assert_eq!(p.len(), 5, "nothing valid typed yet: everything stays listed");

        p.set_filter("");
        assert_eq!(p.mark_matching("*.md"), Ok(2));
        assert_eq!(p.mark_matching("re:^[ab]\\."), Ok(2));
        assert_eq!(p.mark_matching("c.txt"), Ok(1));
        assert_eq!(p.marked.len(), 5);
        assert!(p.mark_matching("re:(").is_err());
    }

    #[test]
    fn changing_folder_is_recorded_for_the_jump_list() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut p = panel_in(dir.path());
        assert!(p.take_visits().is_empty(), "opening a panel is not a visit");
        p.cd(dir.path().join("sub"));
        p.cd(dir.path().join("sub"));
        p.parent();
        p.go_back();
        assert!(!p.cd(dir.path().join("missing")));
        let sub = dir.path().join("sub");
        assert_eq!(p.take_visits(), [sub.clone(), dir.path().to_path_buf(), sub]);
        assert!(p.take_visits().is_empty());
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

        let fixed = vec![dir.path().join("top.txt"), dir.path().join("missing")];
        p.set_flat(Some(Flat {
            title: "x".into(),
            source: FlatSource::Paths(fixed),
            truncated: false,
            announced: false,
        }));
        assert_eq!(p.len(), 1, "paths that do not exist are left out");
    }

    fn rows(p: &Panel) -> Vec<(u16, String)> {
        (0..p.len()).map(|r| (p.depth_at(r), p.entry_at(r).unwrap().name.clone())).collect()
    }

    #[test]
    fn tree_view_expands_folders_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("b/inner")).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join("b/z.txt"), "").unwrap();
        std::fs::write(root.join("b/a.txt"), "").unwrap();
        std::fs::write(root.join("b/.secret"), "").unwrap();
        std::fs::write(root.join("b/inner/deep.txt"), "").unwrap();
        std::fs::write(root.join("a.txt"), "").unwrap();
        let mut p = panel_in(root);
        p.set_tree(true);
        assert_eq!(rows(&p), [(0, "b".into()), (0, "a.txt".into())]);

        assert!(p.toggle_expand(), "b is hovered");
        assert_eq!(
            rows(&p),
            [(0, "b".into()), (1, "inner".into()), (1, "a.txt".into()), (1, "z.txt".into()), (0, "a.txt".into())],
            "children are sorted, folders first, under their parent; hidden ones are left out"
        );
        p.move_by(1);
        assert!(p.toggle_expand(), "inner");
        assert_eq!(p.len(), 6);
        assert_eq!(p.hovered().unwrap().name, "inner", "the cursor stays put");
        assert_eq!(p.target_dir(), root.join("b/inner"), "new items go inside an expanded folder");
        p.move_by(1);
        assert_eq!((p.depth_at(p.cursor), p.hovered().unwrap().name.as_str()), (2, "deep.txt"));
        assert_eq!(p.target_dir(), root.join("b/inner"), "or next to the hovered item");

        p.sort.reverse = true;
        p.resort();
        assert_eq!(p.hovered().unwrap().name, "deep.txt", "resorting keeps the cursor");
        assert_eq!(rows(&p)[0], (0, "b".into()));
        assert_eq!(rows(&p)[3], (1, "z.txt".into()), "files reversed below their folder");
        p.sort.reverse = false;
        p.resort();

        // Hidden entries and their contents appear only with hidden files shown.
        p.show_hidden = true;
        p.expanded.insert(root.join(".git"));
        p.reload();
        assert!(rows(&p).contains(&(1, ".secret".into())));
        p.show_hidden = false;
        p.refilter();
        assert!(!rows(&p).iter().any(|(_, n)| n.starts_with('.')));

        // h: leave the file for its folder, then collapse it.
        p.focus_path(&root.join("b/inner/deep.txt"));
        assert!(p.collapse_or_up());
        assert_eq!(p.hovered().unwrap().name, "inner");
        assert!(p.collapse_or_up());
        assert_eq!(p.len(), 5);
        p.focus_path(&root.join("a.txt"));
        assert!(!p.collapse_or_up(), "top-level items leave it to the caller");

        // Filtering matches relative paths, and leaving the tree keeps the top-level folder.
        p.expanded.insert(root.join("b/inner"));
        p.reload();
        p.set_filter("deep");
        assert_eq!(p.label(p.hovered().unwrap()), "b/inner/deep.txt");
        p.set_filter("");
        p.focus_path(&root.join("b/inner/deep.txt"));
        p.collapse_all();
        assert_eq!(rows(&p), [(0, "b".into()), (0, "a.txt".into())]);
        assert_eq!(p.hovered().unwrap().name, "b");
        p.toggle_expand();
        p.move_by(2);
        p.set_tree(false);
        assert_eq!(p.hovered().unwrap().name, "b", "back in the list, on the folder that held the item");
        assert!(p.expanded.is_empty());
    }

    #[test]
    fn tree_view_survives_cd_and_vanishing_folders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub/x")).unwrap();
        let mut p = panel_in(dir.path());
        p.set_tree(true);
        p.toggle_expand();
        assert_eq!(p.len(), 2);
        std::fs::remove_dir_all(dir.path().join("sub")).unwrap();
        p.reload();
        assert_eq!(p.len(), 0);
        assert!(p.expanded.is_empty(), "gone folders are forgotten");
        std::fs::create_dir_all(dir.path().join("sub/x")).unwrap();
        p.reload();
        p.toggle_expand();
        assert!(p.cd(dir.path().join("sub")));
        assert!(p.tree && p.expanded.is_empty(), "a new root starts collapsed");
        assert_eq!(rows(&p), [(0, "x".into())]);
    }

    #[test]
    fn gitignored_items_hide_in_lists_trees_and_flat_listings() {
        if !strata_core::nas::which("git") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::process::Command::new("git").arg("-C").arg(root).args(["init", "-q"]).output().unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        for f in ["target/debug/out", "src/main.rs", "src/build.log", "notes.log", "readme.md"] {
            std::fs::write(root.join(f), "").unwrap();
        }
        let names = |p: &Panel| (0..p.len()).map(|r| p.label(p.entry_at(r).unwrap()).into_owned()).collect::<Vec<_>>();
        let mut p = panel_in(root).with_gitignored_hidden(true);
        assert_eq!(names(&p), ["src", "readme.md"]);
        p.set_hide_gitignored(false);
        assert_eq!(names(&p), ["src", "target", "notes.log", "readme.md"]);

        p.set_hide_gitignored(true);
        p.set_tree(true);
        p.toggle_expand();
        assert_eq!(names(&p), ["src", "src/main.rs", "readme.md"], "ignored children of expanded folders too");

        p.set_tree(false);
        p.set_flat(Some(Flat {
            title: "flatten".into(),
            source: FlatSource::Walk(Query::files()),
            truncated: false,
            announced: false,
        }));
        assert!(!names(&p).iter().any(|n| n.ends_with(".log") || n.starts_with("target")), "{:?}", names(&p));
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

    #[test]
    fn remote_trees_are_built_in_the_background() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/inner.txt"), "").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let vfs: VfsRef = Arc::new(Remote(LocalVfs));
        let mut p = Panel::new(vfs, dir.path().to_path_buf(), SortOptions::default(), false, Some(tx));
        assert!(p.apply_listing(rx.recv().unwrap()));
        p.set_tree(true);
        assert!(p.apply_listing(rx.recv().unwrap()));
        p.toggle_expand();
        assert!(p.loading);
        assert!(p.apply_listing(rx.recv().unwrap()));
        assert_eq!(rows(&p), [(0, "sub".into()), (1, "inner.txt".into())]);
    }

    /// A local folder that takes a while to list (a slow disk or mount).
    #[derive(Debug)]
    struct Slow(LocalVfs);

    impl strata_core::Vfs for Slow {
        fn scheme(&self) -> &'static str {
            "local"
        }
        fn label(&self) -> String {
            "local".into()
        }
        fn is_local(&self) -> bool {
            true
        }
        fn home(&self) -> PathBuf {
            self.0.home()
        }
        fn read_dir(&self, path: &Path) -> anyhow::Result<Vec<Entry>> {
            std::thread::sleep(LOCAL_WAIT * 3);
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
    fn slow_local_listings_do_not_block_and_keep_the_requested_focus() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let vfs: VfsRef = Arc::new(Slow(LocalVfs));
        let started = std::time::Instant::now();
        let mut p = Panel::new(vfs, dir.path().to_path_buf(), SortOptions::default(), false, None);
        assert!(started.elapsed() < LOCAL_WAIT * 2, "the UI only waits briefly");
        assert!(p.loading);
        assert_eq!(p.len(), 0);

        p.focus_path(&dir.path().join("b.txt"));
        while !p.poll_listing() {
            assert!(started.elapsed() < Duration::from_secs(5), "the listing never arrived");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!p.loading);
        assert_eq!(p.len(), 3);
        assert_eq!(p.hovered().unwrap().name, "b.txt");
    }
}
