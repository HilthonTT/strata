//! Application state and the main event loop.
//!
//! The behaviour is split by concern into submodules that each add an
//! `impl App` block: key handling, actions, commands, plugins, external
//! programs and the per-view logic.

mod actions;
pub use actions::short_path;
mod commands;
pub use commands::COMMANDS;
mod external;
pub mod grep;
mod highlight;
mod input;
pub mod overlay;
pub mod panel;
mod plugins;
pub mod preview;
pub mod sidebar;
mod tabs;
mod undo;
mod vcs;
mod views;
mod workers;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::layout::{Rect, Size};
use ratatui_image::picker::Picker;
use strata_config::{Config, KeyPress, Keymap, Theme, ThemeRegistry};
use strata_core::jobs::Progress;
use strata_core::jobs::{JobManager, JobState};
use strata_core::ops::TransferMode;
use strata_core::sort::SortOptions;
use strata_core::vfs::LocalVfs;
use strata_core::VfsRef;
use strata_plugin::{Level, PluginHost};
use strata_sys::docker::Container;
use strata_sys::du::UsageReport;

use crate::event::{AppEvent, Metrics, NasStatus};
use crate::tui::Term;
use external::External;
use overlay::Overlay;
use panel::Panel;
use preview::PreviewContent;
use sidebar::Sidebar;

pub struct StartOptions {
    pub paths: Vec<PathBuf>,
    pub config_path: PathBuf,
    pub load_plugins: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Files,
    Dashboard,
    Docker,
    Connections,
}

impl View {
    pub const ALL: [View; 4] = [View::Files, View::Dashboard, View::Docker, View::Connections];

    pub fn title(self) -> &'static str {
        match self {
            View::Files => "Files",
            View::Dashboard => "Dashboard",
            View::Docker => "Docker",
            View::Connections => "NAS",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Panels,
    Sidebar,
}

pub struct Clipboard {
    pub vfs: VfsRef,
    pub paths: Vec<PathBuf>,
    pub mode: TransferMode,
}

pub struct Notification {
    pub message: String,
    pub level: Level,
    pub at: Instant,
}

#[derive(Default)]
pub struct DockerState {
    pub containers: Vec<Container>,
    pub error: Option<String>,
    pub cursor: usize,
    pub loading: bool,
    pub last: Option<Instant>,
}

pub struct NasState {
    pub statuses: HashMap<String, NasStatus>,
    pub last_up: HashMap<String, SystemTime>,
    pub cursor: usize,
    pub last_probe: Option<Instant>,
    /// Live SFTP sessions by connection name.
    pub sessions: HashMap<String, VfsRef>,
    /// Connections with a password in the keychain.
    pub saved_passwords: std::collections::HashSet<String>,
}

pub struct DashboardState {
    pub usage: Option<UsageReport>,
    pub scanning: Option<PathBuf>,
    pub cancel: Arc<AtomicBool>,
    pub cursor: usize,
}

/// Details about the hovered file computed in the background.
#[derive(Default)]
pub struct Inspection {
    pub path: Option<PathBuf>,
    pub arch: Option<String>,
    /// `None` while computing (or disabled), then the checksum or an error.
    pub md5: Option<Result<String, String>>,
    progress: Arc<Progress>,
}

/// Screen regions from the last draw, for mouse hit-testing.
#[derive(Default)]
pub struct LayoutCache {
    pub panels: Vec<Rect>,
    pub sidebar: Option<Rect>,
    pub list: Option<Rect>,
}

pub struct App {
    pub config: Config,
    pub config_path: PathBuf,
    pub themes: ThemeRegistry,
    pub theme: Theme,
    pub keymap: Keymap,
    pub local: VfsRef,

    pub panels: Vec<Panel>,
    pub active: usize,
    /// All tabs; the current one's panels live in `panels` (see `tabs.rs`).
    pub tabs: Vec<tabs::Tab>,
    pub tab: usize,
    git: vcs::GitCache,
    undo: undo::UndoHistory,
    /// Password typed for an SFTP login, offered for the keychain on success.
    pending_password: Option<(String, String)>,
    pub view: View,
    pub focus: Focus,
    pub sidebar: Sidebar,
    pub pinned: Vec<PathBuf>,
    pub overlay: Option<Overlay>,
    pub clipboard: Option<Clipboard>,
    pub jobs: JobManager<AppEvent>,
    pub notifications: Vec<Notification>,
    pub pending_keys: Vec<KeyPress>,
    pending_since: Instant,

