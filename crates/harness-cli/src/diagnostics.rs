//! The process's diagnostic log: [`harness_types::diag`] records, one JSON
//! object per line, in `<data_dir>/diagnostics/<YYYYMMDD>-<role>-<pid>.jsonl`.
//!
//! Each process writes its own file (the app, a background worker, its
//! supervisor and `ha exec` run at once, and one file each needs no
//! cross-process rotation); `ha bug-report` merges them by time. Secrets are
//! redacted as records are written, a file rolls over at [`FILE_BYTES`], a
//! process past [`PROCESS_BYTES`] keeps only errors and warnings, and files
//! older than [`KEEP_DAYS`] days or past [`DIRECTORY_BYTES`] in all are
//! pruned when a process starts. `HA_DIAG` sets the level (`off`, `error`,
//! `warn`, `info` - the default - or `debug`) and `HA_DIAG_DIR` the directory.
//!
//! A short command (`ha send`, `ha list`, ...) opens no file unless it has a
//! warning or an error to say, so scripted use does not litter the directory.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use harness_types::diag::{self, Level, Record, Sink};
use serde_json::{Map, Value, json};

/// The directory under the data directory.
pub const DIRECTORY: &str = "diagnostics";
/// A file rolls over past this.
const FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Past this, a process keeps only errors and warnings.
const PROCESS_BYTES: u64 = 24 * 1024 * 1024;
/// Past this, a process writes nothing more.
const PROCESS_HARD_BYTES: u64 = 32 * 1024 * 1024;
/// Files older than this are pruned.
const KEEP_DAYS: u64 = 14;
/// The directory is pruned, oldest first, down to this.
const DIRECTORY_BYTES: u64 = 128 * 1024 * 1024;
/// The longest a message is.
const MESSAGE_CHARS: usize = 4_096;
/// The longest a string field is; a panic's backtrace gets more.
const FIELD_CHARS: usize = 2_048;
const BACKTRACE_CHARS: usize = 16_384;

/// What the process is, from its command line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Tui,
    Exec,
    Rpc,
    Acp,
    Worker,
    Supervisor,
    Cli,
}

impl Role {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Exec => "exec",
            Self::Rpc => "rpc",
            Self::Acp => "acp",
            Self::Worker => "worker",
            Self::Supervisor => "supervisor",
            Self::Cli => "cli",
        }
    }

    /// A process that runs turns or agents, whose start and end are worth a
    /// record even when nothing goes wrong.
    const fn long_lived(self) -> bool {
        !matches!(self, Self::Cli)
    }
}

struct Output {
    file: Option<File>,
    path: PathBuf,
    part: u32,
    file_bytes: u64,
    process_bytes: u64,
    budget_said: bool,
    /// The process's start record, written with the first record after it:
    /// a process that only starts and exits - a refused launch, `ha list` -
    /// leaves no file, as it leaves no other state.
    held: Option<String>,
}

/// The file sink of one process.
pub struct FileSink {
    dir: PathBuf,
    role: Role,
    pid: u32,
    build: String,
    level: Level,
    day: String,
    seq: AtomicU64,
    output: Mutex<Output>,
}

impl FileSink {
    fn new(dir: PathBuf, role: Role, level: Level) -> Self {
        let pid = std::process::id();
        let day = chrono::Utc::now().format("%Y%m%d").to_string();
        let path = dir.join(format!("{day}-{}-{pid}.jsonl", role.as_str()));
        Self {
            dir,
            role,
            pid,
            build: crate::interactive::agents::registry::build_identity(),
            level,
            day,
            seq: AtomicU64::new(0),
            output: Mutex::new(Output {
                file: None,
                path,
                part: 1,
                file_bytes: 0,
                process_bytes: 0,
                budget_said: false,
                held: None,
            }),
        }
    }

