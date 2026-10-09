//! One file panel: a directory listing with cursor, marks, filter and history.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use strata_core::search::Fuzzy;
use strata_core::sort::{sort_entries, SortOptions};
use strata_core::{Entry, VfsRef};

pub struct Panel {
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
}

impl Panel {
    pub fn new(vfs: VfsRef, cwd: PathBuf, sort: SortOptions, show_hidden: bool) -> Self {
        let mut panel = Self {
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

    /// Re-reads the directory, keeping the cursor on the same name if possible.
    pub fn reload(&mut self) {
        let keep = self.hovered().map(|e| e.name.clone());
        match self.vfs.read_dir(&self.cwd) {
            Ok(mut entries) => {
                sort_entries(&mut entries, self.sort);
                self.entries = entries;
                self.error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(format!("{e:#}"));
            }
        }
        let names: HashSet<&PathBuf> = self.entries.iter().map(|e| &e.path).collect();
        self.marked.retain(|p| names.contains(p));
        self.refilter();
        if let Some(name) = keep {
            self.focus_name(&name);
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
            let names = candidates.iter().map(|&i| self.entries[i].name.as_str());
            fuzzy.filter(&self.filter, names).into_iter().map(|m| (candidates[m.index], m.positions)).collect()
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
        if let Err(e) = self.vfs.read_dir(&path) {
            self.error = Some(format!("{e:#}"));
            return false;
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
        self.filter.clear();
        self.marked.clear();
        self.visual_anchor = None;
        self.cursor = 0;
        self.offset = 0;
        self.reload();
        // Coming back up from a child: put the cursor on that child.
        if let Some(prev) = previous {
            if prev.parent() == Some(self.cwd.as_path()) {
                if let Some(name) = prev.file_name() {
                    self.focus_name(&name.to_string_lossy());
                }
            }
        }
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

    pub fn focus_name(&mut self, name: &str) {
        if let Some(row) = self.visible.iter().position(|(i, _)| self.entries[*i].name == name) {
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

/// Minimal glob: `*` matches any run, `?` one character; case-insensitive.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => go(&p[1..], &n[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use strata_core::vfs::LocalVfs;

    fn panel_in(dir: &Path) -> Panel {
        Panel::new(Arc::new(LocalVfs), dir.to_path_buf(), SortOptions::default(), false)
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
}
