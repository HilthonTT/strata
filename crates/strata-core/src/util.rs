use std::path::{Path, PathBuf};

/// Formats a byte count using binary units, e.g. `1.5 GiB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
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
    fn sizes_are_human_readable() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
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
