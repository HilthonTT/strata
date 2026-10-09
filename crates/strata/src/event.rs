//! Messages from worker threads to the UI thread.

use std::path::PathBuf;
use std::time::SystemTime;

use strata_core::git::GitStatus;
use strata_core::jobs::JobFinished;
use strata_core::nas::{Reachability, Step};
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
    #[cfg_attr(not(feature = "sftp"), allow(dead_code))]
    Connected {
        name: String,
        result: Result<VfsRef, String>,
        path: PathBuf,
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
    Checksum {
        path: PathBuf,
        md5: Result<String, String>,
    },
    Git {
        dir: PathBuf,
        status: Option<GitStatus>,
    },
    Grep {
        root: PathBuf,
        pattern: String,
        result: Result<GrepResult, String>,
    },
    /// Connections that have a password in the keychain.
    Keychain(std::collections::HashSet<String>),
}
