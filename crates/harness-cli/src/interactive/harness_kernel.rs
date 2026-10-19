//! The host side of the kernel's `rlm.harness`, ported from prime-agent's
//! `rlm/harness.py`.
//!
//! prime-agent's kernel keeps the harness state itself: `rlm.harness` read and
//! rewrote `harness_state.json` under its own lock, in its own process, while
//! ha's `memory` tool and `/refine` wrote the same file under the host's. Two
//! writers with two validators: a kernel save dropped every field the kernel
//! did not know, and a lock it changed to a directory would have shut out the
//! host's file lock. Here the kernel sends one blocking `harness` host request
//! per call, and every read-modify-write happens in this process under
//! [`super::harness::lock_state`], with one validator. The factory spec dry
//! run stays in the kernel, beside `factory.py`.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde_json::{Map, Value, json};
use unicode_normalization::UnicodeNormalization as _;

/// The host request type the kernel's `rlm.harness` sends.
pub const REQUEST: &str = "harness";

const STATE_FILE: &str = "harness_state.json";
const KINDS: [&str; 5] = ["prompt", "memory", "skill", "subagent", "factory"];
/// The longest id a title gives, cut on a word.
const SLUG_MAX: usize = 60;
/// The longest id the earlier id scheme gave.
const LEGACY_SLUG_MAX: usize = 80;
const OVERVIEW_SUMMARY: usize = 120;

/// The memory directories one kernel is pointed at.
#[derive(Clone, Debug)]
pub struct Dirs {
    pub global: PathBuf,
    pub local: PathBuf,
    /// ha's `settings.json`, where `harnessRefinements` can turn the harness
    /// off.
    pub settings: Option<PathBuf>,
}

/// Answer one `harness` request.
///
/// # Errors
/// The request is malformed, an entry is invalid, or the state cannot be
/// written; the message is what the kernel raises.
pub fn answer(dirs: &Dirs, request: &Value) -> Result<Value, String> {
    let op = request["op"]
        .as_str()
        .ok_or("harness request needs an op")?;
    // prime-agent's master switch, read on every call as its kernel does.
    if let Some(settings) = &dirs.settings
        && !super::config::harness_refinements(settings)
    {
        return Err(super::config::REFINEMENTS_DISABLED.to_owned());
    }
    let global = request["global"].as_bool().unwrap_or(false);
    let dir = match request.get("state_dir").and_then(Value::as_str) {
        Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ if global => dirs.global.clone(),
        _ => dirs.local.clone(),
    };
    let store = Store {
        dir,
        stores: (dirs.global.clone(), dirs.local.clone()),
        scope: if global { "global" } else { "local" },
        // A conversation's local store is `sessions/<conversation>/harness`.
        origin: json!({
            "conversation": dirs.local.parent().and_then(|dir| dir.file_name()).map(|name| name.to_string_lossy().into_owned()),
            "via": "rlm.harness",
        }),
    };
    match op {
        "list" => Ok(Value::Array(store.list(optional_kind(request)?)?)),
        "get" => store.get(&kind(request)?, &text_field(request, "id")?),
        "create" | "update" | "upsert" => store.write(op, request),
        "delete" => store.delete(&kind(request)?, &text_field(request, "id")?),
        "record_refinement" => store.record_refinement(request),
        "search" => store.search(request).map(Value::Array),
        "overview" => store.overview(request).map(Value::String),
        "snapshot" => Ok(store.snapshot()),
        other => Err(format!("unknown harness operation {other:?}")),
    }
}

struct Store {
    dir: PathBuf,
    /// The global and local stores, which a new entry is compared with.
    stores: (PathBuf, PathBuf),
    scope: &'static str,
    /// Who writes, recorded on the entry and in the memory journal.
    origin: Value,
}

impl Store {
    fn file(&self) -> PathBuf {
        self.dir.join(STATE_FILE)
    }

