use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use strata_core::nas::Connection;
use strata_core::sort::SortKey;

use crate::keymap::KeymapPreset;

/// The whole `config.toml`. Every field is optional in the file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub general: General,
    /// Directories shown in the sidebar's "Pinned" section.
    pub pinned: Vec<String>,
    /// Key overrides: `"g p" = ":cd ~/projects"` or `"ctrl+c" = "copy"`.
    pub keys: HashMap<String, String>,
    pub plugins: PluginsConfig,
    /// Saved NAS connections (SMB, NFS, SFTP).
    pub connections: Vec<Connection>,
    /// Programs per file extension: `png = "feh"` or
    /// `pdf = { command = "zathura", detach = true }`.
    pub open_with: HashMap<String, OpenRule>,
}

/// How to open files with a given extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpenRule {
    /// Runs in the terminal; strata waits until it exits.
    Command(String),
    Detailed {
        command: String,
        /// Start in the background (GUI apps) instead of handing over the terminal.
        #[serde(default)]
        detach: bool,
    },
}

impl OpenRule {
    pub fn command(&self) -> &str {
        match self {
            Self::Command(c) | Self::Detailed { command: c, .. } => c,
        }
    }

    pub fn detach(&self) -> bool {
        matches!(self, Self::Detailed { detach: true, .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BorderStyle {
    Plain,
    #[default]
    Rounded,
    Double,
    Thick,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    pub theme: String,
    /// Key layout: `vim` (`y y`, `d d`, `g g`...) or `default`
    /// (`ctrl+c`, `ctrl+x`, `ctrl+v`... like superfile).
    pub keymap: KeymapPreset,
    /// Use the terminal's own background instead of the theme's.
    pub transparent: bool,
    pub show_hidden: bool,
    /// Number of file panels opened at start.
    pub panels: usize,
    pub preview: bool,
    pub image_preview: bool,
    /// Colour code in the preview.
    pub syntax_highlight: bool,
    /// Show git status markers next to files in repositories.
    pub git_status: bool,
    /// Columns after the file name: 0 = name only, 1 = size, 2 = size and date.
    pub extra_columns: u8,
    /// Draw a border around the preview.
    pub preview_border: bool,
    /// Line numbers in text previews.
    pub line_numbers: bool,
    pub sidebar: bool,
    pub footer: bool,
    /// Nerd Font file icons.
    pub icons: bool,
    pub border: BorderStyle,
    pub sort: SortKey,
    pub dirs_first: bool,
    pub use_trash: bool,
    pub confirm_delete: bool,
    /// Falls back to `$VISUAL`, `$EDITOR`, then `vi`.
    pub editor: String,
    /// Editor for `edit_dir` (E). Falls back to `editor`.
    pub dir_editor: String,
    /// Falls back to `xdg-open`, `open` or `start`.
    pub opener: String,
    /// Change the shell's directory on every quit, not only with `quit_cd`.
    /// Needs the shell function from `strata --shell-init <shell>`.
    pub cd_on_quit: bool,
    /// Show the MD5 checksum of the hovered file (reads the whole file).
    pub md5_checksum: bool,
    /// Show the SHA-256 checksum of the hovered file (reads the whole file).
    pub sha256_checksum: bool,
    /// Render Markdown files in the preview instead of showing the source.
    pub markdown_preview: bool,
    /// Hex dump of binary files in the preview.
    pub hex_preview: bool,
    /// PDF pages, video frames, cover art and media details in the preview
    /// (with poppler-utils, ffmpeg or mediainfo installed).
    pub media_preview: bool,
    /// Enter zip and tar archives like directories (read-only).
    pub browse_archives: bool,
    /// Program `compare` runs on two local files, e.g. `nvim -d` or `meld`.
    /// Empty shows strata's own diff.
    pub diff_tool: String,
    pub date_format: String,
    pub metrics_interval_ms: u64,
}

impl Default for General {
    fn default() -> Self {
        Self {
            theme: "catppuccin-mocha".into(),
            keymap: KeymapPreset::Vim,
            transparent: false,
            show_hidden: false,
            panels: 2,
            preview: true,
            image_preview: true,
            syntax_highlight: true,
            git_status: true,
            extra_columns: 0,
            preview_border: false,
            line_numbers: false,
            sidebar: true,
            footer: true,
            icons: true,
            border: BorderStyle::Rounded,
            sort: SortKey::Name,
            dirs_first: true,
            use_trash: true,
            confirm_delete: true,
            editor: String::new(),
            dir_editor: String::new(),
            opener: String::new(),
            cd_on_quit: false,
            md5_checksum: false,
            sha256_checksum: false,
            markdown_preview: true,
            hex_preview: true,
            media_preview: true,
            browse_archives: true,
            diff_tool: String::new(),
            date_format: "%Y-%m-%d %H:%M".into(),
            metrics_interval_ms: 1000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginsConfig {
    /// Official plugins to load.
    pub enabled: Vec<String>,
    /// User plugins (from the plugins directory) to skip.
    pub disabled: Vec<String>,
    /// Options passed to each plugin's `setup(opts)`.
    pub options: HashMap<String, toml::Value>,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            enabled: vec!["git".into(), "bookmarks".into(), "archive".into(), "zoxide".into()],
            disabled: Vec::new(),
            options: HashMap::new(),
        }
    }
}

impl Config {
    /// Loads `path` (or the default location). A missing file yields defaults.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = path.map(Path::to_path_buf).unwrap_or_else(Self::default_path);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("in {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    pub fn default_path() -> PathBuf {
        config_dir().join("config.toml")
    }

    /// Appends a connection to the config file, preserving the rest of it.
    pub fn append_connection(path: &Path, conn: &Connection) -> Result<()> {
        #[derive(Serialize)]
        struct Wrapper<'a> {
            connections: [&'a Connection; 1],
        }
        let snippet = toml::to_string(&Wrapper { connections: [conn] })?;
        let mut text = std::fs::read_to_string(path).unwrap_or_default();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
        text.push_str(&snippet);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)?;
        Ok(())
    }

    /// A documented starting config, printed by `strata --dump-config`.
    pub fn template() -> &'static str {
        include_str!("../default-config.toml")
    }
}

/// `$STRATA_CONFIG_DIR`, `$XDG_CONFIG_HOME/strata` or `~/.config/strata`
/// (`%APPDATA%\strata` on Windows).
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("STRATA_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("strata");
    }
    if cfg!(windows) {
        return dirs::config_dir().unwrap_or_default().join("strata");
    }
    dirs::home_dir().unwrap_or_default().join(".config").join("strata")
}

/// Where strata and its plugins keep state (bookmarks, pins...).
pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("strata")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses() {
        let c = Config::parse(Config::template()).unwrap();
        assert_eq!(c.general.theme, "catppuccin-mocha");
    }

