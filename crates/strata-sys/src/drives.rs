//! Removable drives (USB sticks, SD cards, external disks): listing them,
//! mounted or not, and the commands that mount, unmount and eject them.
//!
//! Linux goes through `lsblk` and `udisksctl` (no root needed); macOS
//! through `diskutil`, which mounts external drives on its own, so only
//! mounted volumes are listed there.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context as _, Result};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drive {
    /// Block device of the filesystem, e.g. `/dev/sdb1` or `/dev/disk4s1`.
    pub device: String,
    /// The whole disk holding it, powered off on eject.
    pub disk: String,
    pub label: Option<String>,
    pub size: u64,
    pub fs: String,
    pub mount: Option<PathBuf>,
}

impl Drive {
    /// The label, or the device name.
    pub fn name(&self) -> &str {
        self.label
            .as_deref()
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| Path::new(&self.device).file_name().and_then(|n| n.to_str()).unwrap_or(&self.device))
    }
}

/// Removable drives with a filesystem, mounted or not.
pub fn list() -> Result<Vec<Drive>> {
    if cfg!(target_os = "linux") {
        let out = Command::new("lsblk")
            .args(["-J", "-l", "-b", "-o", "NAME,PATH,PKNAME,LABEL,SIZE,RM,HOTPLUG,TYPE,FSTYPE,MOUNTPOINT"])
            .output()
            .context("running lsblk")?;
        if !out.status.success() {
            bail!("lsblk failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        parse_lsblk(&out.stdout)
    } else if cfg!(target_os = "macos") {
        Ok(crate::disks::list_disks()
            .into_iter()
            .filter(|d| d.removable && d.mount_point.starts_with("/Volumes"))
            .map(|d| Drive {
                device: d.name.clone(),
                disk: d.name,
                label: d.mount_point.file_name().map(|n| n.to_string_lossy().into_owned()),
                size: d.total,
                fs: d.file_system,
                mount: Some(d.mount_point),
            })
            .collect())
    } else {
        bail!("removable drives are supported on Linux and macOS")
    }
}

#[derive(Deserialize)]
struct Lsblk {
    blockdevices: Vec<BlockDevice>,
}

#[derive(Deserialize)]
struct BlockDevice {
    name: String,
    path: Option<String>,
    pkname: Option<String>,
    label: Option<String>,
    #[serde(deserialize_with = "number_or_string")]
    size: u64,
    #[serde(deserialize_with = "flag")]
    rm: bool,
    #[serde(deserialize_with = "flag")]
    hotplug: bool,
    #[serde(rename = "type")]
    kind: String,
    fstype: Option<String>,
    mountpoint: Option<String>,
}

/// Older `lsblk` prints numbers and booleans as strings (`"1"`, `"0"`).
fn flag<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Bool(b) => b,
        serde_json::Value::String(s) => s == "1" || s == "true",
        serde_json::Value::Number(n) => n.as_u64() == Some(1),
        _ => false,
    })
}

fn number_or_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n.as_u64().unwrap_or(0),
        serde_json::Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    })
}

fn parse_lsblk(json: &[u8]) -> Result<Vec<Drive>> {
    let parsed: Lsblk = serde_json::from_slice(json).context("reading lsblk output")?;
    let devices = &parsed.blockdevices;
    let removable = |d: &BlockDevice| d.rm || d.hotplug;
    let parent = |d: &BlockDevice| d.pkname.as_ref().and_then(|p| devices.iter().find(|x| &x.name == p));
    Ok(devices
        .iter()
        .filter(|d| matches!(d.kind.as_str(), "part" | "disk"))
        .filter(|d| d.fstype.as_deref().is_some_and(|f| !f.is_empty() && f != "swap" && !f.ends_with("_member")))
        .filter(|d| removable(d) || parent(d).is_some_and(removable))
        .map(|d| {
            let path = d.path.clone().unwrap_or_else(|| format!("/dev/{}", d.name));
            let disk = parent(d).map(|p| p.path.clone().unwrap_or_else(|| format!("/dev/{}", p.name)));
            Drive {
                disk: disk.unwrap_or_else(|| path.clone()),
                device: path,
                label: d.label.clone(),
                size: d.size,
                fs: d.fstype.clone().unwrap_or_default(),
                mount: d.mountpoint.as_ref().filter(|m| !m.is_empty()).map(PathBuf::from),
            }
        })
        .collect())
}

