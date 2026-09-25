//! `/refine`: prime-agent's continual harness refinement.
//!
//! Ported from prime-agent's `core/refinement/refinement.ts`. A model pass reads the
//! conversation, the current harness state and the refinement history, and answers
//! with JSON create/update/delete edits to reusable state - prompt notes, memories,
//! skills, subagent specs. The host validates every edit, applies the valid ones to
//! the requested scope's `harness_state.json`, records the refinement, and can roll
//! one back. Every twenty-five turns an automatic review asks the model whether the
//! trajectory holds something worth refining, and refines locally when it does:
//! that is how memory is learned without any keyword or classifier.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_providers::{
    CancellationToken, MessageRole, ModelProvider, ProviderMessage, ProviderRequest,
};
use harness_types::RequestId;
use serde_json::{Map, Value, json};

const STATE_FILE: &str = "harness_state.json";
const HISTORY_FILE: &str = "refinements.jsonl";
const KINDS: [&str; 4] = ["prompt", "memory", "skill", "subagent"];
/// How many assistant turns pass between automatic reviews (prime-agent's default).
pub const AUTO_REFINE_TURN_INTERVAL: u32 = 25;
const REFINEMENT_MAX_OUTPUT_TOKENS: u32 = 8192;
const REVIEW_MAX_OUTPUT_TOKENS: u32 = 4096;
const CONVERSATION_CHARS: usize = 80_000;
const REVIEW_CONVERSATION_CHARS: usize = 40_000;
const TRUNCATED_JSON_ERROR: &str = "the model stopped before completing its JSON object. This usually means the output budget was exhausted; retry with a smaller request.";

const REFINEMENT_SYSTEM_PROMPT: &str = r#"You are the /refine continual harness subsystem of ha, a coding agent.

Your job is to improve the editable continual harness state from the current trajectory.
This is similar in spirit to context compaction, but instead of summarizing the
conversation you emit precise Create, Update, or Delete edits to reusable state.
The continual harness is the persistent, editable set of prompt notes, memories,
skills, and subagent specs that lets the agent improve reusable behavior
outside the token history.
Use "continual harness" for that persistent artifact layer; keep "RLM" for the
runtime, Python REPL kernel, and native call interface that executes those artifacts.

Continual harness components:
- prompt: supplemental prompt notes only. The base system prompt is immutable and MUST NOT be rewritten.
- memory: durable facts, decisions, failures, preferences, and outcomes.
- skill: installed Python REPL skill. Skill create/update edits MUST include a `reference` object with `{"type":"python"}`, a Python import, and a callable or call pattern; they also MUST include an `arguments` object describing accepted inputs, required fields, defaults, and constraints. Use `{}` for `arguments` only when the Python callable truly needs no external inputs. Include the RLM-native call form `await <skill_import>(...)`.
- subagent: reusable delegation specs, including purpose, instructions, and when to invoke. Include the RLM-native call form: compose a concise task prompt and spawn with `handle = await rlm.spawn("sub-task", name="worker")`; admission returns immediately with `rlm_child_id`, `name`, `session_dir`, and `model`, never the child's answer. Collect results with `await rlm.collect([handle], timeout_ms=...)` before the turn ends. Do not invent wrappers like `run_subagent(...)`.

Scope and persistence policy:
- The default editable continual harness store is local to the current conversation. Use it for session-specific progress, active task state, current-run coordination notes, temporary blockers, and project facts that should not affect other conversations.
- A caller may explicitly request global refinement. Global edits must be stable cross-session lessons, durable user preferences, reusable skills/subagents, or tool/environment facts that should affect future conversations.
- Entry ids in the harness overview may carry a display-only `local:` or `global:` prefix. Always use the bare id (no prefix) in edits.
- All edits in one refinement apply only to the requested scope's store. During a local refinement, global entries are read-only context: never propose update or delete edits for them; create a local entry instead when a session-specific override is genuinely needed.
- Project/workspace-specific lessons may be persisted globally only when the title, path, or content explicitly names the project/workspace and the lesson is likely to be reused in future conversations for that project. Prefer local edits when the lesson only belongs in the current conversation.
- Use memory for declarative facts and preferences, skill for repeatable procedures exposed as Python calls, prompt for narrow behavioral policy addendums, and subagent for reusable delegation roles.
- Create or update the smallest relevant component: repeated delegation roles should become subagent specs, repeated procedures should become skills, durable facts/preferences should become memories, and narrow behavioral policies should become prompt addendums.
- When an edit is persisted, include metadata such as `{"scope":"local"}` or `{"scope":"global"}` when that helps future review understand the intended blast radius.

Use the trajectory, current continual harness state, and prior refinement history. Prefer
small evidence-backed edits. If prior refinements caused issues, rollback or
replace the faulty editable entries. Never edit source files directly. Output
JSON only with this exact shape:

