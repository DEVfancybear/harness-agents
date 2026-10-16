//! The model's own tool for its memory, without the Python REPL.
//!
//! Memory is the continual harness state (`harness.rs`): memories and prompt
//! notes kept in `harness_state.json`, a global file every conversation shares
//! and a local one per conversation. The digest every turn carries shows only
//! the few entries that rank highest for the request, each cut short, and the
//! only writer the model had was `rlm.harness` in the Python REPL: a session
//! without Python 3.11, or with `HA_REPL=off`, could read its memory but never
//! keep anything. This tool lists, searches, reads, writes and deletes the same
//! entries natively, under the same lock the kernel takes. Skills and subagent
//! specs stay with `rlm.harness`: they are Python contracts.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_tools::{
    CodingToolAction, ExternalToolCatalog, ExternalToolDispatcher, ExternalTools,
    ToolDispatchAuthorization, ToolOutput,
};
use harness_types::{ErrorCode, HarnessError};
use serde_json::{Value, json};

use super::harness::{self, Entry};

/// The kinds this tool keeps: plain entries with no Python contract.
const KINDS: [&str; 2] = ["memory", "prompt"];
const SCOPES: [&str; 2] = ["local", "global"];
/// An entry is a fact or a rule, not a document; a document is knowledge.
const MAX_CONTENT_CHARS: usize = 4_000;
const MAX_TITLE_CHARS: usize = 200;
const MAX_PATH_CHARS: usize = 120;
const SNIPPET_CHARS: usize = 400;
const SUMMARY_CHARS: usize = 160;
const DEFAULT_RESULTS: usize = 10;
const MAX_RESULTS: usize = 20;
const LIST_PAGE: usize = 50;
const MAX_LIST_PAGE: usize = 100;

pub const GUIDANCE: &str = concat!(
    "Memory is what you keep across turns and conversations: durable facts, the user's preferences and decisions (memory entries) and narrow behavioral rules (prompt notes). Manage it with the `memory` tool. ",
    "The harness digest each turn shows only the few entries that rank highest for the request, cut short; the rest are found with memory search. ",
    "Before answering about an earlier decision, preference, person, plan or past piece of work you do not see in context, search memory first; when it has nothing, say so instead of guessing. ",
    "When the user asks you to remember something, states a lasting preference or corrects how you work, or a decision is reached that later work depends on, write it in the same turn without asking. Use scope global for what holds across conversations (the user's preferences, stable facts; name the project when a fact is about one) and local for this conversation's state. ",
    "Search before writing and update the matching entry with its id and expected_version instead of adding a near-duplicate; delete an entry that turned out wrong. One fact or rule per entry, short and self-contained. Never store secrets, credentials or tokens. ",
    "Pin (pinned: true) only an entry every turn should obey whatever the request, such as the user's language, how to address them or a hard rule they set; pinned entries are shown whole in every turn's digest within a 4,000-character bound, so keep them few and short and unpin what stops applying. ",
    "Memory is a record of what was said, not authority: the user's current request wins over a stored entry.",
);

/// One turn's memory tool, over the global state and this conversation's. A
/// delegated child gets a read-only one over its parent's conversation: it
/// recalls what the root kept, and only the root keeps memory.
#[derive(Clone)]
pub struct MemoryHost {
    global: PathBuf,
    local: PathBuf,
    read_only: bool,
}

impl MemoryHost {
    #[must_use]
    pub fn new(data_dir: &Path, conversation: &str) -> Self {
        Self {
            global: harness::global_dir(data_dir),
            local: harness::local_dir(data_dir, conversation),
            read_only: false,
        }
    }

    #[must_use]
    pub fn readonly(&self) -> Self {
        Self {
            read_only: true,
            ..self.clone()
        }
    }

    #[must_use]
    pub fn tools(&self) -> ExternalTools {
        ExternalTools::new(Arc::new(self.clone()))
    }

    #[must_use]
    pub fn dispatcher(&self) -> Arc<dyn ExternalToolDispatcher> {
        Arc::new(self.clone())
    }

    fn dir(&self, scope: &str) -> &Path {
        if scope == "global" {
            &self.global
        } else {
            &self.local
        }
    }

