//! PDF, video and audio previews through common command-line tools: a
//! thumbnail (first page, a representative frame, embedded cover art) and
//! the facts that matter (pages, duration, resolution, codecs, tags).
//!
//! Every tool is optional. Missing ones are reported with what to install.

use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use image::DynamicImage;

/// How long a tool may take before the preview gives up on it.
const TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Pdf,
    Video,
    Audio,
}

impl MediaKind {
    pub fn of(ext: &str) -> Option<Self> {
        Some(match ext {
            "pdf" => MediaKind::Pdf,
            "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "wmv" | "flv" | "mpg" | "mpeg" | "ts" | "3gp" | "ogv" => {
                MediaKind::Video
            }
            "mp3" | "flac" | "wav" | "ogg" | "oga" | "opus" | "m4a" | "aac" | "wma" | "aiff" | "alac" | "ape" => {
                MediaKind::Audio
            }
            _ => return None,
        })
    }
}

/// What a media preview shows.
#[derive(Default)]
pub struct Media {
    pub image: Option<DynamicImage>,
    /// `(label, value)` facts, e.g. `("Duration", "3:25")`.
    pub facts: Vec<(String, String)>,
    /// Extracted text (PDFs without a thumbnail).
    pub text: Vec<String>,
    /// What to install for a better preview.
    pub hint: Option<String>,
}

/// Builds the preview of a local file. `thumbnail` is false when the
/// terminal cannot show images.
pub fn inspect(path: &Path, kind: MediaKind, thumbnail: bool) -> Media {
    match kind {
        MediaKind::Pdf => pdf(path, thumbnail),
        MediaKind::Video | MediaKind::Audio => audio_video(path, kind, thumbnail),
    }
}

fn pdf(path: &Path, thumbnail: bool) -> Media {
    let mut media = Media::default();
    if let Some(out) = run("pdfinfo", [path.as_os_str()]) {
        media.facts = pdf_facts(&String::from_utf8_lossy(&out));
    }
    if thumbnail {
        media.image = with_temp_png(|png| {
            // pdftoppm adds `.png` to the name it is given.
            let root = png.with_extension("");
            let args = [OsStr::new("-png"), "-f".as_ref(), "1".as_ref(), "-l".as_ref(), "1".as_ref()];
            let args = args.into_iter().chain(["-singlefile".as_ref(), "-scale-to".as_ref(), "1024".as_ref()]);
            run("pdftoppm", args.chain([path.as_os_str(), root.as_os_str()]))
        });
    }
    if media.image.is_none() {
        let args = ["-l".as_ref(), "3".as_ref(), "-layout".as_ref(), path.as_os_str(), "-".as_ref()];
        if let Some(out) = run("pdftotext", args) {
            media.text = String::from_utf8_lossy(&out).lines().map(|l| l.trim_end().replace('\u{c}', "")).collect();
        }
    }
    if media.facts.is_empty() && media.image.is_none() && media.text.is_empty() {
        media.hint = Some("install poppler-utils (pdftoppm, pdftotext, pdfinfo) to preview PDFs".into());
    }
    media
}

/// `Title`, `Author`, `Pages` and `Page size` from `pdfinfo`.
fn pdf_facts(text: &str) -> Vec<(String, String)> {
    let wanted = ["Title", "Author", "Pages", "Page size", "Creator", "Producer"];
    let mut facts: Vec<(String, String)> = text
        .lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, v)| wanted.contains(&k.as_str()) && !v.is_empty())
        .collect();
    facts.sort_by_key(|(k, _)| wanted.iter().position(|w| w == k));
    facts
}

