//! File operations refused for lack of permission, run again as root
//! through `sudo` (or another program such as `doas` or `run0`).
//!
//! strata itself never runs as root: the operation is turned into plain
//! `cp`, `mv`, `rm`, `chmod` or `chown` calls that the elevating program
//! runs in the terminal, where it can ask for a password.

use std::path::PathBuf;

use crate::ops::TransferMode;

/// A failed operation, ready to be run again as root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Privileged {
    /// `(source, destination)` of each top-level item still to transfer.
    Transfer {
        mode: TransferMode,
        items: Vec<(PathBuf, PathBuf)>,
    },
    /// Deleted for good: root's trash is not the user's.
    Delete(Vec<PathBuf>),
    Chmod {
        mode: String,
        recursive: bool,
        paths: Vec<PathBuf>,
    },
    Chown {
        owner: String,
        recursive: bool,
        paths: Vec<PathBuf>,
    },
}

/// Copies or moves each `source destination` pair. A directory whose
/// destination exists (from an earlier, interrupted attempt) is merged
/// into it rather than nested inside it.
const TRANSFER_SCRIPT: &str = r#"set -e
mode=$1; shift
while [ $# -ge 2 ]; do
  if [ "$mode" = mv ] && [ ! -e "$2" ] && [ ! -L "$2" ]; then
    mv -- "$1" "$2"
  elif [ -d "$1" ] && [ ! -L "$1" ]; then
    mkdir -p -- "$2"
    cp -a -- "$1/." "$2"
    if [ "$mode" = mv ]; then rm -rf -- "$1"; fi
  else
    cp -a -- "$1" "$2"
    if [ "$mode" = mv ]; then rm -f -- "$1"; fi
  fi
  shift 2
done"#;

impl Privileged {
    /// The command line that runs the operation through `elevate`
    /// (e.g. `["sudo"]`).
    pub fn argv(&self, elevate: &[String]) -> Vec<String> {
        let mut argv: Vec<String> = elevate.to_vec();
        let paths = |argv: &mut Vec<String>, paths: &[PathBuf]| {
            argv.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
        };
        match self {
            Privileged::Transfer { mode, items } => {
                let mode = if *mode == TransferMode::Move { "mv" } else { "cp" };
                argv.extend(["sh", "-c", TRANSFER_SCRIPT, "strata", mode].map(String::from));
                for (src, dst) in items {
                    paths(&mut argv, &[src.clone(), dst.clone()]);
                }
            }
            Privileged::Delete(targets) => {
                argv.extend(["rm", "-rf", "--"].map(String::from));
                paths(&mut argv, targets);
            }
            Privileged::Chmod { mode: spec, recursive, paths: targets }
            | Privileged::Chown { owner: spec, recursive, paths: targets } => {
                let program = if matches!(self, Privileged::Chmod { .. }) { "chmod" } else { "chown" };
                argv.push(program.into());
                if *recursive {
                    argv.push("-R".into());
                }
                argv.extend(["--".into(), spec.clone()]);
                paths(&mut argv, targets);
            }
        }
        argv
    }

    /// What will happen, for a confirmation prompt: `copy 3 items`.
    pub fn describe(&self) -> String {
        let count = |n: usize, one: &std::path::Path| match n {
            1 => format!("'{}'", one.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()),
            n => format!("{n} items"),
        };
        match self {
            Privileged::Transfer { mode, items } => {
                let verb = if *mode == TransferMode::Move { "move" } else { "copy" };
                let what = count(items.len(), &items[0].0);
                match items[0].1.parent() {
                    Some(dir) => format!("{verb} {what} to {}", dir.display()),
                    None => format!("{verb} {what}"),
                }
            }
            Privileged::Delete(paths) => format!("permanently delete {}", count(paths.len(), &paths[0])),
            Privileged::Chmod { mode, paths, .. } => format!("chmod {mode} {}", count(paths.len(), &paths[0])),
            Privileged::Chown { owner, paths, .. } => format!("chown {owner} {}", count(paths.len(), &paths[0])),
        }
    }

    /// Nothing left to do.
    pub fn is_empty(&self) -> bool {
        match self {
            Privileged::Transfer { items, .. } => items.is_empty(),
            Privileged::Delete(paths) | Privileged::Chmod { paths, .. } | Privileged::Chown { paths, .. } => {
                paths.is_empty()
            }
        }
    }
}

/// Whether a job's error message means the user lacked permission.
pub fn is_permission_error(message: &str) -> bool {
    let message = message.to_lowercase();
    ["permission denied", "operation not permitted", "os error 13)", "os error 1)", "access is denied"]
        .iter()
        .any(|needle| message.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sudo() -> Vec<String> {
        vec!["sudo".into()]
    }

    #[test]
    fn permission_errors_are_recognised() {
        assert!(is_permission_error("copying a.txt: Permission denied (os error 13)"));
        assert!(is_permission_error("chmod /etc/x: Operation not permitted (os error 1)"));
        assert!(!is_permission_error("No such file or directory (os error 2)"));
        assert!(!is_permission_error("No space left on device (os error 28)"));
    }

    #[test]
    fn chmod_and_chown_keep_modes_that_look_like_flags() {
        let chmod = Privileged::Chmod { mode: "-w".into(), recursive: true, paths: vec!["/srv/a".into()] };
        assert_eq!(chmod.argv(&sudo()), ["sudo", "chmod", "-R", "--", "-w", "/srv/a"]);
        let chown = Privileged::Chown { owner: "me:staff".into(), recursive: false, paths: vec!["/srv/a".into()] };
        assert_eq!(chown.argv(&sudo()), ["sudo", "chown", "--", "me:staff", "/srv/a"]);
    }

    #[test]
    fn deletes_are_permanent() {
        let delete = Privileged::Delete(vec!["/srv/a b".into()]);
        assert_eq!(delete.argv(&sudo()), ["sudo", "rm", "-rf", "--", "/srv/a b"]);
        assert_eq!(delete.describe(), "permanently delete 'a b'");
    }

    /// The errors strata's own operations report when refused are the ones
    /// `:sudo` is offered for.
    #[cfg(unix)]
    #[test]
    fn refused_operations_are_recognised() {
        use crate::jobs::Progress;
        use crate::ops::{self, Conflict, Transfer};
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("inside.txt"), "x").unwrap();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, "f").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::write(locked.join("probe"), "").is_ok() {
            return; // running as root: nothing is refused
        }
        let vfs: crate::VfsRef = Arc::new(crate::vfs::LocalVfs);
        let copy = Transfer {
            mode: TransferMode::Copy,
            src: vfs.clone(),
            sources: vec![file],
            dst: vfs.clone(),
            dest_dir: locked.clone(),
            conflict: Conflict::KeepBoth,
            resolutions: Default::default(),
            targets: Default::default(),
        };
        let err = format!("{:#}", copy.run(&Progress::default()).unwrap_err());
        assert!(is_permission_error(&err), "{err}");
        let err = ops::delete(&vfs, &[locked.join("inside.txt")], false, &Progress::default()).unwrap_err();
        assert!(is_permission_error(&format!("{err:#}")), "{err:#}");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_transfer_script_copies_moves_and_merges() {
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("tree/sub")).unwrap();
        fs::write(root.join("tree/sub/a.txt"), "a").unwrap();
        fs::write(root.join("file.txt"), "f").unwrap();
        // A destination left half-done by the failed attempt.
        fs::create_dir_all(root.join("out/tree")).unwrap();
        fs::write(root.join("out/tree/partial.txt"), "p").unwrap();

        let run = |plan: Privileged| {
            let argv = plan.argv(&[]);
            let status = std::process::Command::new(&argv[0]).args(&argv[1..]).status().unwrap();
            assert!(status.success());
        };
        run(Privileged::Transfer {
            mode: TransferMode::Copy,
            items: vec![(root.join("tree"), root.join("out/tree")), (root.join("file.txt"), root.join("out/f.txt"))],
        });
        assert_eq!(fs::read_to_string(root.join("out/tree/sub/a.txt")).unwrap(), "a");
        assert!(root.join("out/tree/partial.txt").exists(), "merged, not replaced");
        assert!(!root.join("out/tree/tree").exists(), "not nested");
        assert_eq!(fs::read_to_string(root.join("out/f.txt")).unwrap(), "f");

        run(Privileged::Transfer {
            mode: TransferMode::Move,
            items: vec![(root.join("tree"), root.join("out/tree")), (root.join("file.txt"), root.join("moved.txt"))],
        });
        assert!(!root.join("tree").exists() && !root.join("file.txt").exists());
        assert_eq!(fs::read_to_string(root.join("moved.txt")).unwrap(), "f");
    }
}
