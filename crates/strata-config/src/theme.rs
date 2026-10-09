//! Themes. Each built-in theme is a 13-colour palette; the semantic colours
//! the UI uses are derived from it, so a custom theme is a dozen hex codes.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use ratatui::style::Color;
use serde::Deserialize;

/// The raw colours a theme is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub bg: Color,
    pub surface: Color,
    pub overlay: Color,
    pub fg: Color,
    pub muted: Color,
    pub red: Color,
    pub orange: Color,
    pub yellow: Color,
    pub green: Color,
    pub cyan: Color,
    pub blue: Color,
    pub purple: Color,
    pub accent: Color,
}

/// Semantic colours used by the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub name: String,
    pub palette: Palette,
    pub bg: Color,
    pub surface: Color,
    pub fg: Color,
    pub muted: Color,
    pub border: Color,
    pub border_focus: Color,
    pub title: Color,
    pub cursor_bg: Color,
    pub cursor_fg: Color,
    pub marked: Color,
    pub dir: Color,
    pub file: Color,
    pub exec: Color,
    pub symlink: Color,
    pub image: Color,
    pub media: Color,
    pub archive: Color,
    pub code: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,
    /// Distinct series colours for charts.
    pub chart: [Color; 6],
}

impl Theme {
    pub fn from_palette(name: impl Into<String>, p: Palette) -> Self {
        Self {
            name: name.into(),
            palette: p,
            bg: p.bg,
            surface: p.surface,
            fg: p.fg,
            muted: p.muted,
            border: p.overlay,
            border_focus: p.accent,
            title: p.accent,
            cursor_bg: p.overlay,
            cursor_fg: p.fg,
            marked: p.yellow,
            dir: p.blue,
            file: p.fg,
            exec: p.green,
            symlink: p.cyan,
            image: p.purple,
            media: p.orange,
            archive: p.red,
            code: p.yellow,
            success: p.green,
            warning: p.yellow,
            error: p.red,
            info: p.blue,
            chart: [p.blue, p.purple, p.cyan, p.green, p.yellow, p.orange],
        }
    }

    /// Uses the terminal background instead of the theme's.
    pub fn transparent(mut self) -> Self {
        self.bg = Color::Reset;
        self
    }

    /// Green → yellow → red as `ratio` grows (for usage gauges).
    pub fn level(&self, ratio: f64) -> Color {
        match ratio {
            r if r >= 0.9 => self.error,
            r if r >= 0.75 => self.warning,
            _ => self.success,
        }
    }
}

fn hex(s: &str) -> Color {
    parse_color(s).unwrap_or(Color::Reset)
}

/// Parses `#rrggbb`, `#rgb`, `reset` or a named ANSI colour.
pub fn parse_color(s: &str) -> Result<Color> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix('#') {
        let expand = |c: &str| u8::from_str_radix(c, 16);
        let (r, g, b) = match h.len() {
            6 => (expand(&h[0..2])?, expand(&h[2..4])?, expand(&h[4..6])?),
            3 => {
                let d = |i: usize| expand(&h[i..i + 1]).map(|v| v * 17);
                (d(0)?, d(1)?, d(2)?)
            }
            _ => bail!("invalid colour '{s}'"),
        };
        return Ok(Color::Rgb(r, g, b));
    }
    s.parse::<Color>().map_err(|_| anyhow::anyhow!("invalid colour '{s}'"))
}

macro_rules! palette {
    ($bg:literal, $surface:literal, $overlay:literal, $fg:literal, $muted:literal,
     $red:literal, $orange:literal, $yellow:literal, $green:literal, $cyan:literal,
     $blue:literal, $purple:literal, accent = $accent:literal) => {
        Palette {
            bg: hex($bg),
            surface: hex($surface),
            overlay: hex($overlay),
            fg: hex($fg),
            muted: hex($muted),
            red: hex($red),
            orange: hex($orange),
            yellow: hex($yellow),
            green: hex($green),
            cyan: hex($cyan),
            blue: hex($blue),
            purple: hex($purple),
            accent: hex($accent),
        }
    };
}

