//! Preview generation, done off the UI thread.

use std::io::Read;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use ratatui::layout::Size;
use ratatui::text::Line;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{FilterType, Resize};
use strata_core::sort::{sort_entries, SortOptions};
use strata_core::{Entry, VfsRef};

use super::highlight::Highlighter;
use crate::event::AppEvent;

const MAX_TEXT_BYTES: usize = 128 * 1024;
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

pub struct PreviewJob {
    pub generation: u64,
    pub vfs: VfsRef,
    pub entry: Entry,
    pub show_hidden: bool,
    pub size: Size,
    pub picker: Option<Picker>,
    pub highlighter: Option<Arc<Highlighter>>,
}

impl PreviewJob {
    pub fn spawn(self, tx: Sender<AppEvent>) {
        std::thread::spawn(move || {
            let generation = self.generation;
            let content = self.build();
            let _ = tx.send(AppEvent::Preview {
                generation,
                content,
            });
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
        if let Some(picker) = self
            .picker
            .as_ref()
            .filter(|_| is_image(entry) && entry.size <= MAX_IMAGE_BYTES)
        {
            return match load_image(&self.vfs, &entry.path, picker, self.size) {
                Ok(p) => PreviewContent::Image(Box::new(p)),
                Err(e) => PreviewContent::Error(e),
            };
        }
        let mut buf = Vec::with_capacity(MAX_TEXT_BYTES.min(entry.size as usize + 1));
        let read = self
            .vfs
            .reader(&entry.path)
            .and_then(|r| Ok(r.take(MAX_TEXT_BYTES as u64).read_to_end(&mut buf)?));
        if let Err(e) = read {
            return PreviewContent::Error(format!("{e:#}"));
        }
        if buf.is_empty() {
            return PreviewContent::Empty;
        }
        if looks_binary(&buf) {
            return PreviewContent::Binary { size: entry.size };
        }
        let text = String::from_utf8_lossy(&buf);
        let max_lines = self.size.height as usize * 4;
        if let Some(lines) = self
            .highlighter
            .as_ref()
            .and_then(|h| h.highlight(&entry.name, &text, max_lines.max(200)))
        {
            return PreviewContent::Code(lines);
        }
        PreviewContent::Text(
            text.lines()
                .take(max_lines.max(200))
                .map(|l| l.replace('\t', "    "))
                .collect(),
        )
    }
}

fn looks_binary(buf: &[u8]) -> bool {
    let sample = &buf[..buf.len().min(8192)];
    sample.contains(&0)
        || std::str::from_utf8(sample).is_err()
            && String::from_utf8_lossy(sample).matches('\u{FFFD}').count() > sample.len() / 20
}

fn load_image(
    vfs: &VfsRef,
    path: &std::path::Path,
    picker: &Picker,
    size: Size,
) -> Result<Protocol, String> {
    let mut bytes = Vec::new();
    vfs.reader(path)
        .and_then(|mut r| Ok(r.read_to_end(&mut bytes)?))
        .map_err(|e| format!("{e:#}"))?;
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
