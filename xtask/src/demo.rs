//! A throwaway home directory with a small project, so recordings look the
//! same on every machine and never show real files.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;
use image::{Rgb, RgbImage};

pub struct Demo {
    pub home: PathBuf,
    pub config: PathBuf,
    pub project: PathBuf,
    pub downloads: PathBuf,
}

const CONFIG: &str = r#"[general]
theme = "catppuccin-mocha"
panels = 2

[plugins]
enabled = ["git", "bookmarks", "archive"]

# Addresses from RFC 5737: documentation-only, they never answer.
[[connections]]
name = "homelab"
protocol = "smb"
host = "192.0.2.10"
share = "media"
user = "demo"

[[connections]]
name = "backup"
protocol = "sftp"
host = "198.51.100.7"
share = "/volume1/backup"
user = "demo"

[[connections]]
name = "archive"
protocol = "nfs"
host = "203.0.113.20"
share = "/export/archive"
"#;

const MAIN_RS: &str = r#"//! nebula — a tiny ray tracer.

mod camera;
mod scene;

use camera::Camera;
use scene::Scene;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

fn main() -> anyhow::Result<()> {
    let scene = Scene::load("scenes/galaxy.toml")?;
    let camera = Camera::looking_at(scene.focus(), 42.0);
    let image = camera.render(&scene, WIDTH, HEIGHT);
    image.save("render.png")?;
    println!("rendered {} objects", scene.len());
    Ok(())
}
"#;

const CAMERA_RS: &str = r#"use crate::scene::{Ray, Scene, Vec3};

/// A pinhole camera.
pub struct Camera {
    origin: Vec3,
    fov: f32,
}

impl Camera {
    pub fn looking_at(target: Vec3, fov: f32) -> Self {
        Self { origin: target - Vec3::new(0.0, 0.0, 10.0), fov }
    }

    pub fn render(&self, scene: &Scene, width: u32, height: u32) -> image::RgbImage {
        image::RgbImage::from_fn(width, height, |x, y| {
            let ray = Ray::through(self.origin, x, y, self.fov);
            scene.trace(&ray).into()
        })
    }
}
"#;

const SCENE_RS: &str = r#"use std::ops::Sub;

#[derive(Clone, Copy, Debug, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
}

impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
"#;