fn audio_video(path: &Path, kind: MediaKind, thumbnail: bool) -> Media {
    let mut media = Media::default();
    let probe = run(
        "ffprobe",
        ["-v", "error", "-print_format", "json", "-show_format", "-show_streams"]
            .map(OsStr::new)
            .into_iter()
            .chain([path.as_os_str()]),
    );
    let mut has_picture = kind == MediaKind::Video;
    if let Some(json) = probe.and_then(|out| serde_json::from_slice::<serde_json::Value>(&out).ok()) {
        media.facts = probe_facts(&json);
        has_picture |= json["streams"].as_array().is_some_and(|s| s.iter().any(|s| s["codec_type"] == "video"));
    } else if let Some(out) = run("mediainfo", [path.as_os_str()]).or_else(|| run("exiftool", [path.as_os_str()])) {
        media.facts = key_value_facts(&String::from_utf8_lossy(&out));
    }
    if thumbnail && has_picture {
        media.image = with_temp_png(|png| {
            let small =
                [OsStr::new("-i"), path.as_os_str(), "-o".as_ref(), png.as_os_str(), "-s".as_ref(), "640".as_ref()];
            if kind == MediaKind::Video {
                if let Some(out) = run("ffmpegthumbnailer", small) {
                    return Some(out);
                }
            }
            // A representative frame of a video, or the cover of a song.
            let filter = if kind == MediaKind::Video { "thumbnail,scale=640:-2" } else { "scale=640:-2" };
            let args = [OsStr::new("-v"), "error".as_ref(), "-y".as_ref(), "-i".as_ref(), path.as_os_str()];
            let rest = ["-an", "-vf", filter, "-frames:v", "1"].map(OsStr::new);
            run("ffmpeg", args.into_iter().chain(rest).chain([png.as_os_str()]))
        });
    }
    if media.facts.is_empty() && media.image.is_none() {
        let what = if kind == MediaKind::Video { "videos" } else { "audio" };
        media.hint = Some(format!("install ffmpeg (ffprobe) or mediainfo to preview {what}"));
    }
    media
}

/// Duration, streams, bitrate and tags from `ffprobe -print_format json`.
fn probe_facts(json: &serde_json::Value) -> Vec<(String, String)> {
    let mut facts = Vec::new();
    let format = &json["format"];
    let tags = &format["tags"];
    // Tag names vary in case between containers (`title`, `TITLE`).
    let tag = |name: &str| {
        tags.as_object()?
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .and_then(|(_, v)| v.as_str())
            .map(str::to_string)
    };
    for (label, name) in
        [("Title", "title"), ("Artist", "artist"), ("Album", "album"), ("Date", "date"), ("Genre", "genre")]
    {
        if let Some(v) = tag(name).filter(|v| !v.trim().is_empty()) {
            facts.push((label.to_string(), v));
        }
    }
    if let Some(secs) = format["duration"].as_str().and_then(|d| d.parse::<f64>().ok()) {
        facts.push(("Duration".into(), duration(secs)));
    }
    for stream in json["streams"].as_array().into_iter().flatten() {
        let codec = stream["codec_name"].as_str().unwrap_or("?");
        match stream["codec_type"].as_str() {
            // Cover art is stored as a one-frame video stream.
            Some("video") if stream["disposition"]["attached_pic"] == 1 => {}
            Some("video") => {
                let mut v = codec.to_string();
                if let (Some(w), Some(h)) = (stream["width"].as_u64(), stream["height"].as_u64()) {
                    v.push_str(&format!(" · {w}×{h}"));
                }
                if let Some(fps) = stream["avg_frame_rate"].as_str().and_then(frame_rate) {
                    v.push_str(&format!(" · {fps} fps"));
                }
                facts.push(("Video".into(), v));
            }
            Some("audio") => {
                let mut a = codec.to_string();
                if let Some(rate) = stream["sample_rate"].as_str().and_then(|r| r.parse::<f64>().ok()) {
                    a.push_str(&format!(" · {} kHz", trim_float(rate / 1000.0)));
                }
                match stream["channels"].as_u64() {
                    Some(1) => a.push_str(" · mono"),
                    Some(2) => a.push_str(" · stereo"),
                    Some(n) => a.push_str(&format!(" · {n} channels")),
                    None => {}
                }
                facts.push(("Audio".into(), a));
            }
            Some("subtitle") => facts.push(("Subtitles".into(), codec.to_string())),
            _ => {}
        }
    }
    if let Some(bps) = format["bit_rate"].as_str().and_then(|b| b.parse::<f64>().ok()) {
        let rate = if bps >= 1e6 {
            format!("{} Mb/s", trim_float(bps / 1e6))
        } else {
            format!("{} kb/s", (bps / 1e3).round())
        };
        facts.push(("Bitrate".into(), rate));
    }
    if let Some(name) = format["format_long_name"].as_str() {
        facts.push(("Format".into(), name.to_string()));
    }
    facts
}

/// `Key : value` lines from mediainfo or exiftool, without repeats.
fn key_value_facts(text: &str) -> Vec<(String, String)> {
    let mut facts: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(" : ").or_else(|| line.split_once(": ")) else { continue };
        let (k, v) = (k.trim(), v.trim());
        if !k.is_empty() && !v.is_empty() && !facts.iter().any(|(fk, _)| fk == k) {
            facts.push((k.to_string(), v.to_string()));
        }
        if facts.len() >= 30 {
            break;
        }
    }
    facts
}