    pub preview: PreviewContent,
    preview_for: Option<(PathBuf, Size)>,
    preview_due: Option<Instant>,
    preview_generation: u64,
    pub preview_area: Size,
    picker: Option<Picker>,
    highlighter: Option<Arc<highlight::Highlighter>>,
    pub inspection: Inspection,

    pub metrics: Metrics,
    pub docker: DockerState,
    pub nas: NasState,
    pub dashboard: DashboardState,
    find_cancel: Arc<AtomicBool>,

    pub plugins: Option<PluginHost>,
    pub plugin_status: Vec<String>,
    pub open_plugin_panels: Vec<String>,
    pub plugin_panel_lines: HashMap<String, Vec<String>>,
    plugin_refreshed: Instant,
    last_location: Option<(String, PathBuf)>,

    pub layout: LayoutCache,
    last_click: Option<(Instant, u16, u16)>,
    external: Option<External>,
    /// Set by `quit_cd`: report the last directory to the shell wrapper.
    cd_on_exit: bool,
    tx: Sender<AppEvent>,
    rx: Receiver<AppEvent>,
    should_quit: bool,
}

impl App {
    pub fn new(config: Config, opts: StartOptions, picker: Picker) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let config_dir = strata_config::config_dir();
        let (themes, theme_errors) = ThemeRegistry::load(&config_dir.join("themes"));

        let mut theme_error = None;
        let theme = match themes.get(&config.general.theme) {
            Some(t) => t.clone(),
            None => {
                theme_error = Some(format!("unknown theme '{}', using the default", config.general.theme));
                themes.get("catppuccin-mocha").cloned().expect("built-in theme exists")
            }
        };
        let theme = if config.general.transparent { theme.transparent() } else { theme };

        let local: VfsRef = Arc::new(LocalVfs);
        let sort = SortOptions { key: config.general.sort, reverse: false, dirs_first: config.general.dirs_first };
        let start_dirs = start_directories(&opts.paths, config.general.panels.clamp(1, 6));
        let panels = start_dirs
            .into_iter()
            .map(|dir| Panel::new(local.clone(), dir, sort, config.general.show_hidden))
            .collect();

        let pinned = load_pins(&config);
        let picker = config.general.image_preview.then_some(picker);
        let jobs = JobManager::new(tx.clone(), AppEvent::Job);

        let highlighter =
            config.general.syntax_highlight.then(|| Arc::new(highlight::Highlighter::new(&theme.palette)));
        let mut app = Self {
            keymap: Keymap::preset(config.general.keymap),
            config_path: opts.config_path,
            themes,
            theme,
            local,
            panels,
            active: 0,
            tabs: vec![tabs::Tab::default()],
            tab: 0,
            git: vcs::GitCache::default(),
            undo: undo::UndoHistory::default(),
            pending_password: None,
            view: View::Files,
            focus: Focus::Panels,
            sidebar: Sidebar::default(),
            pinned,
            overlay: None,
            clipboard: None,
            jobs,
            notifications: Vec::new(),
            pending_keys: Vec::new(),
            pending_since: Instant::now(),
            preview: PreviewContent::Empty,
            preview_for: None,
            preview_due: None,
            preview_generation: 0,
            preview_area: Size::new(40, 20),
            picker,
            highlighter,
            inspection: Inspection::default(),
            metrics: Metrics::default(),
            docker: DockerState::default(),
            nas: NasState {
                statuses: HashMap::new(),
                last_up: HashMap::new(),
                cursor: 0,
                last_probe: None,
                sessions: HashMap::new(),
                saved_passwords: Default::default(),
            },
            dashboard: DashboardState {
                usage: None,
                scanning: None,
                cancel: Arc::new(AtomicBool::new(false)),
                cursor: 0,
            },
            find_cancel: Arc::new(AtomicBool::new(false)),
            plugins: None,
            plugin_status: Vec::new(),
            open_plugin_panels: Vec::new(),
            plugin_panel_lines: HashMap::new(),
            plugin_refreshed: Instant::now(),
            last_location: None,
            layout: LayoutCache::default(),
            last_click: None,
            external: None,
            cd_on_exit: false,
            tx,
            rx,
            should_quit: false,
            config,
        };

