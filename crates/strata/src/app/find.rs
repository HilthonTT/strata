//! `:find` and `:flatten`: flat listings of everything below the focused
//! folder that matches a query, shown in the panel itself so every file
//! operation works on the results.

use strata_core::find::{Query, LIMIT};
use strata_plugin::Level;

use super::overlay::{InputPurpose, InputState, Overlay};
use super::panel::{Flat, FlatSource};
use super::App;

pub const FIND_USAGE: &str = "find [glob…] [--size +100M|-1K] [--mtime -7d|+1y] [--type f|d|l] [--ext a,b] \
                              [--depth n] [--empty] [--all]";

impl App {
    /// `:find <query>` lists matches below the focused folder.
    pub(super) fn find_command(&mut self, args: &str) {
        if args.is_empty() {
            return self.find_prompt();
        }
        if !self.panel().vfs.is_local() {
            return self.notify("finding works on local folders", Level::Warn);
        }
        let query = match Query::parse(args) {
            Ok(q) => q,
            Err(e) => return self.error(format!("find: {e:#} — usage: {FIND_USAGE}")),
        };
        self.show_flat(format!("find {args}"), FlatSource::Walk(query));
    }

    pub(super) fn find_prompt(&mut self) {
        let current = match self.panel().flat.as_ref().map(|f| f.title.as_str()) {
            Some(title) => title.strip_prefix("find ").unwrap_or("").to_string(),
            None => String::new(),
        };
        let input = InputState::new("Find (*.log --size +10M --mtime -7d --type f)", current, InputPurpose::Find);
        self.overlay = Some(Overlay::Input(input));
    }

    /// `:flatten` lists every file below the folder; again goes back.
    pub(super) fn toggle_flatten(&mut self) {
        if self.panel().flat.is_some() {
            return self.leave_flat();
        }
        if !self.panel().vfs.is_local() {
            return self.notify("flattening works on local folders", Level::Warn);
        }
        self.show_flat("flatten".into(), FlatSource::Walk(Query::files()));
    }

    fn show_flat(&mut self, title: String, source: FlatSource) {
        self.view = super::View::Files;
        self.focus = super::Focus::Panels;
        self.panel_mut().set_flat(Some(Flat { title, source, truncated: false, announced: false }));
        self.invalidate_preview();
    }

    /// Back from a flat listing to the folder itself.
    pub(super) fn leave_flat(&mut self) {
        let hovered = self.panel().hovered().map(|e| e.path.clone());
        self.panel_mut().set_flat(None);
        // Keep the cursor near what was hovered: on it, or on the folder
        // that contains it.
        let cwd = self.panel().cwd.clone();
        if let Some(rel) = hovered.as_ref().and_then(|h| h.strip_prefix(&cwd).ok()) {
            if let Some(first) = rel.components().next() {
                let name = first.as_os_str().to_string_lossy().into_owned();
                self.panel_mut().focus_name(&name);
            }
        }
        self.invalidate_preview();
    }

    /// Reports a finished flat listing, once per listing.
    pub(super) fn on_flat_listed(&mut self) {
        let Some(flat) = self.panel_mut().flat.as_mut().filter(|f| !f.announced) else { return };
        flat.announced = true;
        let flat = flat.clone();
        let count = self.panel().len();
        if flat.truncated {
            self.notify(format!("{}: showing the first {LIMIT} matches", flat.title), Level::Warn);
        } else if count == 0 {
            self.info(format!("{}: no matches", flat.title));
        }
    }
}
