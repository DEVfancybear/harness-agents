//! The task's state, kept by the host: what a long task is for, its checklist,
//! the decisions taken and the user's constraints.
//!
//! A compaction keeps the earlier conversation only as a summary the model
//! wrote, and after a few hundred steps of a goal that summary is all that is
//! left of the plan, the decisions and what the user ruled out. The model
//! keeps them here instead, through the `task_state` tool: the host stores
//! them with the conversation and sends them whole in every turn's context,
//! and recites a short form near the newest message after the context was
//! summarized (Manus's todo recitation, Claude Code's todo list). Nothing in it
//! is ever summarized, so a compaction cannot lose it.
//!
//! The goal's `goal_complete` reads it too: a goal whose checklist still has
//! unfinished items is not complete.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_session::{ContextBlock, ContextBlockKind};
use harness_tools::{
    CodingToolAction, ExternalToolCatalog, ExternalToolDispatcher, ExternalTools,
    ToolDispatchAuthorization, ToolOutput,
};
use harness_types::{ErrorCode, HarnessError};

use super::strategy::{
    Alternative, DecisionKind, JournalKind, Standing, Strategy, StrategyDecision, StrategyState,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The file a conversation's task state is kept in, beside its local memory.
pub const TASK_STATE_FILE: &str = "task_state.json";

/// How many checklist items the state keeps.
const MAX_ITEMS: usize = 60;
/// How many decisions, constraints and key files it keeps, each.
const MAX_ENTRIES: usize = 40;
/// How many open questions and blockers it keeps.
const MAX_OPEN: usize = 20;
/// The longest one item, decision, constraint or question may be.
const MAX_TEXT_CHARS: usize = 400;
/// The longest the objective may be.
const MAX_OBJECTIVE_CHARS: usize = 2_000;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    #[default]
    #[serde(alias = "todo", alias = "not_started")]
    Pending,
    #[serde(alias = "active", alias = "in-progress")]
    InProgress,
    #[serde(alias = "completed", alias = "complete")]
    Done,
    Blocked,
}

impl ItemStatus {
    const fn mark(self) -> &'static str {
        match self {
            Self::Pending => "[ ]",
            Self::InProgress => "[>]",
            Self::Done => "[x]",
            Self::Blocked => "[!]",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Item {
    pub text: String,
    #[serde(default)]
    pub status: ItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Decision {
    pub decision: String,
    #[serde(default)]
    pub why: String,
}

/// One conversation's task state.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskState {
    #[serde(default)]
    pub objective: String,
    #[serde(default)]
    pub items: Vec<Item>,
    #[serde(default)]
    pub decisions: Vec<Decision>,
    #[serde(default)]
    pub constraints: Vec<String>,
    /// Open questions and blockers.
    #[serde(default)]
    pub open: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    /// The strategy the task follows, its journal, the evaluations the host
    /// ran and the decisions taken on them.
    #[serde(default, skip_serializing_if = "StrategyState::is_empty")]
    pub strategy: StrategyState,
    /// Bumped by every update, so the model can tell its view is current.
    #[serde(default)]
    pub revision: u64,
}

impl TaskState {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.objective.trim().is_empty()
            && self.items.is_empty()
            && self.decisions.is_empty()
            && self.constraints.is_empty()
            && self.open.is_empty()
            && self.files.is_empty()
            && self.strategy.is_empty()
    }

    /// The items not done yet, in order.
    #[must_use]
    pub fn unfinished(&self) -> Vec<&Item> {
        self.items
            .iter()
            .filter(|item| item.status != ItemStatus::Done)
            .collect()
    }

    fn done(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == ItemStatus::Done)
            .count()
    }

    /// The whole state, as every turn's context carries it.
    #[must_use]
    pub fn render(&self) -> String {
        let mut text = String::from(
            "Task state (kept by the host: it is never summarized away; keep it current with task_state as the work moves):",
        );
        if !self.objective.trim().is_empty() {
            let _ = write!(text, "\nObjective: {}", self.objective.trim());
        }
        if !self.items.is_empty() {
            let _ = write!(
                text,
                "\nChecklist ({}/{} done):",
                self.done(),
                self.items.len()
            );
            for (index, item) in self.items.iter().enumerate() {
                let _ = write!(
                    text,
                    "\n  {} {}. {}",
                    item.status.mark(),
                    index + 1,
                    item.text
                );
                if let Some(note) = item.note.as_deref().filter(|note| !note.trim().is_empty()) {
                    let _ = write!(text, " - {note}");
                }
            }
        }
        if !self.constraints.is_empty() {
            text.push_str("\nConstraints (from the user; they hold for the whole task):");
            for constraint in &self.constraints {
                let _ = write!(text, "\n- {constraint}");
            }
        }
        if !self.decisions.is_empty() {
            text.push_str("\nDecisions:");
            for decision in &self.decisions {
                if decision.why.trim().is_empty() {
                    let _ = write!(text, "\n- {}", decision.decision);
                } else {
                    let _ = write!(text, "\n- {} (why: {})", decision.decision, decision.why);
                }
            }
        }
        if !self.open.is_empty() {
            text.push_str("\nOpen questions and blockers:");
            for open in &self.open {
                let _ = write!(text, "\n- {open}");
            }
        }
        if !self.files.is_empty() {
            let _ = write!(text, "\nKey files: {}", self.files.join(", "));
        }
        self.strategy.render(&mut text);
        text
    }