        theme_errors.into_iter().chain(theme_error).for_each(|e| app.notify(e, Level::Warn));
        if opts.load_plugins {
            app.load_plugins();
        }
        let overrides = app.config.keys.clone();
        for e in app.keymap.apply_overrides(&overrides) {
            app.notify(e, Level::Warn);
        }
        app.rebuild_sidebar();
        Ok(app)
    }

    pub fn run(&mut self, terminal: &mut Term) -> Result<()> {
        workers::spawn_metrics(
            self.tx.clone(),
            Duration::from_millis(self.config.general.metrics_interval_ms.max(250)),
        );
        self.probe_connections();
        self.auto_connect();
        self.emit_plugin_event("startup");

        while !self.should_quit {
            terminal.draw(|frame| crate::ui::draw(frame, self))?;
            if event::poll(Duration::from_millis(50))? {
                // Handle every queued input before redrawing.
                loop {
                    match event::read()? {
                        Event::Key(key) if key.kind != KeyEventKind::Release => self.on_key(key),
                        Event::Mouse(mouse) => self.on_mouse(mouse),
                        Event::Paste(text) => self.on_paste(&text),
                        _ => {}
                    }
                    if !event::poll(Duration::ZERO)? || self.external.is_some() {
                        break;
                    }
                }
            }
            self.drain_events();
            self.tick();
            if let Some(ext) = self.external.take() {
                self.run_external(terminal, ext);
            }
        }
        self.emit_plugin_event("quit");
        Ok(())
    }

    /// Directory of the focused panel when the app quits.
    pub fn last_dir(&self) -> Option<PathBuf> {
        if !(self.cd_on_exit || self.config.general.cd_on_quit) {
            return None;
        }
        let p = self.panels.get(self.active)?;
        p.vfs.is_local().then(|| p.cwd.clone())
    }

    pub fn panel(&self) -> &Panel {
        &self.panels[self.active]
    }

    pub fn panel_mut(&mut self) -> &mut Panel {
        &mut self.panels[self.active]
    }

    pub fn notify(&mut self, message: impl Into<String>, level: Level) {
        self.notifications.push(Notification { message: message.into(), level, at: Instant::now() });
        if self.notifications.len() > 200 {
            self.notifications.remove(0);
        }
    }

    pub fn info(&mut self, message: impl Into<String>) {
        self.notify(message, Level::Info);
    }

    pub fn error(&mut self, message: impl Into<String>) {
        self.notify(message, Level::Error);
    }

    /// The notification to show in the status line, if still fresh.
    pub fn current_notification(&self) -> Option<&Notification> {
        let n = self.notifications.last()?;
        let ttl = if n.level == Level::Error { 10 } else { 4 };
        (n.at.elapsed() < Duration::from_secs(ttl)).then_some(n)
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    /// Makes the shell wrapper `cd` into the focused directory on exit.
    pub fn quit_and_cd(&mut self) {
        self.cd_on_exit = true;
    }

    /// Rebuilds theme-dependent state after a theme change.
    pub(super) fn on_theme_changed(&mut self) {
        if self.config.general.syntax_highlight {
            self.highlighter = Some(Arc::new(highlight::Highlighter::new(&self.theme.palette)));
            self.invalidate_preview();
        }
    }

    pub fn reload_all(&mut self) {
        self.panels.iter_mut().for_each(Panel::reload);
        self.preview_for = None;
        self.invalidate_git();
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.on_event(event);
        }
    }