    /// The raw state; unknown fields are kept through a save.
    fn raw(&self) -> Value {
        let mut state = std::fs::read_to_string(self.file())
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({"schema": 1}));
        if !state["entries"].is_object() {
            state["entries"] = json!({});
        }
        if !state["refinements"].is_array() {
            state["refinements"] = json!([]);
        }
        state
    }

    /// `HarnessState.load`'s reading of one kind: entries without a string
    /// title and content are skipped, every other field gets its default.
    fn entries(&self, state: &Value, kind: &str) -> Vec<Value> {
        let Some(records) = state["entries"][kind].as_object() else {
            return Vec::new();
        };
        records
            .iter()
            .filter_map(|(id, raw)| normalize(kind, id, raw, self.scope))
            .collect()
    }

    fn find(&self, state: &Value, kind: &str, id: &str) -> Option<Value> {
        normalize(kind, id, &state["entries"][kind][id], self.scope)
    }

    fn list(&self, kind: Option<String>) -> Result<Vec<Value>, String> {
        let state = self.raw();
        let kinds = match kind {
            Some(kind) => vec![known_kind(&kind)?],
            None => KINDS.to_vec(),
        };
        let mut records = kinds
            .into_iter()
            .flat_map(|kind| self.entries(&state, kind))
            .collect::<Vec<_>>();
        records.sort_by(|a, b| {
            let key = |entry: &Value| {
                ["kind", "path", "title", "id"]
                    .map(|field| entry[field].as_str().unwrap_or("").to_owned())
            };
            key(a).cmp(&key(b))
        });
        Ok(records)
    }

    fn get(&self, kind: &str, id: &str) -> Result<Value, String> {
        let kind = known_kind(kind)?;
        Ok(self.find(&self.raw(), kind, id).unwrap_or(Value::Null))
    }

    fn delete(&self, kind: &str, id: &str) -> Result<Value, String> {
        let kind = known_kind(kind)?;
        let _lock = super::harness::lock_state(&self.dir)?;
        let mut state = self.raw();
        if self.find(&state, kind, id).is_none() {
            return Ok(json!(false));
        }
        // What was removed stays in the memory journal (`memory` history).
        super::memory::journal(
            &self.dir,
            "delete",
            kind,
            id,
            &state["entries"][kind][id],
            &self.origin,
        )?;
        if let Some(records) = state["entries"][kind].as_object_mut() {
            records.remove(id);
        }
        self.save(&state)?;
        Ok(json!(true))
    }

    /// `create`, `update` and `upsert`: `_upsert` behind each one's own check.
    #[allow(
        clippy::too_many_lines,
        reason = "_upsert behind each operation's own check, as the kernel told it"
    )]
    fn write(&self, op: &str, request: &Value) -> Result<Value, String> {
        let kind = known_kind(&kind(request)?)?;
        let title = &request["title"];
        let id = present(request, "id");
        let name = describe(id, title);
        require_text(kind, &name, "title", title)?;
        if let Some(id) = id {
            require_text(kind, &name, "id", id)?;
        }
        let _lock = super::harness::lock_state(&self.dir)?;
        let mut state = self.raw();
        let records = state["entries"][kind].as_object();
        let entry_id = match id.and_then(Value::as_str) {
            Some(id) => id.to_owned(),
            None => title_id(kind, title.as_str().unwrap_or_default(), records),
        };
        let existing = self.find(&state, kind, &entry_id);
        match (op, &existing) {
            ("create", Some(_)) => {
                return Err(format!("{kind} entry {} already exists", repr(&entry_id)));
            }
            ("update", None) => {
                return Err(format!("{kind} entry {} does not exist", repr(&entry_id)));
            }
            _ => {}
        }
        let path = present(request, "path");
        let reference = present(request, "reference");
        let arguments = present(request, "arguments");
        let metadata = present(request, "metadata");
        let agent = json!("agent");
        let source = present(request, "source").unwrap_or(&agent);
        validate_shape(
            kind,
            &entry_id,
            title,
            &request["content"],
            Shape {
                path,
                reference,
                arguments,
                metadata,
                source,
                existing: existing.is_some(),
            },
        )?;
        self.passes_memory_checks(kind, &entry_id, existing.as_ref(), request)?;
        let now = now();
        if existing.is_some() {
            super::memory::journal(
                &self.dir,
                "update",
                kind,
                &entry_id,
                &state["entries"][kind][&entry_id],
                &self.origin,
            )?;
        }
        let entry = match existing {
            Some(mut entry) => {
                entry["title"] = title.clone();
                entry["content"] = request["content"].clone();
                // An omitted field (None) keeps what the entry has, so a
                // title-only update never resets a skill's contract; an
                // explicit value, `{}` included, replaces it.
                for (field, value) in [
                    ("path", path),
                    ("reference", reference),
                    ("arguments", arguments),
                    ("metadata", metadata),
                ] {
                    if let Some(value) = value {
                        entry[field] = value.clone();
                    }
                }
                entry["source"] = source.clone();
                entry["updated_at"] = json!(now);
                entry["version"] = json!(entry["version"].as_i64().unwrap_or(1) + 1);
                entry
            }
            None => json!({
                "id": entry_id,
                "kind": kind,
                "title": title,
                "content": request["content"],
                "path": path.cloned().unwrap_or_else(|| json!("general")),
                "scope": self.scope,
                "reference": reference.cloned().unwrap_or_else(|| json!({})),
                "arguments": arguments.cloned().unwrap_or_else(|| json!({})),
                "metadata": metadata.cloned().unwrap_or_else(|| json!({})),
                "source": source,
                "created_at": now,
                "updated_at": now,
                "version": 1,
            }),
        };
        if !state["entries"][kind].is_object() {
            state["entries"][kind] = json!({});
        }
        // Merged over the stored record, so a field the host keeps beside
        // these (`metadata` aside, which is the entry's own) survives.
        let stored = &mut state["entries"][kind][&entry_id];
        if !stored.is_object() {
            *stored = json!({});
        }
        if let (Some(stored), Some(fields)) = (stored.as_object_mut(), entry.as_object()) {
            for (field, value) in fields {
                stored.insert(field.clone(), value.clone());
            }
            stored.insert("origin".to_owned(), self.origin.clone());
        }
        self.save(&state)?;
        Ok(entry)
    }

    /// The checks the `memory` tool makes, made for a kernel write too: a
    /// secret is never kept, an update names the version it read when it says
    /// one, and a new memory or prompt note that repeats a kept one is refused.
    /// `rlm.harness` used to skip them all, and its duplicates crowded the six
    /// entries a turn's digest shows.
    fn passes_memory_checks(
        &self,
        kind: &'static str,
        id: &str,
        existing: Option<&Value>,
        request: &Value,
    ) -> Result<(), String> {
        let title = request["title"].as_str().unwrap_or_default();
        let content = request["content"].as_str().unwrap_or_default();
        let hits = super::learned::secret_hits(&format!("{title}\n{content}"));
        if !hits.is_empty() {
            return Err(format!(
                "the harness never keeps secrets; this looks like {}",
                hits.join(", ")
            ));
        }
        if let Some(expected) = present(request, "expected_version") {
            let expected = expected
                .as_i64()
                .ok_or("expected_version is the version you read, a number")?;
            let current = existing.map_or(0, |entry| entry["version"].as_i64().unwrap_or(1));
            if current != expected {
                return Err(format!(
                    "{kind} entry {} is at version {current}, not {expected}: read it again before you update it",
                    repr(id)
                ));
            }
        }
        if existing.is_none() && matches!(kind, "memory" | "prompt") {
            super::memory::MemoryHost::for_stores(&self.stores.0, &self.stores.1)
                .refuse_duplicate(self.scope, kind, title, content, request)?;
        }
        Ok(())
    }

    fn record_refinement(&self, request: &Value) -> Result<Value, String> {
        let trigger = &request["trigger"];
        let changes = &request["changes"];
        let empty = json!("");
        let evidence = present(request, "evidence").unwrap_or(&empty);
        let outcome = present(request, "outcome").unwrap_or(&empty);
        validate_refinement(trigger, changes, evidence, outcome)?;
        let id = present(request, "id");
        if let Some(id) = id
            && id.as_str().is_none_or(str::is_empty)
        {
            return Err(format!(
                "refinement event rejected: id must be a non-empty string when provided, got {}",
                type_name(id)
            ));
        }
        let _lock = super::harness::lock_state(&self.dir)?;
        let mut state = self.raw();
        let count = state["refinements"].as_array().map_or(0, Vec::len);
        let changes = match changes {
            Value::String(change) => vec![json!(change)],
            other => other.as_array().cloned().unwrap_or_default(),
        };
        let event = json!({
            "id": id.and_then(Value::as_str).map_or_else(|| format!("refine_{:04}", count + 1), str::to_owned),
            "trigger": trigger,
            "changes": changes,
            "evidence": evidence,
            "outcome": outcome,
            "created_at": now(),
        });
        if let Some(events) = state["refinements"].as_array_mut() {
            events.push(event.clone());
        }
        self.save(&state)?;
        Ok(event)
    }

    /// Weighted term overlap with the query, each term discounted by its
    /// document frequency; ties go to the entry written last.
    fn search(&self, request: &Value) -> Result<Vec<Value>, String> {
        let query = request["query"]
            .as_str()
            .ok_or_else(|| format!("query must be str, got {}", type_name(&request["query"])))?;
        let limit = match request.get("limit") {
            None | Some(Value::Null) => 10,
            Some(value) => value
                .as_u64()
                .filter(|limit| *limit >= 1)
                .and_then(|limit| usize::try_from(limit).ok())
                .ok_or("limit must be a positive int")?,
        };
        let terms = super::harness::query_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let entries = self.list(optional_kind(request)?)?;
        let fields = entries
            .iter()
            .map(|entry| {
                [
                    searchable(entry["title"].as_str().unwrap_or_default()),
                    searchable(entry["content"].as_str().unwrap_or_default()),
                    searchable(&format!(
                        "{} {}",
                        entry["path"].as_str().unwrap_or_default(),
                        entry["id"].as_str().unwrap_or_default()
                    )),
                ]
            })
            .collect::<Vec<_>>();
        #[allow(clippy::cast_precision_loss, reason = "entry counts are small")]
        let documents = entries.len() as f64;
        let weights = terms
            .iter()
            .filter_map(|term| {
                let matches = fields
                    .iter()
                    .filter(|fields| fields.iter().any(|field| field.contains(term.as_str())))
                    .count();
                #[allow(clippy::cast_precision_loss, reason = "small counts")]
                let matches = matches as f64;
                (matches > 0.0).then(|| {
                    (
                        term.as_str(),
                        ((documents - matches + 0.5) / (matches + 0.5)).ln_1p(),
                    )
                })
            })
            .collect::<Vec<_>>();
        let mut scored = entries
            .into_iter()
            .zip(&fields)
            .map(|(entry, fields)| {
                let score = weights
                    .iter()
                    .map(|(term, weight)| {
                        let hits = fields.iter().filter(|field| field.contains(term)).count();
                        if hits == 0 {
                            return 0.0;
                        }
                        #[allow(clippy::cast_precision_loss, reason = "at most three fields")]
                        let spread = 1.0 + (hits as f64 - 1.0) * 0.5;
                        weight * spread
                    })
                    .sum::<f64>();
                (score, entry)
            })
            .filter(|(score, _)| *score > 0.0)
            .collect::<Vec<_>>();
        scored.sort_by(|(a, first), (b, second)| {
            b.total_cmp(a).then_with(|| {
                second["updated_at"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(first["updated_at"].as_str().unwrap_or_default())
            })
        });
        Ok(scored
            .into_iter()
            .take(limit)
            .map(|(_, entry)| entry)
            .collect())
    }

    fn overview(&self, request: &Value) -> Result<String, String> {
        let most = match request.get("max_entries_per_kind") {
            None | Some(Value::Null) => 20,
            Some(value) => value
                .as_u64()
                .and_then(|most| usize::try_from(most).ok())
                .ok_or("max_entries_per_kind must be an int")?,
        };
        let state = self.raw();
        let mut lines = vec![
            format!("Harness state ({}): {}", self.scope, self.file().display()),
            "Call contract: installed Python skills use await <skill_import>(...) or a matching shell CLI; \
             harness skill entries are Python REPL skills and must include a Python reference plus arguments. \
             Spawn a subagent spec by composing a concise task prompt and calling \
             handle = await rlm.spawn('sub-task', name='worker'); admission returns immediately with rlm_child_id, name, session_dir, \
             and model, never the child's answer. Results arrive only through explicit agent_message replies or \
             files; children reply with await agent_message.send(message, receiver_role='parent'). Use \
             await rlm.list_subagents() to recover direct child handles and await agent_message.send(..., \
             receiver_role='child', receiver_name=handle.name) for follow-ups."
                .to_owned(),
        ];
        for kind in KINDS {
            let records = self.list(Some(kind.to_owned()))?;
            lines.push(format!("{kind}: {}", records.len()));
            for entry in records.iter().take(most) {
                let summary = cut(&entry["content"]
                    .as_str()
                    .unwrap_or_default()
                    .trim()
                    .replace('\n', " "));
                let mut extra = String::new();
                if kind == "skill" {
                    for (field, label) in [("reference", "ref"), ("arguments", "args")] {
                        if entry[field]
                            .as_object()
                            .is_some_and(|value| !value.is_empty())
                        {
                            let _ = write!(extra, " {label}={}", cut(&python_json(&entry[field])));
                        }
                    }
                }
                lines.push(format!(
                    "  - [{}:{}] {} ({}, v{}){extra}: {summary}",
                    entry["scope"].as_str().unwrap_or(self.scope),
                    entry["id"].as_str().unwrap_or_default(),
                    entry["title"].as_str().unwrap_or_default(),
                    entry["path"].as_str().unwrap_or_default(),
                    entry["version"].as_i64().unwrap_or(1),
                ));
            }
            if records.len() > most {
                lines.push(format!("  - +{} more", records.len() - most));
            }
        }
        let events = refinements(&state);
        if events.is_empty() {
            lines.push("refinements: 0".to_owned());
        } else {
            lines.push(format!("refinements: {}", events.len()));
            for event in events.iter().skip(events.len().saturating_sub(5)) {
                let changes = event["changes"]
                    .as_array()
                    .map(|changes| {
                        changes
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                lines.push(format!(
                    "  - [{}] {}: {changes}",
                    event["id"].as_str().unwrap_or_default(),
                    event["trigger"].as_str().unwrap_or_default()
                ));
            }
        }
        Ok(lines.join("\n"))
    }

    fn snapshot(&self) -> Value {
        let state = self.raw();
        let mut entries = Map::new();
        for kind in KINDS {
            let records = self
                .entries(&state, kind)
                .into_iter()
                .map(|entry| (entry["id"].as_str().unwrap_or_default().to_owned(), entry))
                .collect::<Map<_, _>>();
            entries.insert(kind.to_owned(), Value::Object(records));
        }
        json!({
            "file_path": self.file().display().to_string(),
            "scope": self.scope,
            "entries": entries,
            "refinements": refinements(&state),
        })
    }

    fn save(&self, state: &Value) -> Result<(), String> {
        super::refine::save_raw(&self.dir, state).map(|_| ())
    }
}

/// One stored record as `HarnessState.load` reads it, or `None` when it has
/// no string title and content.
fn normalize(kind: &str, id: &str, raw: &Value, scope: &str) -> Option<Value> {
    let record = raw.as_object()?;
    let title = record.get("title")?.as_str()?;
    let content = record.get("content")?.as_str()?;
    let text = |field: &str| record.get(field).and_then(Value::as_str);
    let object = |field: &str| {
        record
            .get(field)
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}))
    };
    let version = match record.get("version") {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(1),
        Some(Value::String(number)) => number.parse().unwrap_or(1),
        _ => 1,
    };
    let now = now();
    Some(json!({
        "id": id,
        "kind": kind,
        "title": title,
        "content": content,
        // State written while the grouping was named `topic` has no `path`.
        "path": text("path").or_else(|| text("topic")).unwrap_or("general"),
        "scope": match text("scope") {
            Some(scope @ ("local" | "global")) => scope,
            _ => scope,
        },
        "reference": object("reference"),
        "arguments": object("arguments"),
        "metadata": object("metadata"),
        "source": text("source").unwrap_or("agent"),
        "created_at": text("created_at").unwrap_or(&now),
        "updated_at": text("updated_at").unwrap_or(&now),
        "version": version,
    }))
}

