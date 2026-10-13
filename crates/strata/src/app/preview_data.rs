//! Structured data in the preview: CSV/TSV files as aligned tables, and
//! JSON, YAML and TOML as a coloured outline. Both read only what the
//! preview already loaded, so huge files stay fast.

use std::fmt::Write as _;

use anyhow::{Context, Result};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use strata_config::Theme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Bytes of a CSV file parsed, and rows shown.
const MAX_CSV_BYTES: usize = 256 * 1024;
const MAX_ROWS: usize = 1000;
/// Widest a table column gets before its cells are cut with an ellipsis.
const MAX_COL_WIDTH: usize = 24;
/// Containers deeper than this are summarised as `{…12 keys}`.
const MAX_DEPTH: usize = 8;
/// Items of one container listed before `… 40 more`.
const MAX_CHILDREN: usize = 200;
/// Longest string, and longest one-line container, shown in the outline.
const MAX_STRING: usize = 400;
const MAX_INLINE: usize = 48;

/// A preview with lines pinned at the top (a summary, a table header) above
/// lines that scroll.
pub struct Data {
    pub head: Vec<Line<'static>>,
    pub body: Vec<Line<'static>>,
}

// --- CSV ---------------------------------------------------------------------

pub fn is_csv(ext: &str) -> bool {
    matches!(ext, "csv" | "tsv" | "psv")
}

/// The field separator of a delimited file: the candidate that splits the
/// first lines into the same, largest number of fields.
pub fn sniff_delimiter(sample: &str, ext: &str) -> u8 {
    if ext == "tsv" {
        return b'\t';
    }
    let lines: Vec<&str> = sample.lines().filter(|l| !l.trim().is_empty()).take(20).collect();
    let mut best = (b',', 0usize, 0usize);
    for delim in *b",\t;|" {
        let counts: Vec<usize> = lines.iter().map(|l| count_outside_quotes(l, delim)).collect();
        // How many lines share the first line's count: consistency first.
        let Some(&first) = counts.first().filter(|&&c| c > 0) else { continue };
        let agree = counts.iter().filter(|&&c| c == first).count();
        if (agree, first) > (best.1, best.2) {
            best = (delim, agree, first);
        }
    }
    best.0
}

fn count_outside_quotes(line: &str, delim: u8) -> usize {
    let mut quoted = false;
    line.bytes()
        .filter(|&b| {
            if b == b'"' {
                quoted = !quoted;
            }
            b == delim && !quoted
        })
        .count()
}

/// Records of delimited text, with quoted fields (`"a, ""b"""`) and line
/// breaks inside quotes. With `partial`, the text was cut off, so its last
/// record may be incomplete and is dropped. Also returns the bytes parsed.
pub fn parse_csv(text: &str, delim: u8, max_rows: usize, partial: bool) -> (Vec<Vec<String>>, usize) {
    let delim = delim as char;
    let mut rows = Vec::new();
    let (mut row, mut field) = (Vec::new(), String::new());
    let (mut quoted, mut was_quoted) = (false, false);
    let mut chars = text.char_indices().peekable();
    let mut consumed = 0;
    while let Some((i, c)) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek().is_some_and(|&(_, n)| n == '"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() && !was_quoted => (quoted, was_quoted) = (true, true),
            c if c == delim => {
                row.push(std::mem::take(&mut field));
                was_quoted = false;
            }
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                was_quoted = false;
                // Blank lines are not records.
                if !(row.len() == 1 && row[0].is_empty()) {
                    rows.push(std::mem::take(&mut row));
                }
                row.clear();
                consumed = i + 1;
                if rows.len() >= max_rows {
                    return (rows, consumed);
                }
            }
            c => field.push(c),
        }
    }
    if !partial && (!field.is_empty() || !row.is_empty()) {
        row.push(field);
        rows.push(row);
        consumed = text.len();
    }
    (rows, consumed)
}

