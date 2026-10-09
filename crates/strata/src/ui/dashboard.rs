//! Dashboard: disk capacity, free-space chart, disk I/O, memory pressure
//! and disk usage of the current directory.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Bar, BarChart, BarGroup, Paragraph, Sparkline};
use ratatui::Frame;
use strata_core::util::human_size;
use strata_sys::Pressure;

use super::{bar, block, fit, truncate};
use crate::app::{short_path, App};

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let [top, middle, bottom] =
        Layout::vertical([Constraint::Percentage(34), Constraint::Percentage(33), Constraint::Min(6)]).areas(area);
    let [disks, memory] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(top);
    let [chart, io] = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(middle);
    draw_disks(frame, app, disks);
    draw_memory(frame, app, memory);
    draw_free_chart(frame, app, chart);
    draw_io(frame, app, io);
    draw_usage(frame, app, bottom);
}

fn draw_disks(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let b = block(theme, app.config.general.border, "Disks", false);
    let inner = b.inner(area);
    frame.render_widget(b, area);
    let width = inner.width as usize;
    let name_w = (width / 4).clamp(6, 22);
    let bar_w = width.saturating_sub(name_w + 30).max(5);
    let lines: Vec<Line> = app
        .metrics
        .disks
        .iter()
        .take(inner.height as usize)
        .map(|d| {
            let ratio = d.used_ratio();
            let (done, rest) = bar(ratio, bar_w);
            let tag = if d.network {
                " net"
            } else if d.removable {
                " usb"
            } else {
                ""
            };
            Line::from(vec![
                Span::styled(fit(&format!(" {}", d.mount_point.display()), name_w), Style::default().fg(theme.fg)),
                Span::styled(done, Style::default().fg(theme.level(ratio))),
                Span::styled(rest, Style::default().fg(theme.border)),
                Span::styled(
                    format!(" {:>3.0}% {:>9} free{tag}", ratio * 100.0, human_size(d.available)),
                    Style::default().fg(theme.muted),
                ),
            ])
        })
        .collect();
    if lines.is_empty() {
        return frame
            .render_widget(Paragraph::new(Line::styled(" collecting…", Style::default().fg(theme.muted))), inner);
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_memory(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let m = &app.metrics.memory;
    let pressure_color = match m.pressure {
        Pressure::Normal => theme.success,
        Pressure::Elevated => theme.warning,
        Pressure::High | Pressure::Critical => theme.error,
    };
    let b = block(theme, app.config.general.border, "Memory", false);
    let inner = b.inner(area);
    frame.render_widget(b, area);
    if m.total == 0 {
        return frame
            .render_widget(Paragraph::new(Line::styled(" collecting…", Style::default().fg(theme.muted))), inner);
    }
    let [text, spark] = Layout::vertical([Constraint::Length(5), Constraint::Min(1)]).areas(inner);
    let bar_w = (inner.width as usize).saturating_sub(30).max(5);
    let row = |label: &str, ratio: f64, used: u64, total: u64| {
        let (done, rest) = bar(ratio, bar_w);
        Line::from(vec![
            Span::styled(format!(" {label:<5}"), Style::default().fg(theme.fg)),
            Span::styled(done, Style::default().fg(theme.level(ratio))),
            Span::styled(rest, Style::default().fg(theme.border)),
            Span::styled(
                format!(" {:>3.0}% {}/{}", ratio * 100.0, human_size(used), human_size(total)),
                Style::default().fg(theme.muted),
            ),
        ])
    };
    let mut lines = vec![row("RAM", m.used_ratio(), m.used, m.total)];
    if m.swap_total > 0 {
        lines.push(row("Swap", m.swap_ratio(), m.swap_used, m.swap_total));
    }
    lines.push(Line::from(vec![
        Span::styled(" Pressure ", Style::default().fg(theme.fg)),
        Span::styled(m.pressure.label(), Style::default().fg(pressure_color).add_modifier(Modifier::BOLD)),
    ]));
    match m.psi {
        Some(psi) => lines.push(Line::styled(
            format!(
                " PSI some {:.2}/{:.2}/{:.2}  full {:.2}/{:.2}  (10s/60s/300s %)",
                psi.some_avg10, psi.some_avg60, psi.some_avg300, psi.full_avg10, psi.full_avg60
            ),
            Style::default().fg(theme.muted),
        )),
        None => {
            lines.push(Line::styled(" PSI unavailable — estimated from free memory", Style::default().fg(theme.muted)))
        }
    }
    frame.render_widget(Paragraph::new(lines), text);
    let history = m.usage_history.as_vec();
    let start = history.len().saturating_sub(spark.width as usize);
    frame.render_widget(
        Sparkline::default().data(&history[start..]).max(100).style(Style::default().fg(theme.chart[1])),
        spark,
    );
}

fn draw_free_chart(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let b = block(theme, app.config.general.border, "Free space (GiB)", false);
    let inner = b.inner(area);
    frame.render_widget(b, area);
    let disks: Vec<_> = app.metrics.disks.iter().filter(|d| d.total > 0).take(8).collect();
    if disks.is_empty() {
        return;
    }
    let bar_width = ((inner.width as usize / disks.len()).saturating_sub(1)).clamp(3, 9) as u16;
    let bars: Vec<Bar> = disks
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let gib = d.available / (1024 * 1024 * 1024);
            let label =
                d.mount_point.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
            Bar::default()
                .value(gib)
                .label(Line::from(truncate(&label, bar_width as usize)))
                .style(Style::default().fg(theme.chart[i % theme.chart.len()]))
                .value_style(Style::default().fg(theme.bg).bg(theme.chart[i % theme.chart.len()]))
        })
        .collect();
    let chart = BarChart::default()
        .data(BarGroup::default().bars(&bars))
        .bar_width(bar_width)
        .bar_gap(1)
        .direction(Direction::Vertical);
    frame.render_widget(chart, inner);
}

