//! Top bar (views, plugin segments) and bottom status line.

use chrono::Local;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use strata_config::keymap::format_sequence;
use strata_plugin::Level;
use unicode_width::UnicodeWidthStr;

use super::truncate;
use crate::app::{App, Focus, View};

pub fn top_bar(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let bar_bg = theme.surface;
    let mut left = vec![Span::styled(
        " ◆ strata ",
        Style::default().bg(theme.palette.accent).fg(theme.bg).add_modifier(Modifier::BOLD),
    )];
    for (i, view) in View::ALL.iter().enumerate() {
        let style = if *view == app.view {
            Style::default().fg(theme.palette.accent).bg(bar_bg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted).bg(bar_bg)
        };
        left.push(Span::styled(format!(" {} {} ", i + 1, view.title()), style));
    }
    let titles = app.tab_titles();
    if titles.len() > 1 {
        left.push(Span::styled(" │", Style::default().fg(theme.border).bg(bar_bg)));
        for (i, title) in titles.iter().enumerate() {
            let style = if i == app.tab {
                Style::default().fg(theme.bg).bg(theme.palette.blue).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted).bg(bar_bg)
            };
            left.push(Span::styled(format!(" {}:{} ", i + 1, super::truncate(title, 16)), style));
        }
    }

    let mut right: Vec<String> = app.plugin_status.clone();
    let running = app.jobs.running();
    if running > 0 {
        right.push(format!("⟳ {running} job{}", if running == 1 { "" } else { "s" }));
    }
    let mem = &app.metrics.memory;
    if mem.total > 0 {
        right.push(format!("mem {:.0}% {}", mem.used_ratio() * 100.0, mem.pressure.label()));
    }
    right.push(Local::now().format("%H:%M").to_string());
    let right_text = format!("{} ", right.join("  │  "));

    let used: usize = left.iter().map(|s| s.content.width()).sum();
    let pad = (area.width as usize).saturating_sub(used + right_text.width());
    left.push(Span::styled(" ".repeat(pad), Style::default().bg(bar_bg)));
    left.push(Span::styled(right_text, Style::default().fg(theme.muted).bg(bar_bg)));
    frame.render_widget(Paragraph::new(Line::from(left)), area);
}

pub fn bottom_bar(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let p = app.panel();
    let (mode, mode_color) = if !app.pending_keys.is_empty() {
        (format!(" {} ", format_sequence(&app.pending_keys)), theme.warning)
    } else if p.visual_anchor.is_some() {
        (" VISUAL ".into(), theme.palette.purple)
    } else if app.focus == Focus::Sidebar && app.view == View::Files {
        (" SIDEBAR ".into(), theme.info)
    } else {
        (" NORMAL ".into(), theme.palette.accent)
    };
    let mut spans = vec![Span::styled(mode, Style::default().bg(mode_color).fg(theme.bg).add_modifier(Modifier::BOLD))];

    let width = area.width as usize;
    if let Some(n) = app.current_notification() {
        let color = match n.level {
            Level::Info => theme.fg,
            Level::Warn => theme.warning,
            Level::Error => theme.error,
        };
        let room = width.saturating_sub(spans[0].content.width() + 2);
        spans.push(Span::styled(format!(" {}", truncate(&n.message, room)), Style::default().fg(color)));
    } else {
        let sort = format!("{}{}", p.sort.key.label(), if p.sort.reverse { " ↓" } else { "" });
        let summary = format!(
            " {} · panel {}/{} · sort {sort}{}",
            p.vfs.label(),
            app.active + 1,
            app.panels.len(),
            if p.show_hidden { " · hidden shown" } else { "" }
        );
        spans.push(Span::styled(summary, Style::default().fg(theme.muted)));
        let help = " ? help  : command  q quit ";
        let used: usize = spans.iter().map(|s| s.content.width()).sum();
        if used + help.width() < width {
            spans.push(Span::raw(" ".repeat(width - used - help.width())));
            spans.push(Span::styled(help, Style::default().fg(theme.muted)));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.surface)), area);
}
