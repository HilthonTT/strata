//! Thumbnails for the grid view, made off the UI thread: images are decoded
//! and shrunk, videos get a representative frame (ffmpegthumbnailer or
//! ffmpeg). They are kept in memory while shown and, for videos and large
//! local images, on disk under the cache directory.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::time::SystemTime;

use image::DynamicImage;
use ratatui::layout::{Rect, Size};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{FilterType, Resize};
use strata_core::{Entry, VfsRef};

use super::preview::is_image;
use super::preview_media::{self, MediaKind};
use crate::event::AppEvent;

/// Longest side of a thumbnail in pixels: enough for the tile it fills,
/// in steps so a slightly resized tile reuses it.
const THUMB_STEP: u32 = 128;
const THUMB_MIN: u32 = 256;
const THUMB_MAX: u32 = 1024;
/// Thumbnails made at the same time.
const WORKERS: usize = 3;
/// Thumbnails kept in memory beyond the ones on screen.
const MAX_CACHED: usize = 600;
/// Larger images are decoded at most once: their thumbnail goes on disk.
const DISK_MIN_BYTES: u64 = 512 * 1024;
const MAX_IMAGE_BYTES: u64 = 40 * 1024 * 1024;

/// What a tile can show a picture of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbKind {
    Image,
    Video,
}

impl ThumbKind {
    pub fn of(entry: &Entry) -> Option<Self> {
        if entry.is_dir() {
            None
        } else if is_image(entry) {
            Some(ThumbKind::Image)
        } else if MediaKind::of(&entry.extension()) == Some(MediaKind::Video) {
            Some(ThumbKind::Video)
        } else {
            None
        }
    }
}

/// A folder is shown as a grid when at least half of its files, and three
/// or more, are images or videos. Folders don't count.
pub fn is_media_dir<'a>(entries: impl Iterator<Item = &'a Entry>) -> bool {
    let (mut files, mut media) = (0usize, 0usize);
    for entry in entries.filter(|e| !e.is_dir()) {
        files += 1;
        media += ThumbKind::of(entry).is_some() as usize;
    }
    media >= 3 && media * 2 >= files
}

/// How a grid fills the room it gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub tile_w: u16,
    pub tile_h: u16,
}

impl Grid {
    /// Tiles at least `tile_width` columns wide (stretched to fill the
    /// width), each a border, a picture about as tall as it is wide and a
    /// name. `cell_aspect` is a cell's height over its width.
    pub fn fit(width: u16, height: u16, tile_width: u16, cell_aspect: f32) -> Self {
        let want = tile_width.clamp(8, 80);
        let cols = (width / want).max(1);
        let tile_w = (width / cols).max(1);
        let picture_h = (f32::from(tile_w.saturating_sub(2)) / cell_aspect.max(0.5)).round() as u16;
        // Rows stretch or shrink a little so they fill the height.
        let rows = (f32::from(height) / f32::from(picture_h + 3)).round().max(1.0) as u16;
        let tile_h = (height / rows).max(4);
        let rows = (height / tile_h).max(1);
        Self { cols: cols as usize, rows: rows as usize, tile_w, tile_h }
    }

    /// The tile `slot` places from the top left of `area`.
    pub fn tile(&self, area: Rect, slot: usize) -> Rect {
        let (col, row) = ((slot % self.cols) as u16, (slot / self.cols) as u16);
        Rect { x: area.x + col * self.tile_w, y: area.y + row * self.tile_h, width: self.tile_w, height: self.tile_h }
    }

    /// Where a tile's picture goes: inside its border, above the name.
    pub fn picture(tile: Rect) -> Rect {
        Rect {
            x: tile.x + 1,
            y: tile.y + 1,
            width: tile.width.saturating_sub(2),
            height: tile.height.saturating_sub(3),
        }
    }

