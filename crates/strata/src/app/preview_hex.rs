//! Hex dump of binary files for the preview, sized to the pane.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use strata_config::Theme;

/// Bytes of a binary file shown in the preview.
pub const MAX_HEX_BYTES: usize = 64 * 1024;

/// Columns a row of `n` bytes takes: offset, hex (grouped by 8), text.
fn row_width(n: usize) -> usize {
    10 + 3 * n + (n / 8).saturating_sub(1) + 2 + n
}

/// The most bytes per row (16, 8 or 4) that fit in `width` columns.
pub fn bytes_per_row(width: usize) -> usize {
    [16, 8, 4].into_iter().find(|&n| row_width(n) <= width).unwrap_or(4)
}

/// `00000010  7f 45 4c 46 …  │.ELF…│`, one line per row.
pub fn dump(data: &[u8], width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let n = bytes_per_row(width);
    let muted = Style::default().fg(theme.muted);
    data.chunks(n)
        .enumerate()
        .map(|(row, chunk)| {
            let mut spans = vec![Span::styled(format!("{:08x}  ", row * n), muted)];
            for i in 0..n {
                if i > 0 && i % 8 == 0 {
                    spans.push(Span::raw(" "));
                }
                match chunk.get(i) {
                    Some(&b) => spans.push(Span::styled(format!("{b:02x} "), Style::default().fg(color(b, theme)))),
                    None => spans.push(Span::raw("   ")),
                }
            }
            spans.push(Span::styled("│", muted));
            for &b in chunk {
                let c = if b.is_ascii_graphic() || b == b' ' { b as char } else { '·' };
                spans.push(Span::styled(c.to_string(), Style::default().fg(color(b, theme))));
            }
            spans.push(Span::styled(format!("{}│", " ".repeat(n - chunk.len())), muted));
            Line::from(spans)
        })
        .collect()
}

/// Zero bytes fade out; text, whitespace, control and high bytes differ.
fn color(b: u8, theme: &Theme) -> Color {
    match b {
        0 => theme.muted,
        b'\t' | b'\n' | b'\r' | b' ' => theme.palette.green,
        _ if b.is_ascii_graphic() => theme.fg,
        _ if b.is_ascii() => theme.palette.yellow,
        _ => theme.palette.purple,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_fit_the_pane() {
        assert_eq!(bytes_per_row(80), 16);
        assert_eq!(bytes_per_row(50), 8);
        assert_eq!(bytes_per_row(30), 4);
        assert_eq!(bytes_per_row(5), 4);
        for n in [16, 8, 4] {
            let theme = strata_config::theme::builtin().into_iter().next().unwrap();
            let line = &dump(&[0x41; 40], row_width(n), &theme)[0];
            assert_eq!(line.width(), row_width(n), "{n} bytes per row");
        }
    }

    #[test]
    fn dumps_offsets_hex_and_text() {
        let theme = strata_config::theme::builtin().into_iter().next().unwrap();
        let lines = dump(b"\x7fELF\x00hi", 44, &theme);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "00000000  7f 45 4c 46 00 68 69    │·ELF·hi │");
    }
}