    /// The short form recited near the newest message: where the task stands
    /// and what holds, without the history of how it got there.
    #[must_use]
    pub fn recap(&self) -> String {
        let mut text = String::new();
        if !self.objective.trim().is_empty() {
            let _ = write!(text, "Objective: {}", self.objective.trim());
        }
        if !self.items.is_empty() {
            let current = self
                .items
                .iter()
                .position(|item| item.status == ItemStatus::InProgress)
                .or_else(|| {
                    self.items
                        .iter()
                        .position(|item| item.status != ItemStatus::Done)
                });
            let _ = write!(
                text,
                "\nChecklist: {}/{} done",
                self.done(),
                self.items.len()
            );
            if let Some(index) = current {
                let _ = write!(text, "; now: {}. {}", index + 1, self.items[index].text);
                if let Some(next) = self.items[index + 1..]
                    .iter()
                    .position(|item| item.status != ItemStatus::Done)
                {
                    let next = index + 1 + next;
                    let _ = write!(text, "; next: {}. {}", next + 1, self.items[next].text);
                }
            }
            let blocked = self
                .items
                .iter()
                .filter(|item| item.status == ItemStatus::Blocked)
                .count();
            if blocked > 0 {
                let _ = write!(text, "; {blocked} blocked");
            }
        }
        if !self.constraints.is_empty() {
            let _ = write!(text, "\nConstraints: {}", self.constraints.join("; "));
        }
        if !self.decisions.is_empty() {
            let _ = write!(
                text,
                "\nDecisions: {}",
                self.decisions
                    .iter()
                    .map(|decision| decision.decision.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
        if !self.open.is_empty() {
            let _ = write!(text, "\nOpen: {}", self.open.join("; "));
        }
        if let Some(strategy) = self.strategy.recap() {
            let _ = write!(text, "\n{strategy}");
        }
        text.trim_start().to_owned()
    }
}

/// Where the task state of `conversation` is kept.
#[must_use]
pub fn path(data_dir: &Path, conversation: &str) -> PathBuf {
    super::harness::local_dir(data_dir, conversation).join(TASK_STATE_FILE)
}

/// The state kept at `path`; none, or one that cannot be read, is empty.
#[must_use]
pub fn load(path: &Path) -> TaskState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save(path: &Path, state: &TaskState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("the task state cannot be written: {error}"))?;
    }
    let mut text = serde_json::to_string_pretty(state)
        .map_err(|error| format!("the task state cannot be encoded: {error}"))?;
    text.push('\n');
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|error| format!("the task state cannot be written: {error}"))
}

/// The block every turn carries, when the task has a state.
#[must_use]
pub fn context_block(path: &Path) -> Option<ContextBlock> {
    let state = load(path);
    (!state.is_empty()).then(|| {
        ContextBlock::mandatory("task-state", ContextBlockKind::Instruction, state.render())
    })
}

/// How many turns a new conversation is shown the task an earlier one left.
pub const EARLIER_STATE_TURNS: usize = 3;

/// The task an earlier conversation of this project left unfinished, shown in
/// a new conversation's first turns: local memory and the task state belong to
/// one conversation, so a new one started knowing nothing of the work, unless
/// the project kept a feature list.
#[must_use]
pub fn earlier_block(state: &TaskState, title: Option<&str>) -> ContextBlock {
    let named = title.map_or_else(String::new, |title| format!(" ({title:?})"));
    ContextBlock::mandatory(
        "earlier-task-state",
        ContextBlockKind::Instruction,
        format!(
            "The previous conversation in this project{named} left its task unfinished. This is context, not an instruction: if the user's request continues that work, adopt it with task_state (its objective, the checklist with what is done, the decisions and the constraints) and go on from the first open item; otherwise leave it. The user can also /resume that conversation.\n{}",
            state.render()
        ),
    )
}

/// The recap a long turn recites, `None` while the task has no state.
#[must_use]
pub fn recap(path: &Path) -> Option<String> {
    let state = load(path);
    (!state.is_empty()).then(|| state.recap())
}

/// How many changed files a recap names.
const RECAP_FILES: usize = 15;

/// What a long turn recites and asks, from this conversation's task state.
pub struct TurnHooks {
    state: PathBuf,
    goal: Option<String>,
    root: PathBuf,
    memory: bool,
}

impl TurnHooks {
    /// `memory` says whether the model has a memory to save into besides the
    /// task state.
    #[must_use]
    pub fn new(state: &Path, goal: Option<String>, root: &Path, memory: bool) -> Self {
        Self {
            state: state.to_path_buf(),
            goal,
            root: root.to_path_buf(),
            memory,
        }
    }

