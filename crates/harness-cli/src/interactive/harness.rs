//! Memory, the way prime-agent keeps it: the continual harness state.
//!
//! Ported from prime-agent's `core/refinement/refinement.ts`. The model owns its
//! memory: it creates, updates and deletes entries - memories, prompt notes, skills,
//! subagent specs - through `rlm.harness` in the Python REPL (the vendored
//! `rlm/harness.py`), and the host never guesses what is worth keeping. The entries
//! live in `harness_state.json` files: a global one shared by every conversation, and
//! a local one per conversation. Before each turn the host reads both, ranks each
//! kind's entries by relevance to the task, and hands the model a compact digest
//! inside `<harness_state>`, as prime-agent's harness digest message does.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use harness_session::{ContextBlock, ContextBlockKind};
use serde_json::{Map, Value};
use unicode_normalization::UnicodeNormalization as _;

/// The entry kinds, in the order prime-agent renders them.
const KINDS: [&str; 4] = ["prompt", "memory", "skill", "subagent"];
const STATE_FILE: &str = "harness_state.json";
const DEFAULT_ENTRY_LIMIT: usize = 6;
const DEFAULT_REFINEMENT_LIMIT: usize = 5;
const DEFAULT_CONTENT_LIMIT: usize = 180;
/// The most query terms one digest ranks by (prime-agent caps at 48).
const MAX_QUERY_TERMS: usize = 48;
/// The characters the pinned entries may take in every turn's digest.
///
/// A pinned entry (`metadata.pinned`) is shown whole on every turn, whatever
/// the request: a preference such as "answer in Vietnamese" shares no word
/// with most requests, so ranking left it out of the six shown, and the 180
/// characters each shown entry keeps could cut its point off. `CowAgent` loads
/// its whole `MEMORY.md` every turn; this keeps the same promise for the few
/// entries that need it, within a bound.
pub const PINNED_BUDGET: usize = 4_000;

/// Where the global state lives: shared by every conversation.
#[must_use]
pub fn global_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("harness")
}

/// Where one conversation's local state lives.
#[must_use]
pub fn local_dir(data_dir: &Path, conversation: &str) -> PathBuf {
    data_dir.join("sessions").join(conversation).join("harness")
}

/// One entry as the digest renders it; a malformed field stays `None` and the
/// entry is skipped with a diagnostic instead of breaking the digest.
#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub content: Option<String>,
    pub path: String,
    pub scope: String,
    pub version: i64,
    pub reference: Map<String, Value>,
    pub arguments: Map<String, Value>,
    /// Shown whole in every digest (`metadata.pinned`).
    pub pinned: bool,
    /// When it was last written (`@<seconds>` or RFC 3339), for its age.
    pub updated_at: Option<String>,
}

/// The merged state of one turn.
#[derive(Clone, Debug, Default)]
pub struct State {
    pub entries: BTreeMap<&'static str, Vec<Entry>>,
    pub refinements: Vec<Value>,
}

impl State {
    #[must_use]
    pub fn total(&self) -> usize {
        self.entries.values().map(Vec::len).sum()
    }
}