/// The command that mounts `drive` where the system puts removable media.
pub fn mount_command(drive: &Drive) -> Vec<String> {
    if cfg!(target_os = "macos") {
        vec!["diskutil".into(), "mount".into(), drive.device.clone()]
    } else {
        vec!["udisksctl".into(), "mount".into(), "--block-device".into(), drive.device.clone()]
    }
}

/// The command that unmounts `drive` and, with `eject`, powers its disk
/// off so it can be pulled out safely.
pub fn unmount_command(drive: &Drive, eject: bool) -> Vec<String> {
    if cfg!(target_os = "macos") {
        let target = drive.mount.as_ref().map(|m| m.to_string_lossy().into_owned()).unwrap_or(drive.device.clone());
        return vec!["diskutil".into(), if eject { "eject" } else { "unmount" }.into(), target];
    }
    let unmount = format!("udisksctl unmount --block-device {}", quote(&drive.device));
    if !eject {
        return vec!["sh".into(), "-c".into(), unmount];
    }
    // Other partitions of the same disk must be unmounted before power-off.
    let line = format!("{unmount} && udisksctl power-off --block-device {}", quote(&drive.disk));
    vec!["sh".into(), "-c".into(), line]
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The drive mounted at `path` or above it.
pub fn containing<'a>(drives: &'a [Drive], path: &Path) -> Option<&'a Drive> {
    drives
        .iter()
        .filter(|d| d.mount.as_ref().is_some_and(|m| path.starts_with(m)))
        .max_by_key(|d| d.mount.as_ref().map(|m| m.components().count()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LSBLK: &str = r#"{"blockdevices": [
        {"name":"nvme0n1","path":"/dev/nvme0n1","pkname":null,"label":null,"size":512110190592,"rm":false,"hotplug":false,"type":"disk","fstype":null,"mountpoint":null},
        {"name":"nvme0n1p2","path":"/dev/nvme0n1p2","pkname":"nvme0n1","label":null,"size":511000000000,"rm":false,"hotplug":false,"type":"part","fstype":"ext4","mountpoint":"/"},
        {"name":"sdb","path":"/dev/sdb","pkname":null,"label":null,"size":"15931539456","rm":"1","hotplug":"1","type":"disk","fstype":null,"mountpoint":null},
        {"name":"sdb1","path":"/dev/sdb1","pkname":"sdb","label":"STICK","size":15930490880,"rm":false,"hotplug":true,"type":"part","fstype":"vfat","mountpoint":"/media/me/STICK"},
        {"name":"sdb2","path":"/dev/sdb2","pkname":"sdb","label":null,"size":1048576,"rm":false,"hotplug":true,"type":"part","fstype":"exfat","mountpoint":null},
        {"name":"sdc","path":"/dev/sdc","pkname":null,"label":"RAW","size":1000,"rm":true,"hotplug":true,"type":"disk","fstype":"ext4","mountpoint":null},
        {"name":"sdd","path":"/dev/sdd","pkname":null,"label":null,"size":1000,"rm":true,"hotplug":true,"type":"disk","fstype":"swap","mountpoint":null}
    ]}"#;

    #[test]
    fn lists_removable_filesystems_only() {
        let drives = parse_lsblk(LSBLK.as_bytes()).unwrap();
        let devices: Vec<&str> = drives.iter().map(|d| d.device.as_str()).collect();
        assert_eq!(devices, ["/dev/sdb1", "/dev/sdb2", "/dev/sdc"]);
        assert_eq!(drives[0].disk, "/dev/sdb");
        assert_eq!(drives[0].name(), "STICK");
        assert_eq!(drives[1].name(), "sdb2");
        assert_eq!(drives[0].mount.as_deref(), Some(Path::new("/media/me/STICK")));
        assert_eq!(drives[2].disk, "/dev/sdc", "an unpartitioned disk is its own disk");

        let inside = containing(&drives, Path::new("/media/me/STICK/photos")).unwrap();
        assert_eq!(inside.device, "/dev/sdb1");
        assert!(containing(&drives, Path::new("/home")).is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn eject_unmounts_then_powers_off() {
        let drives = parse_lsblk(LSBLK.as_bytes()).unwrap();
        let cmd = unmount_command(&drives[0], true);
        assert_eq!(
            cmd[2],
            "udisksctl unmount --block-device '/dev/sdb1' && udisksctl power-off --block-device '/dev/sdb'"
        );
        assert_eq!(mount_command(&drives[1]), ["udisksctl", "mount", "--block-device", "/dev/sdb2"]);
    }
}