impl Demo {
    pub fn create(root: &Path) -> Result<Self> {
        let _ = fs::remove_dir_all(root);
        let home = root.join("home");
        let config = root.join("config");
        let project = home.join("projects/nebula");
        let downloads = home.join("Downloads");
        for dir in [
            &config,
            &project.join("src"),
            &project.join("docs"),
            &project.join("assets"),
            &project.join("scenes"),
            &project.join("target/release"),
            &downloads,
            &home.join("Documents/notes"),
            &home.join("Pictures/wallpapers"),
            &home.join("Music"),
            &home.join(".config"),
            &home.join(".local/share/Trash/files"),
        ] {
            fs::create_dir_all(dir)?;
        }
        fs::write(config.join("config.toml"), CONFIG)?;
        fs::write(
            home.join(".config/user-dirs.dirs"),
            "XDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\nXDG_DOCUMENTS_DIR=\"$HOME/Documents\"\nXDG_PICTURES_DIR=\"$HOME/Pictures\"\nXDG_MUSIC_DIR=\"$HOME/Music\"\n",
        )?;

        let p = &project;
        fs::write(p.join("Cargo.toml"), "[package]\nname = \"nebula\"\nversion = \"0.3.0\"\nedition = \"2021\"\n\n[dependencies]\nanyhow = \"1\"\nimage = \"0.25\"\n")?;
        fs::write(
            p.join("README.md"),
            "# nebula\n\nA tiny ray tracer that renders galaxies.\n\n```sh\ncargo run --release\n```\n",
        )?;
        fs::write(p.join("LICENSE"), "MIT License\n\nCopyright (c) 2026 Demo\n")?;
        fs::write(p.join(".gitignore"), "/target\n*.log\n")?;
        fs::write(p.join("src/main.rs"), MAIN_RS)?;
        fs::write(p.join("src/camera.rs"), CAMERA_RS)?;
        fs::write(p.join("src/scene.rs"), SCENE_RS)?;
        fs::write(p.join("docs/architecture.md"), "# Architecture\n\nThe renderer traces one ray per pixel.\n")?;
        fs::write(p.join("docs/roadmap.md"), "# Roadmap\n\n- [x] spheres\n- [ ] volumetric dust\n")?;
        fs::write(
            p.join("scenes/galaxy.toml"),
            "[camera]\nfov = 42\n\n[[star]]\nposition = [0, 0, 0]\nradius = 2.5\n",
        )?;
        fs::write(p.join("target/release/nebula"), vec![0u8; 4096])?;
        fs::write(p.join("render.log"), "rendered 128 objects in 2.4s\n")?;
        gradient(p.join("assets/logo.png"), 256, 256, [(137, 180, 250), (203, 166, 247)])?;
        gradient(p.join("assets/banner.png"), 480, 160, [(250, 179, 135), (243, 139, 168)])?;
        nebula(home.join("Pictures/wallpapers/nebula.png"), 640, 360)?;

        // A git history with some changes, for the status markers.
        let git = |args: &[&str]| {
            let _ = Command::new("git")
                .arg("-C")
                .arg(p)
                .args(["-c", "user.name=Demo", "-c", "user.email=demo@example.com", "-c", "init.defaultBranch=main"])
                .args(args)
                .output();
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-qm", "initial commit"]);
        fs::write(p.join("src/camera.rs"), CAMERA_RS.replace("42.0", "60.0") + "\n// TODO: depth of field\n")?;
        fs::write(p.join("src/light.rs"), "pub struct Light;\n")?;
        fs::write(p.join("docs/roadmap.md"), "# Roadmap\n\n- [x] spheres\n- [x] lights\n- [ ] volumetric dust\n")?;

        for (name, size) in
            [("dataset.csv", 48_000), ("report-q3.pdf", 182_000), ("photos.zip", 2_400_000), ("notes.txt", 900)]
        {
            fs::write(downloads.join(name), vec![b'x'; size])?;
        }
        fs::write(home.join("Documents/notes/ideas.md"), "# Ideas\n\n- a file explorer with plugins\n")?;
        fs::write(home.join("Documents/todo.txt"), "buy milk\nship strata\n")?;

        Ok(Self { home, config, project, downloads })
    }

    /// Environment for strata inside the recording.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("HOME".into(), self.home.display().to_string()),
            ("STRATA_CONFIG_DIR".into(), self.config.display().to_string()),
            ("XDG_CONFIG_HOME".into(), self.home.join(".config").display().to_string()),
            ("XDG_DATA_HOME".into(), self.home.join(".local/share").display().to_string()),
            ("EDITOR".into(), "true".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
        ]
    }
}

fn gradient(path: PathBuf, w: u32, h: u32, [a, b]: [(u8, u8, u8); 2]) -> Result<()> {
    let img = RgbImage::from_fn(w, h, |x, y| {
        let t = (x + y) as f32 / (w + h) as f32;
        let m = |p: u8, q: u8| (p as f32 + (q as f32 - p as f32) * t) as u8;
        Rgb([m(a.0, b.0), m(a.1, b.1), m(a.2, b.2)])
    });
    img.save(path)?;
    Ok(())
}

/// A soft, colourful nebula for the image-preview demo.
fn nebula(path: PathBuf, w: u32, h: u32) -> Result<()> {
    let blobs = [
        (0.30, 0.45, 0.30, (203.0, 166.0, 247.0)),
        (0.65, 0.40, 0.28, (137.0, 180.0, 250.0)),
        (0.50, 0.70, 0.22, (243.0, 139.0, 168.0)),
        (0.80, 0.75, 0.18, (148.0, 226.0, 213.0)),
    ];
    let img = RgbImage::from_fn(w, h, |x, y| {
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32 * 0.56 + 0.22);
        let mut c = [12.0f32, 12.0, 24.0];
        for (bx, by, r, col) in blobs {
            let d = ((fx - bx).powi(2) + (fy - by).powi(2)).sqrt();
            let k = (1.0 - d / r).max(0.0).powf(1.6);
            c[0] += col.0 * k;
            c[1] += col.1 * k;
            c[2] += col.2 * k;
        }
        // A few stars.
        let hash = (x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663)) % 997;
        if hash == 0 {
            c = [255.0, 255.0, 255.0];
        }
        Rgb([c[0].min(255.0) as u8, c[1].min(255.0) as u8, c[2].min(255.0) as u8])
    });
    img.save(path)?;
    Ok(())
}