fn refinements(state: &Value) -> Vec<Value> {
    state["refinements"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| {
                    let id = event["id"].as_str()?;
                    let trigger = event["trigger"].as_str()?;
                    let changes = match &event["changes"] {
                        Value::String(change) => vec![change.clone()],
                        Value::Array(changes) => changes
                            .iter()
                            .map(|change| {
                                change
                                    .as_str()
                                    .map_or_else(|| change.to_string(), str::to_owned)
                            })
                            .collect(),
                        _ => return None,
                    };
                    let text = |field: &str| event[field].as_str().unwrap_or_default().to_owned();
                    Some(json!({
                        "id": id,
                        "trigger": trigger,
                        "changes": changes,
                        "evidence": text("evidence"),
                        "outcome": text("outcome"),
                        "created_at": event["created_at"].as_str().map_or_else(now, str::to_owned),
                    }))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}

/// A field the caller passed, `None` (Python's) counting as left out.
fn present<'a>(request: &'a Value, field: &str) -> Option<&'a Value> {
    request.get(field).filter(|value| !value.is_null())
}

fn kind(request: &Value) -> Result<String, String> {
    request["kind"].as_str().map(str::to_owned).ok_or_else(|| {
        format!(
            "unknown harness kind {}; expected one of {}",
            python_repr(&request["kind"]),
            kinds_tuple()
        )
    })
}