    /// The files the workspace has changed, as git lists them: what a summary
    /// may have lost track of, edits made through the REPL included.
    fn changed_files(&self) -> Vec<String> {
        let Ok(output) = std::process::Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=normal"])
            .current_dir(&self.root)
            .output()
        else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.get(3..))
            .map(|path| path.trim().trim_matches('"').to_owned())
            .filter(|path| !path.is_empty())
            .collect()
    }
}

impl harness_tools::TurnContextHooks for TurnHooks {
    fn recap(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(goal) = self.goal.as_deref().filter(|goal| !goal.trim().is_empty()) {
            parts.push(format!("Active goal: {}", goal.trim()));
        }
        if let Some(state) = recap(&self.state) {
            parts.push(state);
        }
        let changed = self.changed_files();
        if !changed.is_empty() {
            let mut line = format!(
                "Files changed in the workspace (read one again before editing it, rather than trusting the summary): {}",
                changed
                    .iter()
                    .take(RECAP_FILES)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if changed.len() > RECAP_FILES {
                let _ = write!(line, " and {} more", changed.len() - RECAP_FILES);
            }
            parts.push(line);
        }
        (!parts.is_empty()).then(|| parts.join("\n"))
    }

    fn flush_note(&self) -> Option<String> {
        let memory = if self.memory {
            " Write the lasting facts you learned - about the project, the user's preferences, decisions - to memory."
        } else {
            ""
        };
        Some(format!(
            "Before the earlier steps become a summary, save what must not be lost: update task_state with the checklist's status, every decision with its reason and every constraint the user set.{memory} Then go on with the task."
        ))
    }
}

/// One read-modify-write at a time per file, across this process's turns.
fn file_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// `text` trimmed and cut to `limit` characters.
fn bounded(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut cut = text.chars().take(limit).collect::<String>();
    cut.push('…');
    cut
}

fn refused(message: impl Into<String>) -> HarnessError {
    HarnessError::new(ErrorCode::InvalidPayload, message)
}

fn strings(arguments: &Value, key: &str) -> Result<Option<Vec<String>>, HarnessError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(|text| bounded(text, MAX_TEXT_CHARS))
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| refused(format!("task_state: {key} takes non-empty strings")))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(refused(format!(
            "task_state: {key} takes a list of strings"
        ))),
    }
}