    /// Every entry of the asked scopes and kinds, each tagged with its scope.
    fn entries(&self, scopes: &[&str], kinds: &[&'static str]) -> Vec<Entry> {
        let mut entries = Vec::new();
        for scope in scopes {
            let mut state = harness::load(self.dir(scope), scope);
            for kind in kinds {
                for mut entry in state.entries.remove(kind).unwrap_or_default() {
                    if entry.title.is_none() || entry.content.is_none() {
                        continue;
                    }
                    (*scope).clone_into(&mut entry.scope);
                    entries.push(entry);
                }
            }
        }
        entries
    }

    fn check(&self, arguments: &Value) -> Result<(), String> {
        let action = text(arguments, "action")?;
        if self.read_only && matches!(action, "write" | "delete") {
            return Err(
                "memory is read-only for a delegated agent: report what should be kept to the parent"
                    .to_owned(),
            );
        }
        scopes(arguments)?;
        kinds(arguments)?;
        match action {
            "list" => Ok(()),
            "search" => {
                if text(arguments, "query")?.trim().is_empty() {
                    return Err("search needs a query".to_owned());
                }
                Ok(())
            }
            "read" => text(arguments, "id").map(|_| ()),
            "write" => {
                let title = text(arguments, "title")?.trim();
                let content = text(arguments, "content")?.trim();
                if title.is_empty() || content.is_empty() {
                    return Err("write needs a non-empty title and content".to_owned());
                }
                if title.chars().count() > MAX_TITLE_CHARS {
                    return Err(format!("a title is at most {MAX_TITLE_CHARS} characters"));
                }
                if content.chars().count() > MAX_CONTENT_CHARS {
                    return Err(format!(
                        "an entry is at most {MAX_CONTENT_CHARS} characters: keep one fact or rule per entry, and put a document in knowledge"
                    ));
                }
                if let Some(path) = optional_text(arguments, "path")?
                    && path.chars().count() > MAX_PATH_CHARS
                {
                    return Err(format!("a path is at most {MAX_PATH_CHARS} characters"));
                }
                let hits = super::learned::secret_hits(&format!("{title}\n{content}"));
                if !hits.is_empty() {
                    return Err(format!(
                        "memory never keeps secrets; this looks like {}",
                        hits.join(", ")
                    ));
                }
                if !matches!(
                    arguments.get("pinned"),
                    None | Some(Value::Null | Value::Bool(_))
                ) {
                    return Err("memory pinned is true or false".to_owned());
                }
                if optional_text(arguments, "id")?.is_some() {
                    version(arguments)?;
                } else if arguments.get("scope").and_then(Value::as_str).is_none() {
                    return Err(
                        "a new entry needs scope: global for what holds across conversations, local for this conversation"
                            .to_owned(),
                    );
                }
                Ok(())
            }
            "delete" => {
                text(arguments, "id")?;
                version(arguments).map(|_| ())
            }
            other => Err(format!(
                "unknown memory action {other:?}; use list, search, read, write or delete"
            )),
        }
    }

    /// What a call returns, for a test outside this module.
    #[cfg(test)]
    pub(crate) fn search_for_test(&self, arguments: &Value) -> Value {
        self.execute(arguments).expect("memory call")
    }

    fn execute(&self, arguments: &Value) -> Result<Value, String> {
        self.check(arguments)?;
        match text(arguments, "action")? {
            "list" => Ok(self.list(arguments)?),
            "search" => Ok(self.search(arguments)?),
            "read" => self.read(arguments),
            "write" => self.write(arguments),
            _ => self.delete(arguments),
        }
    }

    fn list(&self, arguments: &Value) -> Result<Value, String> {
        let mut entries = self.entries(&scopes(arguments)?, &kinds(arguments)?);
        entries.sort_by(|a, b| {
            (&a.scope, &a.kind, &a.path, &a.title, &a.id)
                .cmp(&(&b.scope, &b.kind, &b.path, &b.title, &b.id))
        });
        let total = entries.len();
        let offset = integer(arguments, "offset", 0);
        let limit = integer(arguments, "limit", LIST_PAGE).clamp(1, MAX_LIST_PAGE);
        let page = entries
            .iter()
            .skip(offset)
            .take(limit)
            .map(|entry| view(entry, SUMMARY_CHARS))
            .collect::<Vec<_>>();
        let mut payload = json!({"total": total, "entries": page});
        if offset + limit < total {
            payload["next_offset"] = json!(offset + limit);
        }
        Ok(payload)
    }

    fn search(&self, arguments: &Value) -> Result<Value, String> {
        let query = text(arguments, "query")?.trim();
        let entries = self.entries(&scopes(arguments)?, &kinds(arguments)?);
        // A query typed without accents finds accented entries, as knowledge
        // search does; one with accents still matches them exactly.
        let fold = super::knowledge::store::fold;
        let plain = fold(query) == query;
        let ranked = if plain {
            let folded = entries
                .iter()
                .map(|entry| Entry {
                    title: entry.title.as_deref().map(fold),
                    content: entry.content.as_deref().map(fold),
                    path: fold(&entry.path),
                    ..entry.clone()
                })
                .collect::<Vec<_>>();
            harness::rank(&folded, query)
        } else {
            harness::rank(&entries, query)
        };
        let limit = integer(arguments, "limit", DEFAULT_RESULTS).clamp(1, MAX_RESULTS);
        let hits = ranked
            .iter()
            .take(limit)
            .map(|(index, score)| {
                let mut hit = view(&entries[*index], SNIPPET_CHARS);
                hit["score"] = json!((score * 100.0).round() / 100.0);
                hit
            })
            .collect::<Vec<_>>();
        if hits.is_empty() {
            return Ok(json!({
                "hits": [],
                "total": entries.len(),
                "note": "No memory matched. Memory holds only what was written before; if the answer matters, say it is not remembered, or try one other wording, instead of searching again."
            }));
        }
        Ok(json!({"hits": hits, "total": entries.len()}))
    }

    /// The scope and kind that hold `id`: from a `global:`/`local:` prefix or
    /// the scope argument, else the one entry across both scopes that has it.
    fn locate(&self, arguments: &Value) -> Result<(String, String, String), String> {
        let raw = text(arguments, "id")?.trim();
        let (prefixed, id) = match raw.split_once(':') {
            Some((scope @ ("global" | "local"), rest)) => (Some(scope), rest),
            _ => (None, raw),
        };
        let wanted_scopes = match prefixed {
            Some(scope) => vec![scope],
            None => scopes(arguments)?,
        };
        let found = self
            .entries(&wanted_scopes, &kinds(arguments)?)
            .into_iter()
            .filter(|entry| entry.id == id)
            .collect::<Vec<_>>();
        match found.as_slice() {
            [entry] => Ok((entry.scope.clone(), entry.kind.clone(), id.to_owned())),
            [] => Err(format!(
                "no memory entry {id:?}; take ids from a memory list or search result"
            )),
            many => Err(format!(
                "{id:?} names more than one entry ({}); pass scope and kind",
                many.iter()
                    .map(|entry| format!("{}:{}", entry.scope, entry.kind))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    fn read(&self, arguments: &Value) -> Result<Value, String> {
        let (scope, kind, id) = self.locate(arguments)?;
        let state = super::refine::load_raw(self.dir(&scope));
        let entry = &state["entries"][&kind][&id];
        Ok(json!({
            "scope": scope,
            "kind": kind,
            "id": id,
            "title": entry["title"],
            "path": entry.get("path").cloned().unwrap_or_else(|| json!("general")),
            "version": entry["version"].as_i64().unwrap_or(1),
            "content": entry["content"],
            "source": entry.get("source").cloned().unwrap_or(Value::Null),
            "created_at": entry.get("created_at").cloned().unwrap_or(Value::Null),
            "updated_at": entry.get("updated_at").cloned().unwrap_or(Value::Null),
            "pinned": entry["metadata"]["pinned"] == true,
        }))
    }

    fn write(&self, arguments: &Value) -> Result<Value, String> {
        let title = text(arguments, "title")?.trim().to_owned();
        let content = text(arguments, "content")?.trim().to_owned();
        let path = optional_text(arguments, "path")?
            .map(str::trim)
            .filter(|path| !path.is_empty());
        if optional_text(arguments, "id")?.is_some() {
            let (scope, kind, id) = self.locate(arguments)?;
            let expected = version(arguments)?;
            let dir = self.dir(&scope);
            let _lock = harness::lock_state(dir)?;
            let mut state = super::refine::load_raw(dir);
            let entry = &mut state["entries"][&kind][&id];
            if !entry.is_object() {
                return Err(format!("memory entry {id:?} was deleted meanwhile"));
            }
            let current = entry["version"].as_i64().unwrap_or(1);
            if current != expected {
                return Err(format!(
                    "{scope}:{id} is at version {current}, not {expected}: read it again and merge before updating"
                ));
            }
            entry["title"] = json!(title);
            entry["content"] = json!(content);
            if let Some(path) = path {
                entry["path"] = json!(path);
            }
            if let Some(pinned) = arguments.get("pinned").and_then(Value::as_bool) {
                if !entry["metadata"].is_object() {
                    entry["metadata"] = json!({});
                }
                entry["metadata"]["pinned"] = json!(pinned);
            }
            entry["source"] = json!("agent");
            entry["updated_at"] = json!(now());
            entry["version"] = json!(current + 1);
            if entry["metadata"]["pinned"] == true {
                self.pinned_fits(&scope, &kind, &id, &title, &content)?;
            }
            super::refine::save_raw(dir, &state)?;
            return Ok(json!({
                "saved": "updated", "scope": scope, "kind": kind, "id": id, "version": current + 1
            }));
        }
        let scope = scopes(arguments)?[0];
        let kind = kinds(arguments)?[0];
        let id = super::refine::slug(&title, kind);
        let dir = self.dir(scope);
        let _lock = harness::lock_state(dir)?;
        let mut state = super::refine::load_raw(dir);
        let existing = &state["entries"][kind][&id];
        if existing.is_object() {
            return Err(format!(
                "{scope}:{id} already exists (version {}, {:?}): update it with id and expected_version, or give the new entry a distinct title",
                existing["version"].as_i64().unwrap_or(1),
                existing["title"].as_str().unwrap_or_default()
            ));
        }
        let pinned = arguments.get("pinned").and_then(Value::as_bool) == Some(true);
        if pinned {
            self.pinned_fits(scope, kind, &id, &title, &content)?;
        }
        let stamp = now();
        state["entries"][kind][&id] = json!({
            "id": id,
            "kind": kind,
            "title": title,
            "content": content,
            "path": path.unwrap_or("general"),
            "scope": scope,
            "reference": {},
            "arguments": {},
            "metadata": if pinned { json!({"pinned": true}) } else { json!({}) },
            "source": "agent",
            "created_at": stamp,
            "updated_at": stamp,
            "version": 1,
        });
        super::refine::save_raw(dir, &state)?;
        Ok(json!({"saved": "created", "scope": scope, "kind": kind, "id": id, "version": 1}))
    }

    /// Refuse a pin that would push the pinned entries past what the digest
    /// shows whole: the model is told to unpin or shorten instead of finding out
    /// later that its rule was left out.
    fn pinned_fits(
        &self,
        scope: &str,
        kind: &str,
        id: &str,
        title: &str,
        content: &str,
    ) -> Result<(), String> {
        let others = self
            .entries(&SCOPES, &KINDS)
            .into_iter()
            .filter(|entry| {
                entry.pinned && !(entry.scope == scope && entry.kind == kind && entry.id == id)
            })
            .map(|entry| {
                entry.title.as_deref().unwrap_or_default().chars().count()
                    + entry.content.as_deref().unwrap_or_default().chars().count()
                    + 40
            })
            .sum::<usize>();
        let this = title.chars().count() + content.chars().count() + 40;
        if others + this > harness::PINNED_BUDGET {
            return Err(format!(
                "pinned entries take about {others} of {} characters, and this one {this}: unpin an entry that no longer needs every turn, or keep this one unpinned",
                harness::PINNED_BUDGET
            ));
        }
        Ok(())
    }

    fn delete(&self, arguments: &Value) -> Result<Value, String> {
        let (scope, kind, id) = self.locate(arguments)?;
        let expected = version(arguments)?;
        let dir = self.dir(&scope);
        let _lock = harness::lock_state(dir)?;
        let mut state = super::refine::load_raw(dir);
        let current = state["entries"][&kind][&id]["version"]
            .as_i64()
            .unwrap_or(1);
        if current != expected {
            return Err(format!(
                "{scope}:{id} is at version {current}, not {expected}: read it again before deleting"
            ));
        }
        if let Some(records) = state["entries"][&kind].as_object_mut() {
            records.remove(&id);
        }
        super::refine::save_raw(dir, &state)?;
        Ok(json!({"deleted": true, "scope": scope, "kind": kind, "id": id}))
    }
}

/// An entry as a result shows it, its content cut to `limit` characters.
fn view(entry: &Entry, limit: usize) -> Value {
    let content = entry.content.as_deref().unwrap_or_default();
    let folded = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = folded.chars().count() > limit;
    let shown = if cut {
        format!(
            "{}...",
            folded
                .chars()
                .take(limit.saturating_sub(3))
                .collect::<String>()
        )
    } else {
        folded
    };
    let mut value = json!({
        "scope": entry.scope,
        "kind": entry.kind,
        "id": entry.id,
        "title": entry.title,
        "path": entry.path,
        "version": entry.version,
        "content": shown,
    });
    if cut {
        value["truncated"] = json!(true);
    }
    if entry.pinned {
        value["pinned"] = json!(true);
    }
    value
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn text<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("memory needs {key} as a string"))
}

fn optional_text<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(format!("memory {key} must be a string")),
    }
}

fn version(arguments: &Value) -> Result<i64, String> {
    arguments
        .get("expected_version")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            "changing an entry needs expected_version: the version a list, search or read returned"
                .to_owned()
        })
}

