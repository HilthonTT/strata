//! Background workers. Each sends its result to the UI as an [`AppEvent`].

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use strata_core::nas::{self, Connection};
use strata_sys::docker::{self, ContainerAction};
use strata_sys::{IoSampler, MemoryMonitor};

use crate::event::{AppEvent, Metrics, NasStatus};

/// Samples disks, I/O and memory forever at `interval`.
pub fn spawn_metrics(tx: Sender<AppEvent>, interval: Duration) {
    thread::spawn(move || {
        let mut io = IoSampler::new();
        let mut memory = MemoryMonitor::new();
        let mut disks = strata_sys::list_disks();
        let mut disks_at = Instant::now();
        io.sample();
        loop {
            thread::sleep(interval);
            if disks_at.elapsed() > Duration::from_secs(5) {
                disks = strata_sys::list_disks();
                disks_at = Instant::now();
            }
            let metrics = Metrics {
                disks: disks.clone(),
                io: io.sample(),
                memory: memory.sample(),
            };
            if tx.send(AppEvent::Metrics(Box::new(metrics))).is_err() {
                return;
            }
        }
    });
}

pub fn list_containers(tx: Sender<AppEvent>) {
    thread::spawn(move || {
        let result = docker::list_containers().map_err(|e| format!("{e:#}"));
        let _ = tx.send(AppEvent::Docker(result));
    });
}

pub fn container_action(tx: Sender<AppEvent>, id: String, name: String, action: ContainerAction) {
    thread::spawn(move || {
        let event = match docker::act(&id, action) {
            Ok(()) => AppEvent::Notify {
                message: format!("{} {name}", action.verb()),
                level: strata_plugin::Level::Info,
            },
            Err(e) => AppEvent::Notify {
                message: format!("docker {}: {e:#}", action.verb()),
                level: strata_plugin::Level::Error,
            },
        };
        let _ = tx.send(event);
        let _ = tx.send(AppEvent::DockerChanged);
    });
}

pub fn container_logs(tx: Sender<AppEvent>, id: String, name: String) {
    thread::spawn(move || {
        let body = docker::logs(&id, 500).unwrap_or_else(|e| format!("{e:#}"));
        let _ = tx.send(AppEvent::Text {
            title: format!("logs: {name}"),
            body,
        });
    });
}

pub fn probe_connections(tx: Sender<AppEvent>, connections: Vec<Connection>) {
    if connections.is_empty() {
        return;
    }
    thread::spawn(move || {
        let statuses = thread::scope(|s| {
            let handles: Vec<_> = connections
                .iter()
                .map(|c| {
                    s.spawn(move || NasStatus {
                        name: c.name.clone(),
                        reach: nas::probe(&c.host, &nas::probe_ports(c), Duration::from_secs(2)),
                        mounted: c.mounted_at(),
                        checked: SystemTime::now(),
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        let _ = tx.send(AppEvent::Nas(statuses));
    });
}

pub fn diagnose(tx: Sender<AppEvent>, conn: Connection) {
    thread::spawn(move || {
        let steps = nas::diagnose(&conn);
        let _ = tx.send(AppEvent::Diagnosed {
            name: conn.name,
            steps,
        });
    });
}

#[cfg(feature = "sftp")]
pub fn connect_sftp(tx: Sender<AppEvent>, conn: Connection, password: Option<String>) {
    use strata_core::vfs::{SftpAuth, SftpVfs};
    thread::spawn(move || {
        let user = conn
            .user
            .clone()
            .unwrap_or_else(|| std::env::var("USER").unwrap_or_else(|_| "root".into()));
        let auth = match (password, &conn.identity_file) {
            (Some(pw), _) => SftpAuth::Password(pw),
            (None, Some(key)) => {
                SftpAuth::Key(strata_core::util::expand_tilde(&key.to_string_lossy()))
            }
            (None, None) => SftpAuth::Auto,
        };
        let result = SftpVfs::connect(&conn.host, conn.port(), &user, &auth)
            .map(|v| Arc::new(v) as strata_core::VfsRef)
            .map_err(|e| format!("{e:#}"));
        let path = match (&result, conn.share.is_empty()) {
            (Ok(vfs), true) => vfs.home(),
            _ => PathBuf::from(&conn.share),
        };
        let _ = tx.send(AppEvent::Connected {
            name: conn.name,
            result,
            path,
        });
    });
}

pub fn disk_usage(tx: Sender<AppEvent>, root: PathBuf, cancel: Arc<AtomicBool>) {
    thread::spawn(move || {
        if let Some(report) = strata_sys::du::scan(&root, &cancel) {
            let _ = tx.send(AppEvent::DiskUsage(report));
        }
    });
}

/// Reads the hovered file's header and, when asked, its MD5 checksum.
pub fn inspect(
    tx: Sender<AppEvent>,
    vfs: strata_core::VfsRef,
    path: PathBuf,
    md5: bool,
    progress: Arc<strata_core::jobs::Progress>,
) {
    thread::spawn(move || {
        let arch = strata_core::inspect::file_arch(&*vfs, &path);
        let _ = tx.send(AppEvent::Inspected {
            path: path.clone(),
            arch,
        });
        if md5 {
            match strata_core::inspect::md5(&*vfs, &path, &progress) {
                Err(e) if e.is::<strata_core::jobs::Cancelled>() => {}
                result => {
                    let _ = tx.send(AppEvent::Checksum {
                        path,
                        md5: result.map_err(|e| format!("{e:#}")),
                    });
                }
            }
        }
    });
}

pub fn find_files(tx: Sender<AppEvent>, root: PathBuf, show_hidden: bool, cancel: Arc<AtomicBool>) {
    thread::spawn(move || {
        let files = strata_core::search::walk_files(&root, 100_000, show_hidden, &cancel);
        let _ = tx.send(AppEvent::Found { root, files });
    });
}