/// `loadHarnessState`: a missing, unreadable or corrupt file is an empty state,
/// never an error - the digest runs before every turn.
#[must_use]
pub fn load(dir: &Path, scope: &str) -> State {
    let mut state = State::default();
    let Some(parsed) = std::fs::read_to_string(dir.join(STATE_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(Value::is_object)
    else {
        return state;
    };
    for kind in KINDS {
        let Some(records) = parsed["entries"][kind].as_object() else {
            continue;
        };
        let entries = state.entries.entry(kind).or_default();
        for (id, raw) in records {
            let Some(entry) = raw.as_object() else {
                continue;
            };
            let text = |field: &str| entry.get(field).and_then(Value::as_str).map(str::to_owned);
            let object = |field: &str| {
                entry
                    .get(field)
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default()
            };
            entries.push(Entry {
                id: id.clone(),
                kind: kind.to_owned(),
                title: text("title"),
                content: text("content"),
                // State written while the grouping was named `topic` has no `path`.
                path: text("path").or_else(|| text("topic")).unwrap_or_default(),
                scope: match entry.get("scope").and_then(Value::as_str) {
                    Some(value @ ("global" | "local")) => value.to_owned(),
                    _ => scope.to_owned(),
                },
                version: entry.get("version").and_then(Value::as_i64).unwrap_or(1),
                reference: object("reference"),
                arguments: object("arguments"),
                pinned: entry
                    .get("metadata")
                    .and_then(|metadata| metadata.get("pinned"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                updated_at: text("updated_at").or_else(|| text("created_at")),
            });
        }
    }
    if let Some(refinements) = parsed["refinements"].as_array() {
        state.refinements.clone_from(refinements);
    }
    state
}

/// `mergeHarnessStates`: global entries first; a local entry whose id a global
/// one already holds is renamed `local:<id>`.
#[must_use]
pub fn merge(global: State, local: State) -> State {
    let mut merged = State::default();
    for kind in KINDS {
        let mut entries = global.entries.get(kind).cloned().unwrap_or_default();
        for entry in &mut entries {
            if entry.scope != "local" {
                "global".clone_into(&mut entry.scope);
            }
        }
        for mut entry in local.entries.get(kind).cloned().unwrap_or_default() {
            if entry.scope != "global" {
                "local".clone_into(&mut entry.scope);
            }
            if entries.iter().any(|existing| existing.id == entry.id) {
                entry.id = format!("{}:{}", entry.scope, entry.id);
            }
            entries.push(entry);
        }
        merged.entries.insert(kind, entries);
    }
    merged.refinements = global.refinements;
    merged.refinements.extend(local.refinements);
    merged
}

fn is_cjk(character: char) -> bool {
    matches!(u32::from(character),
        0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0xac00..=0xd7af
        | 0x20000..=0x2a6df | 0x2a700..=0x2ebef | 0x2ebf0..=0x2ee5f | 0x2f800..=0x2fa1f
        | 0x30000..=0x3347f)
}

/// A letter, digit or combining mark: what `\p{L}\p{N}\p{M}` matches.
fn is_term_character(character: char) -> bool {
    character.is_alphanumeric()
        || matches!(u32::from(character),
            0x0300..=0x036f | 0x0483..=0x0489 | 0x0591..=0x05bd | 0x0610..=0x061a
            | 0x064b..=0x065f | 0x0900..=0x0963 | 0x0966..=0x0dff | 0x1ab0..=0x1aff
            | 0x1dc0..=0x1dff | 0x20d0..=0x20ff | 0xfe20..=0xfe2f)
}

/// `harnessQueryTerms`: lowercase runs of letters, digits and marks; a CJK run
/// becomes overlapping bigrams, since it has no spaces between words. Other runs
/// retain at least two characters across scripts. Document frequency discounts
/// common words without language-specific stoplists.
/// Each term once, in order, with canonically equivalent Unicode normalized.
#[must_use]
pub fn query_terms(text: &str) -> Vec<String> {
    let lowered = normalized_search_text(text);
    let mut terms = Vec::new();
    let push = |term: String, terms: &mut Vec<String>| {
        if !terms.contains(&term) {
            terms.push(term);
        }
    };
    for run in lowered.split(|character: char| !is_term_character(character)) {
        if run.is_empty() {
            continue;
        }
        // Split the run at CJK boundaries.
        let characters = run.chars().collect::<Vec<_>>();
        let mut start = 0;
        while start < characters.len() {
            let cjk = is_cjk(characters[start]);
            let mut end = start;
            while end < characters.len() && is_cjk(characters[end]) == cjk {
                end += 1;
            }
            let segment = &characters[start..end];
            if cjk {
                if segment.len() == 1 {
                    push(segment[0].to_string(), &mut terms);
                } else {
                    for pair in segment.windows(2) {
                        push(pair.iter().collect(), &mut terms);
                    }
                }
            } else if segment.len() >= 2 {
                push(segment.iter().collect(), &mut terms);
            }
            start = end;
        }
    }
    terms
}

/// Query terms with weights, from task signal: the goal objective weighs most,
/// then the newest messages, newest first (`_buildHarnessDigestQueryTerms`).
#[must_use]
pub fn weighted_terms(goal: Option<&str>, recent_newest_first: &[&str]) -> Vec<(String, f64)> {
    let mut terms: Vec<(String, f64)> = Vec::new();
    let mut add = |text: &str, weight: f64| {
        for term in query_terms(text) {
            if terms.iter().any(|(existing, _)| *existing == term) {
                continue;
            }
            if terms.len() >= MAX_QUERY_TERMS {
                return;
            }
            terms.push((term, weight));
        }
    };
    if let Some(goal) = goal {
        add(goal, 3.0);
    }
    let mut weight: f64 = 2.0;
    for text in recent_newest_first.iter().take(4) {
        add(text, weight);
        weight = (weight - 0.5).max(1.0);
    }
    terms
}

fn normalized_search_text(text: &str) -> String {
    text.nfc().collect::<String>().to_lowercase()
}

fn searchable(entry: &Entry) -> (String, String, String) {
    (
        normalized_search_text(entry.title.as_deref().unwrap_or_default()),
        normalized_search_text(entry.content.as_deref().unwrap_or_default()),
        normalized_search_text(&format!("{} {}", entry.path, entry.id)),
    )
}

/// Smoothed corpus IDF: ubiquitous terms approach zero without a stoplist,
/// while matching terms retain a positive score even in a one-entry corpus.
fn idf(entries: &[Entry], terms: &[(String, f64)]) -> BTreeMap<String, f64> {
    let fields = entries.iter().map(searchable).collect::<Vec<_>>();
    #[allow(
        clippy::cast_precision_loss,
        reason = "entry counts are small; this is a relevance weight"
    )]
    let documents = fields.len() as f64;
    let mut weights = BTreeMap::new();
    for (term, _) in terms {
        let matches = fields
            .iter()
            .filter(|(title, content, identifier)| {
                title.contains(term.as_str())
                    || content.contains(term.as_str())
                    || identifier.contains(term.as_str())
            })
            .count();
        if matches > 0 {
            #[allow(clippy::cast_precision_loss, reason = "small counts")]
            let matches = matches as f64;
            weights.insert(
                term.clone(),
                ((documents - matches + 0.5) / (matches + 0.5)).ln_1p(),
            );
        }
    }
    weights
}

/// `scoreHarnessEntryForQuery`: weighted overlap, a term matching more fields
/// counting a little more, each term discounted by its document frequency.
fn score(entry: &Entry, terms: &[(String, f64)], idf: &BTreeMap<String, f64>) -> f64 {
    let (title, content, identifier) = searchable(entry);
    terms
        .iter()
        .map(|(term, weight)| {
            let fields = [&title, &content, &identifier]
                .iter()
                .filter(|field| field.contains(term.as_str()))
                .count();
            if fields == 0 {
                return 0.0;
            }
            #[allow(clippy::cast_precision_loss, reason = "at most three fields")]
            let spread = 1.0 + (fields as f64 - 1.0) * 0.5;
            weight * idf.get(term).copied().unwrap_or(1.0) * spread
        })
        .sum()
}

/// Entries that match `query`, best first, as indexes into `entries`: the
/// digest's ranking, for the `memory` tool's search.
#[must_use]
pub fn rank(entries: &[Entry], query: &str) -> Vec<(usize, f64)> {
    let terms = weighted_terms(None, &[query]);
    let weights = idf(entries, &terms);
    let mut ranked = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (index, score(entry, &terms, &weights)))
        .filter(|(_, score)| *score > 0.0)
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

/// Hold a scope's state lock: the sidecar the Python kernel's `rlm.harness`
/// locks (`harness_state.json.lock`), so a read-modify-write from the host
/// never interleaves with one from the kernel.
///
/// # Errors
/// The lock cannot be opened, or another writer kept it past 30 seconds.
pub fn lock_state(dir: &Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("harness state directory: {error}"))?;
    let file = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(format!("{STATE_FILE}.lock")))
        .map_err(|error| format!("harness state lock: {error}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err("waiting for another memory write timed out".to_owned());
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(format!("harness state lock: {error}"));
            }
        }
    }
}