    /// The line a record is written as.
    fn line(&self, record: &Record) -> String {
        let now = chrono::Utc::now();
        let mut object = Map::new();
        object.insert("v".to_owned(), json!(1));
        object.insert(
            "ts".to_owned(),
            json!(now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        );
        object.insert("ms".to_owned(), json!(now.timestamp_millis()));
        object.insert(
            "seq".to_owned(),
            json!(self.seq.fetch_add(1, Ordering::SeqCst)),
        );
        object.insert("pid".to_owned(), json!(self.pid));
        object.insert("role".to_owned(), json!(self.role.as_str()));
        object.insert("build".to_owned(), json!(self.build));
        object.insert(
            "thread".to_owned(),
            json!(std::thread::current().name().unwrap_or("unnamed")),
        );
        object.insert("level".to_owned(), json!(record.level.as_str()));
        object.insert("kind".to_owned(), json!(record.kind));
        object.insert("event".to_owned(), json!(record.event));
        object.insert(
            "msg".to_owned(),
            json!(clip(&redact(&record.message), MESSAGE_CHARS)),
        );
        let ids = &record.ids;
        // An agent's thread is named after it: a record made there is the
        // agent's even when its maker did not know the id.
        if ids.agent.is_none()
            && let Some(agent) = std::thread::current()
                .name()
                .and_then(|name| name.strip_prefix("agent-"))
        {
            object.insert("agent".to_owned(), json!(agent));
        }
        for (name, value) in [
            ("agent", &ids.agent),
            ("task", &ids.task),
            ("session", &ids.session),
            ("run", &ids.run),
            ("request", &ids.request),
            ("call", &ids.call),
        ] {
            if let Some(value) = value {
                object.insert(name.to_owned(), json!(value));
            }
        }
        let limit = if record.kind == "panic" {
            BACKTRACE_CHARS
        } else {
            FIELD_CHARS
        };
        let fields = record
            .fields
            .iter()
            .map(|(name, value)| (name.clone(), scrub(name, value, limit)))
            .collect::<Map<_, _>>();
        if !fields.is_empty() {
            object.insert("fields".to_owned(), Value::Object(fields));
        }
        let mut line = Value::Object(object).to_string();
        line.push('\n');
        line
    }

    fn append(&self, output: &mut Output, line: &str) {
        let bytes = line.len() as u64;
        if output.file.is_none() || output.file_bytes + bytes > FILE_BYTES {
            if output.file.is_some() {
                output.part += 1;
                output.path = self.dir.join(format!(
                    "{}-{}-{}-{}.jsonl",
                    self.day,
                    self.role.as_str(),
                    self.pid,
                    output.part
                ));
            }
            let _ = std::fs::create_dir_all(&self.dir);
            output.file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&output.path)
                .ok();
            output.file_bytes = output
                .file
                .as_ref()
                .and_then(|file| file.metadata().ok())
                .map_or(0, |metadata| metadata.len());
        }
        if let Some(file) = output.file.as_mut()
            && file.write_all(line.as_bytes()).is_ok()
        {
            let _ = file.flush();
            output.file_bytes += bytes;
            output.process_bytes += bytes;
        }
    }

