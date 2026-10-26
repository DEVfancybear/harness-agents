//! The memory store (`docs/architecture/MEMORY_STORE.en.md`).
//!
//! One `SQLite` database per scope directory - the global one and each
//! conversation's - holds what `harness_state.json` held. Every entry is a row
//! with memory-engine's record fields: an immutable id, a tree, a name, meta,
//! temporal, a version and its hash. Every change is an event in an append-only
//! log that says who made it and why.
//!
//! The writers that read the whole state, change it and save it - the `memory`
//! tool, `/refine` and the kernel's `rlm.harness` - keep doing so through
//! [`load_document`] and [`save_document`]. A save is compared with the rows
//! and becomes one transaction, each changed entry one event, all sharing one
//! operation id. A `harness_state.json` found beside a store is imported into
//! it and renamed: that is how an upgrade migrates, and how a test or a person
//! drops entries in.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

/// The database a scope directory holds.
pub const DB_FILE: &str = "memory.sqlite3";
/// The lock a read-modify-write of a scope holds.
pub const LOCK_FILE: &str = "memory.lock";
/// The state file the store replaced; one found beside a store is imported.
pub const LEGACY_STATE: &str = "harness_state.json";
/// The journal of earlier versions the event log replaced.
pub const LEGACY_JOURNAL: &str = "memory_history.jsonl";
const SCHEMA_VERSION: i32 = 1;
/// Events older than this are dropped...
const RETAIN_DAYS: i64 = 30;
/// ...except each memory's newest ones, so a memory that has not changed for a
/// month can still be reverted.
const RETAIN_VERSIONS: i64 = 10;
const PRUNE_EVERY_SECONDS: i64 = 3_600;
/// The kinds every document has, empty or not.
const KINDS: [&str; 4] = ["prompt", "memory", "skill", "subagent"];
const TEMPORAL_SHAPE: &str =
    "temporal is a time (the start), a list [start] or [start, end], or {start, end}";

const SCHEMA_V1: &str = "
CREATE TABLE memory (
  id              TEXT PRIMARY KEY,
  kind            TEXT NOT NULL,
  name            TEXT NOT NULL,
  tree            TEXT NOT NULL,
  title           TEXT NOT NULL,
  content         TEXT NOT NULL,
  meta            TEXT NOT NULL DEFAULT '{}',
  temporal_start  TEXT,
  temporal_end    TEXT,
  doc             TEXT NOT NULL,
  version         INTEGER NOT NULL,
  version_hash    TEXT NOT NULL,
  content_version INTEGER NOT NULL,
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL,
  UNIQUE (kind, name)
);
CREATE INDEX memory_tree ON memory(tree);
CREATE INDEX memory_temporal ON memory(temporal_start, temporal_end);
CREATE TABLE memory_event (
  event_id     TEXT PRIMARY KEY,
  at           TEXT NOT NULL,
  memory_id    TEXT NOT NULL,
  kind         TEXT NOT NULL,
  name         TEXT NOT NULL,
  tree         TEXT NOT NULL,
  operation    TEXT NOT NULL,
  operation_id TEXT NOT NULL,
  cause        TEXT,
  actor        TEXT NOT NULL,
  version      INTEGER NOT NULL,
  doc          TEXT NOT NULL
);
CREATE INDEX memory_event_memory ON memory_event(memory_id, at, event_id);
CREATE INDEX memory_event_operation ON memory_event(operation_id);
CREATE INDEX memory_event_tree ON memory_event(tree);
CREATE INDEX memory_event_path ON memory_event(tree, name);
CREATE INDEX memory_event_at ON memory_event(at DESC);
CREATE TABLE refinement (seq INTEGER PRIMARY KEY, event TEXT NOT NULL);
CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// Who changes the store and why; every event records it.
#[derive(Clone, Debug)]
pub struct Writer {
    actor: Value,
    cause: String,
    /// The operation its saves belong to: one id for everything one call
    /// changes, even across stores.
    operation: String,
    /// Entries that are a memory coming back or moving in from another
    /// store: `(kind, name)` keeps that memory's id.
    keep: BTreeMap<(String, String), String>,
}

impl Writer {
    /// `who` is the kind of writer (`model`, `refine`, `kernel`, `cli`,
    /// `migration`...), `origin` what the entry's own `origin` says about it,
    /// and `cause` what the write is for (`create`, `update`, `delete`,
    /// `revert`, `refine`...).
    #[must_use]
    pub fn new(who: &str, origin: &Value, cause: &str) -> Self {
        let mut actor = origin.as_object().cloned().unwrap_or_default();
        actor.insert("type".to_owned(), json!(who));
        Self {
            actor: Value::Object(actor),
            cause: cause.to_owned(),
            operation: new_id(),
            keep: BTreeMap::new(),
        }
    }

    /// The same writer bringing a deleted memory back, or moving one in from
    /// another store: the entry it saves as `kind:name` is that memory, under
    /// its id, so its history goes on.
    #[must_use]
    pub fn reviving(mut self, kind: &str, name: &str, memory: &str) -> Self {
        self.keep
            .insert((kind.to_owned(), name.to_owned()), memory.to_owned());
        self
    }

    /// The operation id every event this writer records shares.
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }
}