fn optional_kind(request: &Value) -> Result<Option<String>, String> {
    match present(request, "kind") {
        None => Ok(None),
        Some(_) => kind(request).map(Some),
    }
}

fn known_kind(kind: &str) -> Result<&'static str, String> {
    KINDS
        .iter()
        .find(|known| **known == kind)
        .copied()
        .ok_or_else(|| {
            format!(
                "unknown harness kind {}; expected one of {}",
                repr(kind),
                kinds_tuple()
            )
        })
}

fn kinds_tuple() -> String {
    format!(
        "({})",
        KINDS
            .iter()
            .map(|kind| repr(kind))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn text_field(request: &Value, field: &str) -> Result<String, String> {
    request[field].as_str().map(str::to_owned).ok_or_else(|| {
        format!(
            "{field} must be a string, got {}",
            type_name(&request[field])
        )
    })
}

/// Python's `repr` of a string, as the kernel's messages always quoted it.
fn repr(text: &str) -> String {
    if text.contains('\'') && !text.contains('"') {
        format!("\"{text}\"")
    } else {
        format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

fn python_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::String(text) => repr(text),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        other => other.to_string(),
    }
}

/// `_type_name`: what a rejected value was, in Python's words.
fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Array(_) => "a list",
        Value::String(text) if text.is_empty() => "an empty string",
        Value::String(_) => "str",
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(number) if number.is_f64() => "float",
        Value::Number(_) => "int",
        Value::Object(_) => "dict",
    }
}