fn items(arguments: &Value) -> Result<Option<Vec<Item>>, HarnessError> {
    let Some(values) = arguments.get("items").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let values = values
        .as_array()
        .ok_or_else(|| refused("task_state: items takes a list"))?;
    if values.len() > MAX_ITEMS {
        return Err(refused(format!(
            "task_state: the checklist holds at most {MAX_ITEMS} items; group smaller steps"
        )));
    }
    values
        .iter()
        .map(|value| {
            let text = value
                .get("text")
                .and_then(Value::as_str)
                .map(|text| bounded(text, MAX_TEXT_CHARS))
                .filter(|text| !text.is_empty())
                .ok_or_else(|| refused("task_state: every item needs its text"))?;
            let status = match value.get("status") {
                None | Some(Value::Null) => ItemStatus::Pending,
                Some(status) => serde_json::from_value(status.clone()).map_err(|_| {
                    refused("task_state: an item's status is pending, in_progress, done or blocked")
                })?,
            };
            let note = value
                .get("note")
                .and_then(Value::as_str)
                .map(|note| bounded(note, MAX_TEXT_CHARS))
                .filter(|note| !note.is_empty());
            Ok(Item { text, status, note })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn decisions(arguments: &Value) -> Result<Vec<Decision>, HarnessError> {
    let Some(values) = arguments
        .get("add_decisions")
        .filter(|value| !value.is_null())
    else {
        return Ok(Vec::new());
    };
    values
        .as_array()
        .ok_or_else(|| refused("task_state: add_decisions takes a list"))?
        .iter()
        .map(|value| {
            let decision = value
                .get("decision")
                .and_then(Value::as_str)
                .map(|text| bounded(text, MAX_TEXT_CHARS))
                .filter(|text| !text.is_empty())
                .ok_or_else(|| refused("task_state: every decision needs its decision"))?;
            let why = value
                .get("why")
                .and_then(Value::as_str)
                .map(|text| bounded(text, MAX_TEXT_CHARS))
                .unwrap_or_default();
            Ok(Decision { decision, why })
        })
        .collect()
}

/// Append `added` to `list` without repeating an entry, keeping the newest
/// `limit`.
fn append(list: &mut Vec<String>, added: Vec<String>, limit: usize) {
    for entry in added {
        list.retain(|kept| *kept != entry);
        list.push(entry);
    }
    let excess = list.len().saturating_sub(limit);
    list.drain(..excess);
}

/// Apply one `update` to `state`; returns what the model should be told
/// besides the new state.
fn apply(state: &mut TaskState, arguments: &Value) -> Result<Vec<String>, HarnessError> {
    let mut notes = Vec::new();
    if let Some(objective) = arguments.get("objective").and_then(Value::as_str) {
        state.objective = bounded(objective, MAX_OBJECTIVE_CHARS);
    }
    if let Some(items) = items(arguments)? {
        // The checklist is sent whole; an unfinished item it leaves out is
        // said, so a dropped step is a choice and not an accident.
        let dropped = state
            .items
            .iter()
            .filter(|old| {
                old.status != ItemStatus::Done && !items.iter().any(|new| new.text == old.text)
            })
            .map(|old| old.text.clone())
            .collect::<Vec<_>>();
        if !dropped.is_empty() {
            notes.push(format!(
                "removed unfinished item(s): {}",
                dropped.join("; ")
            ));
        }
        state.items = items;
    }
    for decision in decisions(arguments)? {
        state
            .decisions
            .retain(|kept| kept.decision != decision.decision);
        state.decisions.push(decision);
    }
    let excess = state.decisions.len().saturating_sub(MAX_ENTRIES);
    state.decisions.drain(..excess);
    if let Some(added) = strings(arguments, "add_constraints")? {
        append(&mut state.constraints, added, MAX_ENTRIES);
    }
    if let Some(added) = strings(arguments, "add_files")? {
        append(&mut state.files, added, MAX_ENTRIES);
    }
    if let Some(open) = strings(arguments, "open")? {
        state.open = open;
        state.open.truncate(MAX_OPEN);
    }
    if let Some(removed) = strings(arguments, "remove")? {
        let mut missing = Vec::new();
        for entry in removed {
            let before = state.constraints.len()
                + state.decisions.len()
                + state.files.len()
                + state.open.len();
            state.constraints.retain(|kept| *kept != entry);
            state.files.retain(|kept| *kept != entry);
            state.open.retain(|kept| *kept != entry);
            state.decisions.retain(|kept| kept.decision != entry);
            let after = state.constraints.len()
                + state.decisions.len()
                + state.files.len()
                + state.open.len();
            if before == after {
                missing.push(entry);
            }
        }
        if !missing.is_empty() {
            notes.push(format!(
                "not found, so not removed (use the exact text): {}",
                missing.join("; ")
            ));
        }
    }
    apply_strategy(state, arguments)?;
    Ok(notes)
}

/// The strategy fields of an update: the strategy named, journal entries,
/// and a decision after an evaluation.
fn apply_strategy(state: &mut TaskState, arguments: &Value) -> Result<(), HarnessError> {
    if let Some(value) = arguments.get("strategy").filter(|value| !value.is_null()) {
        let strategy = strategy_of(value, "strategy")?;
        if state
            .strategy
            .current
            .as_ref()
            .is_some_and(|current| current.name != strategy.name)
        {
            return Err(refused(
                "task_state: the task already has a strategy; to change it, decide with kind switch and the evidence",
            ));
        }
        state.strategy.set(strategy);
    }
    if let Some(values) = arguments
        .get("add_journal")
        .filter(|value| !value.is_null())
    {
        for value in values
            .as_array()
            .ok_or_else(|| refused("task_state: add_journal takes a list"))?
        {
            let kind = match value.get("kind") {
                None | Some(Value::Null) => JournalKind::Observation,
                Some(kind) => serde_json::from_value(kind.clone()).map_err(|_| {
                    refused("task_state: a journal entry's kind is plan, observation, lesson, eval or eval_analysis")
                })?,
            };
            let text = value
                .get("text")
                .and_then(Value::as_str)
                .map(|text| bounded(text, MAX_TEXT_CHARS))
                .filter(|text| !text.is_empty())
                .ok_or_else(|| refused("task_state: every journal entry needs its text"))?;
            state.strategy.write(kind, text);
        }
    }
    if let Some(value) = arguments.get("decide").filter(|value| !value.is_null()) {
        let kind: DecisionKind = value
            .get("kind")
            .cloned()
            .and_then(|kind| serde_json::from_value(kind).ok())
            .ok_or_else(|| refused("task_state: decide.kind is continue or switch"))?;
        let evidence = value
            .get("evidence")
            .and_then(Value::as_str)
            .map(|text| bounded(text, MAX_TEXT_CHARS))
            .filter(|text| !text.is_empty())
            .ok_or_else(|| {
                refused("task_state: a decision gives its evidence - what the evaluations showed")
            })?;
        let to = value
            .get("strategy")
            .filter(|value| !value.is_null())
            .map(|value| strategy_of(value, "decide.strategy"))
            .transpose()?;
        state.strategy.decide(kind, to, evidence).map_err(refused)?;
    }
    Ok(())
}

/// The record of a decision, once it is saved.
fn record_decision(decision: &StrategyDecision) {
    harness_types::diag::info("strategy", "decided")
        .field(
            "kind",
            if decision.kind == DecisionKind::Switch {
                "switch"
            } else {
                "continue"
            },
        )
        .field("plateau", decision.plateau)
        .field("proposed", decision.proposed)
        .field("from", decision.from.clone())
        .field("to", decision.to.clone())
        .emit();
}

/// A strategy as an update gives it: a name, why, and what it was weighed
/// against.
fn strategy_of(value: &Value, key: &str) -> Result<Strategy, HarnessError> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .map(|name| bounded(name, MAX_TEXT_CHARS))
        .filter(|name| !name.is_empty())
        .ok_or_else(|| refused(format!("task_state: {key} needs its name")))?;
    let why = value
        .get("why")
        .and_then(Value::as_str)
        .map(|why| bounded(why, MAX_TEXT_CHARS))
        .unwrap_or_default();
    let alternatives = value
        .get("alternatives")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| {
                    let name = value.get("name").and_then(Value::as_str)?;
                    Some(Alternative {
                        name: bounded(name, MAX_TEXT_CHARS),
                        why: value
                            .get("why")
                            .and_then(Value::as_str)
                            .map(|why| bounded(why, MAX_TEXT_CHARS))
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Strategy {
        name,
        why,
        alternatives,
    })
}

/// Record an evaluation the host ran on the task - a goal refused, the
/// project's checks, the verifier - and say where the task stands. `None`
/// when the task has no state to record it in.
pub fn record_evaluation(
    path: &Path,
    source: &str,
    reached: u8,
    left: u32,
    residue: u32,
    detail: &str,
) -> Option<(Standing, TaskState)> {
    let _guard = file_lock().lock().ok()?;
    // A task whose model never used task_state is evaluated too: the agents
    // arXiv 2608.19072 describes are the ones that never stop to plan.
    let mut state = load(path);
    // An unchanged workspace scores what it scored.
    let (reached, left, residue) = if source == "unchanged" {
        state
            .strategy
            .last_score()
            .unwrap_or((reached, left, residue))
    } else {
        (reached, left, residue)
    };
    let standing = state
        .strategy
        .evaluate(source, reached, left, residue, detail);
    save(path, &state).ok()?;
    harness_types::diag::info("strategy", "evaluated")
        .field("source", source.to_owned())
        .field("stage", u64::from(reached))
        .field("left", u64::from(left))
        .field("plateau", standing.plateau)
        .emit();
    Some((standing, state))
}

/// Keep the workspace's fingerprint as a turn-end evaluation saw it.
pub fn note_workspace(path: &Path, fingerprint: &str) {
    let Ok(_guard) = file_lock().lock() else {
        return;
    };
    let mut state = load(path);
    fingerprint.clone_into(&mut state.strategy.workspace);
    let _ = save(path, &state);
}

/// Keep the alternatives a strategist named, for the next decision. None
/// named is not kept: the next evaluation asks again.
pub fn record_proposals(path: &Path, proposals: Vec<Alternative>) {
    if proposals.is_empty() {
        return;
    }
    let Ok(_guard) = file_lock().lock() else {
        return;
    };
    let mut state = load(path);
    state.strategy.propose(proposals);
    let _ = save(path, &state);
}

const ACTIONS: [&str; 2] = ["view", "update"];
const KEYS: [&str; 12] = [
    "action",
    "objective",
    "items",
    "add_decisions",
    "add_constraints",
    "add_files",
    "open",
    "remove",
    "expected_revision",
    "strategy",
    "add_journal",
    "decide",
];

/// The `task_state` tool of one conversation.
#[derive(Clone)]
pub struct TaskStateHost {
    path: PathBuf,
}

impl TaskStateHost {
    #[must_use]
    pub fn new(data_dir: &Path, conversation: &str) -> Self {
        Self {
            path: path(data_dir, conversation),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn tools(&self) -> ExternalTools {
        ExternalTools::new(Arc::new(self.clone()))
    }

    #[must_use]
    pub fn dispatcher(&self) -> Arc<dyn ExternalToolDispatcher> {
        Arc::new(self.clone())
    }

    fn check(arguments: &Value) -> Result<&str, HarnessError> {
        let object = arguments
            .as_object()
            .ok_or_else(|| refused("task_state takes an object"))?;
        if let Some(key) = object.keys().find(|key| !KEYS.contains(&key.as_str())) {
            return Err(refused(format!("task_state does not take {key}")));
        }
        object
            .get("action")
            .and_then(Value::as_str)
            .filter(|action| ACTIONS.contains(action))
            .ok_or_else(|| refused("task_state: action is view or update"))
    }

    /// Run one call: `view` reads, `update` changes and saves.
    ///
    /// # Errors
    /// The arguments are invalid, the revision is stale, or the file cannot be written.
    pub fn execute(&self, arguments: &Value) -> Result<Value, HarnessError> {
        let action = Self::check(arguments)?;
        let _guard = file_lock()
            .lock()
            .map_err(|_| refused("task state lock is poisoned"))?;
        let mut state = load(&self.path);
        if action == "view" {
            return Ok(json!({"revision": state.revision, "state": state.render()}));
        }
        if let Some(expected) = arguments.get("expected_revision").and_then(Value::as_u64)
            && expected != state.revision
        {
            return Err(HarnessError::new(
                ErrorCode::RevisionConflict,
                format!(
                    "task_state: the state is at revision {current}, not {expected}: another update came first (two updates in one batch read the same revision), and nothing was written. Update again with expected_revision {current}; view it first when that update may have changed what this one relies on",
                    current = state.revision
                ),
            ));
        }
        let decided = state.strategy.decisions.last().cloned();
        let notes = apply(&mut state, arguments)?;
        state.revision += 1;
        save(&self.path, &state).map_err(refused)?;
        if let Some(decision) = state
            .strategy
            .decisions
            .last()
            .filter(|decision| Some(*decision) != decided.as_ref())
        {
            record_decision(decision);
        }
        let mut payload = json!({"revision": state.revision, "state": state.render()});
        if !notes.is_empty() {
            payload["notes"] = json!(notes);
        }
        Ok(payload)
    }
}

impl ExternalToolCatalog for TaskStateHost {
    fn schemas(&self) -> Vec<Value> {
        vec![json!({"type":"function","function":{
            "name":"task_state",
            "description":"The state of the task you are working on, kept by the host for this conversation: the objective, a checklist with each item's status, the decisions taken and why, the user's constraints, open questions and blockers, and the key files. Every turn shows it whole, and it is never summarized away when the context is compacted, so a long task keeps its plan. Use it for any task of more than a few steps: write the checklist when you start, mark an item in_progress when you begin it and done when it is finished and checked, record each decision and each constraint the user states as it happens. update changes only the fields it names: items replaces the whole checklist (send every item, in order); add_* fields append; open replaces the list of open questions and blockers; remove deletes constraints, decisions, files or open entries by their exact text. A goal does not complete while its checklist has unfinished items. For work you will evaluate - a goal, checks to pass - name your strategy (the approach, and what you weighed it against) and keep a journal of plans, observations, evaluations and lessons. After the host evaluates the work and it is not done, the state says a decision is due: weigh the evidence and record decide continue or switch; when the strategy has stopped getting further, switching usually beats more of the same.",
            "parameters":{"type":"object","properties":{
                "action":{"type":"string","enum":ACTIONS},
                "objective":{"type":"string","description":"update: what the task is for, in a sentence or two."},
                "items":{"type":"array","maxItems":MAX_ITEMS,"description":"update: the whole checklist, in order.","items":{"type":"object","properties":{
                    "text":{"type":"string"},
                    "status":{"type":"string","enum":["pending","in_progress","done","blocked"]},
                    "note":{"type":"string","description":"Why it is blocked, or what is left."}
                },"required":["text"],"additionalProperties":false}},
                "add_decisions":{"type":"array","items":{"type":"object","properties":{
                    "decision":{"type":"string"},
                    "why":{"type":"string"}
                },"required":["decision"],"additionalProperties":false}},
                "add_constraints":{"type":"array","items":{"type":"string"},"description":"update: rules the user set for this task, in their words."},
                "add_files":{"type":"array","items":{"type":"string"},"description":"update: files the task centers on."},
                "open":{"type":"array","items":{"type":"string"},"maxItems":MAX_OPEN,"description":"update: the open questions and blockers, replacing the earlier list."},
                "remove":{"type":"array","items":{"type":"string"},"description":"update: constraints, decisions, files or open entries to delete, by exact text."},
                "expected_revision":{"type":"integer","minimum":0,"description":"update: the revision you last saw; a newer state refuses the update."},
                "strategy":{"type":"object","description":"update: the strategy the task follows, set once; change it with decide.","properties":{
                    "name":{"type":"string","description":"The approach, in a few words."},
                    "why":{"type":"string"},
                    "alternatives":{"type":"array","description":"Approaches weighed against it.","items":{"type":"object","properties":{
                        "name":{"type":"string"},
                        "why":{"type":"string","description":"Why it was not chosen."}
                    },"required":["name"],"additionalProperties":false}}
                },"required":["name"],"additionalProperties":false},
                "add_journal":{"type":"array","description":"update: entries for the task's journal.","items":{"type":"object","properties":{
                    "kind":{"type":"string","enum":["plan","observation","lesson","eval","eval_analysis"]},
                    "text":{"type":"string"}
                },"required":["text"],"additionalProperties":false}},
                "decide":{"type":"object","description":"update: after an evaluation, keep the strategy or change it.","properties":{
                    "kind":{"type":"string","enum":["continue","switch"]},
                    "evidence":{"type":"string","description":"What the evaluations showed."},
                    "strategy":{"type":"object","description":"switch: the strategy switched to.","properties":{
                        "name":{"type":"string"},
                        "why":{"type":"string"}
                    },"required":["name"],"additionalProperties":false}
                },"required":["kind","evidence"],"additionalProperties":false}
            },"required":["action"],"additionalProperties":false}
        }})]
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        (name == "task_state").then(|| CodingToolAction::ExternalTool {
            plugin_id: "task".to_owned(),
            tool_name: "task_state".to_owned(),
            arguments: arguments.clone(),
            parent_invocation_id: None,
            timeout_ms: 30_000,
        })
    }
}

impl ExternalToolDispatcher for TaskStateHost {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            if plugin_id != "task" || tool_name != "task_state" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "task state tool target is unavailable",
                ));
            }
            Self::check(arguments).map(|_| ())
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
            if plugin_id != "task" || tool_name != "task_state" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "task state tool target is unavailable",
                ));
            }
            let host = self.clone();
            let arguments = arguments.clone();
            let payload = tokio::task::spawn_blocking(move || host.execute(&arguments))
                .await
                .map_err(|error| refused(format!("task state operation failed: {error}")))??;
            Ok(ToolOutput::ExternalTool {
                plugin_id: "task".to_owned(),
                tool_name: "task_state".to_owned(),
                payload,
                inflight: 1,
            })
        })
    }
}

