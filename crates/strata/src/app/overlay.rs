//! Modal overlays: text prompts, pickers, confirmations and text popups.

use std::path::PathBuf;

use strata_core::nas::Connection;
use strata_core::ops::{Clash, Conflict, Transfer};
use strata_core::search::{Fuzzy, Match};
use strata_core::trash::TrashedItem;
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
    /// Password for an SFTP, S3, WebDAV or FTP connection.
    RemotePassword(Connection),
    AddConnectionUrl,
    AddConnectionName(Connection),
    Grep,
    PreviewFind,
    /// Save a password for this connection in the keychain.
    SavePassword(String),
    /// Name of an archive to pack these items into, encrypted or not.
    Compress {
        items: Vec<PathBuf>,
        encrypt: bool,
    },
    /// Password for a new encrypted zip; `first` is set when repeating it.
    CompressPassword {
        items: Vec<PathBuf>,
        name: String,
        first: Option<String>,
    },
    /// Password to open an encrypted archive.
    ArchivePassword {
        file: PathBuf,
        purpose: super::ArchiveUse,
    },
    /// New permissions for these items.
    Chmod {
        vfs: VfsRef,
        paths: Vec<PathBuf>,
    },
    /// A `:find` query for the focused folder.
    Find,
    /// A pattern to rename these items with.
    RenamePattern(super::rename_pattern::PatternTargets),
    /// A commit message for the repository at this root.
    GitCommit(PathBuf),
}

/// New names shown under the pattern-rename prompt as it is typed.
pub struct RenamePreview {
    /// `(old, new)` for the first items.
    pub rows: Vec<(String, String)>,
    /// Why the pattern cannot be applied, if it cannot.
    pub error: Option<String>,
    /// How many items are being renamed.
    pub total: usize,
}

pub struct InputState {
    pub prompt: String,
    pub value: String,
    /// Cursor position in characters.
    pub cursor: usize,
    pub masked: bool,
    pub purpose: InputPurpose,
    /// Live preview drawn under a pattern-rename prompt.
    pub preview: Option<RenamePreview>,
}

impl InputState {
    pub fn new(prompt: impl Into<String>, value: impl Into<String>, purpose: InputPurpose) -> Self {
        let value = value.into();
        Self { prompt: prompt.into(), cursor: value.chars().count(), value, masked: false, purpose, preview: None }
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
    Theme {
        original: String,
    },
    Find {
        vfs: VfsRef,
        root: PathBuf,
    },
    Plugin(usize),
    Sort,
    Grep {
        root: PathBuf,
        hits: Vec<super::grep::GrepHit>,
    },
    Trash {
        items: Vec<TrashedItem>,
    },
    /// The file behind each row of the duplicates picker.
    Dupes {
        paths: Vec<PathBuf>,
    },
    /// The tag menu for these items; `tags[i]` is row `i` (`""` clears).
    Tag {
        paths: Vec<PathBuf>,
        tags: Vec<String>,
    },
    /// Every tag, to list what carries it.
    Tags {
        tags: Vec<String>,
    },
    Drives {
        drives: Vec<strata_sys::drives::Drive>,
    },
    /// Frequently visited folders, best first; `dirs[i]` is row `i`.
    Jump {
        dirs: Vec<PathBuf>,
    },
    /// The git menu; `ops[i]` is row `i`.
    Git {
        target: super::vcs::GitTarget,
        ops: Vec<super::vcs::GitOp>,
    },
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
        // Single steps wrap around; page jumps stop at the ends.
        self.cursor = match delta {
            -1 | 1 => (self.cursor as isize + delta).rem_euclid(len),
            _ => (self.cursor as isize + delta).clamp(0, len - 1),
        } as usize;
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
    /// Store the password that just opened this archive in the keychain.
    SaveArchivePassword {
        file: PathBuf,
        password: String,
    },
    /// Delete items from the trash for good.
    PurgeTrash(Vec<TrashedItem>),
    EmptyTrash,
    Sync(Box<super::sync::SyncRequest>),
    /// Trash one copy of a duplicate, then show the duplicates again.
    TrashDupe(PathBuf),
    /// Throw away unstaged changes to these files.
    GitDiscard {
        root: PathBuf,
        paths: Vec<PathBuf>,
    },
    /// Run an operation refused for lack of permission as root.
    Elevate(strata_core::elevate::Privileged),
    Quit,
}

impl Confirm {
    /// Cannot be undone (or not by strata), so only `y` confirms it, never
    /// an Enter meant for something else.
    pub fn is_destructive(&self) -> bool {
        match self {
            Confirm::Delete { permanent, .. } => *permanent,
            Confirm::DockerRemove { .. }
            | Confirm::PurgeTrash(_)
            | Confirm::EmptyTrash
            | Confirm::Sync(_)
            | Confirm::GitDiscard { .. }
            | Confirm::Elevate(_) => true,
            Confirm::SavePassword { .. }
            | Confirm::SaveArchivePassword { .. }
            | Confirm::TrashDupe(_)
            | Confirm::Quit => false,
        }
    }
}

pub struct ConfirmState {
    pub message: String,
    pub action: Confirm,
}

/// A paste whose destination already has some of the names, asked about
/// one clash at a time.
pub struct ConflictState {
    pub transfer: Transfer,
    pub clashes: Vec<Clash>,
    /// The clash being asked about.
    pub current: usize,
    /// Whether the next answer also settles every clash after it.
    pub apply_all: bool,
}

impl ConflictState {
    pub fn new(transfer: Transfer, clashes: Vec<Clash>) -> Self {
        ConflictState { transfer, clashes, current: 0, apply_all: false }
    }