fn draw_io(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let b = block(theme, app.config.general.border, "Disk I/O", false);
    let inner = b.inner(area);
    frame.render_widget(b, area);
    let io = &app.metrics.io;
    if io.is_empty() {
        let msg = " no block devices reported";
        return frame.render_widget(Paragraph::new(Line::styled(msg, Style::default().fg(theme.muted))), inner);
    }
    let detailed = io.iter().any(|s| s.detailed);
    let header = if detailed {
        format!(
            " {:<10}{:>8}{:>8}{:>11}{:>11}{:>9}{:>9}{:>6}",
            "device", "r/s", "w/s", "read", "write", "r lat", "w lat", "util"
        )
    } else {
        format!(" {:<10}{:>11}{:>11}", "device", "read", "write")
    };
    let table_h = (io.len() as u16 + 1).min(inner.height.saturating_sub(4).max(2));
    let [table, sparks] = Layout::vertical([Constraint::Length(table_h), Constraint::Min(0)]).areas(inner);
    let mut lines = vec![Line::styled(header, Style::default().fg(theme.muted).add_modifier(Modifier::BOLD))];
    for s in io.iter().take(table_h.saturating_sub(1) as usize) {
        let rate = |b: f64| format!("{}/s", human_size(b as u64));
        let text = if detailed {
            format!(
                " {:<10}{:>8.0}{:>8.0}{:>11}{:>11}{:>7.2}ms{:>7.2}ms{:>5.0}%",
                truncate(&s.device, 10),
                s.read_iops,
                s.write_iops,
                rate(s.read_bytes_per_sec),
                rate(s.write_bytes_per_sec),
                s.read_latency_ms,
                s.write_latency_ms,
                s.utilization
            )
        } else {
            format!(
                " {:<10}{:>11}{:>11}",
                truncate(&s.device, 10),
                rate(s.read_bytes_per_sec),
                rate(s.write_bytes_per_sec)
            )
        };
        let busy = s.utilization > 80.0;
        lines.push(Line::styled(text, Style::default().fg(if busy { theme.warning } else { theme.fg })));
    }
    frame.render_widget(Paragraph::new(lines), table);

    // Sparklines for the busiest device.
    if sparks.height >= 2 && detailed {
        let busiest = io.iter().max_by_key(|s| s.iops_history.max()).expect("non-empty");
        let [a, b] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(sparks);
        let iops = busiest.iops_history.as_vec();
        let lat = busiest.latency_history.as_vec();
        let label_style = Style::default().fg(theme.muted);
        let [al, ad] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(a);
        let [bl, bd] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(b);
        frame.render_widget(
            Paragraph::new(Line::styled(
                format!(" {} IOPS (peak {})", busiest.device, busiest.iops_history.max()),
                label_style,
            )),
            al,
        );
        frame.render_widget(
            Paragraph::new(Line::styled(
                format!(" latency (peak {:.2} ms)", busiest.latency_history.max() as f64 / 1000.0),
                label_style,
            )),
            bl,
        );
        let tail = |v: &Vec<u64>, w: u16| v[v.len().saturating_sub(w as usize)..].to_vec();
        frame.render_widget(
            Sparkline::default().data(tail(&iops, ad.width)).style(Style::default().fg(theme.chart[0])),
            ad,
        );
        frame.render_widget(
            Sparkline::default().data(tail(&lat, bd.width)).style(Style::default().fg(theme.chart[5])),
            bd,
        );
    }
}

fn draw_usage(frame: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme.clone();
    let root = app.panel().cwd.clone();
    let title = match (&app.dashboard.usage, &app.dashboard.scanning) {
        (_, Some(scanning)) => format!("Disk usage · scanning {}…", short_path(scanning)),
        (Some(u), None) => {
            format!("Disk usage · {} · {} in {} files", short_path(&u.root), human_size(u.total), u.files)
        }
        (None, None) if !app.panel().vfs.is_local() => "Disk usage · local directories only".into(),
        (None, None) => format!("Disk usage · {}", short_path(&root)),
    };
    let b = block(&theme, app.config.general.border, &title, true);
    let inner = b.inner(area);
    frame.render_widget(b, area);
    app.layout.list = Some(Rect { y: area.y.saturating_sub(1), ..area });
    let Some(usage) = &app.dashboard.usage else {
        return;
    };
    let height = inner.height as usize;
    let cursor = app.dashboard.cursor;
    let offset = cursor.saturating_sub(height.saturating_sub(1));
    let width = inner.width as usize;
    let name_w = (width / 3).clamp(10, 40);
    let bar_w = width.saturating_sub(name_w + 22).max(4);
    let lines: Vec<Line> = usage
        .items
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, item)| {
            let ratio = if usage.total > 0 { item.size as f64 / usage.total as f64 } else { 0.0 };
            let (done, rest) = bar(ratio, bar_w);
            let mut style = Style::default().fg(if item.is_dir { theme.dir } else { theme.fg });
            if i == cursor {
                style = style.bg(theme.cursor_bg).add_modifier(Modifier::BOLD);
            }
            let name = format!(" {}{}", item.name, if item.is_dir { "/" } else { "" });
            Line::from(vec![
                Span::styled(fit(&name, name_w), style),
                Span::styled(format!("{:>10} ", human_size(item.size)), style.fg(theme.muted)),
                Span::styled(done, Style::default().fg(theme.chart[0])),
                Span::styled(rest, Style::default().fg(theme.border)),
                Span::styled(format!(" {:>5.1}%", ratio * 100.0), Style::default().fg(theme.muted)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}
