//! Overlays: prompts, pickers, confirmations, help, text and which-key.

use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use super::{block, centered, fit, truncate};
use crate::app::overlay::{Overlay, PickerState};
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
            frame.render_widget(Clear, rect);
            let b = block(&app.theme, app.config.general.border, &input.prompt, true);
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
            let width = (c.message.width() as u16 + 6).clamp(36, 80);
            let rect = centered(area, width, 6);
            frame.render_widget(Clear, rect);
            let b = block(&app.theme, app.config.general.border, "Confirm", true);
            let inner = b.inner(rect);
            frame.render_widget(b, rect);
            let lines = vec![
                Line::styled(format!(" {}", c.message), Style::default().fg(app.theme.fg)),
                Line::raw(""),
                Line::from(vec![
                    Span::styled(" [y]", Style::default().fg(app.theme.success).add_modifier(Modifier::BOLD)),
                    Span::styled("es   ", Style::default().fg(app.theme.muted)),
                    Span::styled("[n]", Style::default().fg(app.theme.error).add_modifier(Modifier::BOLD)),
                    Span::styled("o", Style::default().fg(app.theme.muted)),
                ]),
            ];
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }
        Overlay::Conflict(c) => {
            let theme = &app.theme;
            let dest = crate::app::short_path(&c.transfer.dest_dir);
            let shown = c.names.len().min(6);
            let rect = centered(area, 70, shown as u16 + 7 + u16::from(c.names.len() > shown));
            frame.render_widget(Clear, rect);
            let b = block(theme, app.config.general.border, "Already exists", true);
            let inner = b.inner(rect);
            frame.render_widget(b, rect);
            let width = inner.width as usize;
            let mut lines = vec![Line::styled(
                truncate(&format!(" {} item(s) already exist in {dest}:", c.names.len()), width),
                Style::default().fg(theme.fg),
            )];
            for name in c.names.iter().take(shown) {
                lines.push(Line::styled(truncate(&format!("   • {name}"), width), Style::default().fg(theme.warning)));
            }
            if c.names.len() > shown {
                lines.push(Line::styled(
                    format!("   … and {} more", c.names.len() - shown),
                    Style::default().fg(theme.muted),
                ));
            }
            lines.push(Line::raw(""));
            let key = |k: &'static str| {
                Span::styled(k, Style::default().fg(theme.palette.accent).add_modifier(Modifier::BOLD))
            };
            let text = |t: &'static str| Span::styled(t, Style::default().fg(theme.muted));
            lines.push(Line::from(vec![
                text(" "),
                key("[k]"),
                text("eep both  "),
                key("[o]"),
                text("verwrite  "),
                key("[s]"),
                text("kip  "),
                key("[esc]"),
                text(" cancel"),
            ]));
            lines.push(Line::styled(
                " Overwritten items go to the trash on local disks.",
                Style::default().fg(theme.muted),
            ));
            frame.render_widget(Paragraph::new(lines), inner);
        }
        Overlay::Help { scroll } => draw_help(frame, app, area, *scroll),
        Overlay::Text(t) => draw_text(frame, app, area, &t.title, &t.lines, t.scroll, t.colored),
    }
}

fn draw_picker(frame: &mut Frame, app: &App, picker: &PickerState, area: Rect) {
    let theme = &app.theme;
    let rect = centered(area, (area.width * 3 / 5).max(50), (area.height * 3 / 5).max(12));
    frame.render_widget(Clear, rect);
    let title = format!("{} ({}/{})", picker.title, picker.matches.len(), picker.items.len());
    let b = block(theme, app.config.general.border, &title, true);
    let inner = b.inner(rect);
    frame.render_widget(b, rect);
    let [query, list] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
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
    frame.set_cursor_position(Position::new(query.x + 3 + picker.query.width() as u16, query.y));

    let height = list.height as usize;
    let offset = picker.cursor.saturating_sub(height.saturating_sub(1));
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

fn render_scrollable(frame: &mut Frame, app: &App, rect: Rect, title: &str, lines: Vec<Line>, scroll: usize) {
    frame.render_widget(Clear, rect);
    let total = lines.len();
    let b = block(&app.theme, app.config.general.border, title, true);
    let inner = b.inner(rect);
    let max_scroll = total.saturating_sub(inner.height as usize);
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
    let b = block(&app.theme, app.config.general.border, "keys", true);
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
