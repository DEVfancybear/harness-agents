//! `ha bug-report`: everything needed to find a bug, in one zip file.
//!
//! A bug report used to be a screenshot: what the terminal showed, without
//! the notice before it, the provider's retries, the compaction's numbers or
//! the worker that died behind it. This gathers what ha recorded - the
//! diagnostics log of every process, the workers' logs, each project store's
//! runs, attempts and settings, each conversation's harness files - from a
//! window of time, redacts secrets, and puts a `SUMMARY.md` in front: the
//! problems found, newest first, and one merged timeline.
//!
//! What it never reads: `private/` (credentials), a worker descriptor's
//! token, the kernel's pickled state, attachments and artifacts. Without
//! `--include-content` no request body, checkpoint or long setting is read:
//! their sizes are.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use harness_types::{ErrorCode, HarnessError};
use serde_json::{Value, json};

/// How much of one log or history file the report keeps: its newest part.
const FILE_TAIL_BYTES: usize = 2 * 1024 * 1024;
/// The uncompressed size the whole report stays under.
const REPORT_BYTES: usize = 64 * 1024 * 1024;
/// Lines of the timeline in `SUMMARY.md`; `timeline.jsonl` has every point.
const SUMMARY_TIMELINE: usize = 400;
/// Conversations whose harness files are read, newest first.
const MAX_CONVERSATIONS: usize = 24;

/// What to report on.
#[derive(Clone, Debug)]
pub struct Options {
    /// The window: records older than this are left out.
    pub since: std::time::Duration,
    /// Only this conversation (`task_...`).
    pub task: Option<String>,
    /// Only this session (`session_...`).
    pub session: Option<String>,
    /// Request bodies, checkpoints and every setting's value.
    pub include_content: bool,
}

/// One point of the merged timeline.
#[derive(Clone, Debug)]
struct Point {
    ms: i64,
    source: String,
    level: &'static str,
    what: String,
    text: String,
    task: Option<String>,
}

/// The report as it is gathered: files by their path in the zip.
struct Report {
    options: Options,
    since_ms: i64,
    data_dir: PathBuf,
    files: BTreeMap<String, Vec<u8>>,
    points: Vec<Point>,
    /// Diagnostic records in the window, for the problems.
    records: Vec<Value>,
    /// Store rows that are problems: failed attempts, runs that did not finish.
    /// What the stores and logs show went wrong, alike ones together: how
    /// many, the first and the last time.
    store_problems: BTreeMap<String, (usize, i64, i64)>,
    /// Panics in the workers' logs, by where and why: how many, the time of
    /// the log line before the newest, and which logs.
    log_panics: BTreeMap<String, (usize, i64, BTreeSet<String>)>,
    conversations: BTreeSet<String>,
    /// The delegated children of the conversation (of every conversation of
    /// the window when the report has none), by task: their ledger entries.
    children: BTreeMap<String, Value>,
    notes: Vec<String>,
    known_secrets: Vec<String>,
    redactions: usize,
}

/// Parse `30m`, `2h`, `3d`, `90s` (a bare number is hours).
///
/// # Errors
/// The text is not a number with one of those units.
pub fn parse_window(text: &str) -> Result<std::time::Duration, String> {
    let text = text.trim();
    let (number, unit) = text
        .find(|character: char| !character.is_ascii_digit())
        .map_or((text, "h"), |index| text.split_at(index));
    let number = number
        .parse::<u64>()
        .map_err(|_| format!("`{text}` is not a window such as 30m, 2h or 3d"))?;
    let seconds = match unit.trim() {
        "s" => number,
        "m" => number * 60,
        "h" => number * 3600,
        "d" => number * 86_400,
        _ => return Err(format!("`{text}` is not a window such as 30m, 2h or 3d")),
    };
    Ok(std::time::Duration::from_secs(seconds))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn data_dir() -> Result<PathBuf, HarnessError> {
    let environment = crate::interactive::paths::LaunchEnvironment::capture();
    crate::interactive::paths::resolve(&crate::interactive::paths::PathRequest {
        platform: crate::interactive::paths::HostPlatform::current(),
        environment: &environment,
        explicit_data_dir: None,
    })
    .map(|paths| paths.data_dir)
}

/// Gather the report and write it as a zip at `out`; returns what it holds.
///
/// # Errors
/// The data directory cannot be resolved, or the zip cannot be written.
pub async fn write(options: Options, out: &Path) -> Result<Value, HarnessError> {
    let report = gather(options).await?;
    let summary = json!({
        "out": out.display().to_string(),
        "files": report.files.len(),
        "problems": report.problem_count(),
        "conversations": report.conversations.iter().collect::<Vec<_>>(),
        "redactions": report.redactions,
        "notes": report.notes,
    });
    let bytes = report.into_zip();
    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|error| io_error(&error, parent))?;
    }
    std::fs::write(out, bytes).map_err(|error| io_error(&error, out))?;
    Ok(summary)
}

/// The merged timeline alone, oldest first, for `ha diag timeline`.
///
/// # Errors
/// The data directory cannot be resolved.
pub async fn timeline(options: Options) -> Result<Vec<Value>, HarnessError> {
    let report = gather(options).await?;
    Ok(report.sorted_points().iter().map(point_json).collect())
}

fn io_error(error: &std::io::Error, path: &Path) -> HarnessError {
    HarnessError::new(
        ErrorCode::StorageWriteFailed,
        format!("{}: {error}", path.display()),
    )
}

async fn gather(options: Options) -> Result<Report, HarnessError> {
    let data_dir = data_dir()?;
    let since_ms =
        now_ms().saturating_sub(i64::try_from(options.since.as_millis()).unwrap_or(i64::MAX));
    let environment = crate::interactive::paths::LaunchEnvironment::capture();
    let known_secrets = crate::interactive::service::export_secrets(&environment, &data_dir, "")
        .into_iter()
        .filter(|secret| secret.len() >= 8)
        .collect();
    let mut report = Report {
        options,
        since_ms,
        data_dir,
        files: BTreeMap::new(),
        points: Vec::new(),
        records: Vec::new(),
        store_problems: BTreeMap::new(),
        log_panics: BTreeMap::new(),
        conversations: BTreeSet::new(),
        children: BTreeMap::new(),
        notes: Vec::new(),
        known_secrets,
        redactions: 0,
    };
    if let Some(task) = &report.options.task {
        report.conversations.insert(task.clone());
    }
    report.environment(&environment);
    // The children first: their records are kept with the conversation's.
    report.ledgers();
    report.diagnostics();
    report.workers();
    report.stores().await;
    report.harness_files();
    report.other_logs();
    Ok(report)
}

impl Report {
    /// Count one problem the stores show, with those alike.
    fn problem(&mut self, what: String, ms: i64) {
        let entry = self.store_problems.entry(what).or_insert((0, ms, ms));
        entry.0 += 1;
        entry.1 = entry.1.min(ms);
        entry.2 = entry.2.max(ms);
    }

    /// Redact a text the report keeps: the known secret values first, then
    /// the patterns the learned skills are cleaned with.
    fn redact(&mut self, text: &str) -> String {
        let mut text = text.to_owned();
        for secret in &self.known_secrets {
            if text.contains(secret.as_str()) {
                self.redactions += text.matches(secret.as_str()).count();
                text = text.replace(secret.as_str(), "[REDACTED:known]");
            }
        }
        let redacted = crate::interactive::learned::redact(&text);
        if redacted != text {
            self.redactions += redacted
                .matches("[REDACTED")
                .count()
                .saturating_sub(text.matches("[REDACTED").count());
        }
        redacted
    }

