//! Messages from worker threads to the UI thread.

use std::path::PathBuf;
use std::time::SystemTime;

use strata_core::git::GitStatus;
use strata_core::inspect::HashAlgo;
use strata_core::jobs::JobFinished;
use strata_core::nas::{Connection, Reachability, Step};
use strata_core::ssh::HostKeyError;
use strata_core::VfsRef;
use strata_plugin::Level;
use strata_sys::docker::Container;
use strata_sys::du::UsageReport;
use strata_sys::{DiskInfo, IoStats, MemorySnapshot};

use crate::app::grep::GrepResult;
use crate::app::preview::PreviewContent;

#[derive(Debug, Clone, Default)]
pub struct Metrics {
    pub disks: Vec<DiskInfo>,
    pub io: Vec<IoStats>,
    pub memory: MemorySnapshot,
}

#[derive(Debug, Clone)]
pub struct NasStatus {
    pub name: String,
    pub reach: Reachability,
    pub mounted: Option<PathBuf>,
    pub checked: SystemTime,
}

pub enum AppEvent {
    Job(JobFinished),
    Metrics(Box<Metrics>),
    Preview {
        generation: u64,
        content: PreviewContent,
    },
    Docker(Result<Vec<Container>, String>),
    DockerChanged,
    Nas(Vec<NasStatus>),
    DiskUsage(UsageReport),
    Found {
        root: PathBuf,
        files: Vec<PathBuf>,
    },
    Diagnosed {
        name: String,
        steps: Vec<Step>,
    },
    Connected {
        conn: Connection,
        result: Result<VfsRef, String>,
        path: PathBuf,
    },
    /// An SFTP server's host key is new, changed or revoked. No
    /// credentials were sent; `password` is the one typed, if any.
    #[cfg_attr(not(feature = "sftp"), allow(dead_code))]
    HostKey {
        conn: Connection,
        password: Option<String>,
        problem: Box<HostKeyError>,
    },
    Text {
        title: String,
        body: String,
    },
    Notify {
        message: String,
        level: Level,
    },
    /// Architecture of the hovered file (for executables).
    Inspected {
        path: PathBuf,
        arch: Option<String>,
    },
    /// Checksums of the hovered file, in the order of `[md5, sha256]`
    /// that are enabled.
    Checksum {
        path: PathBuf,
        sums: Result<Vec<(HashAlgo, String)>, String>,
    },
    /// A report to show in a scrollable popup, from the top.
    Report {
        title: String,
        lines: Vec<String>,
    },
    /// A sync plan is ready to confirm (or to show, for a dry run).
    SyncPlanned(Box<crate::app::sync::SyncRequest>),
    /// Duplicate files found below `root`.
    Dupes {
        root: PathBuf,
        report: strata_core::dupes::DupeReport,
    },
    /// Text to put on the system clipboard (from the UI thread).
    Clipboard {
        text: String,
        message: String,
    },
    /// An archive finished indexing for a panel.
    ArchiveOpened {
        panel: u64,
        file: PathBuf,
        result: Result<VfsRef, String>,
    },
    /// An archive opened for `:extract` into `dest`.
    ArchiveToExtract {
        file: PathBuf,
        dest: PathBuf,
        result: Result<VfsRef, String>,
    },
    /// An encrypted archive needs a password, or a different one.
    ArchiveLocked {
        file: PathBuf,
        purpose: crate::app::ArchiveUse,
        reason: strata_core::vfs::Locked,
    },
    GitDone(Result<crate::app::GitReply, String>),
    Git {
        dir: PathBuf,
        status: Option<GitStatus>,
    },
    Grep {
        root: PathBuf,
        pattern: String,
        result: Result<GrepResult, String>,
    },
    /// Files changed on disk (from the watcher).
    FsChanged(Vec<PathBuf>),
    /// The size of a folder shown in a panel (`dir_sizes`).
    DirSize {
        path: PathBuf,
        size: u64,
    },
    /// Every folder in `root` has been measured.
    DirSizesDone {
        root: PathBuf,
    },
    /// Connections that have a password in the keychain.
    Keychain(std::collections::HashSet<String>),
    /// Finishes work started with `App::run_io` on the UI thread.
    Then(Box<dyn FnOnce(&mut crate::app::App) + Send>),
    /// An SMB share's keychain password was looked up: mount it.
    MountReady {
        conn: Connection,
        password: Option<String>,
    },
}
