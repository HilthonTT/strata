//! What is holding a file or folder open (`:holders`), and the report shown
//! when a drive or share won't unmount because it is busy.

use std::path::{Path, PathBuf};

use strata_plugin::Level;
use strata_sys::holders::{self, Holders};

use super::overlay::{Overlay, TextPopup};
use super::{short_path, App};
use crate::event::AppEvent;

impl App {
    /// `:holders [path]`: processes using the path (the hovered item by
    /// default), or anything inside it.
    pub(super) fn holders_command(&mut self, args: &str) {
        let target = match args.trim() {
            "" => match self.panel().hovered() {
                Some(entry) if self.panel().vfs.is_local() => entry.path.clone(),
                Some(_) => return self.notify("only local files can be checked", Level::Warn),
                None => self.panel().cwd.clone(),
            },
            arg => {
                let path = strata_core::util::expand_tilde(arg);
                if path.is_absolute() {
                    path
                } else {
                    self.panel().cwd.join(path)
                }
            }
        };
        self.info(format!("looking for what uses {}…", short_path(&target)));
        self.find_holders(target, None);
    }

    /// Looks up what uses `target` in the background and shows it. With a
    /// `failure` (an unmount that failed), nothing is shown when nothing
    /// is found: the failure was then about something else.
    pub(super) fn find_holders(&mut self, target: PathBuf, failure: Option<String>) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = holders::find(&target);
            let _ = tx.send(AppEvent::Then(Box::new(move |app: &mut App| match result {
                Ok(found) if found.found.is_empty() && failure.is_some() => {}
                Ok(found) => app.show_overlay(Overlay::Text(holders_popup(&target, &found, failure.as_deref()))),
                Err(e) => app.error(format!("holders: {e:#}")),
            })));
        });
    }
}

/// The report: one line per use, paths relative to `target`.
fn holders_popup(target: &Path, holders: &Holders, failure: Option<&str>) -> TextPopup {
    let shown = short_path(target);
    let mut lines = Vec::new();
    if let Some(failure) = failure {
        lines.push(format!("{failure}: these programs are still using it."));
        lines.push("Close them (or leave the folder in their terminal), then try again.".into());
        lines.push(String::new());
    }
    if holders.found.is_empty() {
        lines.push(format!("Nothing you can see has {shown} open."));
    } else {
        let width = holders.found.iter().map(|h| h.command.chars().count()).max().unwrap_or(0).max(7);
        lines.push(format!("{:>7}  {:<width$}  {:<7}  PATH", "PID", "PROGRAM", "USE"));
        let me = std::process::id();
        for holder in &holders.found {
            let command =
                if holder.pid == me { format!("{} (strata)", holder.command) } else { holder.command.clone() };
            for (i, used) in holder.uses.iter().enumerate() {
                let (pid, command) =
                    if i == 0 { (holder.pid.to_string(), command.as_str()) } else { (String::new(), "") };
                let path = match used.path().strip_prefix(target) {
                    Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
                    Ok(rel) => strata_core::util::posix(rel),
                    Err(_) => short_path(used.path()),
                };
                lines.push(format!("{pid:>7}  {command:<width$}  {:<7}  {path}", used.kind()));
            }
        }
    }
    if holders.unreadable > 0 {
        lines.push(String::new());
        lines.push(format!(
            "{} process(es) of other users could not be checked; run strata as root to see them too.",
            holders.unreadable
        ));
    }
    let title = if failure.is_some() { format!("Busy: {shown}") } else { format!("In use: {shown}") };
    TextPopup { title, lines, scroll: 0, colored: false }
}

/// Moves strata's own working directory (where it was started) out of a
/// mount about to be unmounted: it would keep the mount busy too.
pub fn leave(mount: &Path, home: &Path) {
    if std::env::current_dir().is_ok_and(|cwd| cwd.starts_with(mount)) {
        let _ = std::env::set_current_dir(home);
    }
}

/// A job error that means something still has the file or device open.
pub fn is_busy_error(message: &str) -> bool {
    let message = message.to_lowercase();
    ["resource busy", "text file busy", "os error 16)", "os error 26)", "being used by another process"]
        .iter()
        .any(|needle| message.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use strata_sys::holders::{Holder, Use};

    #[test]
    fn busy_errors_are_recognised() {
        assert!(is_busy_error("cannot remove /media/usb: Device or resource busy (os error 16)"));
        assert!(is_busy_error("Text file busy (os error 26)"));
        assert!(is_busy_error("The process cannot access the file because it is being used by another process."));
        assert!(!is_busy_error("Permission denied (os error 13)"));
    }

    #[test]
    fn the_report_lists_each_use_relative_to_the_target() {
        let target = Path::new("/media/usb");
        let holders = Holders {
            found: vec![
                Holder {
                    pid: 41,
                    command: "vim".into(),
                    uses: vec![Use::Cwd(target.into()), Use::Open(target.join("notes/a.txt"))],
                },
                Holder { pid: 7, command: "mpv".into(), uses: vec![Use::Mapped(target.join("lib.so"))] },
            ],
            unreadable: 2,
        };
        let popup = holders_popup(target, &holders, Some("cannot eject USB"));
        assert_eq!(popup.title, "Busy: /media/usb");
        assert_eq!(popup.lines[0], "cannot eject USB: these programs are still using it.");
        let table: Vec<&str> = popup.lines[3..7].iter().map(String::as_str).collect();
        assert_eq!(
            table,
            [
                "    PID  PROGRAM  USE      PATH",
                "     41  vim      cwd      .",
                "                  open     notes/a.txt",
                "      7  mpv      mapped   lib.so",
            ]
        );
        assert!(popup.lines.last().unwrap().starts_with("2 process(es) of other users"));

        let empty = holders_popup(target, &Holders::default(), None);
        assert_eq!(empty.title, "In use: /media/usb");
        assert_eq!(empty.lines, ["Nothing you can see has /media/usb open."]);
    }
}
