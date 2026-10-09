//! File panels, styled after superfile: a search line, `>` cursor, icons,
//! git markers and a mode/position footer on the border.

use chrono::{DateTime, Local};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use strata_config::Action;
use strata_core::git::GitState;
use strata_core::util::human_size;
use unicode_width::UnicodeWidthStr;

use super::{block, fit, icons, panels_focused, truncate, truncate_left};
use crate::app::{short_path, App};

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let n = app.panels.len() as u32;
    let rects = Layout::horizontal(vec![Constraint::Ratio(1, n); n as usize]).split(area);
    for (i, rect) in rects.iter().enumerate() {
        draw_panel(frame, app, i, *rect);
    }
    app.layout.panels = rects.to_vec();
}

fn draw_panel(frame: &mut Frame, app: &mut App, index: usize, area: Rect) {
    // Rows available below the border and the search line.
    let list_height = area.height.saturating_sub(3) as usize;
    app.panels[index].scroll_into_view(list_height);
    let app = &*app;

    let focused = index == app.active && panels_focused(app);
    let theme = &app.theme;
    let general = &app.config.general;
    let panel = &app.panels[index];

    let location = if panel.vfs.is_local() {
        short_path(&panel.cwd)
    } else {
        format!("{}:{}", panel.vfs.label(), panel.cwd.display())
    };
    let folder = if general.icons { "\u{f07b} " } else { "" };
    let title = format!("{folder}{}", truncate_left(&location, area.width.saturating_sub(8) as usize));

    let selecting = panel.visual_anchor.is_some() || !panel.marked.is_empty();
    let (mode_icon, mode) = if selecting { ("\u{f0c8} ", "Select") } else { ("\u{f06e} ", "Browser") };
    let mode = format!(" {}{mode} ", if general.icons { mode_icon } else { "" });
    let position =
        if panel.len() == 0 { " 0/0 ".to_string() } else { format!(" {}/{} ", panel.cursor + 1, panel.len()) };
    let muted = Style::default().fg(theme.muted);
    let b = block(theme, general.border, &title, focused)
        .title_bottom(Line::styled(mode, muted).right_aligned())
        .title_bottom(Line::styled(position, muted).right_aligned());
    let inner = b.inner(area);
    frame.render_widget(b, area);
    if inner.height < 2 {
        return;
    }

    // Search line.
    let search_icon = if general.icons { "\u{f002}" } else { "/" };
    let search = if panel.filter.is_empty() {
        let key = app.keymap.keys_for(Action::Filter).unwrap_or_else(|| "/".into());
        Line::styled(format!(" {search_icon} ({key}) Type something"), muted)
    } else {
        Line::from(vec![
            Span::styled(format!(" {search_icon} "), Style::default().fg(theme.palette.accent)),
            Span::styled(panel.filter.clone(), Style::default().fg(theme.fg)),
        ])
    };
    frame.render_widget(Paragraph::new(search), Rect { height: 1, ..inner });
    let list = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };

    if let Some(err) = &panel.error {
        let p = Paragraph::new(Line::styled(format!(" {err}"), Style::default().fg(theme.error)));
        return frame.render_widget(p, list);
    }
    if panel.len() == 0 {
        let msg = if panel.filter.is_empty() { "   empty directory" } else { "   no matches" };
        return frame.render_widget(Paragraph::new(Line::styled(msg, muted)), list);
    }

    let width = list.width as usize;
    let (size_w, date_w) = (9, 16);
    let extra = match general.extra_columns {
        0 => 0,
        1 if width >= 30 => size_w + 1,
        _ if width >= 48 => size_w + date_w + 2,
        _ if width >= 30 => size_w + 1,
        _ => 0,
    };
    let git_w = if general.git_status { 2 } else { 0 };
    let checkbox_w = if selecting && general.icons { 2 } else { 0 };
    let name_w = width.saturating_sub(2 + checkbox_w + extra + git_w);

    let lines: Vec<Line> = (panel.offset..(panel.offset + list.height as usize).min(panel.len()))
        .map(|row| {
            let entry = panel.entry_at(row).expect("row in range");
            let positions = &panel.visible[row].1;
            let is_cursor = row == panel.cursor;
            let marked = panel.is_marked(entry);
            let git = app.git_state(&panel.cwd, &entry.path);

            let mut name_style = Style::default().fg(icons::color(entry, theme));
            if git == Some(GitState::Ignored) {
                name_style = name_style.fg(theme.muted);
            }
            if marked {
                name_style = name_style.fg(theme.marked);
            }
            if is_cursor {
                name_style = name_style.add_modifier(Modifier::BOLD);
            }

            let cursor_style = Style::default().fg(if focused { theme.palette.accent } else { theme.muted });
            let mut spans =
                vec![Span::styled(if is_cursor { "> " } else { "  " }, cursor_style.add_modifier(Modifier::BOLD))];
            if checkbox_w > 0 {
                let (glyph, color) = if marked { ("\u{f0132} ", theme.marked) } else { ("\u{f0131} ", theme.muted) };
                spans.push(Span::styled(glyph, Style::default().fg(color)));
            }
            let icon = icons::icon(entry, general.icons);
            spans.push(Span::styled(format!("{icon} "), Style::default().fg(icons::color(entry, theme))));

            let name_room = name_w.saturating_sub(icon.width() + 1);
            let name = truncate(&entry.name, name_room);
            for (ci, ch) in name.chars().enumerate() {
                let style = if positions.contains(&(ci as u32)) {
                    name_style.fg(theme.palette.accent).add_modifier(Modifier::UNDERLINED)
                } else {
                    name_style
                };
                spans.push(Span::styled(ch.to_string(), style));
            }
            spans.push(Span::raw(" ".repeat(name_room.saturating_sub(name.width()))));

            if extra > 0 {
                let size = if entry.is_dir() { "—".to_string() } else { human_size(entry.size) };
                spans.push(Span::styled(format!(" {size:>size_w$}"), muted));
                if extra > size_w + 1 {
                    let date = entry
                        .modified
                        .map(|t| DateTime::<Local>::from(t).format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_default();
                    spans.push(Span::styled(format!(" {}", fit(&date, date_w)), muted));
                }
            }
            if git_w > 0 {
                let (marker, color) = match git {
                    Some(GitState::Modified) => ("M", theme.warning),
                    Some(GitState::Added) => ("A", theme.success),
                    Some(GitState::Untracked) => ("?", theme.success),
                    Some(GitState::Renamed) => ("R", theme.info),
                    Some(GitState::Deleted) => ("D", theme.error),
                    Some(GitState::Conflicted) => ("U", theme.error),
                    Some(GitState::Ignored) | None => (" ", theme.muted),
                };
                spans.push(Span::styled(format!(" {marker}"), Style::default().fg(color).add_modifier(Modifier::BOLD)));
            }
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), list);
}
