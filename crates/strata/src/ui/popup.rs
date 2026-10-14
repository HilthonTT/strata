//! Overlays: prompts, pickers, confirmations, help, text and which-key.

use chrono::{DateTime, Local};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;
use strata_core::util::human_size;
use strata_core::Entry;
use unicode_width::UnicodeWidthStr;

use super::{block, centered, fit, truncate};
use crate::app::overlay::{ConflictState, Overlay, PickerState, RenamePreview};
use crate::app::{App, View};

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    if app.overlay.is_none() && !app.pending_keys.is_empty() {
        which_key(frame, app, area);
    }
    let Some(overlay) = &app.overlay else { return };
    match overlay {
        Overlay::Input(input) => {
            let rect = centered(area, 64.max(input.prompt.width() as u16 + 8), 3);
            let rect = Rect { y: area.height / 3, ..rect };
            if let Some(preview) = &input.preview {
                draw_rename_preview(frame, app, preview, rect, area);
            }
            frame.render_widget(Clear, rect);
            let b = block(&app.theme, app.border, &input.prompt, true);
            let inner = b.inner(rect);
            frame.render_widget(b, rect);
            let shown: String =
                if input.masked { "•".repeat(input.value.chars().count()) } else { input.value.clone() };
            // Scroll horizontally so the cursor stays visible.
            let before: String = shown.chars().take(input.cursor).collect();
            let skip = before.width().saturating_sub(inner.width.saturating_sub(2) as usize);
            let visible: String = shown.chars().skip(skip).collect();
            frame.render_widget(
                Paragraph::new(Line::styled(format!(" {visible}"), Style::default().fg(app.theme.fg))),
                inner,
            );
            let x = inner.x + 1 + (before.width() - skip) as u16;
            frame.set_cursor_position(Position::new(x.min(inner.right().saturating_sub(1)), inner.y));
        }
        Overlay::Picker(picker) => draw_picker(frame, app, picker, area),
        Overlay::Confirm(c) => {
            let width = (c.message.width() as u16 + 6).clamp(36, 80).min(area.width.saturating_sub(2));
            // Tall enough for the wrapped message, the blank line and the keys.
            let text_lines = wrapped_lines(&c.message, width.saturating_sub(4) as usize);
            let rect = centered(area, width, text_lines as u16 + 4);
            frame.render_widget(Clear, rect);
            let b = block(&app.theme, app.border, "Confirm", true);
            let inner = b.inner(rect);
            frame.render_widget(b, rect);
            let lines = vec![
                Line::styled(c.message.as_str(), Style::default().fg(app.theme.fg)),
                Line::raw(""),
                Line::from(vec![
                    Span::styled("[y]", Style::default().fg(app.theme.success).add_modifier(Modifier::BOLD)),
                    Span::styled("es   ", Style::default().fg(app.theme.muted)),
                    Span::styled("[n]", Style::default().fg(app.theme.error).add_modifier(Modifier::BOLD)),
                    Span::styled("o", Style::default().fg(app.theme.muted)),
                    Span::styled(
                        if c.action.is_destructive() { "   (press y to confirm)" } else { "   (enter = yes)" },
                        Style::default().fg(app.theme.muted),
                    ),
                ]),
            ];
            // A margin keeps wrapped lines aligned with the first.
            let text = inner.inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), text);
        }
        Overlay::Conflict(c) => draw_conflict(frame, app, c, area),
        Overlay::Help { scroll } => draw_help(frame, app, area, *scroll),
        Overlay::Text(t) => draw_text(frame, app, area, &t.title, &t.lines, t.scroll, t.colored),
    }
}

