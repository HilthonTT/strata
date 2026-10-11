//! Renaming many items with one pattern, for `:rename-pattern`.
//!
//! A pattern is either a template for the whole new name,
//! `photo-{n:3}{.ext}`, or a regex substitution, `s/IMG_(\d+)/photo-$1/`.
//! Templates may use:
//!
//! | Token | Value |
//! | --- | --- |
//! | `{name}` | name without the extension |
//! | `{ext}` / `{.ext}` | extension, without / with the dot (empty when there is none) |
//! | `{full}` | the whole current name |
//! | `{n}`, `{n:3}`, `{n:3:10}` | counter, zero-padded to 3 digits, starting at 10 |
//! | `{date}`, `{date:%Y%m%d}` | modification date |
//! | `{parent}` | name of the containing folder |
//! | `{1}`…`{9}`, `$1`…`$9` | regex groups (substitutions only); `{0}`/`$0` is the whole match |
//!
//! Any token takes `|upper`, `|lower` or `|title`: `{name|lower}`. Write
//! `{{` and `}}` for literal braces.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Context as _, Result};
use chrono::{DateTime, Local};
use regex::{Captures, Regex, RegexBuilder};

use crate::bulk::Rename;

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Name,
    Ext { dot: bool },
    Full,
    Counter { width: usize, start: usize },
    Date(String),
    Parent,
    Group(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Case {
    Keep,
    Upper,
    Lower,
    Title,
}

#[derive(Debug, Clone, PartialEq)]
enum Piece {
    Text(String),
    Token(Value, Case),
}

#[derive(Debug, Clone)]
pub struct Pattern {
    substitute: Option<(Regex, bool)>,
    template: Vec<Piece>,
}

/// What a pattern needs to know about one item.
pub struct Item<'a> {
    pub path: &'a Path,
    pub modified: Option<SystemTime>,
}

impl Pattern {
    pub fn parse(input: &str) -> Result<Self> {
        if input.is_empty() {
            bail!("type a pattern such as photo-{{n:3}}{{.ext}} or s/old/new/");
        }
        if let Some(rest) = input.strip_prefix('s').filter(|r| r.len() > 1) {
            let delim = rest.chars().next().expect("not empty");
            if !delim.is_alphanumeric() && delim != '{' {
                let parts = split_unescaped(&rest[delim.len_utf8()..], delim);
                let (find, replace, flags) = match parts.as_slice() {
                    [f, r] => (f.as_str(), r.as_str(), ""),
                    [f, r, flags] => (f.as_str(), r.as_str(), flags.as_str()),
                    _ => bail!("write substitutions as s/find/replace/ (flags: g, i)"),
                };
                if let Some(bad) = flags.chars().find(|c| !matches!(c, 'g' | 'i')) {
                    bail!("unknown flag '{bad}': use g (every match) or i (ignore case)");
                }
                let regex = RegexBuilder::new(find)
                    .case_insensitive(flags.contains('i'))
                    .build()
                    .with_context(|| format!("bad regex '{find}'"))?;
                let template = parse_template(replace, true)?;
                if let Some(group) = template.iter().find_map(|p| match p {
                    Piece::Token(Value::Group(g), _) if *g >= regex.captures_len() => Some(*g),
                    _ => None,
                }) {
                    bail!("the regex has no group {group}");
                }
                return Ok(Self { substitute: Some((regex, flags.contains('g'))), template });
            }
        }
        Ok(Self { substitute: None, template: parse_template(input, false)? })
    }

    /// The new name for `item`, the `index`-th item (from 0).
    pub fn apply(&self, item: &Item, index: usize) -> String {
        let name = item.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match &self.substitute {
            None => self.expand(&name, item, index, None),
            Some((regex, global)) => {
                let limit = if *global { 0 } else { 1 };
                regex.replacen(&name, limit, |caps: &Captures| self.expand(&name, item, index, Some(caps))).into_owned()
            }
        }
    }

    fn expand(&self, name: &str, item: &Item, index: usize, caps: Option<&Captures>) -> String {
        let (stem, ext) = split_ext(name);
        let mut out = String::new();
        for piece in &self.template {
            let (value, case) = match piece {
                Piece::Text(t) => {
                    out.push_str(t);
                    continue;
                }
                Piece::Token(v, c) => (v, *c),
            };
            let text = match value {
                Value::Name => stem.to_string(),
                Value::Ext { dot: false } => ext.to_string(),
                Value::Ext { dot: true } if ext.is_empty() => String::new(),
                Value::Ext { dot: true } => format!(".{ext}"),
                Value::Full => name.to_string(),
                Value::Counter { width, start } => format!("{:0width$}", start + index),
                Value::Date(format) => {
                    item.modified.map(|t| DateTime::<Local>::from(t).format(format).to_string()).unwrap_or_default()
                }
                Value::Parent => item
                    .path
                    .parent()
                    .and_then(Path::file_name)
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                Value::Group(g) => caps.and_then(|c| c.get(*g)).map(|m| m.as_str().to_string()).unwrap_or_default(),
            };
            out.push_str(&apply_case(&text, case));
        }
        out
    }
}