    #[test]
    fn partial_config_keeps_defaults() {
        let c = Config::parse("[general]\ntheme = \"nord\"\n[[connections]]\nname=\"nas\"\nprotocol=\"smb\"\nhost=\"10.0.0.2\"\nshare=\"media\"").unwrap();
        assert_eq!(c.general.theme, "nord");
        assert_eq!(c.general.panels, 2);
        assert_eq!(c.connections[0].port(), 445);
    }

    #[test]
    fn open_rules_accept_both_forms() {
        let c = Config::parse(
            "[general]\nkeymap = \"default\"\n[open_with]\npng = \"feh\"\npdf = { command = \"zathura\", detach = true }",
        )
        .unwrap();
        assert_eq!(c.general.keymap, KeymapPreset::Default);
        assert_eq!(c.open_with["png"], OpenRule::Command("feh".into()));
        assert!(c.open_with["pdf"].detach());
        assert_eq!(c.open_with["pdf"].command(), "zathura");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("[general]\nthme = \"nord\"").is_err());
    }

    #[test]
    fn appends_connections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[general]\ntheme = \"nord\"").unwrap();
        let conn = Connection::from_url("nas", "sftp://me@nas.local/volume1").unwrap();
        Config::append_connection(&path, &conn).unwrap();
        let c = Config::load(Some(&path)).unwrap();
        assert_eq!(c.general.theme, "nord");
        assert_eq!(c.connections, vec![conn]);
    }
}
