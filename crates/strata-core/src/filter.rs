//! Queries for the panel filter (`/`) and `:select`.
//!
//! | Query | Matches |
//! | --- | --- |
//! | `rdm` | fuzzy, best first (`README.md`) |
//! | `re:^v\d+` | a regex anywhere in the name; smart case |
//! | `*.{jpg,png}`, `IMG_????.*`, `[ab]*` | a glob over the whole name; ignores case |
//! | `g:<glob>` | a glob even without `*`, `?`, `[..]` or `{a,b}` |
//!
//! A query is a glob when it holds `*` or `?`, a closed `[..]` or a
//! `{..,..}` group. Globs and regexes match the name, or with a `/` in a
//! glob the path relative to the panel's folder (`src/**/*.rs`); `*` and
//! `?` stop at `/`, `**` does not. Their matches keep the listing's order.

use regex::{Regex, RegexBuilder};

use crate::search::{Fuzzy, Match};

#[derive(Debug, Clone)]
pub enum Matcher {
    Fuzzy(String),
    Regex(Regex),
    Glob {
        regex: Regex,
        /// Matched against the whole relative path, not just the name.
        path: bool,
    },
}

impl Matcher {
    /// A filter query: plain text is fuzzy.
    pub fn parse(query: &str) -> Result<Self, String> {
        Self::parse_with(query, false)
    }

    /// A `:select` pattern: plain text is a glob, so `notes.txt` marks that
    /// name only.
    pub fn parse_selection(query: &str) -> Result<Self, String> {
        Self::parse_with(query, true)
    }

    fn parse_with(query: &str, plain_is_glob: bool) -> Result<Self, String> {
        if let Some(pattern) = query.strip_prefix("re:") {
            let smart_case = !pattern.chars().any(char::is_uppercase);
            return RegexBuilder::new(pattern).case_insensitive(smart_case).build().map(Self::Regex).map_err(|e| {
                format!(
                    "bad regex: {}",
                    e.to_string().lines().last().unwrap_or("invalid").trim_start_matches("error: ")
                )
            });
        }
        if let Some(glob) = query.strip_prefix("g:") {
            return Self::glob(glob);
        }
        if plain_is_glob || looks_like_glob(query) {
            // Text that only looks like a glob (`[draft` or `{x}`) stays fuzzy.
            return match Self::glob(query) {
                Err(_) if !plain_is_glob => Ok(Self::Fuzzy(query.to_string())),
                other => other,
            };
        }
        Ok(Self::Fuzzy(query.to_string()))
    }

    fn glob(glob: &str) -> Result<Self, String> {
        let source = glob_to_regex(glob)?;
        let regex = RegexBuilder::new(&source).case_insensitive(true).build().map_err(|e| format!("bad glob: {e}"))?;
        Ok(Self::Glob { regex, path: glob.contains('/') })
    }

    /// The items that match, with the matched characters for highlighting:
    /// fuzzy matches best first, others in their original order.
    pub fn filter<'a>(&self, fuzzy: &mut Fuzzy, items: impl IntoIterator<Item = &'a str>) -> Vec<Match> {
        match self {
            Self::Fuzzy(query) => fuzzy.filter(query, items),
            Self::Regex(regex) => items
                .into_iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    let m = regex.find(item)?;
                    let start = item[..m.start()].chars().count() as u32;
                    let positions = (start..start + m.as_str().chars().count() as u32).collect();
                    Some(Match { index, score: 0, positions })
                })
                .collect(),
            Self::Glob { regex, path } => items
                .into_iter()
                .enumerate()
                .filter(|(_, item)| {
                    let subject = if *path { item } else { item.rsplit('/').next().unwrap_or(item) };
                    regex.is_match(subject)
                })
                .map(|(index, _)| Match { index, score: 0, positions: Vec::new() })
                .collect(),
        }
    }
}

/// Uses glob syntax: `*`, `?`, a closed `[..]` or a `{..,..}` group.
fn looks_like_glob(query: &str) -> bool {
    let closed = |open: char, close: char, inner: Option<char>| {
        query.find(open).is_some_and(|i| {
            let rest = &query[i + 1..];
            rest.find(close).is_some_and(|j| j > 0 && inner.is_none_or(|c| rest[..j].contains(c)))
        })
    };
    query.contains(['*', '?']) || closed('[', ']', None) || closed('{', '}', Some(','))
}