/// `3725.4` → `1:02:05`, `65` → `1:05`.
fn duration(secs: f64) -> String {
    let s = secs.round() as u64;
    match (s / 3600, s / 60 % 60, s % 60) {
        (0, m, s) => format!("{m}:{s:02}"),
        (h, m, s) => format!("{h}:{m:02}:{s:02}"),
    }
}

/// `30000/1001` → `29.97`; `0/0` → `None`.
fn frame_rate(rate: &str) -> Option<String> {
    let (n, d) = rate.split_once('/')?;
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (d > 0.0 && n > 0.0).then(|| trim_float(n / d))
}

fn trim_float(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Runs `make` with a fresh temporary `.png` path and loads the image it
/// wrote there.
fn with_temp_png(make: impl FnOnce(&Path) -> Option<Vec<u8>>) -> Option<DynamicImage> {
    let dir = tempfile::Builder::new().prefix("strata-preview-").tempdir().ok()?;
    let png = dir.path().join("thumb.png");
    make(&png)?;
    image::open(&png).ok()
}

/// Runs a tool and returns its standard output, or `None` when it is
/// missing, fails or takes longer than [`TIMEOUT`].
fn run<I, S>(program: &str, args: I) -> Option<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Read on a thread so a chatty tool cannot block on a full pipe.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < TIMEOUT => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let out = reader.join().ok()?;
    status.success().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_extension() {
        assert_eq!(MediaKind::of("pdf"), Some(MediaKind::Pdf));
        assert_eq!(MediaKind::of("mkv"), Some(MediaKind::Video));
        assert_eq!(MediaKind::of("flac"), Some(MediaKind::Audio));
        assert_eq!(MediaKind::of("txt"), None);
    }

    #[test]
    fn parses_ffprobe_json() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"streams":[
                {"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"avg_frame_rate":"30000/1001"},
                {"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2},
                {"codec_type":"video","codec_name":"mjpeg","disposition":{"attached_pic":1}}],
              "format":{"duration":"3725.4","bit_rate":"5200000","format_long_name":"QuickTime / MOV",
                        "tags":{"TITLE":"Trip","artist":"Me"}}}"#,
        )
        .unwrap();
        let facts = probe_facts(&json);
        let get = |k: &str| facts.iter().find(|(f, _)| f == k).map(|(_, v)| v.as_str());
        assert_eq!(get("Title"), Some("Trip"));
        assert_eq!(get("Artist"), Some("Me"));
        assert_eq!(get("Duration"), Some("1:02:05"));
        assert_eq!(get("Video"), Some("h264 · 1920×1080 · 29.97 fps"));
        assert_eq!(get("Audio"), Some("aac · 48 kHz · stereo"));
        assert_eq!(get("Bitrate"), Some("5.2 Mb/s"));
        assert_eq!(facts.iter().filter(|(k, _)| k == "Video").count(), 1, "cover art is not a video stream");
    }

    #[test]
    fn parses_pdfinfo_and_key_value_output() {
        let facts =
            pdf_facts("Producer:       LaTeX\nPages:          12\nTitle:          Report\nEncrypted:      no\n");
        assert_eq!(
            facts,
            [("Title".into(), "Report".into()), ("Pages".into(), "12".into()), ("Producer".into(), "LaTeX".into())]
        );
        let kv = key_value_facts("General\nFormat                                   : MPEG-4\nDuration                                 : 1 min 5 s\nFormat : again\n");
        assert_eq!(kv, [("Format".into(), "MPEG-4".into()), ("Duration".into(), "1 min 5 s".into())]);
    }

    #[test]
    fn formats_durations_and_rates() {
        assert_eq!(duration(65.0), "1:05");
        assert_eq!(frame_rate("25/1").as_deref(), Some("25"));
        assert_eq!(frame_rate("0/0"), None);
    }

    #[test]
    fn missing_tools_give_a_hint() {
        let media = inspect(Path::new("/nonexistent/file.pdf"), MediaKind::Pdf, true);
        assert!(media.image.is_none());
        // With poppler installed pdfinfo fails on a missing file, so either way
        // nothing is shown and the hint explains what is needed.
        assert!(media.hint.is_some());
    }

    #[test]
    fn run_reports_missing_programs() {
        assert!(run("strata-no-such-tool", ["x"]).is_none());
    }
}