    /// The tile slot under `(x, y)`, counted from the top left of `area`.
    pub fn slot_at(&self, area: Rect, x: u16, y: u16) -> Option<usize> {
        if x < area.x || y < area.y {
            return None;
        }
        let (col, row) = (((x - area.x) / self.tile_w) as usize, ((y - area.y) / self.tile_h) as usize);
        (col < self.cols && row < self.rows).then_some(row * self.cols + col)
    }
}

/// When a file was last seen to change, so a new version gets a new
/// thumbnail.
type Stamp = (Option<SystemTime>, u64);

fn stamp(entry: &Entry) -> Stamp {
    (entry.modified, entry.size)
}

pub enum Slot {
    Pending,
    Ready {
        image: Arc<DynamicImage>,
        /// The longest side it was made for (the image itself may be
        /// smaller).
        px: u32,
        /// Encoded for the terminal at this size.
        protocol: Option<(Size, Box<Protocol>)>,
        /// A new encoding for another size is on its way.
        encoding: bool,
    },
    /// No picture: unsupported, too large, unreadable or no tool.
    Failed,
}

/// A finished thumbnail, sent to the UI thread.
pub struct ThumbDone {
    path: PathBuf,
    stamp: Stamp,
    px: u32,
    picture: Option<(Arc<DynamicImage>, Size, Option<Protocol>)>,
}

struct Request {
    path: PathBuf,
    stamp: Stamp,
    kind: ThumbKind,
    vfs: VfsRef,
    size: Size,
    px: u32,
    /// Already made; only the encoding for `size` is wanted.
    image: Option<Arc<DynamicImage>>,
}

#[derive(Default)]
struct Queue {
    jobs: Vec<Request>,
    workers: usize,
}

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    picker: Picker,
    tx: Sender<AppEvent>,
}

/// Thumbnails of the files shown in grids.
pub struct Thumbs {
    shared: Option<Arc<Shared>>,
    cache: HashMap<PathBuf, (Stamp, Slot)>,
    /// Paths drawn in the frame being rendered.
    wanted: HashSet<PathBuf>,
}

impl Thumbs {
    /// `picker` is `None` when the terminal shows no images; tiles then
    /// show icons.
    pub fn new(picker: Option<Picker>, tx: Sender<AppEvent>) -> Self {
        let shared = picker
            .map(|picker| Arc::new(Shared { queue: Mutex::new(Queue::default()), ready: Condvar::new(), picker, tx }));
        Self { shared, cache: HashMap::new(), wanted: HashSet::new() }
    }

    /// A cell's width and height in pixels.
    fn cell_px(&self) -> (u16, u16) {
        let size = self.shared.as_ref().map(|s| s.picker.font_size());
        size.map_or((10, 20), |s| (s.width.max(1), s.height.max(1)))
    }

    /// A cell's height over its width, for square-ish pictures.
    pub fn cell_aspect(&self) -> f32 {
        let (w, h) = self.cell_px();
        f32::from(h) / f32::from(w)
    }

    /// Starts a frame: tiles drawn from now on ask for their thumbnails.
    pub fn begin_frame(&mut self) {
        self.wanted.clear();
    }

    /// The thumbnail of `entry` for a picture of `size`, asking for it (or
    /// for an encoding at that size) when there is none yet.
    pub fn want(&mut self, vfs: &VfsRef, entry: &Entry, size: Size) {
        let (Some(shared), Some(kind)) = (&self.shared, ThumbKind::of(entry)) else {
            return;
        };
        if size.width < 2 || size.height < 1 {
            return;
        }
        self.wanted.insert(entry.path.clone());
        let stamp = stamp(entry);
        let (cw, ch) = self.cell_px();
        let px = thumb_px(u32::from(size.width) * u32::from(cw), u32::from(size.height) * u32::from(ch));
        let image = match self.cache.get_mut(&entry.path) {
            Some((seen, slot)) if *seen == stamp => match slot {
                Slot::Pending | Slot::Failed => return,
                Slot::Ready { encoding: true, .. } => return,
                // Too small for a tile that grew: made again, larger.
                Slot::Ready { px: made, encoding, .. } if *made < px => {
                    *encoding = true;
                    None
                }
                Slot::Ready { protocol: Some((made, _)), .. } if *made == size => return,
                Slot::Ready { image, encoding, .. } => {
                    *encoding = true;
                    Some(image.clone())
                }
            },
            _ => {
                self.cache.insert(entry.path.clone(), (stamp, Slot::Pending));
                None
            }
        };
        let request = Request { path: entry.path.clone(), stamp, kind, vfs: vfs.clone(), size, px, image };
        let mut queue = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.jobs.push(request);
        if queue.workers < WORKERS {
            queue.workers += 1;
            let shared = shared.clone();
            std::thread::spawn(move || work(&shared));
        }
        shared.ready.notify_one();
    }

