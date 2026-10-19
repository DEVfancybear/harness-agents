//! Diagnostic records: what happened, when, where, and to which conversation.
//!
//! Every crate reports through this module and none of them writes a file: the
//! binary installs a [`Sink`] at start (the CLI's writes a JSONL log under the
//! data directory), and with none installed - tests, other binaries - a record
//! costs a branch and goes nowhere. Records are rare by design: a notice, an
//! error, a compaction decision, a provider retry, a review's outcome. Stream
//! deltas and tool output are never recorded.
//!
//! Before this, what a user saw - "context: compacted ...", "auto-refine:
//! review skipped", an error only the model read - lived in the terminal's
//! memory and was gone when it closed, and a bug report was a screenshot.

use std::cell::RefCell;
use std::sync::OnceLock;

use serde_json::{Map, Value};

/// How much a record matters. Ordered from the most to the least important:
/// a sink set to `Info` keeps `Error`, `Warn` and `Info`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }

    /// `error`, `warn`, `info` or `debug`, in any case.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            _ => None,
        }
    }
}

/// The ids that tie a record to what it is about.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Ids {
    pub agent: Option<String>,
    pub task: Option<String>,
    pub session: Option<String>,
    pub run: Option<String>,
    pub request: Option<String>,
    pub call: Option<String>,
}

/// One diagnostic record.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub level: Level,
    /// The area: `process`, `panic`, `worker`, `agent`, `turn`, `notice`,
    /// `error`, `tool`, `provider`, `compaction`, `review`, `child`, `store`,
    /// `mcp`, `external`, `approval`.
    pub kind: &'static str,
    /// What happened, in a few words joined by `_` (`writer_locked`).
    pub event: &'static str,
    pub message: String,
    pub ids: Ids,
    pub fields: Map<String, Value>,
}

/// Where records go.
pub trait Sink: Send + Sync {
    /// Whether a record of `level` is kept; a record that is not is never built.
    fn enabled(&self, level: Level) -> bool;
    fn write(&self, record: Record);
}

static SINK: OnceLock<Box<dyn Sink>> = OnceLock::new();

thread_local! {
    /// Records captured on this thread instead of reaching the sink: tests.
    static CAPTURED: RefCell<Option<Vec<Record>>> = const { RefCell::new(None) };
}

/// Install the process's sink, once; `false` when one already is.
pub fn install(sink: Box<dyn Sink>) -> bool {
    SINK.set(sink).is_ok()
}

/// Whether a record of `level` would be kept.
#[must_use]
pub fn enabled(level: Level) -> bool {
    CAPTURED.with(|captured| captured.borrow().is_some())
        || SINK.get().is_some_and(|sink| sink.enabled(level))
}

/// The records `f` emits on this thread, captured instead of written: for
/// tests, which share one process and cannot each install a sink.
pub fn capture<F: FnOnce()>(f: F) -> Vec<Record> {
    CAPTURED.with(|captured| *captured.borrow_mut() = Some(Vec::new()));
    f();
    CAPTURED.with(|captured| captured.borrow_mut().take().unwrap_or_default())
}

/// A record being built; [`Builder::emit`] sends it.
#[must_use = "a record is sent by emit()"]
pub struct Builder {
    record: Record,
}

fn builder(level: Level, kind: &'static str, event: &'static str) -> Builder {
    Builder {
        record: Record {
            level,
            kind,
            event,
            message: String::new(),
            ids: Ids::default(),
            fields: Map::new(),
        },
    }
}

pub fn error(kind: &'static str, event: &'static str) -> Builder {
    builder(Level::Error, kind, event)
}

pub fn warn(kind: &'static str, event: &'static str) -> Builder {
    builder(Level::Warn, kind, event)
}

pub fn info(kind: &'static str, event: &'static str) -> Builder {
    builder(Level::Info, kind, event)
}

pub fn debug(kind: &'static str, event: &'static str) -> Builder {
    builder(Level::Debug, kind, event)
}

/// A record of `level`, for a caller that decides the level at run time.
pub fn at(level: Level, kind: &'static str, event: &'static str) -> Builder {
    builder(level, kind, event)
}