    fn on_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Job(done) => {
                self.job_finished_for_undo(done.id, &done.state);
                match &done.state {
                    JobState::Done => self.info(format!("✓ {}", done.label)),
                    JobState::Failed(e) => self.error(format!("{} failed: {e}", done.label)),
                    JobState::Cancelled => self.notify(format!("{} cancelled", done.label), Level::Warn),
                    JobState::Running => {}
                }
                self.reload_all();
                self.emit_plugin_event("job_done");
            }
            AppEvent::Metrics(m) => {
                let disks_changed = m.disks.len() != self.metrics.disks.len()
                    || m.disks.iter().zip(&self.metrics.disks).any(|(a, b)| a.mount_point != b.mount_point);
                self.metrics = *m;
                if disks_changed {
                    self.rebuild_sidebar();
                }
            }
            AppEvent::Preview { generation, content } => {
                if generation == self.preview_generation {
                    self.preview = content;
                }
            }
            AppEvent::Docker(result) => {
                self.docker.loading = false;
                self.docker.last = Some(Instant::now());
                match result {
                    Ok(c) => {
                        self.docker.containers = c;
                        self.docker.error = None;
                    }
                    Err(e) => self.docker.error = Some(e),
                }
                self.docker.cursor = self.docker.cursor.min(self.docker.containers.len().saturating_sub(1));
            }
            AppEvent::DockerChanged => self.refresh_docker(),
            AppEvent::Nas(statuses) => {
                for s in statuses {
                    if matches!(s.reach, strata_core::nas::Reachability::Up { .. }) {
                        self.nas.last_up.insert(s.name.clone(), s.checked);
                    }
                    self.nas.statuses.insert(s.name.clone(), s);
                }
            }
            AppEvent::DiskUsage(report) => {
                if self.dashboard.scanning.as_ref() == Some(&report.root) {
                    self.dashboard.scanning = None;
                }
                self.dashboard.cursor = 0;
                self.dashboard.usage = Some(report);
            }
            AppEvent::Found { root, files } => self.open_find_picker(root, files),
            AppEvent::Diagnosed { name, steps } => self.show_diagnosis(&name, steps),
            AppEvent::Connected { name, result, path } => self.on_connected(name, result, path),
            AppEvent::Text { title, body } => {
                let lines = body.lines().map(str::to_string).collect::<Vec<_>>();
                let scroll = lines.len().saturating_sub(10);
                self.overlay = Some(Overlay::Text(overlay::TextPopup { title, lines, scroll }));
            }
            AppEvent::Notify { message, level } => self.notify(message, level),
            AppEvent::Inspected { path, arch } => {
                if self.inspection.path.as_ref() == Some(&path) {
                    self.inspection.arch = arch;
                }
            }
            AppEvent::Git { dir, status } => self.on_git_status(dir, status),
            AppEvent::Grep { root, pattern, result } => self.on_grep_results(root, pattern, result),
            AppEvent::Keychain(names) => self.nas.saved_passwords = names,
            AppEvent::Checksum { path, md5 } => {
                if self.inspection.path.as_ref() == Some(&path) {
                    self.inspection.md5 = Some(md5);
                }
            }
        }
    }

    /// Periodic work: key-sequence timeouts, previews, plugin refresh and
    /// polling views.
    fn tick(&mut self) {
        if !self.pending_keys.is_empty() && self.pending_since.elapsed() > Duration::from_millis(900) {
            self.flush_pending_keys();
        }
        self.track_location();
        self.update_preview();
        self.update_inspection();
        self.update_git();

        if self.plugin_refreshed.elapsed() > Duration::from_secs(2) {
            self.refresh_plugin_ui();
        }
        if self.view == View::Docker
            && !self.docker.loading
            && self.docker.last.is_none_or(|t| t.elapsed() > Duration::from_secs(3))
        {
            self.refresh_docker();
        }
        if self.nas.last_probe.is_some_and(|t| t.elapsed() > Duration::from_secs(15)) {
            self.probe_connections();
        }
        if self.view == View::Dashboard {
            self.ensure_disk_usage();
        }
        self.process_plugin_requests();
    }

    /// Notices directory changes of the focused panel and tells plugins.
    fn track_location(&mut self) {
        let p = self.panel();
        let here = (p.vfs.label(), p.cwd.clone());
        if self.last_location.as_ref() != Some(&here) {
            self.last_location = Some(here);
            self.emit_plugin_event("cd");
            self.refresh_plugin_ui();
        }
    }

    fn update_preview(&mut self) {
        if !self.config.general.preview || self.view != View::Files {
            return;
        }
        let Some(entry) = self.panel().hovered().cloned() else {
            if self.preview_for.take().is_some() {
                self.preview = PreviewContent::Empty;
            }
            return;
        };
        let key = (entry.path.clone(), self.preview_area);
        if self.preview_for.as_ref() != Some(&key) {
            // Only a resize of a non-image does not need a new preview.
            let same_path = self.preview_for.as_ref().is_some_and(|(p, _)| *p == entry.path);
            self.preview_for = Some(key);
            if same_path && !preview::is_image(&entry) {
                return;
            }
            self.preview_generation += 1;
            if let Some(lines) = self.plugin_preview(&entry) {
                self.preview = lines;
                self.preview_due = None;
                return;
            }
            self.preview = PreviewContent::Loading;
            self.preview_due = Some(Instant::now() + Duration::from_millis(40));
        }
        if self.preview_due.is_some_and(|due| Instant::now() >= due) {
            self.preview_due = None;
            preview::PreviewJob {
                generation: self.preview_generation,
                vfs: self.panel().vfs.clone(),
                entry,
                show_hidden: self.panel().show_hidden,
                size: self.preview_area,
                picker: self.picker.clone(),
                highlighter: self.highlighter.clone(),
            }
            .spawn(self.tx.clone());
        }
    }

    /// Inspects the hovered file for the metadata pane.
    fn update_inspection(&mut self) {
        if !self.config.general.footer || self.view != View::Files {
            return;
        }
        let hovered = self.panel().hovered().filter(|e| !e.is_dir()).map(|e| e.path.clone());
        if hovered == self.inspection.path {
            return;
        }
        self.inspection.progress.cancel();
        let progress = Arc::new(Progress::default());
        self.inspection = Inspection { path: hovered.clone(), arch: None, md5: None, progress: progress.clone() };
        if let Some(path) = hovered {
            let md5 = self.config.general.md5_checksum;
            workers::inspect(self.tx.clone(), self.panel().vfs.clone(), path, md5, progress);
        }
    }

    pub fn invalidate_preview(&mut self) {
        self.preview_for = None;
    }

    fn ensure_disk_usage(&mut self) {
        let p = self.panel();
        if !p.vfs.is_local() {
            return;
        }
        let root = p.cwd.clone();
        let current = self.dashboard.usage.as_ref().map(|u| &u.root);
        if current == Some(&root) || self.dashboard.scanning.as_ref() == Some(&root) {
            return;
        }
        self.dashboard.cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.dashboard.cancel = cancel.clone();
        self.dashboard.scanning = Some(root.clone());
        workers::disk_usage(self.tx.clone(), root, cancel);
    }

    pub fn refresh_docker(&mut self) {
        self.docker.loading = true;
        workers::list_containers(self.tx.clone());
    }

    pub fn probe_connections(&mut self) {
        self.nas.last_probe = Some(Instant::now());
        workers::probe_connections(self.tx.clone(), self.config.connections.clone());
    }
}

/// One directory per panel: CLI paths first, then the current directory.
fn start_directories(paths: &[PathBuf], count: usize) -> Vec<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")));
    let mut dirs: Vec<PathBuf> = paths
        .iter()
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .map(|p| if p.is_dir() { p } else { p.parent().map(PathBuf::from).unwrap_or(p) })
        .collect();
    while dirs.len() < count.max(paths.len().min(6)) {
        dirs.push(cwd.clone());
    }
    dirs
}

fn pins_file() -> PathBuf {
    strata_config::data_dir().join("pinned.txt")
}

fn load_pins(config: &Config) -> Vec<PathBuf> {
    let mut pins: Vec<PathBuf> = config.pinned.iter().map(|p| strata_core::util::expand_tilde(p)).collect();
    if let Ok(text) = std::fs::read_to_string(pins_file()) {
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let p = PathBuf::from(line);
            if !pins.contains(&p) {
                pins.push(p);
            }
        }
    }
    pins
}