/// One name clash of a paste at a time, Midnight Commander style: both
/// sides with their size and modification time, and the answers.
fn draw_conflict(frame: &mut Frame, app: &App, c: &ConflictState, area: Rect) {
    let theme = &app.theme;
    let Some(clash) = c.clashes.get(c.current) else { return };
    let rect = centered(area, 72, 11);
    frame.render_widget(Clear, rect);
    let title = format!("Already exists ({}/{})", c.current + 1, c.clashes.len());
    let b = block(theme, app.border, &title, true);
    let inner = b.inner(rect);
    frame.render_widget(b, rect);
    let width = inner.width as usize;
    let dest = crate::app::short_path(&c.transfer.dest_dir);
    let lead = format!(" '{}' is already in ", clash.source.name);
    let lead = truncate(&lead, width.saturating_sub(12));
    let dest = super::truncate_left(&dest, width.saturating_sub(lead.width()).max(8));
    let newer = clash.source_is_newer();
    let older = clash.existing.modified.is_some_and(|e| clash.source.modified.is_some_and(|s| s < e));
    let side = |label: &str, e: &Entry, is_newer: bool| {
        let size = if e.is_dir() { "folder".to_string() } else { human_size(e.size) };
        let when = e
            .modified
            .map(|t| DateTime::<Local>::from(t).format(&app.config.general.date_format).to_string())
            .unwrap_or_else(|| "—".into());
        let mut spans = vec![
            Span::styled(format!(" {}", fit(label, 10)), Style::default().fg(theme.muted)),
            Span::styled(format!("{} ", fit(&size, 10)), Style::default().fg(theme.fg)),
            Span::styled(when, Style::default().fg(theme.fg)),
        ];
        if is_newer {
            spans.push(Span::styled("  newer", Style::default().fg(theme.success).add_modifier(Modifier::BOLD)));
        }
        Line::from(spans)
    };
    let key = |k: &'static str| Span::styled(k, Style::default().fg(theme.palette.accent).add_modifier(Modifier::BOLD));
    let text = |t: &'static str| Span::styled(t, Style::default().fg(theme.muted));
    let remaining = c.clashes.len() - c.current - 1;
    let all = if c.apply_all { "[x]" } else { "[ ]" };
    let lines = vec![
        Line::from(vec![
            Span::styled(lead, Style::default().fg(theme.fg)),
            Span::styled(dest, Style::default().fg(theme.warning)),
        ]),
        Line::raw(""),
        side("New", &clash.source, newer),
        side("Existing", &clash.existing, older),
        Line::raw(""),
        Line::from(vec![
            text(" "),
            key("[o]"),
            text("verwrite  "),
            key("[s]"),
            text("kip  "),
            key("[k]"),
            text("eep both  "),
            key("[n]"),
            text(" overwrite if newer"),
        ]),
        Line::from(vec![
            text(" "),
            key("[a]"),
            Span::styled(format!(" {all} "), Style::default().fg(theme.fg)),
            Span::styled(
                format!("apply to this and the {remaining} remaining  "),
                Style::default().fg(if remaining > 0 { theme.fg } else { theme.muted }),
            ),
            key("[esc]"),
            text(" cancel"),
        ]),
        Line::raw(""),
        Line::styled(" Overwritten items go to the trash on local disks.", Style::default().fg(theme.muted)),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// New names under the pattern-rename prompt, `old → new`.
fn draw_rename_preview(frame: &mut Frame, app: &App, preview: &RenamePreview, prompt: Rect, area: Rect) {
    let theme = &app.theme;
    let room = area.bottom().saturating_sub(prompt.bottom() + 1);
    let rows = preview.rows.len().min(12) as u16;
    let height = (rows + 2 + u16::from(preview.error.is_some()) + u16::from(preview.total > rows as usize)).min(room);
    if height < 3 {
        return;
    }
    let rect = Rect { y: prompt.bottom(), height, ..prompt };
    frame.render_widget(Clear, rect);
    let b = block(theme, app.border, "Preview", false);
    let inner = b.inner(rect);
    frame.render_widget(b, rect);
    let width = inner.width as usize;
    let mut lines = Vec::new();
    if let Some(e) = &preview.error {
        lines.push(Line::styled(truncate(&format!(" ✗ {e}"), width), Style::default().fg(theme.error)));
    }
    let half = width.saturating_sub(5) / 2;
    for (old, new) in preview.rows.iter().take(rows as usize) {
        let changed = old != new;
        let new_style = Style::default().fg(if changed { theme.success } else { theme.muted });
        lines.push(Line::from(vec![
            Span::styled(format!(" {}", fit(&truncate(old, half), half)), Style::default().fg(theme.muted)),
            Span::styled(" → ", Style::default().fg(theme.border)),
            Span::styled(truncate(new, half), new_style),
        ]));
    }
    if preview.total > rows as usize {
        let more = format!(" … and {} more", preview.total - rows as usize);
        lines.push(Line::styled(more, Style::default().fg(theme.muted)));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_picker(frame: &mut Frame, app: &App, picker: &PickerState, area: Rect) {
    let theme = &app.theme;
    // A menu opened with the mouse hides its query line until typed into.
    let query_rows = if picker.anchor.is_some() && picker.query.is_empty() { 0 } else { 2 };
    let rect = match picker.anchor {
        Some(at) => menu_rect(area, at, picker, query_rows),
        None => centered(area, (area.width * 3 / 5).max(50), (area.height * 3 / 5).max(12)),
    };
    frame.render_widget(Clear, rect);
    let title = match picker.anchor {
        Some(_) => picker.title.clone(),
        None => format!("{} ({}/{})", picker.title, picker.matches.len(), picker.items.len()),
    };
    let b = block(theme, app.border, &truncate(&title, rect.width.saturating_sub(4) as usize), true);
    let inner = b.inner(rect);
    frame.render_widget(b, rect);
    let [query, list] = Layout::vertical([Constraint::Length(query_rows), Constraint::Min(1)]).areas(inner);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(" › ", Style::default().fg(theme.palette.accent)),
                Span::styled(picker.query.clone(), Style::default().fg(theme.fg)),
            ]),
            Line::styled("─".repeat(inner.width as usize), Style::default().fg(theme.border)),
        ]),
        query,
    );
    if query_rows > 0 {
        frame.set_cursor_position(Position::new(query.x + 3 + picker.query.width() as u16, query.y));
    }

    let height = list.height as usize;
    let offset = picker.cursor.saturating_sub(height.saturating_sub(1));
    app.layout.popup.set(Some(rect));
    app.layout.popup_list.set(Some(list));
    app.layout.popup_offset.set(offset);
    let width = list.width as usize;
    let lines: Vec<Line> = picker
        .matches
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, m)| {
            let selected = i == picker.cursor;
            let mut base = Style::default().fg(theme.fg);
            if selected {
                base = base.bg(theme.cursor_bg).add_modifier(Modifier::BOLD);
            }
            let text = truncate(&picker.items[m.index], width.saturating_sub(3));
            let mut spans = vec![Span::styled(if selected { " ▶ " } else { "   " }, base.fg(theme.palette.accent))];
            for (ci, ch) in text.chars().enumerate() {
                let style = if m.positions.contains(&(ci as u32)) { base.fg(theme.palette.accent) } else { base };
                spans.push(Span::styled(ch.to_string(), style));
            }
            let used: usize = spans.iter().map(|s| s.content.width()).sum();
            spans.push(Span::styled(" ".repeat(width.saturating_sub(used)), base));
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), list);
}