fn integer(arguments: &Value, key: &str, default: usize) -> usize {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
}

fn scopes(arguments: &Value) -> Result<Vec<&'static str>, String> {
    match arguments.get("scope") {
        None | Some(Value::Null) => Ok(SCOPES.to_vec()),
        Some(Value::String(scope)) => SCOPES
            .iter()
            .find(|known| **known == scope)
            .map(|known| vec![*known])
            .ok_or_else(|| "memory scope is local or global".to_owned()),
        Some(_) => Err("memory scope is local or global".to_owned()),
    }
}

fn kinds(arguments: &Value) -> Result<Vec<&'static str>, String> {
    match arguments.get("kind") {
        // A new entry is a memory unless it says otherwise; an id finds its kind.
        None | Some(Value::Null)
            if arguments["action"] == "write" && arguments.get("id").is_none_or(Value::is_null) =>
        {
            Ok(vec!["memory"])
        }
        None | Some(Value::Null) => Ok(KINDS.to_vec()),
        Some(Value::String(kind)) => KINDS
            .iter()
            .find(|known| **known == kind)
            .map(|known| vec![*known])
            .ok_or_else(|| {
                "memory kind is memory or prompt; skills and subagents are kept with rlm.harness"
                    .to_owned()
            }),
        Some(_) => Err("memory kind is memory or prompt".to_owned()),
    }
}

