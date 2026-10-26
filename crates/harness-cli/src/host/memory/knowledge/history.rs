//! A knowledge library's page history (`docs/architecture/MEMORY_STORE.en.md`
//! §7): every write and delete of a page through the store, with the whole
//! page as it then was, so `history` shows a page's versions and `revert`
//! brings one back - a deleted page too. `log.md` keeps only hashes.
//!
//! The events live in `.ha-history.sqlite3` beside the pages. Like the memory
//! store's log they are kept 30 days, except each page's ten newest.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Value, json};

const FILE: &str = ".ha-history.sqlite3";
const RETAIN_DAYS: i64 = 30;
const RETAIN_VERSIONS: i64 = 10;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS page_event (
  event_id     TEXT PRIMARY KEY,
  at           TEXT NOT NULL,
  path         TEXT NOT NULL,
  operation    TEXT NOT NULL,
  operation_id TEXT NOT NULL,
  cause        TEXT,
  actor        TEXT NOT NULL,
  hash         TEXT NOT NULL,
  title        TEXT,
  text         TEXT
);
CREATE INDEX IF NOT EXISTS page_event_path ON page_event(path, at);
CREATE INDEX IF NOT EXISTS page_event_at ON page_event(at);
";

/// The history file of a library root.
#[must_use]
pub(crate) fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// One change of a page to record.
pub(crate) struct Change<'a> {
    pub path: &'a str,
    /// `insert`, `update` or `delete`.
    pub operation: &'a str,
    /// `write`, `delete`, `revert`, or what wrote the page (`review`).
    pub cause: &'a str,
    pub actor: &'a str,
    /// The page's hash after the change, or the removed page's.
    pub hash: &'a str,
    pub title: &'a str,
    /// The whole page after the change, or the page a delete removed.
    pub text: &'a str,
    /// The page as it was before, recorded first when the page has no
    /// history yet: a page written before the history began can still be
    /// reverted to.
    pub before: Option<(&'a str, &'a str)>,
}

/// One recorded version of a page.
#[derive(Clone, Debug)]
pub(crate) struct Event {
    pub id: String,
    pub at: String,
    pub path: String,
    pub operation: String,
    pub cause: Option<String>,
    pub actor: String,
    pub hash: String,
    pub title: Option<String>,
    pub text: Option<String>,
}

impl Event {
    /// The event as `history` shows it, without the page's text.
    pub(crate) fn view(&self) -> Value {
        json!({
            "version": self.id,
            "at": self.at,
            "path": self.path,
            "operation": self.operation,
            "cause": self.cause,
            "by": self.actor,
            "hash": self.hash,
            "title": self.title,
        })
    }
}

fn open(root: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path(root)).map_err(sql)?;
    conn.busy_timeout(Duration::from_secs(5)).map_err(sql)?;
    conn.execute_batch(SCHEMA).map_err(sql)?;
    Ok(conn)
}

fn existing(root: &Path) -> Result<Option<Connection>, String> {
    if path(root).is_file() {
        open(root).map(Some)
    } else {
        Ok(None)
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Record a change of a page; the caller holds the library's lock.
///
/// # Errors
/// The history cannot be written.
pub(crate) fn record(root: &Path, change: &Change<'_>) -> Result<(), String> {
    let mut conn = open(root)?;
    let transaction = conn.transaction().map_err(sql)?;
    let operation = uuid::Uuid::now_v7().to_string();
    let known = transaction
        .query_row(
            "SELECT 1 FROM page_event WHERE path = ? LIMIT 1",
            [change.path],
            |_| Ok(()),
        )
        .optional()
        .map_err(sql)?
        .is_some();
    if !known && let Some((hash, text)) = change.before {
        insert(
            &transaction,
            change.path,
            "insert",
            &operation,
            "baseline",
            "unknown",
            hash,
            None,
            text,
        )?;
    }
    insert(
        &transaction,
        change.path,
        change.operation,
        &operation,
        change.cause,
        change.actor,
        change.hash,
        Some(change.title),
        change.text,
    )?;
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(RETAIN_DAYS))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    transaction
        .execute(
            "DELETE FROM page_event WHERE at < ?1 AND event_id NOT IN (
               SELECT event_id FROM (
                 SELECT event_id, row_number() OVER (PARTITION BY path ORDER BY at DESC, event_id DESC) AS newest
                 FROM page_event)
               WHERE newest <= ?2)",
            params![cutoff, RETAIN_VERSIONS],
        )
        .map_err(sql)?;
    transaction.commit().map_err(sql)
}

#[allow(clippy::too_many_arguments, reason = "the columns of one event")]
fn insert(
    transaction: &rusqlite::Transaction<'_>,
    path: &str,
    operation: &str,
    operation_id: &str,
    cause: &str,
    actor: &str,
    hash: &str,
    title: Option<&str>,
    text: &str,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO page_event(event_id, at, path, operation, operation_id, cause, actor, hash, title, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                uuid::Uuid::now_v7().to_string(),
                now(),
                path,
                operation,
                operation_id,
                cause,
                actor,
                hash,
                title,
                text
            ],
        )
        .map_err(sql)?;
    Ok(())
}

/// The versions recorded of a page, or of every page under a folder
/// (`path` ending in `/`), or of the whole library, newest first.
///
/// # Errors
/// The history cannot be read.
pub(crate) fn events(
    root: &Path,
    path: Option<&str>,
    since: Option<&str>,
    limit: usize,
) -> Result<Vec<Event>, String> {
    let Some(conn) = existing(root)? else {
        return Ok(Vec::new());
    };
    let mut clauses = vec!["1 = 1".to_owned()];
    let mut values = Vec::new();
    match path {
        Some(folder) if folder.ends_with('/') => {
            clauses.push("substr(path, 1, ?) = ?".to_owned());
            values.push(folder.chars().count().to_string());
            values.push(folder.to_owned());
        }
        Some(page) => {
            clauses.push("path = ?".to_owned());
            values.push(page.to_owned());
        }
        None => {}
    }
    if let Some(since) = since {
        clauses.push("at >= ?".to_owned());
        values.push(since.to_owned());
    }
    let query = format!(
        "SELECT event_id, at, path, operation, cause, actor, hash, title, text FROM page_event
         WHERE {} ORDER BY at DESC, event_id DESC LIMIT {}",
        clauses.join(" AND "),
        limit.max(1)
    );
    let mut statement = conn.prepare(&query).map_err(sql)?;
    let events = statement
        .query_map(rusqlite::params_from_iter(values.iter()), row)
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    Ok(events)
}

/// One recorded version, by its id.
///
/// # Errors
/// The history cannot be read.
pub(crate) fn event(root: &Path, event_id: &str) -> Result<Option<Event>, String> {
    let Some(conn) = existing(root)? else {
        return Ok(None);
    };
    conn.query_row(
        "SELECT event_id, at, path, operation, cause, actor, hash, title, text FROM page_event
         WHERE event_id = ?",
        [event_id],
        row,
    )
    .optional()
    .map_err(sql)
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: row.get(0)?,
        at: row.get(1)?,
        path: row.get(2)?,
        operation: row.get(3)?,
        cause: row.get(4)?,
        actor: row.get(5)?,
        hash: row.get(6)?,
        title: row.get(7)?,
        text: row.get(8)?,
    })
}

#[allow(clippy::needless_pass_by_value, reason = "used as a map_err adapter")]
fn sql(error: rusqlite::Error) -> String {
    format!("knowledge history: {error}")
}
