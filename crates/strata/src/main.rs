//! strata — a fast, extensible terminal file explorer.

mod app;
mod event;
mod remote;
mod tui;
mod ui;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use strata_config::{Config, ThemeRegistry};

#[derive(Parser, Debug)]
#[command(name = "strata", version, about = "A fast, extensible terminal file explorer")]
struct Cli {
    /// Directories to open, one panel each.
    paths: Vec<PathBuf>,
    /// Use this config file instead of the default.
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// Override the configured theme.
    #[arg(short, long)]
    theme: Option<String>,
    /// Start without loading any plugins.
    #[arg(long)]
    no_plugins: bool,
    /// Print the available themes and exit.
    #[arg(long)]
    list_themes: bool,
    /// Print a documented default config and exit.
    #[arg(long)]
    dump_config: bool,
    /// Write the last directory to this file when quitting with `quit_cd`
    /// (used by the `--shell-init` wrapper).
    #[arg(long, value_name = "FILE")]
    cwd_file: Option<PathBuf>,
    /// Picker mode for editors and scripts: opening a file writes the
    /// chosen paths to this file, one per line, and quits.
    #[arg(long, value_name = "FILE")]
    chooser_file: Option<PathBuf>,
    /// Open the tabs and panels saved with `:layout save <name>`.
    #[arg(short, long, value_name = "NAME", conflicts_with = "paths")]
    layout: Option<String>,
    /// Print a shell function that lets `Q` change your shell's directory.
    #[arg(long, value_name = "SHELL")]
    shell_init: Option<Shell>,
    /// Run a command-palette command once the panels are up, e.g.
    /// `-e 'select *.rs'`. Repeat to run several, in order.
    #[arg(short = 'e', long = "command", value_name = "CMD")]
    commands: Vec<String>,
    /// Send a command to a running strata and exit; nonzero if it failed.
    /// Repeat to send several, in order.
    #[arg(long, value_name = "CMD")]
    remote: Vec<String>,
    /// Print the running strata's directory, hovered and marked items as
    /// JSON (after any `--remote` commands).
    #[arg(long)]
    remote_query: bool,
    /// The pid of the strata `--remote` drives. Default: `$STRATA_INSTANCE`,
    /// else the most recently started one.
    #[arg(long, value_name = "PID")]
    instance: Option<u32>,
    /// Print the pid and directory of each running strata and exit.
    #[arg(long)]
    list_instances: bool,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

impl Shell {
    fn init_script(self) -> &'static str {
        match self {
            Shell::Bash | Shell::Zsh => {
                r#"strata() {
  local tmp dir
  tmp="$(mktemp -t strata-cwd.XXXXXX)" || return
  command strata --cwd-file="$tmp" "$@"
  dir="$(cat -- "$tmp" 2>/dev/null)"
  rm -f -- "$tmp"
  if [ -n "$dir" ] && [ "$dir" != "$PWD" ]; then
    cd -- "$dir" || return
  fi
}
"#
            }
            Shell::Fish => {
                r#"function strata
    set -l tmp (mktemp -t strata-cwd.XXXXXX); or return
    command strata --cwd-file=$tmp $argv
    set -l dir (cat -- $tmp 2>/dev/null)
    rm -f -- $tmp
    if test -n "$dir"; and test "$dir" != "$PWD"
        cd -- $dir
    end
end
"#
            }
            Shell::Powershell => {
                r#"function strata {
    $tmp = [System.IO.Path]::GetTempFileName()
    $exe = (Get-Command strata -CommandType Application | Select-Object -First 1).Source
    & $exe --cwd-file $tmp @args
    $dir = Get-Content -LiteralPath $tmp -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $tmp -ErrorAction SilentlyContinue
    if ($dir -and $dir -ne $PWD.Path) { Set-Location -LiteralPath $dir }
}
"#
            }
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(shell) = cli.shell_init {
        print!("{}", shell.init_script());
        return Ok(());
    }
    if cli.dump_config {
        print!("{}", Config::template());
        return Ok(());
    }
    if !cli.remote.is_empty() || cli.remote_query || cli.list_instances {
        std::process::exit(remote::run_client(&cli.remote, cli.remote_query, cli.list_instances, cli.instance));
    }
    if cli.list_themes {
        let (themes, _) = ThemeRegistry::load(&strata_config::config_dir().join("themes"));
        themes.names().iter().for_each(|n| println!("{n}"));
        return Ok(());
    }

    let mut config = Config::load(cli.config.as_deref()).context("failed to load config")?;
    if let Some(theme) = cli.theme {
        config.general.theme = theme;
    }
    // Before any thread starts: programs strata runs inherit it, so
    // `strata --remote` from them drives this instance.
    if config.general.remote_control && cfg!(unix) {
        std::env::set_var(remote::INSTANCE_ENV, std::process::id().to_string());
    } else {
        std::env::remove_var(remote::INSTANCE_ENV);
    }
    let options = app::StartOptions {
        paths: cli.paths,
        config_path: cli.config.unwrap_or_else(Config::default_path),
        load_plugins: !cli.no_plugins,
        chooser: cli.chooser_file.is_some(),
        layout: cli.layout,
        commands: cli.commands,
    };
    if let Some(name) = &options.layout {
        app::check_layout(name)?;
    }

    let mut terminal = tui::init()?;
    let picker =
        if config.general.image_preview { tui::image_picker() } else { ratatui_image::picker::Picker::halfblocks() };
    let result = app::App::new(config, options, picker).and_then(|mut app| {
        app.run(&mut terminal)?;
        // Both are attempted; the first failure is reported.
        let saved = app.save_jumps().and(app.save_session());
        Ok((app.last_dir(), std::mem::take(&mut app.chosen), saved))
    });
    tui::restore()?;

    let (last_dir, chosen, saved) = result?;
    if let Err(e) = saved {
        eprintln!("strata: {e:#}");
    }
    if let Some(file) = cli.chooser_file.filter(|_| !chosen.is_empty()) {
        let text: String = chosen.iter().map(|p| format!("{}\n", p.display())).collect();
        std::fs::write(&file, text).with_context(|| format!("writing {}", file.display()))?;
    }
    if let (Some(file), Some(dir)) = (cli.cwd_file, last_dir) {
        std::fs::write(file, dir.to_string_lossy().as_bytes())?;
    }
    Ok(())
}
