//! Terminal setup, teardown and hand-over to external programs.

use std::io::{self, Stdout};
use std::process::{Command, ExitStatus};
use std::time::Duration;

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use ratatui_image::picker::Picker;

pub type Term = Terminal<CrosstermBackend<Stdout>>;

pub fn init() -> Result<Term> {
    // Restore the terminal even when we panic, so the shell stays usable.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        hook(info);
    }));
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(io::stdout()))?)
}

pub fn restore() -> Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

/// Detects the best image protocol (kitty, sixel, iTerm2) the terminal
/// supports, falling back to unicode half blocks.
///
/// The probe is only sent to terminals known to answer it: a terminal that
/// stays silent leaves the probe's reader thread waiting on stdin, where it
/// would swallow the user's first key press.
pub fn image_picker() -> Picker {
    if !graphics_capable_terminal() {
        return Picker::halfblocks();
    }
    let options = QueryStdioOptions { timeout: Duration::from_millis(600), ..Default::default() };
    Picker::from_query_stdio_with_options(options).unwrap_or_else(|_| Picker::halfblocks())
}

fn graphics_capable_terminal() -> bool {
    let var = |k: &str| std::env::var(k).unwrap_or_default().to_ascii_lowercase();
    if std::env::var_os("TMUX").is_some() || var("TERM").starts_with("screen") {
        return false;
    }
    let term = var("TERM");
    let program = var("TERM_PROGRAM");
    ["KITTY_WINDOW_ID", "WEZTERM_EXECUTABLE", "WT_SESSION", "KONSOLE_VERSION", "GHOSTTY_RESOURCES_DIR"]
        .iter()
        .any(|k| std::env::var_os(k).is_some())
        || ["kitty", "foot", "mlterm", "contour", "ghostty", "wezterm"].iter().any(|t| term.contains(t))
        || ["iterm.app", "wezterm", "ghostty", "vscode", "rio"].contains(&program.as_str())
}

/// Runs `cmd` with the terminal handed over, then takes it back.
pub fn run_external(terminal: &mut Term, cmd: &mut Command) -> Result<ExitStatus> {
    restore()?;
    let status = cmd.status();
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    terminal.clear()?;
    Ok(status?)
}

/// Like [`run_external`], writing `input` to the command's stdin when given.
pub fn run_external_with_input(terminal: &mut Term, cmd: &mut Command, input: Option<&str>) -> Result<ExitStatus> {
    let Some(input) = input else {
        return run_external(terminal, cmd);
    };
    restore()?;
    let status = (|| -> std::io::Result<ExitStatus> {
        let mut child = cmd.stdin(std::process::Stdio::piped()).spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(input.as_bytes())?;
        }
        child.wait()
    })();
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    terminal.clear()?;
    Ok(status?)
}

/// Like [`run_external`] but waits for Enter afterwards so output stays readable.
pub fn run_external_and_wait(terminal: &mut Term, cmd: &mut Command) -> Result<ExitStatus> {
    restore()?;
    let status = cmd.status();
    println!("\n[press enter to return to strata]");
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    terminal.clear()?;
    Ok(status?)
}
