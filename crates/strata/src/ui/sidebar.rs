//! Sidebar in superfile's style: a title, ruled section headers and a `>` cursor.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use strata_core::nas::Reachability;
use unicode_width::UnicodeWidthStr;

use super::{block, truncate};
use crate::app::sidebar::SidebarItem;
use crate::app::{App, Focus};

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar && app.overlay.is_none();
    let b = block(&app.theme, app.config.general.border, "", focused);
    let inner = b.inner(area);
    frame.render_widget(b, area);
    app.layout.sidebar = Some(area);
    if inner.height < 3 {
        return;
    }

    // Title line, then a blank line, then the items.
    let icons_on = app.config.general.icons;
    let theme = &app.theme;
    let title = format!("{}strata", if icons_on { "\u{f07c} " } else { "" });
    let pad = (inner.width as usize).saturating_sub(title.width()) / 2;
    let title_line = Line::styled(
        format!("{}{title}", " ".repeat(pad)),
        Style::default().fg(theme.palette.accent).add_modifier(Modifier::BOLD),
    );
    frame.render_widget(Paragraph::new(title_line), Rect { height: 1, ..inner });
    let list = Rect { y: inner.y + 2, height: inner.height - 2, ..inner };

    let height = list.height as usize;
    let sb = &mut app.sidebar;
    if sb.cursor < sb.offset {
        sb.offset = sb.cursor;
    } else if sb.cursor >= sb.offset + height {
        sb.offset = sb.cursor + 1 - height;
    }
    let theme = &app.theme;
    let width = list.width as usize;
    let active_dir = app.panels[app.active].vfs.is_local().then(|| app.panels[app.active].cwd.clone());
    let icon = |nerd: &'static str, plain: &'static str| if icons_on { nerd } else { plain };

    let lines: Vec<Line> = sb
        .items
        .iter()
        .enumerate()
        .skip(sb.offset)
        .take(height)
        .map(|(i, item)| {
            let is_cursor = i == sb.cursor && focused;
            let cursor = Span::styled(
                if is_cursor { "> " } else { "  " },
                Style::default().fg(theme.palette.accent).add_modifier(Modifier::BOLD),
            );
            let mut style = Style::default().fg(theme.fg);
            if is_cursor {
                style = style.add_modifier(Modifier::BOLD);
            }
            match item {
                SidebarItem::Header(h) => {
                    let label = format!(" {} {h} ", icon(header_icon(h), "•"));
                    let rule = "─".repeat(width.saturating_sub(label.width() + 1));
                    Line::from(vec![
                        Span::styled(label, Style::default().fg(theme.title).add_modifier(Modifier::BOLD)),
                        Span::styled(rule, Style::default().fg(theme.border)),
                    ])
                }
                SidebarItem::Place { label, path, icon: nerd } => {
                    let here = active_dir.as_ref() == Some(path);
                    let s = if here { style.fg(theme.palette.accent) } else { style };
                    Line::from(vec![
                        cursor,
                        Span::styled(truncate(&format!("{} {label}", icon(nerd, "")), width - 2), s),
                    ])
                }
                SidebarItem::Pinned(path) => {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    let here = active_dir.as_ref() == Some(path);
                    let s = if here { style.fg(theme.palette.accent) } else { style };
                    Line::from(vec![cursor, Span::styled(truncate(&name, width - 2), s)])
                }
                SidebarItem::Disk { label, ratio, network, .. } => {
                    let pct = format!("{:>3.0}%", ratio * 100.0);
                    let glyph = if *network { icon("\u{f0c2} ", "~ ") } else { icon("\u{f0a0} ", "") };
                    let name_w = width.saturating_sub(2 + glyph.width() + pct.width() + 1);
                    Line::from(vec![
                        cursor,
                        Span::styled(glyph, Style::default().fg(theme.muted)),
                        Span::styled(format!("{:<name_w$} ", truncate(label, name_w)), style),
                        Span::styled(pct, Style::default().fg(theme.level(*ratio))),
                    ])
                }
                SidebarItem::Connection(name) => {
                    let (dot, color) = match app.nas.statuses.get(name) {
                        _ if app.nas.sessions.contains_key(name) => ("●", theme.success),
                        Some(s) if s.mounted.is_some() => ("●", theme.success),
                        Some(s) if matches!(s.reach, Reachability::Up { .. }) => ("●", theme.info),
                        Some(_) => ("●", theme.error),
                        None => ("○", theme.muted),
                    };
                    Line::from(vec![
                        cursor,
                        Span::styled(format!("{dot} "), Style::default().fg(color)),
                        Span::styled(truncate(name, width.saturating_sub(4)), style),
                    ])
                }
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), list);
}

fn header_icon(header: &str) -> &'static str {
    match header {
        "Pinned" => "\u{f08d}",
        "Disks" => "\u{f0a0}",
        "Network" => "\u{f0c2}",
        _ => "\u{f015}",
    }
}
