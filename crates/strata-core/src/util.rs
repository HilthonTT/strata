use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

static SI_UNITS: AtomicBool = AtomicBool::new(false);

/// Makes [`human_size`] use powers of 1000 (`kB`, `MB`) instead of 1024.
pub fn set_si_units(si: bool) {
    SI_UNITS.store(si, Ordering::Relaxed);
}

/// Formats a byte count, e.g. `1.5 GiB` (or `1.6 GB` after [`set_si_units`]).
pub fn human_size(bytes: u64) -> String {
    if SI_UNITS.load(Ordering::Relaxed) {
        format_size(bytes, 1000.0, &["B", "kB", "MB", "GB", "TB", "PB"])
    } else {
        format_size(bytes, 1024.0, &["B", "KiB", "MiB", "GiB", "TiB", "PiB"])
    }
}

fn format_size(bytes: u64, base: f64, units: &[&str]) -> String {
    if (bytes as f64) < base {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= base && unit < units.len() - 1 {
        value /= base;
        unit += 1;
    }
    format!("{value:.1} {}", units[unit])
}

/// Renders unix permission bits as `rwxr-xr-x`.
pub fn permissions_string(mode: u32) -> String {
    let mut out = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        out.push(if bits & 4 != 0 { 'r' } else { '-' });
        out.push(if bits & 2 != 0 { 'w' } else { '-' });
        out.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    out
}

/// Minimal glob: `*` matches any run, `?` one character; case-insensitive.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => go(&p[1..], &n[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}

/// Converts a path to a forward-slash string, as remote backends expect.
pub fn posix(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.is_empty() {
        "/".to_string()
    } else {
        s
    }
}

/// Joins a remote (POSIX) path without relying on the host separator.
pub fn posix_join(base: &Path, name: &str) -> PathBuf {
    let base = posix(base);
    if base.ends_with('/') {
        PathBuf::from(format!("{base}{name}"))
    } else {
        PathBuf::from(format!("{base}/{name}"))
    }
}

/// Expands a leading `~` to the home directory.
pub fn expand_tilde(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix('~') {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest.trim_start_matches(['/', '\\']));
        }
    }
    PathBuf::from(input)
}

/// Returns a name that does not collide with `exists`, e.g. `file (1).txt`.
pub fn unique_name(name: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    (1..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|candidate| !exists(candidate))
        .expect("an unused name always exists")
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 (RFC 4648) with `=` padding.
pub fn base64(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decodes standard base64, with or without padding. `None` on any other
/// character.
pub fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for c in input.trim_end_matches('=').bytes() {
        let v = BASE64.iter().position(|&b| b == c)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Splits a command line into arguments, honouring single quotes, double
/// quotes and backslash escapes: `code --wait "my dir"` → 3 arguments.
pub fn split_command(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    let mut chars = line.chars();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') | (None, '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_arg = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
            }
            (None, c) => {
                current.push(c);
                in_arg = true;
            }
        }
    }
    if in_arg {
        args.push(current);
    }
    args
}

/// Shell-quotes a single argument for POSIX `sh`.
pub fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        for text in ["", "f", "fo", "foo", "foobar"] {
            assert_eq!(base64_decode(&base64(text.as_bytes())).unwrap(), text.as_bytes());
        }
        assert_eq!(base64_decode("Zm8").unwrap(), b"fo", "padding is optional");
        assert_eq!(base64_decode("Zm9v!"), None);
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
        let si = ["B", "kB", "MB", "GB", "TB", "PB"];
        assert_eq!(format_size(999, 1000.0, &si), "999 B");
        assert_eq!(format_size(1500, 1000.0, &si), "1.5 kB");
    }

    #[test]
    fn permissions_render() {
        assert_eq!(permissions_string(0o755), "rwxr-xr-x");
        assert_eq!(permissions_string(0o640), "rw-r-----");
    }

    #[test]
    fn unique_names_skip_existing() {
        let taken = ["a.txt", "a (1).txt"];
        assert_eq!(unique_name("a.txt", |n| taken.contains(&n)), "a (2).txt");
        assert_eq!(unique_name("b", |n| taken.contains(&n)), "b");
        assert_eq!(unique_name(".bashrc", |n| n == ".bashrc"), ".bashrc (1)");
    }

    #[test]
    fn splits_command_lines() {
        assert_eq!(split_command("code --wait"), ["code", "--wait"]);
        assert_eq!(split_command(r#"sh -c 'echo hi > out' "a b" c\ d"#), ["sh", "-c", "echo hi > out", "a b", "c d"]);
        assert_eq!(split_command("x ''"), ["x", ""]);
        assert!(split_command("   ").is_empty());
    }

    #[test]
    fn quoting_escapes_single_quotes() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
