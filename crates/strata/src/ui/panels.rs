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
use strata_core::util::{human_size, permissions_string};
use strata_core::EntryKind;
use unicode_width::UnicodeWidthStr;

use super::{block, fit, icons, panels_focused, tag_color, truncate, truncate_left};
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

    let mut location = if panel.vfs.is_local() {
        short_path(&panel.cwd)
    } else {
        format!("{}:{}", panel.vfs.label(), panel.cwd.display())
    };
    if let Some(flat) = &panel.flat {
        location = format!("{location} · {}{}", flat.title, if flat.truncated { " (first matches)" } else { "" });
    }
    let folder = if general.icons { "\u{f07b} " } else { "" };
    let title = format!("{folder}{}", truncate_left(&location, area.width.saturating_sub(8) as usize));

    let selecting = panel.visual_anchor.is_some() || !panel.marked.is_empty();
    let (mode_icon, mode) = if selecting { ("\u{f0c8} ", "Select") } else { ("\u{f06e} ", "Browser") };
    let mode = format!(" {}{mode} ", if general.icons { mode_icon } else { "" });
    let position = match (panel.loading, panel.len()) {
        (true, _) => " ⟳ ".to_string(),
        (false, 0) => " 0/0 ".to_string(),
        (false, n) => format!(" {}/{n} ", panel.cursor + 1),
    };
    let muted = Style::default().fg(theme.muted);
    let b = block(theme, app.border, &title, focused)
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
        let msg = match (panel.loading, panel.filter.is_empty() && panel.flat.is_none()) {
            (true, _) if panel.flat.is_some() => "   searching…",
            (true, _) => "   loading…",
            (false, true) => "   empty directory",
            (false, false) => "   no matches · esc goes back",
        };
        return frame.render_widget(Paragraph::new(Line::styled(msg, muted)), list);
    }

    let width = list.width as usize;
    let date_format = general.date_format.as_str();
    // Size, date and permissions, each with a leading space.
    let (size_w, date_w, perm_w) = (9, date_width(date_format), 9);
    let git_w = if general.git_status { 2 } else { 0 };
    let checkbox_w = if selecting && general.icons { 2 } else { 0 };
    // Folder sizes are pointless without the size column.
    let wanted = general.extra_columns.max(general.dir_sizes as u8);
    let shown = extra_columns(
        wanted,
        width,
        2 + checkbox_w + git_w,
        general.name_percent,
        &[size_w + 1, date_w + 1, perm_w + 1],
    );
    let extra = [size_w + 1, date_w + 1, perm_w + 1][..shown].iter().sum::<usize>();
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

            // Coloured dots for tags, after the name.
            let tags = app.tags.get(&entry.path);
            let dots = tags.len().min(3);
            let name_room = name_w.saturating_sub(icon.width() + 1 + if dots > 0 { dots + 1 } else { 0 });
            let label = panel.label(entry);
            let name = truncate(&label, name_room);
            for (ci, ch) in name.chars().enumerate() {
                let style = if positions.contains(&(ci as u32)) {
                    name_style.fg(theme.palette.accent).add_modifier(Modifier::UNDERLINED)
                } else {
                    name_style
                };
                spans.push(Span::styled(ch.to_string(), style));
            }
            if dots > 0 {
                spans.push(Span::raw(" "));
                for tag in &tags[..dots] {
                    spans.push(Span::styled("●", Style::default().fg(tag_color(tag, theme))));
                }
            }
            spans.push(Span::raw(" ".repeat(name_room.saturating_sub(name.width()))));

            if shown >= 1 {
                let size = match (entry.kind, app.dir_size(&entry.path)) {
                    (EntryKind::Dir, Some(size)) => human_size(size),
                    (EntryKind::Dir, None) if general.dir_sizes && panel.vfs.is_local() => "…".to_string(),
                    _ if entry.is_dir() => "—".to_string(),
                    _ => human_size(entry.size),
                };
                spans.push(Span::styled(format!(" {size:>size_w$}"), muted));
            }
            if shown >= 2 {
                let date = entry
                    .modified
                    .map(|t| DateTime::<Local>::from(t).format(date_format).to_string())
                    .unwrap_or_default();
                spans.push(Span::styled(format!(" {}", fit(&date, date_w)), muted));
            }
            if shown >= 3 {
                let perms = entry.mode.map(permissions_string).unwrap_or_else(|| "—".into());
                spans.push(Span::styled(format!(" {}", fit(&perms, perm_w)), muted));
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

/// How many of the extra columns (size, date, permissions; `widths` in
/// that order) to show, up to `wanted`. Names keep `name_percent` of the
/// width, or 20 columns when it is 0.
fn extra_columns(wanted: u8, width: usize, fixed: usize, name_percent: u16, widths: &[usize]) -> usize {
    let name_min = match name_percent {
        0 => 20,
        p => width * p.clamp(10, 100) as usize / 100,
    };
    let mut room = width.saturating_sub(fixed + name_min);
    let mut shown = 0;
    for &w in widths.iter().take(wanted as usize) {
        if w > room {
            break;
        }
        room -= w;
        shown += 1;
    }
    shown
}

/// Columns a date takes in `format`, measured on a sample date.
fn date_width(format: &str) -> usize {
    use chrono::TimeZone;
    let sample = Local.with_ymd_and_hms(2026, 12, 31, 23, 59, 59).single().unwrap_or_else(Local::now);
    // An invalid format makes chrono's Display fail, so format via `write!`.
    let mut out = String::new();
    if std::fmt::Write::write_fmt(&mut out, format_args!("{}", sample.format(format))).is_err() {
        return 16;
    }
    out.width().clamp(1, 40)
}

#[cfg(test)]
mod tests {
    use super::{date_width, extra_columns};

    const WIDTHS: [usize; 3] = [10, 17, 10];

    #[test]
    fn columns_fit_the_panel() {
        assert_eq!(extra_columns(2, 60, 4, 0, &WIDTHS), 2);
        assert_eq!(extra_columns(3, 60, 4, 0, &WIDTHS), 2, "permissions need more room");
        assert_eq!(extra_columns(3, 80, 4, 0, &WIDTHS), 3);
        assert_eq!(extra_columns(2, 40, 4, 0, &WIDTHS), 1);
        assert_eq!(extra_columns(2, 30, 4, 0, &WIDTHS), 0);
        assert_eq!(extra_columns(2, 60, 4, 50, &WIDTHS), 1, "the date would squeeze names");
        assert_eq!(extra_columns(2, 60, 4, 90, &WIDTHS), 0);
        assert_eq!(extra_columns(0, 200, 4, 0, &WIDTHS), 0);
    }

    #[test]
    fn date_width_follows_the_format() {
        assert_eq!(date_width("%Y-%m-%d %H:%M"), 16);
        assert_eq!(date_width("%d/%m"), 5);
    }
}