/// `_describe_entry`: the best name for an entry in a rejection.
fn describe(id: Option<&Value>, title: &Value) -> String {
    id.and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .or_else(|| title.as_str().filter(|title| !title.is_empty()))
        .unwrap_or("<unnamed>")
        .to_owned()
}

fn require_text(kind: &str, name: &str, field: &str, value: &Value) -> Result<(), String> {
    if value.as_str().is_some_and(|text| !text.is_empty()) {
        return Ok(());
    }
    Err(format!(
        "{kind} entry {} rejected: {field} must be a non-empty string, got {}",
        repr(name),
        type_name(value)
    ))
}

#[derive(Clone, Copy)]
struct Shape<'a> {
    path: Option<&'a Value>,
    reference: Option<&'a Value>,
    arguments: Option<&'a Value>,
    metadata: Option<&'a Value>,
    source: &'a Value,
    existing: bool,
}

/// `_validate_entry_shape`: an invalid entry is refused before anything is
/// written, naming the entry and the field, instead of being saved and later
/// breaking the digest every turn renders.
fn validate_shape(
    kind: &str,
    id: &str,
    title: &Value,
    content: &Value,
    shape: Shape<'_>,
) -> Result<(), String> {
    let name = describe(Some(&json!(id)), title);
    require_text(kind, &name, "id", &json!(id))?;
    require_text(kind, &name, "title", title)?;
    require_text(kind, &name, "content", content)?;
    if let Some(path) = shape.path {
        require_text(kind, &name, "path", path)?;
    }
    for (field, value) in [
        ("reference", shape.reference),
        ("arguments", shape.arguments),
        ("metadata", shape.metadata),
    ] {
        if let Some(value) = value
            && !value.is_object()
        {
            return Err(format!(
                "{kind} entry {} rejected: {field} must be a dict when provided, got {}",
                repr(&name),
                type_name(value)
            ));
        }
    }
    require_text(kind, &name, "source", shape.source)?;
    match kind {
        "skill" => match shape.reference {
            // An update that omits the reference keeps the stored one.
            None if shape.existing => Ok(()),
            None => Err(format!(
                "skill entry {} rejected: skill entries require a Python reference",
                repr(&name)
            )),
            Some(reference) => skill_reference(reference, &name),
        },
        "factory" => match shape.arguments {
            None if shape.existing => Ok(()),
            Some(arguments)
                if !(arguments["dag"].is_object() && arguments["machine"].is_object())
                    && (arguments["dag"].is_object() || arguments["machine"].is_object()) =>
            {
                Ok(())
            }
            _ => Err(format!(
                "factory entry {} rejected: factory entries require a dag or machine object in arguments",
                repr(&name)
            )),
        },
        _ => Ok(()),
    }
}

/// `_validate_python_skill_reference`.
fn skill_reference(reference: &Value, name: &str) -> Result<(), String> {
    let reject = |message: &str| Err(format!("skill entry {} rejected: {message}", repr(name)));
    let filled = |keys: [&str; 2]| {
        keys.iter().any(|key| {
            reference[*key]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        })
    };
    if reference["type"] != "python" {
        return reject("skill reference.type must be 'python'");
    }
    if !filled(["import", "python_import"]) {
        return reject("skill reference requires a Python import");
    }
    if !filled(["callable", "call_pattern"]) {
        return reject("skill reference requires a callable or call_pattern");
    }
    Ok(())
}

/// `_validate_refinement_event`.
fn validate_refinement(
    trigger: &Value,
    changes: &Value,
    evidence: &Value,
    outcome: &Value,
) -> Result<(), String> {
    if trigger.as_str().is_none_or(str::is_empty) {
        return Err(format!(
            "refinement event rejected: trigger must be a non-empty string, got {}",
            type_name(trigger)
        ));
    }
    match changes {
        Value::String(change) if change.is_empty() => {
            return Err(
                "refinement event rejected: changes must be a non-empty string or a list of strings"
                    .to_owned(),
            );
        }
        Value::String(_) => {}
        Value::Array(changes) => {
            if !changes
                .iter()
                .all(|change| change.as_str().is_some_and(|text| !text.is_empty()))
            {
                return Err(
                    "refinement event rejected: changes must be a list of non-empty strings"
                        .to_owned(),
                );
            }
        }
        other => {
            return Err(format!(
                "refinement event rejected: changes must be a string or a list of strings, got {}",
                type_name(other)
            ));
        }
    }
    for (field, value) in [("evidence", evidence), ("outcome", outcome)] {
        if !value.is_string() {
            return Err(format!(
                "refinement event rejected: {field} must be a string when provided, got {}",
                type_name(value)
            ));
        }
    }
    Ok(())
}

