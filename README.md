<p align="center">
  <img src="website/public/logo.svg" width="96" alt="strata logo">
</p>

<h1 align="center">strata</h1>

<p align="center">A fast, extensible terminal file explorer written in Rust.</p>

![strata](website/public/media/overview.gif)

Everything you need, nothing you don't.

- **File operations**: copy, move, delete (to trash), rename, bulk rename in `$EDITOR` or with a pattern (`photo-{n:3}{.ext}`, `s/old/new/`) and a live preview, duplicate, symlinks and hard links, `chmod`/`chown`, and undo for all of them
- **Trash browser**: restore, delete or empty, newest first
- **Sync and dedupe**: make one folder match another (`:sync`, with a dry run and undo) and find duplicate files (`:dupes`)
- **Compare and verify**: diff two files or directory trees, show checksums (MD5, SHA-1, SHA-256, SHA-512) and check downloads against a hash or `SHA256SUMS`
- **Archives**: browse zip and tar (gz, bz2, xz, zst) archives like directories, extract them with `X` and create them with `:compress`
- **Folder sizes** in the file list (`d c`), measured in the background
- **Jobs you control**: pause copies (`alt+p`) and resume failed or cancelled ones where they stopped (`alt+R`)
- **Sessions**: reopen your tabs and folders on the next start (`restore_session = true`)
- **File picker** for editors and scripts: `strata --chooser-file <file>`
- **Search**: filter, fuzzy find, content search with ripgrep, and `:find` by name, size, age or type with the results listed in the panel; `:flatten` lists every file below a folder
- **Git status** markers next to every file, and **tabs** on top of multiple panels
- **Preview**: syntax-highlighted code, rendered Markdown, images, PDFs, video frames and cover art, Word/Excel/PowerPoint/OpenDocument/EPUB text, hex dumps, archives and directories; executable architecture and optional MD5/SHA-256 in the metadata pane
- **Multiple panels**: browse several directories side by side and copy between them with one key
- **NAS connections**: SMB, NFS and SFTP, with live reachability checks, a step-by-step connection test and passwords in your system keychain
- **Docker**: list, start, stop and inspect containers, and browse their filesystems
- **Dashboard**: disk free space, disk usage, I/O (IOPS, throughput, latency) and memory pressure (PSI)
- **28 themes**: a default `strata` theme, Catppuccin, Nord, Tokyo Night, Dracula, Gruvbox, Rose Pine and more, plus your own
- **Lua plugins**: add keys, commands, panels, status-line segments and previewers
- **Vim-style keys**, all remappable

## Install

```sh
git clone https://github.com/HilthonTT/strata && cd strata
make install            # installs to ~/.local/bin
```

You need Rust 1.95 or newer. On Linux and macOS you also need OpenSSL headers for SFTP (`libssl-dev` on Debian/Ubuntu). Icons need a [Nerd Font](https://www.nerdfonts.com); set `icons = false` if you don't use one.

## Usage

```sh
strata [DIR...]           # one panel per directory
```

| Key | Action | Key | Action |
| --- | --- | --- | --- |
| `h j k l` | navigate | `space` / `v` | mark / visual select |
| `y y` `x` `p` | copy, cut, paste | `c` / `m` | copy / move to the next panel |
| `d d` / `D` | trash / delete | `r` / `R` | rename / bulk rename |
| `a` / `A` | new file / directory | `/` / `f` | filter / fuzzy find |
| `tab` / `n` | next / new panel | `1`–`4` | files, dashboard, Docker, NAS |
| `T` | theme picker | `:` | command palette |
| `u` / `ctrl+r` | undo / redo | `ctrl+g` | search file contents |
| `alt+j` / `alt+k` | scroll the preview | `alt+/` | search the preview |
| `t` | new tab | `g t` | next tab |
| `s` | sort menu | `E` | open the directory in your editor |
| `Y` / `=` | duplicate / permissions | `C` / `#` | compare / checksums |
| `U` | browse the trash | `g l` | paste as symlink |
| `d c` | folder sizes | `X` | extract the archive |
| `alt+p` | pause / resume the latest job | `alt+R` | resume a failed copy |
| `y d` | copy the current directory's path | `Q` | quit and `cd` your shell there |
| `g s` | find by size, age, type | `g a` | flatten / back |
| `g r` | rename with a pattern | | |
| `?` | all keys | `q` | quit |

Prefer superfile's keys (`ctrl+c`, `ctrl+x`, `ctrl+v`, `ctrl+d`…)? Set `keymap = "default"` under `[general]`.

To let `Q` change your shell's directory, add the wrapper to your shell config:

```sh
eval "$(strata --shell-init bash)"     # ~/.bashrc  (zsh: --shell-init zsh in ~/.zshrc)
strata --shell-init fish | source       # ~/.config/fish/config.fish
Invoke-Expression (& strata --shell-init powershell | Out-String)   # $PROFILE
```

## Configuration

Run `strata --dump-config > ~/.config/strata/config.toml` to start from a documented config. In it you can set the theme and key preset, remap keys, choose programs per file type (`[open_with]`), enable plugins and add NAS connections. To make a custom theme, add a `.toml` file in `~/.config/strata/themes/`.

## Plugins

Plugins are Lua files in `~/.config/strata/plugins/`. strata ships four official ones: `git`, `bookmarks`, `archive` (previews of `.7z` and `.rar` archives) and `zoxide`. See [docs/plugins.md](docs/plugins.md) for the API.

## Documentation

Full docs, screenshots and GIFs live in [`website/`](website), a Next.js site deployed to GitHub Pages. Run `make website-dev` to preview it locally.

## Development

```sh
make check    # fmt, clippy, tests (what CI runs)
make run
```

The code is a Cargo workspace:

| Crate | Purpose |
| --- | --- |
| `strata-core` | filesystem backends (local, SFTP, Docker), file operations, undo, jobs, search, git, NAS, keychain |
| `strata-sys` | disks, I/O, memory pressure, disk usage, Docker CLI |
| `strata-config` | config file, keymap, themes |
| `strata-plugin` | Lua plugin host |
| `strata` | the TUI |
| `xtask` | dev tasks: `cargo xtask media` records the docs screenshots and GIFs |

## License

MIT
