use chrono::{DateTime, Local};
use ratatui::layout::{Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;
use ratatui_image::Image;
use strata_core::util::human_size;

use super::{block, icons, truncate};
use crate::app::preview::PreviewContent;
use crate::app::App;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let theme = &app.theme;
    let hovered = app.panel().hovered().cloned();
    let title = hovered
        .as_ref()
        .map(|e| e.name.clone())
        .unwrap_or_else(|| "Preview".into());
    let mut b = block(
        theme,
        app.config.general.border,
        &truncate(&title, area.width.saturating_sub(6) as usize),
        false,
    );
    if let Some(e) = &hovered {
        let date = e.modified.map(|t| {
            DateTime::<Local>::from(t)
                .format(&app.config.general.date_format)
                .to_string()
        });
        let info = match (e.is_dir(), date) {
            (true, Some(d)) => format!(" {d} "),
            (false, Some(d)) => format!(" {} · {d} ", human_size(e.size)),
            (false, None) => format!(" {} ", human_size(e.size)),
            (true, None) => String::new(),
        };
        b = b.title_bottom(Line::styled(info, Style::default().fg(theme.muted)).right_aligned());
    }
    let inner = b.inner(area);
    frame.render_widget(b, area);
    app.preview_area = Size::new(inner.width, inner.height);
    let muted = Style::default().fg(theme.muted);

    match &app.preview {
        PreviewContent::Empty => {
            let msg = if hovered.is_some() { " empty file" } else { "" };
            frame.render_widget(Paragraph::new(Line::styled(msg, muted)), inner);
        }
        PreviewContent::Loading => {
            frame.render_widget(Paragraph::new(Line::styled(" …", muted)), inner)
        }
        PreviewContent::Error(e) => frame.render_widget(
            Paragraph::new(Line::styled(
                format!(" {e}"),
                Style::default().fg(theme.error),
            ))
            .wrap(Wrap { trim: false }),
            inner,
        ),
        PreviewContent::Binary { size } => {
            let lines = vec![
                Line::styled(" binary file", Style::default().fg(theme.warning)),
                Line::styled(format!(" {}", human_size(*size)), muted),
                Line::raw(""),
                Line::styled(
                    match app.keymap.keys_for(strata_config::Action::OpenWith) {
                        Some(k) => format!(" {k}  open with the system app"),
                        None => String::new(),
                    },
                    muted,
                ),
            ];
            frame.render_widget(Paragraph::new(lines), inner);
        }
        PreviewContent::Text(lines) => {
            let num_w = lines.len().to_string().len().max(2);
            let text: Vec<Line> = lines
                .iter()
                .take(inner.height as usize)
                .enumerate()
                .map(|(i, l)| {
                    Line::from(vec![
                        Span::styled(format!("{:>num_w$} ", i + 1), muted),
                        Span::styled(
                            truncate(l, inner.width.saturating_sub(num_w as u16 + 1) as usize),
                            Style::default().fg(theme.fg),
                        ),
                    ])
                })
                .collect();
            frame.render_widget(Paragraph::new(text), inner);
        }
        PreviewContent::Code(lines) => {
            let num_w = lines.len().to_string().len().max(2);
            let room = inner.width.saturating_sub(num_w as u16 + 1) as usize;
            let text: Vec<Line> = lines
                .iter()
                .take(inner.height as usize)
                .enumerate()
                .map(|(i, line)| {
                    let mut spans = vec![Span::styled(format!("{:>num_w$} ", i + 1), muted)];
                    spans.extend(clip_spans(line, room, theme.fg));
                    Line::from(spans)
                })
                .collect();
            frame.render_widget(Paragraph::new(text), inner);
        }
        PreviewContent::Dir(entries) => {
            if entries.is_empty() {
                frame.render_widget(
                    Paragraph::new(Line::styled(" empty directory", muted)),
                    inner,
                );
                return;
            }
            let icons_on = app.config.general.icons;
            let lines: Vec<Line> = entries
                .iter()
                .take(inner.height as usize)
                .map(|e| {
                    let style = Style::default().fg(icons::color(e, theme));
                    let style = if e.is_dir() {
                        style.add_modifier(Modifier::BOLD)
                    } else {
                        style
                    };
                    Line::styled(
                        truncate(
                            &format!(" {} {}", icons::icon(e, icons_on), e.name),
                            inner.width as usize,
                        ),
                        style,
                    )
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), inner);
        }
        PreviewContent::Image(protocol) => {
            frame.render_widget(Image::new(protocol), inner);
        }
    }
}

/// Cuts a highlighted line to `width` columns.
fn clip_spans(
    line: &Line<'static>,
    width: usize,
    default_fg: ratatui::style::Color,
) -> Vec<Span<'static>> {
    use unicode_width::UnicodeWidthChar;
    let mut out = Vec::new();
    let mut used = 0;
    for span in &line.spans {
        let mut text = String::new();
        for c in span.content.chars() {
            let w = c.width().unwrap_or(0);
            if used + w > width {
                break;
            }
            used += w;
            text.push(c);
        }
        let truncated = text.chars().count() < span.content.chars().count();
        let style = if span.style.fg.is_none() {
            span.style.fg(default_fg)
        } else {
            span.style
        };
        out.push(Span::styled(text, style));
        if truncated {
            break;
        }
    }
    out
}