{
  "summary": "one sentence",
  "rationale": "why these edits are justified by trajectory evidence",
  "expectedOutcome": "what should improve and how to validate it",
  "edits": [
    {
      "action": "create|update|delete",
      "kind": "prompt|memory|skill|subagent",
      "id": "stable id for update/delete, optional for create",
      "title": "required for create/update except delete",
      "content": "required for create/update except delete",
      "path": "optional grouping path",
      "reference": {"type": "python", "import": "package.module", "callable": "function_name", "call_pattern": "await function_name(...)"},
      "arguments": {"name": {"type": "string", "required": true, "description": "accepted input"}},
      "metadata": {},
      "reason": "why this edit is useful"
    }
  ]
}"#;

const AUTO_REFINE_REVIEW_SYSTEM_PROMPT: &str = r#"You are the automatic /refine review gate of ha, a coding agent.

Decide whether this checkpoint should run /refine. Auto /refine writes local continual harness state by default, so approve when the trajectory contains evidence useful to this session's future turns.
Reject one-off noise, unsupported hypotheses, and transient tool outputs. Ask for global refinement only for durable cross-session lessons or explicitly project-qualified lessons likely to be reused in future sessions.

Return JSON only:
{
  "shouldRefine": true|false,
  "rationale": "short reason",
  "instructions": "optional concise instructions for /refine if shouldRefine is true"
}"#;

/// What one refinement is asked to do.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RefineOptions {
    pub instructions: Option<String>,
    pub global: bool,
    /// Roll back this refinement instead of planning a new one.
    pub rollback: Option<String>,
}

impl RefineOptions {
    /// `/refine [--global] [--rollback <id>] [instructions]`.
    #[must_use]
    pub fn parse(arguments: &str) -> Self {
        let mut options = Self::default();
        let mut words = arguments.split_whitespace().peekable();
        let mut rest = Vec::new();
        while let Some(word) = words.next() {
            match word {
                "--global" => options.global = true,
                "--rollback" => options.rollback = words.next().map(str::to_owned),
                _ => rest.push(word),
            }
        }
        let instructions = rest.join(" ");
        options.instructions = (!instructions.is_empty()).then_some(instructions);
        options
    }
}

/// Where the two scopes live.
#[derive(Clone, Debug)]
pub struct HarnessScopes {
    pub global: PathBuf,
    pub local: PathBuf,
}

impl HarnessScopes {
    fn dir(&self, global: bool) -> &Path {
        if global { &self.global } else { &self.local }
    }
}

fn empty_state() -> Value {
    json!({"schema": 1, "entries": {"prompt": {}, "memory": {}, "skill": {}, "subagent": {}}, "refinements": []})
}

/// The raw state of one scope; unknown fields are kept through a save.
fn load_raw(dir: &Path) -> Value {
    let mut state = std::fs::read_to_string(dir.join(STATE_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(Value::is_object)
        .unwrap_or_else(empty_state);
    for kind in KINDS {
        if !state["entries"][kind].is_object() {
            state["entries"][kind] = json!({});
        }
    }
    if !state["refinements"].is_array() {
        state["refinements"] = json!([]);
    }
    state
}

/// `saveHarnessState`: written to a temporary file and renamed, so a reader never
/// sees half a file.
fn save_raw(dir: &Path, state: &Value) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("harness state directory: {error}"))?;
    let path = dir.join(STATE_FILE);
    let temporary = dir.join(format!("{STATE_FILE}.tmp"));
    let text = serde_json::to_string_pretty(state).map_err(|error| error.to_string())? + "\n";
    std::fs::write(&temporary, text).map_err(|error| format!("harness state: {error}"))?;
    std::fs::rename(&temporary, &path).map_err(|error| format!("harness state: {error}"))?;
    Ok(path)
}

fn now() -> String {
    // RFC 3339 in UTC without a date crate: seconds since the epoch are enough to
    // order entries, and the digest never prints them.
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    format!("@{seconds}")
}

/// `slug`: lowercase ASCII words joined by `_`, at most 80 characters.
fn slug(raw: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for character in raw.trim().to_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('_');
            }
            gap = false;
            out.push(character);
        } else {
            gap = true;
        }
    }
    out.truncate(80);
    if out.is_empty() {
        fallback.to_owned()
    } else {
        out
    }
}

