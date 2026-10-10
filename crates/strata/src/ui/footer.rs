//! Footer: processes with progress bars, metadata of the hovered item and
//! the clipboard, laid out like superfile's.

use std::sync::atomic::Ordering;

use chrono::{DateTime, Local};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use strata_config::Action;
use strata_core::inspect::HashAlgo;
use strata_core::jobs::JobState;
use strata_core::ops::TransferMode;
use strata_core::util::{human_size, permissions_string};
use strata_core::EntryKind;

use super::{block, gradient_bar, icons, truncate, truncate_left};
use crate::app::App;

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let [jobs, meta, clip] =
        Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(33), Constraint::Percentage(33)])
            .areas(area);
    draw_jobs(frame, app, jobs);
    draw_metadata(frame, app, meta);
    draw_clipboard(frame, app, clip);
}

fn counter(text: String, app: &App) -> Line<'static> {
    Line::styled(text, Style::default().fg(app.theme.muted)).right_aligned()
}

fn draw_jobs(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let jobs = app.jobs.jobs();
    let mut b = block(theme, app.config.general.border, "Processes", false);
    if !jobs.is_empty() {
        b = b.title_bottom(counter(format!(" {}/{} ", app.jobs.running(), jobs.len()), app));
    }
    let inner = b.inner(area);
    frame.render_widget(b, area);
    if jobs.is_empty() {
        let hint = match app.keymap.keys_for(Action::CancelJob) {
            Some(k) => format!(" no running processes · {k} cancels the latest"),
            None => " no running processes".into(),
        };
        return frame.render_widget(Paragraph::new(Line::styled(hint, Style::default().fg(theme.muted))), inner);
    }

    // Each job takes two lines (name, then progress bar) and a spacer.
    let width = inner.width as usize;
    let mut lines = Vec::new();
    for job in jobs.iter().rev().take((inner.height as usize + 1) / 3) {
        let p = &job.progress;
        let state = job.state();
        let (icon, color) = match state {
            JobState::Running => ("\u{f110}", theme.info),
            JobState::Done => ("\u{f05d}", theme.success),
            JobState::Failed(_) => ("\u{f057}", theme.error),
            JobState::Cancelled => ("\u{f05e}", theme.warning),
        };
        let icon = if app.config.general.icons { icon } else { "●" };
        let ratio = if state == JobState::Done { 1.0 } else { p.ratio() };
        let detail = match &state {
            JobState::Running => {
                let total = p.total_bytes.load(Ordering::Relaxed);
                if total > 0 {
                    format!("  {}/{}", human_size(p.done_bytes.load(Ordering::Relaxed)), human_size(total))
                } else {
                    String::new()
                }
            }
            JobState::Failed(e) => format!("  {e}"),
            _ => String::new(),
        };
        lines.push(Line::from(vec![
            Span::styled(
                truncate(&format!(" {}{detail}", job.label), width.saturating_sub(3)),
                Style::default().fg(theme.fg),
            ),
            Span::styled(format!(" {icon}"), Style::default().fg(color)),
        ]));
        let bar_w = width.saturating_sub(7);
        let mut bar = vec![Span::raw(" ")];
        bar.extend(gradient_bar(ratio, bar_w, theme.palette.blue, theme.palette.accent, theme.surface));
        bar.push(Span::styled(format!(" {:>3.0}%", ratio * 100.0), Style::default().fg(theme.muted)));
        lines.push(Line::from(bar));
        lines.push(Line::raw(""));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_metadata(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let mut b = block(theme, app.config.general.border, "Metadata", false);
    let Some(e) = app.panel().hovered() else {
        let inner = b.inner(area);
        frame.render_widget(b, area);
        return frame
            .render_widget(Paragraph::new(Line::styled(" nothing selected", Style::default().fg(theme.muted))), inner);
    };
    let inspected = app.inspection.path.as_ref() == Some(&e.path);
    let kind = match e.kind {
        EntryKind::Dir => "Directory".to_string(),
        EntryKind::Symlink { to_dir: true } => "Link to directory".into(),
        EntryKind::Symlink { .. } => "Link".into(),
        EntryKind::Other => "Special file".into(),
        EntryKind::File => match e.extension().as_str() {
            "" => "File".into(),
            ext => format!("{} file", ext.to_uppercase()),
        },
    };
    let kind = match app.inspection.arch.as_ref().filter(|_| inspected) {
        Some(arch) => format!("{kind} · {arch}"),
        None => kind,
    };
    let modified = e
        .modified
        .map(|t| DateTime::<Local>::from(t).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "—".into());
    let mut rows = vec![
        ("FileName", e.name.clone()),
        ("FileType", kind),
        ("FileSize", if e.is_dir() { "—".into() } else { human_size(e.size) }),
        ("FileModifyDate", modified),
        ("Permissions", e.mode.map(|m| format!("{} ({:o})", permissions_string(m), m)).unwrap_or_else(|| "—".into())),
    ];
    let g = &app.config.general;
    for (on, algo, key) in
        [(g.md5_checksum, HashAlgo::Md5, "MD5Checksum"), (g.sha256_checksum, HashAlgo::Sha256, "SHA256Checksum")]
    {
        if !on || e.is_dir() {
            continue;
        }
        let value = match app.inspection.sums.as_ref().filter(|_| inspected) {
            Some(Ok(sums)) => sums.iter().find(|(a, _)| *a == algo).map(|(_, s)| s.clone()).unwrap_or_default(),
            Some(Err(err)) => format!("error: {err}"),
            None => "computing…".into(),
        };
        rows.push((key, value));
    }
    rows.push(("Path", e.path.to_string_lossy().into_owned()));
    let inner = b.inner(area);
    let shown = rows.len().min(inner.height as usize);
    b = b.title_bottom(counter(format!(" {shown}/{} ", rows.len()), app));
    frame.render_widget(b, area);

    let key_w = 16;
    let value_w = (inner.width as usize).saturating_sub(key_w + 1);
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|(k, v)| {
            let value = if k == "Path" { truncate_left(&v, value_w) } else { truncate(&v, value_w) };
            Line::from(vec![
                Span::styled(format!(" {k:<width$}", width = key_w - 1), Style::default().fg(theme.fg)),
                Span::styled(value, Style::default().fg(theme.muted)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_clipboard(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let mut b = block(theme, app.config.general.border, "Clipboard", false);
    let Some(clip) = &app.clipboard else {
        let inner = b.inner(area);
        frame.render_widget(b, area);
        let key = |a| app.keymap.keys_for(a).unwrap_or_else(|| "?".into());
        let hint = format!(" empty · {} copy · {} cut", key(Action::Copy), key(Action::Cut));
        return frame.render_widget(Paragraph::new(Line::styled(hint, Style::default().fg(theme.muted))), inner);
    };
    let verb = if clip.mode == TransferMode::Move { "cut" } else { "copy" };
    b = b.title_bottom(counter(format!(" {verb} · {} ", clip.paths.len()), app));
    let inner = b.inner(area);
    frame.render_widget(b, area);

    let height = inner.height as usize;
    let overflow = clip.paths.len() > height;
    let shown = if overflow { height.saturating_sub(1) } else { clip.paths.len() };
    let mut lines: Vec<Line> = clip
        .paths
        .iter()
        .take(shown)
        .map(|p| {
            let entry = clip.vfs.stat(p).ok();
            let (icon, color) = match &entry {
                Some(e) => (icons::icon(e, app.config.general.icons), icons::color(e, theme)),
                None => ("", theme.muted),
            };
            Line::from(vec![
                Span::styled(format!(" {icon} "), Style::default().fg(color)),
                Span::styled(
                    truncate_left(&p.to_string_lossy(), (inner.width as usize).saturating_sub(4)),
                    Style::default().fg(theme.fg),
                ),
            ])
        })
        .collect();
    if overflow {
        lines.push(Line::styled(
            format!(" {} item left....", clip.paths.len() - shown),
            Style::default().fg(theme.muted).add_modifier(Modifier::ITALIC),
        ));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}