/// An aligned table: a summary and the header row stay pinned.
pub fn csv(text: &str, ext: &str, file_size: u64, theme: &Theme) -> Data {
    let mut cut = text.len().min(MAX_CSV_BYTES);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let partial = (cut as u64) < file_size;
    let delim = sniff_delimiter(&text[..cut], ext);
    let (mut rows, consumed) = parse_csv(&text[..cut], delim, MAX_ROWS + 1, partial);
    let header = if rows.is_empty() { Vec::new() } else { rows.remove(0) };
    let complete = !partial && rows.len() <= MAX_ROWS;
    rows.truncate(MAX_ROWS);

    let columns = rows.iter().map(Vec::len).chain([header.len()]).max().unwrap_or(0);
    let mut widths = vec![0; columns];
    for row in rows.iter().chain([&header]) {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell_text(cell).width().min(MAX_COL_WIDTH));
        }
    }
    // Columns of numbers line up on the right.
    let numeric: Vec<bool> = (0..columns)
        .map(|i| {
            let mut cells = rows.iter().filter_map(|r| r.get(i)).filter(|c| !c.trim().is_empty()).peekable();
            cells.peek().is_some() && cells.all(|c| c.trim().parse::<f64>().is_ok())
        })
        .collect();

    let name = match delim {
        b'\t' => "tab-separated",
        b';' => "semicolon-separated",
        b'|' => "pipe-separated",
        _ => "comma-separated",
    };
    let count = if complete {
        format!("{} rows", group(rows.len()))
    } else if consumed > 0 {
        // Estimated from the bytes the shown rows took.
        let estimate = (file_size as f64 * (rows.len() + 1) as f64 / consumed as f64) as usize;
        format!("first {} of ~{} rows", group(rows.len()), group(estimate.saturating_sub(1).max(rows.len())))
    } else {
        format!("{}+ rows", group(rows.len()))
    };
    let muted = Style::default().fg(theme.muted);
    let summary = format!(" {name} · {count} · {columns} columns");
    let mut head = vec![Line::styled(summary, muted)];
    let header_style = Style::default().fg(theme.palette.accent).add_modifier(Modifier::BOLD);
    head.push(table_row(&header, &widths, &numeric, header_style, muted));
    let rule: Vec<String> = widths.iter().map(|&w| "─".repeat(w)).collect();
    head.push(Line::styled(format!(" {}", rule.join("─┼─")), muted));
    let body = rows.iter().map(|r| table_row(r, &widths, &numeric, Style::default().fg(theme.fg), muted)).collect();
    Data { head, body }
}

fn table_row(cells: &[String], widths: &[usize], numeric: &[bool], style: Style, muted: Style) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, &w) in widths.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", muted));
        }
        let text = cells.get(i).map(|c| cell_text(c)).unwrap_or_default();
        spans.push(Span::styled(fit(&text, w, numeric[i]), style));
    }
    Line::from(spans)
}

/// A cell on one line: tabs and line breaks become spaces.
fn cell_text(cell: &str) -> String {
    cell.trim().chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// `text` cut to `width` columns with an ellipsis, then padded to it.
pub fn fit(text: &str, width: usize, right: bool) -> String {
    let mut out = String::new();
    if text.width() <= width {
        out.push_str(text);
    } else {
        let mut used = 0;
        for c in text.chars() {
            let w = c.width().unwrap_or(0);
            if used + w + 1 > width {
                break;
            }
            out.push(c);
            used += w;
        }
        out.push('…');
    }
    let pad = " ".repeat(width.saturating_sub(out.width()));
    if right {
        pad + &out
    } else {
        out + &pad
    }
}

/// `1234567` → `1,234,567`.
pub fn group(n: impl std::fmt::Display) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// --- JSON, YAML and TOML -----------------------------------------------------

/// The structured formats shown as an outline, by extension.
pub fn tree_format(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "json" | "geojson" | "jsonld" | "webmanifest" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        _ => return None,
    })
}

/// A parsed document, with keys in the order they were written.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Null,
    Bool(bool),
    Number(String),
    Str(String),
    /// TOML dates and times.
    Date(String),
    List(Vec<Node>),
    Map(Vec<(String, Node)>),
}