    /// Ends a frame: thumbnails no longer on screen are not made, and the
    /// memory cache is trimmed.
    pub fn end_frame(&mut self) {
        let Some(shared) = &self.shared else {
            return;
        };
        let dropped: Vec<PathBuf> = {
            let mut queue = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
            let (keep, drop) = std::mem::take(&mut queue.jobs).into_iter().partition(|r| self.wanted.contains(&r.path));
            queue.jobs = keep;
            drop.into_iter().map(|r: Request| r.path).collect()
        };
        for path in dropped {
            match self.cache.get_mut(&path) {
                Some((_, Slot::Pending)) => {
                    self.cache.remove(&path);
                }
                Some((_, Slot::Ready { encoding, .. })) => *encoding = false,
                _ => {}
            }
        }
        if self.cache.len() > MAX_CACHED + self.wanted.len() {
            let wanted = &self.wanted;
            self.cache.retain(|path, (_, slot)| wanted.contains(path) || matches!(slot, Slot::Pending));
        }
    }

    /// Takes a thumbnail made in the background.
    pub fn finish(&mut self, done: ThumbDone) {
        let Some((seen, slot)) = self.cache.get_mut(&done.path) else {
            return;
        };
        if *seen != done.stamp {
            return;
        }
        *slot = match done.picture {
            Some((image, size, protocol)) => {
                Slot::Ready { image, px: done.px, protocol: protocol.map(|p| (size, Box::new(p))), encoding: false }
            }
            None => Slot::Failed,
        };
    }

    /// What the tile of `entry` can show.
    pub fn get(&self, entry: &Entry) -> Option<&Slot> {
        self.cache.get(&entry.path).filter(|(seen, _)| *seen == stamp(entry)).map(|(_, slot)| slot)
    }
}

/// A worker: makes the most recently asked-for thumbnail first, so the
/// tiles on screen win over ones scrolled past.
fn work(shared: &Shared) {
    loop {
        let request = {
            let mut queue = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(request) = queue.jobs.pop() {
                    break request;
                }
                queue = shared.ready.wait(queue).unwrap_or_else(|e| e.into_inner());
            }
        };
        let image = request.image.clone().or_else(|| make(&request).map(Arc::new));
        let picture = image.map(|image| {
            let protocol = shared
                .picker
                .new_protocol((*image).clone(), request.size, Resize::Scale(Some(FilterType::Triangle)))
                .ok();
            (image, request.size, protocol)
        });
        let done = ThumbDone { path: request.path, stamp: request.stamp, px: request.px, picture };
        if shared.tx.send(AppEvent::Thumb(Box::new(done))).is_err() {
            return;
        }
    }
}