    /// Write a record no matter who holds the lock: a panic can happen while
    /// another thread writes, and waiting on it would hang the panic.
    fn write_panic(&self, record: &Record) {
        let line = self.line(record);
        if let Ok(mut output) = self.output.try_lock() {
            if let Some(held) = output.held.take() {
                self.append(&mut output, &held);
            }
            self.append(&mut output, &line);
            return;
        }
        let path = self.dir.join(format!(
            "{}-{}-{}-panic.jsonl",
            self.day,
            self.role.as_str(),
            self.pid
        ));
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

impl Sink for FileSink {
    fn enabled(&self, level: Level) -> bool {
        level <= self.level
    }

    fn write(&self, record: Record) {
        if !self.enabled(record.level) {
            return;
        }
        let line = self.line(&record);
        let Ok(mut output) = self.output.lock() else {
            return;
        };
        match (record.kind, record.event) {
            ("process", "start") => {
                output.held = Some(line);
                return;
            }
            // A process that ended - even failing - having done nothing
            // else: its error was printed, and it touched no state.
            ("process", "exit" | "failed") if output.held.take().is_some() => return,
            _ => {}
        }
        if let Some(held) = output.held.take() {
            self.append(&mut output, &held);
        }
        if output.process_bytes >= PROCESS_HARD_BYTES {
            return;
        }
        if output.process_bytes >= PROCESS_BYTES && record.level > Level::Warn {
            if !output.budget_said {
                output.budget_said = true;
                let note = self.line(&Record {
                    level: Level::Warn,
                    kind: "process",
                    event: "diag_budget_reached",
                    message: format!(
                        "this process wrote {PROCESS_BYTES} bytes of diagnostics; only errors and warnings are kept from here"
                    ),
                    ids: diag::Ids::default(),
                    fields: Map::new(),
                });
                self.append(&mut output, &note);
            }
            return;
        }
        self.append(&mut output, &line);
    }
}

static SINK: std::sync::OnceLock<&'static FileSink> = std::sync::OnceLock::new();

/// A forwarding sink, so the panic hook can reach the same file.
struct Forward(&'static FileSink);

impl Sink for Forward {
    fn enabled(&self, level: Level) -> bool {
        self.0.enabled(level)
    }

    fn write(&self, record: Record) {
        self.0.write(record);
    }
}

/// Start the process's log: prune old files, install the sink and the panic
/// hook, and record the start. Nothing happens with `HA_DIAG=off` or without
/// a data directory.
pub fn init(role: Role) {
    diag::set_process(role.as_str());
    let environment = crate::interactive::paths::LaunchEnvironment::capture();
    let level = match std::env::var("HA_DIAG") {
        Ok(value) if value.trim().eq_ignore_ascii_case("off") => return,
        Ok(value) => Level::parse(&value).unwrap_or(Level::Info),
        Err(_) => Level::Info,
    };
    let dir = match std::env::var_os("HA_DIAG_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => match crate::interactive::paths::resolve(&crate::interactive::paths::PathRequest {
            platform: crate::interactive::paths::HostPlatform::current(),
            environment: &environment,
            explicit_data_dir: None,
        }) {
            Ok(paths) => paths.data_dir.join(DIRECTORY),
            Err(_) => return,
        },
    };
    if role.long_lived() {
        prune(&dir, std::time::SystemTime::now());
    }
    let sink: &'static FileSink = Box::leak(Box::new(FileSink::new(dir, role, level)));
    if SINK.set(sink).is_err() || !diag::install(Box::new(Forward(sink))) {
        return;
    }
    install_panic_hook();
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    // The flags' names only: a value, or the first word, can be the user's
    // prompt (`ha "fix the build"`).
    let flags = arguments
        .iter()
        .filter_map(|argument| argument.strip_prefix("--"))
        .map(|flag| flag.split('=').next().unwrap_or_default().to_owned())
        .collect::<Vec<_>>();
    diag::at(
        if role.long_lived() {
            Level::Info
        } else {
            Level::Debug
        },
        "process",
        "start",
    )
    .field("flags", flags)
    .field("arguments", arguments.len())
    .field(
        "cwd",
        std::env::current_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default(),
    )
    .emit();
}

/// Record how the process ends.
pub fn finish(error: Option<&harness_types::HarnessError>, role: Role) {
    match error {
        Some(error) => diag::error("process", "failed")
            .msg(error.to_string())
            .field("code", error.code().as_str())
            .field("exit_code", error.exit_code())
            .emit(),
        None => diag::at(
            if role.long_lived() {
                Level::Info
            } else {
                Level::Debug
            },
            "process",
            "exit",
        )
        .emit(),
    }
}

/// Record a panic - thread, place, message, backtrace - then let the hook
/// before this one (the terminal's restore, the default report) run.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(sink) = SINK.get() {
            let thread = std::thread::current();
            let name = thread.name().unwrap_or("unnamed").to_owned();
            let payload = info
                .payload()
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "(non-text panic payload)".to_owned());
            let mut fields = Map::new();
            fields.insert("thread".to_owned(), json!(name));
            if let Some(location) = info.location() {
                fields.insert(
                    "location".to_owned(),
                    json!(format!("{}:{}", location.file(), location.line())),
                );
            }
            fields.insert(
                "backtrace".to_owned(),
                json!(std::backtrace::Backtrace::force_capture().to_string()),
            );
            let mut ids = diag::Ids::default();
            if let Some(agent) = name.strip_prefix("agent-") {
                ids.agent = Some(agent.to_owned());
            }
            sink.write_panic(&Record {
                level: Level::Error,
                kind: "panic",
                event: "panicked",
                message: payload,
                ids,
                fields,
            });
        }
        previous(info);
    }));
}

