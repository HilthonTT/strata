//! Docker containers view.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{block, fit};
use crate::app::App;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let [list, hint] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    let running = app
        .docker
        .containers
        .iter()
        .filter(|c| c.is_running())
        .count();
    let title = format!(
        "Docker · {running} running / {} total{}",
        app.docker.containers.len(),
        if app.docker.loading { " ⟳" } else { "" }
    );
    let b = block(&theme, app.config.general.border, &title, true);
    let inner = b.inner(list);
    frame.render_widget(b, list);
    app.layout.list = Some(list);

    if let Some(err) = &app.docker.error {
        let lines = vec![
            Line::styled(format!(" {err}"), Style::default().fg(theme.error)),
            Line::raw(""),
            Line::styled(
                " Is Docker installed and the daemon running?",
                Style::default().fg(theme.muted),
            ),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
    } else if app.docker.containers.is_empty() {
        let msg = if app.docker.loading || app.docker.last.is_none() {
            " loading…"
        } else {
            " no containers"
        };
        frame.render_widget(
            Paragraph::new(Line::styled(msg, Style::default().fg(theme.muted))),
            inner,
        );
    } else {
        let w = inner.width as usize;
        let (name_w, image_w, status_w, cpu_w, mem_w) = (22, 24, 22, 8, 22);
        let ports_w = w.saturating_sub(3 + name_w + image_w + status_w + cpu_w + mem_w);
        let header = format!(
            "   {}{}{}{}{}{}",
            fit("NAME", name_w),
            fit("IMAGE", image_w),
            fit("STATUS", status_w),
            fit("CPU", cpu_w),
            fit("MEMORY", mem_w),
            fit("PORTS", ports_w)
        );
        let mut lines = vec![Line::styled(
            header,
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD),
        )];
        let height = inner.height.saturating_sub(1) as usize;
        let offset = app.docker.cursor.saturating_sub(height.saturating_sub(1));
        for (i, c) in app
            .docker
            .containers
            .iter()
            .enumerate()
            .skip(offset)
            .take(height)
        {
            let dot_color = match c.state.as_str() {
                "running" => theme.success,
                "paused" => theme.warning,
                "restarting" => theme.info,
                _ => theme.muted,
            };
            let mut style = Style::default().fg(if c.is_running() {
                theme.fg
            } else {
                theme.muted
            });
            if i == app.docker.cursor {
                style = style.bg(theme.cursor_bg).add_modifier(Modifier::BOLD);
            }
            lines.push(Line::from(vec![
                Span::styled(" ● ", style.fg(dot_color)),
                Span::styled(fit(&c.name, name_w), style),
                Span::styled(fit(&c.image, image_w), style),
                Span::styled(fit(&c.status, status_w), style),
                Span::styled(fit(c.cpu.as_deref().unwrap_or("—"), cpu_w), style),
                Span::styled(fit(c.memory.as_deref().unwrap_or("—"), mem_w), style),
                Span::styled(fit(&c.ports, ports_w), style),
            ]));
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }
    let keys = " enter browse files · e shell · L logs · s start · S stop · r restart · p pause · x remove · 1 files";
    frame.render_widget(
        Paragraph::new(Line::styled(keys, Style::default().fg(theme.muted))),
        hint,
    );
}