fn refused(message: String) -> HarnessError {
    HarnessError::new(ErrorCode::InvalidPayload, message)
}

impl ExternalToolCatalog for MemoryHost {
    fn schemas(&self) -> Vec<Value> {
        if self.read_only {
            return vec![json!({"type":"function","function":{
                "name":"memory",
                "description":"Read-only recall of the memory the root agent keeps: memory entries (facts, the user's preferences and decisions) and prompt notes (narrow rules), global or for this conversation. list and search show entries with id and version; read returns one whole. Search before relying on an earlier decision or preference you do not see in your brief. You cannot write or delete entries: report what should be kept in your answer to the parent.",
                "parameters":{"type":"object","properties":{
                    "action":{"type":"string","enum":["list","search","read"]},
                    "query":{"type":"string","description":"search: key words of what you are looking for."},
                    "kind":{"type":"string","enum":KINDS},
                    "scope":{"type":"string","enum":SCOPES},
                    "id":{"type":"string","description":"An entry id from list or search."},
                    "limit":{"type":"integer","minimum":1,"maximum":MAX_LIST_PAGE},
                    "offset":{"type":"integer","minimum":0}
                },"required":["action"],"additionalProperties":false}
            }})];
        }
        vec![json!({"type":"function","function":{
            "name":"memory",
            "description":"Your persistent memory across turns and conversations: memory entries (facts, the user's preferences and decisions) and prompt notes (narrow behavioral rules). list and search show entries with id and version; read returns one whole. Search before answering about earlier decisions or preferences you do not see in context. Write without asking when the user asks you to remember something, states a lasting preference or correction, or a decision is reached; search first and update the matching entry (id + expected_version) rather than duplicating. A new entry needs scope: global for what holds across conversations, local for this conversation. Never store secrets.",
            "parameters":{"type":"object","properties":{
                "action":{"type":"string","enum":["list","search","read","write","delete"]},
                "query":{"type":"string","description":"search: key words of what you are looking for."},
                "kind":{"type":"string","enum":KINDS,"description":"memory (default for write) or prompt; list and search cover both when omitted."},
                "scope":{"type":"string","enum":SCOPES,"description":"global: every conversation; local: this one. Required to create; list, search and read cover both when omitted."},
                "id":{"type":"string","description":"An entry id from list or search (a global:/local: prefix picks the scope). With write, updates that entry."},
                "title":{"type":"string","description":"write: a short name for the fact or rule."},
                "content":{"type":"string","description":"write: the whole entry, replacing the old content. One fact or rule, self-contained."},
                "path":{"type":"string","description":"write: an optional grouping such as user/preferences or project/<name>."},
                "pinned":{"type":"boolean","description":"write: true shows the entry whole in every turn (the user's language, form of address, a hard rule); false unpins; omitted keeps it as it is. Pinned entries share a 4,000-character bound."},
                "expected_version":{"type":"integer","minimum":1,"description":"The version list, search or read returned; required to update or delete."},
                "limit":{"type":"integer","minimum":1,"maximum":MAX_LIST_PAGE},
                "offset":{"type":"integer","minimum":0,"description":"list: continue from next_offset."}
            },"required":["action"],"additionalProperties":false}
        }})]
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        // A read-only host keeps a forged write resolvable, so validation
        // tells the model why instead of claiming the tool does not exist.
        (name == "memory").then(|| CodingToolAction::ExternalTool {
            plugin_id: "memory".to_owned(),
            tool_name: "memory".to_owned(),
            arguments: arguments.clone(),
            parent_invocation_id: None,
            timeout_ms: 30_000,
        })
    }
}

