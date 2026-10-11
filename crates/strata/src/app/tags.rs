//! Tags and colour labels: `,` opens the tag menu for the marked items,
//! `:tag`/`:untag` add and remove tags, `:tags` lists everything tagged.

use std::path::PathBuf;

use strata_core::tags::COLORS;
use strata_plugin::Level;

use super::overlay::{Overlay, PickerPurpose, PickerState};
use super::App;

/// The menu row that removes every tag.
const CLEAR_ROW: &str = "✕ remove all tags";

impl App {
    /// `:tag <name>` tags the marked items; without a name it opens the menu.
    pub(super) fn tag_command(&mut self, args: &str) {
        if args.is_empty() {
            return self.tag_menu();
        }
        let paths = self.tag_targets();
        for tag in args.split([',', ' ']).filter(|t| !t.is_empty()) {
            self.apply_tag(paths.clone(), tag, true);
        }
    }

    /// `:untag [name]` removes one tag, or every tag, from the marked items.
    pub(super) fn untag_command(&mut self, args: &str) {
        let paths = self.tag_targets();
        let tag = (!args.is_empty()).then_some(args);
        self.apply_tag(paths, tag.unwrap_or(""), false);
    }

    /// `:tags` lists tags to browse; `:tags prune` forgets missing items.
    pub(super) fn tags_command(&mut self, args: &str) {
        match args {
            "" => {
                let all = self.tags.all();
                if all.is_empty() {
                    return self.info("nothing is tagged yet: mark items and press , or run :tag <name>");
                }
                let rows = all.iter().map(|(tag, n)| format!("{tag} ({n})")).collect();
                let tags = all.into_keys().collect();
                self.overlay = Some(Overlay::Picker(PickerState::new("Tags", rows, PickerPurpose::Tags { tags })));
            }
            "prune" => match self.tags.prune() {
                Ok(n) => self.info(format!("forgot the tags of {n} missing item(s)")),
                Err(e) => self.error(format!("tags: {e:#}")),
            },
            _ => self.error("usage: tags [prune]"),
        }
    }

    /// Shows every item carrying `tag` in the focused panel.
    pub(super) fn show_tagged(&mut self, tag: &str) {
        let mut paths: Vec<PathBuf> = self.tags.with_all(&[tag.to_string()]).into_iter().collect();
        paths.sort();
        self.show_paths(format!("tag {tag}"), paths);
    }

    fn tag_targets(&mut self) -> Vec<PathBuf> {
        if !self.panel().vfs.is_local() {
            self.notify("only local items can be tagged", Level::Warn);
            return Vec::new();
        }
        self.panel().targets()
    }

    /// A menu of colours and known tags; picking one toggles it on the
    /// marked items. Typing a new name and pressing enter adds that tag.
    pub(super) fn tag_menu(&mut self) {
        let paths = self.tag_targets();
        if paths.is_empty() {
            return;
        }
        let has = |tag: &str| paths.iter().all(|p| self.tags.get(p).iter().any(|t| t == tag));
        let mut tags: Vec<String> = COLORS.iter().map(|c| c.to_string()).collect();
        tags.extend(self.tags.all().into_keys().filter(|t| !COLORS.contains(&t.as_str())));
        let mut rows: Vec<String> = tags.iter().map(|t| format!("{} {t}", if has(t) { "●" } else { "○" })).collect();
        rows.push(CLEAR_ROW.into());
        tags.push(String::new());
        let title = format!("Tag {} item(s) · type a new tag and press enter", paths.len());
        self.overlay = Some(Overlay::Picker(PickerState::new(title, rows, PickerPurpose::Tag { paths, tags })));
    }

    /// Enter in the tag menu: toggles the chosen tag, or adds a new one.
    pub(super) fn tag_menu_chosen(&mut self, paths: Vec<PathBuf>, tag: Option<String>, typed: String) {
        match tag {
            Some(tag) if tag.is_empty() => self.apply_tag(paths, "", false),
            Some(tag) => {
                let all_have = paths.iter().all(|p| self.tags.get(p).contains(&tag));
                self.apply_tag(paths, &tag, !all_have);
            }
            None if !typed.trim().is_empty() => self.apply_tag(paths, typed.trim(), true),
            None => {}
        }
    }

    /// Adds or removes `tag` (all tags when removing `""`).
    pub(super) fn apply_tag(&mut self, paths: Vec<PathBuf>, tag: &str, add: bool) {
        if paths.is_empty() {
            return;
        }
        let result =
            if add { self.tags.add(&paths, tag) } else { self.tags.remove(&paths, (!tag.is_empty()).then_some(tag)) };
        match result {
            Ok(0) => return self.info("nothing changed"),
            Ok(n) => {
                let what = if tag.is_empty() { "all tags".to_string() } else { format!("'{}'", tag.to_lowercase()) };
                let verb = if add { "tagged" } else { "removed" };
                self.info(if add {
                    format!("{verb} {n} item(s) {what}")
                } else {
                    format!("{verb} {what} from {n} item(s)")
                });
            }
            Err(e) => return self.error(format!("tag: {e:#}")),
        }
        self.rebuild_sidebar();
    }

    /// `:select tag:<name>` marks the listed items with that tag.
    pub(super) fn select_tagged(&mut self, tag: &str) -> usize {
        let tagged = self.tags.with_all(&[tag.to_lowercase()]);
        self.panel_mut().mark_paths(&tagged)
    }
}
