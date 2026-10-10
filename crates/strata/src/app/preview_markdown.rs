//! Markdown rendered for the preview: headings, lists, quotes, code blocks
//! with syntax colours, tables and inline styles, wrapped to the pane.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use strata_config::Theme;
use unicode_width::UnicodeWidthStr;

use super::highlight::Highlighter;

pub fn is_markdown(ext: &str) -> bool {
    matches!(ext, "md" | "markdown" | "mdown" | "mkd" | "mdx")
}

/// Renders `text` for a pane `width` columns wide.
pub fn render(text: &str, width: usize, theme: &Theme, highlighter: Option<&Highlighter>) -> Vec<Line<'static>> {
    let width = width.max(10);
    let muted = Style::default().fg(theme.muted);
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(raw) = lines.next() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        // Fenced code: highlighted by its language, never wrapped.
        if let Some(fence) = ["```", "~~~"].into_iter().find(|f| trimmed.starts_with(f)) {
            let lang = trimmed[3..].split_whitespace().next().unwrap_or("").to_string();
            let mut code = String::new();
            for next in lines.by_ref() {
                if next.trim_start().starts_with(fence) {
                    break;
                }
                code.push_str(next);
                code.push('\n');
            }
            let bar = Span::styled("▎ ", Style::default().fg(theme.surface));
            let highlighted = highlighter
                .filter(|_| !lang.is_empty())
                .and_then(|h| h.highlight(&format!("code.{lang}"), &code, usize::MAX));
            match highlighted {
                Some(code_lines) => {
                    out.extend(code_lines.into_iter().map(|l| prefixed(bar.clone(), l.spans)));
                }
                None => out.extend(code.lines().map(|l| {
                    Line::from(vec![
                        bar.clone(),
                        Span::styled(l.replace('\t', "    "), Style::default().fg(theme.palette.green)),
                    ])
                })),
            }
            continue;
        }

        if trimmed.is_empty() {
            out.push(Line::default());
            continue;
        }

        // Headings.
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            let title = trimmed[hashes..].trim().trim_end_matches('#').trim_end();
            let style = match hashes {
                1 => Style::default().fg(theme.palette.accent).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                2 => Style::default().fg(theme.palette.accent).add_modifier(Modifier::BOLD),
                _ => Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
            };
            out.extend(wrap(inline(title, style, theme), width, Vec::new(), Vec::new()));
            continue;
        }

        // Rules: `---`, `***`, `___`.
        let rule_char = trimmed.chars().next().unwrap_or(' ');
        if matches!(rule_char, '-' | '*' | '_')
            && trimmed.chars().filter(|c| !c.is_whitespace()).count() >= 3
            && trimmed.chars().all(|c| c == rule_char || c.is_whitespace())
        {
            out.push(Line::styled("─".repeat(width), muted));
            continue;
        }

        // Quotes.
        if let Some(rest) = trimmed.strip_prefix('>') {
            let bar = vec![Span::styled("▎ ", Style::default().fg(theme.palette.accent))];
            let style = Style::default().fg(theme.muted).add_modifier(Modifier::ITALIC);
            out.extend(wrap(inline(rest.trim_start(), style, theme), width, bar.clone(), bar));
            continue;
        }

        // Tables: cells separated by thin bars; the alignment row is a rule.
        if trimmed.starts_with('|') {
            let cells: Vec<&str> = trimmed.trim_matches('|').split('|').map(str::trim).collect();
            if cells.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':'))) {
                out.push(Line::styled("─".repeat(width.min(line.width())), muted));
                continue;
            }
            let mut spans = Vec::new();
            for (i, cell) in cells.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" │ ", muted));
                }
                spans.extend(inline(cell, Style::default().fg(theme.fg), theme));
            }
            out.push(Line::from(spans));
            continue;
        }

        // List items keep their nesting; bullets become •, tasks ☐ / ☑.
        let pad = " ".repeat(indent);
        if let Some((marker, rest)) = list_item(trimmed) {
            let (marker, rest) = match (marker.as_str(), rest) {
                ("•", r) if r.starts_with("[ ] ") => ("☐".to_string(), &r[4..]),
                ("•", r) if r.starts_with("[x] ") || r.starts_with("[X] ") => ("☑".to_string(), &r[4..]),
                (m, r) => (m.to_string(), r),
            };
            let first = vec![
                Span::raw(pad.clone()),
                Span::styled(format!("{marker} "), Style::default().fg(theme.palette.accent)),
            ];
            let rest_pad = vec![Span::raw(" ".repeat(indent + marker.width() + 1))];
            out.extend(wrap(inline(rest, Style::default().fg(theme.fg), theme), width, first, rest_pad));
            continue;
        }

        // Paragraph text; consecutive lines join like Markdown does.
        let mut para = trimmed.to_string();
        while let Some(next) = lines.peek() {
            let t = next.trim();
            if t.is_empty() || starts_block(t) {
                break;
            }
            para.push(' ');
            para.push_str(t);
            lines.next();
        }
        let pad = vec![Span::raw(pad)];
        out.extend(wrap(inline(&para, Style::default().fg(theme.fg), theme), width, pad.clone(), pad));
    }
    out
}