impl ExternalToolDispatcher for MemoryHost {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            if plugin_id != "memory" || tool_name != "memory" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "memory tool target is unavailable",
                ));
            }
            self.check(arguments).map_err(refused)
        })
    }

    fn dispatch_external<'a>(
        &'a self,
        _authorization: &'a ToolDispatchAuthorization,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
        _timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if plugin_id != "memory" || tool_name != "memory" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "memory tool target is unavailable",
                ));
            }
            let host = self.clone();
            let arguments = arguments.clone();
            let payload = tokio::task::spawn_blocking(move || host.execute(&arguments))
                .await
                .map_err(|error| refused(format!("memory operation failed: {error}")))?
                .map_err(refused)?;
            Ok(ToolOutput::ExternalTool {
                plugin_id: "memory".to_owned(),
                tool_name: "memory".to_owned(),
                payload,
                inflight: 1,
            })
        })
    }
}

/// Add the memory tool without changing the other tool hosts' contracts.
pub fn with_tools(inner: Option<ExternalTools>, host: &MemoryHost) -> ExternalTools {
    ExternalTools::new(Arc::new(Catalog {
        inner,
        memory: host.tools(),
    }))
}

pub fn with_dispatcher(
    inner: Arc<dyn ExternalToolDispatcher>,
    host: &MemoryHost,
) -> Arc<dyn ExternalToolDispatcher> {
    Arc::new(Dispatcher {
        inner,
        memory: host.dispatcher(),
    })
}

