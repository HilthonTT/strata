//! Draws a cell grid as a terminal window image.
//!
//! Box-drawing and block characters are drawn as shapes rather than font
//! glyphs so borders join seamlessly at any line height.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use fontdue::{Font, FontSettings, Metrics};
use image::{Rgba, RgbaImage};

use crate::ansi::{Grid, Rgb};

const FONT_PX: f32 = 15.0;
const LINE_HEIGHT: f32 = 1.3;
const MARGIN: u32 = 0;
const TITLEBAR: u32 = 36;
const PADDING: u32 = 14;
const RADIUS: f32 = 12.0;

/// Page colour around the window (matches the website background).
pub const PAGE: Rgb = Rgb(0x0b, 0x0c, 0x12);
const DEFAULT_BG: Rgb = Rgb(0x1e, 0x1e, 0x2e);
const DEFAULT_FG: Rgb = Rgb(0xcd, 0xd6, 0xf4);

type GlyphKey = (char, u8);

pub struct Renderer {
    fonts: [Font; 3],
    cell_w: u32,
    cell_h: u32,
    ascent: f32,
    glyphs: RefCell<HashMap<GlyphKey, (Metrics, Vec<u8>)>>,
}

impl Renderer {
    pub fn load(dir: &Path) -> Result<Self> {
        let load = |name: &str| -> Result<Font> {
            let bytes = std::fs::read(dir.join(name)).with_context(|| format!("reading {name}"))?;
            Font::from_bytes(bytes, FontSettings::default()).map_err(|e| anyhow!("{name}: {e}"))
        };
        let fonts = [
            load("JetBrainsMonoNerdFontMono-Regular.ttf")?,
            load("JetBrainsMonoNerdFontMono-Bold.ttf")?,
            load("JetBrainsMonoNerdFontMono-Italic.ttf")?,
        ];
        let advance = fonts[0].metrics('M', FONT_PX).advance_width;
        let lines = fonts[0].horizontal_line_metrics(FONT_PX).context("font has no line metrics")?;
        let cell_h = (FONT_PX * LINE_HEIGHT).round() as u32;
        let content = lines.ascent - lines.descent;
        let ascent = lines.ascent + (cell_h as f32 - content) / 2.0;
        Ok(Self { fonts, cell_w: advance.round() as u32, cell_h, ascent, glyphs: RefCell::default() })
    }

