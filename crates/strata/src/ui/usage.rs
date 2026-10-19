//! Disk usage view: the scanned folder's items, largest first, with their
//! share of the folder.

use std::time::Duration;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use strata_core::util::human_size;

use super::{bar, block, fit, truncate_left};
use crate::app::{short_path, App};

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let [list, hint] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    let u = &app.usage;
    let scanning = u.scanning().map(|(root, n)| (root.to_path_buf(), n));
    // A new scan hides the old tree; a folder scanned again keeps it shown.
    let shown = u.node().filter(|_| !u.scanning_anew());
    let title = match (shown, &scanning) {
        (None, Some((root, _))) => format!("Disk usage · scanning {}…", short_path(root)),
        (None, None) => "Disk usage".into(),
        (Some(node), _) => {
            let path = u.path().map(|p| truncate_left(&short_path(&p), 60)).unwrap_or_default();
            let mut title = format!(
                "Disk usage · {path} · {} in {} files · by {}",
                human_size(node.size),
                node.files,
                u.sort.label()
            );
            match (&scanning, u.scanned) {
                (Some((_, n)), _) => title.push_str(&format!(" · rescanning… {n} items")),
                (None, Some(at)) => title.push_str(&format!(" · scanned {}", ago(at.elapsed()))),
                (None, None) => {}
            }
            title
        }
    };
    let b = block(&theme, app.border, &title, true);
    let inner = b.inner(list);
    frame.render_widget(b, list);
    app.layout.list = Some(list);

    let Some(node) = shown else {
        let text = match &scanning {
            Some((_, n)) => format!(" {n} items so far…"),
            None => " :du <folder> measures a folder; 5 opens this view on the focused panel's folder".into(),
        };
        frame.render_widget(Paragraph::new(Line::styled(text, Style::default().fg(theme.muted))), inner);
        return draw_hint(frame, app, hint);
    };
    let items = u.listed();
    let width = inner.width as usize;
    let bar_w = (width / 5).clamp(8, 30);
    let name_w = width.saturating_sub(10 + 2 + bar_w + 7 + 9 + 3);
    let header = format!(" {:>10}  {} {:>6} {:>9}  {}", "SIZE", fit("SHARE", bar_w), "", "FILES", fit("NAME", name_w));
    let mut lines = vec![Line::styled(header, Style::default().fg(theme.muted).add_modifier(Modifier::BOLD))];
    if items.is_empty() {
        lines.push(Line::styled(" (empty folder)", Style::default().fg(theme.muted)));
    }
    let height = inner.height.saturating_sub(1) as usize;
    let offset = u.cursor.saturating_sub(height.saturating_sub(1));
    for (i, item) in items.iter().enumerate().skip(offset).take(height) {
        let ratio = if node.size > 0 { item.size as f64 / node.size as f64 } else { 0.0 };
        let (done, rest) = bar(ratio, bar_w);
        let mut style = Style::default().fg(if item.is_dir { theme.dir } else { theme.fg });
        if i == u.cursor {
            style = style.bg(theme.cursor_bg).add_modifier(Modifier::BOLD);
        }
        let files = if item.is_dir { item.files.to_string() } else { String::new() };
        let name = format!("{}{}", item.name, if item.is_dir { "/" } else { "" });
        lines.push(Line::from(vec![
            Span::styled(format!(" {:>10}  ", human_size(item.size)), style.fg(theme.muted)),
            Span::styled(done, Style::default().fg(theme.chart[0])),
            Span::styled(rest, Style::default().fg(theme.border)),
            Span::styled(format!(" {:>5.1}%", ratio * 100.0), Style::default().fg(theme.muted)),
            Span::styled(format!(" {files:>9}  "), style.fg(theme.muted)),
            Span::styled(fit(&name, name_w), style),
        ]));
    }
    app.layout.list_offset = offset;
    frame.render_widget(Paragraph::new(lines), inner);
    draw_hint(frame, app, hint);
}

fn draw_hint(frame: &mut Frame, app: &App, area: Rect) {
    let keys = " l/enter open · h up · d d trash · D delete · r rescan · s sort · o show in files · 1 files";
    frame.render_widget(Paragraph::new(Line::styled(keys, Style::default().fg(app.theme.muted))), area);
}

/// `just now`, `5m ago`, `2h ago`.
fn ago(elapsed: Duration) -> String {
    match elapsed.as_secs() {
        0..60 => "just now".into(),
        s @ 60..3600 => format!("{}m ago", s / 60),
        s => format!("{}h ago", s / 3600),
    }
}
