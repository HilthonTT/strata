//! Sidebar: well-known places, pinned directories, disks and NAS connections.

use std::path::PathBuf;

use strata_core::nas::Connection;
use strata_sys::DiskInfo;

#[derive(Debug, Clone)]
pub enum SidebarItem {
    Header(&'static str),
    Place {
        label: String,
        path: PathBuf,
        icon: &'static str,
    },
    Pinned(PathBuf),
    Disk {
        label: String,
        mount: PathBuf,
        ratio: f64,
        network: bool,
    },
    Connection(String),
}

impl SidebarItem {
    pub fn selectable(&self) -> bool {
        !matches!(self, SidebarItem::Header(_))
    }
}

#[derive(Debug, Default)]
pub struct Sidebar {
    pub items: Vec<SidebarItem>,
    pub cursor: usize,
    pub offset: usize,
}

impl Sidebar {
    pub fn rebuild(&mut self, pinned: &[PathBuf], disks: &[DiskInfo], connections: &[Connection]) {
        let mut items = vec![SidebarItem::Header("Places")];
        items.extend(places());
        if !pinned.is_empty() {
            items.push(SidebarItem::Header("Pinned"));
            items.extend(pinned.iter().cloned().map(SidebarItem::Pinned));
        }
        if !disks.is_empty() {
            items.push(SidebarItem::Header("Disks"));
            items.extend(disks.iter().map(|d| SidebarItem::Disk {
                label: disk_label(d),
                mount: d.mount_point.clone(),
                ratio: d.used_ratio(),
                network: d.network,
            }));
        }
        if !connections.is_empty() {
            items.push(SidebarItem::Header("Network"));
            items.extend(
                connections
                    .iter()
                    .map(|c| SidebarItem::Connection(c.name.clone())),
            );
        }
        self.items = items;
        self.cursor = self.cursor.min(self.items.len().saturating_sub(1));
        if !self
            .items
            .get(self.cursor)
            .is_some_and(SidebarItem::selectable)
        {
            self.move_by(1);
        }
    }

    pub fn selected(&self) -> Option<&SidebarItem> {
        self.items.get(self.cursor).filter(|i| i.selectable())
    }

    pub fn move_by(&mut self, delta: isize) {
        let len = self.items.len() as isize;
        if len == 0 {
            return;
        }
        let step = if delta < 0 { -1 } else { 1 };
        let mut remaining = delta.abs();
        let mut pos = self.cursor as isize;
        while remaining > 0 {
            let mut next = pos + step;
            while (0..len).contains(&next) && !self.items[next as usize].selectable() {
                next += step;
            }
            if !(0..len).contains(&next) {
                break;
            }
            pos = next;
            remaining -= 1;
        }
        self.cursor = pos as usize;
    }

    pub fn first(&mut self) {
        self.cursor = 0;
        self.move_by(1);
    }

    pub fn last(&mut self) {
        self.cursor = self.items.len().saturating_sub(1);
    }
}

fn disk_label(d: &DiskInfo) -> String {
    let mount = d.mount_point.to_string_lossy();
    if mount == "/" {
        return "/".into();
    }
    d.mount_point
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| mount.into_owned())
}

fn places() -> Vec<SidebarItem> {
    let mut out = Vec::new();
    let mut add = |label: &str, path: Option<PathBuf>, icon: &'static str| {
        if let Some(path) = path.filter(|p| p.is_dir()) {
            out.push(SidebarItem::Place {
                label: label.into(),
                path,
                icon,
            });
        }
    };
    add("Home", dirs::home_dir(), "\u{f015}");
    add("Desktop", dirs::desktop_dir(), "\u{f108}");
    add("Documents", dirs::document_dir(), "\u{f02d}");
    add("Downloads", dirs::download_dir(), "\u{f019}");
    add("Pictures", dirs::picture_dir(), "\u{f03e}");
    add("Music", dirs::audio_dir(), "\u{f001}");
    add("Videos", dirs::video_dir(), "\u{f03d}");
    #[cfg(target_os = "linux")]
    add(
        "Trash",
        dirs::data_dir().map(|d| d.join("Trash/files")),
        "\u{f1f8}",
    );
    add("Root", Some(root_dir()), "\u{f0a0}");
    out
}

pub fn root_dir() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("C:\\")
    } else {
        PathBuf::from("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_skips_headers() {
        let mut s = Sidebar::default();
        s.rebuild(&[PathBuf::from("/tmp")], &[], &[]);
        assert!(s.selected().is_some());
        s.last();
        s.move_by(-100);
        assert!(s.selected().is_some(), "never rests on a header");
        assert!(matches!(s.items[0], SidebarItem::Header(_)));
        assert_ne!(s.cursor, 0);
    }
}
