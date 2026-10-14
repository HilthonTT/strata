//! Mouse input: the wheel, clicks and double-clicks, the right-click menu,
//! drag and drop between panels, and clicks on tabs and path segments.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use strata_config::Action;
use strata_core::ops::{Conflict, Transfer, TransferMode};
use strata_core::vfs::ArchiveFormat;
use strata_core::Entry;
use strata_plugin::Level;
use unicode_width::UnicodeWidthChar;

use super::overlay::{Overlay, PickerPurpose, PickerState};
use super::{App, Focus, View};

/// A clickable stretch of the top bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopHit {
    /// First column and the column after the last.
    pub x: (u16, u16),
    pub target: TopTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopTarget {
    View(View),
    Tab(usize),
}

/// Where a panel prints its location in its title.
#[derive(Debug, Clone)]
pub struct Crumb {
    pub x: u16,
    /// The location as shown, perhaps cut short on the left.
    pub text: String,
}

/// Items picked up with the left button, dropped where it is released.
pub struct Drag {
    panel: usize,
    sources: Vec<PathBuf>,
    from: (u16, u16),
    at: (u16, u16),
    /// Moved far enough from where it started to count as a drag.
    active: bool,
    /// Alt or ctrl held on the last report: move instead of copy.
    moving: bool,
}

/// A row of the right-click menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Action(Action),
    /// A command-palette line, e.g. `extract`.
    Command(&'static str),
}

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// A second click on the same row soon after the first. A column or two
/// of slack allows for a slightly shaky hand.
fn is_double(last: Option<(Instant, u16, u16)>, now: Instant, x: u16, y: u16) -> bool {
    last.is_some_and(|(t, lx, ly)| ly == y && lx.abs_diff(x) <= 2 && now.duration_since(t) < DOUBLE_CLICK)
}

/// How many folders up a click at display column `col` of a location such
/// as `~/src/strata` leads: the clicked segment's ancestor level. A click
/// on a `/` belongs to the segment before it, a leading `/` to the root.
/// `None` outside the path.
pub fn crumb_levels(text: &str, col: usize) -> Option<usize> {
    // `· flat`, `· tree …` and the like follow the path.
    let path = text.split(" · ").next().unwrap_or(text);
    let mut used = 0;
    let index = path.char_indices().find_map(|(i, c)| {
        let w = c.width().unwrap_or(0);
        let hit = col >= used && col < used + w.max(1);
        used += w;
        hit.then_some(i)
    })?;
    Some(path[index..].matches(['/', '\\']).count())
}

/// Where items dropped on a panel go: into the folder under the pointer,
/// unless it is one of them, else the panel's own folder.
fn drop_dir(sources: &[PathBuf], over: Option<&Entry>, cwd: &Path) -> PathBuf {
    match over {
        Some(e) if e.is_dir() && !sources.contains(&e.path) => e.path.clone(),
        _ => cwd.to_path_buf(),
    }
}

/// Why dropping `sources` into `dest` on the same filesystem does nothing
/// useful, if it does not.
fn drop_refusal(sources: &[PathBuf], dest: &Path) -> Option<&'static str> {
    if sources.iter().any(|s| dest.starts_with(s)) {
        return Some("can't drop a folder into itself");
    }
    if sources.iter().all(|s| s.parent() == Some(dest)) {
        return Some("already in this folder");
    }
    None
}