impl Builder {
    pub fn msg(mut self, message: impl Into<String>) -> Self {
        self.record.message = message.into();
        self
    }

    pub fn agent(mut self, id: &(impl std::fmt::Display + ?Sized)) -> Self {
        self.record.ids.agent = Some(id.to_string());
        self
    }

    pub fn task(mut self, id: &(impl std::fmt::Display + ?Sized)) -> Self {
        self.record.ids.task = Some(id.to_string());
        self
    }

    pub fn session(mut self, id: &(impl std::fmt::Display + ?Sized)) -> Self {
        self.record.ids.session = Some(id.to_string());
        self
    }

    pub fn run(mut self, id: &(impl std::fmt::Display + ?Sized)) -> Self {
        self.record.ids.run = Some(id.to_string());
        self
    }

    pub fn request(mut self, id: &(impl std::fmt::Display + ?Sized)) -> Self {
        self.record.ids.request = Some(id.to_string());
        self
    }

    pub fn call(mut self, id: &(impl std::fmt::Display + ?Sized)) -> Self {
        self.record.ids.call = Some(id.to_string());
        self
    }

    /// Every id at once, for a caller that keeps them together.
    pub fn ids(mut self, ids: &Ids) -> Self {
        self.record.ids = ids.clone();
        self
    }

    pub fn field(mut self, name: &str, value: impl Into<Value>) -> Self {
        self.record.fields.insert(name.to_owned(), value.into());
        self
    }

    /// Send the record to the sink, or to this thread's capture.
    pub fn emit(self) {
        let record = self.record;
        let captured = CAPTURED.with(|captured| {
            let mut captured = captured.borrow_mut();
            match captured.as_mut() {
                Some(records) => {
                    records.push(record.clone());
                    true
                }
                None => false,
            }
        });
        if captured {
            return;
        }
        if let Some(sink) = SINK.get()
            && sink.enabled(record.level)
        {
            sink.write(record);
        }
    }
}

/// When the entity named by `id` was created, in ms since the epoch: every id
/// ha makes is a `UUIDv7` behind a prefix (`task_01a1…`), and its first 48 bits
/// are that time. `None` for anything else.
#[must_use]
pub fn unix_ms_of_id(id: &str) -> Option<i64> {
    let uuid = id.rsplit('_').next()?;
    let hex = uuid.replace('-', "");
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if hex.as_bytes()[12] != b'7' {
        return None;
    }
    i64::from_str_radix(&hex[..12], 16).ok()
}

#[cfg(test)]
mod tests {
    use super::{Level, capture, info, unix_ms_of_id, warn};

    #[test]
    fn records_are_captured_with_their_ids_and_fields() {
        let records = capture(|| {
            info("compaction", "decision")
                .msg("summarized 12 messages")
                .task("task_1")
                .session("session_2")
                .field("before", 147_088)
                .emit();
            warn("store", "writer_locked").emit();
        });
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].kind, "compaction");
        assert_eq!(records[0].ids.task.as_deref(), Some("task_1"));
        assert_eq!(records[0].fields["before"], 147_088);
        assert_eq!(records[1].level, Level::Warn);
    }

    #[test]
    fn without_a_sink_a_record_goes_nowhere() {
        info("notice", "shown").msg("nothing listens").emit();
        assert!(capture(|| {}).is_empty());
    }

    #[test]
    fn levels_order_and_parse() {
        assert!(Level::Error < Level::Info);
        assert_eq!(Level::parse("WARNING"), Some(Level::Warn));
        assert_eq!(Level::parse("loud"), None);
    }

    #[test]
    fn a_uuid_v7_id_says_when_it_was_made() {
        // 0x01a13d6f385f ms = 2026-10-15T ...
        let ms = unix_ms_of_id("task_01a13d6f-385f-7390-a32a-c2a3326523b3").expect("v7");
        assert_eq!(ms, 0x01a1_3d6f_385f);
        assert!(
            unix_ms_of_id("task_01a13d6f-385f-4390-a32a-c2a3326523b3").is_none(),
            "not v7"
        );
        assert!(unix_ms_of_id("project-74d0cfa253fa6e71").is_none());
    }
}
