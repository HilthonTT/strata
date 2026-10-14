//! Processes holding a file or folder open: what keeps a drive "busy" when
//! it won't unmount. Linux reads `/proc` (open files, working and root
//! directories, the executable and mapped libraries); macOS and other
//! Unixes ask `lsof`.

use std::path::{Path, PathBuf};

use anyhow::Result;

/// A process using something at or below the path that was asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    pub command: String,
    pub uses: Vec<Use>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Use {
    /// An open file or folder.
    Open(PathBuf),
    /// The process's working directory.
    Cwd(PathBuf),
    /// Its root directory (chroot).
    Root(PathBuf),
    /// The program it runs.
    Exe(PathBuf),
    /// A file mapped into memory, such as a library.
    Mapped(PathBuf),
}

impl Use {
    pub fn kind(&self) -> &'static str {
        match self {
            Use::Open(_) => "open",
            Use::Cwd(_) => "cwd",
            Use::Root(_) => "root",
            Use::Exe(_) => "program",
            Use::Mapped(_) => "mapped",
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            Use::Open(p) | Use::Cwd(p) | Use::Root(p) | Use::Exe(p) | Use::Mapped(p) => p,
        }
    }
}

/// What was found, and how many processes could not be looked into
/// (other users' processes, without root).
#[derive(Debug, Default)]
pub struct Holders {
    pub found: Vec<Holder>,
    pub unreadable: usize,
}

/// Processes using `target` or anything below it, by pid.
pub fn find(target: &Path) -> Result<Holders> {
    // The kernel and lsof report real paths.
    let target = &std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    #[cfg(target_os = "linux")]
    {
        Ok(from_proc(Path::new("/proc"), target))
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        from_lsof(target)
    }
    #[cfg(not(unix))]
    {
        let _ = target;
        anyhow::bail!("finding what holds a file open is not supported on this system yet")
    }
}

/// `path` is `target` or inside it.
#[cfg(any(unix, test))]
fn within(path: &Path, target: &Path) -> bool {
    path.starts_with(target)
}

/// A link target from `/proc`, without the ` (deleted)` the kernel adds
/// for removed files.
#[cfg(any(target_os = "linux", all(unix, test)))]
fn proc_link(path: &Path) -> Option<PathBuf> {
    let link = std::fs::read_link(path).ok()?;
    let text = link.to_string_lossy();
    Some(match text.strip_suffix(" (deleted)") {
        Some(rest) => PathBuf::from(rest),
        None => link,
    })
}

/// Walks every process under `proc` (a `/proc`-like tree).
#[cfg(any(target_os = "linux", all(unix, test)))]
fn from_proc(proc: &Path, target: &Path) -> Holders {
    let mut holders = Holders::default();
    let Ok(dir) = std::fs::read_dir(proc) else {
        return holders;
    };
    for process in dir.flatten() {
        let Some(pid) = process.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let base = process.path();
        let mut uses = Vec::new();
        let mut readable = false;
        for (link, make) in [("cwd", Use::Cwd as fn(PathBuf) -> Use), ("root", Use::Root), ("exe", Use::Exe)] {
            if let Some(path) = proc_link(&base.join(link)) {
                readable = true;
                // Every process has `/` as its root; only a chroot inside counts.
                if within(&path, target) && !(link == "root" && path == Path::new("/")) {
                    uses.push(make(path));
                }
            }
        }
        if let Ok(fds) = std::fs::read_dir(base.join("fd")) {
            readable = true;
            let mut open: Vec<PathBuf> = fds
                .flatten()
                .filter_map(|fd| proc_link(&fd.path()))
                .filter(|p| p.is_absolute() && within(p, target))
                .collect();
            open.sort();
            open.dedup();
            uses.extend(open.into_iter().map(Use::Open));
        }
        if let Ok(maps) = std::fs::read_to_string(base.join("maps")) {
            let exe = uses.iter().find_map(|u| matches!(u, Use::Exe(_)).then(|| u.path().to_path_buf()));
            let mut mapped: Vec<PathBuf> =
                maps.lines().filter_map(mapped_path).filter(|p| within(p, target) && Some(p) != exe.as_ref()).collect();
            mapped.sort();
            mapped.dedup();
            uses.extend(mapped.into_iter().map(Use::Mapped));
        }
        if !readable {
            holders.unreadable += 1;
            continue;
        }
        if !uses.is_empty() {
            let command = std::fs::read_to_string(base.join("comm")).unwrap_or_default().trim().to_string();
            holders.found.push(Holder { pid, command, uses });
        }
    }
    holders.found.sort_by_key(|h| h.pid);
    holders
}

