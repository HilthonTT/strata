use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use strata_core::nas::Reachability;

use super::{bar, block, truncate};
use crate::app::sidebar::SidebarItem;
use crate::app::{App, Focus};

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.theme;
    let focused = app.focus == Focus::Sidebar && app.overlay.is_none();
    let b = block(theme, app.config.general.border, "strata", focused);
    let inner = b.inner(area);
    frame.render_widget(b, area);
    app.layout.sidebar = Some(area);

    let height = inner.height as usize;
    let sb = &mut app.sidebar;
    if sb.cursor < sb.offset {
        sb.offset = sb.cursor;
    } else if sb.cursor >= sb.offset + height {
        sb.offset = sb.cursor + 1 - height;
    }
    let width = inner.width as usize;
    let icons_on = app.config.general.icons;
    let active_dir = app.panels[app.active]
        .vfs
        .is_local()
        .then(|| app.panels[app.active].cwd.clone());

    let lines: Vec<Line> = sb
        .items
        .iter()
        .enumerate()
        .skip(sb.offset)
        .take(height)
        .map(|(i, item)| {
            let selected = i == sb.cursor && focused;
            let mut style = Style::default().fg(theme.fg);
            if selected {
                style = style.bg(theme.cursor_bg).add_modifier(Modifier::BOLD);
            }
            let icon =
                |nerd: &'static str, plain: &'static str| if icons_on { nerd } else { plain };
            match item {
                SidebarItem::Header(h) => Line::styled(
                    format!(" {h}"),
                    Style::default()
                        .fg(theme.title)
                        .add_modifier(Modifier::BOLD),
                ),
                SidebarItem::Place {
                    label,
                    path,
                    icon: nerd,
                } => {
                    let here = active_dir.as_ref() == Some(path);
                    let s = if here && !selected {
                        style.fg(theme.palette.accent)
                    } else {
                        style
                    };
                    Line::styled(
                        truncate(&format!("  {} {label}", icon(nerd, "•")), width),
                        s,
                    )
                }
                SidebarItem::Pinned(path) => {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string());
                    Line::styled(
                        truncate(&format!("  {} {name}", icon("\u{f08d}", "*")), width),
                        style,
                    )
                }
                SidebarItem::Disk {
                    label,
                    ratio,
                    network,
                    ..
                } => {
                    let glyph = if *network {
                        icon("\u{f0c2}", "~")
                    } else {
                        icon("\u{f0a0}", "▪")
                    };
                    let bar_w = 6usize;
                    let name_w = width.saturating_sub(bar_w + 6);
                    let (done, rest) = bar(*ratio, bar_w);
                    Line::from(vec![
                        Span::styled(
                            format!(
                                "  {glyph} {:<name_w$}",
                                truncate(label, name_w.saturating_sub(4))
                            ),
                            style,
                        ),
                        Span::styled(done, style.fg(theme.level(*ratio))),
                        Span::styled(rest, style.fg(theme.border)),
                    ])
                }
                SidebarItem::Connection(name) => {
                    let (dot, color) = match app.nas.statuses.get(name) {
                        Some(s) if s.mounted.is_some() => ("●", theme.success),
                        Some(s) if matches!(s.reach, Reachability::Up { .. }) => ("●", theme.info),
                        Some(_) => ("●", theme.error),
                        None => ("○", theme.muted),
                    };
                    let connected = app.nas.sessions.contains_key(name);
                    Line::from(vec![
                        Span::styled("  ", style),
                        Span::styled(dot, style.fg(if connected { theme.success } else { color })),
                        Span::styled(
                            truncate(&format!(" {name}"), width.saturating_sub(3)),
                            style,
                        ),
                    ])
                }
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}