/// Translates a glob into an anchored regex.
fn glob_to_regex(glob: &str) -> Result<String, String> {
    let chars: Vec<char> = glob.chars().collect();
    let mut out = String::from("^");
    let mut groups = 0;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                out.push_str(".*");
                i += 1;
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            '[' => match class(&chars[i + 1..]) {
                Some((class, used)) => {
                    out.push_str(&class);
                    i += used;
                }
                None => out.push_str(r"\["),
            },
            '{' => {
                groups += 1;
                out.push_str("(?:");
            }
            ',' if groups > 0 => out.push('|'),
            '}' if groups > 0 => {
                groups -= 1;
                out.push(')');
            }
            '\\' if i + 1 < chars.len() => {
                i += 1;
                out.push_str(&regex::escape(&chars[i].to_string()));
            }
            c => out.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    if groups > 0 {
        return Err("bad glob: a { has no closing }".into());
    }
    out.push('$');
    Ok(out)
}

/// A `[..]` class starting just after the `[`: the regex for it and how
/// many characters it used, closing `]` included. `None` when unclosed.
/// `[!..]` and `[^..]` negate; a `]` first is literal.
fn class(rest: &[char]) -> Option<(String, usize)> {
    let negate = matches!(rest.first(), Some('!' | '^'));
    let start = usize::from(negate);
    let end = start + 1 + rest.get(start + 1..)?.iter().position(|c| *c == ']')?;
    let inner = &rest[start..end];
    let mut out = String::from(if negate { "[^" } else { "[" });
    for (k, c) in inner.iter().enumerate() {
        if *c == '-' && k > 0 && k + 1 < inner.len() {
            out.push('-');
        } else {
            out.push_str(&regex::escape(&c.to_string()));
        }
    }
    out.push(']');
    Some((out, end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: [&str; 7] =
        ["photo.jpg", "Photo.PNG", "notes.txt", "v12-release", "[draft] plan.md", "src/main.rs", "a-b"];

    fn matches(query: &str) -> Vec<&'static str> {
        let m = Matcher::parse(query).unwrap();
        m.filter(&mut Fuzzy::default(), NAMES).into_iter().map(|m| NAMES[m.index]).collect()
    }

    #[test]
    fn plain_text_stays_fuzzy() {
        assert!(matches!(Matcher::parse("pht").unwrap(), Matcher::Fuzzy(_)));
        assert_eq!(matches("ntxt"), ["notes.txt"]);
        // Brackets and braces that are not glob syntax are plain text.
        assert!(matches!(Matcher::parse("[draft").unwrap(), Matcher::Fuzzy(_)));
        assert!(matches!(Matcher::parse("{x}").unwrap(), Matcher::Fuzzy(_)));
    }

    #[test]
    fn globs_are_detected_and_ignore_case() {
        assert_eq!(matches("*.{jpg,png}"), ["photo.jpg", "Photo.PNG"]);
        assert_eq!(matches("photo.???"), ["photo.jpg", "Photo.PNG"]);
        assert_eq!(matches("[n-p]*"), ["photo.jpg", "Photo.PNG", "notes.txt"]);
        assert_eq!(matches("[!np]*"), ["v12-release", "[draft] plan.md", "src/main.rs", "a-b"]);
        assert_eq!(matches("[[]draft]*"), ["[draft] plan.md"]);
        assert_eq!(matches("*.rs"), ["src/main.rs"], "without a / the name is matched");
        assert_eq!(matches("src/*"), ["src/main.rs"]);
        assert_eq!(matches("*/*.rs"), ["src/main.rs"]);
        assert_eq!(matches("**.rs"), ["src/main.rs"]);
        assert_eq!(matches("g:notes.txt"), ["notes.txt"], "g: forces a glob");
        assert!(matches("g:notes").is_empty(), "globs match the whole name");
        assert!(Matcher::parse("g:{a,b").is_err());
    }

    #[test]
    fn regexes_use_smart_case_and_highlight_the_match() {
        assert_eq!(matches(r"re:^v\d+"), ["v12-release"]);
        assert_eq!(matches("re:photo"), ["photo.jpg", "Photo.PNG"]);
        assert_eq!(matches("re:Photo"), ["Photo.PNG"], "upper case makes it case-sensitive");
        assert_eq!(matches("re:"), NAMES, "an empty regex matches everything");
        let m = Matcher::parse("re:é.").unwrap().filter(&mut Fuzzy::default(), ["café!"]);
        assert_eq!(m[0].positions, [3, 4], "positions count characters");
        let err = Matcher::parse("re:(unclosed").unwrap_err();
        assert!(err.starts_with("bad regex"), "{err}");
    }

    #[test]
    fn selections_treat_plain_text_as_a_glob() {
        let select = |q: &str| {
            let m = Matcher::parse_selection(q).unwrap();
            m.filter(&mut Fuzzy::default(), NAMES).into_iter().map(|m| NAMES[m.index]).collect::<Vec<_>>()
        };
        assert_eq!(select("notes.txt"), ["notes.txt"]);
        assert!(select("notes").is_empty());
        assert_eq!(select("re:^a"), ["a-b"]);
        assert!(Matcher::parse_selection("{a").is_err(), "no fuzzy fallback when selecting");
    }
}