/// The file of a `/proc/<pid>/maps` line, if it maps one.
#[cfg(any(target_os = "linux", test))]
fn mapped_path(line: &str) -> Option<PathBuf> {
    // address perms offset dev inode path — the path may contain spaces.
    let mut rest = line;
    for _ in 0..5 {
        rest = rest.trim_start().split_once(char::is_whitespace)?.1;
    }
    let path = rest.trim();
    let path = path.strip_suffix(" (deleted)").unwrap_or(path);
    path.starts_with('/').then(|| PathBuf::from(path))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn from_lsof(target: &Path) -> Result<Holders> {
    use anyhow::Context;
    // `+D` looks below a folder; a mount point is matched as a whole by
    // lsof itself, but `+D` covers plain folders too.
    let mut cmd = std::process::Command::new("lsof");
    cmd.args(["-n", "-P", "-w", "-F", "pcfn"]);
    if target.is_dir() {
        cmd.arg("+D");
    } else {
        cmd.arg("--");
    }
    let out = cmd.arg(target).stdin(std::process::Stdio::null()).output().context("cannot run lsof")?;
    // lsof exits 1 when it finds nothing.
    if !out.status.success() && !out.stdout.is_empty() {
        anyhow::bail!("lsof failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(Holders { found: parse_lsof(&String::from_utf8_lossy(&out.stdout), target), unreadable: 0 })
}

/// Parses `lsof -F pcfn` output: `p<pid>`, `c<command>`, then `f<fd>`
/// and `n<name>` pairs.
#[cfg(any(all(unix, not(target_os = "linux")), test))]
fn parse_lsof(text: &str, target: &Path) -> Vec<Holder> {
    let mut found: Vec<Holder> = Vec::new();
    let mut fd = String::new();
    for line in text.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => {
                if let Ok(pid) = value.parse() {
                    found.push(Holder { pid, command: String::new(), uses: Vec::new() });
                }
            }
            "c" => {
                if let Some(h) = found.last_mut() {
                    h.command = value.to_string();
                }
            }
            "f" => fd = value.to_string(),
            "n" => {
                let path = PathBuf::from(value.strip_suffix(" (deleted)").unwrap_or(value));
                let Some(h) = found.last_mut().filter(|_| within(&path, target)) else {
                    continue;
                };
                let usage = match fd.as_str() {
                    "cwd" => Use::Cwd(path),
                    "rtd" => Use::Root(path),
                    "txt" => Use::Exe(path),
                    "mem" => Use::Mapped(path),
                    _ => Use::Open(path),
                };
                if !h.uses.contains(&usage) {
                    h.uses.push(usage);
                }
            }
            _ => {}
        }
    }
    found.retain(|h| !h.uses.is_empty());
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_lines_give_their_file() {
        let line = "7f2a1c000000-7f2a1c021000 r--p 00000000 08:01 1234   /media/usb/lib/libfoo.so";
        assert_eq!(mapped_path(line), Some(PathBuf::from("/media/usb/lib/libfoo.so")));
        let spaced = "7f2a-7f2b r--p 00000000 08:01 99 /media/my stick/a b.so (deleted)";
        assert_eq!(mapped_path(spaced), Some(PathBuf::from("/media/my stick/a b.so")));
        assert_eq!(mapped_path("7ffd-7ffe rw-p 00000000 00:00 0                          [stack]"), None);
        assert_eq!(mapped_path("7ffd-7ffe rw-p 00000000 00:00 0"), None);
    }

    #[test]
    fn lsof_output_is_grouped_by_process() {
        let text = "p101\ncvim\nfcwd\nn/Volumes/USB\nf4\nn/Volumes/USB/notes.txt\nf5\nn/dev/ttys001\n\
                    p202\ncFinder\nf9\nn/Users/me/x\np303\ncmpv\nftxt\nn/Volumes/USB/mpv\nfmem\nn/Volumes/USB/lib.dylib\n";
        let found = parse_lsof(text, Path::new("/Volumes/USB"));
        assert_eq!(found.len(), 2, "Finder holds nothing there");
        assert_eq!((found[0].pid, found[0].command.as_str()), (101, "vim"));
        assert_eq!(found[0].uses, [Use::Cwd("/Volumes/USB".into()), Use::Open("/Volumes/USB/notes.txt".into())]);
        assert_eq!(found[1].uses, [Use::Exe("/Volumes/USB/mpv".into()), Use::Mapped("/Volumes/USB/lib.dylib".into())]);
        assert!(!within(Path::new("/Volumes/USB2/x"), Path::new("/Volumes/USB")), "whole components only");
    }

    #[cfg(unix)]
    #[test]
    fn processes_are_found_in_a_proc_tree() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let (proc, media) = (dir.path().join("proc"), dir.path().join("media"));
        std::fs::create_dir_all(&media).unwrap();
        // 10 works in the drive, 20 elsewhere, 30 belongs to someone else.
        for (pid, cwd, files) in [(10, media.clone(), vec![media.join("a.txt")]), (20, dir.path().into(), vec![])] {
            let p = proc.join(pid.to_string());
            std::fs::create_dir_all(p.join("fd")).unwrap();
            std::fs::write(p.join("comm"), format!("cmd{pid}\n")).unwrap();
            symlink(&cwd, p.join("cwd")).unwrap();
            symlink("/", p.join("root")).unwrap();
            for (i, f) in files.iter().enumerate() {
                symlink(format!("{} (deleted)", f.display()), p.join("fd").join(i.to_string())).unwrap();
            }
            let maps = format!("1-2 r-xp 0 08:01 7 {}\n3-4 rw-p 0 00:00 0 [heap]\n", media.join("lib.so").display());
            std::fs::write(p.join("maps"), if pid == 10 { maps } else { String::new() }).unwrap();
        }
        std::fs::create_dir_all(proc.join("30")).unwrap();
        std::fs::create_dir_all(proc.join("self-not-a-pid")).unwrap();

        let holders = from_proc(&proc, &media);
        assert_eq!(holders.unreadable, 1);
        assert_eq!(holders.found.len(), 1);
        let h = &holders.found[0];
        assert_eq!((h.pid, h.command.as_str()), (10, "cmd10"));
        assert_eq!(
            h.uses,
            [Use::Cwd(media.clone()), Use::Open(media.join("a.txt")), Use::Mapped(media.join("lib.so"))]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn finds_this_process_holding_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("held.txt");
        let _file = std::fs::File::create(&path).unwrap();
        let holders = find(dir.path()).unwrap();
        let me = holders.found.iter().find(|h| h.pid == std::process::id()).expect("this test holds the file");
        assert!(me.uses.contains(&Use::Open(path)));
    }
}
