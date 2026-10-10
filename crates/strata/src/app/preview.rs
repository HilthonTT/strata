//! Preview generation, done off the UI thread: directories, images, code
//! rendered Markdown, office documents and e-books, and hex dumps of
//! binary files.

use std::io::Read;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use ratatui::layout::Size;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{FilterType, Resize};
use strata_config::Theme;
use strata_core::sort::{sort_entries, SortOptions};
use strata_core::util::human_size;
use strata_core::{Entry, VfsRef};

use super::highlight::Highlighter;
use super::preview_docs::{self, Block};
use super::preview_hex;
use super::preview_markdown;
use crate::event::AppEvent;

const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// Lines kept for scrolling, and how many of them get syntax colours.
const MAX_LINES: usize = 5000;
const MAX_HIGHLIGHTED: usize = 2000;
const MAX_IMAGE_BYTES: u64 = 40 * 1024 * 1024;
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"];

pub enum PreviewContent {
    Empty,
    Loading,
    Text(Vec<String>),
    /// Syntax-highlighted source.
    Code(Vec<Line<'static>>),
    Dir(Vec<Entry>),
    Image(Box<Protocol>),
    Binary {
        size: u64,
    },
    Error(String),
}

pub fn is_image(entry: &Entry) -> bool {
    IMAGE_EXTS.contains(&entry.extension().as_str())
}

/// Which kinds of rich preview are turned on.
#[derive(Debug, Clone, Copy)]
pub struct PreviewOptions {
    pub markdown: bool,
    pub hex: bool,
}

pub struct PreviewJob {
    pub generation: u64,
    pub vfs: VfsRef,
    pub entry: Entry,
    pub show_hidden: bool,
    pub size: Size,
    pub picker: Option<Picker>,
    pub highlighter: Option<Arc<Highlighter>>,
    pub theme: Theme,
    pub options: PreviewOptions,
}

impl PreviewJob {
    pub fn spawn(self, tx: Sender<AppEvent>) {
        std::thread::spawn(move || {
            let generation = self.generation;
            let content = self.build();
            let _ = tx.send(AppEvent::Preview { generation, content });
        });
    }

    fn build(self) -> PreviewContent {
        let entry = &self.entry;
        if entry.is_dir() {
            return match self.vfs.read_dir(&entry.path) {
                Ok(mut entries) => {
                    entries.retain(|e| self.show_hidden || !e.is_hidden());
                    sort_entries(&mut entries, SortOptions::default());
                    PreviewContent::Dir(entries)
                }
                Err(e) => PreviewContent::Error(format!("{e:#}")),
            };
        }
        if let Some(picker) = self.picker.as_ref().filter(|_| is_image(entry) && entry.size <= MAX_IMAGE_BYTES) {
            return match load_image(&self.vfs, &entry.path, picker, self.size) {
                Ok(p) => PreviewContent::Image(Box::new(p)),
                Err(e) => PreviewContent::Error(e),
            };
        }
        let ext = entry.extension();
        if preview_docs::is_document(&ext) && entry.size <= preview_docs::MAX_DOC_BYTES {
            return self.document(&ext);
        }
        let mut buf = Vec::with_capacity(MAX_TEXT_BYTES.min(entry.size as usize + 1));
        let read =
            self.vfs.reader(&entry.path).and_then(|r| Ok(r.take(MAX_TEXT_BYTES as u64).read_to_end(&mut buf)?));
        if let Err(e) = read {
            return PreviewContent::Error(format!("{e:#}"));
        }
        if buf.is_empty() {
            return PreviewContent::Empty;
        }
        if looks_binary(&buf) {
            return if self.options.hex { self.hex(&buf) } else { PreviewContent::Binary { size: entry.size } };
        }
        let text = String::from_utf8_lossy(&buf);
        if self.options.markdown && preview_markdown::is_markdown(&ext) {
            let width = self.size.width.saturating_sub(1) as usize;
            let mut lines = preview_markdown::render(&text, width, &self.theme, self.highlighter.as_deref());
            lines.truncate(MAX_LINES);
            return PreviewContent::Code(lines);
        }
        let plain = |l: &str| l.replace('\t', "    ");
        if let Some(mut lines) =
            self.highlighter.as_ref().and_then(|h| h.highlight(&entry.name, &text, MAX_HIGHLIGHTED))
        {
            // Past the highlighted part, keep the rest as plain text.
            lines.extend(
                text.lines().skip(MAX_HIGHLIGHTED).take(MAX_LINES - MAX_HIGHLIGHTED).map(|l| Line::raw(plain(l))),
            );
            return PreviewContent::Code(lines);
        }
        PreviewContent::Text(text.lines().take(MAX_LINES).map(plain).collect())
    }
}

impl PreviewJob {
    /// A header with the size and executable type, then the first bytes.
    fn hex(&self, buf: &[u8]) -> PreviewContent {
        let muted = Style::default().fg(self.theme.muted);
        let mut header = format!(" binary · {}", human_size(self.entry.size));
        if let Some(arch) = strata_core::inspect::binary_arch(buf) {
            header.push_str(&format!(" · {arch}"));
        }
        let shown = buf.len().min(preview_hex::MAX_HEX_BYTES);
        let mut lines = vec![Line::styled(header, Style::default().fg(self.theme.warning)), Line::default()];
        lines.extend(preview_hex::dump(&buf[..shown], self.size.width as usize, &self.theme));
        if (shown as u64) < self.entry.size {
            lines.push(Line::styled(
                format!(" … first {} of {}", human_size(shown as u64), human_size(self.entry.size)),
                muted,
            ));
        }
        PreviewContent::Code(lines)
    }