/// All built-in themes, keyed by name.
pub fn builtin() -> Vec<Theme> {
    let t = Theme::from_palette;
    vec![
        t(
            "catppuccin-mocha",
            palette!(
                "#1e1e2e",
                "#313244",
                "#45475a",
                "#cdd6f4",
                "#7f849c",
                "#f38ba8",
                "#fab387",
                "#f9e2af",
                "#a6e3a1",
                "#94e2d5",
                "#89b4fa",
                "#cba6f7",
                accent = "#cba6f7"
            ),
        ),
        t(
            "catppuccin-macchiato",
            palette!(
                "#24273a",
                "#363a4f",
                "#494d64",
                "#cad3f5",
                "#8087a2",
                "#ed8796",
                "#f5a97f",
                "#eed49f",
                "#a6da95",
                "#8bd5ca",
                "#8aadf4",
                "#c6a0f6",
                accent = "#c6a0f6"
            ),
        ),
        t(
            "catppuccin-frappe",
            palette!(
                "#303446",
                "#414559",
                "#51576d",
                "#c6d0f5",
                "#838ba7",
                "#e78284",
                "#ef9f76",
                "#e5c890",
                "#a6d189",
                "#81c8be",
                "#8caaee",
                "#ca9ee6",
                accent = "#ca9ee6"
            ),
        ),
        t(
            "catppuccin-latte",
            palette!(
                "#eff1f5",
                "#e6e9ef",
                "#ccd0da",
                "#4c4f69",
                "#8c8fa1",
                "#d20f39",
                "#fe640b",
                "#df8e1d",
                "#40a02b",
                "#179299",
                "#1e66f5",
                "#8839ef",
                accent = "#8839ef"
            ),
        ),
        t(
            "nord",
            palette!(
                "#2e3440",
                "#3b4252",
                "#434c5e",
                "#eceff4",
                "#7b88a1",
                "#bf616a",
                "#d08770",
                "#ebcb8b",
                "#a3be8c",
                "#8fbcbb",
                "#81a1c1",
                "#b48ead",
                accent = "#88c0d0"
            ),
        ),
        t(
            "tokyo-night",
            palette!(
                "#1a1b26",
                "#24283b",
                "#414868",
                "#c0caf5",
                "#565f89",
                "#f7768e",
                "#ff9e64",
                "#e0af68",
                "#9ece6a",
                "#7dcfff",
                "#7aa2f7",
                "#bb9af7",
                accent = "#7aa2f7"
            ),
        ),
        t(
            "tokyo-night-storm",
            palette!(
                "#24283b",
                "#292e42",
                "#414868",
                "#c0caf5",
                "#565f89",
                "#f7768e",
                "#ff9e64",
                "#e0af68",
                "#9ece6a",
                "#7dcfff",
                "#7aa2f7",
                "#bb9af7",
                accent = "#bb9af7"
            ),
        ),
        t(
            "tokyo-night-day",
            palette!(
                "#e1e2e7",
                "#d0d5e3",
                "#c4c8da",
                "#3760bf",
                "#848cb5",
                "#f52a65",
                "#b15c00",
                "#8c6c3e",
                "#587539",
                "#007197",
                "#2e7de9",
                "#9854f1",
                accent = "#2e7de9"
            ),
        ),
        t(
            "dracula",
            palette!(
                "#282a36",
                "#343746",
                "#44475a",
                "#f8f8f2",
                "#6272a4",
                "#ff5555",
                "#ffb86c",
                "#f1fa8c",
                "#50fa7b",
                "#8be9fd",
                "#bd93f9",
                "#ff79c6",
                accent = "#bd93f9"
            ),
        ),
        t(
            "gruvbox-dark",
            palette!(
                "#282828",
                "#3c3836",
                "#504945",
                "#ebdbb2",
                "#928374",
                "#fb4934",
                "#fe8019",
                "#fabd2f",
                "#b8bb26",
                "#8ec07c",
                "#83a598",
                "#d3869b",
                accent = "#fe8019"
            ),
        ),
        t(
            "gruvbox-light",
            palette!(
                "#fbf1c7",
                "#ebdbb2",
                "#d5c4a1",
                "#3c3836",
                "#928374",
                "#9d0006",
                "#af3a03",
                "#b57614",
                "#79740e",
                "#427b58",
                "#076678",
                "#8f3f71",
                accent = "#af3a03"
            ),
        ),
        t(
            "rose-pine",
            palette!(
                "#191724",
                "#1f1d2e",
                "#26233a",
                "#e0def4",
                "#6e6a86",
                "#eb6f92",
                "#ebbcba",
                "#f6c177",
                "#9ccfd8",
                "#9ccfd8",
                "#31748f",
                "#c4a7e7",
                accent = "#c4a7e7"
            ),
        ),
        t(
            "rose-pine-moon",
            palette!(
                "#232136",
                "#2a273f",
                "#393552",
                "#e0def4",
                "#6e6a86",
                "#eb6f92",
                "#ea9a97",
                "#f6c177",
                "#9ccfd8",
                "#9ccfd8",
                "#3e8fb0",
                "#c4a7e7",
                accent = "#c4a7e7"
            ),
        ),
        t(
            "rose-pine-dawn",
            palette!(
                "#faf4ed",
                "#fffaf3",
                "#f2e9e1",
                "#575279",
                "#9893a5",
                "#b4637a",
                "#d7827e",
                "#ea9d34",
                "#56949f",
                "#56949f",
                "#286983",
                "#907aa9",
                accent = "#907aa9"
            ),
        ),
        t(
            "one-dark",
            palette!(
                "#282c34",
                "#2c313a",
                "#3e4451",
                "#abb2bf",
                "#5c6370",
                "#e06c75",
                "#d19a66",
                "#e5c07b",
                "#98c379",
                "#56b6c2",
                "#61afef",
                "#c678dd",
                accent = "#61afef"
            ),
        ),
        t(
            "solarized-dark",
            palette!(
                "#002b36",
                "#073642",
                "#0e4b5a",
                "#93a1a1",
                "#657b83",
                "#dc322f",
                "#cb4b16",
                "#b58900",
                "#859900",
                "#2aa198",
                "#268bd2",
                "#6c71c4",
                accent = "#268bd2"
            ),
        ),
        t(
            "solarized-light",
            palette!(
                "#fdf6e3",
                "#eee8d5",
                "#e4ddc8",
                "#586e75",
                "#93a1a1",
                "#dc322f",
                "#cb4b16",
                "#b58900",
                "#859900",
                "#2aa198",
                "#268bd2",
                "#6c71c4",
                accent = "#268bd2"
            ),
        ),
        t(
            "everforest",
            palette!(
                "#2d353b",
                "#343f44",
                "#3d484d",
                "#d3c6aa",
                "#859289",
                "#e67e80",
                "#e69875",
                "#dbbc7f",
                "#a7c080",
                "#83c092",
                "#7fbbb3",
                "#d699b6",
                accent = "#a7c080"
            ),
        ),
        t(
            "kanagawa",
            palette!(
                "#1f1f28",
                "#2a2a37",
                "#363646",
                "#dcd7ba",
                "#727169",
                "#e46876",
                "#ffa066",
                "#e6c384",
                "#98bb6c",
                "#7aa89f",
                "#7e9cd8",
                "#957fb8",
                accent = "#7e9cd8"
            ),
        ),
        t(
            "monokai",
            palette!(
                "#272822",
                "#3e3d32",
                "#49483e",
                "#f8f8f2",
                "#75715e",
                "#f92672",
                "#fd971f",
                "#e6db74",
                "#a6e22e",
                "#66d9ef",
                "#66d9ef",
                "#ae81ff",
                accent = "#a6e22e"
            ),
        ),
        t(
            "ayu-dark",
            palette!(
                "#0b0e14",
                "#131721",
                "#202229",
                "#bfbdb6",
                "#565b66",
                "#f07178",
                "#ff8f40",
                "#e6b450",
                "#aad94c",
                "#95e6cb",
                "#59c2ff",
                "#d2a6ff",
                accent = "#e6b450"
            ),
        ),
        t(
            "github-dark",
            palette!(
                "#0d1117",
                "#161b22",
                "#30363d",
                "#c9d1d9",
                "#8b949e",
                "#ff7b72",
                "#ffa657",
                "#d29922",
                "#3fb950",
                "#39c5cf",
                "#58a6ff",
                "#bc8cff",
                accent = "#58a6ff"
            ),
        ),
        t(
            "github-light",
            palette!(
                "#ffffff",
                "#f6f8fa",
                "#d0d7de",
                "#24292f",
                "#57606a",
                "#cf222e",
                "#bc4c00",
                "#9a6700",
                "#1a7f37",
                "#1b7c83",
                "#0969da",
                "#8250df",
                accent = "#0969da"
            ),
        ),
        t(
            "nightfox",
            palette!(
                "#192330",
                "#212e3f",
                "#29394f",
                "#cdcecf",
                "#71839b",
                "#c94f6d",
                "#f4a261",
                "#dbc074",
                "#81b29a",
                "#63cdcf",
                "#719cd6",
                "#9d79d6",
                accent = "#719cd6"
            ),
        ),
        t(
            "palenight",
            palette!(
                "#292d3e",
                "#32364a",
                "#444267",
                "#a6accd",
                "#676e95",
                "#f07178",
                "#f78c6c",
                "#ffcb6b",
                "#c3e88d",
                "#89ddff",
                "#82aaff",
                "#c792ea",
                accent = "#c792ea"
            ),
        ),
        t(
            "sonokai",
            palette!(
                "#2c2e34",
                "#33353f",
                "#414550",
                "#e2e2e3",
                "#7f8490",
                "#fc5d7c",
                "#f39660",
                "#e7c664",
                "#9ed072",
                "#76cce0",
                "#76cce0",
                "#b39df3",
                accent = "#9ed072"
            ),
        ),
        t(
            "terminal",
            Palette {
                bg: Color::Reset,
                surface: Color::Reset,
                overlay: Color::DarkGray,
                fg: Color::Reset,
                muted: Color::DarkGray,
                red: Color::Red,
                orange: Color::LightRed,
                yellow: Color::Yellow,
                green: Color::Green,
                cyan: Color::Cyan,
                blue: Color::Blue,
                purple: Color::Magenta,
                accent: Color::Cyan,
            },
        ),
    ]
}

