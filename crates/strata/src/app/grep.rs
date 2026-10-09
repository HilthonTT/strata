//! Content search with ripgrep (falling back to grep).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use strata_core::nas::which;
use strata_plugin::Level;

use super::external::{After, External, Wait};
use super::overlay::{Overlay, PickerPurpose, PickerState};
use super::App;
use crate::event::AppEvent;

const MAX_HITS: usize = 2000;

#[derive(Debug, Clone)]
pub struct GrepHit {
    /// Relative to the search root.
    pub path: PathBuf,
    pub line: u64,
    pub text: String,
}

pub struct GrepResult {
    pub hits: Vec<GrepHit>,
    pub truncated: bool,
    pub tool: &'static str,
}

/// Runs ripgrep (or grep) in `root`. Stops after [`MAX_HITS`] matches.
pub fn search(root: &Path, pattern: &str, hidden: bool, cancel: &AtomicBool) -> Result<GrepResult, String> {
    let (tool, mut cmd) = if which("rg") {
        let mut c = Command::new("rg");
        c.args(["--null", "--line-number", "--no-heading", "--color", "never", "--smart-case"]);
        c.args(["--max-columns", "300", "--max-columns-preview"]);
        if hidden {
            c.arg("--hidden");
        }
        c.args(["-e", pattern, "."]);
        ("ripgrep", c)
    } else if which("grep") {
        let mut c = Command::new("grep");
        // `--null`, not `-Z`: BSD grep (macOS) reads `-Z` as "decompress".
        c.args(["-rnI", "--null", "--exclude-dir=.git", "-e", pattern, "."]);
        ("grep", c)
    } else {
        return Err("install ripgrep (rg) to search file contents".into());
    };
    let mut child = cmd
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{tool}: {e}"))?;
    let stdout = child.stdout.take().ok_or("no output")?;
    let mut hits = Vec::new();
    let mut truncated = false;
    for line in BufReader::new(stdout).split(b'\n').map_while(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if let Some(hit) = parse_line(&line) {
            hits.push(hit);
        }
        if hits.len() >= MAX_HITS {
            truncated = true;
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(GrepResult { hits, truncated, tool })
}

/// Parses `path\0line:text` (the `--null` output of rg and grep).
fn parse_line(line: &[u8]) -> Option<GrepHit> {
    let nul = line.iter().position(|b| *b == 0)?;
    let path = String::from_utf8_lossy(&line[..nul]).into_owned();
    let rest = String::from_utf8_lossy(&line[nul + 1..]).into_owned();
    let (num, text) = rest.split_once(':')?;
    Some(GrepHit {
        path: PathBuf::from(path.trim_start_matches("./")),
        line: num.parse().ok()?,
        text: text.trim().replace('\t', " "),
    })
}

impl App {
    pub(super) fn start_content_search(&mut self, pattern: &str) {
        if !self.panel().vfs.is_local() {
            return self.notify("content search works on local directories", Level::Warn);
        }
        if pattern.is_empty() {
            return;
        }
        self.find_cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.find_cancel = cancel.clone();
        let root = self.panel().cwd.clone();
        let hidden = self.panel().show_hidden;
        let pattern = pattern.to_string();
        let tx = self.tx.clone();
        self.info(format!("searching for '{pattern}'…"));
        std::thread::spawn(move || {
            let result = search(&root, &pattern, hidden, &cancel);
            let _ = tx.send(AppEvent::Grep { root, pattern, result });
        });
    }

    pub(super) fn on_grep_results(&mut self, root: PathBuf, pattern: String, result: Result<GrepResult, String>) {
        let result = match result {
            Ok(r) => r,
            Err(e) => return self.error(e),
        };
        if result.hits.is_empty() {
            return self.notify(format!("no matches for '{pattern}'"), Level::Warn);
        }
        if self.overlay.is_some() {
            return;
        }
        let items = result
            .hits
            .iter()
            .map(|h| format!("{}:{}  {}", strata_core::util::posix(&h.path), h.line, h.text))
            .collect();
        let more = if result.truncated { "+" } else { "" };
        let title = format!("'{pattern}' · {}{more} matches ({})", result.hits.len(), result.tool);
        self.overlay =
            Some(Overlay::Picker(PickerState::new(title, items, PickerPurpose::Grep { root, hits: result.hits })));
    }

    /// Shows the file in the panel and opens it in the editor at the line.
    pub(super) fn open_hit(&mut self, root: &Path, hit: &GrepHit) {
        let path = root.join(&hit.path);
        self.reveal(path.clone());
        let mut argv = self.editor();
        let program =
            Path::new(&argv[0]).file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let file = path.to_string_lossy().into_owned();
        match program.as_str() {
            "hx" | "helix" | "zed" | "subl" => argv.push(format!("{file}:{}", hit.line)),
            "code" | "codium" | "code-insiders" | "cursor" => {
                argv.push("--goto".into());
                argv.push(format!("{file}:{}", hit.line));
            }
            "notepad" => argv.push(file),
            _ => {
                argv.push(format!("+{}", hit.line));
                argv.push(file);
            }
        }
        self.queue_external(External::Run {
            argv,
            cwd: Some(root.to_path_buf()),
            wait: Wait::OnFailure,
            after: After::Reload,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_null_separated_output() {
        let hit = parse_line(b"./src/main.rs\x0012:    let x = 1; // a:b").unwrap();
        assert_eq!(hit.path, PathBuf::from("src/main.rs"));
        assert_eq!(hit.line, 12);
        assert_eq!(hit.text, "let x = 1; // a:b");
        assert!(parse_line(b"no separator").is_none());
    }

    #[test]
    fn finds_matches_in_files() {
        if !which("rg") && !which("grep") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha\nneedle here\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "nothing").unwrap();
        let r = search(dir.path(), "needle", false, &AtomicBool::new(false)).unwrap();
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].path, PathBuf::from("a.txt"));
        assert_eq!(r.hits[0].line, 2);
    }
}
