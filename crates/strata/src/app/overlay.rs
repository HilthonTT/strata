//! Modal overlays: text prompts, pickers, confirmations and text popups.

use std::path::PathBuf;

use strata_core::nas::Connection;
use strata_core::search::{Fuzzy, Match};
use strata_core::VfsRef;

/// What a text prompt is for.
pub enum InputPurpose {
    Rename(PathBuf),
    NewFile,
    NewDir,
    /// Live filter of the focused panel.
    Filter,
    Command,
    Plugin(usize),
    SftpPassword(Connection),
    AddConnectionUrl,
    AddConnectionName(Connection),
    Grep,
    PreviewFind,
    /// Save a password for this connection in the keychain.
    SavePassword(String),
    /// New permissions for these items.
    Chmod {
        vfs: VfsRef,
        paths: Vec<PathBuf>,
    },
}

pub struct InputState {
    pub prompt: String,
    pub value: String,
    /// Cursor position in characters.
    pub cursor: usize,
    pub masked: bool,
    pub purpose: InputPurpose,
}

impl InputState {
    pub fn new(prompt: impl Into<String>, value: impl Into<String>, purpose: InputPurpose) -> Self {
        let value = value.into();
        Self { prompt: prompt.into(), cursor: value.chars().count(), value, masked: false, purpose }
    }

    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    /// Places the cursor before the extension, handy when renaming.
    pub fn cursor_before_extension(mut self) -> Self {
        if let Some((stem, _)) = self.value.rsplit_once('.') {
            if !stem.is_empty() {
                self.cursor = stem.chars().count();
            }
        }
        self
    }

    fn byte_index(&self, char_idx: usize) -> usize {
        self.value.char_indices().nth(char_idx).map(|(i, _)| i).unwrap_or(self.value.len())
    }

    pub fn insert(&mut self, c: char) {
        let i = self.byte_index(self.cursor);
        self.value.insert(i, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let i = self.byte_index(self.cursor);
            self.value.remove(i);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.value.chars().count() {
            let i = self.byte_index(self.cursor);
            self.value.remove(i);
        }
    }

    pub fn delete_word(&mut self) {
        let chars: Vec<char> = self.value.chars().collect();
        let mut start = self.cursor;
        while start > 0 && chars[start - 1] == ' ' {
            start -= 1;
        }
        while start > 0 && chars[start - 1] != ' ' && chars[start - 1] != '/' {
            start -= 1;
        }
        let (a, b) = (self.byte_index(start), self.byte_index(self.cursor));
        self.value.replace_range(a..b, "");
        self.cursor = start;
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.value.chars().count());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.value.chars().count();
    }

    pub fn set(&mut self, value: String) {
        self.value = value;
        self.end();
    }
}

/// What a picker selection does.
pub enum PickerPurpose {
    Theme { original: String },
    Find { vfs: VfsRef, root: PathBuf },
    Plugin(usize),
    Sort,
    Grep { root: PathBuf, hits: Vec<super::grep::GrepHit> },
}

pub struct PickerState {
    pub title: String,
    pub items: Vec<String>,
    pub query: String,
    pub matches: Vec<Match>,
    pub cursor: usize,
    pub offset: usize,
    pub purpose: PickerPurpose,
    fuzzy: Fuzzy,
}

impl PickerState {
    pub fn new(title: impl Into<String>, items: Vec<String>, purpose: PickerPurpose) -> Self {
        let mut p = Self {
            title: title.into(),
            items,
            query: String::new(),
            matches: Vec::new(),
            cursor: 0,
            offset: 0,
            purpose,
            fuzzy: Fuzzy::default(),
        };
        p.refilter();
        p
    }

    pub fn refilter(&mut self) {
        self.matches = self.fuzzy.filter(&self.query, self.items.iter().map(String::as_str));
        self.cursor = 0;
        self.offset = 0;
    }

    /// Index into `items` of the highlighted entry.
    pub fn selected_index(&self) -> Option<usize> {
        self.matches.get(self.cursor).map(|m| m.index)
    }

    pub fn selected(&self) -> Option<&str> {
        self.matches.get(self.cursor).map(|m| self.items[m.index].as_str())
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let len = self.matches.len() as isize;
        self.cursor = (self.cursor as isize + delta).rem_euclid(len) as usize;
    }

    pub fn focus(&mut self, item: &str) {
        if let Some(pos) = self.matches.iter().position(|m| self.items[m.index] == item) {
            self.cursor = pos;
        }
    }
}

/// Something that needs a yes/no first.
pub enum Confirm {
    Delete {
        vfs: VfsRef,
        paths: Vec<PathBuf>,
        permanent: bool,
    },
    DockerRemove {
        id: String,
        name: String,
    },
    /// Store a password that just worked in the keychain.
    SavePassword {
        name: String,
        password: String,
    },
    Quit,
}

pub struct ConfirmState {
    pub message: String,
    pub action: Confirm,
}

/// A paste whose destination already has some of the names.
pub struct ConflictState {
    pub transfer: strata_core::ops::Transfer,
    pub names: Vec<String>,
}

pub struct TextPopup {
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
    /// Colour lines by their first character: `+`/`✓` added or fine,
    /// `-`/`✗` removed or failed, `~`/`?` changed, `@@` hunk headers.
    pub colored: bool,
}

pub enum Overlay {
    Input(InputState),
    Picker(PickerState),
    Confirm(ConfirmState),
    Conflict(ConflictState),
    Help { scroll: usize },
    Text(TextPopup),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_edits_unicode_safely() {
        let mut i = InputState::new("x", "héllo.txt", InputPurpose::NewFile).cursor_before_extension();
        assert_eq!(i.cursor, 5);
        i.insert('!');
        assert_eq!(i.value, "héllo!.txt");
        i.home();
        i.delete();
        assert_eq!(i.value, "éllo!.txt");
        i.end();
        i.delete_word();
        assert_eq!(i.value, "");
    }
}