/// Makes a thumbnail, from the disk cache when it has one.
fn make(request: &Request) -> Option<DynamicImage> {
    let local = request.vfs.local_path(&request.path);
    let cached = local.as_deref().filter(|_| request.kind == ThumbKind::Video || request.stamp.1 >= DISK_MIN_BYTES);
    let cache_file = cached.and_then(|path| cache_file(path, request.stamp, request.px));
    if let Some(image) = cache_file.as_deref().and_then(|f| image::open(f).ok()) {
        return Some(image);
    }
    let image = match request.kind {
        ThumbKind::Image => {
            if request.stamp.1 > MAX_IMAGE_BYTES {
                return None;
            }
            let mut bytes = Vec::new();
            request.vfs.reader(&request.path).ok()?.read_to_end(&mut bytes).ok()?;
            image::load_from_memory(&bytes).ok()?.thumbnail(request.px, request.px)
        }
        // The tools need a real file.
        ThumbKind::Video => preview_media::picture(local.as_deref()?, MediaKind::Video, request.px)?,
    };
    if let Some(file) = cache_file {
        // A failed save only costs making it again next time.
        let _ = file.parent().map(std::fs::create_dir_all);
        let _ = image.save(&file);
    }
    Some(image)
}

/// The thumbnail size for a picture of `width` × `height` pixels.
fn thumb_px(width: u32, height: u32) -> u32 {
    width.max(height).div_ceil(THUMB_STEP).saturating_mul(THUMB_STEP).clamp(THUMB_MIN, THUMB_MAX)
}