/// How long ago an entry was written, as `updated 3d ago`: an entry that
/// records a state (progress, a status, a count) goes stale, and its age is
/// how a reader knows to check it before trusting it.
#[must_use]
pub fn age_label(entry: &Entry) -> Option<String> {
    age_of(entry.updated_at.as_deref()?)
}

/// [`age_label`] of a written time (`@<seconds>` or RFC 3339).
#[must_use]
pub fn age_of(written: &str) -> Option<String> {
    let seconds = match written.strip_prefix('@') {
        Some(seconds) => seconds.parse::<i64>().ok()?,
        None => chrono::DateTime::parse_from_rfc3339(written)
            .ok()?
            .timestamp(),
    };
    let age = chrono::Utc::now()
        .timestamp()
        .saturating_sub(seconds)
        .max(0);
    Some(match age {
        0..3_600 => "updated <1h ago".to_owned(),
        3_600..172_800 => format!("updated {}h ago", age / 3_600),
        _ => format!("updated {}d ago", age / 86_400),
    })
}

/// `, updated 3d ago` for a line that has room for it, or nothing.
fn age_suffix(entry: &Entry) -> String {
    age_label(entry).map_or_else(String::new, |age| format!(", {age}"))
}

fn identity(entry: &Entry) -> String {
    format!(
        "{}\0{}\0{}",
        entry.path,
        entry.title.as_deref().unwrap_or_default(),
        entry.id
    )
}

