//! NAS connections view: status, reachability and quick actions.

use std::time::SystemTime;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use strata_core::nas::Reachability;
use strata_core::util::human_size;

use super::{block, fit};
use crate::app::App;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let [summary, list, hint] =
        Layout::vertical([Constraint::Length(4), Constraint::Min(3), Constraint::Length(1)]).areas(area);

    let conns = &app.config.connections;
    let reachable = conns
        .iter()
        .filter(|c| app.nas.statuses.get(&c.name).is_some_and(|s| matches!(s.reach, Reachability::Up { .. })))
        .count();
    let mounted = conns
        .iter()
        .filter(|c| {
            app.nas.sessions.contains_key(&c.name) || app.nas.statuses.get(&c.name).is_some_and(|s| s.mounted.is_some())
        })
        .count();
    let (total, free) =
        app.metrics.disks.iter().filter(|d| d.network).fold((0, 0), |(t, f), d| (t + d.total, f + d.available));

    let b = block(&theme, app.config.general.border, "Overview", false);
    let inner = b.inner(summary);
    frame.render_widget(b, summary);
    let stat = |label: &str, value: String, color| {
        vec![
            Span::styled(format!(" {label} "), Style::default().fg(theme.muted)),
            Span::styled(value, Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::raw("   "),
        ]
    };
    let mut spans = stat("Connections", conns.len().to_string(), theme.fg);
    spans.extend(stat("Reachable", reachable.to_string(), theme.success));
    spans.extend(stat(
        "Unreachable",
        (app.nas.statuses.len().min(conns.len()) - reachable.min(conns.len())).to_string(),
        theme.error,
    ));
    spans.extend(stat("Connected", mounted.to_string(), theme.info));
    let storage = if total > 0 {
        format!(
            " Network storage: {} used of {} ({} free)",
            human_size(total - free),
            human_size(total),
            human_size(free)
        )
    } else {
        " Network storage: no mounted shares".into()
    };
    frame.render_widget(
        Paragraph::new(vec![Line::from(spans), Line::styled(storage, Style::default().fg(theme.muted))]),
        inner,
    );

    let b = block(&theme, app.config.general.border, "NAS connections", true);
    let inner = b.inner(list);
    frame.render_widget(b, list);
    app.layout.list = Some(list);

    if conns.is_empty() {
        let lines = vec![
            Line::styled(" No connections yet.", Style::default().fg(theme.fg)),
            Line::raw(""),
            Line::styled(
                " Press a to add one: smb://user@nas/share, nfs://nas/export or sftp://user@nas/path",
                Style::default().fg(theme.muted),
            ),
            Line::styled(" or add [[connections]] entries to config.toml (press e).", Style::default().fg(theme.muted)),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
    } else {
        let w = inner.width as usize;
        let (name_w, proto_w, status_w, seen_w) = (18, 6, 30, 14);
        let url_w = w.saturating_sub(3 + name_w + proto_w + status_w + seen_w);
        let header = format!(
            "   {}{}{}{}{}",
            fit("NAME", name_w),
            fit("TYPE", proto_w),
            fit("ADDRESS", url_w),
            fit("STATUS", status_w),
            fit("LAST SEEN", seen_w)
        );
        let mut lines = vec![Line::styled(header, Style::default().fg(theme.muted).add_modifier(Modifier::BOLD))];
        for (i, c) in conns.iter().enumerate() {
            let status = app.nas.statuses.get(&c.name);
            let session = app.nas.sessions.contains_key(&c.name);
            let (dot, color, text) = match (session, status) {
                (true, _) => ("●", theme.success, "connected (sftp)".to_string()),
                (_, Some(s)) if s.mounted.is_some() => {
                    ("●", theme.success, format!("mounted at {}", s.mounted.as_ref().unwrap().display()))
                }
                (_, Some(s)) => match &s.reach {
                    Reachability::Up { latency, port } => {
                        ("●", theme.info, format!("reachable :{port} · {} ms", latency.as_millis()))
                    }
                    Reachability::Down(e) => ("●", theme.error, format!("unreachable · {e}")),
                    Reachability::Unknown => ("○", theme.muted, "unknown".into()),
                },
                (_, None) => ("○", theme.muted, "checking…".into()),
            };
            let seen = app.nas.last_up.get(&c.name).map(|t| ago(*t)).unwrap_or_else(|| "never".into());
            let mut style = Style::default().fg(theme.fg);
            if i == app.nas.cursor {
                style = style.bg(theme.cursor_bg).add_modifier(Modifier::BOLD);
            }
            lines.push(Line::from(vec![
                Span::styled(format!(" {dot} "), style.fg(color)),
                Span::styled(fit(&c.name, name_w), style),
                Span::styled(fit(c.protocol.as_str(), proto_w), style),
                Span::styled(fit(&c.url(), url_w), style.fg(theme.muted)),
                Span::styled(format!("{} ", fit(&text, status_w - 1)), style.fg(color)),
                Span::styled(fit(&seen, seen_w), style.fg(theme.muted)),
            ]));
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }
    let keys = " enter connect/open · a add · t diagnose · u disconnect · r recheck · e edit config · 1 files";
    frame.render_widget(Paragraph::new(Line::styled(keys, Style::default().fg(theme.muted))), hint);
}

fn ago(t: SystemTime) -> String {
    let secs = SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86400),
    }
}
