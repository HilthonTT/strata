//! Footer: running jobs, metadata of the hovered item and the clipboard.

use std::sync::atomic::Ordering;

use chrono::{DateTime, Local};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use strata_core::jobs::JobState;
use strata_core::ops::TransferMode;
use strata_core::util::{human_size, permissions_string};
use strata_core::EntryKind;

use super::{bar, block, truncate};
use crate::app::App;

pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let [jobs, meta, clip] = Layout::horizontal([
        Constraint::Percentage(40),
        Constraint::Percentage(35),
        Constraint::Percentage(25),
    ])
    .areas(area);
    draw_jobs(frame, app, jobs);
    draw_metadata(frame, app, meta);
    draw_clipboard(frame, app, clip);
}

fn draw_jobs(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let running = app.jobs.running();
    let title = if running > 0 {
        format!("Processes ({running})")
    } else {
        "Processes".into()
    };
    let b = block(theme, app.config.general.border, &title, false);
    let inner = b.inner(area);
    frame.render_widget(b, area);
    let width = inner.width as usize;
    let lines: Vec<Line> = app
        .jobs
        .jobs()
        .iter()
        .rev()
        .take(inner.height as usize)
        .map(|job| {
            let p = &job.progress;
            let (icon, color) = match job.state() {
                JobState::Running => ("●", theme.info),
                JobState::Done => ("✓", theme.success),
                JobState::Failed(_) => ("✗", theme.error),
                JobState::Cancelled => ("■", theme.warning),
            };
            let ratio = if job.state() == JobState::Done {
                1.0
            } else {
                p.ratio()
            };
            let bar_w = 10.min(width / 4);
            let (done, rest) = bar(ratio, bar_w);
            let detail = if job.is_running() {
                let total = p.total_bytes.load(Ordering::Relaxed);
                if total > 0 {
                    format!(
                        " {}/{}",
                        human_size(p.done_bytes.load(Ordering::Relaxed)),
                        human_size(total)
                    )
                } else {
                    format!(" {}", p.current())
                }
            } else {
                String::new()
            };
            let label_w = width.saturating_sub(bar_w + 9);
            Line::from(vec![
                Span::styled(format!(" {icon} "), Style::default().fg(color)),
                Span::styled(
                    truncate(&format!("{}{detail}", job.label), label_w),
                    Style::default().fg(theme.fg),
                ),
                Span::raw(
                    " ".repeat(
                        label_w.saturating_sub(
                            truncate(&format!("{}{detail}", job.label), label_w)
                                .chars()
                                .count(),
                        ),
                    ),
                ),
                Span::styled(done, Style::default().fg(color)),
                Span::styled(rest, Style::default().fg(theme.border)),
                Span::styled(
                    format!(" {:>3.0}%", ratio * 100.0),
                    Style::default().fg(theme.muted),
                ),
            ])
        })
        .collect();
    if lines.is_empty() {
        let hint = Line::styled(
            match app.keymap.keys_for(strata_config::Action::CancelJob) {
                Some(k) => format!(" no running processes · {k} cancels the latest"),
                None => " no running processes".into(),
            },
            Style::default().fg(theme.muted),
        );
        frame.render_widget(Paragraph::new(hint), inner);
    } else {
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

fn draw_metadata(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let b = block(theme, app.config.general.border, "Metadata", false);
    let inner = b.inner(area);
    frame.render_widget(b, area);
    let Some(e) = app.panel().hovered() else {
        return;
    };
    let kind = match e.kind {
        EntryKind::Dir => "directory".to_string(),
        EntryKind::File => {
            let ext = e.extension();
            if ext.is_empty() {
                "file".into()
            } else {
                format!("{ext} file")
            }
        }
        EntryKind::Symlink { to_dir: true } => "link → directory".into(),
        EntryKind::Symlink { .. } => "link".into(),
        EntryKind::Other => "special".into(),
    };
    let modified = e
        .modified
        .map(|t| {
            DateTime::<Local>::from(t)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "—".into());
    let inspected = app.inspection.path.as_ref() == Some(&e.path);
    let kind = match app.inspection.arch.as_ref().filter(|_| inspected) {
        Some(arch) => format!("{kind} · {arch}"),
        None => kind,
    };
    let mut rows = vec![
        ("Name", e.name.clone()),
        ("Type", kind),
        (
            "Size",
            if e.is_dir() {
                "—".into()
            } else {
                if e.size < 1024 {
                    human_size(e.size)
                } else {
                    format!("{} ({} B)", human_size(e.size), e.size)
                }
            },
        ),
        ("Modified", modified),
        (
            "Mode",
            e.mode
                .map(|m| format!("{} {:o}", permissions_string(m), m))
                .unwrap_or_else(|| "—".into()),
        ),
        ("Path", e.path.to_string_lossy().into_owned()),
    ];
    if app.config.general.md5_checksum && !e.is_dir() {
        let md5 = match app.inspection.md5.as_ref().filter(|_| inspected) {
            Some(Ok(sum)) => sum.clone(),
            Some(Err(err)) => format!("error: {err}"),
            None => "computing…".into(),
        };
        rows.insert(5, ("MD5", md5));
    }
    let value_w = inner.width.saturating_sub(11) as usize;
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!(" {k:<9} "), Style::default().fg(theme.muted)),
                Span::styled(truncate(&v, value_w), Style::default().fg(theme.fg)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_clipboard(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let title = match &app.clipboard {
        Some(c) if c.mode == TransferMode::Move => format!("Clipboard · cut {}", c.paths.len()),
        Some(c) => format!("Clipboard · copy {}", c.paths.len()),
        None => "Clipboard".into(),
    };
    let b = block(theme, app.config.general.border, &title, false);
    let inner = b.inner(area);
    frame.render_widget(b, area);
    let Some(clip) = &app.clipboard else {
        let hint = Line::styled(
            {
                let key = |a| app.keymap.keys_for(a).unwrap_or_else(|| "?".into());
                format!(
                    " empty · {} copy · {} cut",
                    key(strata_config::Action::Copy),
                    key(strata_config::Action::Cut)
                )
            },
            Style::default().fg(theme.muted),
        );
        return frame.render_widget(Paragraph::new(hint), inner);
    };
    let color = if clip.mode == TransferMode::Move {
        theme.warning
    } else {
        theme.success
    };
    let mut lines: Vec<Line> = clip
        .paths
        .iter()
        .take(inner.height as usize)
        .map(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            Line::styled(
                truncate(&format!(" {name}"), inner.width as usize),
                Style::default().fg(color),
            )
        })
        .collect();
    if clip.paths.len() > inner.height as usize {
        lines.pop();
        lines.push(Line::styled(
            format!(" … {} more", clip.paths.len() + 1 - inner.height as usize),
            Style::default().fg(theme.muted),
        ));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}