/// `compactText`: whitespace folded, cut with `...` past `limit` characters.
fn compact(text: &str, limit: usize) -> String {
    let folded = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if folded.chars().count() <= limit {
        return folded;
    }
    let kept = folded
        .chars()
        .take(limit.saturating_sub(3))
        .collect::<String>();
    format!("{kept}...")
}

/// What the digest may tell the model to call.
#[derive(Clone, Copy, Debug, Default)]
pub struct DigestOptions {
    /// The Python REPL is available (`includeIpythonExamples`).
    pub repl: bool,
    /// The `refine` skill is available (`includeRefineExamples`).
    pub refine: bool,
    /// The native `memory` tool is available: memories and prompt notes are
    /// kept with it, with or without the REPL.
    pub memory_tool: bool,
}

/// `formatHarnessStateForPrompt`.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one render, told in prime-agent's order"
)]
pub fn format_digest(state: &State, options: DigestOptions, terms: &[(String, f64)]) -> String {
    let mut lines: Vec<String> = vec![
        "# Continual Harness State".to_owned(),
        String::new(),
        "Local continual harness entries belong to this conversation. Global continual harness entries persist across conversations.".to_owned(),
        "The continual harness entries below are compact summaries, not full descriptions. Use them as routing/context hints; inspect or refine the underlying continual harness entry only when detail matters.".to_owned(),
        "Default to local continual harness refinement for current task progress, temporary blockers, and session coordination. Use global continual harness refinement only for stable cross-session lessons, durable user preferences, reusable skills/subagents, or explicitly project-qualified facts.".to_owned(),
        "Use these continual harness prompt notes, memories, skills, and subagent specs when they are relevant. The base system prompt is immutable; prompt entries below are supplemental notes only.".to_owned(),
        String::new(),
        if options.refine {
            "When to call `await refine.run()`: after a repeated failure, a reusable tactic emerges, a repeated delegation role should become a subagent spec, a repeated procedure should become a skill, a durable fact/preference should become a memory, a narrow behavioral policy should become a prompt addendum, a user corrects behavior that should persist locally or globally, validation shows a continual harness entry is wrong, or a skill/subagent/memory/prompt note should be created, updated, deleted, or rolled back. Keep `await refine.run()` continual harness edits small and evidence-backed."
        } else {
            "When to refine the continual harness: after a repeated failure, a reusable tactic emerges, a repeated delegation role should become a subagent spec, a repeated procedure should become a skill, a durable fact/preference should become a memory, a narrow behavioral policy should become a prompt addendum, a user corrects behavior that should persist locally or globally, validation shows a continual harness entry is wrong, or a skill/subagent/memory/prompt note should be created, updated, deleted, or rolled back. Keep continual harness edits small and evidence-backed."
        }
        .to_owned(),
        String::new(),
        if options.repl {
            "Call contract: manage entries in the Python REPL with `rlm.harness` - `create_memory(title, content, path=..., global_=...)`, `update_memory(id, title, content)`, `delete_memory(id)`, and the same for `prompt_note`, `skill` and `subagent`; `rlm.harness.overview()` and `rlm.harness.search(...)` read them. Continual harness skill entries are Python REPL skills with an explicit Python `reference` and `arguments` contract. Spawn a continual harness subagent spec by composing a concise task prompt and calling `handle = await rlm.spawn('sub-task', name='worker')`, then `await rlm.collect([handle], timeout_ms=...)` before the turn ends. Do not invent wrappers such as `call_skill(...)`, `run_subagent(...)`, or named subagent registries."
        } else {
            "Call contract: continual harness entries are routing/context hints only in sessions without the Python REPL; do not use Python `await`, `asyncio`, or `rlm` examples unless the prompt also documents a Python kernel."
        }
        .to_owned(),
        String::new(),
    ];
    if options.memory_tool
        && let Some(contract) = lines
            .iter()
            .position(|line| line.starts_with("Call contract"))
    {
        lines.splice(
            contract..contract,
            [
                "Memory tool: keep memories and prompt notes with the `memory` tool (list, search, read, write, delete). Only the most relevant few entries of each kind appear below, cut short: search memory for the rest, and read an entry before relying on its detail.".to_owned(),
                String::new(),
            ],
        );
    }
    pinned_lines(state, &mut lines);
    let mut total = 0;
    for kind in KINDS {
        let mut entries = state.entries.get(kind).cloned().unwrap_or_default();
        let kept = entries.len();
        // A pinned entry is shown whole above; the ranked list is the rest.
        entries.retain(|entry| !entry.pinned);
        let pinned = kept - entries.len();
        let weights = if terms.is_empty() {
            BTreeMap::new()
        } else {
            idf(&entries, terms)
        };
        entries.sort_by(|a, b| {
            if terms.is_empty() {
                return identity(a).cmp(&identity(b));
            }
            score(b, terms, &weights)
                .total_cmp(&score(a, terms, &weights))
                .then_with(|| identity(a).cmp(&identity(b)))
        });
        total += kept;
        if pinned > 0 {
            lines.push(format!("{kind}: {kept} ({pinned} pinned above)"));
        } else if kind == "subagent" && !entries.is_empty() && options.repl {
            lines.push(format!(
                "{kind}: {} (invoke a spec by turning it into a concise task prompt and spawning with `await rlm.spawn('<task>', name='<worker>')`; admission returns a child handle, never the answer)",
                entries.len()
            ));
        } else {
            lines.push(format!("{kind}: {}", entries.len()));
        }
        if !terms.is_empty() && entries.len() > DEFAULT_ENTRY_LIMIT {
            lines.push(if options.memory_tool && matches!(kind, "memory" | "prompt") {
                "(entries ranked by relevance to the current task; the memory tool searches the rest)"
                    .to_owned()
            } else {
                "(entries ranked by relevance to the current task; see harness.search)".to_owned()
            });
        }
        for entry in entries.iter().take(DEFAULT_ENTRY_LIMIT) {
            let (Some(title), Some(content)) = (&entry.title, &entry.content) else {
                let reason = if entry.content.is_none() {
                    "content not a string"
                } else {
                    "title not a string"
                };
                lines.push(format!(
                    "harness: skipped malformed entry {} ({reason})",
                    entry.id
                ));
                continue;
            };
            let mut extra = String::new();
            if entry.kind == "skill" && !entry.reference.is_empty() {
                let _ = write!(
                    extra,
                    " ref={}",
                    compact(
                        &Value::Object(entry.reference.clone()).to_string(),
                        DEFAULT_CONTENT_LIMIT
                    )
                );
            }
            if entry.kind == "skill" && !entry.arguments.is_empty() {
                let _ = write!(
                    extra,
                    " args={}",
                    compact(
                        &Value::Object(entry.arguments.clone()).to_string(),
                        DEFAULT_CONTENT_LIMIT
                    )
                );
            }
            lines.push(format!(
                "- [{}:{}] {} ({}, v{}{}){extra}: {}",
                entry.scope,
                entry.id,
                title,
                entry.path,
                entry.version,
                age_suffix(entry),
                compact(content, DEFAULT_CONTENT_LIMIT)
            ));
        }
        if entries.len() > DEFAULT_ENTRY_LIMIT {
            lines.push(format!(
                "- +{} more {kind} entries",
                entries.len() - DEFAULT_ENTRY_LIMIT
            ));
        }
        lines.push(String::new());
    }
    if total == 0 {
        lines.push("No saved harness entries yet.".to_owned());
        lines.push(String::new());
    }
    lines.push(format!("recent refinements: {}", state.refinements.len()));
    let skip = state
        .refinements
        .len()
        .saturating_sub(DEFAULT_REFINEMENT_LIMIT);
    for event in state.refinements.iter().skip(skip) {
        let id = event["id"].as_str();
        let trigger = event["trigger"].as_str();
        let changes = event["changes"].as_array().and_then(|changes| {
            changes
                .iter()
                .map(|change| change.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
        });
        let outcome = &event["outcome"];
        match (id, trigger, changes) {
            (Some(id), Some(trigger), Some(changes))
                if outcome.is_null() || outcome.is_string() =>
            {
                let changes = if changes.is_empty() {
                    "no applied edits".to_owned()
                } else {
                    changes.join(", ")
                };
                let outcome = outcome
                    .as_str()
                    .filter(|outcome| !outcome.is_empty())
                    .map(|outcome| {
                        format!("; outcome: {}", compact(outcome, DEFAULT_CONTENT_LIMIT))
                    })
                    .unwrap_or_default();
                lines.push(format!(
                    "- [{id}] {}: {changes}{outcome}",
                    compact(trigger, DEFAULT_CONTENT_LIMIT)
                ));
            }
            _ => {
                let label = match event {
                    Value::Object(object) => object
                        .get("id")
                        .and_then(Value::as_str)
                        .map_or_else(|| "an event without a string id".to_owned(), str::to_owned),
                    Value::Null => "null".to_owned(),
                    Value::Array(_) => "an array".to_owned(),
                    _ => "a scalar".to_owned(),
                };
                lines.push(format!(
                    "harness: skipped malformed refinement event {label}"
                ));
            }
        }
    }
    if skip > 0 {
        lines.push(format!("- +{skip} older refinement events"));
    }
    lines.join("\n").trim().to_owned()
}

/// The pinned entries, whole, within [`PINNED_BUDGET`]; what does not fit is
/// counted so the model knows to search for it.
fn pinned_lines(state: &State, lines: &mut Vec<String>) {
    let pinned = KINDS
        .iter()
        .flat_map(|kind| state.entries.get(kind).into_iter().flatten())
        .filter(|entry| entry.pinned && entry.title.is_some() && entry.content.is_some())
        .collect::<Vec<_>>();
    if pinned.is_empty() {
        return;
    }
    lines.push("pinned (always shown in full; they hold whatever the request):".to_owned());
    let mut used = 0;
    let mut left_out = 0;
    for entry in pinned {
        let content = entry
            .content
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let line = format!(
            "- [{}:{}] {} ({}, {}, v{}{}): {content}",
            entry.scope,
            entry.id,
            entry.title.as_deref().unwrap_or_default(),
            entry.kind,
            entry.path,
            entry.version,
            age_suffix(entry)
        );
        let size = line.chars().count();
        if used + size > PINNED_BUDGET {
            left_out += 1;
            continue;
        }
        used += size;
        lines.push(line);
    }
    if left_out > 0 {
        lines.push(format!(
            "- +{left_out} pinned entries past the {PINNED_BUDGET}-character bound: search memory for them, and unpin what no longer needs to be in every turn"
        ));
    }
    lines.push(String::new());
}

/// The digest as the context block a turn carries, wrapped the way prime-agent's
/// harness digest message is.
#[must_use]
pub fn digest_block(digest: &str) -> ContextBlock {
    ContextBlock::mandatory(
        "harness_state",
        ContextBlockKind::Instruction,
        format!(
            "[harness-digest]\n\nThe persistent memories produced across this session so far:\n\n<harness_state>\n{digest}\n</harness_state>"
        ),
    )
}

#[cfg(test)]
mod age_tests {
    use serde_json::json;

    /// Every entry says how old it is, in the digest and to the memory tool,
    /// so a reader checks a recorded state before trusting it.
    #[test]
    fn an_entry_says_how_old_it_is() {
        let directory = tempfile::tempdir().expect("dir");
        let now = chrono::Utc::now().timestamp();
        let state = json!({"schema": 1, "entries": {"memory": {
            "old": {"title": "Goal state", "content": "the goal is not complete", "updated_at": format!("@{}", now - 3 * 86_400)},
            "new": {"title": "Fresh", "content": "written just now", "updated_at": chrono::Utc::now().to_rfc3339()},
            "bare": {"title": "Undated", "content": "no time recorded"}
        }, "prompt": {}, "skill": {}, "subagent": {}}, "refinements": []});
        std::fs::write(
            directory.path().join("harness_state.json"),
            state.to_string(),
        )
        .expect("state");
        let loaded = super::load(directory.path(), "global");
        let entries = &loaded.entries["memory"];
        let age = |id: &str| {
            super::age_label(entries.iter().find(|entry| entry.id == id).expect("entry"))
        };
        assert_eq!(age("old").as_deref(), Some("updated 3d ago"));
        assert_eq!(age("new").as_deref(), Some("updated <1h ago"));
        assert_eq!(age("bare"), None);
        let digest = super::format_digest(&loaded, super::DigestOptions::default(), &[]);
        assert!(digest.contains("v1, updated 3d ago"), "{digest}");
    }
}

#[cfg(test)]
mod tests {
    use super::{DigestOptions, format_digest, load, merge, query_terms, weighted_terms};
    use serde_json::json;

    fn write(dir: &std::path::Path, state: &serde_json::Value) {
        std::fs::create_dir_all(dir).expect("dir");
        std::fs::write(dir.join("harness_state.json"), state.to_string()).expect("state");
    }

    #[test]
    fn query_terms_preserve_words_without_language_stoplists() {
        assert_eq!(
            query_terms("Fix the worktree? now"),
            ["fix", "the", "worktree", "now"]
        );
        assert_eq!(query_terms("修复登录"), ["修复", "复登", "登录"]);
        assert_eq!(query_terms("naïve naïve"), ["naïve"]);
        // Vietnamese words are letters, so they count like any other script.
        assert_eq!(query_terms("người dùng thích"), ["người", "dùng", "thích"]);
        assert_eq!(query_terms("tôi và bạn"), ["tôi", "và", "bạn"]);
    }

    #[test]
    fn a_missing_or_corrupt_state_is_empty() {
        let directory = tempfile::tempdir().expect("dir");
        assert_eq!(load(directory.path(), "global").entries.len(), 0);
        std::fs::write(directory.path().join("harness_state.json"), "{not json").expect("write");
        assert_eq!(load(directory.path(), "global").entries.len(), 0);
    }

    #[test]
    fn the_digest_merges_scopes_ranks_and_skips_malformed_entries() {
        let directory = tempfile::tempdir().expect("dir");
        let global = directory.path().join("global");
        let local = directory.path().join("local");
        write(
            &global,
            &json!({"schema": 1, "entries": {"memory": {
                "tabs": {"title": "Indentation", "content": "The user prefers tabs", "path": "prefs", "version": 2},
                "deploy": {"title": "Deploy", "content": "Deploys go through the release script", "path": "ops", "version": 1},
                "broken": {"title": 3, "content": "x"}
            }}, "refinements": [{"id": "r1", "trigger": "user asked", "changes": ["create memory tabs"], "outcome": "ok"}, 7]}),
        );
        write(
            &local,
            &json!({"entries": {"memory": {"tabs": {"title": "Local tabs", "content": "This task uses spaces", "path": "task"}}}}),
        );
        let state = merge(load(&global, "global"), load(&local, "local"));
        let digest = format_digest(
            &state,
            DigestOptions {
                repl: true,
                refine: false,
                memory_tool: false,
            },
            &weighted_terms(None, &["how do deploys work"]),
        );
        assert!(digest.contains("memory: 4"), "{digest}");
        assert!(
            digest.contains("- [global:tabs] Indentation (prefs, v2): The user prefers tabs"),
            "{digest}"
        );
        assert!(
            digest.contains("- [local:local:tabs] Local tabs"),
            "{digest}"
        );
        assert!(
            digest.contains("harness: skipped malformed entry broken (title not a string)"),
            "{digest}"
        );
        // The relevant entry ranks first.
        let deploy = digest.find("Deploy").expect("deploy");
        let tabs = digest.find("Indentation").expect("tabs");
        assert!(deploy < tabs, "{digest}");
        assert!(
            digest.contains("- [r1] user asked: create memory tabs; outcome: ok"),
            "{digest}"
        );
        assert!(
            digest.contains("harness: skipped malformed refinement event a scalar"),
            "{digest}"
        );
        assert!(digest.contains("rlm.harness"), "{digest}");
    }

    #[test]
    fn with_the_memory_tool_the_digest_points_at_it() {
        let digest = format_digest(
            &retrieval_fixture(),
            DigestOptions {
                memory_tool: true,
                ..DigestOptions::default()
            },
            &weighted_terms(None, &["máy in"]),
        );
        let tool = digest.find("Memory tool:").expect("memory tool line");
        assert!(
            tool < digest.find("Call contract").expect("contract"),
            "{digest}"
        );
        assert!(
            digest.contains("the memory tool searches the rest"),
            "{digest}"
        );
        let without = format_digest(&retrieval_fixture(), DigestOptions::default(), &[]);
        assert!(!without.contains("Memory tool:"), "{without}");
    }

    /// A pinned preference shares no word with the request and would rank out
    /// of the six shown; it is shown whole every turn instead.
    #[test]
    fn a_pinned_entry_is_shown_whole_whatever_the_request() {
        let directory = tempfile::tempdir().expect("state");
        let mut memories = serde_json::Map::new();
        for number in 0..12 {
            memories.insert(
                format!("alpha-{number}"),
                json!({"title": format!("Deploy step {number}"), "content": "deploy fixture"}),
            );
        }
        let long = format!(
            "Answer in Vietnamese and call the user anh Duong. {}",
            "x".repeat(300)
        );
        memories.insert(
            "language".to_owned(),
            json!({"title": "Language", "content": long, "metadata": {"pinned": true}}),
        );
        write(directory.path(), &json!({"entries":{"memory":memories}}));
        let digest = format_digest(
            &load(directory.path(), "global"),
            DigestOptions::default(),
            &weighted_terms(None, &["how do deploys work"]),
        );
        assert!(digest.contains("pinned (always shown in full"), "{digest}");
        assert!(digest.contains(&long), "the whole entry: {digest}");
        assert!(digest.contains("memory: 13 (1 pinned above)"), "{digest}");
        assert_eq!(digest.matches("[global:language]").count(), 1, "{digest}");
    }

    #[test]
    fn pinned_entries_past_the_bound_are_counted_not_shown() {
        let directory = tempfile::tempdir().expect("state");
        let mut memories = serde_json::Map::new();
        for number in 0..3 {
            memories.insert(
                format!("rule-{number}"),
                json!({"title": format!("Rule {number}"), "content": "y".repeat(super::PINNED_BUDGET / 2), "metadata": {"pinned": true}}),
            );
        }
        write(directory.path(), &json!({"entries":{"memory":memories}}));
        let digest = format_digest(
            &load(directory.path(), "global"),
            DigestOptions::default(),
            &[],
        );
        assert!(digest.contains("+2 pinned entries past the"), "{digest}");
    }

    #[test]
    fn an_empty_state_says_so() {
        let digest = format_digest(&super::State::default(), DigestOptions::default(), &[]);
        assert!(digest.contains("No saved harness entries yet."));
        assert!(digest.contains("routing/context hints only"));
    }

    fn retrieval_fixture() -> super::State {
        let directory = tempfile::tempdir().expect("state");
        let mut memories = serde_json::Map::new();
        for number in 0..12 {
            memories.insert(
                format!("alpha-{number}"),
                json!({
                    "title": format!("Alpha {number}"), "content": "Unrelated fixture item"
                }),
            );
        }
        memories.insert(
            "printer".to_owned(),
            json!({
                "title": "Zebra Máy in", "content": "PRINTER=orange-63"
            }),
        );
        memories.insert(
            "config".to_owned(),
            json!({
                "title": "Zebra Cấu hình", "content": "CONFIG=violet-64"
            }),
        );
        write(directory.path(), &json!({"entries":{"memory":memories}}));
        load(directory.path(), "local")
    }

    #[test]
    fn short_vietnamese_words_route_the_relevant_memory() {
        let digest = format_digest(
            &retrieval_fixture(),
            DigestOptions::default(),
            &weighted_terms(None, &["tôi và bạn hỏi về máy in"]),
        );
        assert!(digest.contains("PRINTER=orange-63"), "{digest}");
        assert_eq!(query_terms("máy in"), ["máy", "in"]);
    }

    #[test]
    fn canonically_equivalent_unicode_routes_the_same_memory() {
        let state = retrieval_fixture();
        for query in ["cấu hình", "ca\u{0302}\u{0301}u hi\u{0300}nh"] {
            let digest = format_digest(
                &state,
                DigestOptions::default(),
                &weighted_terms(None, &[query]),
            );
            assert!(digest.contains("CONFIG=violet-64"), "{digest}");
        }
    }

    #[test]
    fn short_queries_route_memory_across_scripts() {
        let directory = tempfile::tempdir().expect("state");
        let mut memories = serde_json::Map::new();
        for number in 0..12 {
            memories.insert(
                format!("alpha-{number}"),
                json!({"title": format!("Alpha {number}"), "content": "Unrelated fixture"}),
            );
        }
        let queries = ["go ui", "máy in", "le thé", "миф", "خط", "गति", "登录"];
        for (number, query) in queries.iter().enumerate() {
            memories.insert(
                format!("language-{number}"),
                json!({"title": format!("Zebra {query}"), "content": format!("CANARY={number}")}),
            );
        }
        write(directory.path(), &json!({"entries":{"memory":memories}}));
        let state = load(directory.path(), "local");
        for (number, query) in queries.iter().enumerate() {
            let digest = format_digest(
                &state,
                DigestOptions::default(),
                &weighted_terms(None, &[query]),
            );
            assert!(
                digest.contains(&format!("CANARY={number}")),
                "query {query:?}: {digest}"
            );
        }
    }

    #[test]
    fn common_words_do_not_hide_a_distinctive_short_term() {
        let directory = tempfile::tempdir().expect("state");
        let mut memories = serde_json::Map::new();
        for number in 0..24 {
            memories.insert(
                format!("alpha-{number}"),
                json!({"title": format!("Alpha we can use {number}"), "content": "we can use"}),
            );
        }
        for number in 0..12 {
            memories.insert(
                format!("other-{number}"),
                json!({"title": format!("Other {number}"), "content": "Unrelated"}),
            );
        }
        memories.insert(
            "target".to_owned(),
            json!({"title": "Zebra UI", "content": "RESULT=42"}),
        );
        write(directory.path(), &json!({"entries":{"memory":memories}}));
        let digest = format_digest(
            &load(directory.path(), "local"),
            DigestOptions::default(),
            &weighted_terms(None, &["we can use ui"]),
        );
        assert!(digest.contains("RESULT=42"), "{digest}");
        assert!(
            digest.find("[local:target]").expect("target")
                < digest.find("[local:alpha-").expect("alpha"),
            "{digest}"
        );
    }
}
