//! Nerd Font icons by file type.

use ratatui::style::Color;
use strata_config::Theme;
use strata_core::{Entry, EntryKind};

pub fn icon(entry: &Entry, enabled: bool) -> &'static str {
    if !enabled {
        return if entry.is_dir() { "▸" } else { " " };
    }
    if entry.is_dir() {
        return match entry.name.as_str() {
            ".git" => "\u{e5fb}",
            "node_modules" => "\u{e5fa}",
            ".config" | ".github" => "\u{e5fc}",
            _ if entry.is_symlink() => "\u{f482}",
            _ => "\u{f07b}",
        };
    }
    match entry.name.as_str() {
        "Dockerfile" | "docker-compose.yml" | "compose.yaml" => return "\u{f308}",
        "Makefile" | "justfile" => return "\u{e779}",
        "Cargo.toml" | "Cargo.lock" => return "\u{e7a8}",
        "LICENSE" | "LICENSE.md" => return "\u{f0219}",
        ".gitignore" | ".gitattributes" => return "\u{f02a2}",
        _ => {}
    }
    match entry.extension().as_str() {
        "rs" => "\u{e7a8}",
        "py" => "\u{e606}",
        "js" | "mjs" | "cjs" => "\u{e74e}",
        "ts" | "tsx" => "\u{e628}",
        "go" => "\u{e626}",
        "c" | "h" => "\u{e61e}",
        "cpp" | "cc" | "hpp" => "\u{e61d}",
        "cs" => "\u{f031b}",
        "java" | "jar" => "\u{e738}",
        "lua" => "\u{e620}",
        "rb" => "\u{e739}",
        "php" => "\u{e73d}",
        "html" | "htm" => "\u{e736}",
        "css" | "scss" => "\u{e749}",
        "json" => "\u{e60b}",
        "toml" | "yaml" | "yml" | "ini" | "conf" | "cfg" => "\u{e615}",
        "md" | "markdown" => "\u{e73e}",
        "txt" | "log" => "\u{f15c}",
        "sh" | "bash" | "zsh" | "fish" | "ps1" => "\u{f489}",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "svg" | "tiff" => "\u{f1c5}",
        "mp4" | "mkv" | "webm" | "mov" | "avi" => "\u{f1c8}",
        "mp3" | "flac" | "wav" | "ogg" | "m4a" => "\u{f1c7}",
        "zip" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "7z" | "rar" | "zst" => "\u{f1c6}",
        "pdf" => "\u{f1c1}",
        "doc" | "docx" | "odt" => "\u{f1c2}",
        "xls" | "xlsx" | "ods" | "csv" => "\u{f1c3}",
        "ppt" | "pptx" | "odp" => "\u{f1c4}",
        "lock" => "\u{f023}",
        "exe" | "msi" | "appimage" => "\u{f013}",
        "iso" | "img" | "dmg" => "\u{f0a0}",
        "db" | "sqlite" | "sql" => "\u{f1c0}",
        _ if entry.is_symlink() => "\u{f481}",
        _ if entry.is_executable() => "\u{f489}",
        _ => "\u{f15b}",
    }
}

pub fn color(entry: &Entry, theme: &Theme) -> Color {
    if entry.is_dir() {
        return theme.dir;
    }
    if matches!(entry.kind, EntryKind::Symlink { .. }) {
        return theme.symlink;
    }
    match entry.extension().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "svg" | "tiff" => theme.image,
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "mp3" | "flac" | "wav" | "ogg" | "m4a" => {
            theme.media
        }
        "zip" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "7z" | "rar" | "zst" | "iso" | "deb"
        | "rpm" => theme.archive,
        "rs" | "py" | "js" | "ts" | "tsx" | "go" | "c" | "h" | "cpp" | "java" | "lua" | "rb"
        | "cs" | "php" => theme.code,
        _ if entry.is_executable() => theme.exec,
        _ if entry.is_hidden() => theme.muted,
        _ => theme.file,
    }
}