/// `overviewForPrompt`: every scope's entries, 40 per kind, 240 characters each.
fn overview(scopes: &HarnessScopes) -> String {
    let state = super::harness::merge(
        super::harness::load(&scopes.global, "global"),
        super::harness::load(&scopes.local, "local"),
    );
    let mut lines = Vec::new();
    for kind in KINDS {
        let entries = state.entries.get(kind).cloned().unwrap_or_default();
        lines.push(format!("{kind}: {}", entries.len()));
        for entry in entries.iter().take(40) {
            let (Some(title), Some(content)) = (&entry.title, &entry.content) else {
                lines.push(format!("- harness: skipped malformed entry {}", entry.id));
                continue;
            };
            let content = content.split_whitespace().collect::<Vec<_>>().join(" ");
            let content = content.chars().take(240).collect::<String>();
            let mut extra = String::new();
            if kind == "skill" && !entry.reference.is_empty() {
                let reference = Value::Object(entry.reference.clone()).to_string();
                let _ = write!(
                    extra,
                    " ref={}",
                    reference.chars().take(240).collect::<String>()
                );
            }
            if kind == "skill" && !entry.arguments.is_empty() {
                let arguments = Value::Object(entry.arguments.clone()).to_string();
                let _ = write!(
                    extra,
                    " args={}",
                    arguments.chars().take(240).collect::<String>()
                );
            }
            lines.push(format!(
                "- [{}:{}] {title} ({}, v{}){extra}: {content}",
                entry.scope, entry.id, entry.path, entry.version
            ));
        }
        if entries.len() > 40 {
            lines.push(format!("- +{} more {kind} entries", entries.len() - 40));
        }
    }
    lines.join("\n")
}

/// Every recorded refinement of both scopes, oldest first.
fn history(scopes: &HarnessScopes) -> Vec<Value> {
    let mut results = Vec::new();
    for dir in [&scopes.global, &scopes.local] {
        let Ok(text) = std::fs::read_to_string(dir.join(HISTORY_FILE)) else {
            continue;
        };
        results.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
                .filter(|result| result["id"].is_string() && result["appliedEdits"].is_array()),
        );
    }
    results
}

