//! The system clipboard: a native tool when strata runs locally, else the
//! terminal through OSC 52, wrapped so tmux and GNU screen pass it on.

use std::io::Write;
use std::process::{Command, Stdio};

use super::App;

/// A terminal multiplexer between strata and the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mux {
    None,
    Tmux,
    Screen,
}

impl App {
    /// Puts `text` on the system clipboard.
    pub(super) fn set_clipboard(&self, text: &str) {
        let configured = self.config.general.clipboard.trim();
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let mux = if var("TMUX").is_some() {
            Mux::Tmux
        } else if var("STY").is_some() {
            Mux::Screen
        } else {
            Mux::None
        };
        if configured != "osc52" {
            let tool = match configured {
                "" => native_tool(text, &var, &|p: &str| which(p)),
                command => Some(strata_core::util::split_command(command)),
            };
            if tool.is_some_and(|argv| pipe_to(&argv, text)) {
                return;
            }
            // tmux keeps a copy and hands it to the terminal itself, which
            // works whatever its passthrough setting.
            if mux == Mux::Tmux && run_to_end(&["tmux", "load-buffer", "-w", "-"], text) {
                return;
            }
        }
        let mut out = std::io::stdout();
        let _ = out.write_all(osc52(text, mux).as_bytes()).and_then(|_| out.flush());
    }
}

/// The clipboard program for this session, unless it runs over SSH (where
/// a local tool would fill the remote machine's clipboard).
fn native_tool(text: &str, var: &dyn Fn(&str) -> Option<String>, has: &dyn Fn(&str) -> bool) -> Option<Vec<String>> {
    if var("SSH_CONNECTION").is_some() || var("SSH_TTY").is_some() {
        return None;
    }
    let argv = |args: &[&str]| Some(args.iter().map(|a| a.to_string()).collect());
    if cfg!(target_os = "macos") && has("pbcopy") {
        return argv(&["pbcopy"]);
    }
    if var("WAYLAND_DISPLAY").is_some() && has("wl-copy") {
        return argv(&["wl-copy"]);
    }
    if var("DISPLAY").is_some() {
        if has("xclip") {
            return argv(&["xclip", "-selection", "clipboard"]);
        }
        if has("xsel") {
            return argv(&["xsel", "--clipboard", "--input"]);
        }
    }
    // clip.exe reads the console code page, garbling anything but ASCII.
    if (cfg!(windows) || var("WSL_DISTRO_NAME").is_some()) && text.is_ascii() && has("clip.exe") {
        return argv(&["clip.exe"]);
    }
    None
}

/// Writes `text` to the program's stdin and lets it finish in the
/// background (X11 and Wayland tools keep running to serve the paste).
/// Returns false when it could not be started.
fn pipe_to(argv: &[String], text: &str) -> bool {
    let Some((program, args)) = argv.split_first() else { return false };
    let spawned =
        Command::new(program).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    let Ok(mut child) = spawned else { return false };
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(text.as_bytes()).is_err() {
            let _ = child.kill();
            return false;
        }
    }
    std::thread::spawn(move || child.wait());
    true
}

/// Runs a quick program with `text` on stdin. Returns whether it succeeded.
fn run_to_end(argv: &[&str], text: &str) -> bool {
    let spawned = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = spawned else { return false };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().is_ok_and(|s| s.success())
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// The OSC 52 sequence setting the clipboard to `text`, wrapped in a DCS
/// passthrough for multiplexers that would otherwise swallow it.
fn osc52(text: &str, mux: Mux) -> String {
    let osc = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    match mux {
        Mux::None => osc,
        // Escapes inside the passthrough are doubled.
        Mux::Tmux => format!("\x1bPtmux;{}\x1b\\", osc.replace('\x1b', "\x1b\x1b")),
        // screen limits a DCS string to 768 bytes, so send it in pieces.
        Mux::Screen => {
            osc.as_bytes().chunks(76).map(|chunk| format!("\x1bP{}\x1b\\", String::from_utf8_lossy(chunk))).collect()
        }
    }
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_is_wrapped_for_multiplexers() {
        assert_eq!(osc52("hi", Mux::None), "\x1b]52;c;aGk=\x07");
        assert_eq!(osc52("hi", Mux::Tmux), "\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\");
        assert_eq!(osc52("hi", Mux::Screen), "\x1bP\x1b]52;c;aGk=\x07\x1b\\");
        let long = osc52(&"x".repeat(200), Mux::Screen);
        assert!(long.matches("\x1bP").count() > 1, "split into pieces");
        assert!(long.split("\x1bP").all(|piece| piece.len() <= 80));
    }

    #[test]
    fn native_tools_follow_the_session() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
        };
        let all = |_: &str| true;
        if !cfg!(target_os = "macos") {
            let wayland = env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")]);
            assert_eq!(native_tool("x", &wayland, &all).unwrap()[0], "wl-copy");
            let x11 = env(&[("DISPLAY", ":0")]);
            assert_eq!(native_tool("x", &x11, &|p| p == "xsel").unwrap()[0], "xsel");
            if !cfg!(windows) {
                assert_eq!(native_tool("x", &env(&[]), &all), None);
            }
        }
        let ssh = env(&[("SSH_CONNECTION", "1"), ("DISPLAY", ":0"), ("WAYLAND_DISPLAY", "w")]);
        assert_eq!(native_tool("x", &ssh, &all), None, "over SSH the terminal gets it");
        let wsl = env(&[("WSL_DISTRO_NAME", "Ubuntu")]);
        if !cfg!(target_os = "macos") {
            assert_eq!(native_tool("plain", &wsl, &all).unwrap()[0], "clip.exe");
            assert_eq!(native_tool("café", &wsl, &all), None, "clip.exe garbles UTF-8");
        }
    }
}
