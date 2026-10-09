use chrono::{DateTime, Local};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use strata_core::util::human_size;
use unicode_width::UnicodeWidthStr;

use super::{block, fit, icons, panels_focused, truncate};
use crate::app::short_path;
use crate::app::App;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let n = app.panels.len() as u32;
    let rects = Layout::horizontal(vec![Constraint::Ratio(1, n); n as usize]).split(area);
    for (i, rect) in rects.iter().enumerate() {
        draw_panel(frame, app, i, *rect);
    }
    app.layout.panels = rects.to_vec();
}

fn draw_panel(frame: &mut Frame, app: &mut App, index: usize, area: Rect) {
    let focused = index == app.active && panels_focused(app);
    let theme = app.theme.clone();
    let border = app.config.general.border;
    let icons_on = app.config.general.icons;
    let date_format = app.config.general.date_format.clone();

    let panel = &mut app.panels[index];
    let location = if panel.vfs.is_local() {
        short_path(&panel.cwd)
    } else {
        format!("{}:{}", panel.vfs.label(), panel.cwd.display())
    };
    let title = truncate(&location, area.width.saturating_sub(6) as usize);
    let mut b = block(&theme, border, &title, focused);
    let mut info = format!(" {} ", panel.len());
    if !panel.marked.is_empty() {
        info = format!(" {} marked · {}", panel.marked.len(), info.trim_start());
    }
    if !panel.filter.is_empty() {
        info = format!(" /{} ·{info}", panel.filter);
    }
    b = b.title_bottom(
        Line::from(Span::styled(info, Style::default().fg(theme.muted))).right_aligned(),
    );
    let inner = b.inner(area);
    frame.render_widget(b, area);
    if inner.height < 2 {
        return;
    }

    if let Some(err) = &panel.error {
        let p = Paragraph::new(Line::from(Span::styled(
            format!(" {err}"),
            Style::default().fg(theme.error),
        )));
        frame.render_widget(p, inner);
        return;
    }

    let width = inner.width as usize;
    let show_date = width >= 46;
    let show_size = width >= 26;
    let size_w = 10;
    let date_w = 17;
    let name_w = width
        .saturating_sub(if show_size { size_w + 1 } else { 0 })
        .saturating_sub(if show_date { date_w + 1 } else { 0 });

    let mut header = fit(" Name", name_w);
    if show_size {
        header.push_str(&format!(" {:>size_w$}", "Size"));
    }
    if show_date {
        header.push_str(&format!(" {:<date_w$}", "Modified"));
    }
    let header_style = Style::default()
        .fg(theme.muted)
        .add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(Line::styled(header, header_style)),
        Rect { height: 1, ..inner },
    );

    let list_area = Rect {
        y: inner.y + 1,
        height: inner.height - 1,
        ..inner
    };
    let height = list_area.height as usize;
    panel.scroll_into_view(height);

    if panel.len() == 0 {
        let msg = if panel.filter.is_empty() {
            " empty directory"
        } else {
            " no matches"
        };
        frame.render_widget(
            Paragraph::new(Line::styled(msg, Style::default().fg(theme.muted))),
            list_area,
        );
        return;
    }

    let mut lines = Vec::with_capacity(height);
    for row in panel.offset..(panel.offset + height).min(panel.len()) {
        let entry = panel.entry_at(row).expect("row in range");
        let positions = &panel.visible[row].1;
        let is_cursor = row == panel.cursor;
        let marked = panel.is_marked(entry);

        let mut base = Style::default().fg(icons::color(entry, &theme));
        if is_cursor && focused {
            base = base.bg(theme.cursor_bg).add_modifier(Modifier::BOLD);
        } else if is_cursor {
            base = base.bg(theme.surface);
        }
        let mark_style = base.fg(theme.marked);

        let mut spans = vec![Span::styled(if marked { "▌" } else { " " }, mark_style)];
        let icon = icons::icon(entry, icons_on);
        spans.push(Span::styled(format!("{icon} "), base));
        let suffix = if entry.is_dir() { "/" } else { "" };
        let name_room = name_w.saturating_sub(icon.width() + 2);
        let name = truncate(&format!("{}{suffix}", entry.name), name_room);
        let name_style = if marked {
            mark_style.add_modifier(Modifier::BOLD)
        } else {
            base
        };
        // Highlight fuzzy-matched characters.
        for (ci, ch) in name.chars().enumerate() {
            let style = if positions.contains(&(ci as u32)) {
                name_style
                    .fg(theme.palette.accent)
                    .add_modifier(Modifier::UNDERLINED)
            } else {
                name_style
            };
            spans.push(Span::styled(ch.to_string(), style));
        }
        let pad = name_room.saturating_sub(name.width());
        let meta_style = if is_cursor {
            base.fg(theme.muted)
        } else {
            Style::default().fg(theme.muted)
        };
        spans.push(Span::styled(" ".repeat(pad), base));
        if show_size {
            let size = if entry.is_dir() {
                "—".to_string()
            } else {
                human_size(entry.size)
            };
            spans.push(Span::styled(format!(" {size:>size_w$}"), meta_style));
        }
        if show_date {
            let date = entry
                .modified
                .map(|t| DateTime::<Local>::from(t).format(&date_format).to_string())
                .unwrap_or_default();
            spans.push(Span::styled(format!(" {}", fit(&date, date_w)), meta_style));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), list_area);
}
