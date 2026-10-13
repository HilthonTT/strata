//! Jumping to frequently and recently visited folders: `jump` opens a
//! picker over them, best first, and `:z <words>` goes straight to the best
//! match. Every local folder a panel changes to is remembered (see
//! [`strata_core::frecency`]) in `jumps.json` in the data directory.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use strata_core::frecency::{self, Frecency};
use strata_plugin::Level;

use super::overlay::{Overlay, PickerPurpose, PickerState};
use super::{short_path, App};

/// Most folders listed in the picker.
const PICKER_LIMIT: usize = 500;

#[derive(Default)]
struct State {
    db: Frecency,
    /// Visits not written to the file yet, with their times.
    unsaved: Vec<(PathBuf, u64)>,
    /// The file has been read.
    loaded: bool,
}

/// The visited folders, shared with the thread that reads them at start.
pub struct Jumps {
    state: Arc<Mutex<State>>,
    file: PathBuf,
}

impl Default for Jumps {
    fn default() -> Self {
        Self::load(strata_config::data_dir().join("jumps.json"))
    }
}

impl Jumps {
    /// Starts reading `file` in the background, forgetting folders that no
    /// longer exist (checking them could hang on a stale mount).
    pub fn load(file: PathBuf) -> Self {
        let jumps = Self { state: Arc::default(), file };
        let (state, file) = (jumps.state.clone(), jumps.file.clone());
        std::thread::spawn(move || {
            let mut db = Frecency::load(&file);
            if db.prune(Path::is_dir) > 0 {
                let _ = db.save(&file);
            }
            let mut state = lock(&state);
            // Visits made while the file was being read.
            for (dir, at) in &state.unsaved {
                db.add(dir, *at);
            }
            state.db = db;
            state.loaded = true;
        });
        jumps
    }

    pub fn record(&self, dir: &Path) {
        let now = frecency::now();
        let mut state = lock(&self.state);
        state.db.add(dir, now);
        state.unsaved.push((dir.to_path_buf(), now));
    }

    /// Adds the new visits to the file as it is now, so strata instances
    /// running side by side keep each other's visits.
    pub fn save(&self) -> anyhow::Result<()> {
        let mut state = lock(&self.state);
        if state.unsaved.is_empty() {
            return Ok(());
        }
        let mut db = Frecency::load(&self.file);
        for (dir, at) in &state.unsaved {
            db.add(dir, *at);
        }
        db.save(&self.file)?;
        state.unsaved.clear();
        Ok(())
    }

    fn ranked(&self) -> Vec<PathBuf> {
        lock(&self.state).db.ranked(frecency::now())
    }

    fn best(&self, query: &str, except: Option<&Path>) -> Option<PathBuf> {
        lock(&self.state).db.best(query, frecency::now(), except)
    }

    fn forget(&self, dir: &Path) {
        lock(&self.state).db.remove(dir);
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl App {
    /// Remembers the local folders panels changed to.
    pub(super) fn record_visits(&mut self) {
        for panel in &mut self.panels {
            for dir in panel.take_visits() {
                self.jumps.record(&dir);
            }
        }
    }

    /// Writes new visits to the jump list file (at quit).
    pub fn save_jumps(&self) -> anyhow::Result<()> {
        self.jumps.save()
    }

    /// The local folder of the focused panel, which a jump skips.
    fn here(&self) -> Option<PathBuf> {
        let p = self.panel();
        p.vfs.is_local().then(|| p.cwd.clone())
    }

    /// `jump`: a picker over the visited folders, best first.
    pub(super) fn open_jump_picker(&mut self) {
        let here = self.here();
        let dirs: Vec<PathBuf> =
            self.jumps.ranked().into_iter().filter(|d| Some(d) != here.as_ref()).take(PICKER_LIMIT).collect();
        if dirs.is_empty() {
            return self.info("no folders to jump to yet: the ones you visit are remembered");
        }
        let items = dirs.iter().map(|d| short_path(d)).collect();
        let picker = PickerState::new("Jump to a frequent folder", items, PickerPurpose::Jump { dirs });
        self.overlay = Some(Overlay::Picker(picker));
    }

    /// Goes to a folder picked from the jump list, or forgets it if it is gone.
    pub(super) fn jump_to(&mut self, dir: PathBuf) {
        if dir.is_dir() {
            return self.cd_local(dir);
        }
        self.jumps.forget(&dir);
        self.notify(format!("{} no longer exists", short_path(&dir)), Level::Warn);
    }

    /// `:z <words>`: the best visited folder whose path has the words in
    /// order, the last in its name (like zoxide). An existing folder's path
    /// is entered as is; no words opens the picker.
    pub(super) fn z_command(&mut self, args: &str) {
        if args.is_empty() {
            return self.open_jump_picker();
        }
        if self.here().is_some() && !args.contains(char::is_whitespace) {
            let path = self.resolve(args);
            if path.is_dir() {
                return self.cd(path);
            }
        }
        let here = self.here();
        while let Some(dir) = self.jumps.best(args, here.as_deref()) {
            if dir.is_dir() {
                return self.cd_local(dir);
            }
            self.jumps.forget(&dir);
        }
        self.notify(format!("no visited folder matches '{args}'"), Level::Warn);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_loaded(jumps: &Jumps) {
        let start = std::time::Instant::now();
        while !lock(&jumps.state).loaded {
            assert!(start.elapsed() < std::time::Duration::from_secs(5), "the file was never read");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn visits_are_added_to_the_file_without_losing_other_instances() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("jumps.json");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let mut seed = Frecency::default();
        seed.add(&dir.path().join("gone"), frecency::now());
        seed.save(&file).unwrap();

        let first = Jumps::load(file.clone());
        let second = Jumps::load(file.clone());
        wait_loaded(&first);
        wait_loaded(&second);
        first.record(&a);
        second.record(&b);
        second.record(&b);
        first.save().unwrap();
        second.save().unwrap();
        let saved = Frecency::load(&file);
        assert!(saved.score(&a, frecency::now()) > 0.0, "the first instance's visit survives the second's save");
        assert_eq!(saved.ranked(frecency::now())[0], b);

        // Reading in the background drops folders that are gone.
        let reloaded = Jumps::load(file.clone());
        wait_loaded(&reloaded);
        assert_eq!(reloaded.ranked(), [b.clone(), a.clone()]);
        assert_eq!(reloaded.best("a", None), Some(a));
        reloaded.forget(&b);
        assert_eq!(reloaded.best("b", None), None);
    }
}