fn prefixed(prefix: Span<'static>, spans: Vec<Span<'static>>) -> Line<'static> {
    let mut all = vec![prefix];
    all.extend(spans);
    Line::from(all)
}

/// True for lines that start their own block instead of continuing a paragraph.
fn starts_block(t: &str) -> bool {
    t.starts_with('#')
        || t.starts_with('>')
        || t.starts_with('|')
        || t.starts_with("```")
        || t.starts_with("~~~")
        || list_item(t).is_some()
}

/// `- x`, `* x`, `+ x` → `("•", "x")`; `3. x` → `("3.", "x")`.
fn list_item(t: &str) -> Option<(String, &str)> {
    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("+ ")) {
        return Some(("•".into(), rest));
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    let rest = &t[digits..];
    if (1..=9).contains(&digits) && (rest.starts_with(". ") || rest.starts_with(") ")) {
        return Some((t[..digits + 1].to_string(), &rest[2..]));
    }
    None
}

/// Inline Markdown: `code`, **bold**, *italic*, _italic_, ~~strike~~,
/// [links](url) and ![images](src).
fn inline(text: &str, base: Style, theme: &Theme) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut buf = String::new();
    let (mut bold, mut italic, mut strike) = (false, false, false);
    let style_now = |bold: bool, italic: bool, strike: bool| {
        let mut s = base;
        if bold {
            s = s.add_modifier(Modifier::BOLD);
        }
        if italic {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if strike {
            s = s.add_modifier(Modifier::CROSSED_OUT);
        }
        s
    };
    let find = |from: usize, pat: &[char]| -> Option<usize> {
        (from..chars.len().saturating_sub(pat.len() - 1)).find(|&j| chars[j..j + pat.len()] == *pat)
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let flush = |buf: &mut String, spans: &mut Vec<Span<'static>>, style: Style| {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(buf), style));
            }
        };
        let current = style_now(bold, italic, strike);
        match c {
            '\\' if i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() => {
                buf.push(chars[i + 1]);
                i += 2;
                continue;
            }
            '`' => {
                if let Some(end) = find(i + 1, &['`']) {
                    flush(&mut buf, &mut spans, current);
                    let code: String = chars[i + 1..end].iter().collect();
                    spans.push(Span::styled(code, Style::default().fg(theme.palette.green).bg(theme.surface)));
                    i = end + 1;
                    continue;
                }
            }
            '*' | '_' if chars.get(i + 1) == Some(&c) => {
                flush(&mut buf, &mut spans, current);
                bold = !bold;
                i += 2;
                continue;
            }
            '~' if chars.get(i + 1) == Some(&'~') => {
                flush(&mut buf, &mut spans, current);
                strike = !strike;
                i += 2;
                continue;
            }
            // `_` only emphasises at word edges, so snake_case stays intact.
            '*' | '_' => {
                let prev_word = i > 0 && chars[i - 1].is_alphanumeric();
                let next_word = chars.get(i + 1).is_some_and(|n| n.is_alphanumeric());
                let edge = if italic { !next_word } else { !prev_word && next_word };
                if c == '*' || edge {
                    flush(&mut buf, &mut spans, current);
                    italic = !italic;
                    i += 1;
                    continue;
                }
            }
            '!' | '[' => {
                let start = if c == '!' { i + 1 } else { i };
                if chars.get(start) == Some(&'[') {
                    if let Some(close) = find(start + 1, &[']', '(']) {
                        if let Some(end) = find(close + 2, &[')']) {
                            flush(&mut buf, &mut spans, current);
                            let label: String = chars[start + 1..close].iter().collect();
                            let link = Style::default().fg(theme.palette.blue).add_modifier(Modifier::UNDERLINED);
                            if c == '!' {
                                spans.push(Span::styled(format!("[image: {label}]"), Style::default().fg(theme.muted)));
                            } else {
                                spans.extend(inline(&label, link, theme));
                            }
                            i = end + 1;
                            continue;
                        }
                    }
                }
            }
            _ => {}
        }
        buf.push(c);
        i += 1;
    }
    spans.push(Span::styled(buf, style_now(bold, italic, strike)));
    spans.retain(|s| !s.content.is_empty());
    spans
}