/// A user theme file: `~/.config/strata/themes/<name>.toml`.
///
/// ```toml
/// inherits = "nord"   # optional base theme
/// accent = "#ff8800"  # any palette key
/// ```
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    name: Option<String>,
    inherits: Option<String>,
    bg: Option<String>,
    surface: Option<String>,
    overlay: Option<String>,
    fg: Option<String>,
    muted: Option<String>,
    red: Option<String>,
    orange: Option<String>,
    yellow: Option<String>,
    green: Option<String>,
    cyan: Option<String>,
    blue: Option<String>,
    purple: Option<String>,
    accent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ThemeRegistry {
    themes: BTreeMap<String, Theme>,
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        Self { themes: builtin().into_iter().map(|t| (t.name.clone(), t)).collect() }
    }
}

impl ThemeRegistry {
    /// Built-ins plus every `*.toml` in `dir`. Broken files are reported,
    /// not fatal.
    pub fn load(dir: &Path) -> (Self, Vec<String>) {
        let mut reg = Self::default();
        let mut errors = Vec::new();
        let Ok(read) = std::fs::read_dir(dir) else {
            return (reg, errors);
        };
        let mut files: Vec<_> = read.filter_map(|e| e.ok().map(|e| e.path())).collect();
        files.sort();
        for path in files.into_iter().filter(|p| p.extension().is_some_and(|e| e == "toml")) {
            if let Err(e) = reg.load_file(&path) {
                errors.push(format!("theme {}: {e:#}", path.display()));
            }
        }
        (reg, errors)
    }