/// Add the task state tool without changing the other tool hosts' contracts.
pub fn with_tools(inner: Option<ExternalTools>, host: &TaskStateHost) -> ExternalTools {
    ExternalTools::new(Arc::new(Catalog {
        inner,
        task: host.tools(),
    }))
}

pub fn with_dispatcher(
    inner: Arc<dyn ExternalToolDispatcher>,
    host: &TaskStateHost,
) -> Arc<dyn ExternalToolDispatcher> {
    Arc::new(Dispatcher {
        inner,
        task: host.dispatcher(),
    })
}

struct Catalog {
    inner: Option<ExternalTools>,
    task: ExternalTools,
}

impl ExternalToolCatalog for Catalog {
    fn schemas(&self) -> Vec<Value> {
        self.inner
            .iter()
            .flat_map(ExternalTools::schemas)
            .filter(|schema| {
                schema.pointer("/function/name").and_then(Value::as_str) != Some("task_state")
            })
            .chain(self.task.schemas())
            .collect()
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        if name == "task_state" {
            self.task.resolve(name, arguments)
        } else {
            self.inner
                .as_ref()
                .and_then(|tools| tools.resolve(name, arguments))
        }
    }
}

struct Dispatcher {
    inner: Arc<dyn ExternalToolDispatcher>,
    task: Arc<dyn ExternalToolDispatcher>,
}