/// Where the thumbnail of a local file's current version is kept.
fn cache_file(path: &Path, stamp: Stamp, px: u32) -> Option<PathBuf> {
    let mut hasher = DefaultHasher::new();
    (path, stamp, px).hash(&mut hasher);
    Some(dirs::cache_dir()?.join("strata").join("thumbnails").join(format!("{:016x}.png", hasher.finish())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, StartOptions};
    use strata_config::{Action, Config};
    use strata_core::EntryKind;

    fn entry(name: &str, kind: EntryKind) -> Entry {
        Entry { name: name.into(), path: PathBuf::from("/d").join(name), kind, size: 1, modified: None, mode: None }
    }

    #[test]
    fn media_folders_are_mostly_pictures_and_videos() {
        let files = |names: &[&str]| names.iter().map(|n| entry(n, EntryKind::File)).collect::<Vec<_>>();
        assert!(is_media_dir(files(&["a.jpg", "b.png", "c.mp4", "notes.txt"]).iter()));
        assert!(is_media_dir(files(&["a.jpg", "a.xmp", "b.jpg", "b.xmp", "c.jpg", "c.xmp"]).iter()));
        assert!(!is_media_dir(files(&["a.jpg", "b.jpg"]).iter()), "too few to bother");
        assert!(!is_media_dir(files(&["a.jpg", "b.jpg", "c.jpg", "x.rs", "y.rs", "z.rs", "w.rs"]).iter()));
        let mut with_dirs = files(&["a.jpg", "b.jpg", "c.jpg"]);
        with_dirs.extend(["one", "two", "three", "four"].map(|n| entry(n, EntryKind::Dir)));
        assert!(is_media_dir(with_dirs.iter()), "folders don't count");
        assert!(!is_media_dir(["a.jpg", "b.jpg", "c.jpg"].map(|n| entry(n, EntryKind::Dir)).iter()));
    }

    #[test]
    fn grids_fill_the_width_with_square_pictures() {
        let grid = Grid::fit(100, 40, 22, 2.0);
        assert_eq!((grid.cols, grid.tile_w), (4, 25));
        // 23 picture columns are about 12 rows tall, plus border and name:
        // 15 rows, stretched to fill 40 with three rows of tiles.
        assert_eq!((grid.rows, grid.tile_h), (3, 13));
        assert_eq!(Grid::fit(100, 38, 22, 2.0).rows, 3, "two and a half rows round up");
        assert_eq!(Grid::fit(100, 34, 22, 2.0).rows, 2);
        // Narrow panels still get one tile, no taller than the room.
        let narrow = Grid::fit(10, 6, 22, 2.0);
        assert_eq!((narrow.cols, narrow.rows, narrow.tile_w, narrow.tile_h), (1, 1, 10, 6));
        assert_eq!(Grid::fit(10, 2, 22, 2.0).rows, 1, "even with too little room");
    }

    #[test]
    fn tiles_and_clicks_agree() {
        let grid = Grid::fit(100, 40, 22, 2.0);
        let area = Rect { x: 3, y: 2, width: 100, height: 40 };
        assert_eq!(grid.tile(area, 0), Rect { x: 3, y: 2, width: 25, height: 13 });
        assert_eq!(grid.tile(area, 5), Rect { x: 28, y: 15, width: 25, height: 13 });
        assert_eq!(grid.slot_at(area, 3, 2), Some(0));
        assert_eq!(grid.slot_at(area, 30, 20), Some(5));
        assert_eq!(grid.slot_at(area, 102, 40), Some(11));
        assert_eq!(grid.slot_at(area, 2, 2), None);
        assert_eq!(grid.slot_at(area, 3, 41), None, "below the last full row");
        assert_eq!(Grid::picture(grid.tile(area, 0)), Rect { x: 4, y: 3, width: 23, height: 10 });
    }

    #[test]
    fn only_images_and_videos_get_thumbnails() {
        assert_eq!(ThumbKind::of(&entry("a.JPG", EntryKind::File)), Some(ThumbKind::Image));
        assert_eq!(ThumbKind::of(&entry("a.mkv", EntryKind::File)), Some(ThumbKind::Video));
        assert_eq!(ThumbKind::of(&entry("a.mp3", EntryKind::File)), None);
        assert_eq!(ThumbKind::of(&entry("photos.jpg", EntryKind::Dir)), None);
    }

    #[test]
    fn cache_files_change_with_the_file_and_size() {
        let a = cache_file(Path::new("/p/a.mp4"), (None, 10), 256);
        assert_eq!(a, cache_file(Path::new("/p/a.mp4"), (None, 10), 256));
        assert_ne!(a, cache_file(Path::new("/p/a.mp4"), (None, 11), 256));
        assert_ne!(a, cache_file(Path::new("/p/b.mp4"), (None, 10), 256));
        assert_ne!(a, cache_file(Path::new("/p/a.mp4"), (None, 10), 384));
    }

    #[test]
    fn thumbnails_are_as_large_as_their_tile() {
        // 23 × 10 cells of 10 × 20 pixels.
        assert_eq!(thumb_px(230, 200), 256);
        // The same tile on a high-resolution screen.
        assert_eq!(thumb_px(23 * 22, 10 * 44), 512);
        assert_eq!(thumb_px(4000, 100), 1024);
    }

    fn app(dir: &std::path::Path) -> App {
        let options = StartOptions {
            paths: vec![dir.to_path_buf()],
            config_path: dir.join("config.toml"),
            load_plugins: false,
            chooser: false,
            layout: None,
            commands: vec![],
        };
        App::new(Config::default(), options, ratatui_image::picker::Picker::halfblocks()).unwrap()
    }

    #[test]
    fn left_and_right_move_across_a_grid_and_leave_or_open_folders_in_a_list() {
        let dir = tempfile::tempdir().unwrap();
        let photos = dir.path().join("photos");
        std::fs::create_dir(&photos).unwrap();
        for name in ["a.jpg", "b.jpg", "c.jpg", "d.jpg", "e.jpg"] {
            std::fs::write(photos.join(name), "").unwrap();
        }
        let mut app = app(dir.path());
        app.active = 0;
        app.dispatch(Action::Right);
        assert_eq!(app.panels[0].cwd, photos, "in a list, right opens the folder");
        assert!(app.panels[0].is_grid());

        app.panels[0].scroll_grid_into_view(2, 3);
        app.dispatch(Action::Right);
        assert_eq!(app.panels[0].cursor, 1);
        app.dispatch(Action::Down);
        assert_eq!(app.panels[0].cursor, 3, "down moves a row of tiles");
        app.dispatch(Action::Left);
        assert_eq!((app.panels[0].cwd.clone(), app.panels[0].cursor), (photos.clone(), 2));

        app.dispatch(Action::ToggleGrid);
        assert!(!app.panels[0].is_grid());
        app.dispatch(Action::Left);
        assert_eq!(app.panels[0].cwd, dir.path(), "in a list, left goes to the parent");
    }
}