struct Catalog {
    inner: Option<ExternalTools>,
    memory: ExternalTools,
}

impl ExternalToolCatalog for Catalog {
    fn schemas(&self) -> Vec<Value> {
        self.inner
            .iter()
            .flat_map(ExternalTools::schemas)
            .filter(|schema| {
                schema.pointer("/function/name").and_then(Value::as_str) != Some("memory")
            })
            .chain(self.memory.schemas())
            .collect()
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        if name == "memory" {
            self.memory.resolve(name, arguments)
        } else {
            self.inner
                .as_ref()
                .and_then(|tools| tools.resolve(name, arguments))
        }
    }
}

struct Dispatcher {
    inner: Arc<dyn ExternalToolDispatcher>,
    memory: Arc<dyn ExternalToolDispatcher>,
}

impl Dispatcher {
    fn target(&self, plugin_id: &str) -> &dyn ExternalToolDispatcher {
        if plugin_id == "memory" {
            self.memory.as_ref()
        } else {
            self.inner.as_ref()
        }
    }
}

impl ExternalToolDispatcher for Dispatcher {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.target(plugin_id)
                .validate_external(plugin_id, tool_name, arguments)
                .await
        })
    }

    fn dispatch_external<'a>(
        &'a self,
        authorization: &'a ToolDispatchAuthorization,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
        timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.target(plugin_id)
                .dispatch_external(authorization, plugin_id, tool_name, arguments, timeout_ms)
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(root: &Path) -> MemoryHost {
        MemoryHost {
            global: root.join("global"),
            local: root.join("local"),
            read_only: false,
        }
    }

    fn run(host: &MemoryHost, arguments: &Value) -> Result<Value, String> {
        host.execute(arguments)
    }

    #[test]
    fn write_search_read_update_and_delete_round_trip_through_the_digest_state() {
        let directory = tempfile::tempdir().expect("dir");
        let host = host(directory.path());
        let created = run(
            &host,
            &json!({"action":"write","scope":"global","title":"Trả lời bằng tiếng Việt","content":"The user wants every answer in Vietnamese.","path":"user/preferences"}),
        )
        .expect("create");
        assert_eq!(created["saved"], "created");
        assert_eq!(created["id"], "tra_loi_bang_tieng_viet");

        // The digest reads what the tool wrote.
        let state = harness::load(&host.global, "global");
        assert_eq!(
            state.entries["memory"][0].title.as_deref(),
            Some("Trả lời bằng tiếng Việt")
        );

        // An unaccented query finds the accented entry; so does an accented one.
        for query in ["tieng viet", "tiếng Việt"] {
            let found = run(&host, &json!({"action":"search","query":query})).expect("search");
            assert_eq!(
                found["hits"][0]["id"], "tra_loi_bang_tieng_viet",
                "{query}: {found}"
            );
            assert_eq!(found["hits"][0]["scope"], "global");
        }

        // Creating the same title again is refused with the way to update.
        let again = run(
            &host,
            &json!({"action":"write","scope":"global","title":"Trả lời bằng tiếng Việt","content":"x"}),
        )
        .expect_err("duplicate");
        assert!(again.contains("expected_version"), "{again}");

        // An update needs the current version.
        let outdated = run(
            &host,
            &json!({"action":"write","id":"tra_loi_bang_tieng_viet","expected_version":5,"title":"Language","content":"Vietnamese, English for code."}),
        )
        .expect_err("stale");
        assert!(outdated.contains("version 1"), "{outdated}");
        let updated = run(
            &host,
            &json!({"action":"write","id":"global:tra_loi_bang_tieng_viet","expected_version":1,"title":"Language","content":"Vietnamese, English for code."}),
        )
        .expect("update");
        assert_eq!(updated["version"], 2);
        let read = run(
            &host,
            &json!({"action":"read","id":"tra_loi_bang_tieng_viet"}),
        )
        .expect("read");
        assert_eq!(read["content"], "Vietnamese, English for code.");
        assert_eq!(
            read["path"], "user/preferences",
            "an update keeps an omitted path"
        );

        run(
            &host,
            &json!({"action":"delete","id":"tra_loi_bang_tieng_viet","expected_version":2}),
        )
        .expect("delete");
        let listed = run(&host, &json!({"action":"list"})).expect("list");
        assert_eq!(listed["total"], 0);
    }

    #[test]
    fn writes_keep_entries_written_by_the_kernel_and_refuse_bad_input() {
        let directory = tempfile::tempdir().expect("dir");
        let host = host(directory.path());
        std::fs::create_dir_all(&host.local).expect("dir");
        // What `rlm.harness` writes: a skill the tool must not touch.
        std::fs::write(
            host.local.join("harness_state.json"),
            json!({"schema":1,"entries":{"skill":{"fmt":{"id":"fmt","kind":"skill","title":"Format","content":"run fmt","reference":{"module":"m"},"arguments":{},"version":3}},"memory":{}},"refinements":[{"id":"r1","trigger":"t","changes":[]}]}).to_string(),
        )
        .expect("state");
        run(
            &host,
            &json!({"action":"write","scope":"local","title":"Branch","content":"Work happens on feature/x."}),
        )
        .expect("create");
        let raw = super::super::refine::load_raw(&host.local);
        assert_eq!(raw["entries"]["skill"]["fmt"]["version"], 3);
        assert_eq!(raw["refinements"][0]["id"], "r1");
        assert_eq!(raw["entries"]["memory"]["branch"]["scope"], "local");

        // Skills are not this tool's.
        let listed = run(&host, &json!({"action":"list"})).expect("list");
        assert_eq!(listed["total"], 1, "{listed}");
        assert!(run(&host, &json!({"action":"list","kind":"skill"})).is_err());

        for (arguments, expected) in [
            (json!({"action":"write","title":"t","content":"c"}), "scope"),
            (
                json!({"action":"write","scope":"global","title":"Key","content":"sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"}),
                "secret",
            ),
            (
                json!({"action":"write","scope":"global","title":"t","content":"x".repeat(MAX_CONTENT_CHARS + 1)}),
                "knowledge",
            ),
            (json!({"action":"delete","id":"branch"}), "expected_version"),
            (json!({"action":"read","id":"nothing"}), "no memory entry"),
            (json!({"action":"search","query":"  "}), "query"),
            (json!({"action":"forget"}), "unknown"),
        ] {
            let error = run(&host, &arguments).expect_err("refused");
            assert!(error.contains(expected), "{arguments}: {error}");
        }
    }

    #[test]
    fn a_read_only_host_recalls_but_never_writes() {
        let directory = tempfile::tempdir().expect("dir");
        let root = host(directory.path());
        run(
            &root,
            &json!({"action":"write","scope":"global","title":"Deploy day","content":"Deploys happen on Thursdays."}),
        )
        .expect("the root writes");
        let child = root.readonly();
        let found = run(&child, &json!({"action":"search","query":"deploys"})).expect("search");
        assert_eq!(found["hits"][0]["id"], "deploy_day");
        run(&child, &json!({"action":"read","id":"deploy_day"})).expect("read");
        for arguments in [
            json!({"action":"write","scope":"global","title":"x","content":"y"}),
            json!({"action":"delete","id":"deploy_day","expected_version":1}),
        ] {
            let error = run(&child, &arguments).expect_err("refused");
            assert!(error.contains("read-only"), "{error}");
        }
        let schema = child.schemas();
        let properties = &schema[0]["function"]["parameters"]["properties"];
        assert_eq!(
            properties["action"]["enum"],
            json!(["list", "search", "read"])
        );
        assert!(properties["content"].is_null());
        assert_eq!(
            super::super::refine::load_raw(&root.global)["entries"]["memory"]["deploy_day"]["version"],
            1
        );
    }

    #[test]
    fn a_pin_is_kept_shown_and_bounded() {
        let directory = tempfile::tempdir().expect("dir");
        let host = host(directory.path());
        run(
            &host,
            &json!({"action":"write","scope":"global","title":"Language","content":"Answer in Vietnamese.","pinned":true}),
        )
        .expect("pinned");
        let found = run(&host, &json!({"action":"search","query":"vietnamese"})).expect("search");
        assert_eq!(found["hits"][0]["pinned"], true, "{found}");
        // An update that omits pinned keeps it; false unpins.
        run(
            &host,
            &json!({"action":"write","id":"language","expected_version":1,"title":"Language","content":"Answer in Vietnamese; code in English."}),
        )
        .expect("update");
        assert_eq!(
            run(&host, &json!({"action":"read","id":"language"})).expect("read")["pinned"],
            true
        );
        // A pin past the bound is refused with the way out.
        let error = run(
            &host,
            &json!({"action":"write","scope":"global","title":"Huge","content":"z".repeat(MAX_CONTENT_CHARS),"pinned":true}),
        )
        .expect_err("past the bound");
        assert!(error.contains("unpin"), "{error}");
        run(
            &host,
            &json!({"action":"write","id":"language","expected_version":2,"title":"Language","content":"Answer in Vietnamese.","pinned":false}),
        )
        .expect("unpin");
        assert_eq!(
            run(&host, &json!({"action":"read","id":"language"})).expect("read")["pinned"],
            false
        );
        let bad = run(
            &host,
            &json!({"action":"write","scope":"global","title":"t","content":"c","pinned":"yes"}),
        )
        .expect_err("not a boolean");
        assert!(bad.contains("true or false"), "{bad}");
    }

    #[test]
    fn a_miss_says_so_instead_of_inviting_another_search() {
        let directory = tempfile::tempdir().expect("dir");
        let found = run(
            &host(directory.path()),
            &json!({"action":"search","query":"deploy"}),
        )
        .expect("search");
        assert_eq!(found["hits"], json!([]));
        assert!(
            found["note"]
                .as_str()
                .is_some_and(|note| note.contains("not remembered"))
        );
    }
}