impl Dispatcher {
    fn target(&self, plugin_id: &str) -> &dyn ExternalToolDispatcher {
        if plugin_id == "task" {
            self.task.as_ref()
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

    fn host(root: &Path) -> TaskStateHost {
        TaskStateHost {
            path: root.join(TASK_STATE_FILE),
        }
    }

    /// The strategy is named once, the journal grows, and a decision after an
    /// evaluation keeps it or changes it; changing it without a decision is
    /// refused.
    #[test]
    fn a_strategy_changes_only_by_a_decision() {
        let root = tempfile::tempdir().expect("temp dir");
        let host = host(root.path());
        host.execute(&json!({"action": "update", "objective": "parse configs",
            "strategy": {"name": "patch the regex", "why": "smallest change",
                "alternatives": [{"name": "a parser", "why": "more work"}]},
            "add_journal": [{"kind": "plan", "text": "extend the regex for nesting"}]}))
            .expect("named");
        let refused = host
            .execute(&json!({"action": "update", "strategy": {"name": "a parser"}}))
            .expect_err("a second strategy needs a decision");
        assert!(refused.to_string().contains("decide"), "{refused}");
        record_evaluation(host.path(), "checks", 1, 2, 0, "check `nested` failed")
            .expect("recorded");
        let shown = load(host.path()).render();
        assert!(shown.contains("Decision due"), "{shown}");
        host.execute(&json!({"action": "update", "decide": {"kind": "switch",
            "evidence": "the regex cannot count brackets",
            "strategy": {"name": "a parser"}}}))
            .expect("switched");
        let state = load(host.path());
        assert_eq!(
            state.strategy.current.as_ref().map(|s| s.name.as_str()),
            Some("a parser")
        );
        assert!(!state.render().contains("Decision due"));
        assert!(
            state
                .render()
                .contains("[plan] extend the regex for nesting")
        );
        let missing = host
            .execute(&json!({"action": "update", "decide": {"kind": "continue"}}))
            .expect_err("a decision gives its evidence");
        assert!(missing.to_string().contains("evidence"), "{missing}");
    }

    #[test]
    fn an_update_is_kept_and_rendered_whole() {
        let root = tempfile::tempdir().expect("temp dir");
        let host = host(root.path());
        host.execute(&json!({
            "action": "update",
            "objective": "Port the parser",
            "items": [
                {"text": "read the grammar", "status": "done"},
                {"text": "write the lexer", "status": "in_progress"},
                {"text": "write the tests"}
            ],
            "add_decisions": [{"decision": "use SQLite", "why": "single binary"}],
            "add_constraints": ["never touch vendor/"],
            "add_files": ["src/lexer.rs"]
        }))
        .expect("update");
        let state = load(host.path());
        assert_eq!(state.revision, 1);
        let block = context_block(host.path()).expect("a block").text;
        for expected in [
            "Objective: Port the parser",
            "Checklist (1/3 done)",
            "[x] 1. read the grammar",
            "[>] 2. write the lexer",
            "[ ] 3. write the tests",
            "never touch vendor/",
            "use SQLite (why: single binary)",
            "Key files: src/lexer.rs",
        ] {
            assert!(block.contains(expected), "{expected} in {block}");
        }
        let recap = recap(host.path()).expect("a recap");
        assert!(recap.contains("now: 2. write the lexer"), "{recap}");
        assert!(recap.contains("next: 3. write the tests"), "{recap}");
        assert!(recap.contains("never touch vendor/"), "{recap}");
    }

    #[test]
    fn fields_left_out_are_kept_and_additions_do_not_repeat() {
        let root = tempfile::tempdir().expect("temp dir");
        let host = host(root.path());
        host.execute(&json!({"action":"update","objective":"o","add_constraints":["tabs only"]}))
            .expect("first");
        host.execute(&json!({"action":"update","add_constraints":["tabs only","no unwrap()"]}))
            .expect("second");
        let state = load(host.path());
        assert_eq!(state.objective, "o");
        assert_eq!(state.constraints, vec!["tabs only", "no unwrap()"]);
        host.execute(&json!({"action":"update","remove":["tabs only","missing"]}))
            .map(|payload| {
                assert!(
                    payload["notes"][0]
                        .as_str()
                        .unwrap_or_default()
                        .contains("missing")
                );
            })
            .expect("remove");
        assert_eq!(load(host.path()).constraints, vec!["no unwrap()"]);
    }

    #[test]
    fn dropping_an_unfinished_item_is_said_and_a_stale_revision_is_refused() {
        let root = tempfile::tempdir().expect("temp dir");
        let host = host(root.path());
        host.execute(
            &json!({"action":"update","items":[{"text":"alpha"},{"text":"beta","status":"done"}]}),
        )
        .expect("first");
        let payload = host
            .execute(&json!({"action":"update","items":[{"text":"gamma"}]}))
            .expect("second");
        let notes = payload["notes"][0].as_str().unwrap_or_default();
        assert!(
            notes.contains("alpha") && !notes.contains("beta"),
            "{notes}"
        );
        let error = host
            .execute(&json!({"action":"update","expected_revision":1,"objective":"x"}))
            .expect_err("stale");
        assert!(error.to_string().contains("revision 2"), "{error}");
        assert_eq!(
            error.retry_class(),
            harness_types::RetryClass::Conflict,
            "a stale revision is a conflict: read and write again"
        );
    }

    #[test]
    fn invalid_calls_are_refused_and_nothing_is_written() {
        let root = tempfile::tempdir().expect("temp dir");
        let host = host(root.path());
        assert!(host.execute(&json!({"action":"erase"})).is_err());
        assert!(host.execute(&json!({"action":"update","extra":1})).is_err());
        assert!(
            host.execute(&json!({"action":"update","items":[{"text":"a","status":"later"}]}))
                .is_err()
        );
        assert!(!host.path().exists());
        assert!(context_block(host.path()).is_none());
        assert!(recap(host.path()).is_none());
    }

    #[test]
    fn unfinished_items_are_listed_for_the_goal() {
        let state = TaskState {
            items: vec![
                Item {
                    text: "a".to_owned(),
                    status: ItemStatus::Done,
                    note: None,
                },
                Item {
                    text: "b".to_owned(),
                    status: ItemStatus::Blocked,
                    note: Some("waiting".to_owned()),
                },
            ],
            ..TaskState::default()
        };
        let unfinished = state.unfinished();
        assert_eq!(unfinished.len(), 1);
        assert_eq!(unfinished[0].text, "b");
    }

    /// A new conversation is told of the task an earlier one left, as context
    /// it may adopt, never as an instruction.
    #[test]
    fn an_earlier_task_is_offered_as_context() {
        let state = TaskState {
            objective: "Port the parser".to_owned(),
            items: vec![
                Item {
                    text: "lexer".to_owned(),
                    status: ItemStatus::Done,
                    note: None,
                },
                Item {
                    text: "grammar".to_owned(),
                    status: ItemStatus::Pending,
                    note: None,
                },
            ],
            ..TaskState::default()
        };
        let block = earlier_block(&state, Some("Parser port"));
        assert!(block.text.contains("\"Parser port\""), "{}", block.text);
        assert!(block.text.contains("not an instruction"), "{}", block.text);
        assert!(block.text.contains("[ ] 2. grammar"), "{}", block.text);
    }
}