    fn load_file(&mut self, path: &Path) -> Result<()> {
        let text = std::fs::read_to_string(path)?;
        let file: ThemeFile = toml::from_str(&text)?;
        let stem = path.file_stem().context("no file name")?.to_string_lossy().into_owned();
        let name = file.name.clone().unwrap_or(stem);
        let base = file.inherits.as_deref().unwrap_or("catppuccin-mocha");
        let mut p = self.get(base).with_context(|| format!("unknown base theme '{base}'"))?.palette;
        let set = |slot: &mut Color, v: &Option<String>| -> Result<()> {
            if let Some(v) = v {
                *slot = parse_color(v)?;
            }
            Ok(())
        };
        set(&mut p.bg, &file.bg)?;
        set(&mut p.surface, &file.surface)?;
        set(&mut p.overlay, &file.overlay)?;
        set(&mut p.fg, &file.fg)?;
        set(&mut p.muted, &file.muted)?;
        set(&mut p.red, &file.red)?;
        set(&mut p.orange, &file.orange)?;
        set(&mut p.yellow, &file.yellow)?;
        set(&mut p.green, &file.green)?;
        set(&mut p.cyan, &file.cyan)?;
        set(&mut p.blue, &file.blue)?;
        set(&mut p.purple, &file.purple)?;
        set(&mut p.accent, &file.accent)?;
        self.themes.insert(name.clone(), Theme::from_palette(name, p));
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Theme> {
        self.themes.get(name)
    }

    pub fn names(&self) -> Vec<String> {
        self.themes.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.themes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.themes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_twenty_plus_themes() {
        let reg = ThemeRegistry::default();
        assert!(reg.len() >= 20, "only {} themes", reg.len());
        for name in ["catppuccin-mocha", "nord", "tokyo-night", "dracula", "gruvbox-dark", "rose-pine"] {
            assert!(reg.get(name).is_some(), "missing {name}");
        }
    }

    #[test]
    fn every_builtin_colour_parses() {
        for theme in builtin().iter().filter(|t| t.name != "terminal") {
            assert_ne!(theme.palette.bg, Color::Reset, "{} has a bad colour", theme.name);
            assert_ne!(theme.palette.accent, Color::Reset, "{} has a bad colour", theme.name);
        }
    }

    #[test]
    fn parses_colours() {
        assert_eq!(parse_color("#ff0080").unwrap(), Color::Rgb(255, 0, 128));
        assert_eq!(parse_color("#fff").unwrap(), Color::Rgb(255, 255, 255));
        assert_eq!(parse_color("red").unwrap(), Color::Red);
        assert!(parse_color("#12").is_err());
    }

    #[test]
    fn loads_custom_theme_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mine.toml"), "inherits = \"nord\"\naccent = \"#ff8800\"").unwrap();
        std::fs::write(dir.path().join("broken.toml"), "accent = \"nope\"").unwrap();
        let (reg, errors) = ThemeRegistry::load(dir.path());
        let mine = reg.get("mine").unwrap();
        assert_eq!(mine.border_focus, Color::Rgb(255, 136, 0));
        assert_eq!(mine.palette.bg, reg.get("nord").unwrap().palette.bg);
        assert_eq!(errors.len(), 1);
    }
}