/// The id an entry named by its title gets: the one an entry kept under the
/// earlier id scheme already has, so writing it again updates it.
fn title_id(kind: &str, title: &str, records: Option<&Map<String, Value>>) -> String {
    let id = slug(title, kind);
    let legacy = legacy_slug(title, kind);
    let has = |id: &str| records.is_some_and(|records| records.contains_key(id));
    if !has(&id) && has(&legacy) {
        legacy
    } else {
        id
    }
}

/// Drop the accents of Latin letters (Vietnamese `ở` is `o`, `đ` is `d`); a
/// mark on any other letter is part of its word and stays.
fn fold_latin(raw: &str) -> String {
    let mut out = String::new();
    let mut base_ascii = false;
    for character in raw.replace('đ', "d").replace('Đ', "D").nfd() {
        if unicode_normalization::char::is_combining_mark(character) {
            if base_ascii {
                continue;
            }
        } else {
            base_ascii = character.is_ascii_alphabetic();
        }
        out.push(character);
    }
    out.nfc().collect()
}

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || unicode_normalization::char::is_combining_mark(character)
}

/// An entry id from its title: ASCII where the title is Latin, whole words,
/// at most 60 characters.
fn slug(raw: &str, fallback: &str) -> String {
    let normalized = fold_latin(raw)
        .trim()
        .chars()
        .flat_map(|character| {
            if is_word(character) {
                character.to_lowercase().collect::<Vec<_>>()
            } else {
                vec!['_']
            }
        })
        .collect::<String>();
    let parts = normalized
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let mut slug = String::new();
    for part in &parts {
        let longer = if slug.is_empty() {
            (*part).to_owned()
        } else {
            format!("{slug}_{part}")
        };
        if longer.chars().count() > SLUG_MAX {
            break;
        }
        slug = longer;
    }
    if slug.is_empty()
        && let Some(first) = parts.first()
    {
        slug = first.chars().take(SLUG_MAX).collect();
    }
    if slug.is_empty() {
        fallback.to_owned()
    } else {
        slug
    }
}

/// The id a title got before ids were folded and cut on words.
fn legacy_slug(raw: &str, fallback: &str) -> String {
    let normalized = raw
        .trim()
        .chars()
        .flat_map(|character| {
            if character.is_alphanumeric() {
                character.to_lowercase().collect::<Vec<_>>()
            } else {
                vec!['_']
            }
        })
        .collect::<String>();
    let joined = normalized
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    let chosen = if joined.is_empty() { fallback } else { &joined };
    chosen.chars().take(LEGACY_SLUG_MAX).collect()
}

fn searchable(text: &str) -> String {
    text.nfc().collect::<String>().to_lowercase()
}

/// `text[:117] + "..."` past 120 characters.
fn cut(text: &str) -> String {
    if text.chars().count() <= OVERVIEW_SUMMARY {
        return text.to_owned();
    }
    format!(
        "{}...",
        text.chars().take(OVERVIEW_SUMMARY - 3).collect::<String>()
    )
}

/// `json.dumps(value, ensure_ascii=False, sort_keys=True)`: the spacing the
/// overview always showed.
fn python_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| format!(
                        "{}: {}",
                        Value::String(key.clone()),
                        python_json(&map[key])
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(python_json).collect::<Vec<_>>().join(", ")
        ),
        other => other.to_string(),
    }
}