/// A menu box just below and right of the pointer, moved back on screen
/// when it would run off an edge.
fn menu_rect(area: Rect, (x, y): (u16, u16), picker: &PickerState, query_rows: u16) -> Rect {
    let longest = picker.items.iter().map(|i| i.width()).chain([picker.title.width()]).max().unwrap_or(0);
    let width = (longest as u16 + 6).clamp(20, area.width);
    let height = (picker.matches.len() as u16 + query_rows + 2).clamp(3, area.height);
    let x = x.min(area.x + area.width - width);
    let y = if y + 1 + height <= area.y + area.height { y + 1 } else { y.saturating_sub(height).max(area.y) };
    Rect { x, y, width, height }
}

fn draw_help(frame: &mut Frame, app: &App, area: Rect, scroll: usize) {
    let theme = &app.theme;
    let mut lines = vec![Line::styled(
        " Keys (customise under [keys] in config.toml)",
        Style::default().fg(theme.title).add_modifier(Modifier::BOLD),
    )];
    for (keys, desc) in app.keymap.describe() {
        lines.push(Line::from(vec![
            Span::styled(format!("  {}", fit(&keys, 22)), Style::default().fg(theme.palette.accent)),
            Span::styled(desc, Style::default().fg(theme.fg)),
        ]));
    }
    let section =
        |title: &str| Line::styled(format!(" {title}"), Style::default().fg(theme.title).add_modifier(Modifier::BOLD));
    let row = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!("  {}", fit(k, 22)), Style::default().fg(theme.palette.accent)),
            Span::styled(d.to_string(), Style::default().fg(theme.fg)),
        ])
    };
    lines.push(Line::raw(""));
    lines.push(section("Docker view"));
    for (k, d) in [
        ("enter", "Browse the container's files"),
        ("e", "Shell in the container"),
        ("L", "Logs"),
        ("s / S", "Start / stop"),
        ("r", "Restart"),
        ("p", "Pause / unpause"),
        ("x", "Remove"),
    ] {
        lines.push(row(k, d));
    }
    lines.push(Line::raw(""));
    lines.push(section("NAS view"));
    for (k, d) in [
        ("enter", "Connect / open"),
        ("a", "Add a connection"),
        ("t", "Diagnose step by step"),
        ("u", "Disconnect / unmount"),
        ("p", "Save a password in the keychain"),
        ("f", "Forget the saved password"),
        ("r", "Re-check reachability"),
        ("e", "Edit config"),
    ] {
        lines.push(row(k, d));
    }
    lines.push(Line::raw(""));
    lines.push(section("Commands (:)"));
    for (_, d) in crate::app::COMMANDS {
        lines.push(Line::styled(format!("  {d}"), Style::default().fg(theme.fg)));
    }
    if let Some(host) = &app.plugins {
        let cmds = host.commands();
        if !cmds.is_empty() {
            lines.push(Line::raw(""));
            lines.push(section("Plugin commands"));
            for (n, d) in cmds {
                lines.push(row(&format!(":{n}"), &d));
            }
        }
    }
    let strings: Vec<Line> = lines;
    let rect = centered(area, 86, area.height.saturating_sub(4));
    render_scrollable(frame, app, rect, "Help · j/k scroll · q close", strings, scroll);
}