/// Parses `text` as `format` (from [`tree_format`]).
pub fn parse_tree(text: &str, format: &str) -> Result<Node> {
    match format {
        "JSON" => serde_json::from_str(text).context("invalid JSON"),
        "TOML" => {
            let table = toml::de::DeTable::parse(text).map_err(|e| anyhow::anyhow!("invalid TOML: {e}"))?;
            Ok(from_toml(toml::de::DeValue::Table(table.into_inner())))
        }
        "YAML" => {
            let mut docs = yaml_rust2::YamlLoader::load_from_str(text).context("invalid YAML")?;
            Ok(match docs.len() {
                0 => Node::Null,
                1 => from_yaml(docs.remove(0)),
                _ => Node::List(docs.into_iter().map(from_yaml).collect()),
            })
        }
        _ => anyhow::bail!("not a structured format"),
    }
}

fn from_yaml(y: yaml_rust2::Yaml) -> Node {
    use yaml_rust2::Yaml;
    match y {
        Yaml::Real(s) => Node::Number(s),
        Yaml::Integer(i) => Node::Number(i.to_string()),
        Yaml::String(s) => Node::Str(s),
        Yaml::Boolean(b) => Node::Bool(b),
        Yaml::Array(items) => Node::List(items.into_iter().map(from_yaml).collect()),
        Yaml::Hash(map) => Node::Map(map.into_iter().map(|(k, v)| (yaml_key(k), from_yaml(v))).collect()),
        Yaml::Alias(_) | Yaml::Null | Yaml::BadValue => Node::Null,
    }
}

/// toml keeps keys sorted; their positions in the file give the order.
fn from_toml(value: toml::de::DeValue) -> Node {
    use toml::de::DeValue;
    match value {
        DeValue::String(s) => Node::Str(s.into_owned()),
        DeValue::Integer(i) => Node::Number(i.to_string()),
        DeValue::Float(f) => Node::Number(f.to_string()),
        DeValue::Boolean(b) => Node::Bool(b),
        DeValue::Datetime(d) => Node::Date(d.to_string()),
        DeValue::Array(items) => Node::List(items.into_iter().map(|v| from_toml(v.into_inner())).collect()),
        DeValue::Table(table) => {
            let mut entries: Vec<_> = table.into_iter().collect();
            entries.sort_by_key(|(k, _)| k.span().start);
            Node::Map(
                entries.into_iter().map(|(k, v)| (k.into_inner().into_owned(), from_toml(v.into_inner()))).collect(),
            )
        }
    }
}

/// YAML keys can be any value; most are strings.
fn yaml_key(y: yaml_rust2::Yaml) -> String {
    use yaml_rust2::Yaml;
    match y {
        Yaml::String(s) | Yaml::Real(s) => s,
        Yaml::Integer(i) => i.to_string(),
        Yaml::Boolean(b) => b.to_string(),
        Yaml::Null => "~".into(),
        other => format!("{other:?}"),
    }
}

/// serde_json sorts keys into maps of its own; this keeps the file's order.
impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(NodeVisitor)
    }
}

struct NodeVisitor;

