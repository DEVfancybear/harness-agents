//! Work a read-only call needs, started while the model is still streaming
//! the rest of its response.
//!
//! A response that asks for three searches and a read streams them one after
//! another; the batch only starts when the stream ends. Here each call's work
//! starts as soon as its arguments are complete: a search runs and keeps its
//! result, a listing or glob takes the walk, a read pulls the file into the
//! operating system's cache. When the call then crosses the gate - approval,
//! intent, receipt, all unchanged and in order - its dispatch finds the work
//! done.
//!
//! A kept result is only handed out while nothing in the workspace changed
//! since it was computed (the shared generation of [`crate::walk`]), so it is
//! exactly what the dispatch would have computed itself.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use serde_json::Value;

use crate::{
    walk::{ensure_watched, generation},
    workspace::{SearchOutput, resolve_relative, tool_walk},
};

/// Most prefetches running at once; more wait for the dispatch.
const MAX_RUNNING: usize = 4;

/// How long a kept search result is offered when no watcher vouches for the
/// workspace.
const UNWATCHED_TTL: Duration = Duration::from_secs(1);

/// How long a kept search result is offered at all.
const WATCHED_TTL: Duration = Duration::from_secs(30);

static RUNNING: AtomicUsize = AtomicUsize::new(0);

struct Kept {
    output: SearchOutput,
    generation: u64,
    at: Instant,
    watched: bool,
}

fn kept() -> &'static Mutex<HashMap<String, Kept>> {
    static KEPT: OnceLock<Mutex<HashMap<String, Kept>>> = OnceLock::new();
    KEPT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The key of one search: everything its result depends on.
pub(crate) fn search_key(root: &Path, arguments: &[&dyn std::fmt::Debug]) -> String {
    format!("{}|{arguments:?}", root.display())
}

/// A kept result of the search `key`, if nothing changed since it was made.
pub(crate) fn kept_search(key: &str) -> Option<SearchOutput> {
    let kept = kept().lock().ok()?;
    let entry = kept.get(key)?;
    let ttl = if entry.watched {
        WATCHED_TTL
    } else {
        UNWATCHED_TTL
    };
    (entry.generation == generation() && entry.at.elapsed() < ttl).then(|| entry.output.clone())
}

/// Keep a search result computed at generation `stamp`.
pub(crate) fn keep_search(key: String, output: &SearchOutput, stamp: u64, watched: bool) {
    let Ok(mut kept) = kept().lock() else {
        return;
    };
    if kept.len() >= 32 {
        let now = generation();
        kept.retain(|_, entry| entry.generation == now && entry.at.elapsed() < WATCHED_TTL);
        if kept.len() >= 32 {
            kept.clear();
        }
    }
    kept.insert(
        key,
        Kept {
            output: output.clone(),
            generation: stamp,
            at: Instant::now(),
            watched,
        },
    );
}

/// Start the work of one streamed call, if it is a read worth starting early.
/// Never fails and never blocks: anything it cannot do is left to the
/// dispatch.
pub(crate) fn prefetch(root: &Path, hashline: bool, name: &str, arguments: &str) {
    if !matches!(name, "search_text" | "glob" | "list_files" | "read_file") {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(arguments) else {
        return;
    };
    if RUNNING.fetch_add(1, Ordering::SeqCst) >= MAX_RUNNING {
        RUNNING.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    let root = root.to_owned();
    let name = name.to_owned();
    let spawned = std::thread::Builder::new()
        .name("ha-prefetch".to_owned())
        .spawn(move || {
            run(&root, hashline, &name, &value);
            RUNNING.fetch_sub(1, Ordering::SeqCst);
        });
    if spawned.is_err() {
        RUNNING.fetch_sub(1, Ordering::SeqCst);
    }
}

fn text<'v>(value: &'v Value, key: &str) -> Option<&'v str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
}

fn run(root: &Path, hashline: bool, name: &str, value: &Value) {
    let _ = ensure_watched(root);
    match name {
        "read_file" => {
            let paths = value
                .get("paths")
                .and_then(Value::as_array)
                .map(|paths| paths.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .unwrap_or_default();
            for path in text(value, "path").into_iter().chain(paths) {
                if let Ok(target) = resolve_relative(root, path, false) {
                    let _ = std::fs::read(target);
                }
            }
        }
        "glob" | "list_files" => {
            let _ = tool_walk(root, text(value, "path"), name);
        }
        "search_text" => {
            let Some(query) = text(value, "query") else {
                return;
            };
            let flag = |key: &str| value.get(key).and_then(Value::as_bool).unwrap_or(false);
            let context = value
                .get("context_lines")
                .and_then(Value::as_u64)
                .and_then(|lines| u32::try_from(lines).ok())
                .map_or(0, |lines| {
                    lines.min(crate::contracts::SEARCH_CONTEXT_MAX_LINES)
                });
            let _ = crate::workspace::search_text(
                root,
                query,
                text(value, "path"),
                flag("regex"),
                flag("case_insensitive"),
                text(value, "glob"),
                context,
                hashline,
            );
        }
        _ => {}
    }
}

/// Follows one model call's stream and prefetches each tool call once its
/// arguments are complete: when the next call starts, or the stream ends.
pub(crate) struct StreamPrefetcher {
    root: PathBuf,
    hashline: bool,
    current: Mutex<Option<(String, String, String)>>,
}

impl StreamPrefetcher {
    pub(crate) fn new(root: &Path, hashline: bool) -> Option<Arc<Self>> {
        if std::env::var_os("HA_PREFETCH").is_some_and(|value| value == "off") {
            return None;
        }
        let root = std::fs::canonicalize(root).ok()?;
        Some(Arc::new(Self {
            root,
            hashline,
            current: Mutex::new(None),
        }))
    }

    /// A tool-call delta arrived.
    pub(crate) fn delta(&self, call_id: &str, name: &str, arguments: &str) {
        let finished = {
            let Ok(mut current) = self.current.lock() else {
                return;
            };
            match current.as_mut() {
                Some((id, call_name, text)) if id == call_id => {
                    if !name.is_empty() {
                        name.clone_into(call_name);
                    }
                    text.push_str(arguments);
                    None
                }
                _ => current.replace((call_id.to_owned(), name.to_owned(), arguments.to_owned())),
            }
        };
        if let Some((_, name, arguments)) = finished {
            prefetch(&self.root, self.hashline, &name, &arguments);
        }
    }

    /// The stream ended: its last call is complete.
    pub(crate) fn finish(&self) {
        let last = self
            .current
            .lock()
            .ok()
            .and_then(|mut current| current.take());
        if let Some((_, name, arguments)) = last {
            prefetch(&self.root, self.hashline, &name, &arguments);
        }
    }

    /// A retried attempt streams its calls again from the start.
    pub(crate) fn restart(&self) {
        if let Ok(mut current) = self.current.lock() {
            *current = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_search_is_offered_only_until_something_changes() {
        let root = std::env::temp_dir();
        let key = search_key(&root, &[&"needle", &false]);
        let output = SearchOutput {
            matches: Vec::new(),
            truncated: false,
        };
        keep_search(key.clone(), &output, generation(), true);
        assert!(kept_search(&key).is_some());
        crate::walk::note_change();
        assert!(kept_search(&key).is_none(), "a change retires it");
    }
}
