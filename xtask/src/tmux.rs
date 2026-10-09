//! Runs strata in a private tmux server and captures the screen.

use std::process::Command;

use anyhow::{bail, Result};

const SOCKET: &str = "strata-media";

pub fn available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success())
}

pub struct Session {
    name: String,
}

impl Session {
    /// Starts `command` in a detached session of the given size.
    pub fn start(name: &str, width: u16, height: u16, env: &[(String, String)], command: &str) -> Result<Self> {
        let _ = tmux(&["kill-session", "-t", name]);
        let mut args: Vec<String> =
            ["new-session", "-d", "-s", name, "-x", &width.to_string(), "-y", &height.to_string()]
                .iter()
                .map(|s| s.to_string())
                .collect();
        for (k, v) in env {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        args.push(command.to_string());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        tmux(&refs)?;
        // Keep 24-bit colours in captures.
        let _ = tmux(&["set-option", "-t", name, "-g", "default-terminal", "tmux-256color"]);
        Ok(Self { name: name.to_string() })
    }

    /// Sends tmux key names (`j`, `Enter`, `C-g`...).
    pub fn keys(&self, keys: &[&str]) -> Result<()> {
        let mut args = vec!["send-keys", "-t", &self.name];
        args.extend_from_slice(keys);
        tmux(&args).map(drop)
    }

    /// Types literal text.
    pub fn text(&self, text: &str) -> Result<()> {
        tmux(&["send-keys", "-t", &self.name, "-l", text]).map(drop)
    }

    /// The screen with colours, as ANSI text.
    pub fn capture(&self) -> Result<String> {
        tmux(&["capture-pane", "-p", "-e", "-N", "-t", &self.name])
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = tmux(&["kill-session", "-t", &self.name]);
    }
}

fn tmux(args: &[&str]) -> Result<String> {
    let out = Command::new("tmux").args(["-L", SOCKET, "-f", "/dev/null"]).args(args).output()?;
    if !out.status.success() {
        bail!("tmux {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