impl<'de> Visitor<'de> for NodeVisitor {
    type Value = Node;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("any value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Node, E> {
        Ok(Node::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Node, E> {
        Ok(Node::Number(v.to_string()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Node, E> {
        Ok(Node::Number(v.to_string()))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Node, E> {
        Ok(Node::Number(v.to_string()))
    }

    fn visit_str<E>(self, v: &str) -> Result<Node, E> {
        Ok(Node::Str(v.to_string()))
    }

    fn visit_string<E>(self, v: String) -> Result<Node, E> {
        Ok(Node::Str(v))
    }

    fn visit_unit<E>(self) -> Result<Node, E> {
        Ok(Node::Null)
    }

    fn visit_none<E>(self) -> Result<Node, E> {
        Ok(Node::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Node, D::Error> {
        Node::deserialize(d)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Node::List(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
        let mut entries: Vec<(String, Node)> = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            entries.push((key, map.next_value()?));
        }
        Ok(Node::Map(entries))
    }
}

impl Node {
    fn summary(&self) -> String {
        match self {
            Node::List(items) => plural(items.len(), "item"),
            Node::Map(entries) => plural(entries.len(), "key"),
            _ => String::new(),
        }
    }

    fn is_container(&self) -> bool {
        matches!(self, Node::List(_) | Node::Map(_))
    }
}

fn plural(n: usize, what: &str) -> String {
    format!("{} {what}{}", group(n), if n == 1 { "" } else { "s" })
}

/// The document as an indented outline: `key: value` for scalars, a count
/// for containers with their contents below, and short lists of scalars on
/// one line. Containers deeper than [`MAX_DEPTH`] are summarised.
pub fn tree(root: &Node, format: &str, theme: &Theme, max_lines: usize) -> Data {
    let mut out = Outline { lines: Vec::new(), theme, max_lines };
    let kind = match root {
        Node::Map(_) => format!("object · {}", root.summary()),
        Node::List(_) => format!("list · {}", root.summary()),
        _ => "value".into(),
    };
    let head = vec![Line::styled(format!(" {format} · {kind}"), Style::default().fg(theme.muted))];
    out.children(root, 0, 0);
    if !root.is_container() {
        out.value(0, Vec::new(), root, 0);
    }
    // A nested container can run a few lines past the limit.
    out.lines.truncate(max_lines);
    Data { head, body: out.lines }
}

struct Outline<'t> {
    lines: Vec<Line<'static>>,
    theme: &'t Theme,
    max_lines: usize,
}

impl Outline<'_> {
    fn full(&self) -> bool {
        self.lines.len() >= self.max_lines
    }

    fn muted(&self) -> Style {
        Style::default().fg(self.theme.muted)
    }

    /// Lists the entries of a container at `indent`.
    fn children(&mut self, node: &Node, indent: usize, depth: usize) {
        let total = match node {
            Node::List(items) => {
                for item in items.iter().take(MAX_CHILDREN) {
                    let lead = vec![Span::styled("- ", self.muted())];
                    self.value(indent, lead, item, depth);
                    if self.full() {
                        return;
                    }
                }
                items.len()
            }
            Node::Map(entries) => {
                for (key, value) in entries.iter().take(MAX_CHILDREN) {
                    let lead = vec![
                        Span::styled(show_key(key), Style::default().fg(self.theme.palette.yellow)),
                        Span::styled(": ", self.muted()),
                    ];
                    self.value(indent, lead, value, depth);
                    if self.full() {
                        return;
                    }
                }
                entries.len()
            }
            _ => return,
        };
        if total > MAX_CHILDREN {
            let rest = format!("{}… {} more", " ".repeat(indent + 1), group(total - MAX_CHILDREN));
            self.lines.push(Line::styled(rest, self.muted()));
        }
    }

    /// One entry: `lead` (a key or a list dash) then the value.
    fn value(&mut self, indent: usize, lead: Vec<Span<'static>>, node: &Node, depth: usize) {
        let mut spans = vec![Span::raw(" ".repeat(indent + 1))];
        spans.extend(lead);
        let empty = matches!(node, Node::List(v) if v.is_empty()) || matches!(node, Node::Map(v) if v.is_empty());
        if !node.is_container() || empty {
            spans.extend(self.scalar(node));
            self.lines.push(Line::from(spans));
            return;
        }
        if let Some(inline) = self.inline(node) {
            spans.extend(inline);
            self.lines.push(Line::from(spans));
            return;
        }
        let (open, close) = if matches!(node, Node::List(_)) { ("[", "]") } else { ("{", "}") };
        if depth + 1 >= MAX_DEPTH {
            spans.push(Span::styled(format!("{open}…{}{close}", node.summary()), self.muted()));
            self.lines.push(Line::from(spans));
            return;
        }
        spans.push(Span::styled(format!("{open}{}{close}", node.summary()), self.muted()));
        self.lines.push(Line::from(spans));
        self.children(node, indent + 2, depth + 1);
    }

    /// `[1, 2, 3]` or `{a: 1, b: 2}` when every item is a short scalar.
    fn inline(&self, node: &Node) -> Option<Vec<Span<'static>>> {
        let comma = || Span::styled(", ", self.muted());
        let mut spans = Vec::new();
        match node {
            Node::List(items) => {
                spans.push(Span::styled("[", self.muted()));
                for (i, item) in items.iter().enumerate() {
                    if item.is_container() {
                        return None;
                    }
                    if i > 0 {
                        spans.push(comma());
                    }
                    spans.extend(self.scalar(item));
                }
                spans.push(Span::styled("]", self.muted()));
            }
            Node::Map(entries) => {
                spans.push(Span::styled("{", self.muted()));
                for (i, (key, value)) in entries.iter().enumerate() {
                    if value.is_container() {
                        return None;
                    }
                    if i > 0 {
                        spans.push(comma());
                    }
                    spans.push(Span::styled(show_key(key), Style::default().fg(self.theme.palette.yellow)));
                    spans.push(Span::styled(": ", self.muted()));
                    spans.extend(self.scalar(value));
                }
                spans.push(Span::styled("}", self.muted()));
            }
            _ => return None,
        }
        let width: usize = spans.iter().map(|s| s.content.width()).sum();
        (width <= MAX_INLINE).then_some(spans)
    }

    fn scalar(&self, node: &Node) -> Vec<Span<'static>> {
        let p = &self.theme.palette;
        let (text, color) = match node {
            Node::Null => ("null".to_string(), p.purple),
            Node::Bool(b) => (b.to_string(), p.purple),
            Node::Number(n) => (n.clone(), p.orange),
            Node::Date(d) => (d.clone(), p.cyan),
            Node::Str(s) => (quote(s), p.green),
            Node::List(_) => ("[]".into(), self.theme.muted),
            Node::Map(_) => ("{}".into(), self.theme.muted),
        };
        vec![Span::styled(text, Style::default().fg(color))]
    }
}

/// A string in quotes, with escapes for line breaks and control
/// characters, cut short when very long.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for (i, c) in s.chars().enumerate() {
        if i == MAX_STRING {
            out.push('…');
            break;
        }
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Keys as written, quoted when they would be ambiguous.
fn show_key(key: &str) -> String {
    let plain = !key.is_empty() && !key.chars().any(|c| c.is_whitespace() || c.is_control() || c == ':' || c == '"');
    if plain {
        key.to_string()
    } else {
        quote(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        strata_config::theme::builtin().into_iter().next().unwrap()
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn sniffs_the_delimiter() {
        assert_eq!(sniff_delimiter("a,b,c\n1,2,3\n", "csv"), b',');
        assert_eq!(sniff_delimiter("a;b;c\n1,5;2;3\n", "csv"), b';');
        assert_eq!(sniff_delimiter("a\tb\n1\t2\n", "txt"), b'\t');
        assert_eq!(sniff_delimiter("a|b|c\n1|2|3\n", "csv"), b'|');
        // Commas inside quotes don't count.
        assert_eq!(sniff_delimiter("name;note\n\"x\";\"a, b, c\"\n", "csv"), b';');
        assert_eq!(sniff_delimiter("a,b\n", "tsv"), b'\t');
    }

    #[test]
    fn parses_quoted_fields() {
        let (rows, _) = parse_csv("a,\"b, c\",\"say \"\"hi\"\"\"\r\n1,\"two\nlines\",3\n\n", b',', 10, false);
        assert_eq!(rows, vec![vec!["a", "b, c", "say \"hi\""], vec!["1", "two\nlines", "3"]]);
        // No trailing newline: the last record still counts.
        let (rows, _) = parse_csv("x,y\n1,2", b',', 10, false);
        assert_eq!(rows.len(), 2);
        // Cut-off text drops the incomplete record.
        let (rows, consumed) = parse_csv("x,y\n1,2\n3,", b',', 10, true);
        assert_eq!((rows.len(), consumed), (2, 8));
        let (rows, _) = parse_csv("1\n2\n3\n4\n", b',', 2, false);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn tables_align_and_truncate() {
        let long = "x".repeat(40);
        let csv_text = format!("id,name,score\n1,{long},3.5\n22,bo,10\n");
        let data = csv(&csv_text, "csv", csv_text.len() as u64, &theme());
        assert_eq!(text(&data.head[0]), " comma-separated · 2 rows · 3 columns");
        assert_eq!(text(&data.head[1]), format!(" id │ {} │ score", fit("name", MAX_COL_WIDTH, false)));
        let row = text(&data.body[0]);
        assert!(row.contains(&format!("{}…", "x".repeat(MAX_COL_WIDTH - 1))), "{row}");
        // Numbers line up on the right.
        assert!(row.starts_with("  1 │"), "{row}");
        assert!(text(&data.body[1]).ends_with("   10"));
        assert_eq!(data.body.len(), 2);
    }

    #[test]
    fn estimates_rows_of_files_read_in_part() {
        let csv_text = "a,b\n1,2\n3,4\n5,";
        let data = csv(csv_text, "csv", 1200, &theme());
        assert!(text(&data.head[0]).contains("first 2 of ~"), "{}", text(&data.head[0]));
        assert_eq!(fit("abcdef", 4, false), "abc…");
        assert_eq!(fit("ab", 4, true), "  ab");
        assert_eq!(group(1234567), "1,234,567");
    }

    #[test]
    fn keeps_key_order_in_all_formats() {
        let json = parse_tree(r#"{"z": 1, "a": [true, null], "m": {"x": "y"}}"#, "JSON").unwrap();
        let toml = parse_tree("z = 1\na = [true]\n[m]\nx = \"y\"\n", "TOML").unwrap();
        let yaml = parse_tree("z: 1\na: [true, ~]\nm:\n  x: y\n", "YAML").unwrap();
        for node in [&json, &toml, &yaml] {
            let Node::Map(entries) = node else { panic!("{node:?}") };
            let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
            assert_eq!(keys, ["z", "a", "m"]);
        }
        assert_eq!(json, yaml);
        let date = parse_tree("when = 1979-05-27T07:32:00Z", "TOML").unwrap();
        assert_eq!(date, Node::Map(vec![("when".into(), Node::Date("1979-05-27T07:32:00Z".into()))]));
        assert!(parse_tree("{not json", "JSON").is_err());
        assert!(parse_tree("a = = 1", "TOML").is_err());
    }

    #[test]
    fn outlines_and_collapses_deep_nodes() {
        let node = parse_tree(
            r#"{"name": "strata", "tags": ["a", "b"], "deps": [{"n": 1, "long": "this string is long enough to stop inlining"}]}"#,
            "JSON",
        )
        .unwrap();
        let data = tree(&node, "JSON", &theme(), 100);
        let lines: Vec<String> = data.body.iter().map(text).collect();
        assert_eq!(text(&data.head[0]), " JSON · object · 3 keys");
        assert_eq!(
            lines,
            [
                " name: \"strata\"",
                " tags: [\"a\", \"b\"]",
                " deps: [1 item]",
                "   - {2 keys}",
                "     n: 1",
                "     long: \"this string is long enough to stop inlining\"",
            ]
        );
        // Ten levels deep, with a long string so nothing inlines.
        let mut deep = String::from(r#""end of the line, long enough not to fit inline""#);
        for i in 0..10 {
            deep = format!(r#"{{"k{i}": {deep}, "pad": "padding padding padding padding padding"}}"#);
        }
        let data = tree(&parse_tree(&deep, "JSON").unwrap(), "JSON", &theme(), 1000);
        let collapsed: Vec<String> = data.body.iter().map(text).filter(|l| l.contains('…')).collect();
        assert_eq!(collapsed.len(), 1, "{collapsed:?}");
        assert!(collapsed[0].ends_with("k2: {…2 keys}"), "{}", collapsed[0]);
    }

    #[test]
    fn long_containers_and_strings_are_cut() {
        let items: Vec<String> = (0..250).map(|i| format!("{{\"i\": {i}, \"s\": \"{}\"}}", "y".repeat(50))).collect();
        let node = parse_tree(&format!("[{}]", items.join(",")), "JSON").unwrap();
        let data = tree(&node, "JSON", &theme(), 100_000);
        assert_eq!(text(data.body.last().unwrap()), " … 50 more");
        let data = tree(&node, "JSON", &theme(), 10);
        assert_eq!(data.body.len(), 10);
        assert_eq!(quote("a\nb\"c"), "\"a\\nb\\\"c\"");
        assert_eq!(quote(&"z".repeat(MAX_STRING + 10)).chars().count(), MAX_STRING + 3);
        assert_eq!(show_key("has space"), "\"has space\"");
    }
}
