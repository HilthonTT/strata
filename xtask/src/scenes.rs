//! Scripted recordings: each scene drives strata with keys and saves
//! screenshots (PNG) or animations (GIF).

use std::fs::File;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use anyhow::Result;
use image::RgbaImage;

use crate::ansi;
use crate::demo::Demo;
use crate::render::Renderer;
use crate::tmux::Session;

const WIDTH: u16 = 150;
const HEIGHT: u16 = 40;
const TICK: u64 = 100;

pub enum Step {
    Keys(&'static [&'static str]),
    Text(&'static str),
    Wait(u64),
    Shot(&'static str),
    Gif(&'static str),
    EndGif,
}

pub struct Scene {
    pub name: &'static str,
    pub theme: &'static str,
    /// Open the Downloads folder in a second panel.
    pub two_panels: bool,
    pub steps: Vec<Step>,
}

use Step::*;

/// Opens `src/main.rs` in the demo project from a fresh start.
fn open_main() -> Vec<Step> {
    vec![Keys(&["j", "j", "j"]), Wait(300), Keys(&["l"]), Wait(400), Keys(&["j", "j"]), Wait(900)]
}

fn theme_shot(theme: &'static str, file: &'static str) -> Scene {
    let mut steps = open_main();
    steps.push(Shot(file));
    Scene { name: file.trim_end_matches(".png"), theme, two_panels: true, steps }
}

pub fn all() -> Vec<Scene> {
    let mut main = open_main();
    main.extend([
        Shot("main.png"),
        Keys(&["2"]),
        Wait(3500),
        Shot("dashboard.png"),
        Keys(&["4"]),
        Wait(3500),
        Shot("nas.png"),
    ]);
    main.extend([Keys(&["1"]), Wait(300), Keys(&["?"]), Wait(400), Shot("help.png")]);

    vec![
        Scene { name: "screens", theme: "catppuccin-mocha", two_panels: true, steps: main },
        Scene {
            name: "overview",
            theme: "catppuccin-mocha",
            two_panels: true,
            steps: vec![
                Gif("overview.gif"),
                Wait(800),
                Keys(&["j"]),
                Wait(350),
                Keys(&["j"]),
                Wait(350),
                Keys(&["j"]),
                Wait(500),
                Keys(&["l"]),
                Wait(700),
                Keys(&["j"]),
                Wait(700),
                Keys(&["j"]),
                Wait(700),
                Keys(&["j"]),
                Wait(1300),
                Keys(&["h"]),
                Wait(500),
                Keys(&["g", "g"]),
                Wait(400),
                Keys(&["l"]),
                Wait(500),
                Keys(&["j"]),
                Wait(1500),
                Keys(&["h"]),
                Wait(500),
                Keys(&["/"]),
                Wait(300),
                Text("read"),
                Wait(900),
                Keys(&["Enter"]),
                Wait(900),
                Keys(&["Escape"]),
                Wait(500),
                Keys(&["2"]),
                Wait(3000),
                Keys(&["4"]),
                Wait(3200),
                Keys(&["1"]),
                Wait(1200),
                EndGif,
            ],
        },
        Scene {
            name: "file-operations",
            theme: "catppuccin-mocha",
            two_panels: true,
            steps: vec![
                Gif("file-operations.gif"),
                Wait(800),
                Keys(&["Tab"]),
                Wait(500),
                Keys(&["space"]),
                Wait(400),
                Keys(&["j"]),
                Wait(300),
                Keys(&["space"]),
                Wait(700),
                Keys(&["c"]),
                Wait(1600),
                Keys(&["BTab"]),
                Wait(600),
                Keys(&["G"]),
                Wait(500),
                Keys(&["r"]),
                Wait(500),
                Keys(&["C-u"]),
                Text("render-2026.log"),
                Wait(500),
                Keys(&["Enter"]),
                Wait(1000),
                Keys(&["u"]),
                Wait(1300),
                Keys(&["d", "d"]),
                Wait(900),
                Keys(&["y"]),
                Wait(1200),
                Keys(&["u"]),
                Wait(1500),
                EndGif,
            ],
        },
        Scene {
            name: "themes",
            theme: "catppuccin-mocha",
            two_panels: true,
            steps: {
                let mut s = open_main();
                s.extend([Gif("themes.gif"), Wait(600), Keys(&["T"]), Wait(700)]);
                for _ in 0..10 {
                    s.extend([Keys(&["Down"]), Wait(650)]);
                }
                s.extend([Keys(&["Enter"]), Wait(1200), EndGif]);
                s
            },
        },
        Scene {
            name: "search",
            theme: "tokyo-night",
            two_panels: true,
            steps: vec![
                Gif("search.gif"),
                Wait(800),
                Keys(&["f"]),
                Wait(700),
                Text("cam"),
                Wait(1000),
                Keys(&["Enter"]),
                Wait(1200),
                Keys(&["C-g"]),
                Wait(500),
                Text("Vec3"),
                Wait(500),
                Keys(&["Enter"]),
                Wait(1500),
                Keys(&["Down"]),
                Wait(600),
                Keys(&["Down"]),
                Wait(900),
                Keys(&["Escape"]),
                Wait(500),
                Keys(&["t"]),
                Wait(900),
                Keys(&["~"]),
                Wait(900),
                Keys(&["g", "t"]),
                Wait(1300),
                EndGif,
            ],
        },
        theme_shot("nord", "theme-nord.png"),
        theme_shot("tokyo-night", "theme-tokyo-night.png"),
        theme_shot("gruvbox-dark", "theme-gruvbox-dark.png"),
        theme_shot("rose-pine", "theme-rose-pine.png"),
        theme_shot("dracula", "theme-dracula.png"),
        theme_shot("catppuccin-latte", "theme-catppuccin-latte.png"),
    ]
}

pub fn record(scene: &Scene, demo: &Demo, binary: &Path, renderer: &Renderer, out: &Path) -> Result<()> {
    let mut command = format!("{} --theme {} {}", binary.display(), scene.theme, demo.project.display());
    if scene.two_panels {
        command.push_str(&format!(" {}", demo.downloads.display()));
    }
    let session = Session::start(scene.name, WIDTH, HEIGHT, &demo.env(), &command)?;
    sleep(Duration::from_millis(2500));

    let title = "strata — ~/projects/nebula";
    let mut frames: Option<(&str, Vec<(String, u64)>)> = None;
    let capture = |frames: &mut Option<(&str, Vec<(String, u64)>)>, ms: u64| -> Result<()> {
        let mut left = ms;
        loop {
            let step = left.min(TICK);
            sleep(Duration::from_millis(step));
            left -= step;
            if let Some((_, list)) = frames.as_mut() {
                let screen = session.capture()?;
                match list.last_mut() {
                    Some((last, dur)) if *last == screen => *dur += step,
                    _ => list.push((screen, step)),
                }
            }
            if left == 0 {
                return Ok(());
            }
        }
    };

    for step in &scene.steps {
        match step {
            Keys(keys) => {
                session.keys(keys)?;
                capture(&mut frames, TICK)?;
            }
            Text(text) => {
                for ch in text.chars() {
                    session.text(&ch.to_string())?;
                    capture(&mut frames, 80)?;
                }
            }
            Wait(ms) => capture(&mut frames, *ms)?,
            Shot(file) => {
                let grid = ansi::parse(&session.capture()?, WIDTH as usize, HEIGHT as usize);
                renderer.render(&grid, title).save(out.join(file))?;
                println!("  wrote {file}");
            }
            Gif(file) => frames = Some((file, Vec::new())),
            EndGif => {
                if let Some((file, list)) = frames.take() {
                    write_gif(&out.join(file), &list, renderer, title)?;
                    println!("  wrote {file} ({} frames)", list.len());
                }
            }
        }
    }
    Ok(())
}

/// Encodes frames, storing only the changed region of each one on top of
/// the previous frame (disposal "keep").
fn write_gif(path: &Path, frames: &[(String, u64)], renderer: &Renderer, title: &str) -> Result<()> {
    let mut encoder: Option<gif::Encoder<File>> = None;
    let mut previous: Option<RgbaImage> = None;
    for (i, (screen, ms)) in frames.iter().enumerate() {
        let image = renderer.render(&ansi::parse(screen, WIDTH as usize, HEIGHT as usize), title);
        let hold = if i + 1 == frames.len() { ms + 1500 } else { *ms };
        let enc = match encoder.as_mut() {
            Some(e) => e,
            None => {
                let mut e = gif::Encoder::new(File::create(path)?, image.width() as u16, image.height() as u16, &[])?;
                e.set_repeat(gif::Repeat::Infinite)?;
                encoder.insert(e)
            }
        };
        let (x, y, w, h) = match &previous {
            None => (0, 0, image.width(), image.height()),
            Some(p) => changed_region(p, &image).unwrap_or((0, 0, 1, 1)),
        };
        let mut pixels = image::imageops::crop_imm(&image, x, y, w, h).to_image().into_raw();
        let mut frame = gif::Frame::from_rgba_speed(w as u16, h as u16, &mut pixels, 10);
        frame.left = x as u16;
        frame.top = y as u16;
        frame.delay = (hold / 10).min(u16::MAX as u64) as u16;
        frame.dispose = gif::DisposalMethod::Keep;
        enc.write_frame(&frame)?;
        previous = Some(image);
    }
    Ok(())
}

fn changed_region(a: &RgbaImage, b: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, p) in b.enumerate_pixels() {
        if a.get_pixel(x, y) != p {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    (x0 != u32::MAX).then(|| (x0, y0, x1 - x0 + 1, y1 - y0 + 1))
}
