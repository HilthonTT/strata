use std::path::PathBuf;

use sysinfo::Disks;

/// Filesystem types served over the network.
const NETWORK_FS: &[&str] = &[
    "cifs",
    "smb",
    "smb2",
    "smb3",
    "smbfs",
    "nfs",
    "nfs4",
    "afpfs",
    "fuse.sshfs",
    "sshfs",
    "fuse.rclone",
    "davfs",
    "webdav",
];

/// Mount points that are implementation details (WSL, Docker Desktop, snaps).
const IGNORED_MOUNTS: &[&str] = &["/usr/lib/wsl", "/mnt/wslg", "/mnt/wsl", "/snap", "/var/lib/docker", "/boot/efi"];

/// Pseudo filesystems that are noise in a disk list.
const IGNORED_FS: &[&str] = &["overlay", "squashfs", "tmpfs", "devtmpfs", "ramfs", "proc", "sysfs", "autofs"];

#[derive(Debug, Clone)]
pub struct DiskInfo {
    pub name: String,
    pub mount_point: PathBuf,
    pub file_system: String,
    pub total: u64,
    pub available: u64,
    pub removable: bool,
    pub read_only: bool,
    pub network: bool,
}

impl DiskInfo {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn used_ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.used() as f64 / self.total as f64
        }
    }
}

/// Mounted disks with capacity, de-duplicated and sorted by mount point.
pub fn list_disks() -> Vec<DiskInfo> {
    let disks = Disks::new_with_refreshed_list();
    let mut out: Vec<DiskInfo> = disks
        .list()
        .iter()
        .filter_map(|d| {
            let fs = d.file_system().to_string_lossy().to_ascii_lowercase();
            let hidden = IGNORED_MOUNTS.iter().any(|m| d.mount_point().starts_with(m));
            if IGNORED_FS.contains(&fs.as_str()) || d.total_space() == 0 || hidden {
                return None;
            }
            Some(DiskInfo {
                name: d.name().to_string_lossy().into_owned(),
                mount_point: d.mount_point().to_path_buf(),
                network: NETWORK_FS.iter().any(|n| fs == *n || fs.starts_with(n)),
                file_system: fs,
                total: d.total_space(),
                available: d.available_space(),
                removable: d.is_removable(),
                read_only: d.is_read_only(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    out.dedup_by(|a, b| a.mount_point == b.mount_point);
    out
}

/// The disk holding `path` (longest matching mount point).
pub fn disk_for<'a>(disks: &'a [DiskInfo], path: &std::path::Path) -> Option<&'a DiskInfo> {
    disks.iter().filter(|d| path.starts_with(&d.mount_point)).max_by_key(|d| d.mount_point.as_os_str().len())
}