    /// Redact every text of a record and leave its numbers alone: redacting
    /// the line took a millisecond timestamp for a card number.
    fn redact_value(&mut self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.redact(text),
            Value::Array(items) => items.iter_mut().for_each(|item| self.redact_value(item)),
            Value::Object(fields) => fields
                .values_mut()
                .for_each(|field| self.redact_value(field)),
            _ => {}
        }
    }

    fn add_text(&mut self, path: &str, text: &str) {
        let text = self.redact(text);
        self.add_bytes(path, text.into_bytes());
    }

    fn add_bytes(&mut self, path: &str, bytes: Vec<u8>) {
        let used = self.files.values().map(Vec::len).sum::<usize>();
        if used + bytes.len() > REPORT_BYTES {
            self.notes.push(format!(
                "{path} left out: the report reached its size limit"
            ));
            return;
        }
        self.files.insert(path.to_owned(), bytes);
    }

    /// Whether a record or row with these ids is in scope.
    fn wanted(&self, task: Option<&str>, session: Option<&str>) -> bool {
        let task_ok = self.options.task.as_deref().is_none_or(|wanted| {
            task.is_none_or(|task| task == wanted || self.children.contains_key(task))
        });
        let session_ok = self
            .options
            .session
            .as_deref()
            .is_none_or(|wanted| session.is_none_or(|session| session == wanted));
        task_ok && session_ok
    }

    fn environment(&mut self, environment: &crate::interactive::paths::LaunchEnvironment) {
        let variables = std::env::vars_os()
            .filter_map(|(name, value)| {
                let name = name.to_string_lossy().into_owned();
                name.starts_with("HA_").then(|| {
                    let shown = if secret_name(&name) {
                        "[withheld]".to_owned()
                    } else {
                        value.to_string_lossy().into_owned()
                    };
                    (name, Value::String(shown))
                })
            })
            .collect::<serde_json::Map<_, _>>();
        let listing = std::fs::read_dir(&self.data_dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let bytes = directory_bytes(&entry.path(), 3);
                        json!({"name": name, "bytes": bytes})
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let config = crate::interactive::paths::resolve(&crate::interactive::paths::PathRequest {
            platform: crate::interactive::paths::HostPlatform::current(),
            environment,
            explicit_data_dir: None,
        })
        .ok()
        .map(|paths| paths.config_file);
        let env = json!({
            "version": env!("CARGO_PKG_VERSION"),
            "build": crate::interactive::agents::registry::build_identity(),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "exe": std::env::current_exe().map(|path| path.display().to_string()).unwrap_or_default(),
            "data_dir": self.data_dir.display().to_string(),
            "config_file": config.as_ref().map(|path| path.display().to_string()),
            "window_since": ms_text(self.since_ms),
            "generated": ms_text(now_ms()),
            "task": self.options.task,
            "session": self.options.session,
            "include_content": self.options.include_content,
            "variables": variables,
            "terminal": {
                "TERM": std::env::var("TERM").ok(),
                "TERM_PROGRAM": std::env::var("TERM_PROGRAM").ok(),
                "COLORTERM": std::env::var("COLORTERM").ok(),
                "windows_terminal": std::env::var_os("WT_SESSION").is_some(),
                "ssh": std::env::var_os("SSH_CONNECTION").is_some(),
            },
            "data_dir_listing": listing,
        });
        self.add_text("env.json", &pretty(&env));
        if let Some(config) = &config
            && let Ok(text) = std::fs::read_to_string(config)
        {
            let text = withhold_secret_lines(&text);
            self.add_text("config/config.toml", &text);
        }
        // The settings and custom models beside it, every secret-named value
        // withheld.
        if let Some(dir) = config.as_ref().and_then(|config| config.parent()) {
            for name in ["settings.json", "models.json"] {
                let Ok(text) = std::fs::read_to_string(dir.join(name)) else {
                    continue;
                };
                let shown = match serde_json::from_str::<Value>(&text) {
                    Ok(mut value) => {
                        withhold_secret_values(&mut value);
                        pretty(&value)
                    }
                    Err(_) => withhold_secret_lines(&text),
                };
                self.add_text(&format!("config/{name}"), &shown);
            }
        }
    }

    /// Every process's diagnostics log, cut to the window.
    fn diagnostics(&mut self) {
        let dir = crate::diagnostics::location(&self.data_dir);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            self.notes
                .push(format!("no diagnostics directory at {}", dir.display()));
            return;
        };
        let mut files = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .filter(|path| modified_ms(path).is_none_or(|ms| ms >= self.since_ms))
            .collect::<Vec<_>>();
        files.sort();
        let mut left_out = 0;
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut kept = String::new();
            let mut points = Vec::new();
            let mut records = Vec::new();
            let mut tasks = BTreeSet::new();
            for line in text.lines() {
                let Ok(mut record) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if record["ms"].as_i64().is_some_and(|ms| ms < self.since_ms) {
                    continue;
                }
                let task = record["task"].as_str().map(str::to_owned);
                if !self.wanted(task.as_deref(), record["session"].as_str()) {
                    continue;
                }
                self.redact_value(&mut record);
                kept.push_str(&record.to_string());
                kept.push('\n');
                if let Some(task) = &task {
                    tasks.insert(task.clone());
                }
                let task = task.as_deref();
                points.push(Point {
                    ms: record["ms"].as_i64().unwrap_or_default(),
                    source: format!(
                        "{}:{}",
                        record["role"].as_str().unwrap_or("?"),
                        record["pid"].as_u64().unwrap_or_default()
                    ),
                    level: level_of(record["level"].as_str()),
                    what: format!(
                        "{}.{}",
                        record["kind"].as_str().unwrap_or("?"),
                        record["event"].as_str().unwrap_or("?")
                    ),
                    text: record_text(&record),
                    task: task.map(str::to_owned),
                });
                records.push(record);
            }
            // A conversation's report keeps the processes that worked on it: the
            // others' records carry no conversation, and every other process of
            // the window - another project, a test run - came along with them.
            if let Some(wanted) = &self.options.task
                && !tasks.contains(wanted)
                && !tasks.iter().any(|task| self.children.contains_key(task))
            {
                if !kept.is_empty() {
                    left_out += 1;
                }
                continue;
            }
            if self.options.task.is_none() {
                self.conversations.extend(tasks);
            }
            self.points.extend(points);
            self.records.extend(records);
            if !kept.is_empty() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                self.add_bytes(&format!("diagnostics/{name}"), kept.into_bytes());
            }
        }
        if left_out > 0 {
            self.notes.push(format!(
                "{left_out} process log(s) with no record of this conversation left out (`ha bug-report` without a conversation keeps them)"
            ));
        }
    }

    /// The delegation ledgers: each conversation's children, with their role,
    /// model, state, error, time and tool calls - what a `[child-failed]`
    /// notice does not say. A conversation's report follows its children's
    /// ledgers down, and keeps their diagnostics with its own.
    fn ledgers(&mut self) {
        let dir = self.data_dir.join("delegation").join("ledger");
        let mut pending = match &self.options.task {
            Some(task) => vec![task.clone()],
            None => std::fs::read_dir(&dir)
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .map(|entry| entry.path())
                        .filter(|path| modified_ms(path).is_none_or(|ms| ms >= self.since_ms))
                        .filter_map(|path| {
                            path.file_stem()
                                .map(|stem| stem.to_string_lossy().into_owned())
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        let mut read = BTreeSet::new();
        while let Some(task) = pending.pop() {
            if !is_id(&task) || !read.insert(task.clone()) {
                continue;
            }
            let path = dir.join(format!("{task}.json"));
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(mut ledger) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            for child in ledger.as_array().into_iter().flatten() {
                if let Some(id) = child["task_id"].as_str() {
                    self.children.insert(id.to_owned(), child.clone());
                    pending.push(id.to_owned());
                }
            }
            self.redact_value(&mut ledger);
            self.add_bytes(
                &format!("delegation/ledger/{task}.json"),
                pretty(&ledger).into_bytes(),
            );
        }
    }

    /// Logs no conversation owns: the shared REPL's, and what the delegated
    /// children left (names and sizes only).
    fn other_logs(&mut self) {
        let repl = self.data_dir.join("repl");
        if let Ok(entries) = std::fs::read_dir(&repl) {
            let mut paths = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "log"))
                .filter(|path| modified_ms(path).is_none_or(|ms| ms >= self.since_ms))
                .collect::<Vec<_>>();
            paths.sort();
            for path in paths {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                self.copy_file(&path, &format!("repl/{name}"), true);
            }
        }
        let delegation = self.data_dir.join("delegation");
        if let Ok(entries) = std::fs::read_dir(&delegation) {
            let listing = entries
                .filter_map(Result::ok)
                .filter(|entry| modified_ms(&entry.path()).is_none_or(|ms| ms >= self.since_ms))
                .map(|entry| {
                    json!({
                        "name": entry.file_name().to_string_lossy(),
                        "bytes": directory_bytes(&entry.path(), 3),
                        "modified": modified_ms(&entry.path()).map(ms_text),
                    })
                })
                .collect::<Vec<_>>();
            if !listing.is_empty() {
                self.add_text("delegation.json", &pretty(&Value::Array(listing)));
            }
        }
    }

    /// The background workers' logs, journals and descriptors (without the
    /// connection token).
    fn workers(&mut self) {
        let dir = self.data_dir.join("workers");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        let mut paths = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            match path.extension().and_then(|ext| ext.to_str()) {
                Some("log") => {
                    if modified_ms(&path).is_some_and(|ms| ms < self.since_ms) {
                        continue;
                    }
                    let text = tail(&path, FILE_TAIL_BYTES);
                    let kept = self.worker_log(&name, &text);
                    if !kept.is_empty() {
                        self.add_text(&format!("workers/{name}"), &kept);
                    }
                }
                Some("journal") => {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        self.add_text(&format!("workers/{name}"), &text);
                    }
                }
                Some("json") => {
                    if let Ok(mut descriptor) = std::fs::read(&path)
                        .map_err(|error| error.to_string())
                        .and_then(|bytes| {
                            serde_json::from_slice::<Value>(&bytes)
                                .map_err(|error| error.to_string())
                        })
                    {
                        if let Some(object) = descriptor.as_object_mut() {
                            object.remove("token");
                        }
                        self.add_text(&format!("workers/{name}"), &pretty(&descriptor));
                    }
                }
                _ => {}
            }
        }
    }

    /// The lines of a worker log in the window; a line without a time (a
    /// panic's report) goes with the stamped line before it.
    fn worker_log(&mut self, name: &str, text: &str) -> String {
        let mut kept = String::new();
        let mut keeping = false;
        let source = format!("worker:{}", clip(name.trim_end_matches(".log"), 8));
        // The default panic report has no time: it is placed at the line
        // before it, and its message is the line after `panicked at`.
        let mut last_ms = 0;
        let mut panic_at: Option<String> = None;
        for line in text.lines() {
            let stamp = line
                .split_once(' ')
                .and_then(|(first, _)| chrono::DateTime::parse_from_rfc3339(first).ok())
                .map(|time| time.timestamp_millis());
            if let Some(ms) = stamp {
                last_ms = ms;
                panic_at = None;
                keeping = ms >= self.since_ms;
                if keeping {
                    let message = line.split_once(' ').map_or("", |(_, rest)| rest);
                    let level = if message.contains("ended (") || message.contains("could not") {
                        "warn"
                    } else {
                        "info"
                    };
                    self.points.push(Point {
                        ms,
                        source: source.clone(),
                        level,
                        what: "worker.log".to_owned(),
                        text: clip(message, 300),
                        task: None,
                    });
                }
            } else if let Some((_, at)) = line.split_once("panicked at ") {
                panic_at = Some(at.trim().trim_end_matches(':').to_owned());
            } else if let Some(at) = panic_at.take()
                && (keeping || last_ms == 0)
            {
                let what = format!("{at}: {}", clip(line.trim(), 300));
                let entry = self
                    .log_panics
                    .entry(what.clone())
                    .or_insert((0, 0, BTreeSet::new()));
                entry.0 += 1;
                entry.1 = entry.1.max(last_ms);
                entry.2.insert(name.to_owned());
                self.points.push(Point {
                    ms: last_ms,
                    source: source.clone(),
                    level: "error",
                    what: "panic".to_owned(),
                    text: what,
                    task: None,
                });
            }
            if keeping || stamp.is_none() && !kept.is_empty() {
                kept.push_str(line);
                kept.push('\n');
            }
        }
        kept
    }

    /// Each project store written in the window: its runs, attempts, leases
    /// and settings, as rows.
    async fn stores(&mut self) {
        let projects = self.data_dir.join("projects");
        let Ok(entries) = std::fs::read_dir(&projects) else {
            return;
        };
        let mut dirs = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        dirs.sort();
        for dir in dirs {
            let database = dir.join("harness.sqlite3");
            let newest = [
                modified_ms(&database),
                modified_ms(&dir.join("harness.sqlite3-wal")),
            ]
            .into_iter()
            .flatten()
            .max();
            if newest.is_none_or(|ms| ms < self.since_ms) {
                continue;
            }
            let project = dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let store = match harness_store_sqlite::SqliteStore::open_read_only(dir.clone()).await {
                Ok(store) => store,
                Err(error) => {
                    self.notes
                        .push(format!("the store of {project} could not be read: {error}"));
                    continue;
                }
            };
            let tables = store.bug_report_tables(self.options.include_content).await;
            let _ = store.close().await;
            let mut tables = self.filter_tables(&project, tables);
            // Field by field: a row's millisecond times were taken for card
            // numbers when the file was redacted as one text.
            self.redact_value(&mut tables);
            self.add_bytes(
                &format!("store/{project}.json"),
                pretty(&tables).into_bytes(),
            );
            for name in [
                "hidden-conversations.json",
                "writer.owner.json",
                "harness-data.json",
            ] {
                if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
                    self.add_text(&format!("store/{project}/{name}"), &text);
                }
            }
            if let Ok(pending) = std::fs::read_dir(dir.join("refine-pending")) {
                for entry in pending.filter_map(Result::ok) {
                    if let Ok(text) = std::fs::read_to_string(entry.path()) {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        self.add_text(&format!("store/{project}/refine-pending/{name}"), &text);
                    }
                }
            }
        }
    }

    /// Keep the rows in the window and in scope; note the failures and add
    /// the store's points to the timeline.
    fn filter_tables(&mut self, project: &str, mut tables: Value) -> Value {
        let Some(object) = tables.as_object_mut() else {
            return tables;
        };
        for (name, table) in object.iter_mut() {
            let columns = table["columns"]
                .as_array()
                .map(|columns| {
                    columns
                        .iter()
                        .map(|column| column.as_str().unwrap_or_default().to_owned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let column = |row: &[Value], name: &str| -> Option<String> {
                let index = columns.iter().position(|column| column == name)?;
                match row.get(index)? {
                    Value::String(text) => Some(text.clone()),
                    Value::Null => None,
                    other => Some(other.to_string()),
                }
            };
            let Some(rows) = table.get_mut("rows").and_then(Value::as_array_mut) else {
                continue;
            };
            let mut kept = Vec::new();
            for row in rows.drain(..) {
                let Some(cells) = row.as_array() else {
                    continue;
                };
                let id = cells.first().and_then(Value::as_str).unwrap_or_default();
                let ms = harness_types::diag::unix_ms_of_id(id).or_else(|| {
                    column(cells, "updated_at")
                        .or_else(|| column(cells, "created_at"))
                        .or_else(|| column(cells, "claimed_at"))
                        .and_then(|text| sqlite_time_ms(&text))
                });
                if ms.is_some_and(|ms| ms < self.since_ms) && name != "task_leases" {
                    continue;
                }
                let task = column(cells, "task_id");
                if !self.wanted(task.as_deref(), column(cells, "session_id").as_deref()) {
                    continue;
                }
                if let Some(task) = &task
                    && self.options.task.is_none()
                    && name == "sessions"
                {
                    self.conversations.insert(task.clone());
                }
                if let Some(ms) = ms {
                    self.store_point(project, name, cells, &column, ms, task.clone());
                }
                kept.push(row);
            }
            *rows = kept;
        }
        tables
    }

    fn store_point(
        &mut self,
        project: &str,
        table: &str,
        cells: &[Value],
        column: &dyn Fn(&[Value], &str) -> Option<String>,
        ms: i64,
        task: Option<String>,
    ) {
        let source = format!("store:{project}");
        let (level, what, text) = match table {
            "sessions" => (
                "info",
                "session.created",
                format!(
                    "{} ({} events)",
                    column(cells, "session_id").unwrap_or_default(),
                    cells.get(2).and_then(Value::as_i64).unwrap_or_default()
                ),
            ),
            "provider_attempts" => {
                let state = column(cells, "state").unwrap_or_default();
                // A cancel is the user's (Ctrl-C, a new message), not a fault.
                if matches!(state.as_str(), "completed" | "canceled") {
                    return;
                }
                let error = column(cells, "error").unwrap_or_default();
                self.problem(
                    format!("provider attempt {state}: {}", clip(&error, 300)),
                    ms,
                );
                (
                    "warn",
                    "provider.attempt",
                    format!("{state}: {}", clip(&error, 300)),
                )
            }
            "runs" => {
                let state = column(cells, "state").unwrap_or_default();
                let stop = column(cells, "stop_reason").unwrap_or_default();
                // A cancel is the user's, and a pause waits on the user or a
                // bound: neither is a fault.
                if !matches!(
                    state.as_str(),
                    "completed" | "succeeded" | "done" | "canceled" | "cancelled" | "paused"
                ) {
                    self.problem(format!("run ended {state} ({stop})"), ms);
                }
                ("info", "run", format!("{state} {stop}"))
            }
            "context_checkpoints" => (
                "info",
                "checkpoint",
                format!(
                    "revision {} through {}",
                    column(cells, "revision").unwrap_or_default(),
                    column(cells, "through_sequence").unwrap_or_default()
                ),
            ),
            "runtime_commands" => {
                let error = column(cells, "last_error").unwrap_or_default();
                if error.is_empty() || error.starts_with("provider_canceled") {
                    return;
                }
                self.problem(
                    format!(
                        "runtime command {}: {}",
                        column(cells, "state").unwrap_or_default(),
                        clip(&error, 300)
                    ),
                    ms,
                );
                ("warn", "runtime.command", clip(&error, 300))
            }
            _ => return,
        };
        self.points.push(Point {
            ms,
            source,
            level,
            what: what.to_owned(),
            text,
            task,
        });
    }

    /// The harness files of each conversation in scope, and the global ones.
    fn harness_files(&mut self) {
        let global = crate::interactive::harness::global_dir(&self.data_dir);
        for name in [
            "harness_state.json",
            "memory_changes",
            "memory_consolidated_changes",
            "memory_consolidation_failed",
        ] {
            self.copy_file(&global.join(name), &format!("harness/global/{name}"), false);
        }
        let usage = self.data_dir.join("skill-usage");
        if let Ok(entries) = std::fs::read_dir(&usage) {
            for entry in entries.filter_map(Result::ok) {
                let name = entry.file_name().to_string_lossy().into_owned();
                self.copy_file(&entry.path(), &format!("harness/skill-usage/{name}"), true);
            }
        }
        let mut conversations = self
            .conversations
            .iter()
            .filter(|task| is_id(task))
            .map(|task| {
                let dir = self.data_dir.join("sessions").join(task);
                (modified_ms(&dir).unwrap_or_default(), task.clone())
            })
            .collect::<Vec<_>>();
        conversations.sort_by(|a, b| b.cmp(a));
        if conversations.len() > MAX_CONVERSATIONS {
            self.notes.push(format!(
                "harness files of {} older conversation(s) left out",
                conversations.len() - MAX_CONVERSATIONS
            ));
            conversations.truncate(MAX_CONVERSATIONS);
        }
        for (_, task) in conversations {
            let local = crate::interactive::harness::local_dir(&self.data_dir, &task);
            for name in [
                "harness_state.json",
                "task_state.json",
                "review_watermark",
                "refinements.jsonl",
                "memory_history.jsonl",
            ] {
                self.copy_file(&local.join(name), &format!("harness/{task}/{name}"), true);
            }
            let kernel = self.data_dir.join("sessions").join(&task).join("kernel");
            for name in ["kernel-stderr.log", "kernel-state.json"] {
                self.copy_file(
                    &kernel.join(name),
                    &format!("harness/{task}/kernel/{name}"),
                    true,
                );
            }
            let schedules = self.data_dir.join("schedules").join(format!("{task}.json"));
            self.copy_file(&schedules, &format!("harness/{task}/schedules.json"), false);
        }
    }

    fn copy_file(&mut self, source: &Path, path: &str, tail_only: bool) {
        if !source.is_file() {
            return;
        }
        let text = if tail_only {
            tail(source, FILE_TAIL_BYTES)
        } else {
            match std::fs::read_to_string(source) {
                Ok(text) => text,
                Err(_) => return,
            }
        };
        self.add_text(path, &text);
    }

    fn sorted_points(&self) -> Vec<Point> {
        let mut points = self.points.clone();
        points.sort_by_key(|point| point.ms);
        points
    }

    fn problem_count(&self) -> usize {
        self.records
            .iter()
            .filter(|record| matches!(record["level"].as_str(), Some("error" | "warn")))
            .count()
            + self.store_problems.len()
            + self.log_panics.len()
            + self.unclean_exits().len()
    }

    /// Processes whose log starts and never says how they ended.
    fn unclean_exits(&self) -> Vec<String> {
        let mut processes: BTreeMap<(String, u64), (i64, bool, bool)> = BTreeMap::new();
        for record in &self.records {
            let key = (
                record["role"].as_str().unwrap_or("?").to_owned(),
                record["pid"].as_u64().unwrap_or_default(),
            );
            let entry = processes.entry(key).or_insert((0, false, false));
            match (record["kind"].as_str(), record["event"].as_str()) {
                (Some("process"), Some("start")) => {
                    entry.0 = record["ms"].as_i64().unwrap_or_default();
                    entry.1 = true;
                }
                (Some("process"), Some("exit" | "failed")) | (Some("panic"), _) => entry.2 = true,
                _ => {}
            }
        }
        processes
            .into_iter()
            .filter(|(_, (_, started, ended))| *started && !*ended)
            .map(|((role, pid), (ms, _, _))| {
                let last = self
                    .records
                    .iter()
                    .rfind(|record| {
                        record["pid"].as_u64() == Some(pid) && record["role"].as_str() == Some(&role)
                    })
                    .map(|record| {
                        format!(
                            "{} {}.{}",
                            record["ts"].as_str().unwrap_or_default(),
                            record["kind"].as_str().unwrap_or_default(),
                            record["event"].as_str().unwrap_or_default()
                        )
                    })
                    .unwrap_or_default();
                format!(
                    "{role} pid {pid}, started {}: no exit record - still running, or killed or crashed (a stack overflow leaves no panic); its last record: {last}",
                    ms_text(ms)
                )
            })
            .collect()
    }

    fn summary(&self) -> String {
        let mut out = String::new();
        self.summary_header(&mut out);
        self.summary_problems(&mut out);
        self.summary_patterns(&mut out);
        self.summary_activity(&mut out);
        self.summary_children(&mut out);
        self.summary_timeline(&mut out);
        out
    }

    /// What repeats in the window, as prime-agent's incident timeline counts
    /// what recurs: who started the turns, a child that kept waking its
    /// parent, a summary written turn after turn, a tool failing the same way,
    /// the busiest ten minutes. A session where a verifier and its parent
    /// pinged each other every half minute for hours read as many short,
    /// healthy turns.
    fn summary_patterns(&self, out: &mut String) {
        let mut lines = Vec::new();
        let starts = self.recorded("turn", "start");
        let mut by: BTreeMap<String, usize> = BTreeMap::new();
        for record in &starts {
            if let Some(source) = record["fields"]["by"].as_str() {
                *by.entry(source.to_owned()).or_default() += 1;
            }
        }
        if !by.is_empty() {
            let mut by = by.into_iter().collect::<Vec<_>>();
            by.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
            lines.push(format!(
                "turns started by: {}",
                by.iter()
                    .map(|(source, count)| format!("{source} {count}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let mut reports: BTreeMap<String, Vec<i64>> = BTreeMap::new();
        for record in self.recorded("child", "settled") {
            if let Some(child) = record["fields"]["child"].as_str() {
                reports
                    .entry(child.to_owned())
                    .or_default()
                    .push(record["ms"].as_i64().unwrap_or_default());
            }
        }
        for (child, times) in reports.iter().filter(|(_, times)| times.len() >= 3) {
            lines.push(format!(
                "`{child}` reported {} times ({} to {}); each report starts a turn of its parent when it is idle",
                times.len(),
                ms_text(times[0]),
                ms_text(times[times.len() - 1])
            ));
        }
        let written = self
            .recorded("compaction", "earlier_summarized")
            .iter()
            .filter(|record| record["fields"]["model_call"] == true)
            .count();
        let turns = self.recorded("turn", "terminal").len();
        if written >= 3 && written * 3 >= turns {
            lines.push(format!(
                "the earlier conversation was summarized by a model call {written} times in {turns} turns"
            ));
        }
        let mut failures: BTreeMap<(String, String), usize> = BTreeMap::new();
        for record in self.recorded("tool", "failed") {
            let tool = record["fields"]["tool"].as_str().unwrap_or("?").to_owned();
            let message = clip(record["msg"].as_str().unwrap_or_default(), 90);
            *failures.entry((tool, message)).or_default() += 1;
        }
        for ((tool, message), count) in failures.iter().filter(|(_, count)| **count >= 3) {
            lines.push(format!("`{tool}` failed {count} times: {message}"));
        }
        let times = starts
            .iter()
            .filter_map(|record| record["ms"].as_i64())
            .collect::<Vec<_>>();
        let mut busiest = (0, 0);
        let mut from = 0;
        for (index, at) in times.iter().enumerate() {
            while at - times[from] > 10 * 60 * 1000 {
                from += 1;
            }
            if index + 1 - from > busiest.0 {
                busiest = (index + 1 - from, times[from]);
            }
        }
        if busiest.0 >= 10 {
            lines.push(format!(
                "busiest ten minutes: {} turns from {}",
                busiest.0,
                ms_text(busiest.1)
            ));
        }
        let waits = self.recorded("goal", "waits_for_children").len();
        let continued = self.recorded("goal", "continued").len();
        if waits + continued > 0 {
            lines.push(format!(
                "goal: continued {continued} time(s), waited for children {waits} time(s)"
            ));
        }
        if lines.is_empty() {
            return;
        }
        let _ = writeln!(out, "## Patterns\n");
        for line in lines {
            let _ = writeln!(out, "- {line}");
        }
        out.push('\n');
    }

    /// The delegated children: who, on which model, how they ended and why.
    fn summary_children(&self, out: &mut String) {
        if self.children.is_empty() {
            return;
        }
        let failed = self
            .children
            .values()
            .filter(|child| {
                child["error"].is_string()
                    || matches!(
                        child["status"].as_str(),
                        Some("failed" | "error" | "canceled")
                    )
            })
            .count();
        let _ = writeln!(
            out,
            "## Sub-agents\n\n{} child(ren), {failed} failed or stopped. Their ledgers are in `delegation/ledger/`; their records are in the diagnostics and the timeline.\n",
            self.children.len()
        );
        let _ = writeln!(
            out,
            "| child | role | model | status | time | tool calls | error |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---|---|");
        let mut children = self.children.values().collect::<Vec<_>>();
        // The failed first, then by name.
        children.sort_by_key(|child| {
            (
                !child["error"].is_string(),
                child["name"].as_str().unwrap_or_default().to_owned(),
            )
        });
        for child in children {
            let error = child["error"]
                .as_str()
                .map(|error| self.clip_cell(error))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} | {} | {} |",
                child["name"].as_str().unwrap_or("?"),
                child["role"].as_str().unwrap_or("?"),
                child["model"].as_str().unwrap_or("-"),
                child["status"].as_str().unwrap_or("?"),
                child["duration_ms"]
                    .as_u64()
                    .map_or_else(|| "-".to_owned(), |ms| format!("{:.0}s", seconds(ms))),
                child["tool_calls"].as_u64().unwrap_or_default(),
                error,
            );
        }
        let _ = writeln!(out);
    }

    /// A table cell's text: one line, at most 200 characters, redacted.
    fn clip_cell(&self, text: &str) -> String {
        let mut text = learned_redact(text);
        for secret in &self.known_secrets {
            text = text.replace(secret.as_str(), "[REDACTED:known]");
        }
        clip(&text.replace(['\n', '|'], " "), 200)
    }

    /// The records of one `kind.event`, oldest first.
    fn recorded(&self, kind: &str, event: &str) -> Vec<&Value> {
        let mut found = self
            .records
            .iter()
            .filter(|record| record["kind"] == kind && record["event"] == event)
            .collect::<Vec<_>>();
        found.sort_by_key(|record| record["ms"].as_i64().unwrap_or_default());
        found
    }

    /// What the window's work was: its turns, its model calls, its compactions
    /// and its tools, from the records.
    #[allow(
        clippy::too_many_lines,
        reason = "four tables of one section, each a few lines"
    )]
    fn summary_activity(&self, out: &mut String) {
        let field = |record: &Value, name: &str| record["fields"][name].as_u64();
        let turns = self.recorded("turn", "terminal");
        if !turns.is_empty() {
            let _ = writeln!(out, "## Turns\n");
            let _ = writeln!(
                out,
                "| ended | conversation | outcome | steps | tool calls |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|");
            for record in turns.iter().rev().take(30).rev() {
                let _ = writeln!(
                    out,
                    "| {} | `{}` | {} | {} | {} |",
                    ms_text(record["ms"].as_i64().unwrap_or_default()),
                    short_id(record["task"].as_str().unwrap_or("-")),
                    record["fields"]["outcome"].as_str().unwrap_or("?"),
                    field(record, "steps").map_or_else(|| "-".to_owned(), |n| n.to_string()),
                    field(record, "tool_calls").map_or_else(|| "-".to_owned(), |n| n.to_string()),
                );
            }
            if turns.len() > 30 {
                let _ = writeln!(
                    out,
                    "\n{} older turn(s) in `timeline.jsonl`.",
                    turns.len() - 30
                );
            }
            let _ = writeln!(out);
        }
        let calls = self.recorded("provider", "call");
        if !calls.is_empty() {
            let mut durations = calls
                .iter()
                .filter_map(|record| field(record, "duration_ms"))
                .collect::<Vec<_>>();
            durations.sort_unstable();
            let at = |share: usize| {
                durations
                    .get((durations.len().saturating_sub(1)) * share / 100)
                    .copied()
                    .unwrap_or_default()
            };
            let sum = |name: &str| {
                calls
                    .iter()
                    .filter_map(|record| field(record, name))
                    .sum::<u64>()
            };
            let _ = writeln!(out, "## Model calls\n");
            let _ = writeln!(
                out,
                "- {} call(s): {} input token(s) ({} from the cache), {} output token(s)",
                calls.len(),
                sum("input"),
                sum("cached"),
                sum("output")
            );
            let _ = writeln!(
                out,
                "- duration: median {:.1}s, 95th percentile {:.1}s, longest {:.1}s",
                seconds(at(50)),
                seconds(at(95)),
                seconds(durations.last().copied().unwrap_or_default())
            );
            let mut slowest = calls.clone();
            slowest.sort_by_key(|record| std::cmp::Reverse(field(record, "duration_ms")));
            let _ = writeln!(
                out,
                "\n| slowest | model | duration | input | output | tool calls |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|---|");
            for record in slowest.iter().take(5) {
                let _ = writeln!(
                    out,
                    "| {} | {} | {:.1}s | {} | {} | {} |",
                    ms_text(record["ms"].as_i64().unwrap_or_default()),
                    record["fields"]["model"].as_str().unwrap_or("?"),
                    seconds(field(record, "duration_ms").unwrap_or_default()),
                    field(record, "input").map_or_else(|| "-".to_owned(), |n| n.to_string()),
                    field(record, "output").map_or_else(|| "-".to_owned(), |n| n.to_string()),
                    field(record, "tool_calls").unwrap_or_default(),
                );
            }
            let _ = writeln!(out);
        }
        let summarized = self.recorded("compaction", "earlier_summarized");
        let steps = self.recorded("compaction", "turn_steps_summarized").len();
        let shortened = self.recorded("compaction", "tool_results_shortened").len();
        // A long turn shortens its oldest results as it grows; one record a turn.
        let trimmed: u64 = self
            .recorded("compaction", "tool_results_trimmed")
            .iter()
            .filter_map(|record| field(record, "total"))
            .sum();
        let failed = self.recorded("compaction", "earlier_summary_failed").len()
            + self.recorded("compaction", "turn_summary_failed").len();
        if !summarized.is_empty() || steps + shortened + failed > 0 || trimmed > 0 {
            let written = summarized
                .iter()
                .filter(|record| record["fields"]["model_call"] == true)
                .count();
            let read_back = summarized
                .iter()
                .filter(|record| record["fields"]["model_call"] == false)
                .count();
            let _ = writeln!(out, "## Compaction\n");
            let _ = writeln!(
                out,
                "- earlier conversation summarized: {} time(s) - {written} by a model call, {read_back} read back as kept{}",
                summarized.len(),
                if written + read_back < summarized.len() {
                    ", the rest by a build that did not say"
                } else {
                    ""
                }
            );
            let _ = writeln!(out, "- a turn's own steps summarized: {steps}");
            let _ = writeln!(
                out,
                "- tool results shortened: {shortened} time(s) to fit the window, {trimmed} older result(s) as long turns grew"
            );
            let _ = writeln!(out, "- summaries that failed: {failed}\n");
        }
        let mut tools: BTreeMap<String, (usize, usize, u64, u64)> = BTreeMap::new();
        for (event, failed) in [("ok", false), ("failed", true)] {
            for record in self.recorded("tool", event) {
                let name = record["fields"]["tool"].as_str().unwrap_or("?").to_owned();
                let elapsed = field(record, "elapsed_ms").unwrap_or_default();
                let entry = tools.entry(name).or_default();
                entry.0 += 1;
                entry.1 += usize::from(failed);
                entry.2 += elapsed;
                entry.3 = entry.3.max(elapsed);
            }
        }
        if !tools.is_empty() {
            let mut tools = tools.into_iter().collect::<Vec<_>>();
            tools.sort_by_key(|(_, (calls, ..))| std::cmp::Reverse(*calls));
            let _ = writeln!(out, "## Tools\n");
            let _ = writeln!(out, "| tool | calls | failed | total time | longest |");
            let _ = writeln!(out, "|---|---|---|---|---|");
            for (name, (calls, failed, total, longest)) in tools.iter().take(20) {
                let _ = writeln!(
                    out,
                    "| `{name}` | {calls} | {failed} | {:.1}s | {:.1}s |",
                    seconds(*total),
                    seconds(*longest)
                );
            }
            let _ = writeln!(out);
        }
    }

    fn summary_header(&self, out: &mut String) {
        let _ = writeln!(out, "# ha bug report\n");
        let _ = writeln!(
            out,
            "- ha {} (`{}`), {} {}",
            env!("CARGO_PKG_VERSION"),
            crate::interactive::agents::registry::build_identity(),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        let _ = writeln!(out, "- generated {}", ms_text(now_ms()));
        let _ = writeln!(out, "- window: since {}", ms_text(self.since_ms));
        if let Some(task) = &self.options.task {
            let _ = writeln!(out, "- conversation: `{task}`");
        }
        if let Some(session) = &self.options.session {
            let _ = writeln!(out, "- session: `{session}`");
        }
        let _ = writeln!(out, "- data directory: `{}`", self.data_dir.display());
        let _ = writeln!(
            out,
            "- content: {}",
            if self.options.include_content {
                "request bodies, checkpoints and settings included"
            } else {
                "sizes only for requests, checkpoints and long settings (`--include-content` adds them)"
            }
        );
        let _ = writeln!(out, "- secrets redacted: {}", self.redactions);
        for note in &self.notes {
            let _ = writeln!(out, "- note: {note}");
        }
    }

    /// The errors and warnings recorded, by level and `kind.event`: how many,
    /// the first and last time, the latest message.
    fn recorded_problems(&self) -> BTreeMap<(String, String), (usize, String, String, String)> {
        let mut groups: BTreeMap<(String, String), (usize, String, String, String)> =
            BTreeMap::new();
        // Oldest first: the records come file by file, and a group's first and
        // last were the first and last read (first 20:44, last 09:31).
        let mut problems = self
            .records
            .iter()
            .filter(|record| matches!(record["level"].as_str(), Some("error" | "warn")))
            .filter(|record| record["kind"] != "panic" && record["event"] != "died")
            .collect::<Vec<_>>();
        problems.sort_by_key(|record| record["ms"].as_i64().unwrap_or_default());
        for record in problems {
            let key = (
                record["level"].as_str().unwrap_or_default().to_owned(),
                format!(
                    "{}.{}",
                    record["kind"].as_str().unwrap_or_default(),
                    record["event"].as_str().unwrap_or_default()
                ),
            );
            let ts = record["ts"].as_str().unwrap_or_default().to_owned();
            let entry = groups
                .entry(key)
                .or_insert_with(|| (0, ts.clone(), String::new(), String::new()));
            entry.0 += 1;
            entry.2 = ts;
            entry.3 = clip(&record_text(record), 400);
        }
        groups
    }

    fn summary_problems(&self, out: &mut String) {
        let _ = writeln!(out, "\n## Problems\n");
        let mut any = false;
        let panics = self
            .records
            .iter()
            .filter(|record| record["kind"] == "panic" || record["event"] == "died")
            .collect::<Vec<_>>();
        if !panics.is_empty() || !self.log_panics.is_empty() {
            any = true;
            let _ = writeln!(out, "### Crashes\n");
            for (what, (count, ms, logs)) in &self.log_panics {
                let logs = logs
                    .iter()
                    .map(|log| format!("`workers/{log}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(
                    out,
                    "- **panic**, {count} time(s), the last after {}, in {logs}: `{what}`",
                    ms_text(*ms)
                );
            }
            for record in panics.iter().rev() {
                let _ = writeln!(
                    out,
                    "- {} **{}.{}** {} pid {} thread `{}` at `{}`: {}",
                    record["ts"].as_str().unwrap_or_default(),
                    record["kind"].as_str().unwrap_or_default(),
                    record["event"].as_str().unwrap_or_default(),
                    record["role"].as_str().unwrap_or_default(),
                    record["pid"],
                    record["fields"]["thread"]
                        .as_str()
                        .or(record["thread"].as_str())
                        .unwrap_or_default(),
                    record["fields"]["location"].as_str().unwrap_or("-"),
                    clip(record["msg"].as_str().unwrap_or_default(), 400)
                );
            }
            out.push('\n');
        }
        let unclean = self.unclean_exits();
        if !unclean.is_empty() {
            any = true;
            let _ = writeln!(out, "### Processes without an exit record\n");
            for line in &unclean {
                let _ = writeln!(out, "- {line}");
            }
            out.push('\n');
        }
        let groups = self.recorded_problems();
        if !groups.is_empty() {
            any = true;
            let _ = writeln!(out, "### Errors and warnings recorded\n");
            let _ = writeln!(
                out,
                "| level | what | count | first | last | latest message |"
            );
            let _ = writeln!(out, "|---|---|---|---|---|---|");
            let mut rows = groups.into_iter().collect::<Vec<_>>();
            rows.sort_by(|a, b| a.0.0.cmp(&b.0.0).then(b.1.2.cmp(&a.1.2)));
            for ((level, what), (count, first, last, message)) in rows {
                let _ = writeln!(
                    out,
                    "| {level} | `{what}` | {count} | {first} | {last} | {} |",
                    message.replace('|', "\\|").replace('\n', " ")
                );
            }
            out.push('\n');
        }
        if !self.store_problems.is_empty() {
            any = true;
            let _ = writeln!(out, "### From the stores\n");
            let _ = writeln!(out, "| count | first | last | what |");
            let _ = writeln!(out, "|---|---|---|---|");
            let mut rows = self.store_problems.iter().collect::<Vec<_>>();
            rows.sort_by_key(|(_, (_, _, last))| std::cmp::Reverse(*last));
            for (what, (count, first, last)) in rows.into_iter().take(200) {
                let _ = writeln!(
                    out,
                    "| {count} | {} | {} | {} |",
                    ms_text(*first),
                    ms_text(*last),
                    what.replace('|', "\\|").replace('\n', " ")
                );
            }
            out.push('\n');
        }
        if !any {
            let _ = writeln!(
                out,
                "Nothing was recorded as an error or a warning in the window.\n"
            );
        }
    }

    fn summary_timeline(&self, out: &mut String) {
        let points = self.sorted_points();
        let _ = writeln!(
            out,
            "## Timeline\n\nThe newest {} of {} points, oldest first; `timeline.jsonl` has them all.\n",
            points.len().min(SUMMARY_TIMELINE),
            points.len()
        );
        out.push_str("```\n");
        for point in points
            .iter()
            .skip(points.len().saturating_sub(SUMMARY_TIMELINE))
        {
            let _ = writeln!(
                out,
                "{} {:5} {:<28} {:<34} {}{}",
                ms_text(point.ms),
                point.level,
                clip(&point.source, 28),
                clip(&point.what, 34),
                point
                    .task
                    .as_deref()
                    .map(|task| format!("[{}] ", short_id(task)))
                    .unwrap_or_default(),
                clip(&point.text.replace('\n', " "), 220)
            );
        }
        out.push_str("```\n\n");
        let _ = writeln!(out, "## Files\n");
        let _ = writeln!(
            out,
            "- `diagnostics/` - each process's records (`kind`, `event`, ids, `fields`), one JSON object a line"
        );
        let _ = writeln!(
            out,
            "- `workers/` - background workers' logs (panics land here), journals, descriptors without their token"
        );
        let _ = writeln!(
            out,
            "- `store/<project>.json` - runs, steps, provider attempts, leases, checkpoints, settings"
        );
        let _ = writeln!(
            out,
            "- `harness/<task>/` - task state, memory history, applied refinements, review watermark, kernel stderr"
        );
        let _ = writeln!(
            out,
            "- `env.json`, `config/config.toml` - version, platform, `HA_*` variables, configuration (secrets withheld)"
        );
        let _ = writeln!(out, "- `timeline.jsonl` - the merged timeline");
    }

    fn into_zip(mut self) -> Vec<u8> {
        let summary = self.summary();
        // Each point's texts are redacted, never the line: its `ms` was taken
        // for a card number and the line was no longer JSON.
        let points = self.sorted_points();
        let mut lines = Vec::with_capacity(points.len());
        for point in &points {
            let mut value = point_json(point);
            self.redact_value(&mut value);
            lines.push(value.to_string());
        }
        let timeline = lines.join("\n");
        let summary = self.redact(&summary);
        let manifest = json!({
            "schema_version": 1,
            "kind": "ha-bug-report",
            "version": env!("CARGO_PKG_VERSION"),
            "generated": ms_text(now_ms()),
            "since": ms_text(self.since_ms),
            "redactions": self.redactions,
            "files": self.files.iter().map(|(path, bytes)| json!({"path": path, "bytes": bytes.len()})).collect::<Vec<_>>(),
        });
        let mut zip = zip::Writer::default();
        zip.add("SUMMARY.md", summary.as_bytes());
        zip.add("manifest.json", pretty(&manifest).as_bytes());
        zip.add("timeline.jsonl", timeline.as_bytes());
        for (path, bytes) in &self.files {
            zip.add(path, bytes);
        }
        zip.finish()
    }
}

fn point_json(point: &Point) -> Value {
    json!({
        "ms": point.ms,
        "ts": ms_text(point.ms),
        "level": point.level,
        "source": point.source,
        "what": point.what,
        "task": point.task,
        "text": point.text,
    })
}

fn record_text(record: &Value) -> String {
    let message = record["msg"].as_str().unwrap_or_default();
    let fields = record["fields"]
        .as_object()
        .map(|fields| {
            fields
                .iter()
                .filter(|(name, _)| name.as_str() != "backtrace")
                .map(|(name, value)| match value {
                    Value::String(text) => format!("{name}={}", clip(text, 80)),
                    other => format!("{name}={other}"),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    match (message.is_empty(), fields.is_empty()) {
        (true, _) => fields,
        (false, true) => message.to_owned(),
        (false, false) => format!("{message} ({fields})"),
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "a duration shown to a tenth of a second"
)]
fn seconds(ms: u64) -> f64 {
    ms as f64 / 1000.0
}

/// Withhold the value of every secret-named key of a settings file.
fn withhold_secret_values(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (name, field) in fields.iter_mut() {
                if secret_name(name) && !field.is_object() && !field.is_array() {
                    *field = Value::String("[withheld]".to_owned());
                } else {
                    withhold_secret_values(field);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(withhold_secret_values),
        _ => {}
    }
}

fn learned_redact(text: &str) -> String {
    crate::interactive::learned::redact(text)
}

fn level_of(level: Option<&str>) -> &'static str {
    match level {
        Some("error") => "error",
        Some("warn") => "warn",
        Some("debug") => "debug",
        _ => "info",
    }
}

fn ms_text(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map_or_else(
        || ms.to_string(),
        |time| time.format("%Y-%m-%d %H:%M:%S%.3fZ").to_string(),
    )
}

/// `2026-10-20 08:15:02`, as `SQLite`'s `CURRENT_TIMESTAMP` writes (UTC).
fn sqlite_time_ms(text: &str) -> Option<i64> {
    chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|time| time.and_utc().timestamp_millis())
}

fn short_id(id: &str) -> String {
    let (prefix, rest) = id.split_once('_').unwrap_or(("", id));
    let tail = rest.chars().rev().take(8).collect::<Vec<_>>();
    let tail = tail.into_iter().rev().collect::<String>();
    if prefix.is_empty() {
        tail
    } else {
        format!("{prefix}…{tail}")
    }
}

fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut clipped = text
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    clipped.push('…');
    clipped
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// A conversation id is a plain name: nothing that could climb out of the
/// sessions directory.
fn is_id(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

fn secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "key",
        "token",
        "secret",
        "password",
        "authorization",
        "credential",
        "cookie",
    ]
    .iter()
    .any(|marker| name.contains(marker))
}

/// A configuration with each line that sets a secret-named key withheld.
fn withhold_secret_lines(text: &str) -> String {
    text.lines()
        .map(|line| match line.split_once('=') {
            Some((name, _)) if secret_name(name.trim()) => {
                format!("{} = \"[withheld]\"", name.trim_end())
            }
            _ => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn modified_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

fn directory_bytes(path: &Path, depth: usize) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    if depth == 0 || !metadata.is_dir() {
        return 0;
    }
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| directory_bytes(&entry.path(), depth - 1))
                .sum()
        })
        .unwrap_or_default()
}

/// The newest `limit` bytes of a file, from a line start.
fn tail(path: &Path, limit: usize) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    if bytes.len() <= limit {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    let start = bytes.len() - limit;
    let start = bytes[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(start, |offset| start + offset + 1);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// A zip archive written in memory: deflated entries, UTF-8 names. Windows
/// Explorer and every unzip open it; no zip crate is in the tree, and
/// `flate2` already was.
mod zip {
    use std::io::Write as _;

    #[derive(Default)]
    pub struct Writer {
        bytes: Vec<u8>,
        central: Vec<u8>,
        entries: u16,
    }

    impl Writer {
        pub fn add(&mut self, name: &str, contents: &[u8]) {
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            let _ = encoder.write_all(contents);
            let compressed = encoder.finish().unwrap_or_default();
            let crc = crc32fast::hash(contents);
            let (time, date) = dos_time(chrono::Utc::now());
            let offset = u32::try_from(self.bytes.len()).unwrap_or(u32::MAX);
            let compressed_len = u32::try_from(compressed.len()).unwrap_or(u32::MAX);
            let len = u32::try_from(contents.len()).unwrap_or(u32::MAX);
            let name_len = u16::try_from(name.len()).unwrap_or(u16::MAX);
            // Local file header.
            self.bytes.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
            self.bytes.extend_from_slice(&20_u16.to_le_bytes());
            self.bytes.extend_from_slice(&0x0800_u16.to_le_bytes());
            self.bytes.extend_from_slice(&8_u16.to_le_bytes());
            self.bytes.extend_from_slice(&time.to_le_bytes());
            self.bytes.extend_from_slice(&date.to_le_bytes());
            self.bytes.extend_from_slice(&crc.to_le_bytes());
            self.bytes.extend_from_slice(&compressed_len.to_le_bytes());
            self.bytes.extend_from_slice(&len.to_le_bytes());
            self.bytes.extend_from_slice(&name_len.to_le_bytes());
            self.bytes.extend_from_slice(&0_u16.to_le_bytes());
            self.bytes.extend_from_slice(name.as_bytes());
            self.bytes.extend_from_slice(&compressed);
            // Its central directory entry.
            self.central
                .extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
            self.central.extend_from_slice(&20_u16.to_le_bytes());
            self.central.extend_from_slice(&20_u16.to_le_bytes());
            self.central.extend_from_slice(&0x0800_u16.to_le_bytes());
            self.central.extend_from_slice(&8_u16.to_le_bytes());
            self.central.extend_from_slice(&time.to_le_bytes());
            self.central.extend_from_slice(&date.to_le_bytes());
            self.central.extend_from_slice(&crc.to_le_bytes());
            self.central
                .extend_from_slice(&compressed_len.to_le_bytes());
            self.central.extend_from_slice(&len.to_le_bytes());
            self.central.extend_from_slice(&name_len.to_le_bytes());
            self.central.extend_from_slice(&[0; 12]);
            self.central.extend_from_slice(&offset.to_le_bytes());
            self.central.extend_from_slice(name.as_bytes());
            self.entries = self.entries.saturating_add(1);
        }

        pub fn finish(mut self) -> Vec<u8> {
            let offset = u32::try_from(self.bytes.len()).unwrap_or(u32::MAX);
            let size = u32::try_from(self.central.len()).unwrap_or(u32::MAX);
            self.bytes.append(&mut self.central);
            self.bytes.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
            self.bytes.extend_from_slice(&[0; 4]);
            self.bytes.extend_from_slice(&self.entries.to_le_bytes());
            self.bytes.extend_from_slice(&self.entries.to_le_bytes());
            self.bytes.extend_from_slice(&size.to_le_bytes());
            self.bytes.extend_from_slice(&offset.to_le_bytes());
            self.bytes.extend_from_slice(&0_u16.to_le_bytes());
            self.bytes
        }
    }

    fn dos_time(now: chrono::DateTime<chrono::Utc>) -> (u16, u16) {
        use chrono::{Datelike, Timelike};
        let time = (now.hour() << 11) | (now.minute() << 5) | (now.second() / 2);
        let date = ((now.year().clamp(1980, 2107) - 1980).cast_unsigned() << 9)
            | (now.month() << 5)
            | now.day();
        (
            u16::try_from(time).unwrap_or_default(),
            u16::try_from(date).unwrap_or_default(),
        )
    }

    /// The entries of an archive this writer made: for tests.
    #[cfg(test)]
    pub fn read(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        use std::io::Read as _;
        let mut entries = Vec::new();
        let mut at = 0;
        let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        let u32_at = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        while u32_at(at) == 0x0403_4b50 {
            let crc = u32_at(at + 14);
            let compressed = u32_at(at + 18) as usize;
            let name_len = usize::from(u16_at(at + 26));
            let extra = usize::from(u16_at(at + 28));
            let name =
                String::from_utf8(bytes[at + 30..at + 30 + name_len].to_vec()).expect("utf-8 name");
            let start = at + 30 + name_len + extra;
            let mut data = Vec::new();
            flate2::read::DeflateDecoder::new(&bytes[start..start + compressed])
                .read_to_end(&mut data)
                .expect("deflate");
            assert_eq!(crc32fast::hash(&data), crc, "crc of {name}");
            entries.push((name, data));
            at = start + compressed;
        }
        assert_eq!(u32_at(at), 0x0201_4b50, "the central directory follows");
        entries
    }
}

/// `ha bug-report`.
///
/// # Errors
/// The report cannot be gathered or written.
pub async fn run(
    options: Options,
    out: Option<PathBuf>,
    json: bool,
) -> Result<std::process::ExitCode, HarnessError> {
    let out = out.unwrap_or_else(|| {
        PathBuf::from(format!(
            "ha-bug-report-{}.zip",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ))
    });
    let summary = write(options, &out).await?;
    let mut stdout = std::io::stdout().lock();
    if json {
        let _ = writeln!(stdout, "{summary}");
    } else {
        let _ = writeln!(stdout, "bug report: {}", out.display());
        let _ = writeln!(
            stdout,
            "  {} file(s), {} problem(s) recorded, {} secret(s) redacted",
            summary["files"], summary["problems"], summary["redactions"]
        );
        for note in summary["notes"].as_array().into_iter().flatten() {
            let _ = writeln!(stdout, "  note: {}", note.as_str().unwrap_or_default());
        }
        let _ = writeln!(
            stdout,
            "  SUMMARY.md inside lists the problems and the timeline; send the zip as it is"
        );
    }
    Ok(std::process::ExitCode::SUCCESS)
}

/// `ha diag timeline`.
///
/// # Errors
/// The timeline cannot be gathered.
pub async fn print_timeline(
    options: Options,
    level: Option<&str>,
    json: bool,
) -> Result<std::process::ExitCode, HarnessError> {
    let floor = level
        .and_then(harness_types::diag::Level::parse)
        .unwrap_or(harness_types::diag::Level::Debug);
    let points = timeline(options).await?;
    let mut stdout = std::io::stdout().lock();
    for point in points {
        let point_level =
            harness_types::diag::Level::parse(point["level"].as_str().unwrap_or("info"))
                .unwrap_or(harness_types::diag::Level::Info);
        if point_level > floor {
            continue;
        }
        if json {
            let _ = writeln!(stdout, "{point}");
        } else {
            let _ = writeln!(
                stdout,
                "{} {:5} {:<28} {:<34} {}",
                point["ts"].as_str().unwrap_or_default(),
                point["level"].as_str().unwrap_or_default(),
                clip(point["source"].as_str().unwrap_or_default(), 28),
                clip(point["what"].as_str().unwrap_or_default(), 34),
                clip(
                    &point["text"]
                        .as_str()
                        .unwrap_or_default()
                        .replace('\n', " "),
                    220
                )
            );
        }
    }
    Ok(std::process::ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::{parse_window, withhold_secret_lines, zip};

    #[test]
    fn a_zip_reads_back_with_its_names_and_bytes() {
        let mut writer = zip::Writer::default();
        writer.add("SUMMARY.md", b"# ha bug report\n");
        writer.add(
            "diagnostics/a.jsonl",
            "{\"msg\":\"Tiếng Việt\"}\n".repeat(400).as_bytes(),
        );
        let entries = zip::read(&writer.finish());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "SUMMARY.md");
        assert_eq!(entries[0].1, b"# ha bug report\n");
        assert!(String::from_utf8_lossy(&entries[1].1).contains("Tiếng Việt"));
    }

    #[test]
    fn a_window_is_a_number_and_a_unit() {
        assert_eq!(parse_window("90s").unwrap().as_secs(), 90);
        assert_eq!(parse_window("30m").unwrap().as_secs(), 1800);
        assert_eq!(parse_window("2h").unwrap().as_secs(), 7200);
        assert_eq!(parse_window("3d").unwrap().as_secs(), 259_200);
        assert_eq!(parse_window("6").unwrap().as_secs(), 21_600);
        assert!(parse_window("soon").is_err());
    }

    #[test]
    fn a_configuration_keeps_no_secret_value() {
        let text = withhold_secret_lines(
            "model = \"deepseek\"\napi_key = \"sk-live-1234\"\n[providers.x]\nauth_token=\"abc\"",
        );
        assert!(text.contains("model = \"deepseek\""));
        assert!(!text.contains("sk-live-1234"));
        assert!(!text.contains("abc"));
    }
}