/// The directory a state file lives in, for a test outside this module.
#[cfg(test)]
pub(crate) fn state_file(dir: &std::path::Path) -> PathBuf {
    dir.join(STATE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn dirs(root: &Path) -> Dirs {
        Dirs {
            global: root.join("global"),
            local: root.join("local"),
            settings: None,
        }
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "the cases are written as literals"
    )]
    fn call(dirs: &Dirs, request: Value) -> Result<Value, String> {
        answer(dirs, &request)
    }

    #[test]
    fn entries_are_created_read_updated_listed_and_deleted() {
        let root = tempfile::tempdir().expect("tempdir");
        let dirs = dirs(root.path());
        let created = call(
            &dirs,
            json!({"op":"create","kind":"memory","title":"Lịch sử triển khai","content":"Deploys on Thursday.","path":"ops","source":"agent"}),
        )
        .expect("create");
        assert_eq!(created["id"], "lich_su_trien_khai");
        assert_eq!(created["scope"], "local");
        assert_eq!(created["version"], 1);
        let again = call(
            &dirs,
            json!({"op":"create","kind":"memory","title":"Lịch sử triển khai","content":"x"}),
        )
        .expect_err("duplicate");
        assert_eq!(again, "memory entry 'lich_su_trien_khai' already exists");

        let updated = call(
            &dirs,
            json!({"op":"update","kind":"memory","id":"lich_su_trien_khai","title":"Deploy day","content":"Deploys on Friday.","path":null}),
        )
        .expect("update");
        assert_eq!(updated["version"], 2);
        assert_eq!(updated["path"], "ops", "an omitted path is kept");
        assert_eq!(updated["created_at"], created["created_at"]);

        let missing = call(
            &dirs,
            json!({"op":"update","kind":"memory","id":"nothing","title":"t","content":"c"}),
        )
        .expect_err("missing");
        assert_eq!(missing, "memory entry 'nothing' does not exist");

        let got = call(
            &dirs,
            json!({"op":"get","kind":"memory","id":"lich_su_trien_khai"}),
        )
        .expect("get");
        assert_eq!(got["content"], "Deploys on Friday.");
        assert_eq!(
            call(&dirs, json!({"op":"get","kind":"memory","id":"nothing"})).expect("get"),
            Value::Null
        );
        let listed = call(&dirs, json!({"op":"list"})).expect("list");
        assert_eq!(listed.as_array().map(Vec::len), Some(1));
        assert_eq!(
            call(
                &dirs,
                json!({"op":"delete","kind":"memory","id":"lich_su_trien_khai"})
            )
            .expect("delete"),
            json!(true)
        );
        assert_eq!(
            call(
                &dirs,
                json!({"op":"delete","kind":"memory","id":"lich_su_trien_khai"})
            )
            .expect("delete"),
            json!(false)
        );
    }

    #[test]
    fn global_writes_go_to_the_global_store_and_upsert_updates_by_title() {
        let root = tempfile::tempdir().expect("tempdir");
        let dirs = dirs(root.path());
        let first = call(
            &dirs,
            json!({"op":"upsert","kind":"prompt","title":"Answer in Vietnamese","content":"Always.","global":true}),
        )
        .expect("upsert");
        assert_eq!(first["scope"], "global");
        let second = call(
            &dirs,
            json!({"op":"upsert","kind":"prompt","title":"Answer in Vietnamese","content":"Always, briefly.","global":true}),
        )
        .expect("upsert");
        assert_eq!(second["version"], 2);
        assert!(state_file(&dirs.global).is_file());
        assert!(!state_file(&dirs.local).exists());
        // The host's digest reads what the kernel wrote.
        let state = super::super::harness::load(&dirs.global, "global");
        assert_eq!(
            state.entries["prompt"][0].content.as_deref(),
            Some("Always, briefly.")
        );
    }

    #[test]
    fn fields_the_host_keeps_survive_a_kernel_update() {
        let root = tempfile::tempdir().expect("tempdir");
        let dirs = dirs(root.path());
        std::fs::create_dir_all(&dirs.local).expect("dir");
        std::fs::write(
            state_file(&dirs.local),
            json!({"entries":{"memory":{"m":{"title":"T","content":"C","topic":"old","history":[1]}}}}).to_string(),
        )
        .expect("seed");
        let updated = call(
            &dirs,
            json!({"op":"update","kind":"memory","id":"m","title":"T","content":"C2"}),
        )
        .expect("update");
        assert_eq!(updated["path"], "old", "a topic is read as the path");
        let raw: Value =
            serde_json::from_str(&std::fs::read_to_string(state_file(&dirs.local)).expect("read"))
                .expect("json");
        assert_eq!(raw["entries"]["memory"]["m"]["history"], json!([1]));
        assert_eq!(raw["entries"]["memory"]["m"]["content"], "C2");
    }

    #[test]
    fn invalid_entries_are_refused_with_the_kernels_words() {
        let root = tempfile::tempdir().expect("tempdir");
        let dirs = dirs(root.path());
        for (request, expected) in [
            (
                json!({"op":"create","kind":"memory","title":"t","content":["a"]}),
                "memory entry 't' rejected: content must be a non-empty string, got a list",
            ),
            (
                json!({"op":"create","kind":"memory","title":5,"content":"c"}),
                "memory entry '<unnamed>' rejected: title must be a non-empty string, got int",
            ),
            (
                json!({"op":"create","kind":"memory","title":"t","content":"c","metadata":[]}),
                "memory entry 't' rejected: metadata must be a dict when provided, got a list",
            ),
            (
                json!({"op":"create","kind":"skill","title":"s","content":"c"}),
                "skill entry 's' rejected: skill entries require a Python reference",
            ),
            (
                json!({"op":"create","kind":"skill","title":"s","content":"c","reference":{"type":"python","import":"x"}}),
                "skill entry 's' rejected: skill reference requires a callable or call_pattern",
            ),
            (
                json!({"op":"create","kind":"factory","title":"f","content":"c"}),
                "factory entry 'f' rejected: factory entries require a dag or machine object in arguments",
            ),
            (
                json!({"op":"create","kind":"note","title":"t","content":"c"}),
                "unknown harness kind 'note'; expected one of ('prompt', 'memory', 'skill', 'subagent', 'factory')",
            ),
            (
                json!({"op":"record_refinement","trigger":"t","changes":[]}),
                "",
            ),
            (
                json!({"op":"record_refinement","trigger":"","changes":"c"}),
                "refinement event rejected: trigger must be a non-empty string, got an empty string",
            ),
        ] {
            match call(&dirs, request.clone()) {
                Err(error) => assert_eq!(error, expected, "{request}"),
                Ok(_) => assert!(expected.is_empty(), "{request} was accepted"),
            }
        }
        // A skill update without a reference keeps the stored one.
        call(
            &dirs,
            json!({"op":"create","kind":"skill","title":"s","content":"c","reference":{"type":"python","import":"x","callable":"run"}}),
        )
        .expect("skill");
        let updated = call(
            &dirs,
            json!({"op":"update","kind":"skill","id":"s","title":"s","content":"c2"}),
        )
        .expect("update");
        assert_eq!(updated["reference"]["callable"], "run");
    }

    #[test]
    fn refinements_are_recorded_with_numbered_ids() {
        let root = tempfile::tempdir().expect("tempdir");
        let dirs = dirs(root.path());
        let first = call(
            &dirs,
            json!({"op":"record_refinement","trigger":"repeated failure","changes":"added a note","evidence":"two runs"}),
        )
        .expect("record");
        assert_eq!(first["id"], "refine_0001");
        assert_eq!(first["changes"], json!(["added a note"]));
        let second = call(
            &dirs,
            json!({"op":"record_refinement","trigger":"t","changes":["a","b"]}),
        )
        .expect("record");
        assert_eq!(second["id"], "refine_0002");
        let overview = call(&dirs, json!({"op":"overview"})).expect("overview");
        assert!(
            overview.as_str().is_some_and(|text| text
                .contains("refinements: 2\n  - [refine_0001] repeated failure: added a note")),
            "{overview}"
        );
    }

    #[test]
    fn search_ranks_rare_terms_first_and_overview_matches_the_kernels() {
        let root = tempfile::tempdir().expect("tempdir");
        let dirs = dirs(root.path());
        for (title, content) in [
            ("Worktree cleanup", "Remove a worktree after merging."),
            ("Release notes", "Write notes for every release."),
            ("Release checklist", "Tag, build, publish the release."),
        ] {
            call(
                &dirs,
                json!({"op":"create","kind":"memory","title":title,"content":content}),
            )
            .expect("create");
        }
        let hits =
            call(&dirs, json!({"op":"search","query":"worktree?","limit":5})).expect("search");
        assert_eq!(hits[0]["id"], "worktree_cleanup");
        assert_eq!(hits.as_array().map(Vec::len), Some(1));
        let release = call(
            &dirs,
            json!({"op":"search","query":"release","kind":"memory"}),
        )
        .expect("search");
        assert_eq!(release.as_array().map(Vec::len), Some(2));
        assert_eq!(
            call(&dirs, json!({"op":"search","query":"release","limit":0})).expect_err("limit"),
            "limit must be a positive int"
        );
        call(
            &dirs,
            json!({"op":"create","kind":"skill","title":"Deploy","content":"Run it.","reference":{"type":"python","import":"deploy","callable":"run"},"arguments":{"b":1,"a":[1,2]}}),
        )
        .expect("skill");
        let overview =
            call(&dirs, json!({"op":"overview","max_entries_per_kind":2})).expect("overview");
        let overview = overview.as_str().unwrap_or_default();
        assert!(
            overview.starts_with("Harness state (local): "),
            "{overview}"
        );
        assert!(overview.contains("memory: 3\n  - [local:release_checklist] Release checklist (general, v1): Tag, build, publish the release."), "{overview}");
        assert!(overview.contains("  - +1 more"), "{overview}");
        assert!(
            overview.contains(r#"  - [local:deploy] Deploy (general, v1) ref={"callable": "run", "import": "deploy", "type": "python"} args={"a": [1, 2], "b": 1}: Run it."#),
            "{overview}"
        );
        let snapshot = call(&dirs, json!({"op":"snapshot"})).expect("snapshot");
        assert_eq!(
            snapshot["entries"]["memory"].as_object().map(Map::len),
            Some(3)
        );
        assert!(snapshot["entries"]["factory"].is_object());
    }

    #[test]
    fn concurrent_writers_keep_every_entry() {
        let root = tempfile::tempdir().expect("tempdir");
        let dirs = std::sync::Arc::new(dirs(root.path()));
        let writers = (0..8)
            .map(|writer| {
                let dirs = std::sync::Arc::clone(&dirs);
                std::thread::spawn(move || {
                    for index in 0..10 {
                        call(
                            &dirs,
                            // Eighty different facts: alike ones would be refused as duplicates.
                            json!({"op":"create","kind":"memory","title":format!("fact {writer} {index}"),"content":format!("writer{writer} recorded item{index} as w{writer}i{index}"),"global":true}),
                        )
                        .expect("create");
                    }
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().expect("writer");
        }
        let listed = call(&dirs, json!({"op":"list","global":true})).expect("list");
        assert_eq!(listed.as_array().map(Vec::len), Some(80));
    }

    #[test]
    fn ids_fold_latin_accents_and_keep_other_scripts() {
        assert_eq!(
            slug("Lịch sử: triển khai đúng giờ!", "memory"),
            "lich_su_trien_khai_dung_gio"
        );
        assert_eq!(slug("修复登录", "memory"), "修复登录");
        assert_eq!(slug("!!!", "memory"), "memory");
        let long = "word ".repeat(30);
        assert!(slug(&long, "memory").chars().count() <= SLUG_MAX);
        assert!(!slug(&long, "memory").ends_with('_'));
        // An entry kept under the earlier id is found by its title.
        let records = json!({"lịch_sử": {"title": "Lịch sử", "content": "c"}});
        assert_eq!(
            title_id("memory", "Lịch sử", records.as_object()),
            "lịch_sử"
        );
        assert_eq!(title_id("memory", "Lịch sử", None), "lich_su");
    }

    /// `rlm.harness` passes the checks the memory tool makes: no secret, no
    /// duplicate, and the version an update names.
    #[test]
    fn kernel_writes_pass_the_memory_checks() {
        let root = tempfile::tempdir().expect("root");
        let dirs = dirs(root.path());
        call(
            &dirs,
            json!({"op":"create","kind":"memory","title":"Deploy day","content":"The project deploys to Fly.io every Friday afternoon."}),
        )
        .expect("create");
        let duplicate = call(
            &dirs,
            json!({"op":"create","kind":"memory","title":"Deploys","content":"The project deploys to Fly.io every Friday afternoon."}),
        )
        .expect_err("the same fact");
        assert!(
            duplicate.contains("same fact") || duplicate.contains("already says"),
            "{duplicate}"
        );
        let secret = call(
            &dirs,
            json!({"op":"create","kind":"memory","title":"Token","content":"the key is sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}),
        )
        .expect_err("a secret");
        assert!(secret.contains("never keeps secrets"), "{secret}");
        let stale = call(
            &dirs,
            json!({"op":"update","kind":"memory","id":"deploy_day","title":"Deploy day","content":"Deploys on Monday.","expected_version":3}),
        )
        .expect_err("a stale version");
        assert!(stale.contains("version 1, not 3"), "{stale}");
        call(
            &dirs,
            json!({"op":"update","kind":"memory","id":"deploy_day","title":"Deploy day","content":"Deploys on Monday.","expected_version":1}),
        )
        .expect("the version it read");
    }
}