    /// Renders the grid inside a window with a title bar.
    pub fn render(&self, grid: &Grid, title: &str) -> RgbaImage {
        let rows = grid.len() as u32;
        let cols = grid.first().map_or(0, |r| r.len()) as u32;
        let term_w = cols * self.cell_w;
        let term_h = rows * self.cell_h;
        let win_w = term_w + PADDING * 2;
        let win_h = term_h + PADDING * 2 + TITLEBAR;
        let mut img = RgbaImage::from_pixel(win_w + MARGIN * 2, win_h + MARGIN * 2, rgba(PAGE));

        // The window's background is the terminal's most common background.
        let term_bg = dominant_bg(grid);
        let bar = mix(term_bg, Rgb(255, 255, 255), 0.04);
        self.window(&mut img, (MARGIN, MARGIN, win_w, win_h), term_bg, bar);
        self.title(&mut img, title, MARGIN + win_w / 2, MARGIN + TITLEBAR / 2, mix(term_bg, DEFAULT_FG, 0.55));

        let (ox, oy) = (MARGIN + PADDING, MARGIN + TITLEBAR + PADDING);
        for (r, row) in grid.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                let x = ox + c as u32 * self.cell_w;
                let y = oy + r as u32 * self.cell_h;
                let bg = cell.bg.unwrap_or(term_bg);
                let fg = cell.fg.unwrap_or(DEFAULT_FG);
                fill(&mut img, x, y, self.cell_w, self.cell_h, bg);
                if cell.ch != ' ' && !self.draw_shape(&mut img, cell.ch, x, y, fg, bg) {
                    let variant = if cell.bold {
                        1
                    } else if cell.italic {
                        2
                    } else {
                        0
                    };
                    self.draw_glyph(&mut img, cell.ch, variant, x, y, fg);
                }
                if cell.underline {
                    fill(&mut img, x, y + self.cell_h - 2, self.cell_w, 1, fg);
                }
            }
        }
        img
    }

    fn window(&self, img: &mut RgbaImage, (x, y, w, h): (u32, u32, u32, u32), body: Rgb, bar: Rgb) {
        let border = mix(body, Rgb(255, 255, 255), 0.10);
        for py in y..y + h {
            for px in x..x + w {
                let coverage = rounded_coverage(px, py, x, y, w, h, RADIUS);
                if coverage <= 0.0 {
                    continue;
                }
                let inner = rounded_coverage(px, py, x + 1, y + 1, w - 2, h - 2, RADIUS - 1.0);
                let fill_color = if py < y + TITLEBAR { bar } else { body };
                let color = mix(border, fill_color, inner);
                blend(img, px, py, color, coverage);
            }
        }
        // Traffic lights.
        for (i, color) in [Rgb(0xff, 0x5f, 0x57), Rgb(0xfe, 0xbc, 0x2e), Rgb(0x28, 0xc8, 0x40)].iter().enumerate() {
            circle(img, (x + 20 + i as u32 * 20) as f32, (y + TITLEBAR / 2) as f32, 6.0, *color);
        }
    }

    fn title(&self, img: &mut RgbaImage, text: &str, cx: u32, cy: u32, color: Rgb) {
        let width = text.chars().count() as u32 * self.cell_w;
        let x = cx.saturating_sub(width / 2);
        let y = cy.saturating_sub(self.cell_h / 2);
        for (i, ch) in text.chars().enumerate() {
            self.draw_glyph(img, ch, 0, x + i as u32 * self.cell_w, y, color);
        }
    }

    fn draw_glyph(&self, img: &mut RgbaImage, ch: char, variant: u8, x: u32, y: u32, color: Rgb) {
        let mut cache = self.glyphs.borrow_mut();
        let font = &self.fonts[variant as usize];
        let font = if font.lookup_glyph_index(ch) == 0 { &self.fonts[0] } else { font };
        let (m, bitmap) = cache.entry((ch, variant)).or_insert_with(|| font.rasterize(ch, FONT_PX));
        let baseline = y as f32 + self.ascent;
        let gx = x as i32 + m.xmin;
        let gy = (baseline - m.height as f32 - m.ymin as f32).round() as i32;
        for row in 0..m.height {
            for col in 0..m.width {
                let a = bitmap[row * m.width + col] as f32 / 255.0;
                if a > 0.0 {
                    let (px, py) = (gx + col as i32, gy + row as i32);
                    if px >= 0 && py >= 0 {
                        blend(img, px as u32, py as u32, color, a);
                    }
                }
            }
        }
    }

    /// Box-drawing and block elements. Returns false for other characters.
    fn draw_shape(&self, img: &mut RgbaImage, ch: char, x: u32, y: u32, fg: Rgb, bg: Rgb) -> bool {
        let (w, h) = (self.cell_w, self.cell_h);
        match ch {
            '█' => fill(img, x, y, w, h, fg),
            '▀' => fill(img, x, y, w, h / 2, fg),
            '▄' => fill(img, x, y + h / 2, w, h - h / 2, fg),
            '▌' => fill(img, x, y, w / 2, h, fg),
            '▐' => fill(img, x + w / 2, y, w - w / 2, h, fg),
            '░' => fill(img, x, y, w, h, mix(bg, fg, 0.25)),
            '▒' => fill(img, x, y, w, h, mix(bg, fg, 0.5)),
            '▁'..='▇' => {
                let eighths = ch as u32 - '▁' as u32 + 1;
                let bar = h * eighths / 8;
                fill(img, x, y + h - bar, w, bar, fg);
            }
            _ => match box_arms(ch) {
                Some(arms) => self.box_lines(img, x, y, arms, fg),
                None => return false,
            },
        }
        true
    }

    fn box_lines(&self, img: &mut RgbaImage, x: u32, y: u32, arms: Arms, fg: Rgb) {
        let (w, h) = (self.cell_w, self.cell_h);
        let t = 1;
        let (cx, cy) = (x + w / 2, y + h / 2);
        if let Some((sx, sy)) = arms.round {
            // Quarter circle joining a horizontal and a vertical arm.
            let r = (w / 2) as f32;
            let (ccx, ccy) = (cx as f32 + 0.5 + sx * r, cy as f32 + 0.5 + sy * r);
            for py in y..y + h {
                for px in x..x + w {
                    let (dx, dy) = (px as f32 + 0.5 - ccx, py as f32 + 0.5 - ccy);
                    if dx * sx > 0.0 || dy * sy > 0.0 {
                        continue;
                    }
                    let d = (dx * dx + dy * dy).sqrt();
                    let a = (1.0 - (d - r).abs()).clamp(0.0, 1.0);
                    if a > 0.0 {
                        blend(img, px, py, fg, a);
                    }
                }
            }
            let rr = r as u32;
            if sx > 0.0 {
                fill(img, cx + rr, cy, x + w - (cx + rr), t, fg);
            } else {
                fill(img, x, cy, cx - rr - x + 1, t, fg);
            }
            if sy > 0.0 {
                fill(img, cx, cy + rr, t, y + h - (cy + rr), fg);
            } else {
                fill(img, cx, y, t, cy - rr - y + 1, fg);
            }
            return;
        }
        if arms.left {
            fill(img, x, cy, cx - x + t, t, fg);
        }
        if arms.right {
            fill(img, cx, cy, x + w - cx, t, fg);
        }
        if arms.up {
            fill(img, cx, y, t, cy - y + t, fg);
        }
        if arms.down {
            fill(img, cx, cy, t, y + h - cy, fg);
        }
    }
}