/// Word-wraps styled spans to `width` columns. Every line starts with
/// `first` (the first one) or `rest` (the others).
pub fn wrap(
    spans: Vec<Span<'static>>,
    width: usize,
    first: Vec<Span<'static>>,
    rest: Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    let prefix_width = |p: &[Span]| p.iter().map(|s| s.content.width()).sum::<usize>();
    let mut lines = Vec::new();
    let mut current = first.clone();
    let mut used = prefix_width(&first);
    let mut has_word = false;
    // A space seen since the last word, even at the end of another span.
    let mut pending_space = false;
    for span in spans {
        for (k, word) in span.content.split(' ').enumerate() {
            if word.is_empty() {
                pending_space |= k > 0;
                continue;
            }
            let space = std::mem::take(&mut pending_space) || k > 0;
            let mut word = word.to_string();
            let mut w = word.width();
            let needed = w + usize::from(space && has_word);
            if has_word && used + needed > width {
                lines.push(Line::from(std::mem::replace(&mut current, rest.clone())));
                used = prefix_width(&rest);
            } else if space && has_word {
                current.push(Span::raw(" "));
                used += 1;
            }
            // A word longer than a whole line is cut into pieces.
            while used + w > width && w > 1 {
                let room = width.saturating_sub(used).max(1);
                let (head, tail) = split_at_width(&word, room);
                current.push(Span::styled(head, span.style));
                lines.push(Line::from(std::mem::replace(&mut current, rest.clone())));
                used = prefix_width(&rest);
                word = tail;
                w = word.width();
            }
            current.push(Span::styled(word, span.style));
            used += w;
            has_word = true;
        }
    }
    if has_word || lines.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

fn split_at_width(s: &str, width: usize) -> (String, String) {
    use unicode_width::UnicodeWidthChar;
    let mut used = 0;
    for (i, c) in s.char_indices() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            return (s[..i].to_string(), s[i..].to_string());
        }
        used += w;
    }
    (s.to_string(), String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        strata_config::theme::builtin().into_iter().next().unwrap()
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn renders_blocks() {
        let md = "# Title\n\nSome *em* and **bold** with `code`.\n- one\n- [x] done\n> quoted\n\n---\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let out = text(&render(md, 40, &theme(), None));
        assert_eq!(out[0], "Title");
        assert_eq!(out[2], "Some em and bold with code.");
        assert_eq!(out[3], "• one");
        assert_eq!(out[4], "☑ done");
        assert_eq!(out[5], "▎ quoted");
        assert!(out[7].starts_with("───"));
        assert_eq!(out[8], "a │ b");
        assert_eq!(out[10], "1 │ 2");
    }

    #[test]
    fn paragraphs_join_and_wrap() {
        let out = text(&render("alpha beta\ngamma delta epsilon", 12, &theme(), None));
        assert_eq!(out, ["alpha beta", "gamma delta", "epsilon"]);
    }

    #[test]
    fn code_blocks_are_kept_verbatim() {
        let out = text(&render("```\nlet x = 1;   // keep   spacing\n```\n", 12, &theme(), None));
        assert_eq!(out, ["▎ let x = 1;   // keep   spacing"]);
    }

    #[test]
    fn links_images_and_snake_case() {
        let t = theme();
        let spans = inline("see [the docs](http://x) and ![logo](a.png) in my_var_name", Style::default(), &t);
        let s: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(s, "see the docs and [image: logo] in my_var_name");
    }

    #[test]
    fn long_words_are_split() {
        let lines = wrap(vec![Span::raw("abcdefghij")], 4, Vec::new(), Vec::new());
        assert_eq!(text(&lines), ["abcd", "efgh", "ij"]);
    }
}