/// The id of each live memory of a store, by `(kind, name)`.
///
/// # Errors
/// The store cannot be read.
pub fn ids(dir: &Path) -> Result<BTreeMap<(String, String), String>, String> {
    let Some(store) = Store::existing(dir)? else {
        return Ok(BTreeMap::new());
    };
    let mut statement = store
        .conn
        .prepare("SELECT kind, name, id FROM memory")
        .map_err(sql)?;
    let ids = statement
        .query_map([], |row| {
            Ok((
                (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(sql)?
        .collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(sql)?;
    Ok(ids)
}

/// Which events a history feed reads: at least one of `memory`, `tree`,
/// `operation_id` or `since`, so a read is never an unbounded scan.
#[derive(Clone, Debug, Default)]
pub struct Feed {
    pub memory: Option<String>,
    /// A subtree: events at or under this tree.
    pub tree: Option<String>,
    pub operation_id: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    /// `insert`, `update` or `delete`.
    pub operation: Option<String>,
    /// Newest first unless set.
    pub ascending: bool,
    /// Past this `(at, event_id)`, in the feed's order.
    pub after: Option<(String, String)>,
    pub limit: usize,
}

/// One event of a feed.
#[derive(Clone, Debug)]
pub struct FeedEvent {
    pub event_id: String,
    pub at: String,
    pub memory: String,
    pub kind: String,
    pub name: String,
    pub tree: String,
    pub operation: String,
    pub operation_id: String,
    pub cause: Option<String>,
    pub actor: Value,
    pub version: i64,
    pub doc: Value,
}

/// The events of a store a feed selects, in its order, at most its limit.
///
/// # Errors
/// The feed has no scope, or the store cannot be read.
pub fn feed(dir: &Path, feed: &Feed) -> Result<Vec<FeedEvent>, String> {
    if feed.memory.is_none()
        && feed.tree.is_none()
        && feed.operation_id.is_none()
        && feed.since.is_none()
    {
        return Err(
            "a history feed needs a scope: an id, a tree, an operation_id or since".to_owned(),
        );
    }
    let Some(store) = Store::existing(dir)? else {
        return Ok(Vec::new());
    };
    let mut clauses = Vec::new();
    let mut values: Vec<String> = Vec::new();
    if let Some(memory) = &feed.memory {
        clauses.push("memory_id = ?".to_owned());
        values.push(memory.clone());
    }
    if let Some(tree) = &feed.tree {
        clauses.push("(tree = ? OR substr(tree, 1, ?) = ?)".to_owned());
        values.push(tree.clone());
        let prefix = format!("{}/", tree.trim_end_matches('/'));
        values.push(prefix.chars().count().to_string());
        values.push(prefix);
    }
    if let Some(operation) = &feed.operation_id {
        clauses.push("operation_id = ?".to_owned());
        values.push(operation.clone());
    }
    if let Some(since) = &feed.since {
        clauses.push("at >= ?".to_owned());
        values.push(instant(since)?);
    }
    if let Some(until) = &feed.until {
        clauses.push("at < ?".to_owned());
        values.push(instant(until)?);
    }
    if let Some(operation) = &feed.operation {
        clauses.push("operation = ?".to_owned());
        values.push(operation.clone());
    }
    let order = if feed.ascending { "ASC" } else { "DESC" };
    if let Some((at, event)) = &feed.after {
        clauses.push(format!(
            "(at, event_id) {} (?, ?)",
            if feed.ascending { ">" } else { "<" }
        ));
        values.push(at.clone());
        values.push(event.clone());
    }
    let query = format!(
        "SELECT event_id, at, memory_id, kind, name, tree, operation, operation_id, cause, actor,
         version, doc FROM memory_event WHERE {} ORDER BY at {order}, event_id {order} LIMIT {}",
        clauses.join(" AND "),
        feed.limit.max(1)
    );
    let mut statement = store.conn.prepare(&query).map_err(sql)?;
    let events = statement
        .query_map(rusqlite::params_from_iter(values.iter()), |row| {
            Ok(FeedEvent {
                event_id: row.get(0)?,
                at: row.get(1)?,
                memory: row.get(2)?,
                kind: row.get(3)?,
                name: row.get(4)?,
                tree: row.get(5)?,
                operation: row.get(6)?,
                operation_id: row.get(7)?,
                cause: row.get(8)?,
                actor: parse(&row.get::<_, String>(9)?),
                version: row.get(10)?,
                doc: parse(&row.get::<_, String>(11)?),
            })
        })
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    Ok(events)
}

/// memory-engine's `version_hash` of an entry: 32 hex characters that change
/// whenever its tree, name, title, content, meta or temporal value does.
#[must_use]
pub fn version_hash(root: &str, kind: &str, name: &str, doc: &Value) -> String {
    Fields::of(root, kind, name, doc).hash
}

/// The database of a scope directory.
#[must_use]
pub fn db_path(dir: &Path) -> PathBuf {
    dir.join(DB_FILE)
}

/// The tree a store's records hang from: a conversation's store is
/// `sessions/<conversation>/harness`; any other is the global one.
#[must_use]
pub fn root_of(dir: &Path) -> &'static str {
    let local = dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .is_some_and(|name| name == "sessions");
    if local { "/local" } else { "/global" }
}

/// A path segment as a tree label: accents folded, and every character
/// outside `[A-Za-z0-9_-]` an underscore.
#[must_use]
pub fn label(segment: &str) -> String {
    super::knowledge::store::fold(segment.trim())
        .chars()
        .filter(|character| !unicode_normalization::char::is_combining_mark(*character))
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// Where an entry sits: its scope's root, then its path as labels, or
/// `general` when it has none. A global entry kept for one project sits under
/// `/projects/<key>`.
#[must_use]
pub fn tree_for(root: &str, doc: &Value) -> String {
    let mut tree = match super::harness::project_key(doc.get("project")) {
        Some(key) if root == "/global" => format!("/projects/{}", label(&key)),
        _ => root.to_owned(),
    };
    let path = doc
        .get("path")
        .and_then(Value::as_str)
        .or_else(|| doc.get("topic").and_then(Value::as_str))
        .unwrap_or_default();
    let labels = path
        .split('/')
        .map(label)
        .filter(|label| label.chars().any(|character| character != '_'))
        .collect::<Vec<_>>();
    if labels.is_empty() {
        tree.push_str("/general");
    }
    for label in labels {
        tree.push('/');
        tree.push_str(&label);
    }
    tree
}

/// A time as the store keeps it: UTC, RFC 3339 with milliseconds, so stored
/// times compare as text.
fn stamp(time: chrono::DateTime<chrono::Utc>) -> String {
    time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The time now, as the store keeps it.
#[must_use]
pub fn now() -> String {
    stamp(chrono::Utc::now())
}

/// A time given as RFC 3339, a date (midnight UTC), or ha's own `@<seconds>`.
///
/// # Errors
/// None of those.
pub fn instant(text: &str) -> Result<String, String> {
    let text = text.trim();
    if let Some(seconds) = text.strip_prefix('@')
        && let Some(time) = seconds
            .parse::<i64>()
            .ok()
            .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
    {
        return Ok(stamp(time));
    }
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(text) {
        return Ok(stamp(time.with_timezone(&chrono::Utc)));
    }
    if let Some(midnight) = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
    {
        return Ok(stamp(midnight.and_utc()));
    }
    Err(format!(
        "{text:?} is not a time such as 2026-10-26T09:30:00Z or a date such as 2026-10-26"
    ))
}

/// A temporal value as memory-engine takes it - a string (the start), a list
/// of one or two strings, or `{start, end?}` - as its two bounds in UTC. A
/// point has `end == start`; a period has an exclusive end after its start.
///
/// # Errors
/// Not one of those shapes, a time that does not parse, or an end before its
/// start.
pub fn temporal_bounds(value: &Value) -> Result<Option<(String, String)>, String> {
    let (start, end) = match value {
        Value::Null => return Ok(None),
        Value::String(start) => (start.as_str(), None),
        Value::Array(items) => match items.as_slice() {
            [Value::String(start)] => (start.as_str(), None),
            [Value::String(start), Value::String(end)] => (start.as_str(), Some(end.as_str())),
            _ => return Err(TEMPORAL_SHAPE.to_owned()),
        },
        Value::Object(fields) => {
            let start = fields
                .get("start")
                .and_then(Value::as_str)
                .ok_or(TEMPORAL_SHAPE)?;
            let end = match fields.get("end") {
                None | Some(Value::Null) => None,
                Some(Value::String(end)) => Some(end.as_str()),
                Some(_) => return Err(TEMPORAL_SHAPE.to_owned()),
            };
            (start, end)
        }
        _ => return Err(TEMPORAL_SHAPE.to_owned()),
    };
    let start = instant(start)?;
    let end = match end {
        Some(end) => instant(end)?,
        None => start.clone(),
    };
    if end < start {
        return Err(format!("temporal ends ({end}) before it starts ({start})"));
    }
    Ok(Some((start, end)))
}

/// The temporal value an entry keeps: `{start}` for a point, `{start, end}`
/// for a period.
#[must_use]
pub fn temporal_value(bounds: &(String, String)) -> Value {
    if bounds.0 == bounds.1 {
        json!({"start": bounds.0})
    } else {
        json!({"start": bounds.0, "end": bounds.1})
    }
}

/// Whether an entry's period is over: it says what was true until then, so
/// the digest no longer shows it as current. A point in time never ends.
#[must_use]
pub fn ended(doc: &Value) -> bool {
    matches!(
        temporal_bounds(doc.get("temporal").unwrap_or(&Value::Null)),
        Ok(Some((start, end))) if end > start && end <= now()
    )
}

/// An empty document.
#[must_use]
pub fn empty_document() -> Value {
    json!({"schema": 1, "entries": {"prompt": {}, "memory": {}, "skill": {}, "subagent": {}}, "refinements": []})
}

/// The whole state of a scope as one document, `harness_state.json`'s shape:
/// `{"schema", "entries": {kind: {name: entry}}, "refinements"}`. A scope with
/// no store reads as empty and is not created.
///
/// # Errors
/// The store cannot be opened or read.
pub fn load_document(dir: &Path) -> Result<Value, String> {
    match Store::existing(dir)? {
        Some(store) => document(&store.conn),
        None => Ok(empty_document()),
    }
}

/// Save a whole document over a scope: each entry that differs from its row
/// is inserted, updated or deleted, with one event each, in one transaction.
/// An entry that names a removed one among its `aliases` is that entry
/// renamed, and keeps its id. Returns how many entries changed. The caller
/// holds the scope's lock.
///
/// # Errors
/// The store cannot be opened or written; nothing is then changed.
pub fn save_document(dir: &Path, state: &Value, writer: &Writer) -> Result<usize, String> {
    let mut store = Store::open(dir)?;
    let root = store.root;
    let transaction = store.begin()?;
    let changed = save(&transaction, root, state, writer)?;
    transaction.commit().map_err(sql)?;
    Ok(changed)
}

/// One earlier version of an entry, as `history` shows it and `restore`
/// brings back.
#[derive(Clone, Debug)]
pub struct Earlier {
    /// The memory's immutable id.
    pub memory: String,
    /// The entry's name when it was this version.
    pub name: String,
    pub version: i64,
    pub doc: Value,
    /// When a later version or a delete replaced it.
    pub replaced_at: Value,
    /// Who replaced it.
    pub by: Value,
    /// A delete replaced it.
    pub deleted: bool,
}

/// An entry's earlier versions, newest first: those of the live entry `id`
/// names (by name or by an alias), else those of the entry last deleted under
/// that name.
///
/// # Errors
/// The store cannot be read.
pub fn earlier_versions(dir: &Path, kind: &str, id: &str) -> Result<Vec<Earlier>, String> {
    let Some(store) = Store::existing(dir)? else {
        return Ok(Vec::new());
    };
    let Some((memory, live)) = resolve(&store.conn, kind, id)? else {
        return Ok(Vec::new());
    };
    let mut statement = store
        .conn
        .prepare(
            "SELECT name, at, operation, actor, version, doc FROM memory_event
             WHERE memory_id = ? ORDER BY at, rowid",
        )
        .map_err(sql)?;
    let events = statement
        .query_map([&memory], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?
        .into_iter()
        .map(|(name, at, operation, actor, version, doc)| Event {
            name,
            at,
            deleted: operation == "delete",
            actor: parse(&actor),
            version,
            doc: parse(&doc),
        })
        .collect::<Vec<_>>();
    let mut earlier = versions(&events, live);
    for version in &mut earlier {
        version.memory.clone_from(&memory);
    }
    Ok(earlier)
}

/// One event of the log, as history reads it.
struct Event {
    name: String,
    at: String,
    deleted: bool,
    actor: Value,
    version: i64,
    doc: Value,
}

/// The versions a memory's events show, newest first. Each version is
/// replaced by the event after it. An event migrated from the old journal
/// says itself when and by whom it was replaced. A delete right after the
/// version it removed is that version's end, not one of its own. The live
/// entry's newest event is its current state, not an earlier one.
fn versions(events: &[Event], live: bool) -> Vec<Earlier> {
    let mut earlier = Vec::new();
    for (index, event) in events.iter().enumerate() {
        let next = events.get(index + 1);
        if event.deleted {
            let shown =
                index > 0 && !events[index - 1].deleted && events[index - 1].doc == event.doc;
            if !shown {
                earlier.push(Earlier {
                    memory: String::new(),
                    name: event.name.clone(),
                    version: event.version,
                    doc: event.doc.clone(),
                    replaced_at: json!(event.at),
                    by: event.actor.clone(),
                    deleted: true,
                });
            }
            continue;
        }
        let replaced_at = event.actor.get("replaced_at").cloned();
        let by = event.actor.get("replaced_by").cloned();
        let (replaced_at, by, deleted) = match (next, replaced_at) {
            (Some(next), replaced_at) => (
                replaced_at.unwrap_or_else(|| json!(next.at)),
                by.unwrap_or_else(|| next.actor.clone()),
                next.deleted && next.doc == event.doc,
            ),
            (None, Some(replaced_at)) if !live => (replaced_at, by.unwrap_or(Value::Null), false),
            (None, _) => continue,
        };
        earlier.push(Earlier {
            memory: String::new(),
            name: event.name.clone(),
            version: event.version,
            doc: event.doc.clone(),
            replaced_at,
            by,
            deleted,
        });
    }
    earlier.reverse();
    earlier
}

/// The memory `id` names among `kind`: a live entry by its name, then by an
/// alias an earlier title gave it, else the last one the log knew by that
/// name. Says whether it is live.
fn resolve(conn: &Connection, kind: &str, id: &str) -> Result<Option<(String, bool)>, String> {
    if let Some(memory) = conn
        .query_row(
            "SELECT id FROM memory WHERE kind = ? AND name = ?",
            params![kind, id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(sql)?
    {
        return Ok(Some((memory, true)));
    }
    let mut statement = conn
        .prepare("SELECT id, doc FROM memory WHERE kind = ?")
        .map_err(sql)?;
    let aliased = statement
        .query_map([kind], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sql)?
        .filter_map(Result::ok)
        .find(|(_, doc)| aliases(&parse(doc)).iter().any(|alias| alias == id));
    if let Some((memory, _)) = aliased {
        return Ok(Some((memory, true)));
    }
    let logged = conn
        .query_row(
            "SELECT memory_id FROM memory_event WHERE kind = ? AND name = ?
             ORDER BY at DESC, rowid DESC LIMIT 1",
            params![kind, id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(sql)?;
    let Some(memory) = logged else {
        return Ok(None);
    };
    let live = conn
        .query_row("SELECT 1 FROM memory WHERE id = ?", [&memory], |_| Ok(()))
        .optional()
        .map_err(sql)?
        .is_some();
    Ok(Some((memory, live)))
}

fn aliases(doc: &Value) -> Vec<String> {
    doc.get("aliases")
        .and_then(Value::as_array)
        .map(|aliases| {
            aliases
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// What a store holds, read without changing anything (no migration, no
/// import), for a bug report: its schema, entry, event and refinement counts,
/// and the whole document. `None` when the scope has no store.
///
/// # Errors
/// The store cannot be read.
pub fn snapshot(dir: &Path) -> Result<Option<Value>, String> {
    let path = db_path(dir);
    if !path.is_file() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(sql)?;
    conn.busy_timeout(Duration::from_secs(5)).map_err(sql)?;
    let count = |table: &str| {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(sql)
    };
    let schema = conn
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
        .map_err(sql)?;
    if schema < 1 {
        return Ok(Some(json!({"schema": schema})));
    }
    Ok(Some(json!({
        "schema": schema,
        "entries": count("memory")?,
        "events": count("memory_event")?,
        "refinements": count("refinement")?,
        "document": document(&conn)?,
    })))
}

struct Store {
    conn: Connection,
    root: &'static str,
}

impl Store {
    /// The store of `dir`, created when missing, migrated to this schema, with
    /// any legacy file beside it imported.
    fn open(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|error| format!("memory store directory: {error}"))?;
        let conn = Connection::open(db_path(dir)).map_err(sql)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(sql)?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
            .map_err(sql)?;
        conn.execute_batch("PRAGMA synchronous = NORMAL")
            .map_err(sql)?;
        let mut store = Self {
            conn,
            root: root_of(dir),
        };
        store.migrate()?;
        store.import_legacy(dir)?;
        Ok(store)
    }

    /// The store of `dir` when it has one, or a legacy file to import: reading
    /// a scope never creates a store.
    fn existing(dir: &Path) -> Result<Option<Self>, String> {
        if db_path(dir).exists()
            || dir.join(LEGACY_STATE).exists()
            || dir.join(LEGACY_JOURNAL).exists()
        {
            Self::open(dir).map(Some)
        } else {
            Ok(None)
        }
    }

    fn begin(&mut self) -> Result<Transaction<'_>, String> {
        self.conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)
    }

    fn migrate(&mut self) -> Result<(), String> {
        let read = |conn: &Connection| {
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))
                .map_err(sql)
        };
        let version = read(&self.conn)?;
        if version > SCHEMA_VERSION {
            return Err(format!(
                "the memory store was written by a newer ha (schema {version}, this one knows {SCHEMA_VERSION})"
            ));
        }
        if version == SCHEMA_VERSION {
            return Ok(());
        }
        let transaction = self.begin()?;
        // Another process may have migrated while this one waited.
        if read(&transaction)? < 1 {
            transaction.execute_batch(SCHEMA_V1).map_err(sql)?;
        }
        transaction
            .execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
            .map_err(sql)?;
        transaction.commit().map_err(sql)
    }

    /// Import a `harness_state.json` and a `memory_history.jsonl` found beside
    /// the store, then rename them: the state is upserted entry by entry and
    /// its refinements appended, and the journal's earlier versions become
    /// events. Importing the same journal twice adds nothing; an unreadable
    /// state file is set aside as `.unreadable`.
    fn import_legacy(&mut self, dir: &Path) -> Result<(), String> {
        let state_path = dir.join(LEGACY_STATE);
        let journal_path = dir.join(LEGACY_JOURNAL);
        if !state_path.exists() && !journal_path.exists() {
            return Ok(());
        }
        let root = self.root;
        let transaction = self.begin()?;
        let mut set_aside = Vec::new();
        // The state first, so the journal's versions join the rows they are
        // earlier versions of.
        if let Ok(text) = std::fs::read_to_string(&state_path) {
            match serde_json::from_str::<Value>(&text)
                .ok()
                .filter(Value::is_object)
            {
                Some(legacy) => {
                    let mut current = document(&transaction)?;
                    merge_legacy(&mut current, &legacy);
                    save(
                        &transaction,
                        root,
                        &current,
                        &Writer::new("migration", &json!({"from": LEGACY_STATE}), "migrate"),
                    )?;
                    set_aside.push((state_path, "imported"));
                }
                None => set_aside.push((state_path, "unreadable")),
            }
        }
        if let Ok(text) = std::fs::read_to_string(&journal_path) {
            let key = format!("imported_journal:{}", hex(&Sha256::digest(text.as_bytes())));
            let seen = transaction
                .query_row("SELECT 1 FROM store_meta WHERE key = ?", [&key], |_| Ok(()))
                .optional()
                .map_err(sql)?
                .is_some();
            if !seen {
                import_journal(&transaction, root, &text)?;
                transaction
                    .execute(
                        "INSERT INTO store_meta(key, value) VALUES (?, ?)",
                        params![key, now()],
                    )
                    .map_err(sql)?;
            }
            set_aside.push((journal_path, "imported"));
        }
        transaction.commit().map_err(sql)?;
        for (path, suffix) in set_aside {
            let target = PathBuf::from(format!("{}.{suffix}", path.display()));
            let _ = std::fs::remove_file(&target);
            // Left in place (a reader holds it open), it is imported again on
            // the next open, which changes nothing.
            let _ = std::fs::rename(&path, &target);
        }
        Ok(())
    }
}

/// Merge a legacy state into a document: its entries replace the document's
/// of the same kind and name, its refinements not yet present are appended,
/// and its other fields fill the ones the document lacks.
fn merge_legacy(current: &mut Value, legacy: &Value) {
    if let Some(kinds) = legacy["entries"].as_object() {
        for (kind, records) in kinds {
            let Some(records) = records.as_object() else {
                continue;
            };
            if !current["entries"][kind].is_object() {
                current["entries"][kind] = json!({});
            }
            for (name, entry) in records {
                current["entries"][kind][name] = entry.clone();
            }
        }
    }
    if let Some(events) = legacy["refinements"].as_array() {
        let present = current["refinements"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut merged = present.clone();
        for event in events {
            let known = present.iter().any(|kept| {
                kept == event || (event["id"].is_string() && kept["id"] == event["id"])
            });
            if !known {
                merged.push(event.clone());
            }
        }
        current["refinements"] = Value::Array(merged);
    }
    if let (Some(current), Some(legacy)) = (current.as_object_mut(), legacy.as_object()) {
        for (field, value) in legacy {
            if field != "entries" && field != "refinements" && !current.contains_key(field) {
                current.insert(field.clone(), value.clone());
            }
        }
    }
}

/// The old journal's records as events. A record kept the version an update
/// replaced, or the entry a delete removed, with when and by whom: a replaced
/// version becomes an event at the time it was written that says who
/// replaced it and when; a removal becomes the delete it was.
fn import_journal(transaction: &Transaction<'_>, root: &str, text: &str) -> Result<(), String> {
    let operation = new_id();
    let mut fresh = BTreeMap::<(String, String), String>::new();
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let entry = &record["entry"];
        let (Some(kind), Some(name)) = (
            record["kind"].as_str(),
            record["id"].as_str().or_else(|| entry["id"].as_str()),
        ) else {
            continue;
        };
        if !entry.is_object() {
            continue;
        }
        let memory = match resolve(transaction, kind, name)? {
            Some((memory, _)) => memory,
            None => fresh
                .entry((kind.to_owned(), name.to_owned()))
                .or_insert_with(new_id)
                .clone(),
        };
        let replaced_at = record["at"]
            .as_str()
            .and_then(|at| instant(at).ok())
            .unwrap_or_else(now);
        let by = &record["by"];
        let (operation_kind, at, actor, cause) = if record["action"] == "delete" {
            (
                "delete",
                replaced_at,
                Writer::new(&writer_type(by), by, "delete").actor,
                "delete",
            )
        } else {
            let written = entry["updated_at"]
                .as_str()
                .or_else(|| entry["created_at"].as_str())
                .and_then(|at| instant(at).ok())
                .unwrap_or_else(|| replaced_at.clone());
            let mut actor = Writer::new("migration", &entry["origin"], "migrate").actor;
            actor["replaced_at"] = json!(replaced_at);
            actor["replaced_by"] = by.clone();
            ("update", written, actor, "migrate")
        };
        insert_event(
            transaction,
            &EventRow {
                at: &at,
                memory: &memory,
                kind,
                name,
                tree: &tree_for(root, entry),
                operation: operation_kind,
                operation_id: &operation,
                cause,
                actor: &actor,
                version: entry["version"].as_i64().unwrap_or(1),
                doc: entry,
            },
        )?;
    }
    Ok(())
}

/// The writer type an old journal's `by` names.
fn writer_type(by: &Value) -> String {
    match by["via"].as_str() {
        Some("memory tool") => "model",
        Some("rlm.harness") => "kernel",
        Some("refine") => "review",
        _ => "migration",
    }
    .to_owned()
}

/// The document the rows make.
fn document(conn: &Connection) -> Result<Value, String> {
    let mut top = conn
        .query_row(
            "SELECT value FROM store_meta WHERE key = 'top'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(sql)?
        .map(|text| parse(&text))
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"schema": 1}));
    let mut entries = Map::new();
    for kind in KINDS {
        entries.insert(kind.to_owned(), json!({}));
    }
    let mut statement = conn
        .prepare("SELECT kind, name, doc FROM memory ORDER BY kind, name")
        .map_err(sql)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(sql)?;
    for row in rows {
        let (kind, name, doc) = row.map_err(sql)?;
        let records = entries.entry(kind).or_insert_with(|| json!({}));
        records[name] = parse(&doc);
    }
    top["entries"] = Value::Object(entries);
    top["refinements"] = Value::Array(refinements(conn)?);
    Ok(top)
}

fn refinements(conn: &Connection) -> Result<Vec<Value>, String> {
    let mut statement = conn
        .prepare("SELECT event FROM refinement ORDER BY seq")
        .map_err(sql)?;
    let events = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql)?
        .map(|event| event.map(|event| parse(&event)).map_err(sql))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(events)
}

/// A row as a save compares it.
struct Current {
    id: String,
    doc: Value,
    content: String,
    content_version: i64,
}

/// [`save_document`] inside a transaction.
#[allow(
    clippy::too_many_lines,
    reason = "one diff: renames, then updates and inserts, then deletes, then the rest"
)]
fn save(
    transaction: &Transaction<'_>,
    root: &str,
    state: &Value,
    writer: &Writer,
) -> Result<usize, String> {
    let mut current = BTreeMap::<(String, String), Current>::new();
    {
        let mut statement = transaction
            .prepare("SELECT id, kind, name, doc, content, content_version FROM memory")
            .map_err(sql)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(sql)?;
        for row in rows {
            let (id, kind, name, doc, content, content_version) = row.map_err(sql)?;
            current.insert(
                (kind, name),
                Current {
                    id,
                    doc: parse(&doc),
                    content,
                    content_version,
                },
            );
        }
    }
    let mut wanted = BTreeMap::<(String, String), &Value>::new();
    if let Some(kinds) = state["entries"].as_object() {
        for (kind, records) in kinds {
            if let Some(records) = records.as_object() {
                for (name, doc) in records {
                    wanted.insert((kind.clone(), name.clone()), doc);
                }
            }
        }
    }
    // An entry a new title renamed is saved under its new name with the old
    // one among its aliases: it is the same memory, and keeps its id.
    let mut renamed = BTreeMap::<(String, String), (String, String)>::new();
    for (key, doc) in &wanted {
        if current.contains_key(key) {
            continue;
        }
        let from = aliases(doc).into_iter().find_map(|alias| {
            let old = (key.0.clone(), alias);
            (current.contains_key(&old)
                && !wanted.contains_key(&old)
                && !renamed.values().any(|taken| *taken == old))
            .then_some(old)
        });
        if let Some(from) = from {
            renamed.insert(key.clone(), from);
        }
    }
    let operation = writer.operation.clone();
    let at = now();
    let mut changed = 0;
    for (key, doc) in &wanted {
        let (kind, name) = key;
        let fields = Fields::of(root, kind, name, doc);
        let existing = current
            .get(key)
            .or_else(|| renamed.get(key).and_then(|from| current.get(from)));
        let (id, operation_kind, content_version) = match existing {
            Some(row) if current.contains_key(key) && row.doc == **doc => continue,
            Some(row) => (
                row.id.clone(),
                "update",
                if row.content == fields.content {
                    row.content_version
                } else {
                    row.content_version + 1
                },
            ),
            None => (
                writer.keep.get(key).cloned().unwrap_or_else(new_id),
                "insert",
                1,
            ),
        };
        if operation_kind == "insert" {
            transaction
                .execute(
                    "INSERT INTO memory(id, kind, name, tree, title, content, meta, temporal_start,
                     temporal_end, doc, version, version_hash, content_version, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    params![
                        id,
                        kind,
                        name,
                        fields.tree,
                        fields.title,
                        fields.content,
                        fields.meta,
                        fields.temporal.as_ref().map(|bounds| &bounds.0),
                        fields.temporal.as_ref().map(|bounds| &bounds.1),
                        doc.to_string(),
                        fields.version,
                        fields.hash,
                        content_version,
                        fields.created_at,
                        fields.updated_at,
                    ],
                )
                .map_err(sql)?;
        } else {
            transaction
                .execute(
                    "UPDATE memory SET name = ?2, tree = ?3, title = ?4, content = ?5, meta = ?6,
                     temporal_start = ?7, temporal_end = ?8, doc = ?9, version = ?10,
                     version_hash = ?11, content_version = ?12, updated_at = ?13 WHERE id = ?1",
                    params![
                        id,
                        name,
                        fields.tree,
                        fields.title,
                        fields.content,
                        fields.meta,
                        fields.temporal.as_ref().map(|bounds| &bounds.0),
                        fields.temporal.as_ref().map(|bounds| &bounds.1),
                        doc.to_string(),
                        fields.version,
                        fields.hash,
                        content_version,
                        fields.updated_at,
                    ],
                )
                .map_err(sql)?;
        }
        insert_event(
            transaction,
            &EventRow {
                at: &at,
                memory: &id,
                kind,
                name,
                tree: &fields.tree,
                operation: operation_kind,
                operation_id: &operation,
                cause: &writer.cause,
                actor: &writer.actor,
                version: fields.version,
                doc,
            },
        )?;
        changed += 1;
    }
    for (key, row) in &current {
        if wanted.contains_key(key) || renamed.values().any(|from| from == key) {
            continue;
        }
        transaction
            .execute("DELETE FROM memory WHERE id = ?", [&row.id])
            .map_err(sql)?;
        insert_event(
            transaction,
            &EventRow {
                at: &at,
                memory: &row.id,
                kind: &key.0,
                name: &key.1,
                tree: &tree_for(root, &row.doc),
                operation: "delete",
                operation_id: &operation,
                cause: &writer.cause,
                actor: &writer.actor,
                version: row.doc["version"].as_i64().unwrap_or(1),
                doc: &row.doc,
            },
        )?;
        changed += 1;
    }
    let events = state["refinements"].as_array().cloned().unwrap_or_default();
    if refinements(transaction)? != events {
        transaction
            .execute("DELETE FROM refinement", [])
            .map_err(sql)?;
        for (seq, event) in events.iter().enumerate() {
            transaction
                .execute(
                    "INSERT INTO refinement(seq, event) VALUES (?, ?)",
                    params![i64::try_from(seq).unwrap_or(i64::MAX), event.to_string()],
                )
                .map_err(sql)?;
        }
    }
    let mut top = state.as_object().cloned().unwrap_or_default();
    top.remove("entries");
    top.remove("refinements");
    transaction
        .execute(
            "INSERT INTO store_meta(key, value) VALUES ('top', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [Value::Object(top).to_string()],
        )
        .map_err(sql)?;
    prune(transaction)?;
    Ok(changed)
}

/// Drop events past the retention window, keeping each memory's newest ones;
/// at most once an hour.
fn prune(transaction: &Transaction<'_>) -> Result<(), String> {
    let now = chrono::Utc::now();
    let last = transaction
        .query_row(
            "SELECT value FROM store_meta WHERE key = 'pruned_at'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(sql)?
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(&at).ok());
    if last.is_some_and(|last| {
        (now - last.with_timezone(&chrono::Utc)).num_seconds() < PRUNE_EVERY_SECONDS
    }) {
        return Ok(());
    }
    let cutoff = stamp(now - chrono::Duration::days(RETAIN_DAYS));
    transaction
        .execute(
            "DELETE FROM memory_event WHERE at < ?1 AND event_id NOT IN (
               SELECT event_id FROM (
                 SELECT event_id, row_number() OVER (
                   PARTITION BY memory_id ORDER BY at DESC, rowid DESC) AS newest
                 FROM memory_event)
               WHERE newest <= ?2)",
            params![cutoff, RETAIN_VERSIONS],
        )
        .map_err(sql)?;
    transaction
        .execute(
            "INSERT INTO store_meta(key, value) VALUES ('pruned_at', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [stamp(now)],
        )
        .map_err(sql)?;
    Ok(())
}

/// The columns an entry's document gives its row.
struct Fields {
    tree: String,
    title: String,
    content: String,
    meta: String,
    temporal: Option<(String, String)>,
    version: i64,
    hash: String,
    created_at: String,
    updated_at: String,
}

impl Fields {
    fn of(root: &str, kind: &str, name: &str, doc: &Value) -> Self {
        let text = |field: &str| {
            doc.get(field)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let time = |field: &str| {
            doc.get(field)
                .and_then(Value::as_str)
                .and_then(|time| instant(time).ok())
        };
        let meta = doc
            .get("metadata")
            .filter(|meta| meta.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        // A stored temporal that does not parse stays in the entry, unindexed.
        let temporal = temporal_bounds(doc.get("temporal").unwrap_or(&Value::Null))
            .ok()
            .flatten();
        let tree = tree_for(root, doc);
        let title = text("title");
        let content = text("content");
        let hash = hex(&Sha256::digest(
            json!([kind, name, tree, title, content, meta, temporal])
                .to_string()
                .as_bytes(),
        ))[..32]
            .to_owned();
        let created_at = time("created_at").unwrap_or_else(now);
        Self {
            updated_at: time("updated_at").unwrap_or_else(|| created_at.clone()),
            created_at,
            meta: meta.to_string(),
            tree,
            title,
            content,
            temporal,
            version: doc.get("version").and_then(Value::as_i64).unwrap_or(1),
            hash,
        }
    }
}

/// One event to write.
struct EventRow<'a> {
    at: &'a str,
    memory: &'a str,
    kind: &'a str,
    name: &'a str,
    tree: &'a str,
    operation: &'a str,
    operation_id: &'a str,
    cause: &'a str,
    actor: &'a Value,
    version: i64,
    doc: &'a Value,
}

fn insert_event(transaction: &Transaction<'_>, event: &EventRow<'_>) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO memory_event(event_id, at, memory_id, kind, name, tree, operation,
             operation_id, cause, actor, version, doc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                new_id(),
                event.at,
                event.memory,
                event.kind,
                event.name,
                event.tree,
                event.operation,
                event.operation_id,
                event.cause,
                event.actor.to_string(),
                event.version,
                event.doc.to_string(),
            ],
        )
        .map_err(sql)?;
    Ok(())
}

fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[allow(clippy::needless_pass_by_value, reason = "used as a map_err adapter")]
fn sql(error: rusqlite::Error) -> String {
    format!("memory store: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer() -> Writer {
        Writer::new("model", &json!({"via": "test"}), "update")
    }

    fn entry(title: &str, content: &str, version: i64) -> Value {
        json!({"id": title, "title": title, "content": content, "path": "user/Sở thích", "version": version})
    }

    #[test]
    fn a_scope_without_a_store_reads_empty_and_creates_nothing() {
        let directory = tempfile::tempdir().expect("dir");
        let dir = directory.path().join("harness");
        assert_eq!(load_document(&dir).expect("load"), empty_document());
        assert!(!dir.exists());
    }

    #[test]
    fn a_document_round_trips_with_unknown_fields() {
        let directory = tempfile::tempdir().expect("dir");
        let mut state = empty_document();
        state["entries"]["memory"]["tabs"] =
            json!({"title": "Tabs", "content": "tabs", "version": 1, "odd": [1, {"x": 2}]});
        state["entries"]["factory"] = json!({"f": {"title": 3, "content": null}});
        state["refinements"] = json!([{"id": "r1", "trigger": "t", "changes": []}]);
        state["extra"] = json!("kept");
        assert_eq!(
            save_document(directory.path(), &state, &writer()).expect("save"),
            2
        );
        assert_eq!(load_document(directory.path()).expect("load"), state);
        // An identical save changes nothing.
        assert_eq!(
            save_document(directory.path(), &state, &writer()).expect("save"),
            0
        );
    }

    #[test]
    fn each_change_is_an_event_and_a_rename_keeps_the_memory() {
        let directory = tempfile::tempdir().expect("dir");
        let dir = directory.path();
        let mut state = empty_document();
        state["entries"]["memory"]["a"] = entry("a", "first", 1);
        save_document(dir, &state, &writer()).expect("create");
        state["entries"]["memory"]["a"] = entry("a", "second", 2);
        save_document(dir, &state, &writer()).expect("update");
        let mut renamed = entry("b", "second", 3);
        renamed["aliases"] = json!(["a"]);
        state["entries"]["memory"]
            .as_object_mut()
            .expect("kind")
            .remove("a");
        state["entries"]["memory"]["b"] = renamed;
        save_document(dir, &state, &Writer::new("review", &json!({}), "refine")).expect("rename");
        let earlier = earlier_versions(dir, "memory", "a").expect("history");
        assert_eq!(
            earlier
                .iter()
                .map(|version| version.version)
                .collect::<Vec<_>>(),
            [2, 1]
        );
        assert_eq!(earlier[0].by["type"], "review");
        assert!(!earlier[0].deleted);
        state["entries"]["memory"]
            .as_object_mut()
            .expect("kind")
            .remove("b");
        save_document(dir, &state, &writer()).expect("delete");
        let earlier = earlier_versions(dir, "memory", "b").expect("history");
        assert_eq!(
            earlier
                .iter()
                .map(|version| (version.version, version.deleted))
                .collect::<Vec<_>>(),
            [(3, true), (2, false), (1, false)]
        );
    }

    #[test]
    fn a_legacy_state_and_journal_are_imported_once_and_set_aside() {
        let directory = tempfile::tempdir().expect("dir");
        let dir = directory.path();
        std::fs::write(
            dir.join(LEGACY_STATE),
            json!({"schema": 1, "entries": {"memory": {"tabs": {"title": "Tabs", "content": "now spaces", "version": 2, "updated_at": "2026-10-01T00:00:00Z"}}}, "refinements": [{"id": "r1"}]}).to_string(),
        )
        .expect("state");
        std::fs::write(
            dir.join(LEGACY_JOURNAL),
            format!(
                "{}\n{}\n",
                json!({"at": "2026-10-01T00:00:00Z", "action": "update", "kind": "memory", "id": "tabs", "entry": {"title": "Tabs", "content": "tabs", "version": 1, "updated_at": "2026-09-01T00:00:00Z"}, "by": {"via": "memory tool"}}),
                json!({"at": "2026-10-02T00:00:00Z", "action": "delete", "kind": "memory", "id": "gone", "entry": {"title": "Gone", "content": "x", "version": 4}, "by": {"via": "refine"}})
            ),
        )
        .expect("journal");
        let state = load_document(dir).expect("load");
        assert_eq!(state["entries"]["memory"]["tabs"]["content"], "now spaces");
        assert_eq!(state["refinements"], json!([{"id": "r1"}]));
        assert!(!dir.join(LEGACY_STATE).exists());
        assert!(dir.join(format!("{LEGACY_STATE}.imported")).exists());
        let earlier = earlier_versions(dir, "memory", "tabs").expect("history");
        assert_eq!(earlier.len(), 1);
        assert_eq!(earlier[0].version, 1);
        assert_eq!(earlier[0].by["via"], "memory tool");
        assert_eq!(earlier[0].replaced_at, "2026-10-01T00:00:00.000Z");
        let gone = earlier_versions(dir, "memory", "gone").expect("history");
        assert!(gone[0].deleted);
        assert_eq!(gone[0].by["type"], "review");
        // Dropped in again, a state file adds to what is kept.
        std::fs::write(
            dir.join(LEGACY_STATE),
            json!({"entries": {"prompt": {"p": {"title": "P", "content": "c"}}}}).to_string(),
        )
        .expect("state");
        let state = load_document(dir).expect("load");
        assert_eq!(state["entries"]["prompt"]["p"]["title"], "P");
        assert_eq!(state["entries"]["memory"]["tabs"]["version"], 2);
    }

    #[test]
    fn an_unreadable_legacy_state_is_set_aside() {
        let directory = tempfile::tempdir().expect("dir");
        std::fs::write(directory.path().join(LEGACY_STATE), "{not json").expect("state");
        assert_eq!(
            load_document(directory.path()).expect("load"),
            empty_document()
        );
        assert!(
            directory
                .path()
                .join(format!("{LEGACY_STATE}.unreadable"))
                .exists()
        );
    }

    #[test]
    fn trees_temporal_and_hashes_come_from_the_entry() {
        assert_eq!(
            tree_for("/global", &json!({"path": "user/Sở thích"})),
            "/global/user/So_thich"
        );
        assert_eq!(
            tree_for("/global", &json!({"project": {"key": "p-1"}})),
            "/projects/p-1/general"
        );
        assert_eq!(root_of(Path::new("data/sessions/task_1/harness")), "/local");
        assert_eq!(root_of(Path::new("data/harness")), "/global");
        assert_eq!(
            temporal_bounds(&json!("2026-10-26")).expect("date"),
            Some((
                "2026-10-26T00:00:00.000Z".to_owned(),
                "2026-10-26T00:00:00.000Z".to_owned()
            ))
        );
        assert!(temporal_bounds(&json!(["2026-10-27T00:00:00Z", "2026-10-26T00:00:00Z"])).is_err());
        assert!(ended(
            &json!({"temporal": {"start": "2020-01-01", "end": "2021-01-01"}})
        ));
        assert!(!ended(&json!({"temporal": {"start": "2020-01-01"}})));
        assert!(!ended(
            &json!({"temporal": {"start": "2020-01-01", "end": "2999-01-01"}})
        ));
    }
}
