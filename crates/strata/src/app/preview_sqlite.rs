//! Tables and views of SQLite databases for the preview, with row counts
//! and columns. Databases are opened read-only, wait at most a moment for a
//! writer's lock, and give up on queries that take too long, so a big or
//! busy database never stalls the preview.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use rusqlite::{Connection, OpenFlags};
use strata_config::Theme;

use super::preview_data::{fit, group, Data};

/// The first bytes of every SQLite 3 database.
pub const MAGIC: &[u8] = b"SQLite format 3\0";
/// Rows counted per table before showing `100,000+`.
const COUNT_LIMIT: u64 = 100_000;
/// Tables and views listed.
const MAX_OBJECTS: usize = 200;
/// Time for the whole preview, and for a writer to release its lock.
const BUDGET: Duration = Duration::from_secs(2);
const BUSY: Duration = Duration::from_millis(250);

#[derive(Debug, PartialEq, Eq)]
pub struct Object {
    pub name: String,
    pub view: bool,
    /// Rows, `None` when not counted (views, or out of time).
    pub rows: Option<u64>,
    pub columns: Vec<Column>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub kind: String,
    pub primary: bool,
    pub not_null: bool,
}

/// Reads the schema of the database at `path`.
pub fn inspect(path: &Path) -> Result<Vec<Object>> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let db = Connection::open_with_flags(path, flags).context("cannot open the database")?;
    db.busy_timeout(BUSY)?;
    let start = Instant::now();
    // Interrupts whatever runs past the budget.
    db.progress_handler(10_000, Some(move || start.elapsed() > BUDGET))?;

    let mut stmt = db
        .prepare(
            "SELECT name, type = 'view' FROM sqlite_schema \
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .context("cannot read the schema")?;
    let names: Vec<(String, bool)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .take(MAX_OBJECTS)
        .collect::<rusqlite::Result<_>>()
        .context("cannot read the schema")?;

    let mut objects = Vec::new();
    for (name, view) in names {
        let mut stmt = db.prepare("SELECT name, type, pk, \"notnull\" FROM pragma_table_info(?1)")?;
        // Virtual tables whose module is missing can't be described.
        let columns = stmt
            .query_map([&name], |r| {
                Ok(Column { name: r.get(0)?, kind: r.get(1)?, primary: r.get::<_, i64>(2)? > 0, not_null: r.get(3)? })
            })
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .unwrap_or_default();
        // Views run their query to be counted: skip them.
        let rows = (!view && start.elapsed() < BUDGET).then(|| count(&db, &name)).flatten();
        objects.push(Object { name, view, rows, columns });
    }
    Ok(objects)
}

/// Rows in a table, up to just past [`COUNT_LIMIT`].
fn count(db: &Connection, table: &str) -> Option<u64> {
    let quoted = format!("\"{}\"", table.replace('"', "\"\""));
    let sql = format!("SELECT count(*) FROM (SELECT 1 FROM {quoted} LIMIT {})", COUNT_LIMIT + 1);
    db.query_row(&sql, [], |r| r.get::<_, i64>(0)).ok().map(|n| n as u64)
}

/// The schema as lines: each table with its row count, then its columns.
pub fn render(objects: &[Object], file_size: u64, theme: &Theme) -> Data {
    let muted = Style::default().fg(theme.muted);
    let tables = objects.iter().filter(|o| !o.view).count();
    let views = objects.len() - tables;
    let mut summary = format!(" SQLite · {tables} table{}", if tables == 1 { "" } else { "s" });
    if views > 0 {
        summary.push_str(&format!(" · {views} view{}", if views == 1 { "" } else { "s" }));
    }
    summary.push_str(&format!(" · {}", strata_core::util::human_size(file_size)));
    let head = vec![Line::styled(summary, muted)];

    let mut body = Vec::new();
    for object in objects {
        if !body.is_empty() {
            body.push(Line::default());
        }
        let detail = match (object.view, object.rows) {
            (true, _) => "view".to_string(),
            (false, Some(n)) if n > COUNT_LIMIT => format!("{}+ rows", group(COUNT_LIMIT)),
            (false, Some(1)) => "1 row".into(),
            (false, Some(n)) => format!("{} rows", group(n)),
            (false, None) => "rows not counted".into(),
        };
        body.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(object.name.clone(), Style::default().fg(theme.palette.accent).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {detail}"), muted),
        ]));
        let name_w = object.columns.iter().map(|c| c.name.chars().count()).max().unwrap_or(0).min(28);
        let kind_w = object.columns.iter().map(|c| c.kind.chars().count()).max().unwrap_or(0).min(16);
        for column in &object.columns {
            let mut flags = Vec::new();
            if column.primary {
                flags.push("primary key");
            }
            if column.not_null {
                flags.push("not null");
            }
            let mut spans = vec![
                Span::raw("   "),
                Span::styled(fit(&column.name, name_w, false), Style::default().fg(theme.fg)),
                Span::raw("  "),
                Span::styled(fit(&column.kind, kind_w, false), Style::default().fg(theme.palette.yellow)),
                Span::styled(format!("  {}", flags.join(", ")), muted),
            ];
            // No padding after the last thing on the line.
            while spans.last().is_some_and(|s| s.content.trim().is_empty()) {
                spans.pop();
            }
            if let Some(last) = spans.last_mut() {
                last.content = last.content.trim_end().to_string().into();
            }
            body.push(Line::from(spans));
        }
    }
    if objects.is_empty() {
        body.push(Line::styled(" no tables", muted));
    }
    Data { head, body }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn lists_tables_views_and_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \"odd \"\"name\" BLOB);
             INSERT INTO users (name) VALUES ('a'), ('b'), ('c');
             CREATE TABLE \"empty one\" (x);
             CREATE VIEW names AS SELECT name FROM users;",
        )
        .unwrap();
        drop(db);
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(MAGIC));

        let objects = inspect(&path).unwrap();
        let summary: Vec<(&str, bool, Option<u64>)> =
            objects.iter().map(|o| (o.name.as_str(), o.view, o.rows)).collect();
        assert_eq!(summary, [("empty one", false, Some(0)), ("users", false, Some(3)), ("names", true, None)]);
        let users = &objects[1];
        assert_eq!(users.columns.len(), 3);
        assert_eq!(
            users.columns[0],
            Column { name: "id".into(), kind: "INTEGER".into(), primary: true, not_null: false }
        );
        assert!(users.columns[1].not_null);

        let theme = strata_config::theme::builtin().into_iter().next().unwrap();
        let data = render(&objects, bytes.len() as u64, &theme);
        assert!(text(&data.head[0]).starts_with(" SQLite · 2 tables · 1 view · "));
        let lines: Vec<String> = data.body.iter().map(text).collect();
        assert!(lines.contains(&" users  3 rows".to_string()), "{lines:?}");
        assert!(lines.contains(&"   id         INTEGER  primary key".to_string()), "{lines:?}");
        assert!(lines.contains(&"   x".to_string()), "{lines:?}");
        // Opening read-only left the database as it was.
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn rejects_files_that_are_not_databases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.db");
        std::fs::write(&path, b"SQLite format 3\0 but then garbage").unwrap();
        assert!(inspect(&path).is_err());
    }
}
