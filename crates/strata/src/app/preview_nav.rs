//! Scrolling and searching inside the preview.

use strata_plugin::Level;

use super::preview::PreviewContent;
use super::App;

impl App {
    /// The preview as plain lines, for searching and scroll limits.
    fn preview_text(&self) -> Vec<String> {
        match &self.preview {
            PreviewContent::Text(lines) => lines.clone(),
            PreviewContent::Code(lines) => {
                lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
            }
            PreviewContent::Dir(entries) => entries.iter().map(|e| e.name.clone()).collect(),
            _ => Vec::new(),
        }
    }

    /// Scrolls the preview by `delta` lines (clamped to the content).
    pub(super) fn scroll_preview(&mut self, delta: isize) {
        let len = self.preview_text().len();
        let max = len.saturating_sub(self.preview_area.height as usize / 2);
        self.preview_scroll = (self.preview_scroll as isize + delta).clamp(0, max as isize) as usize;
    }

    pub(super) fn half_preview(&self) -> isize {
        (self.preview_area.height as isize / 2).max(1)
    }

    /// Searches the preview for `query` and jumps to the first match.
    pub(super) fn find_in_preview(&mut self, query: &str) {
        if query.is_empty() {
            self.preview_query = None;
            return;
        }
        self.preview_query = Some(query.to_string());
        self.preview_scroll = 0;
        let count = self.preview_matches().len();
        if count == 0 {
            return self.notify(format!("'{query}' not found in the preview"), Level::Warn);
        }
        self.next_preview_match(true, true);
        self.info(format!("{count} line(s) match '{query}'"));
    }

    /// Lines containing the query (case-insensitive).
    pub fn preview_matches(&self) -> Vec<usize> {
        let Some(query) = self.preview_query.as_ref().map(|q| q.to_lowercase()) else { return Vec::new() };
        self.preview_text()
            .iter()
            .enumerate()
            .filter(|(_, l)| l.to_lowercase().contains(&query))
            .map(|(i, _)| i)
            .collect()
    }

    /// Scrolls to the next (or previous) match, wrapping around.
    pub(super) fn next_preview_match(&mut self, forward: bool, inclusive: bool) {
        let matches = self.preview_matches();
        if matches.is_empty() {
            return;
        }
        // Keep a little context above the match.
        let context = 2;
        let current = self.preview_scroll + context;
        let target = if forward {
            matches.iter().find(|&&m| if inclusive { m >= current } else { m > current }).or(matches.first())
        } else {
            matches.iter().rev().find(|&&m| m < current).or(matches.last())
        };
        if let Some(&line) = target {
            self.preview_scroll = line.saturating_sub(context);
        }
    }
}
