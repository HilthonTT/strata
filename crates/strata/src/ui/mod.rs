//! Rendering. Every function here reads the [`App`]; the only state it
//! writes back is layout information (scroll offsets, hit-test areas).

mod connections;
mod dashboard;
mod docker;
mod footer;
mod icons;
mod panels;
mod popup;
mod preview;
mod sidebar;
mod status;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;
use strata_config::{BorderStyle, Theme};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Focus, View};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(app.theme.bg).fg(app.theme.fg)), area);

    let show_footer = app.config.general.footer && area.height >= 24 && app.view == View::Files;
    let [top, main, footer, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(if show_footer { 10 } else { 0 }),
        Constraint::Length(1),
    ])
    .areas(area);

    status::top_bar(frame, app, top);
    app.layout = Default::default();
    match app.view {
        View::Files => draw_files(frame, app, main),
        View::Dashboard => dashboard::draw(frame, app, main),
        View::Docker => docker::draw(frame, app, main),
        View::Connections => connections::draw(frame, app, main),
    }
    if show_footer {
        footer::draw(frame, app, footer);
    }
    status::bottom_bar(frame, app, bottom);
    popup::draw(frame, app, area);
}

fn draw_files(frame: &mut Frame, app: &mut App, area: Rect) {
    let general = &app.config.general;
    let show_sidebar = general.sidebar && area.width >= 90;
    let show_preview = general.preview && area.width >= 70;
    let plugin_panels = app.visible_plugin_panels().len() as u16;
    let plugin_width = if area.width >= 120 { 32 * plugin_panels } else { 0 };

    let mut constraints = Vec::new();
    if show_sidebar {
        constraints.push(Constraint::Length(24));
    }
    constraints.push(Constraint::Min(20));
    if show_preview {
        constraints.push(Constraint::Percentage(if app.panels.len() > 2 { 28 } else { 34 }));
    }
    if plugin_width > 0 {
        constraints.push(Constraint::Length(plugin_width));
    }
    let columns = Layout::horizontal(constraints).split(area);
    let mut i = 0;
    if show_sidebar {
        sidebar::draw(frame, app, columns[i]);
        i += 1;
    }
    panels::draw(frame, app, columns[i]);
    i += 1;
    if show_preview {
        preview::draw(frame, app, columns[i]);
        i += 1;
    }
    if plugin_width > 0 {
        draw_plugin_panels(frame, app, columns[i]);
    }
}

fn draw_plugin_panels(frame: &mut Frame, app: &App, area: Rect) {
    let panels = app.visible_plugin_panels();
    let cols = Layout::horizontal(vec![Constraint::Ratio(1, panels.len() as u32); panels.len()]).split(area);
    for ((title, lines), rect) in panels.into_iter().zip(cols.iter()) {
        let block = block(&app.theme, app.config.general.border, &title, false);
        let text: Vec<Line> = lines.iter().map(|l| Line::raw(l.clone())).collect();
        frame.render_widget(Paragraph::new(text).block(block).style(Style::default().fg(app.theme.fg)), *rect);
    }
}

/// A bordered block in the theme's style.
pub fn block<'a>(theme: &Theme, style: BorderStyle, title: &str, focused: bool) -> Block<'a> {
    let border_type = match style {
        BorderStyle::Plain => BorderType::Plain,
        BorderStyle::Rounded => BorderType::Rounded,
        BorderStyle::Double => BorderType::Double,
        BorderStyle::Thick => BorderType::Thick,
    };
    let color = if focused { theme.border_focus } else { theme.border };
    let title_style = if focused {
        Style::default().fg(theme.title).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.muted)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(color))
        .style(Style::default().bg(theme.bg));
    if title.is_empty() {
        block
    } else {
        block.title(Span::styled(format!(" {title} "), title_style))
    }
}

pub fn panels_focused(app: &App) -> bool {
    app.focus == Focus::Panels && app.overlay.is_none()
}

/// Truncates to `width` columns with an ellipsis.
pub fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Truncates from the left: `…/code/strata`.
pub fn truncate_left(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out: Vec<char> = Vec::new();
    let mut used = 1;
    for c in s.chars().rev() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out.into_iter().rev().collect()
}

/// A progress bar whose filled part fades from `from` to `to`.
pub fn gradient_bar(ratio: f64, width: usize, from: Color, to: Color, empty: Color) -> Vec<Span<'static>> {
    let filled = (ratio.clamp(0.0, 1.0) * width as f64).round() as usize;
    let mix = |a: u8, b: u8, t: f64| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
    (0..width)
        .map(|i| {
            if i >= filled {
                return Span::styled("█", Style::default().fg(empty));
            }
            let t = if width > 1 { i as f64 / (width - 1) as f64 } else { 0.0 };
            let color = match (from, to) {
                (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
                    Color::Rgb(mix(r1, r2, t), mix(g1, g2, t), mix(b1, b2, t))
                }
                _ => from,
            };
            Span::styled("█", Style::default().fg(color))
        })
        .collect()
}

/// Pads or truncates to exactly `width` columns.
pub fn fit(s: &str, width: usize) -> String {
    let t = truncate(s, width);
    let pad = width.saturating_sub(t.width());
    format!("{t}{}", " ".repeat(pad))
}

/// A text progress bar like `████▌░░░`.
pub fn bar(ratio: f64, width: usize) -> (String, String) {
    let ratio = ratio.clamp(0.0, 1.0);
    let filled = ratio * width as f64;
    let full = filled.floor() as usize;
    let mut done = "█".repeat(full);
    if full < width && filled - full as f64 >= 0.5 {
        done.push('▌');
    }
    let rest = width.saturating_sub(done.chars().count());
    (done, "░".repeat(rest))
}

pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_helpers() {
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate_left("/home/me/code/strata", 10), "…de/strata");
        assert_eq!(gradient_bar(0.5, 4, Color::Red, Color::Blue, Color::Gray).len(), 4);
        assert_eq!(fit("ab", 4), "ab  ");
        assert_eq!(bar(0.5, 4), ("██".to_string(), "░░".to_string()));
        assert_eq!(bar(1.0, 3).0, "███");
    }
}