fn split_ext(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        _ => (name, ""),
    }
}

fn apply_case(text: &str, case: Case) -> String {
    match case {
        Case::Keep => text.to_string(),
        Case::Upper => text.to_uppercase(),
        Case::Lower => text.to_lowercase(),
        Case::Title => {
            let mut out = String::with_capacity(text.len());
            let mut start = true;
            for c in text.chars() {
                if start {
                    out.extend(c.to_uppercase());
                } else {
                    out.extend(c.to_lowercase());
                }
                start = !c.is_alphanumeric();
            }
            out
        }
    }
}

/// Splits on `delim` unless it is escaped with a backslash; the escape is
/// dropped for the delimiter only, so regex escapes survive. A trailing
/// empty part (after the closing delimiter) is dropped.
fn split_unescaped(input: &str, delim: char) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&delim) {
            parts.last_mut().expect("never empty").push(chars.next().expect("peeked"));
        } else if c == delim {
            parts.push(String::new());
        } else {
            parts.last_mut().expect("never empty").push(c);
        }
    }
    if parts.len() > 2 && parts.last().is_some_and(String::is_empty) {
        parts.pop();
    }
    parts
}

fn parse_template(input: &str, groups: bool) -> Result<Vec<Piece>> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                text.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                text.push('}');
            }
            '$' if groups && chars.peek().is_some_and(char::is_ascii_digit) => {
                let digit = chars.next().and_then(|d| d.to_digit(10)).expect("peeked a digit");
                pieces.push(Piece::Text(std::mem::take(&mut text)));
                pieces.push(Piece::Token(Value::Group(digit as usize), Case::Keep));
            }
            '{' => {
                let mut token = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => token.push(c),
                        None => bail!("'{{' without a closing '}}' (write {{{{ for a literal brace)"),
                    }
                }
                pieces.push(Piece::Text(std::mem::take(&mut text)));
                pieces.push(parse_token(&token, groups)?);
            }
            '}' => bail!("'}}' without an opening '{{' (write }}}} for a literal brace)"),
            c => text.push(c),
        }
    }
    pieces.push(Piece::Text(text));
    pieces.retain(|p| !matches!(p, Piece::Text(t) if t.is_empty()));
    Ok(pieces)
}

fn parse_token(token: &str, groups: bool) -> Result<Piece> {
    let (body, case) = match token.rsplit_once('|') {
        Some((body, case)) => (
            body,
            match case.trim() {
                "upper" => Case::Upper,
                "lower" => Case::Lower,
                "title" => Case::Title,
                other => bail!("unknown case '{other}': use upper, lower or title"),
            },
        ),
        None => (token, Case::Keep),
    };
    let (name, arg) = match body.split_once(':') {
        Some((n, a)) => (n.trim(), Some(a)),
        None => (body.trim(), None),
    };
    let value = match (name, arg) {
        ("name", None) => Value::Name,
        ("ext", None) => Value::Ext { dot: false },
        (".ext", None) => Value::Ext { dot: true },
        ("full", None) => Value::Full,
        ("parent", None) => Value::Parent,
        ("n", None) => Value::Counter { width: 1, start: 1 },
        ("n", Some(arg)) => {
            let (width, start) = arg.split_once(':').unwrap_or((arg, "1"));
            let number = |s: &str, what: &str| {
                s.trim().parse::<usize>().with_context(|| format!("{{n:{arg}}}: the {what} must be a number"))
            };
            let width = if width.trim().is_empty() { 1 } else { number(width, "width")? };
            Value::Counter { width: width.min(20), start: number(start, "start")? }
        }
        ("date", None) => Value::Date("%Y-%m-%d".into()),
        ("date", Some(format)) => {
            use chrono::format::{Item, StrftimeItems};
            if StrftimeItems::new(format).any(|i| matches!(i, Item::Error)) {
                bail!("'{format}' is not a valid date format");
            }
            Value::Date(format.to_string())
        }
        (digit, None) if groups && digit.len() == 1 && digit.chars().all(|c| c.is_ascii_digit()) => {
            Value::Group(digit.parse().expect("a digit"))
        }
        (digit, None) if digit.parse::<usize>().is_ok() => bail!("{{{digit}}} only works in s/find/replace/"),
        _ => bail!("unknown token {{{token}}}: use name, ext, .ext, full, n, date or parent"),
    };
    Ok(Piece::Token(value, case))
}

/// Renames grouped by folder, ready for [`crate::bulk::apply`].
pub type Plan = BTreeMap<PathBuf, Vec<Rename>>;

