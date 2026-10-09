//! Development tasks. `cargo xtask media` records the screenshots and GIFs
//! used by the README and the website, by driving strata inside tmux.

mod ansi;
mod demo;
mod render;
mod scenes;
mod tmux;

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("media") => media(&args[1..]),
        Some("gif-frames") => gif_frames(&args[1..]),
        _ => {
            eprintln!("usage: cargo xtask media [--only <scene>] [--font-dir <dir>] [--out <dir>]");
            eprintln!("scenes: {}", scenes::all().iter().map(|s| s.name).collect::<Vec<_>>().join(", "));
            Ok(())
        }
    }
}

fn media(args: &[String]) -> Result<()> {
    let root = workspace_root();
    let mut out = root.join("website/public/media");
    let mut fonts = root.join("target/fonts");
    let mut only = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--out" => out = PathBuf::from(it.next().context("--out needs a value")?),
            "--font-dir" => fonts = PathBuf::from(it.next().context("--font-dir needs a value")?),
            "--only" => only = Some(it.next().context("--only needs a value")?.clone()),
            other => bail!("unknown argument {other}"),
        }
    }
    if !tmux::available() {
        bail!("tmux is required to record media");
    }
    std::fs::create_dir_all(&out)?;

    let status = std::process::Command::new("cargo").args(["build", "--release", "-p", "strata"]).status()?;
    if !status.success() {
        bail!("building strata failed");
    }
    let binary = root.join("target/release/strata");
    let renderer = render::Renderer::load(&fonts).with_context(|| {
        format!("loading fonts from {} (run `make fonts` to download JetBrains Mono Nerd Font)", fonts.display())
    })?;

    for scene in scenes::all() {
        if only.as_deref().is_some_and(|o| o != scene.name) {
            continue;
        }
        println!("recording {}…", scene.name);
        let env = demo::Demo::create(&root.join("target/media-demo"))?;
        scenes::record(&scene, &env, &binary, &renderer, &out)?;
    }
    println!("media written to {}", out.display());
    Ok(())
}

/// Writes every composed frame of a GIF as PNG, to check recordings.
fn gif_frames(args: &[String]) -> Result<()> {
    use image::AnimationDecoder;
    let [gif, dir] = args else { bail!("usage: cargo xtask gif-frames <file.gif> <out-dir>") };
    std::fs::create_dir_all(dir)?;
    let decoder = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(std::fs::File::open(gif)?))?;
    for (i, frame) in decoder.into_frames().enumerate() {
        frame?.into_buffer().save(PathBuf::from(dir).join(format!("{i:03}.png")))?;
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask lives in the workspace").to_path_buf()
}
