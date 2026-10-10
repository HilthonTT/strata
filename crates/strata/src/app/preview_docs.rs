//! Text of office documents and e-books for the preview: Word (`.docx`),
//! PowerPoint (`.pptx`), Excel (`.xlsx`), OpenDocument (`.odt`, `.odp`,
//! `.ods`) and EPUB. They are all zip files of XML, read without any
//! external program.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use anyhow::{bail, Context, Result};

/// Rows of a spreadsheet shown per sheet, and sheets shown.
const MAX_ROWS: usize = 200;
const MAX_SHEETS: usize = 8;
/// Paragraphs read from slide decks and e-books.
const MAX_BLOCKS: usize = 2000;

/// Biggest document read for a preview.
pub const MAX_DOC_BYTES: u64 = 64 * 1024 * 1024;

pub fn is_document(ext: &str) -> bool {
    matches!(ext, "docx" | "pptx" | "xlsx" | "odt" | "odp" | "ods" | "epub")
}

/// A piece of a document, in reading order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// Title, heading, slide or sheet name.
    Heading(String),
    Para(String),
    /// Spreadsheet or table row.
    Row(Vec<String>),
}

/// Extracts the text of a document from its bytes.
pub fn extract(data: Vec<u8>, ext: &str) -> Result<Vec<Block>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(data)).context("not a valid document (zip)")?;
    let mut blocks = match ext {
        "docx" => word(&read(&mut zip, "word/document.xml")?),
        "pptx" => slides(&mut zip)?,
        "xlsx" => sheets(&mut zip)?,
        "odt" | "odp" | "ods" => open_document(&read(&mut zip, "content.xml")?),
        "epub" => epub(&mut zip)?,
        _ => bail!("not a document"),
    };
    blocks.retain(|b| !matches!(b, Block::Para(p) if p.trim().is_empty()));
    Ok(blocks)
}

type Zip = zip::ZipArchive<Cursor<Vec<u8>>>;

fn read(zip: &mut Zip, name: &str) -> Result<String> {
    let file = zip.by_name(name).with_context(|| format!("{name} is missing"))?;
    // The archive's size says nothing about what an entry expands to.
    let mut bytes = Vec::new();
    file.take(MAX_DOC_BYTES).read_to_end(&mut bytes).with_context(|| format!("cannot read {name}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

// --- XML ---------------------------------------------------------------------

/// A token of an XML document.
#[derive(Debug, PartialEq, Eq)]
enum Xml<'a> {
    /// Local name (no prefix) and the raw attribute text.
    Open(&'a str, &'a str),
    Close(&'a str),
    /// Decoded text between tags.
    Text(String),
}

/// A forgiving XML tokenizer: enough for the text of office formats.
fn tokens<'a>(xml: &'a str) -> Vec<Xml<'a>> {
    let mut out = Vec::new();
    let mut rest = xml;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            out.push(Xml::Text(decode(rest)));
            break;
        };
        if lt > 0 {
            out.push(Xml::Text(decode(&rest[..lt])));
        }
        rest = &rest[lt..];
        // Comments, CDATA, declarations and processing instructions.
        let skip = [("<!--", "-->"), ("<![CDATA[", "]]>"), ("<?", "?>"), ("<!", ">")];
        if let Some((open, close)) = skip.iter().find(|(o, _)| rest.starts_with(o)) {
            // An unclosed section runs to the end of the document.
            let (body_end, end) = match rest[open.len()..].find(close) {
                Some(i) => (open.len() + i, open.len() + i + close.len()),
                None => (rest.len(), rest.len()),
            };
            if *open == "<![CDATA[" {
                out.push(Xml::Text(rest[open.len()..body_end].to_string()));
            }
            rest = &rest[end..];
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        let local = |name: &'a str| name.rsplit(':').next().unwrap_or(name);
        if let Some(name) = tag.strip_prefix('/') {
            out.push(Xml::Close(local(name.trim())));
            continue;
        }
        let self_closing = tag.ends_with('/');
        let tag = tag.trim_end_matches('/');
        let (name, attrs) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
        out.push(Xml::Open(local(name), attrs));
        if self_closing {
            out.push(Xml::Close(local(name)));
        }
    }
    out
}

