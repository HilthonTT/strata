//! The man page printed by `strata --man`. Options, keys and palette
//! commands come from the code, so the page cannot drift from them.

use std::io::{self, Write};

use clap_mangen::roff::{bold, italic, roman, Inline, Roff};
use clap_mangen::Man;
use strata_config::Keymap;

use crate::app::COMMANDS;

pub fn render(cmd: clap::Command, w: &mut dyn Write) -> io::Result<()> {
    let man = Man::new(cmd.clone()).manual("strata manual");
    man.render_title(w)?;
    man.render_name_section(w)?;
    man.render_synopsis_section(w)?;

    let mut page = Roff::new();
    section(&mut page, "DESCRIPTION");
    paragraph(
        &mut page,
        "strata is a terminal file explorer with vim-style keys. It opens one panel per directory given \
         (the current one by default), with a preview beside the focused one.",
    );
    paragraph(
        &mut page,
        "Besides local folders it browses archives, SFTP, SMB and NFS shares, S3, WebDAV, FTP and rclone \
         remotes, and Docker containers. It has a disk usage view, a dashboard of disks, I/O and memory, \
         and a git menu. Lua plugins can add keys, commands, panels, previewers, columns and sort orders.",
    );
    page.to_writer(w)?;
    man.render_options_section(w)?;

    let mut page = Roff::new();
    keys(&mut page);
    palette(&mut page);
    files(&mut page);
    environment(&mut page);
    examples(&mut page);
    section(&mut page, "SEE ALSO");
    page.text([
        bold("git"),
        roman("(1), "),
        bold("rg"),
        roman("(1), "),
        bold("rclone"),
        roman("(1). Full documentation: https://github.com/HilthonTT/strata"),
    ]);
    page.to_writer(w)?;
    man.render_version_section(w)
}

fn section(page: &mut Roff, title: &str) {
    page.control("SH", [title]);
}

fn paragraph(page: &mut Roff, text: &str) {
    page.control("PP", []).text([roman(text)]);
}

/// A term in bold followed by its description, indented.
fn entry(page: &mut Roff, term: impl Into<Vec<Inline>>, text: &str) {
    page.control("TP", []).text(term).text([roman(text)]);
}

/// The default (vim) key bindings, as the `?` screen lists them.
fn keys(page: &mut Roff) {
    section(page, "KEYS");
    paragraph(
        page,
        "The default bindings (keymap = \"vim\"). Sequences such as g g are typed one key after the other. \
         Press ? in strata to see the active bindings, including those of plugins and the [keys] section \
         of config.toml.",
    );
    // Built-in actions first, then keys that run palette commands.
    let (commands, actions): (Vec<_>, Vec<_>) =
        Keymap::with_defaults().describe().into_iter().partition(|(_, desc)| desc.starts_with(':'));
    for (keys, desc) in actions.into_iter().chain(commands) {
        entry(page, [bold(keys)], &desc);
    }
}

/// The `:` command palette.
fn palette(page: &mut Roff) {
    section(page, "COMMANDS");
    paragraph(page, "Type : to open the command palette. Commands can also be run with -e and --remote.");
    for (name, desc) in COMMANDS {
        let (usage, text) = desc.split_once(" — ").unwrap_or((name, desc));
        entry(page, [bold(format!(":{usage}"))], text);
    }
}

fn files(page: &mut Roff) {
    section(page, "FILES");
    let rows = [
        ("~/.config/strata/config.toml", "The config file. strata --dump-config prints a documented one."),
        ("~/.config/strata/themes/", "Custom themes, one .toml file each."),
        ("~/.config/strata/plugins/", "Plugins: name.lua files and name/init.lua folders."),
        (
            "~/.local/share/strata/",
            "State: session.json, layouts.json, jumps.json (frecency), selections.json, and plugin data. \
             ~/Library/Application Support/strata on macOS, %APPDATA%\\strata on Windows.",
        ),
    ];
    for (path, text) in rows {
        entry(page, [italic(path)], text);
    }
    paragraph(page, "On Windows the config folder is %APPDATA%\\strata.");
}

fn environment(page: &mut Roff) {
    section(page, "ENVIRONMENT");
    let rows = [
        ("STRATA_CONFIG_DIR", "Use this folder instead of ~/.config/strata."),
        ("XDG_CONFIG_HOME", "When set, the config folder is $XDG_CONFIG_HOME/strata."),
        ("VISUAL, EDITOR", "The editor for e, E and bulk renames."),
        ("SHELL", "The shell that ! opens."),
        ("STRATA_INSTANCE", "The pid of the strata that --remote drives. strata sets it for the programs it runs."),
        ("XDG_RUNTIME_DIR", "Where the sockets for --remote live (else a private folder in the temp folder)."),
    ];
    for (name, text) in rows {
        entry(page, [bold(name)], text);
    }
}

fn examples(page: &mut Roff) {
    section(page, "EXAMPLES");
    let rows = [
        ("strata ~/src ~/Downloads", "Open two panels side by side."),
        ("strata -e 'select *.jpg' ~/Pictures", "Mark every JPEG once the panel is up."),
        ("strata --remote-query | jq -r '.marked[]'", "Print the items marked in the running strata."),
        ("eval \"$(strata --shell-init bash)\"", "In ~/.bashrc: let Q change the shell's directory."),
        ("strata --man > ~/.local/share/man/man1/strata.1", "Install this page."),
    ];
    for (cmd, text) in rows {
        entry(page, [bold(cmd)], text);
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    #[test]
    fn page_covers_options_and_keys() {
        let mut out = Vec::new();
        super::render(crate::Cli::command(), &mut out).unwrap();
        let page = String::from_utf8(out).unwrap();
        assert!(page.starts_with(".ie"), "{}", &page[..80]);
        for needle in [".TH strata 1", "dump\\-config", ".SH KEYS", ".SH COMMANDS"] {
            assert!(page.contains(needle), "missing {needle}");
        }
    }
}