/// Works out every new name and checks them: no empty names, separators,
/// or two items ending up with the same name in one folder. `exists`
/// tells whether a name is taken in a folder by something not being
/// renamed.
pub fn plan(pattern: &Pattern, items: &[Item], exists: impl Fn(&Path) -> bool) -> Result<Plan> {
    let mut plan = Plan::new();
    let mut taken: HashSet<PathBuf> = HashSet::new();
    let sources: HashSet<&Path> = items.iter().map(|i| i.path).collect();
    for (index, item) in items.iter().enumerate() {
        let (Some(dir), Some(name)) = (item.path.parent(), item.path.file_name()) else { continue };
        let from = name.to_string_lossy().into_owned();
        let to = pattern.apply(item, index);
        if to.is_empty() || to == "." || to == ".." {
            bail!("'{from}' would get an empty name");
        }
        if to.contains(['/', '\\']) {
            bail!("'{to}' contains a path separator");
        }
        let target = dir.join(&to);
        if !taken.insert(target.clone()) {
            bail!("two items would be named '{to}'");
        }
        if to == from {
            continue;
        }
        if !sources.contains(target.as_path()) && exists(&target) {
            bail!("'{to}' already exists");
        }
        plan.entry(dir.to_path_buf()).or_default().push(Rename { from, to });
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rename(pattern: &str, names: &[&str]) -> Vec<String> {
        let p = Pattern::parse(pattern).unwrap();
        names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let path = PathBuf::from("/photos").join(n);
                p.apply(&Item { path: &path, modified: None }, i)
            })
            .collect()
    }

    #[test]
    fn templates() {
        assert_eq!(rename("photo-{n:3}{.ext}", &["a.JPG", "b"]), ["photo-001.JPG", "photo-002"]);
        assert_eq!(rename("{n:2:9}_{name|upper}.{ext|lower}", &["x.TXT"]), ["09_X.txt"]);
        assert_eq!(rename("{parent}-{full}", &["a.b.c"]), ["photos-a.b.c"]);
        assert_eq!(rename("{name|title}{.ext}", &["hello wORLD.md"]), ["Hello World.md"]);
        assert_eq!(rename("{{{name}}}", &["a"]), ["{a}"]);
        assert_eq!(rename("{name}", &[".bashrc"]), [".bashrc"], "dotfiles have no extension");
    }

    #[test]
    fn substitutions() {
        assert_eq!(rename(r"s/IMG_(\d+)/photo-$1/", &["IMG_42.jpg", "other.jpg"]), ["photo-42.jpg", "other.jpg"]);
        assert_eq!(rename("s/a/b/g", &["banana"]), ["bbnbnb"]);
        assert_eq!(rename("s/a/b/", &["banana"]), ["bbnana"]);
        assert_eq!(rename("s/A/b/i", &["a"]), ["b"]);
        assert_eq!(rename(r"s|\s+|_|g", &["a  b c"]), ["a_b_c"]);
        assert_eq!(rename(r"s/(.*)/{1|lower}/", &["LOUD.TXT"]), ["loud.txt"]);
        assert_eq!(rename(r"s/^/{n:2}-/", &["a", "b"]), ["01-a", "02-b"]);
        assert_eq!(rename(r"s/\//x/", &["a"]), ["a"], "escaped delimiters");
    }

    #[test]
    fn rejects_bad_patterns() {
        for bad in ["", "{nope}", "{n:x}", "{name|shout}", "a{b", "a}b", "s/(/x/", "s/a/b/q", "s/a/$2/", "{1}"] {
            assert!(Pattern::parse(bad).is_err(), "{bad} should be rejected");
        }
        assert!(Pattern::parse("sale").is_ok(), "a name starting with s is a template");
    }

    #[test]
    fn plans_check_collisions() {
        let paths = [PathBuf::from("/d/a.txt"), PathBuf::from("/d/b.txt"), PathBuf::from("/e/a.txt")];
        let items: Vec<Item> = paths.iter().map(|p| Item { path: p, modified: None }).collect();
        let p = Pattern::parse("x{.ext}").unwrap();
        assert!(plan(&p, &items, |_| false).unwrap_err().to_string().contains("two items"));

        let p = Pattern::parse("{name|upper}{.ext}").unwrap();
        let planned = plan(&p, &items, |_| false).unwrap();
        assert_eq!(planned.len(), 2, "grouped by folder");
        assert_eq!(planned[Path::new("/d")][1], Rename { from: "b.txt".into(), to: "B.txt".into() });
        assert!(plan(&p, &items, |t| t == Path::new("/d/A.txt")).is_err(), "an existing file is not overwritten");

        // A swap within the set is fine: the target is being renamed too.
        let two = [PathBuf::from("/d/1"), PathBuf::from("/d/2")];
        let items: Vec<Item> = two.iter().map(|p| Item { path: p, modified: None }).collect();
        let p = Pattern::parse("{n:1:2}").unwrap();
        let planned = plan(&p, &items, |t| two.iter().any(|s| s == t)).unwrap();
        assert_eq!(planned[Path::new("/d")].len(), 2);
    }
}