#[derive(Default)]
struct Arms {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    /// Rounded corner: direction of the horizontal and vertical arms.
    round: Option<(f32, f32)>,
}

fn box_arms(ch: char) -> Option<Arms> {
    let a = |up, down, left, right| Some(Arms { up, down, left, right, round: None });
    let round = |sx, sy| Some(Arms { round: Some((sx, sy)), ..Default::default() });
    match ch {
        '─' | '━' => a(false, false, true, true),
        '│' | '┃' => a(true, true, false, false),
        '┌' | '┏' => a(false, true, false, true),
        '┐' | '┓' => a(false, true, true, false),
        '└' | '┗' => a(true, false, false, true),
        '┘' | '┛' => a(true, false, true, false),
        '├' => a(true, true, false, true),
        '┤' => a(true, true, true, false),
        '┬' => a(false, true, true, true),
        '┴' => a(true, false, true, true),
        '┼' => a(true, true, true, true),
        '╭' => round(1.0, 1.0),
        '╮' => round(-1.0, 1.0),
        '╰' => round(1.0, -1.0),
        '╯' => round(-1.0, -1.0),
        _ => None,
    }
}

fn dominant_bg(grid: &Grid) -> Rgb {
    let mut counts: HashMap<Rgb, usize> = HashMap::new();
    for cell in grid.iter().flatten() {
        *counts.entry(cell.bg.unwrap_or(DEFAULT_BG)).or_default() += 1;
    }
    counts.into_iter().max_by_key(|(_, n)| *n).map(|(c, _)| c).unwrap_or(DEFAULT_BG)
}

/// Anti-aliased coverage of a pixel by a rounded rectangle.
fn rounded_coverage(px: u32, py: u32, x: u32, y: u32, w: u32, h: u32, r: f32) -> f32 {
    let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
    let (x0, y0, x1, y1) = (x as f32, y as f32, (x + w) as f32, (y + h) as f32);
    if fx < x0 || fy < y0 || fx > x1 || fy > y1 {
        return 0.0;
    }
    let cx = fx.clamp(x0 + r, x1 - r);
    let cy = fy.clamp(y0 + r, y1 - r);
    let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
    (r - d + 0.5).clamp(0.0, 1.0)
}

fn circle(img: &mut RgbaImage, cx: f32, cy: f32, r: f32, color: Rgb) {
    for py in (cy - r - 1.0) as u32..=(cy + r + 1.0) as u32 {
        for px in (cx - r - 1.0) as u32..=(cx + r + 1.0) as u32 {
            let d = ((px as f32 + 0.5 - cx).powi(2) + (py as f32 + 0.5 - cy).powi(2)).sqrt();
            let a = (r - d + 0.5).clamp(0.0, 1.0);
            if a > 0.0 {
                blend(img, px, py, color, a);
            }
        }
    }
}

fn fill(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, color: Rgb) {
    for py in y..(y + h).min(img.height()) {
        for px in x..(x + w).min(img.width()) {
            img.put_pixel(px, py, rgba(color));
        }
    }
}

fn blend(img: &mut RgbaImage, x: u32, y: u32, color: Rgb, alpha: f32) {
    if x >= img.width() || y >= img.height() {
        return;
    }
    let p = img.get_pixel(x, y).0;
    let under = Rgb(p[0], p[1], p[2]);
    img.put_pixel(x, y, rgba(mix(under, color, alpha)));
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Rgb(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

fn rgba(c: Rgb) -> Rgba<u8> {
    Rgba([c.0, c.1, c.2, 255])
}
