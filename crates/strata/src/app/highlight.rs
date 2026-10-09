//! Syntax highlighting for the preview, coloured from the active theme.

use std::str::FromStr;
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use strata_config::Palette;
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SynColor, FontStyle, ScopeSelectors, StyleModifier, Theme as SynTheme, ThemeItem, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// A syntect theme generated from a strata palette.
pub struct Highlighter {
    theme: SynTheme,
}

impl Highlighter {
    pub fn new(p: &Palette) -> Self {
        let rules: &[(&str, Color, FontStyle)] = &[
            ("comment, punctuation.definition.comment", p.muted, FontStyle::ITALIC),
            ("string, string.quoted, markup.inline.raw", p.green, FontStyle::empty()),
            ("constant.character.escape, string.regexp", p.cyan, FontStyle::empty()),
            ("constant.numeric, constant.language, constant.other, support.constant", p.orange, FontStyle::empty()),
            ("keyword, storage.modifier, keyword.control, meta.preprocessor", p.purple, FontStyle::empty()),
            ("keyword.operator, punctuation.accessor", p.cyan, FontStyle::empty()),
            (
                "storage.type, entity.name.type, entity.name.class, support.type, support.class, entity.other.inherited-class",
                p.yellow,
                FontStyle::empty(),
            ),
            ("entity.name.function, support.function, meta.function-call variable.function", p.blue, FontStyle::empty()),
            ("variable.language, variable.parameter", p.red, FontStyle::empty()),
            ("entity.name.tag", p.blue, FontStyle::empty()),
            ("entity.other.attribute-name, support.type.property-name", p.yellow, FontStyle::empty()),
            ("markup.heading, entity.name.section", p.blue, FontStyle::BOLD),
            ("markup.bold", p.orange, FontStyle::BOLD),
            ("markup.italic", p.purple, FontStyle::ITALIC),
            ("markup.underline.link, markup.link", p.cyan, FontStyle::UNDERLINE),
            ("markup.inserted", p.green, FontStyle::empty()),
            ("markup.deleted, invalid", p.red, FontStyle::empty()),
        ];
        let scopes = rules
            .iter()
            .filter_map(|(selector, color, font)| {
                Some(ThemeItem {
                    scope: ScopeSelectors::from_str(selector).ok()?,
                    style: StyleModifier { foreground: to_syn(*color), background: None, font_style: Some(*font) },
                })
            })
            .collect();
        let theme = SynTheme {
            settings: ThemeSettings { foreground: to_syn(p.fg), ..Default::default() },
            scopes,
            ..Default::default()
        };
        Self { theme }
    }

    /// Highlighted lines, or `None` when the language is unknown.
    pub fn highlight(&self, file_name: &str, text: &str, max_lines: usize) -> Option<Vec<Line<'static>>> {
        let syntax = find_syntax(file_name, text)?;
        let mut lines = HighlightLines::new(syntax, &self.theme);
        let mut out = Vec::new();
        for line in LinesWithEndings::from(text).take(max_lines) {
            let ranges = lines.highlight_line(line, syntaxes()).ok()?;
            let spans = ranges
                .into_iter()
                .map(|(style, piece)| {
                    let text = piece.trim_end_matches(['\n', '\r']).replace('\t', "    ");
                    let mut s = Style::default();
                    if let Some(c) = from_syn(style.foreground) {
                        s = s.fg(c);
                    }
                    if style.font_style.contains(FontStyle::BOLD) {
                        s = s.add_modifier(Modifier::BOLD);
                    }
                    if style.font_style.contains(FontStyle::ITALIC) {
                        s = s.add_modifier(Modifier::ITALIC);
                    }
                    if style.font_style.contains(FontStyle::UNDERLINE) {
                        s = s.add_modifier(Modifier::UNDERLINED);
                    }
                    Span::styled(text, s)
                })
                .filter(|s| !s.content.is_empty())
                .collect::<Vec<_>>();
            out.push(Line::from(spans));
        }
        Some(out)
    }
}

fn find_syntax<'a>(file_name: &str, text: &str) -> Option<&'a SyntaxReference> {
    let set = syntaxes();
    let ext = file_name.rsplit_once('.').map(|(_, e)| e).unwrap_or(file_name);
    set.find_syntax_by_extension(ext)
        .or_else(|| set.find_syntax_by_extension(file_name))
        .or_else(|| set.find_syntax_by_first_line(text.lines().next().unwrap_or("")))
        .filter(|s| s.name != "Plain Text")
}

/// Theme colours that are not RGB (the `terminal` theme) are stored as an
/// ANSI index with alpha 0, the same trick `bat` uses.
fn to_syn(color: Color) -> Option<SynColor> {
    let indexed = |i: u8| Some(SynColor { r: i, g: 0, b: 0, a: 0 });
    match color {
        Color::Rgb(r, g, b) => Some(SynColor { r, g, b, a: 0xFF }),
        Color::Indexed(i) => indexed(i),
        Color::Black => indexed(0),
        Color::Red => indexed(1),
        Color::Green => indexed(2),
        Color::Yellow => indexed(3),
        Color::Blue => indexed(4),
        Color::Magenta => indexed(5),
        Color::Cyan => indexed(6),
        Color::Gray => indexed(7),
        Color::DarkGray => indexed(8),
        Color::LightRed => indexed(9),
        Color::LightGreen => indexed(10),
        Color::LightYellow => indexed(11),
        Color::LightBlue => indexed(12),
        Color::LightMagenta => indexed(13),
        Color::LightCyan => indexed(14),
        Color::White => indexed(15),
        Color::Reset => None,
    }
}

fn from_syn(c: SynColor) -> Option<Color> {
    match c.a {
        0 => Some(Color::Indexed(c.r)),
        _ => Some(Color::Rgb(c.r, c.g, c.b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strata_config::ThemeRegistry;

    #[test]
    fn highlights_known_languages_only() {
        let reg = ThemeRegistry::default();
        let h = Highlighter::new(&reg.get("nord").unwrap().palette);
        let lines = h.highlight("main.rs", "fn main() {\n    // hi\n}\n", 10).unwrap();
        assert_eq!(lines.len(), 3);
        let colours: std::collections::HashSet<_> =
            lines.iter().flat_map(|l| l.spans.iter().map(|s| s.style.fg)).collect();
        assert!(colours.len() > 1, "code should use several colours");
        assert!(h.highlight("notes.unknownext", "just text", 10).is_none());
        assert!(h.highlight("script", "#!/bin/sh\necho hi\n", 10).is_some(), "shebang detection");
    }
}