/// `historyForPrompt`.
fn history_for_prompt(history: &[Value]) -> String {
    if history.is_empty() {
        return "No prior refinement history.".to_owned();
    }
    history
        .iter()
        .skip(history.len().saturating_sub(20))
        .map(|item| {
            let edits = item["appliedEdits"]
                .as_array()
                .map(|edits| {
                    edits
                        .iter()
                        .map(|edit| {
                            format!(
                                "{} {} {}:{}",
                                if edit["applied"] == true {
                                    "applied"
                                } else {
                                    "failed"
                                },
                                edit["action"].as_str().unwrap_or("?"),
                                edit["kind"].as_str().unwrap_or("?"),
                                edit["id"].as_str().unwrap_or("?")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let rollback = item["rollbackOf"]
                .as_str()
                .map(|id| format!(" rollbackOf={id}"))
                .unwrap_or_default();
            format!(
                "[{}]{rollback} {}\n{edits}\nExpected outcome: {}",
                item["id"].as_str().unwrap_or("?"),
                item["summary"].as_str().unwrap_or(""),
                item["expectedOutcome"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Whether a JSON candidate ends mid-value (`isIncompleteJson`).
fn incomplete_json(candidate: &str) -> bool {
    let (mut depth, mut in_string, mut escaped) = (0_i64, false, false);
    for character in candidate.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_string {
            match character {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' | '[' => depth += 1,
            '}' | ']' => depth -= 1,
            _ => {}
        }
    }
    in_string || depth > 0
}

fn parse_candidate(candidate: &str) -> Result<Value, String> {
    serde_json::from_str(candidate).map_err(|error| {
        if incomplete_json(candidate) {
            TRUNCATED_JSON_ERROR.to_owned()
        } else {
            format!("the model did not return valid JSON: {error}")
        }
    })
}

/// `extractJsonObject`: the whole reply, a fenced block, or the outermost braces.
fn extract_json(text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return parse_candidate(trimmed);
    }
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        let after = after.strip_prefix("json").unwrap_or(after);
        if let Some(end) = after.find("```") {
            return parse_candidate(after[..end].trim());
        }
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}'))
        && end > start
    {
        return serde_json::from_str(&trimmed[start..=end])
            .or_else(|_| parse_candidate(&trimmed[start..]));
    }
    if incomplete_json(trimmed) {
        return Err(TRUNCATED_JSON_ERROR.to_owned());
    }
    Err("the refiner did not return a JSON object".to_owned())
}

/// One proposed edit, kept as the model wrote it for apply-time validation.
#[derive(Clone, Debug)]
struct Edit(Map<String, Value>);

impl Edit {
    fn text(&self, field: &str) -> Option<&str> {
        self.0.get(field).and_then(Value::as_str)
    }

    /// `validateEdit`.
    fn invalid(&self, id: &str) -> Option<String> {
        let action = self.text("action").unwrap_or_default();
        let kind = self.text("kind").unwrap_or_default();
        if !matches!(action, "create" | "update" | "delete") {
            return Some(format!("unsupported action {action}"));
        }
        if !KINDS.contains(&kind) {
            return Some(format!("unsupported kind {kind}"));
        }
        if kind == "prompt" && id == "base_system_prompt" {
            return Some("base system prompt is not editable".to_owned());
        }
        if action != "create" && id.is_empty() {
            return Some(format!("{action} requires id"));
        }
        if let Some(path) = self.0.get("path")
            && path.as_str().is_none_or(str::is_empty)
        {
            return Some(format!(
                "{action} requires path to be a non-empty string when provided"
            ));
        }
        if action != "delete"
            && !(self.text("title").is_some_and(|title| !title.is_empty())
                && self
                    .text("content")
                    .is_some_and(|content| !content.is_empty()))
        {
            return Some(format!(
                "{action} requires title and content to be non-empty strings"
            ));
        }
        for field in ["reference", "arguments", "metadata"] {
            if self.0.get(field).is_some_and(|value| !value.is_object()) {
                return Some(format!(
                    "{action} requires {field} to be an object when provided"
                ));
            }
        }
        if action != "delete" && kind == "skill" {
            if !self.0.contains_key("arguments") {
                return Some(format!("{action} skill requires arguments"));
            }
            let Some(reference) = self.0.get("reference").and_then(Value::as_object) else {
                return Some(format!("{action} skill requires python reference"));
            };
            if reference.get("type").and_then(Value::as_str) != Some("python") {
                return Some(format!("{action} skill reference.type must be python"));
            }
            let has = |fields: [&str; 2]| {
                fields.iter().any(|field| {
                    reference
                        .get(*field)
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.is_empty())
                })
            };
            if !has(["import", "python_import"]) {
                return Some(format!("{action} skill requires python import"));
            }
            if !has(["callable", "call_pattern"]) {
                return Some(format!("{action} skill requires callable or call_pattern"));
            }
        }
        None
    }
}

/// A proposal the model made (`RefinementProposal`).
#[derive(Clone, Debug)]
struct Proposal {
    summary: String,
    rationale: String,
    expected_outcome: String,
    edits: Vec<Edit>,
}

impl Proposal {
    /// `normalizeRefinementProposal`.
    fn from_value(value: &Value) -> Self {
        let text = |field: &str| value[field].as_str().map(str::to_owned);
        Self {
            summary: text("summary")
                .unwrap_or_else(|| "Refined continual harness state".to_owned()),
            rationale: text("rationale").unwrap_or_default(),
            expected_outcome: text("expectedOutcome").unwrap_or_default(),
            edits: value["edits"]
                .as_array()
                .map(|edits| {
                    edits
                        .iter()
                        .filter_map(|edit| edit.as_object().cloned().map(Edit))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// The outcome of one refinement, as recorded and reported.
#[derive(Clone, Debug)]
pub struct Refinement {
    pub record: Value,
}

impl Refinement {
    /// `formatRefinementNoticeBody`.
    #[must_use]
    pub fn notice(&self) -> String {
        let compact = |text: &str| {
            let folded = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if folded.chars().count() <= 180 {
                folded
            } else {
                format!("{}...", folded.chars().take(177).collect::<String>())
            }
        };
        let mut lines = vec![compact(self.record["summary"].as_str().unwrap_or(""))];
        let scope = self.record["scope"].as_str().unwrap_or("local");
        for edit in self.record["appliedEdits"].as_array().into_iter().flatten() {
            if edit["applied"] != true {
                continue;
            }
            let entry = if edit["after"].is_object() {
                &edit["after"]
            } else {
                &edit["before"]
            };
            lines.push(format!(
                "- {} {} [{}:{}] {}: {}",
                edit["action"].as_str().unwrap_or("?"),
                edit["kind"].as_str().unwrap_or("?"),
                entry["scope"].as_str().unwrap_or(scope),
                edit["id"].as_str().unwrap_or("?"),
                entry["title"]
                    .as_str()
                    .unwrap_or(edit["id"].as_str().unwrap_or("?")),
                compact(entry["content"].as_str().unwrap_or(""))
            ));
        }
        let failed = self.record["appliedEdits"].as_array().map_or(0, |edits| {
            edits.iter().filter(|edit| edit["applied"] != true).count()
        });
        if failed > 0 {
            lines.push(format!("({failed} edit(s) were refused)"));
        }
        lines.join("\n")
    }

    #[must_use]
    pub fn id(&self) -> &str {
        self.record["id"].as_str().unwrap_or_default()
    }
}

/// `applyRefinementProposal`, on the target scope's file, with `baseline` the state
/// read before planning: an entry changed meanwhile is not overwritten.
#[allow(
    clippy::too_many_lines,
    reason = "applyRefinementProposal, told in prime-agent's order"
)]
fn apply(
    scopes: &HarnessScopes,
    proposal: &Proposal,
    id: &str,
    global: bool,
    rollback_of: Option<&str>,
    baseline: Option<&Value>,
) -> Result<Refinement, String> {
    let dir = scopes.dir(global);
    let scope = if global { "global" } else { "local" };
    let mut state = load_raw(dir);
    let mut applied_edits = Vec::new();
    let mut modified = std::collections::BTreeSet::new();
    for edit in &proposal.edits {
        let action = edit.text("action").unwrap_or_default().to_owned();
        let kind = edit.text("kind").unwrap_or_default().to_owned();
        let computed = edit
            .text("id")
            .map(str::to_owned)
            .or_else(|| {
                (action == "create").then(|| slug(edit.text("title").unwrap_or(&kind), &kind))
            })
            .unwrap_or_default();
        let mut record = Value::Object(edit.0.clone());
        record["id"] = json!(computed);
        if let Some(error) = edit.invalid(&computed) {
            record["applied"] = json!(false);
            record["error"] = json!(error);
            applied_edits.push(record);
            continue;
        }
        let before = state["entries"][&kind][&computed].clone();
        let key = format!("{kind}:{computed}");
        if let Some(baseline) = baseline
            && !modified.contains(&key)
            && before != baseline["entries"][&kind][&computed]
        {
            record["before"] = before;
            record["applied"] = json!(false);
            record["error"] = json!("entry changed during refinement planning");
            applied_edits.push(record);
            continue;
        }
        let error = match action.as_str() {
            "delete" | "update" if before.is_null() => Some("entry not found"),
            "create" if !before.is_null() => Some("entry already exists"),
            _ => None,
        };
        if let Some(error) = error {
            if !before.is_null() {
                record["before"] = before;
            }
            record["applied"] = json!(false);
            record["error"] = json!(error);
            applied_edits.push(record);
            continue;
        }
        if action == "delete" {
            if let Some(records) = state["entries"][&kind].as_object_mut() {
                records.remove(&computed);
            }
            modified.insert(key);
            record["before"] = before;
            record["applied"] = json!(true);
            applied_edits.push(record);
            continue;
        }
        let field = |name: &str, fallback: Value| {
            edit.0
                .get(name)
                .cloned()
                .filter(|value| !value.is_null())
                .unwrap_or(fallback)
        };
        let after = json!({
            "id": computed,
            "kind": kind,
            "title": field("title", before["title"].clone()),
            "content": field("content", before["content"].clone()),
            "path": field("path", before.get("path").cloned().unwrap_or_else(|| json!("general"))),
            "scope": before.get("scope").cloned().unwrap_or_else(|| json!(scope)),
            "reference": field("reference", before.get("reference").cloned().unwrap_or_else(|| json!({}))),
            "arguments": field("arguments", before.get("arguments").cloned().unwrap_or_else(|| json!({}))),
            "metadata": field("metadata", before.get("metadata").cloned().unwrap_or_else(|| json!({}))),
            "source": "refine",
            "created_at": before.get("created_at").cloned().unwrap_or_else(|| json!(now())),
            "updated_at": now(),
            "version": before["version"].as_i64().map_or(1, |version| version + 1),
        });
        state["entries"][&kind][&computed] = after.clone();
        modified.insert(key);
        if !before.is_null() {
            record["before"] = before;
        }
        record["after"] = after;
        record["applied"] = json!(true);
        applied_edits.push(record);
    }
    let changes = applied_edits
        .iter()
        .filter(|edit| edit["applied"] == true)
        .map(|edit| {
            format!(
                "{} {}:{}",
                edit["action"].as_str().unwrap_or("?"),
                edit["kind"].as_str().unwrap_or("?"),
                edit["id"].as_str().unwrap_or("?")
            )
        })
        .collect::<Vec<_>>();
    if let Some(refinements) = state["refinements"].as_array_mut() {
        refinements.push(json!({
            "id": id,
            "trigger": proposal.summary,
            "changes": changes,
            "evidence": proposal.rationale,
            "outcome": proposal.expected_outcome,
            "created_at": now(),
        }));
    }
    let path = save_raw(dir, &state)?;
    let record = json!({
        "id": id,
        "summary": proposal.summary,
        "rationale": proposal.rationale,
        "expectedOutcome": proposal.expected_outcome,
        "appliedEdits": applied_edits,
        "harnessStatePath": path.display().to_string(),
        "rollbackOf": rollback_of,
        "scope": scope,
    });
    // The history is what `--rollback` reads.
    let mut line = record.to_string();
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(HISTORY_FILE))
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .map_err(|error| format!("refinement history: {error}"))?;
    Ok(Refinement { record })
}

/// `rollbackProposal`: restore each applied edit's snapshot, newest first.
fn rollback_proposal(target: &Value) -> Proposal {
    let id = target["id"].as_str().unwrap_or("?");
    let mut edits = Vec::new();
    for edit in target["appliedEdits"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
    {
        if edit["applied"] != true {
            continue;
        }
        let before = &edit["before"];
        let mut restored = Map::new();
        restored.insert("kind".to_owned(), edit["kind"].clone());
        restored.insert("id".to_owned(), edit["id"].clone());
        restored.insert("reason".to_owned(), json!(format!("Rollback {id}")));
        if before.is_object() {
            restored.insert(
                "action".to_owned(),
                json!(if edit["after"].is_object() {
                    "update"
                } else {
                    "create"
                }),
            );
            for field in [
                "title",
                "content",
                "path",
                "reference",
                "arguments",
                "metadata",
            ] {
                if !before[field].is_null() {
                    restored.insert(field.to_owned(), before[field].clone());
                }
            }
        } else if edit["after"].is_object() {
            restored.insert("action".to_owned(), json!("delete"));
        } else {
            continue;
        }
        edits.push(Edit(restored));
    }
    Proposal {
        summary: format!("Rollback refinement {id}"),
        rationale: format!("Restores continual harness state snapshots from refinement {id}."),
        expected_outcome: "Faulty refinement edits are reverted.".to_owned(),
        edits,
    }
}

/// `generateRefinementId`.
fn refinement_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("refine_{nanos}")
}

/// The last `limit` characters of the conversation, marked when cut.
fn tail(conversation: &str, limit: usize) -> String {
    let count = conversation.chars().count();
    if count <= limit {
        return conversation.to_owned();
    }
    let kept = conversation.chars().skip(count - limit).collect::<String>();
    format!("[Earlier conversation omitted to fit the model context.]\n{kept}")
}

/// One model call, answered as text.
async fn complete(
    provider: &Arc<dyn ModelProvider>,
    model: &str,
    system: &str,
    user: String,
    max_tokens: u32,
) -> Result<String, String> {
    let request = ProviderRequest::new(
        RequestId::generate(),
        model,
        vec![
            ProviderMessage::new(MessageRole::System, system),
            ProviderMessage::new(MessageRole::User, user),
        ],
    )
    .with_max_output_tokens(max_tokens);
    let events = tokio::time::timeout(
        std::time::Duration::from_mins(5),
        provider.stream(request, CancellationToken::new()),
    )
    .await
    .map_err(|_| "the refinement model call timed out".to_owned())?
    .map_err(|error| format!("the refinement model call failed: {error}"))?;
    let response =
        harness_providers::assemble_stream(&events).map_err(|error| error.to_string())?;
    if response.finish_reason.as_deref() == Some("length") {
        return Err(TRUNCATED_JSON_ERROR.to_owned());
    }
    Ok(response.text)
}

/// Run one refinement: plan with the model (or build the rollback), then apply to the
/// requested scope. `conversation` is the conversation serialized as `[User]: ...` /
/// `[Assistant]: ...` blocks.
pub async fn refine(
    provider: &Arc<dyn ModelProvider>,
    model: &str,
    conversation: &str,
    scopes: &HarnessScopes,
    options: &RefineOptions,
) -> Result<Refinement, String> {
    let history = history(scopes);
    let id = refinement_id();
    if let Some(target_id) = &options.rollback {
        let target = history
            .iter()
            .find(|item| item["id"].as_str() == Some(target_id.as_str()))
            .ok_or_else(|| format!("Refinement {target_id} not found"))?;
        let global = target["scope"]
            .as_str()
            .map_or(options.global, |scope| scope == "global");
        return apply(
            scopes,
            &rollback_proposal(target),
            &id,
            global,
            Some(target_id),
            None,
        );
    }
    let scope_instruction = if options.global {
        "Requested refinement scope: global. Only propose stable cross-session continual harness edits, durable user preferences, reusable skills/subagents, or explicitly project-qualified facts that should affect future conversations. Do not persist session-only progress, temporary blockers, or current-run coordination globally."
    } else {
        "Requested refinement scope: local. Prefer local continual harness edits for current task progress, temporary blockers, current-run coordination, and project facts that are not clearly reusable across conversations. Global entries in the overview are read-only context: do not propose update or delete edits for them; create a local entry instead if an override is needed."
    };
    let baseline = load_raw(scopes.dir(options.global));
    let mut parts = vec![
        format!(
            "<current_harness_state>\n{}\n</current_harness_state>",
            overview(scopes)
        ),
        format!(
            "<refinement_history>\n{}\n</refinement_history>",
            history_for_prompt(&history)
        ),
        format!(
            "<conversation>\n{}\n</conversation>",
            tail(conversation, CONVERSATION_CHARS)
        ),
        format!("<scope_policy>\n{scope_instruction}\n</scope_policy>"),
    ];
    if let Some(instructions) = &options.instructions {
        parts.push(format!(
            "<user_refine_instructions>\n{instructions}\n</user_refine_instructions>"
        ));
    }
    parts.push("Return only JSON edits. If no useful edit is justified, return an empty edits array with a rationale.".to_owned());
    let text = complete(
        provider,
        model,
        REFINEMENT_SYSTEM_PROMPT,
        parts.join("\n\n"),
        REFINEMENT_MAX_OUTPUT_TOKENS,
    )
    .await?;
    let value = extract_json(&text)?;
    if !value.is_object() {
        return Err("the refiner JSON must be an object".to_owned());
    }
    apply(
        scopes,
        &Proposal::from_value(&value),
        &id,
        options.global,
        None,
        Some(&baseline),
    )
}

/// The automatic review's verdict (`AutoRefineReview`).
#[derive(Clone, Debug)]
pub struct Review {
    pub should_refine: bool,
    pub rationale: String,
    pub instructions: Option<String>,
}

/// `reviewAutoRefine`: ask whether this checkpoint should refine.
pub async fn review(
    provider: &Arc<dyn ModelProvider>,
    model: &str,
    conversation: &str,
    scopes: &HarnessScopes,
    turns_since_last_review: u32,
) -> Result<Review, String> {
    let history = history(scopes);
    let prompt = [
        format!("<trigger>\nturn_interval; {turns_since_last_review} assistant turns since last auto-refine review\n</trigger>"),
        format!("<current_harness_state>\n{}\n</current_harness_state>", overview(scopes)),
        format!("<refinement_history>\n{}\n</refinement_history>", history_for_prompt(&history)),
        format!("<conversation>\n{}\n</conversation>", tail(conversation, REVIEW_CONVERSATION_CHARS)),
        "Return shouldRefine=true when the trajectory contains evidence useful to this session's future turns. Prefer local harness edits for current task progress, temporary blockers, and current-run coordination. Ask for global refinement only for durable cross-session lessons or explicitly project-qualified facts likely to be reused in future sessions.".to_owned(),
    ]
    .join("\n\n");
    let text = complete(
        provider,
        model,
        AUTO_REFINE_REVIEW_SYSTEM_PROMPT,
        prompt,
        REVIEW_MAX_OUTPUT_TOKENS,
    )
    .await?;
    let value = extract_json(&text)?;
    Ok(Review {
        should_refine: value["shouldRefine"] == true,
        rationale: value["rationale"]
            .as_str()
            .unwrap_or("No rationale provided.")
            .to_owned(),
        instructions: value["instructions"].as_str().map(str::to_owned),
    })
}

/// A conversation's turns as the refiner reads them (`serializeConversation`).
#[must_use]
pub fn serialize_turns(turns: &[(String, String)]) -> String {
    turns
        .iter()
        .flat_map(|(question, answer)| {
            [
                (!question.is_empty()).then(|| format!("[User]: {question}")),
                (!answer.is_empty()).then(|| format!("[Assistant]: {answer}")),
            ]
        })
        .flatten()
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::{
        HarnessScopes, Proposal, RefineOptions, apply, extract_json, load_raw, rollback_proposal,
        slug,
    };
    use serde_json::json;

    fn scopes(root: &std::path::Path) -> HarnessScopes {
        HarnessScopes {
            global: root.join("global"),
            local: root.join("local"),
        }
    }

    #[test]
    fn options_parse_like_the_slash_command() {
        assert_eq!(
            RefineOptions::parse("--global keep the tab rule"),
            RefineOptions {
                instructions: Some("keep the tab rule".to_owned()),
                global: true,
                rollback: None,
            }
        );
        assert_eq!(
            RefineOptions::parse("--rollback refine_1")
                .rollback
                .as_deref(),
            Some("refine_1")
        );
        assert_eq!(
            slug("  Use Tabs, not spaces! ", "memory"),
            "use_tabs_not_spaces"
        );
        assert_eq!(slug("!!!", "memory"), "memory");
    }

    #[test]
    fn json_is_found_in_prose_or_fences_and_truncation_is_named() {
        assert_eq!(extract_json("{\"a\": 1}").expect("plain")["a"], 1);
        assert_eq!(
            extract_json("Here:\n```json\n{\"a\": 2}\n```").expect("fenced")["a"],
            2
        );
        assert_eq!(
            extract_json("result {\"a\": 3} done").expect("braces")["a"],
            3
        );
        let cut = extract_json("{\"summary\": \"x\", \"edits\": [{\"action\"").expect_err("cut");
        assert!(cut.contains("stopped before completing"), "{cut}");
    }

    /// `applyRefinementProposal`: valid edits land in the requested scope with a
    /// version, invalid ones are refused with the reason, and a rollback restores
    /// the snapshot.
    #[test]
    fn edits_are_validated_applied_and_rolled_back() {
        let directory = tempfile::tempdir().expect("dir");
        let scopes = scopes(directory.path());
        let proposal = Proposal::from_value(&json!({
            "summary": "remember the indentation rule",
            "rationale": "the user corrected it twice",
            "expectedOutcome": "tabs are used",
            "edits": [
                {"action": "create", "kind": "memory", "title": "Indentation", "content": "The user wants tabs", "path": "prefs"},
                {"action": "create", "kind": "skill", "title": "Formatter", "content": "runs fmt"},
                {"action": "update", "kind": "prompt", "id": "base_system_prompt", "title": "x", "content": "y"},
                {"action": "delete", "kind": "memory", "id": "missing"}
            ]
        }));
        let result = apply(&scopes, &proposal, "refine_1", false, None, None).expect("applied");
        let state = load_raw(&scopes.local);
        assert_eq!(
            state["entries"]["memory"]["indentation"]["content"],
            "The user wants tabs"
        );
        assert_eq!(state["entries"]["memory"]["indentation"]["version"], 1);
        assert_eq!(state["entries"]["memory"]["indentation"]["scope"], "local");
        let edits = result.record["appliedEdits"].as_array().expect("edits");
        assert_eq!(edits[1]["error"], "create skill requires arguments");
        assert_eq!(edits[2]["error"], "base system prompt is not editable");
        assert_eq!(edits[3]["error"], "entry not found");
        assert!(
            result
                .notice()
                .contains("create memory [local:indentation] Indentation"),
            "{}",
            result.notice()
        );
        assert!(result.notice().contains("(3 edit(s) were refused)"));
        // The digest reads what refine wrote.
        let digest_state = crate::interactive::harness::load(&scopes.local, "local");
        assert_eq!(digest_state.entries["memory"].len(), 1);

        let update = Proposal::from_value(&json!({
            "summary": "tighten", "edits": [
                {"action": "update", "kind": "memory", "id": "indentation", "title": "Indentation", "content": "Tabs, width 4"}
            ]
        }));
        let second = apply(&scopes, &update, "refine_2", false, None, None).expect("updated");
        assert_eq!(
            load_raw(&scopes.local)["entries"]["memory"]["indentation"]["version"],
            2
        );
        let rolled = apply(
            &scopes,
            &rollback_proposal(&second.record),
            "refine_3",
            false,
            Some("refine_2"),
            None,
        )
        .expect("rolled back");
        let restored = load_raw(&scopes.local);
        assert_eq!(
            restored["entries"]["memory"]["indentation"]["content"],
            "The user wants tabs"
        );
        assert_eq!(rolled.record["rollbackOf"], "refine_2");
        assert_eq!(super::history(&scopes).len(), 3);
    }

    /// The whole pass: the model answers with edits, the host applies them; the
    /// automatic review reads its verdict the same way.
    #[tokio::test]
    async fn a_model_proposal_is_applied_and_a_review_is_read() {
        let directory = tempfile::tempdir().expect("dir");
        let scopes = scopes(directory.path());
        let provider: std::sync::Arc<dyn harness_providers::ModelProvider> =
            std::sync::Arc::new(harness_providers::MockProvider::text(
                "Here are the edits:
```json
{\"summary\": \"keep the tab rule\", \"rationale\": \"asked twice\", \"expectedOutcome\": \"tabs\", \"edits\": [{\"action\": \"create\", \"kind\": \"memory\", \"title\": \"Tabs\", \"content\": \"Use tabs\"}]}
```",
            ));
        let result = super::refine(
            &provider,
            "fixture-model",
            "[User]: use tabs

[Assistant]: ok",
            &scopes,
            &RefineOptions::parse("--global"),
        )
        .await
        .expect("refined");
        assert_eq!(result.record["scope"], "global");
        assert_eq!(
            load_raw(&scopes.global)["entries"]["memory"]["tabs"]["content"],
            "Use tabs"
        );
        let review_provider: std::sync::Arc<dyn harness_providers::ModelProvider> =
            std::sync::Arc::new(harness_providers::MockProvider::text(
                "{\"shouldRefine\": true, \"rationale\": \"a durable preference\", \"instructions\": \"record it\"}",
            ));
        let review = super::review(
            &review_provider,
            "fixture-model",
            "conversation",
            &scopes,
            25,
        )
        .await
        .expect("reviewed");
        assert!(review.should_refine);
        assert_eq!(review.instructions.as_deref(), Some("record it"));
    }

    #[test]
    fn an_entry_changed_while_planning_is_not_overwritten() {
        let directory = tempfile::tempdir().expect("dir");
        let scopes = scopes(directory.path());
        let create = Proposal::from_value(&json!({"edits": [
            {"action": "create", "kind": "memory", "id": "note", "title": "Note", "content": "one"}
        ]}));
        apply(&scopes, &create, "r1", false, None, None).expect("created");
        let baseline = load_raw(&scopes.local);
        let concurrent = Proposal::from_value(&json!({"edits": [
            {"action": "update", "kind": "memory", "id": "note", "title": "Note", "content": "two"}
        ]}));
        apply(&scopes, &concurrent, "r2", false, None, None).expect("changed meanwhile");
        let planned = Proposal::from_value(&json!({"edits": [
            {"action": "update", "kind": "memory", "id": "note", "title": "Note", "content": "three"}
        ]}));
        let result = apply(&scopes, &planned, "r3", false, None, Some(&baseline)).expect("applied");
        assert_eq!(
            result.record["appliedEdits"][0]["error"],
            "entry changed during refinement planning"
        );
        assert_eq!(
            load_raw(&scopes.local)["entries"]["memory"]["note"]["content"],
            "two"
        );
    }
}