    fn document(&self, ext: &str) -> PreviewContent {
        let mut data = Vec::new();
        if let Err(e) = self.vfs.reader(&self.entry.path).and_then(|mut r| Ok(r.read_to_end(&mut data)?)) {
            return PreviewContent::Error(format!("{e:#}"));
        }
        let blocks = match preview_docs::extract(data, ext) {
            Ok(blocks) if blocks.is_empty() => return PreviewContent::Text(vec!["(no text)".into()]),
            Ok(blocks) => blocks,
            Err(e) => return PreviewContent::Error(format!("{e:#}")),
        };
        let width = self.size.width.saturating_sub(1) as usize;
        let t = &self.theme;
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut previous_row = false;
        for block in blocks {
            let is_row = matches!(block, Block::Row(_));
            // Blank lines between paragraphs and around headings, not rows.
            if !lines.is_empty() && !(is_row && previous_row) {
                lines.push(Line::default());
            }
            previous_row = is_row;
            match block {
                Block::Heading(text) => {
                    let style = Style::default().fg(t.palette.accent).add_modifier(Modifier::BOLD);
                    lines.extend(preview_markdown::wrap(vec![Span::styled(text, style)], width, vec![], vec![]));
                }
                Block::Para(text) => {
                    let span = Span::styled(text, Style::default().fg(t.fg));
                    lines.extend(preview_markdown::wrap(vec![span], width, vec![], vec![]));
                }
                Block::Row(cells) => {
                    let mut spans = Vec::new();
                    for (i, cell) in cells.into_iter().enumerate() {
                        if i > 0 {
                            spans.push(Span::styled(" │ ", Style::default().fg(t.muted)));
                        }
                        spans.push(Span::styled(cell, Style::default().fg(t.fg)));
                    }
                    lines.push(Line::from(spans));
                }
            }
            if lines.len() >= MAX_LINES {
                break;
            }
        }
        PreviewContent::Code(lines)
    }
}

fn looks_binary(buf: &[u8]) -> bool {
    let sample = &buf[..buf.len().min(8192)];
    sample.contains(&0)
        || std::str::from_utf8(sample).is_err()
            && String::from_utf8_lossy(sample).matches('\u{FFFD}').count() > sample.len() / 20
}

fn load_image(vfs: &VfsRef, path: &std::path::Path, picker: &Picker, size: Size) -> Result<Protocol, String> {
    let mut bytes = Vec::new();
    vfs.reader(path).and_then(|mut r| Ok(r.read_to_end(&mut bytes)?)).map_err(|e| format!("{e:#}"))?;
    let img = image::load_from_memory(&bytes).map_err(|e| format!("cannot decode image: {e}"))?;
    picker
        .new_protocol(img, size, Resize::Scale(Some(FilterType::Triangle)))
        .map_err(|e| format!("cannot render image: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_binary() {
        assert!(looks_binary(&[0x7f, b'E', b'L', b'F', 0, 1]));
        assert!(!looks_binary("plain text\nwith ünïcode".as_bytes()));
    }
}