fn draw_text(frame: &mut Frame, app: &App, area: Rect, title: &str, lines: &[String], scroll: usize, colored: bool) {
    let rect = centered(area, (area.width * 4 / 5).max(60), (area.height * 4 / 5).max(10));
    let t = &app.theme;
    let color = |line: &str| match line.chars().next() {
        _ if !colored => t.fg,
        _ if line.starts_with("@@") => t.palette.accent,
        _ if line.starts_with("+++") || line.starts_with("---") => t.muted,
        Some('+' | '✓') => t.success,
        Some('-' | '✗') => t.error,
        Some('~' | '?') => t.warning,
        _ => t.fg,
    };
    let lines = lines.iter().map(|l| Line::styled(l.replace('\t', "    "), Style::default().fg(color(l)))).collect();
    render_scrollable(frame, app, rect, title, lines, scroll);
}

/// Lines `text` takes when word-wrapped to `width` columns.
fn wrapped_lines(text: &str, width: usize) -> usize {
    let width = width.max(1);
    let (mut lines, mut used) = (1, 0);
    for word in text.split(' ') {
        let w = word.width();
        let needed = if used == 0 { w } else { used + 1 + w };
        if needed <= width {
            used = needed;
            continue;
        }
        if used > 0 {
            lines += 1;
        }
        // A word longer than the line is broken across lines.
        lines += w.saturating_sub(1) / width;
        used = w.saturating_sub(1) % width + 1;
    }
    lines
}

fn render_scrollable(frame: &mut Frame, app: &App, rect: Rect, title: &str, lines: Vec<Line>, scroll: usize) {
    frame.render_widget(Clear, rect);
    let total = lines.len();
    let b = block(&app.theme, app.border, title, true);
    let inner = b.inner(rect);
    let max_scroll = total.saturating_sub(inner.height as usize);
    app.layout.popup_max_scroll.set(max_scroll);
    let scroll = scroll.min(max_scroll);
    let b = b.title_bottom(
        Line::styled(
            format!(" {}/{} ", (scroll + inner.height as usize).min(total), total),
            Style::default().fg(app.theme.muted),
        )
        .right_aligned(),
    );
    frame.render_widget(b, rect);
    let visible: Vec<Line> = lines.into_iter().skip(scroll).take(inner.height as usize).collect();
    frame.render_widget(Paragraph::new(visible), inner);
}

/// Shows what can follow a pending key prefix like `g`.
fn which_key(frame: &mut Frame, app: &App, area: Rect) {
    if app.view != View::Files
        && app.view != View::Dashboard
        && app.view != View::Docker
        && app.view != View::Connections
    {
        return;
    }
    let options = app.keymap.continuations(&app.pending_keys);
    if options.is_empty() {
        return;
    }
    let width = options.iter().map(|(k, d)| k.width() + d.width() + 6).max().unwrap_or(20).min(60) as u16;
    let height = (options.len() as u16 + 2).min(area.height.saturating_sub(4));
    let rect =
        Rect::new(area.right().saturating_sub(width + 1), area.bottom().saturating_sub(height + 2), width, height);
    frame.render_widget(Clear, rect);
    let b = block(&app.theme, app.border, "keys", true);
    let inner = b.inner(rect);
    frame.render_widget(b, rect);
    let lines: Vec<Line> = options
        .into_iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(
                    format!(" {k:<5}"),
                    Style::default().fg(app.theme.palette.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(d, Style::default().fg(app.theme.fg)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::wrapped_lines;

    #[test]
    fn counts_word_wrapped_lines() {
        assert_eq!(wrapped_lines("short", 20), 1);
        assert_eq!(wrapped_lines("one two three", 9), 2);
        assert_eq!(wrapped_lines("one two three four", 9), 3);
        assert_eq!(wrapped_lines("aaaaaaaaaaaaaaaaaaaaaaaaa", 10), 3, "long words are broken");
    }
}