    /// Records `choice` for the current clash, or with `apply_all` for it
    /// and every one after it. True once every clash has an answer.
    pub fn answer(&mut self, choice: Conflict) -> bool {
        let end = if self.apply_all { self.clashes.len() } else { self.current + 1 };
        for clash in &self.clashes[self.current.min(end)..end] {
            self.transfer.resolutions.per_source.insert(clash.source.path.clone(), choice);
        }
        self.current = end;
        self.current >= self.clashes.len()
    }
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

    #[test]
    fn conflict_answers_one_clash_or_all_remaining() {
        use strata_core::{Entry, EntryKind};
        let entry = |p: &str| Entry {
            name: p.trim_start_matches('/').into(),
            path: p.into(),
            kind: EntryKind::File,
            size: 1,
            modified: None,
            mode: None,
        };
        let vfs: VfsRef = std::sync::Arc::new(strata_core::vfs::LocalVfs);
        let transfer = Transfer {
            mode: strata_core::ops::TransferMode::Copy,
            src: vfs.clone(),
            sources: vec![],
            dst: vfs,
            dest_dir: "/d".into(),
            conflict: Conflict::KeepBoth,
            resolutions: Default::default(),
            targets: Default::default(),
            options: Default::default(),
        };
        let clashes = ["/a", "/b", "/c"].map(|p| Clash { source: entry(p), existing: entry(p) }).to_vec();
        let mut state = ConflictState::new(transfer, clashes);
        assert!(!state.answer(Conflict::Skip));
        assert_eq!(state.current, 1);
        state.apply_all = true;
        assert!(state.answer(Conflict::OverwriteIfNewer), "the rest are settled at once");
        let answers = &state.transfer.resolutions.per_source;
        assert_eq!(answers[&PathBuf::from("/a")], Conflict::Skip);
        assert_eq!(answers[&PathBuf::from("/b")], Conflict::OverwriteIfNewer);
        assert_eq!(answers[&PathBuf::from("/c")], Conflict::OverwriteIfNewer);
    }

    #[test]
    fn pickers_wrap_on_single_steps_and_stop_on_pages() {
        let items = (0..20).map(|i| format!("item {i}")).collect();
        let mut p = PickerState::new("t", items, PickerPurpose::Sort);
        p.move_by(-1);
        assert_eq!(p.cursor, 19, "up from the top wraps");
        p.move_by(1);
        assert_eq!(p.cursor, 0, "down from the bottom wraps");
        p.move_by(15);
        p.move_by(10);
        assert_eq!(p.cursor, 19, "page down stops at the end");
        p.move_by(-30);
        assert_eq!(p.cursor, 0, "page up stops at the start");
    }
}
