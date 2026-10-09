//! Parses `tmux capture-pane -e` output into a grid of styled cells.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self { ch: ' ', fg: None, bg: None, bold: false, italic: false, underline: false }
    }
}

pub type Grid = Vec<Vec<Cell>>;

pub fn parse(text: &str, width: usize, height: usize) -> Grid {
    let mut grid = vec![vec![Cell::default(); width]; height];
    let mut style = Cell::default();
    for (row, line) in text.lines().take(height).enumerate() {
        let mut col = 0;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                let mut params = String::new();
                for p in chars.by_ref() {
                    if p.is_ascii_alphabetic() {
                        if p == 'm' {
                            apply_sgr(&mut style, &params);
                        }
                        break;
                    }
                    params.push(p);
                }
                continue;
            }
            if col < width {
                grid[row][col] = Cell { ch: c, ..style };
            }
            col += 1;
        }
    }
    grid
}

fn apply_sgr(style: &mut Cell, params: &str) {
    let nums: Vec<u16> =
        if params.is_empty() { vec![0] } else { params.split(';').map(|p| p.parse().unwrap_or(0)).collect() };
    let mut i = 0;
    while i < nums.len() {
        match nums[i] {
            0 => *style = Cell::default(),
            1 => style.bold = true,
            3 => style.italic = true,
            4 => style.underline = true,
            22 => style.bold = false,
            23 => style.italic = false,
            24 => style.underline = false,
            7 => std::mem::swap(&mut style.fg, &mut style.bg),
            n @ 30..=37 => style.fg = Some(ansi16(n - 30)),
            n @ 90..=97 => style.fg = Some(ansi16(n - 90 + 8)),
            n @ 40..=47 => style.bg = Some(ansi16(n - 40)),
            n @ 100..=107 => style.bg = Some(ansi16(n - 100 + 8)),
            39 => style.fg = None,
            49 => style.bg = None,
            n @ (38 | 48) => {
                let color = match nums.get(i + 1) {
                    Some(2) if i + 4 < nums.len() => {
                        let c = Rgb(nums[i + 2] as u8, nums[i + 3] as u8, nums[i + 4] as u8);
                        i += 4;
                        Some(c)
                    }
                    Some(5) if i + 2 < nums.len() => {
                        let c = ansi256(nums[i + 2] as u8);
                        i += 2;
                        Some(c)
                    }
                    _ => None,
                };
                if n == 38 {
                    style.fg = color;
                } else {
                    style.bg = color;
                }
            }
            _ => {}
        }
        i += 1;
    }
}

fn ansi16(n: u16) -> Rgb {
    const TABLE: [Rgb; 16] = [
        Rgb(0x1e, 0x1e, 0x2e),
        Rgb(0xf3, 0x8b, 0xa8),
        Rgb(0xa6, 0xe3, 0xa1),
        Rgb(0xf9, 0xe2, 0xaf),
        Rgb(0x89, 0xb4, 0xfa),
        Rgb(0xcb, 0xa6, 0xf7),
        Rgb(0x94, 0xe2, 0xd5),
        Rgb(0xba, 0xc2, 0xde),
        Rgb(0x58, 0x5b, 0x70),
        Rgb(0xf3, 0x8b, 0xa8),
        Rgb(0xa6, 0xe3, 0xa1),
        Rgb(0xf9, 0xe2, 0xaf),
        Rgb(0x89, 0xb4, 0xfa),
        Rgb(0xcb, 0xa6, 0xf7),
        Rgb(0x94, 0xe2, 0xd5),
        Rgb(0xcd, 0xd6, 0xf4),
    ];
    TABLE[n as usize % 16]
}

fn ansi256(n: u8) -> Rgb {
    match n {
        0..=15 => ansi16(n as u16),
        16..=231 => {
            let n = n - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Rgb(level(n / 36), level((n / 6) % 6), level(n % 6))
        }
        _ => {
            let v = 8 + (n - 232) * 10;
            Rgb(v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_truecolor_and_reset() {
        let g = parse("\x1b[38;2;255;0;0;1mA\x1b[0mB", 3, 1);
        assert_eq!(g[0][0].fg, Some(Rgb(255, 0, 0)));
        assert!(g[0][0].bold);
        assert_eq!(g[0][1], Cell { ch: 'B', ..Cell::default() });
        assert_eq!(g[0][2].ch, ' ');
    }
}