impl App {
    pub(super) fn on_mouse(&mut self, mouse: MouseEvent) {
        if self.overlay.is_some() {
            self.drag = None;
            return self.on_overlay_mouse(mouse);
        }
        let (x, y) = (mouse.column, mouse.row);
        let inside = |r: &Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;

        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => return self.on_drag(x, y, mouse.modifiers),
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(drag) = self.drag.take().filter(|d| d.active) {
                    let moving = drag.moving || mouse.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL);
                    self.drop_items(drag, x, y, moving);
                }
                return;
            }
            _ => {}
        }
        if y == 0 && self.on_top_bar(mouse) {
            return;
        }

        if self.view == View::Files {
            if let Some(i) = self.layout.panels.iter().position(inside) {
                return self.on_panel_mouse(i, mouse);
            }
            if self.layout.preview.is_some_and(|r| inside(&r)) {
                match mouse.kind {
                    MouseEventKind::ScrollDown => self.scroll_preview(3),
                    MouseEventKind::ScrollUp => self.scroll_preview(-3),
                    _ => {}
                }
                return;
            }
            if let Some(rect) = self.layout.sidebar.filter(|r| inside(r)) {
                match mouse.kind {
                    MouseEventKind::ScrollDown => self.sidebar.move_by(1),
                    MouseEventKind::ScrollUp => self.sidebar.move_by(-1),
                    MouseEventKind::Down(MouseButton::Left) => {
                        // Below the border, the title line and a blank line.
                        let top = rect.y + 3;
                        let row = y.saturating_sub(top) as usize + self.sidebar.offset;
                        if y >= top && self.sidebar.items.get(row).is_some_and(|i| i.selectable()) {
                            self.sidebar.cursor = row;
                            self.open_sidebar_item();
                        }
                    }
                    _ => {}
                }
            }
            return;
        }
        if let Some(rect) = self.layout.list.filter(|r| inside(r)) {
            let delta = match mouse.kind {
                MouseEventKind::ScrollDown => 1,
                MouseEventKind::ScrollUp => -1,
                MouseEventKind::Down(MouseButton::Left) => {
                    // Row 0 is the border, row 1 the column header.
                    let row = y.saturating_sub(rect.y + 2) as usize + self.layout.list_offset;
                    if y >= rect.y + 2 && row < self.view_len() {
                        self.set_view_cursor(row);
                        if self.is_double_click(x, y) {
                            self.dispatch(Action::Open);
                        }
                    }
                    0
                }
                _ => 0,
            };
            if delta != 0 {
                self.move_view_cursor(delta);
            }
        }
    }

    fn is_double_click(&mut self, x: u16, y: u16) -> bool {
        let now = Instant::now();
        let double = is_double(self.last_click, now, x, y);
        self.last_click = if double { None } else { Some((now, x, y)) };
        double
    }

    /// Views and tabs in the top bar: a click switches, a middle click
    /// closes a tab and the wheel cycles tabs. True when handled.
    fn on_top_bar(&mut self, mouse: MouseEvent) -> bool {
        let x = mouse.column;
        let hit = self.layout.top.iter().find(|h| x >= h.x.0 && x < h.x.1).map(|h| h.target);
        match (mouse.kind, hit) {
            (MouseEventKind::Down(MouseButton::Left), Some(TopTarget::View(view))) => self.set_view(view),
            (MouseEventKind::Down(MouseButton::Left), Some(TopTarget::Tab(i))) => {
                self.set_view(View::Files);
                self.switch_tab(i, false);
            }
            (MouseEventKind::Down(MouseButton::Middle), Some(TopTarget::Tab(i))) => {
                self.switch_tab(i, false);
                self.close_tab();
            }
            (MouseEventKind::ScrollDown, _) if self.tabs.len() > 1 => self.cycle_tab(1),
            (MouseEventKind::ScrollUp, _) if self.tabs.len() > 1 => self.cycle_tab(-1),
            _ => return false,
        }
        true
    }

    fn on_panel_mouse(&mut self, i: usize, mouse: MouseEvent) {
        let (x, y) = (mouse.column, mouse.row);
        let rect = self.layout.panels[i];
        // Row 0 is the border, row 1 the search line.
        let row = (y >= rect.y + 2).then(|| (y - rect.y - 2) as usize + self.panels[i].offset);
        let row = row.filter(|&r| r < self.panels[i].len());
        match mouse.kind {
            MouseEventKind::ScrollDown => self.panels[i].move_by(3),
            MouseEventKind::ScrollUp => self.panels[i].move_by(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                self.active = i;
                self.focus = Focus::Panels;
                if y == rect.y {
                    return self.click_crumb(i, x);
                }
                let Some(row) = row else {
                    return;
                };
                self.panels[i].move_to(row);
                if self.is_double_click(x, y) {
                    return self.dispatch(Action::Open);
                }
                let panel = &self.panels[i];
                let Some(entry) = panel.hovered() else {
                    return;
                };
                let sources = if panel.is_marked(entry) { panel.targets() } else { vec![entry.path.clone()] };
                self.drag = Some(Drag { panel: i, sources, from: (x, y), at: (x, y), active: false, moving: false });
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.active = i;
                self.focus = Focus::Panels;
                let on_entry = row.is_some();
                if let Some(row) = row {
                    self.panels[i].move_to(row);
                    // Right-clicking outside the marked items acts on the
                    // clicked one alone, as in graphical file managers.
                    let panel = &self.panels[i];
                    if panel.hovered().is_some_and(|e| !panel.is_marked(e)) {
                        self.panels[i].clear_marks();
                    }
                }
                self.open_context_menu(x, y, on_entry);
            }
            _ => {}
        }
    }

    /// A click on a segment of a panel's location opens that folder.
    fn click_crumb(&mut self, i: usize, x: u16) {
        let Some(crumb) = self.layout.crumbs.get(i) else {
            return;
        };
        let Some(levels) = x.checked_sub(crumb.x).and_then(|col| crumb_levels(&crumb.text, col as usize)) else {
            return;
        };
        if levels == 0 {
            return;
        }
        if let Some(dir) = self.panels[i].cwd.ancestors().nth(levels).map(Path::to_path_buf) {
            self.cd(dir);
        }
    }

    fn on_drag(&mut self, x: u16, y: u16, mods: KeyModifiers) {
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        drag.at = (x, y);
        drag.moving = mods.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL);
        drag.active |= drag.from.1 != y || drag.from.0.abs_diff(x) > 2;
    }

    /// The panel and folder under the pointer, for a drop.
    fn drop_target(&self, sources: &[PathBuf], x: u16, y: u16) -> Option<(usize, PathBuf)> {
        let inside = |r: &Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        let j = self.layout.panels.iter().position(inside)?;
        let (rect, panel) = (self.layout.panels[j], self.panels.get(j)?);
        let over = (y >= rect.y + 2).then(|| panel.entry_at((y - rect.y - 2) as usize + panel.offset)).flatten();
        Some((j, drop_dir(sources, over, &panel.cwd)))
    }

    /// Copies (or moves) dragged items into the folder they were dropped on.
    fn drop_items(&mut self, drag: Drag, x: u16, y: u16, moving: bool) {
        let Some(src) = self.panels.get(drag.panel) else {
            return;
        };
        let Some((j, dest)) = self.drop_target(&drag.sources, x, y) else {
            return self.notify("drop on a panel or a folder in it", Level::Warn);
        };
        let dst = &self.panels[j];
        let same_fs = std::sync::Arc::ptr_eq(&src.vfs, &dst.vfs) || (src.vfs.is_local() && dst.vfs.is_local());
        if same_fs {
            if let Some(why) = drop_refusal(&drag.sources, &dest) {
                return self.notify(why, Level::Warn);
            }
        }
        if dst.vfs.read_only() {
            return self.notify("archives are read-only: drop items somewhere else", Level::Warn);
        }
        if moving && src.vfs.read_only() {
            return self.notify("archives are read-only: copy items out of them instead", Level::Warn);
        }
        let transfer = Transfer {
            mode: if moving { TransferMode::Move } else { TransferMode::Copy },
            src: src.vfs.clone(),
            sources: drag.sources,
            dst: dst.vfs.clone(),
            dest_dir: dest,
            conflict: Conflict::KeepBoth,
            resolutions: Default::default(),
            targets: Default::default(),
            options: Default::default(),
        };
        self.panels[drag.panel].clear_marks();
        self.begin_transfer(transfer);
    }

    /// What the status line shows while items are dragged.
    pub fn drag_hint(&self) -> Option<String> {
        let drag = self.drag.as_ref().filter(|d| d.active)?;
        let what = match drag.sources.as_slice() {
            [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            many => format!("{} items", many.len()),
        };
        let verb = if drag.moving { "move" } else { "copy" };
        Some(match self.drop_target(&drag.sources, drag.at.0, drag.at.1) {
            Some((j, dest)) => {
                format!("{verb} {what} → {}", super::fileops::location(&self.panels[j].vfs, &dest))
            }
            None => format!("{verb} {what} · release on a panel or folder (alt or ctrl moves)"),
        })
    }

    /// The right-click menu for the hovered item, or for the folder when
    /// the click was below the items.
    fn open_context_menu(&mut self, x: u16, y: u16, on_entry: bool) {
        use Action::*;
        let panel = self.panel();
        let writable = !panel.vfs.read_only();
        let mut items: Vec<MenuItem> = Vec::new();
        let mut add = |item: MenuItem, show: bool| {
            if show {
                items.push(item);
            }
        };
        if on_entry {
            let targets = panel.targets();
            let hovered = panel.hovered();
            let file = hovered.is_some_and(|e| !e.is_dir());
            let archive = file
                && panel.vfs.is_local()
                && hovered.is_some_and(|e| {
                    ArchiveFormat::detect(&e.name).is_some() || [".7z", ".rar"].iter().any(|x| e.name.ends_with(x))
                });
            add(MenuItem::Action(Open), true);
            add(MenuItem::Action(OpenWith), file);
            add(MenuItem::Action(Edit), file);
            add(MenuItem::Action(Rename), writable && targets.len() == 1);
            add(MenuItem::Action(RenamePattern), writable && targets.len() > 1);
            add(MenuItem::Action(Copy), true);
            add(MenuItem::Action(Cut), writable);
            add(MenuItem::Action(Paste), writable && self.clipboard.is_some());
            add(MenuItem::Action(Duplicate), writable);
            add(MenuItem::Action(CopyToOther), self.panels.len() > 1);
            add(MenuItem::Action(MoveToOther), writable && self.panels.len() > 1);
            add(MenuItem::Action(Delete), writable);
            add(MenuItem::Action(DeletePermanent), writable);
            add(MenuItem::Action(Chmod), writable);
            add(MenuItem::Action(Checksum), true);
            add(MenuItem::Command("extract"), archive);
            add(MenuItem::Command("compress"), writable && panel.vfs.is_local() && !archive);
            add(MenuItem::Action(TagMenu), panel.vfs.is_local());
            add(MenuItem::Action(CopyPath), true);
        } else {
            add(MenuItem::Action(Paste), writable && self.clipboard.is_some());
            add(MenuItem::Action(NewFile), writable);
            add(MenuItem::Action(NewDir), writable);
            add(MenuItem::Action(CopyCwd), true);
            add(MenuItem::Action(Shell), panel.vfs.is_local());
            add(MenuItem::Action(SortMenu), true);
            add(MenuItem::Action(ToggleHidden), true);
            add(MenuItem::Action(Refresh), true);
        }
        let labels: Vec<(&str, String)> = items.iter().map(|&item| (menu_label(item), self.menu_keys(item))).collect();
        let width = labels.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
        let rows = labels.into_iter().map(|(l, k)| format!("{l:<width$}  {k}").trim_end().to_string()).collect();
        let title = match self.panel().targets().len() {
            n if on_entry && n > 1 => format!("{n} items"),
            _ if on_entry => self.panel().hovered().map(|e| e.name.clone()).unwrap_or_default(),
            _ => "This folder".into(),
        };
        let picker = PickerState::new(title, rows, PickerPurpose::Context { items }).at(x, y);
        self.overlay = Some(Overlay::Picker(picker));
    }

    /// The keys bound to a menu row, shown next to it.
    fn menu_keys(&self, item: MenuItem) -> String {
        match item {
            MenuItem::Action(action) => self.keymap.keys_for(action).unwrap_or_default(),
            MenuItem::Command(_) => String::new(),
        }
    }

    pub(super) fn run_menu_item(&mut self, item: MenuItem) {
        match item {
            MenuItem::Action(action) => self.dispatch(action),
            MenuItem::Command(line) => self.run_command(line),
        }
    }

    /// The wheel scrolls the open popup or picker; a click picks a row or,
    /// outside the box, closes it.
    fn on_overlay_mouse(&mut self, mouse: MouseEvent) {
        let (x, y) = (mouse.column, mouse.row);
        let inside = |r: Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        let menu = matches!(&self.overlay, Some(Overlay::Picker(p)) if p.anchor.is_some());
        let delta: isize = match mouse.kind {
            MouseEventKind::ScrollDown if menu => 1,
            MouseEventKind::ScrollUp if menu => -1,
            MouseEventKind::ScrollDown => 3,
            MouseEventKind::ScrollUp => -3,
            _ => 0,
        };
        let max_scroll = self.layout.popup_max_scroll.get();
        let list = self.layout.popup_list.get();
        let row = list.filter(|&r| inside(r)).map(|r| (y - r.y) as usize + self.layout.popup_offset.get());
        let outside = self.layout.popup.get().is_some_and(|r| !inside(r));
        match self.overlay.as_mut() {
            Some(Overlay::Help { scroll }) | Some(Overlay::Text(super::overlay::TextPopup { scroll, .. })) => {
                if delta != 0 {
                    *scroll = scroll.saturating_add_signed(delta).min(max_scroll);
                }
            }
            Some(Overlay::Picker(picker)) => match mouse.kind {
                _ if delta != 0 => {
                    picker.move_by(delta);
                    self.on_picker_moved();
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(row) = row.filter(|&r| r < picker.matches.len()) {
                        picker.cursor = row;
                        self.on_picker_moved();
                        self.submit_picker();
                    } else if outside {
                        self.cancel_picker();
                    }
                }
                MouseEventKind::Down(MouseButton::Right) if outside => self.cancel_picker(),
                // Menus follow the pointer.
                MouseEventKind::Moved if picker.anchor.is_some() => {
                    if let Some(row) = row.filter(|&r| r < picker.matches.len()) {
                        picker.cursor = row;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// A short name for a menu row.
fn menu_label(item: MenuItem) -> &'static str {
    use Action::*;
    match item {
        MenuItem::Command("extract") => "Extract here",
        MenuItem::Command("compress") => "Compress…",
        MenuItem::Command(other) => other,
        MenuItem::Action(action) => match action {
            Open => "Open",
            OpenWith => "Open with default app",
            Edit => "Edit",
            Rename => "Rename",
            RenamePattern => "Rename with a pattern",
            Copy => "Copy",
            Cut => "Cut",
            Paste => "Paste",
            Duplicate => "Duplicate",
            CopyToOther => "Copy to next panel",
            MoveToOther => "Move to next panel",
            Delete => "Move to trash",
            DeletePermanent => "Delete permanently",
            Chmod => "Permissions…",
            Checksum => "Checksums",
            TagMenu => "Tag…",
            CopyPath => "Copy path",
            NewFile => "New file",
            NewDir => "New folder",
            CopyCwd => "Copy folder path",
            Shell => "Open a shell here",
            SortMenu => "Sort by…",
            ToggleHidden => "Show / hide dotfiles",
            Refresh => "Refresh",
            other => other.description(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strata_core::EntryKind;

    #[test]
    fn double_clicks_need_the_same_row_and_little_time() {
        let t = Instant::now();
        assert!(is_double(Some((t, 10, 5)), t + Duration::from_millis(200), 11, 5));
        assert!(!is_double(Some((t, 10, 5)), t + Duration::from_millis(200), 10, 6), "another row");
        assert!(!is_double(Some((t, 10, 5)), t + Duration::from_millis(200), 40, 5), "another panel");
        assert!(!is_double(Some((t, 10, 5)), t + Duration::from_millis(500), 10, 5), "too slow");
        assert!(!is_double(None, t, 10, 5));
    }

    #[test]
    fn crumbs_lead_to_the_clicked_segment() {
        let text = "~/src/strata";
        assert_eq!(crumb_levels(text, 0), Some(2), "~ is home");
        assert_eq!(crumb_levels(text, 3), Some(1), "src");
        assert_eq!(crumb_levels(text, 5), Some(1), "the / after src");
        assert_eq!(crumb_levels(text, 8), Some(0), "the current folder");
        assert_eq!(crumb_levels(text, 40), None, "past the end");
        assert_eq!(crumb_levels("/usr/lib", 0), Some(2), "the leading / is the root");
        assert_eq!(crumb_levels("nas:/srv/media", 1), Some(2), "the connection name is its root");
        assert_eq!(crumb_levels("…/b/c", 0), Some(2));
        assert_eq!(crumb_levels("~/a · flat", 6), None, "the mode after the path");
    }

    fn entry(path: &str, kind: EntryKind) -> Entry {
        let path = PathBuf::from(path);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        Entry { name, path, kind, size: 0, modified: None, mode: None }
    }

    #[test]
    fn drops_go_into_the_folder_under_the_pointer() {
        let sources = vec![PathBuf::from("/a/x.txt"), PathBuf::from("/a/sub")];
        let cwd = Path::new("/b");
        assert_eq!(drop_dir(&sources, None, cwd), cwd, "empty space");
        assert_eq!(drop_dir(&sources, Some(&entry("/b/f.txt", EntryKind::File)), cwd), cwd, "a file");
        assert_eq!(drop_dir(&sources, Some(&entry("/b/d", EntryKind::Dir)), cwd), Path::new("/b/d"));
        assert_eq!(drop_dir(&sources, Some(&entry("/a/sub", EntryKind::Dir)), cwd), cwd, "one of the dragged");
    }

    #[test]
    fn pointless_drops_are_refused() {
        let sources = vec![PathBuf::from("/a/x.txt"), PathBuf::from("/a/sub")];
        assert_eq!(drop_refusal(&sources, Path::new("/a")), Some("already in this folder"));
        assert_eq!(drop_refusal(&sources, Path::new("/a/sub/deeper")), Some("can't drop a folder into itself"));
        assert_eq!(drop_refusal(&sources, Path::new("/b")), None);
        assert_eq!(drop_refusal(&sources, Path::new("/a/other")), None);
    }
}