/// Where the diagnostics log of a data directory is: `HA_DIAG_DIR` when set.
#[must_use]
pub fn location(data_dir: &Path) -> PathBuf {
    std::env::var_os("HA_DIAG_DIR")
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| data_dir.join(DIRECTORY), PathBuf::from)
}

/// The diagnostics directory this process writes to, when it has one.
#[must_use]
pub fn directory() -> Option<PathBuf> {
    SINK.get().map(|sink| sink.dir.clone())
}

/// Delete files older than [`KEEP_DAYS`] days, then the oldest until the
/// directory holds at most [`DIRECTORY_BYTES`].
fn prune(dir: &Path, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            Some((entry.path(), metadata.modified().ok()?, metadata.len()))
        })
        .collect::<Vec<_>>();
    let keep = std::time::Duration::from_secs(KEEP_DAYS * 24 * 60 * 60);
    files.retain(|(path, modified, _)| {
        let old = now.duration_since(*modified).is_ok_and(|age| age > keep);
        if old {
            let _ = std::fs::remove_file(path);
        }
        !old
    });
    files.sort_by_key(|(_, modified, _)| *modified);
    let mut total = files.iter().map(|(_, _, size)| size).sum::<u64>();
    for (path, _, size) in files {
        if total <= DIRECTORY_BYTES {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

/// `text` with the secret shapes ha knows replaced.
fn redact(text: &str) -> String {
    crate::interactive::learned::redact(text)
}

/// A field's value, redacted and clipped; a field named like a secret is withheld.
fn scrub(name: &str, value: &Value, limit: usize) -> Value {
    let lowered = name.to_ascii_lowercase();
    if [
        "key",
        "token",
        "secret",
        "password",
        "authorization",
        "credential",
    ]
    .iter()
    .any(|word| lowered.contains(word))
    {
        return json!("[withheld]");
    }
    match value {
        Value::String(text) => json!(clip(&redact(text), limit)),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| scrub("", item, limit)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| (key.clone(), scrub(key, item, limit)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut cut = text.chars().take(limit).collect::<String>();
    let _ = write!(cut, "… [{} more chars]", text.chars().count() - limit);
    cut
}

#[cfg(test)]
mod tests {
    use super::{FileSink, Level, Role, Sink, prune, scrub};
    use harness_types::diag::{Ids, Record};
    use serde_json::{Map, json};

    fn record(level: Level, kind: &'static str, message: &str) -> Record {
        Record {
            level,
            kind,
            event: "test",
            message: message.to_owned(),
            ids: Ids {
                task: Some("task_1".to_owned()),
                ..Ids::default()
            },
            fields: Map::new(),
        }
    }

    fn lines(dir: &std::path::Path) -> Vec<serde_json::Value> {
        let mut all = Vec::new();
        for entry in std::fs::read_dir(dir).expect("dir") {
            let text = std::fs::read_to_string(entry.expect("entry").path()).expect("file");
            all.extend(
                text.lines()
                    .map(|line| serde_json::from_str(line).expect("json")),
            );
        }
        all
    }

    #[test]
    fn a_record_is_one_json_line_with_its_ids_and_no_secret() {
        let dir = tempfile::tempdir().expect("dir");
        let sink = FileSink::new(dir.path().to_path_buf(), Role::Worker, Level::Info);
        let mut with_fields = record(
            Level::Error,
            "store",
            "the key sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA was refused",
        );
        with_fields
            .fields
            .insert("api_key".to_owned(), json!("visible?"));
        with_fields
            .fields
            .insert("holder".to_owned(), json!("worker 42"));
        sink.write(with_fields);
        sink.write(record(Level::Debug, "notice", "below the level"));
        let written = lines(dir.path());
        assert_eq!(written.len(), 1, "{written:?}");
        let line = &written[0];
        assert_eq!(line["kind"], "store");
        assert_eq!(line["task"], "task_1");
        assert_eq!(line["role"], "worker");
        assert!(
            !line["msg"].as_str().unwrap_or_default().contains("sk-ant"),
            "{line}"
        );
        assert_eq!(line["fields"]["api_key"], "[withheld]");
        assert_eq!(line["fields"]["holder"], "worker 42");
    }

    /// A refused launch must leave no state: a process that only starts and
    /// exits writes no file, and one that does more writes its start first.
    #[test]
    fn a_process_that_only_starts_and_exits_leaves_no_file() {
        let event = |kind, event| Record {
            event,
            ..record(Level::Info, kind, "")
        };
        let dir = tempfile::tempdir().expect("dir");
        let quiet = FileSink::new(dir.path().join("quiet"), Role::Tui, Level::Info);
        quiet.write(event("process", "start"));
        quiet.write(event("process", "failed"));
        assert!(!dir.path().join("quiet").exists(), "no file, no directory");
        let busy = FileSink::new(dir.path().join("busy"), Role::Tui, Level::Info);
        busy.write(event("process", "start"));
        busy.write(event("store", "writer_locked"));
        busy.write(event("process", "exit"));
        let events = lines(&dir.path().join("busy"))
            .iter()
            .map(|line| line["event"].as_str().unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(events, ["start", "writer_locked", "exit"]);
    }

    #[test]
    fn a_full_file_rolls_over_and_a_loud_process_keeps_only_errors() {
        let dir = tempfile::tempdir().expect("dir");
        let sink = FileSink::new(dir.path().to_path_buf(), Role::Exec, Level::Debug);
        let big = "x".repeat(3_000);
        // Past the per-process budget: about 8,000 records of 3 KiB.
        for _ in 0..12_000 {
            let mut item = record(Level::Info, "notice", "");
            item.fields.insert("blob".to_owned(), json!(big.clone()));
            sink.write(item);
        }
        sink.write(record(Level::Error, "store", "still kept"));
        let files = std::fs::read_dir(dir.path()).expect("dir").count();
        assert!(files >= 3, "rolled over into {files} files");
        let written = lines(dir.path());
        assert!(
            written
                .iter()
                .any(|line| line["event"] == "diag_budget_reached")
        );
        assert!(written.iter().any(|line| line["msg"] == "still kept"));
    }

    #[test]
    fn old_files_are_pruned() {
        let dir = tempfile::tempdir().expect("dir");
        let old = dir.path().join("20200101-tui-1.jsonl");
        std::fs::write(&old, "{}\n").expect("old");
        let fresh = dir.path().join("20260101-tui-2.jsonl");
        std::fs::write(&fresh, "{}\n").expect("fresh");
        let later = std::time::SystemTime::now() + std::time::Duration::from_hours(480);
        std::fs::File::options()
            .write(true)
            .open(&fresh)
            .expect("open")
            .set_modified(later)
            .expect("touch");
        prune(dir.path(), later);
        assert!(!old.exists());
        assert!(fresh.exists());
    }

    #[test]
    fn nested_fields_are_scrubbed() {
        let value = scrub(
            "detail",
            &json!({"token": "abc", "note": "Bearer abcdefghijklmnopqrstuvwxyz0123"}),
            100,
        );
        assert_eq!(value["token"], "[withheld]");
        assert!(
            !value["note"]
                .as_str()
                .unwrap_or_default()
                .contains("abcdefghijklmnop"),
            "{value}"
        );
    }
}