/// Replaces `&amp;`, `&#233;`, `&#xE9;`… with their characters.
fn decode(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest.find(';').filter(|&i| i <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            e if e.starts_with("#x") || e.starts_with("#X") => {
                u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32)
            }
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The value of attribute `name` (with its prefix, e.g. `r:id`).
fn attr(attrs: &str, name: &str) -> Option<String> {
    let mut rest = attrs;
    while let Some(i) = rest.find(name) {
        let before_ok = i == 0 || rest[..i].ends_with(char::is_whitespace);
        let after = rest[i + name.len()..].trim_start();
        if before_ok {
            if let Some(value) = after.strip_prefix('=') {
                let value = value.trim_start();
                let quote = value.chars().next()?;
                if quote == '"' || quote == '\'' {
                    let end = value[1..].find(quote)?;
                    return Some(decode(&value[1..1 + end]));
                }
            }
        }
        rest = &rest[i + name.len()..];
    }
    None
}

/// Collects paragraphs and table rows from a token stream. `para` and
/// `heading` are the elements that end a paragraph or heading; text inside
/// table cells goes to the row instead.
struct Collector {
    blocks: Vec<Block>,
    text: String,
    heading: bool,
    cells: Option<Vec<String>>,
    cell: String,
}

impl Collector {
    fn new() -> Self {
        Self { blocks: Vec::new(), text: String::new(), heading: false, cells: None, cell: String::new() }
    }

    fn push_text(&mut self, s: &str) {
        if self.cells.is_some() {
            self.cell.push_str(s);
        } else {
            self.text.push_str(s);
        }
    }

    fn end_para(&mut self) {
        if self.cells.is_some() {
            if !self.cell.is_empty() && !self.cell.ends_with(' ') {
                self.cell.push(' ');
            }
            self.heading = false;
            return;
        }
        let text = collapse(&std::mem::take(&mut self.text));
        if !text.is_empty() {
            self.blocks.push(if self.heading { Block::Heading(text) } else { Block::Para(text) });
        }
        self.heading = false;
    }

    fn start_row(&mut self) {
        self.end_para();
        self.cells = Some(Vec::new());
    }

    fn end_cell(&mut self) {
        let cell = collapse(&std::mem::take(&mut self.cell));
        if let Some(cells) = &mut self.cells {
            cells.push(cell);
        }
    }

    fn end_row(&mut self) {
        if let Some(cells) = self.cells.take() {
            if cells.iter().any(|c| !c.is_empty()) {
                self.blocks.push(Block::Row(cells));
            }
        }
    }

    fn finish(mut self) -> Vec<Block> {
        self.end_para();
        self.blocks
    }
}

/// Runs of whitespace become one space.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// --- formats -------------------------------------------------------------------

/// Word: `w:p` paragraphs with `w:t` text; `Heading…`/`Title` styles.
fn word(xml: &str) -> Vec<Block> {
    let mut c = Collector::new();
    let mut in_text = false;
    for t in tokens(xml) {
        match t {
            Xml::Open("t", _) => in_text = true,
            Xml::Close("t") => in_text = false,
            Xml::Text(s) if in_text => c.push_text(&s),
            Xml::Open("tab", _) | Xml::Open("br", _) => c.push_text(" "),
            Xml::Open("pStyle", attrs) => {
                let style = attr(attrs, "w:val").unwrap_or_default().to_ascii_lowercase();
                c.heading = style.starts_with("heading") || style == "title";
            }
            Xml::Close("p") => c.end_para(),
            Xml::Open("tr", _) => c.start_row(),
            Xml::Close("tc") => c.end_cell(),
            Xml::Close("tr") => c.end_row(),
            _ => {}
        }
    }
    c.finish()
}

/// OpenDocument text, presentations and spreadsheets (`content.xml`).
fn open_document(xml: &str) -> Vec<Block> {
    let mut c = Collector::new();
    let mut rows = 0;
    for t in tokens(xml) {
        match t {
            Xml::Text(s) => c.push_text(&s),
            Xml::Open("s", _) | Xml::Open("tab", _) | Xml::Open("line-break", _) => c.push_text(" "),
            Xml::Open("h", _) => {
                c.end_para();
                c.heading = true;
            }
            Xml::Close("h") | Xml::Close("p") => c.end_para(),
            // A slide (`draw:page`) or a sheet (`table:table`) by name.
            Xml::Open("page", attrs) | Xml::Open("table", attrs) => {
                c.end_para();
                rows = 0;
                if let Some(name) = attr(attrs, "draw:name").or_else(|| attr(attrs, "table:name")) {
                    c.blocks.push(Block::Heading(name));
                }
            }
            Xml::Open("table-row", _) => {
                rows += 1;
                c.start_row();
            }
            Xml::Close("table-cell") => c.end_cell(),
            Xml::Close("table-row") => {
                if rows <= MAX_ROWS {
                    c.end_row();
                } else {
                    c.cells = None;
                }
            }
            _ => {}
        }
    }
    c.finish()
}

/// PowerPoint: `ppt/slides/slideN.xml`, in slide order.
fn slides(zip: &mut Zip) -> Result<Vec<Block>> {
    let mut names = numbered(zip, "ppt/slides/slide", ".xml");
    names.truncate(MAX_BLOCKS);
    let mut blocks = Vec::new();
    for (n, name) in names {
        blocks.push(Block::Heading(format!("Slide {n}")));
        let mut c = Collector::new();
        let mut in_text = false;
        for t in tokens(&read(zip, &name)?) {
            match t {
                Xml::Open("t", _) => in_text = true,
                Xml::Close("t") => in_text = false,
                Xml::Text(s) if in_text => c.push_text(&s),
                Xml::Open("br", _) => c.push_text(" "),
                Xml::Close("p") => c.end_para(),
                Xml::Open("tr", _) => c.start_row(),
                Xml::Close("tc") => c.end_cell(),
                Xml::Close("tr") => c.end_row(),
                _ => {}
            }
        }
        blocks.extend(c.finish());
        if blocks.len() > MAX_BLOCKS {
            break;
        }
    }
    Ok(blocks)
}

/// Excel: shared strings, then each `xl/worksheets/sheetN.xml` as rows.
fn sheets(zip: &mut Zip) -> Result<Vec<Block>> {
    let shared: Vec<String> = match read(zip, "xl/sharedStrings.xml") {
        Ok(xml) => {
            let mut strings = Vec::new();
            let (mut current, mut in_text) = (String::new(), false);
            for t in tokens(&xml) {
                match t {
                    Xml::Open("si", _) => current.clear(),
                    Xml::Open("t", _) => in_text = true,
                    Xml::Close("t") => in_text = false,
                    Xml::Text(s) if in_text => current.push_str(&s),
                    Xml::Close("si") => strings.push(std::mem::take(&mut current)),
                    _ => {}
                }
            }
            strings
        }
        Err(_) => Vec::new(),
    };
    let mut blocks = Vec::new();
    for (name, file) in sheet_files(zip).into_iter().take(MAX_SHEETS) {
        blocks.push(Block::Heading(name));
        let mut rows = 0;
        let mut cells: Option<Vec<String>> = None;
        let (mut kind, mut value, mut in_value) = (String::new(), String::new(), false);
        for t in tokens(&read(zip, &file)?) {
            match t {
                Xml::Open("row", _) => cells = Some(Vec::new()),
                Xml::Open("c", attrs) => {
                    kind = attr(attrs, "t").unwrap_or_default();
                    value.clear();
                }
                Xml::Open("v", _) | Xml::Open("t", _) => in_value = true,
                Xml::Close("v") | Xml::Close("t") => in_value = false,
                Xml::Text(s) if in_value => value.push_str(&s),
                Xml::Close("c") => {
                    let shown = match kind.as_str() {
                        "s" => {
                            value.trim().parse::<usize>().ok().and_then(|i| shared.get(i).cloned()).unwrap_or_default()
                        }
                        "b" => if value.trim() == "1" { "TRUE" } else { "FALSE" }.to_string(),
                        _ => value.clone(),
                    };
                    if let Some(cells) = &mut cells {
                        cells.push(collapse(&shown));
                    }
                }
                Xml::Close("row") => {
                    if let Some(cells) = cells.take().filter(|c| c.iter().any(|v| !v.is_empty())) {
                        blocks.push(Block::Row(cells));
                        rows += 1;
                        if rows >= MAX_ROWS {
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(blocks)
}

/// Each sheet's name and file, in tab order. The workbook links names to
/// files by relationship id: `sheet3.xml` need not be the third tab.
fn sheet_files(zip: &mut Zip) -> Vec<(String, String)> {
    let targets: HashMap<String, String> = read(zip, "xl/_rels/workbook.xml.rels")
        .map(|xml| {
            tokens(&xml)
                .into_iter()
                .filter_map(|t| match t {
                    Xml::Open("Relationship", attrs) => {
                        let target = attr(attrs, "Target")?;
                        let file = match target.strip_prefix('/') {
                            Some(absolute) => absolute.to_string(),
                            None => join_zip_path("xl/", &target),
                        };
                        Some((attr(attrs, "Id")?, file))
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    // (name, relationship id) of each tab.
    let tabs: Vec<(String, Option<String>)> = read(zip, "xl/workbook.xml")
        .map(|xml| {
            tokens(&xml)
                .into_iter()
                .filter_map(|t| match t {
                    Xml::Open("sheet", attrs) => Some((attr(attrs, "name")?, attr(attrs, "r:id"))),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let linked: Vec<(String, String)> = tabs
        .iter()
        .filter_map(|(name, id)| Some((name.clone(), targets.get(id.as_ref()?)?.clone())))
        .filter(|(_, file)| zip.index_for_name(file).is_some())
        .collect();
    if !linked.is_empty() {
        return linked;
    }
    // Without relationships, assume the files are numbered in tab order.
    numbered(zip, "xl/worksheets/sheet", ".xml")
        .into_iter()
        .enumerate()
        .map(|(i, (n, file))| (tabs.get(i).map_or_else(|| format!("Sheet {n}"), |(name, _)| name.clone()), file))
        .collect()
}

/// EPUB: title and author from the package file, then the chapters in
/// reading order.
fn epub(zip: &mut Zip) -> Result<Vec<Block>> {
    let container = read(zip, "META-INF/container.xml")?;
    let opf_path = tokens(&container)
        .into_iter()
        .find_map(|t| match t {
            Xml::Open("rootfile", attrs) => attr(attrs, "full-path"),
            _ => None,
        })
        .context("the e-book has no package file")?;
    let opf = read(zip, &opf_path)?;
    let base = opf_path.rsplit_once('/').map(|(dir, _)| format!("{dir}/")).unwrap_or_default();

    let mut blocks = Vec::new();
    let (mut manifest, mut spine) = (HashMap::new(), Vec::new());
    let mut field: Option<&str> = None;
    let mut author = String::new();
    for t in tokens(&opf) {
        match t {
            Xml::Open("title", _) => field = Some("title"),
            Xml::Open("creator", _) => field = Some("creator"),
            Xml::Close("title") | Xml::Close("creator") => field = None,
            Xml::Text(s) if field == Some("title") && blocks.is_empty() => blocks.push(Block::Heading(collapse(&s))),
            Xml::Text(s) if field == Some("creator") && author.is_empty() => author = collapse(&s),
            Xml::Open("item", attrs) => {
                if let (Some(id), Some(href)) = (attr(attrs, "id"), attr(attrs, "href")) {
                    manifest.insert(id, href);
                }
            }
            Xml::Open("itemref", attrs) => spine.extend(attr(attrs, "idref")),
            _ => {}
        }
    }
    if !author.is_empty() {
        blocks.push(Block::Para(format!("by {author}")));
    }
    for id in spine {
        let Some(href) = manifest.get(&id) else { continue };
        let path = join_zip_path(&base, href);
        let Ok(xhtml) = read(zip, &path) else { continue };
        blocks.extend(html(&xhtml));
        if blocks.len() > MAX_BLOCKS {
            break;
        }
    }
    Ok(blocks)
}

/// Text of an XHTML chapter: headings, paragraphs, list items and tables.
fn html(xml: &str) -> Vec<Block> {
    let mut c = Collector::new();
    let mut skip = 0usize;
    for t in tokens(xml) {
        match t {
            Xml::Open("head" | "style" | "script", _) => skip += 1,
            Xml::Close("head" | "style" | "script") => skip = skip.saturating_sub(1),
            _ if skip > 0 => {}
            Xml::Text(s) => c.push_text(&s),
            Xml::Open("br", _) => c.push_text(" "),
            Xml::Open(name, _) if is_heading(name) => {
                c.end_para();
                c.heading = true;
            }
            Xml::Open("p" | "li" | "div" | "blockquote", _) => c.end_para(),
            Xml::Close(name) if is_heading(name) => c.end_para(),
            Xml::Close("p" | "li" | "div" | "blockquote") => c.end_para(),
            Xml::Open("tr", _) => c.start_row(),
            Xml::Close("td" | "th") => c.end_cell(),
            Xml::Close("tr") => c.end_row(),
            _ => {}
        }
    }
    c.finish()
}

fn is_heading(name: &str) -> bool {
    matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

/// Zip entries named `{prefix}N{suffix}`, sorted by `N`.
fn numbered(zip: &Zip, prefix: &str, suffix: &str) -> Vec<(usize, String)> {
    let mut found: Vec<(usize, String)> = zip
        .file_names()
        .filter_map(|name| {
            let n = name.strip_prefix(prefix)?.strip_suffix(suffix)?.parse().ok()?;
            Some((n, name.to_string()))
        })
        .collect();
    found.sort();
    found
}

/// `OEBPS/` + `../Text/ch%201.xhtml#x` → `Text/ch 1.xhtml`.
fn join_zip_path(base: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href);
    let mut parts: Vec<String> = Vec::new();
    for part in base.split('/').chain(href.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(percent_decode(p)),
        }
    }
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            // Bytes, not `&s[..]`: the next two may be part of a multi-byte character.
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, body) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn tokenizes_and_decodes() {
        let t = tokens(r#"<?xml version="1.0"?><!-- c --><w:p a="1"><w:t>A &amp; B&#233;</w:t><w:br/></w:p>"#);
        assert_eq!(
            t,
            [
                Xml::Open("p", r#"a="1""#),
                Xml::Open("t", ""),
                Xml::Text("A & Bé".into()),
                Xml::Close("t"),
                Xml::Open("br", ""),
                Xml::Close("br"),
                Xml::Close("p")
            ]
        );
        assert_eq!(attr(r#"r:id="rId1" id='x'"#, "id"), Some("x".into()));
    }

    #[test]
    fn reads_word_documents() {
        let doc = r#"<w:document><w:body>
            <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Intro</w:t></w:r></w:p>
            <w:p><w:r><w:t xml:space="preserve">Hello </w:t></w:r><w:r><w:t>world</w:t></w:r></w:p>
            <w:tbl><w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
        </w:body></w:document>"#;
        let blocks = extract(zip_of(&[("word/document.xml", doc)]), "docx").unwrap();
        assert_eq!(
            blocks,
            [
                Block::Heading("Intro".into()),
                Block::Para("Hello world".into()),
                Block::Row(vec!["a".into(), "b".into()])
            ]
        );
    }

    #[test]
    fn reads_spreadsheets_with_shared_strings() {
        let shared = "<sst><si><t>Name</t></si><si><t>Ann</t></si></sst>";
        let book = r#"<workbook><sheets><sheet name="People" r:id="rId1"/></sheets></workbook>"#;
        let sheet = r#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>42</v></c></row>
            <row r="2"><c t="s"><v>1</v></c><c t="b"><v>1</v></c></row></sheetData></worksheet>"#;
        let data =
            zip_of(&[("xl/sharedStrings.xml", shared), ("xl/workbook.xml", book), ("xl/worksheets/sheet1.xml", sheet)]);
        assert_eq!(
            extract(data, "xlsx").unwrap(),
            [
                Block::Heading("People".into()),
                Block::Row(vec!["Name".into(), "42".into()]),
                Block::Row(vec!["Ann".into(), "TRUE".into()])
            ]
        );
    }

    #[test]
    fn sheet_names_follow_the_workbook_links() {
        let book = r#"<workbook><sheets><sheet name="Second" r:id="rId2"/><sheet name="First" r:id="rId1"/></sheets></workbook>"#;
        let rels = r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/>
            <Relationship Id="rId2" Target="/xl/worksheets/sheet2.xml"/></Relationships>"#;
        let sheet = |v: &str| {
            format!(
                r#"<worksheet><sheetData><row><c t="inlineStr"><is><t>{v}</t></is></c></row></sheetData></worksheet>"#
            )
        };
        let (one, two) = (sheet("in sheet1"), sheet("in sheet2"));
        let data = zip_of(&[
            ("xl/workbook.xml", book),
            ("xl/_rels/workbook.xml.rels", rels),
            ("xl/worksheets/sheet1.xml", &one),
            ("xl/worksheets/sheet2.xml", &two),
        ]);
        assert_eq!(
            extract(data, "xlsx").unwrap(),
            [
                Block::Heading("Second".into()),
                Block::Row(vec!["in sheet2".into()]),
                Block::Heading("First".into()),
                Block::Row(vec!["in sheet1".into()])
            ]
        );
    }

    #[test]
    fn malformed_text_does_not_panic() {
        assert_eq!(tokens("<a><![CDATA[éé"), [Xml::Open("a", ""), Xml::Text("éé".into())]);
        assert_eq!(percent_decode("ch%aé.xhtml"), "ch%aé.xhtml");
        assert_eq!(percent_decode("a%20b"), "a b");
    }

    #[test]
    fn headings_in_table_cells_stay_there() {
        let doc = r#"<w:document><w:body><w:tbl><w:tr><w:tc>
            <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>cell</w:t></w:r></w:p>
            </w:tc></w:tr></w:tbl><w:p><w:r><w:t>after</w:t></w:r></w:p></w:body></w:document>"#;
        assert_eq!(word(doc), [Block::Row(vec!["cell".into()]), Block::Para("after".into())]);
    }

    #[test]
    fn reads_slides_in_order() {
        let slide = |t: &str| format!("<p:sld><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>");
        let data = zip_of(&[("ppt/slides/slide10.xml", &slide("ten")), ("ppt/slides/slide2.xml", &slide("two"))]);
        assert_eq!(
            extract(data, "pptx").unwrap(),
            [
                Block::Heading("Slide 2".into()),
                Block::Para("two".into()),
                Block::Heading("Slide 10".into()),
                Block::Para("ten".into())
            ]
        );
    }

    #[test]
    fn reads_open_document_text() {
        let content = r#"<office:document-content><office:body><office:text>
            <text:h text:outline-level="1">Title</text:h><text:p>One<text:s/>two</text:p>
        </office:text></office:body></office:document-content>"#;
        let blocks = extract(zip_of(&[("content.xml", content)]), "odt").unwrap();
        assert_eq!(blocks, [Block::Heading("Title".into()), Block::Para("One two".into())]);
    }

    #[test]
    fn reads_epub_chapters_in_spine_order() {
        let container = r#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#;
        let opf = r#"<package><metadata><dc:title>My Book</dc:title><dc:creator>Ann</dc:creator></metadata>
            <manifest><item id="c2" href="Text/two.xhtml"/><item id="c1" href="Text/one%20a.xhtml"/></manifest>
            <spine><itemref idref="c1"/><itemref idref="c2"/></spine></package>"#;
        let ch = |h: &str, p: &str| {
            format!("<html><head><style>p{{}}</style></head><body><h1>{h}</h1><p>{p}</p></body></html>")
        };
        let data = zip_of(&[
            ("META-INF/container.xml", container),
            ("OEBPS/content.opf", opf),
            ("OEBPS/Text/one a.xhtml", &ch("One", "first")),
            ("OEBPS/Text/two.xhtml", &ch("Two", "second")),
        ]);
        assert_eq!(
            extract(data, "epub").unwrap(),
            [
                Block::Heading("My Book".into()),
                Block::Para("by Ann".into()),
                Block::Heading("One".into()),
                Block::Para("first".into()),
                Block::Heading("Two".into()),
                Block::Para("second".into())
            ]
        );
    }

    #[test]
    fn joins_relative_zip_paths() {
        assert_eq!(join_zip_path("OEBPS/Text/", "../Images/a%20b.png#x"), "OEBPS/Images/a b.png");
    }
}
