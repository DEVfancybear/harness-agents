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

/// The entry kinds, in the order prime-agent renders them.
const KINDS: [&str; 4] = ["prompt", "memory", "skill", "subagent"];
const STATE_FILE: &str = "harness_state.json";
const DEFAULT_ENTRY_LIMIT: usize = 6;
const DEFAULT_REFINEMENT_LIMIT: usize = 5;
const DEFAULT_CONTENT_LIMIT: usize = 180;
/// The most query terms one digest ranks by (prime-agent caps at 48).
const MAX_QUERY_TERMS: usize = 48;

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
/// becomes overlapping bigrams, since it has no spaces between words; other runs
/// shorter than four characters are noise and dropped. Each term once, in order.
#[must_use]
pub fn query_terms(text: &str) -> Vec<String> {
    let lowered = text.to_lowercase();
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
            } else if segment.len() >= 4 {
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

fn searchable(entry: &Entry) -> (String, String, String) {
    (
        entry.title.clone().unwrap_or_default().to_lowercase(),
        entry.content.clone().unwrap_or_default().to_lowercase(),
        format!("{} {}", entry.path.to_lowercase(), entry.id.to_lowercase()),
    )
}

/// `harnessQueryTermIdf`: `ln(1 + documents / matches)` per term that matches.
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
            weights.insert(term.clone(), (1.0 + documents / matches as f64).ln());
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
    let mut total = 0;
    for kind in KINDS {
        let mut entries = state.entries.get(kind).cloned().unwrap_or_default();
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
        total += entries.len();
        if kind == "subagent" && !entries.is_empty() && options.repl {
            lines.push(format!(
                "{kind}: {} (invoke a spec by turning it into a concise task prompt and spawning with `await rlm.spawn('<task>', name='<worker>')`; admission returns a child handle, never the answer)",
                entries.len()
            ));
        } else {
            lines.push(format!("{kind}: {}", entries.len()));
        }
        if !terms.is_empty() && entries.len() > DEFAULT_ENTRY_LIMIT {
            lines.push(
                "(entries ranked by relevance to the current task; see harness.search)".to_owned(),
            );
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
                "- [{}:{}] {} ({}, v{}){extra}: {}",
                entry.scope,
                entry.id,
                title,
                entry.path,
                entry.version,
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
mod tests {
    use super::{DigestOptions, format_digest, load, merge, query_terms, weighted_terms};
    use serde_json::json;

    fn write(dir: &std::path::Path, state: &serde_json::Value) {
        std::fs::create_dir_all(dir).expect("dir");
        std::fs::write(dir.join("harness_state.json"), state.to_string()).expect("state");
    }

    #[test]
    fn query_terms_follow_prime_agent() {
        assert_eq!(query_terms("Fix the worktree? now"), ["worktree"]);
        assert_eq!(query_terms("修复登录"), ["修复", "复登", "登录"]);
        assert_eq!(query_terms("naïve naïve"), ["naïve"]);
        // Vietnamese words are letters, so they count like any other script.
        assert_eq!(query_terms("người dùng thích"), ["người", "dùng", "thích"]);
        assert_eq!(query_terms("tôi và bạn"), Vec::<String>::new());
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
    fn an_empty_state_says_so() {
        let digest = format_digest(&super::State::default(), DigestOptions::default(), &[]);
        assert!(digest.contains("No saved harness entries yet."));
        assert!(digest.contains("routing/context hints only"));
    }
}
