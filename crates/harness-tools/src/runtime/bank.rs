//! Guarded decision reuse: the action bank (`HarnessMM`, Jiang et al. 2026).
//!
//! At a model step the model may author, beside the calls it makes now, a
//! small acyclic set of future calls whose arguments are already literal: each
//! node names its predecessor (`root`, the calls of the authoring response, or
//! an earlier node), a guard on that predecessor's public result, a tool and
//! its arguments. After a call settles, the driver looks at the nodes whose
//! predecessor it was: exactly one whose guard holds runs through the normal
//! tool pipeline with no model call; several go to a typed selector, which
//! returns one node id or abstains; none, an abstention or an unclear result
//! calls the model as usual. A node runs at most once, its arguments never
//! change, and a newly authored bank replaces the nodes not run yet.
//!
//! Guards read only observed results, never predictions. Everything here is
//! deterministic; the only model-shaped part is the optional selector.
//! `docs/architecture/ACTION_BANK.en.md` maps the paper to this module.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{ToolExecutionView, ToolOutput};

/// The host tool the model authors a bank with.
pub const ACTION_BANK_TOOL: &str = "action_bank";

/// The most nodes one bank may hold. The paper's negative results come from
/// banks that are rarely usable: authoring costs output tokens on every bank,
/// so banks stay small and the model is told so.
pub const MAX_BANK_NODES: usize = 8;

/// The longest node id, in characters (letters, digits, `_` and `-`), so
/// it fits a provider call id.
const MAX_NODE_ID_CHARS: usize = 24;

/// The longest text an `output_contains` / `output_lacks` guard may test.
const MAX_GUARD_TEXT_CHARS: usize = 200;

/// The predecessor that stands for the other calls of the authoring response.
pub const ROOT: &str = "root";

/// How many bytes of the bank's arguments are counted as one token when its
/// authoring cost is estimated. The provider reports the whole response's
/// output tokens, not the bank's share; this is the estimate, and it is
/// recorded as one.
const ESTIMATE_BYTES_PER_TOKEN: usize = 4;

/// Calls a node may not make: the host's own control tools, and a question
/// to the user, which needs a model to read the answer.
const NOT_BANKABLE: [&str; 4] = [
    ACTION_BANK_TOOL,
    crate::turn_driver::TOOL_SCHEMA_TOOL,
    crate::turn_driver::USE_TOOL_TOOL,
    "ask_user",
];

/// The `action_bank` tool's schema, offered only while the bank is on.
#[must_use]
pub fn action_bank_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": ACTION_BANK_TOOL,
            "description": format!("Queue tool calls you have already fully decided, so the harness runs them after the calls you make now without calling you again. Call it IN THE SAME RESPONSE as the calls it follows, as one more parallel tool call: for example write_file, write_file and action_bank with node `test` (after `root`, when `ok`) running the tests, then node `commit` (after `test`, when `ok`). `root` is the other calls of that response; a node can also follow another node. A bank called alone in a response saves nothing for its first node, which only runs where your own call would have. Use it only when the next commands are already determined and their arguments are complete and literal; never bank a call whose arguments depend on output you have not seen yet. After each call settles, the one node that follows it and whose `when` guard holds on its result runs; if none holds, or the result is unclear, you are asked as usual and see every call the harness ran with its result. Guards: `ok`, `failed`, `exit_code: N`, `output_contains: \"text\"`, `output_lacks: \"text\"`, `changed` (a write changed its file). Keep banks small (at most {MAX_BANK_NODES} nodes); a new action_bank replaces the nodes not run yet, and each node runs at most once. Approvals and checks apply to banked calls as to your own."),
            "parameters": {
                "type": "object",
                "properties": {
                    "nodes": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_BANK_NODES,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string", "description": "A short id: letters, digits, _ or -."},
                                "after": {"type": "string", "description": "`root` (the other calls of this response) or the id of another node."},
                                "when": {"type": "string", "description": "The guard on the result of `after`: ok | failed | exit_code: N | output_contains: \"text\" | output_lacks: \"text\" | changed."},
                                "tool": {"type": "string", "description": "A tool from your tool list."},
                                "arguments": {"type": "object", "description": "The tool's complete arguments, as you would call it."}
                            },
                            "required": ["id", "after", "when", "tool", "arguments"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["nodes"],
                "additionalProperties": false
            }
        }
    })
}

/// A guard on a predecessor's public result: the small typed grammar the
/// paper's guards come from. Anything else is refused when the bank is
/// authored, so no guard is ever unknown at run time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Guard {
    /// The call ran and succeeded (a process exited 0).
    Ok,
    /// The call ran and failed (a process exited non-zero or timed out).
    Failed,
    /// A process exited with this code.
    ExitCode(i32),
    /// The result's text holds this text.
    OutputContains(String),
    /// The result's text does not hold this text.
    OutputLacks(String),
    /// A write, edit or patch changed its file.
    Changed,
}

impl Guard {
    /// The guard `text` names.
    ///
    /// # Errors
    /// The text is not one of the guards, or its value is empty or too long.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let (name, value) = match text.split_once(':') {
            Some((name, value)) => (name.trim(), Some(value.trim())),
            None => (text, None),
        };
        let quoted = |value: &str| -> Result<String, String> {
            let value = if value.starts_with('"') {
                serde_json::from_str::<String>(value)
                    .map_err(|_| format!("{name}: the text is not a closed JSON string"))?
            } else {
                value.to_owned()
            };
            if value.is_empty() {
                return Err(format!("{name} needs a non-empty text"));
            }
            if value.chars().count() > MAX_GUARD_TEXT_CHARS {
                return Err(format!(
                    "{name}: the text is longer than {MAX_GUARD_TEXT_CHARS} characters"
                ));
            }
            Ok(value)
        };
        match (name, value) {
            ("ok", None) => Ok(Self::Ok),
            ("failed", None) => Ok(Self::Failed),
            ("changed", None) => Ok(Self::Changed),
            ("exit_code", Some(value)) => value
                .parse::<i32>()
                .map(Self::ExitCode)
                .map_err(|_| format!("exit_code needs a whole number, not {value:?}")),
            ("output_contains", Some(value)) => quoted(value).map(Self::OutputContains),
            ("output_lacks", Some(value)) => quoted(value).map(Self::OutputLacks),
            _ => Err(format!(
                "unknown guard {text:?}: use ok, failed, exit_code: N, output_contains: \"text\", output_lacks: \"text\" or changed"
            )),
        }
    }

    /// Whether the guard holds on `receipt`. A result that is not clear - a
    /// refused, denied or interrupted call - meets no guard: the model reads it.
    #[must_use]
    pub fn holds(&self, receipt: &Receipt) -> bool {
        match (self, receipt.status) {
            (_, Status::Unclear) => false,
            (Self::Ok, status) => status == Status::Ok,
            (Self::Failed, status) => status == Status::Failed,
            (Self::ExitCode(code), _) => receipt.exit_code == Some(*code),
            (Self::OutputContains(text), _) => receipt.text.contains(text.as_str()),
            (Self::OutputLacks(text), _) => !receipt.text.contains(text.as_str()),
            (Self::Changed, status) => status == Status::Ok && receipt.changed,
        }
    }

    /// The guard as the model wrote it, normalized.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Ok => "ok".to_owned(),
            Self::Failed => "failed".to_owned(),
            Self::Changed => "changed".to_owned(),
            Self::ExitCode(code) => format!("exit_code: {code}"),
            Self::OutputContains(text) => format!("output_contains: {}", json!(text)),
            Self::OutputLacks(text) => format!("output_lacks: {}", json!(text)),
        }
    }
}

/// One future call of a bank.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BankNode {
    pub id: String,
    /// `root` or another node's id.
    pub after: String,
    pub guard: Guard,
    pub tool: String,
    /// The arguments as JSON text, after the tool's schema checked them.
    pub arguments: String,
}

/// A validated bank: its nodes and the fingerprint of what was authored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bank {
    /// A short hash of the authored nodes: the certificate's graph hash.
    pub hash: String,
    pub nodes: Vec<BankNode>,
    /// The tokens the bank's arguments are estimated to have cost.
    pub author_tokens: u64,
}

/// Parse and check an `action_bank` call against the tools of this turn.
///
/// Literal closure is the schema check: every node's arguments must pass its
/// tool's schema now, as a call would. Whether the author resolved the action
/// correctly is a semantic assumption the task's result measures.
///
/// # Errors
/// What is wrong with the bank, in words the model reads; nothing is kept.
#[allow(
    clippy::too_many_lines,
    reason = "one check per rule of a bank, in the order they apply"
)]
pub fn parse_bank(arguments: &str, schemas: &[Value]) -> Result<Bank, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Authored {
        nodes: Vec<AuthoredNode>,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AuthoredNode {
        id: String,
        after: String,
        when: String,
        tool: String,
        arguments: Value,
    }
    let authored: Authored = serde_json::from_str(arguments).map_err(|error| {
        format!("the bank is not {{nodes: [{{id, after, when, tool, arguments}}]}}: {error}")
    })?;
    if authored.nodes.is_empty() {
        return Err("the bank has no nodes".to_owned());
    }
    if authored.nodes.len() > MAX_BANK_NODES {
        return Err(format!(
            "the bank has {} nodes; at most {MAX_BANK_NODES} are allowed",
            authored.nodes.len()
        ));
    }
    let mut ids = HashSet::new();
    for node in &authored.nodes {
        let id = node.id.trim();
        if id.is_empty()
            || id == ROOT
            || id.chars().count() > MAX_NODE_ID_CHARS
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!(
                "node id {:?} must be 1-{MAX_NODE_ID_CHARS} letters, digits, _ or -, and not \"root\"",
                node.id
            ));
        }
        if !ids.insert(id.to_owned()) {
            return Err(format!("node id {id:?} is used twice"));
        }
    }
    let mut nodes = Vec::with_capacity(authored.nodes.len());
    for node in authored.nodes {
        let id = node.id.trim().to_owned();
        let after = node.after.trim().to_owned();
        if after != ROOT && !ids.contains(&after) {
            return Err(format!(
                "node {id:?}: after {after:?} is neither \"root\" nor a node of this bank"
            ));
        }
        if after == id {
            return Err(format!("node {id:?} cannot follow itself"));
        }
        let guard = Guard::parse(&node.when).map_err(|reason| format!("node {id:?}: {reason}"))?;
        let tool = node.tool.trim().to_owned();
        if NOT_BANKABLE.contains(&tool.as_str()) {
            return Err(format!("node {id:?}: {tool} cannot be banked"));
        }
        let Some(schema) = crate::turn_driver::parameters_schema(schemas, &tool) else {
            return Err(format!(
                "node {id:?}: {tool:?} is not a tool in your tool list"
            ));
        };
        if !node.arguments.is_object() {
            return Err(format!("node {id:?}: arguments must be a JSON object"));
        }
        let text = node.arguments.to_string();
        let arguments = match crate::validation::validate_tool_arguments_json(&tool, schema, &text)
        {
            Ok(None) => text,
            Ok(Some(coerced)) => coerced.to_string(),
            Err(message) => {
                return Err(format!(
                    "node {id:?}: its arguments do not pass {tool}'s schema: {message}"
                ));
            }
        };
        nodes.push(BankNode {
            id,
            after,
            guard,
            tool,
            arguments,
        });
    }
    // Each node has one predecessor, so a cycle is a chain of `after` that
    // never reaches the root.
    let after: HashMap<&str, &str> = nodes
        .iter()
        .map(|node| (node.id.as_str(), node.after.as_str()))
        .collect();
    for node in &nodes {
        let mut current = node.after.as_str();
        let mut hops = 0;
        while current != ROOT {
            hops += 1;
            if hops > nodes.len() {
                return Err(format!(
                    "node {:?} is on a cycle: its `after` chain never reaches root",
                    node.id
                ));
            }
            current = after.get(current).copied().unwrap_or(ROOT);
        }
    }
    let mut fingerprint = String::new();
    for node in &nodes {
        let _ = writeln!(
            fingerprint,
            "{}|{}|{}|{}|{}",
            node.id,
            node.after,
            node.guard.text(),
            node.tool,
            canonical_hash(&node.arguments)
        );
    }
    Ok(Bank {
        hash: short_hash(&fingerprint),
        nodes,
        author_tokens: u64::try_from(arguments.len().div_ceil(ESTIMATE_BYTES_PER_TOKEN))
            .unwrap_or(u64::MAX),
    })
}

/// A short, stable hash of a call's arguments with keys in order.
#[must_use]
pub fn canonical_hash(arguments: &str) -> String {
    crate::regulation::arguments_hash(arguments)
}

fn short_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest[..6].iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// How a call came out, as far as its public result says.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// It ran and succeeded.
    #[default]
    Ok,
    /// It ran and failed: a non-zero exit, a timeout.
    Failed,
    /// Not clear: refused, denied, malformed, interrupted or of unknown
    /// outcome. No guard holds on it, so the model reads it.
    Unclear,
}

/// The public result of a call (or of the root's calls together), as guards
/// read it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Receipt {
    pub status: Status,
    /// The last process exit code among the calls, when one ran.
    pub exit_code: Option<i32>,
    /// What the model is shown of the result.
    pub text: String,
    /// Whether a write, edit or patch changed its file.
    pub changed: bool,
    /// The durable receipts' execution ids, when the calls reached the gate.
    pub receipt_ids: Vec<String>,
}

impl Receipt {
    /// The receipt of a finished call that reached the gate.
    #[must_use]
    pub fn of_view(view: &ToolExecutionView, text: &str) -> Self {
        let (status, exit_code, changed) = match &view.output {
            ToolOutput::Process {
                exit_code,
                timed_out,
                canceled,
                ..
            } => {
                let status = if *canceled {
                    Status::Unclear
                } else if *timed_out || *exit_code != Some(0) {
                    Status::Failed
                } else {
                    Status::Ok
                };
                (status, *exit_code, false)
            }
            ToolOutput::WriteFile {
                before_hash,
                after_hash,
                ..
            }
            | ToolOutput::ApplyPatch {
                before_hash,
                after_hash,
                ..
            }
            | ToolOutput::EditFile {
                before_hash,
                after_hash,
                ..
            } => (Status::Ok, None, before_hash != after_hash),
            ToolOutput::Denied { .. } | ToolOutput::OutcomeUnknown { .. } => {
                (Status::Unclear, None, false)
            }
            _ => (Status::Ok, None, false),
        };
        Self {
            status,
            exit_code,
            text: text.to_owned(),
            changed,
            receipt_ids: view
                .receipt
                .as_ref()
                .map(|receipt| receipt.tool_execution_id.as_str().to_owned())
                .into_iter()
                .collect(),
        }
    }

    /// The receipt of a call that never reached a tool, or whose tool failed
    /// before it ran: nothing a guard can hold on.
    #[must_use]
    pub fn unclear(text: &str) -> Self {
        Self {
            status: Status::Unclear,
            text: text.to_owned(),
            ..Self::default()
        }
    }

    /// A call the host answered itself (a question answered, a schema shown).
    #[must_use]
    pub fn answered(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            ..Self::default()
        }
    }

    /// Several receipts read as one, as the root's calls are: unclear if any
    /// is, failed if any failed, the last exit code, every text.
    #[must_use]
    pub fn combined(parts: Vec<Self>) -> Self {
        let mut combined = Self::default();
        let mut texts = Vec::new();
        for part in parts {
            combined.status = match (combined.status, part.status) {
                (Status::Unclear, _) | (_, Status::Unclear) => Status::Unclear,
                (Status::Failed, _) | (_, Status::Failed) => Status::Failed,
                _ => Status::Ok,
            };
            if part.exit_code.is_some() {
                combined.exit_code = part.exit_code;
            }
            combined.changed |= part.changed;
            combined.receipt_ids.extend(part.receipt_ids);
            texts.push(part.text);
        }
        combined.text = texts.join("\n\n");
        combined
    }

    /// A short hash of the receipt: the certificate's receipt fields.
    #[must_use]
    pub fn hash(&self) -> String {
        short_hash(&format!(
            "{:?}|{:?}|{}|{}|{}",
            self.status,
            self.exit_code,
            self.changed,
            self.receipt_ids.join(","),
            self.text
        ))
    }
}

/// One eligible node, as the selector sees it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BankCandidate {
    pub id: String,
    pub tool: String,
    pub arguments: String,
    pub guard: String,
}

/// What the selector is asked: the user's request, the call that just
/// settled and its result, and the eligible nodes. The texts are data.
#[derive(Clone, Debug)]
pub struct BankChoice {
    pub request: String,
    pub predecessor: String,
    pub result: String,
    pub candidates: Vec<BankCandidate>,
}

/// The selector's answer: one node id, or `None` to abstain, and what the
/// answer cost.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Selection {
    pub node: Option<String>,
    /// Input tokens the selector's model call reported (0 for none).
    pub input_tokens: u64,
    /// Whether a model call was made at all.
    pub asked: bool,
}

/// Chooses among several eligible nodes. It returns only an id: it has no
/// way to change a node's arguments. Without a selector the driver abstains.
pub trait BankSelector: Send + Sync {
    fn select<'a>(
        &'a self,
        choice: BankChoice,
    ) -> Pin<Box<dyn Future<Output = Selection> + Send + 'a>>;
}

/// The action bank of one driver: whether it is on, and its selector.
#[derive(Clone, Default)]
pub struct ActionBank {
    pub selector: Option<std::sync::Arc<dyn BankSelector>>,
}

impl std::fmt::Debug for ActionBank {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActionBank")
            .field("selector", &self.selector.is_some())
            .finish()
    }
}

/// One turn's bank accounting: what was authored, what ran without a model
/// step, and what went back to the model. Complete-task cost is the turn's
/// provider usage, which already holds the authoring and the fallbacks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct BankTally {
    /// Banks accepted.
    pub authored: u32,
    /// Banks refused (nothing kept).
    pub refused: u32,
    /// Nodes in the accepted banks.
    pub nodes: u32,
    /// Nodes run without a model call: model steps saved.
    pub dispatched: u32,
    /// Nodes run right after a bank authored alone in its response: where
    /// the model's own call would have run, so no model step is saved.
    pub immediate: u32,
    /// Boundaries where the selector abstained or could not answer.
    pub abstained: u32,
    /// Nodes dropped unrun: guard false, not chosen, replaced, turn end.
    pub discarded: u32,
    /// Selector model calls, and their input tokens.
    pub selector_calls: u32,
    pub selector_input_tokens: u64,
    /// Estimated output tokens the accepted banks' arguments cost.
    pub author_tokens_estimate: u64,
}

impl BankTally {
    /// Add another turn's tally.
    pub const fn add(&mut self, other: &Self) {
        self.authored += other.authored;
        self.refused += other.refused;
        self.nodes += other.nodes;
        self.dispatched += other.dispatched;
        self.immediate += other.immediate;
        self.abstained += other.abstained;
        self.discarded += other.discarded;
        self.selector_calls += other.selector_calls;
        self.selector_input_tokens += other.selector_input_tokens;
        self.author_tokens_estimate += other.author_tokens_estimate;
    }

    /// Whether anything happened.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.authored == 0 && self.refused == 0
    }
}

/// The live bank of one turn: the authored nodes, and which are resolved.
#[derive(Debug, Default)]
pub struct BankState {
    bank: Option<Bank>,
    /// Nodes run or dropped: each node is resolved at most once.
    resolved: HashSet<String>,
    pub tally: BankTally,
}

/// Why nodes were dropped unrun.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Discard {
    /// Their guard did not hold on their predecessor's result.
    GuardFalse,
    /// Their predecessor finished and another node, or the model, went on.
    NotChosen,
    /// A newly authored bank replaced them.
    Replaced,
    /// The turn ended.
    TurnEnd,
    /// They were ready, but the model had to read this boundary (steering,
    /// a loop warning, a regulation note) or the tool bound had no room.
    Deferred,
}

impl Discard {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GuardFalse => "guard_false",
            Self::NotChosen => "not_chosen",
            Self::Replaced => "replaced",
            Self::TurnEnd => "turn_end",
            Self::Deferred => "deferred",
        }
    }
}

impl BankState {
    /// The bank's graph hash, while one is live.
    #[must_use]
    pub fn hash(&self) -> Option<&str> {
        self.bank.as_ref().map(|bank| bank.hash.as_str())
    }

    /// Put a newly authored bank in place; the unresolved nodes of the one
    /// before it are dropped and returned.
    pub fn install(&mut self, bank: Bank) -> Vec<String> {
        let dropped = self.drop_unresolved(Discard::Replaced);
        self.tally.authored += 1;
        self.tally.nodes += u32::try_from(bank.nodes.len()).unwrap_or(u32::MAX);
        self.tally.author_tokens_estimate += bank.author_tokens;
        self.bank = Some(bank);
        self.resolved.clear();
        dropped
    }

    /// The nodes that may run now that `predecessor` settled with `receipt`.
    /// Its other unresolved children are dropped: their guard did not hold.
    pub fn eligible(&mut self, predecessor: &str, receipt: &Receipt) -> Vec<BankNode> {
        let Some(bank) = &self.bank else {
            return Vec::new();
        };
        let mut eligible = Vec::new();
        let mut dropped = Vec::new();
        for node in &bank.nodes {
            if node.after != predecessor || self.resolved.contains(&node.id) {
                continue;
            }
            if node.guard.holds(receipt) {
                eligible.push(node.clone());
            } else {
                dropped.push(node.id.clone());
            }
        }
        self.discard(&dropped, Discard::GuardFalse);
        eligible
    }

    /// Mark `id` run. Its siblings that were eligible too are dropped.
    pub fn dispatched(&mut self, id: &str, siblings: &[BankNode]) {
        self.resolved.insert(id.to_owned());
        let others = siblings
            .iter()
            .filter(|node| node.id != id)
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        self.discard(&others, Discard::NotChosen);
    }

    /// The model goes on instead of the eligible nodes: they are dropped.
    pub fn abstained(&mut self, eligible: &[BankNode]) {
        self.tally.abstained += 1;
        let ids = eligible
            .iter()
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        self.discard(&ids, Discard::NotChosen);
    }

    /// The model must read this boundary: the ready nodes are dropped.
    pub fn deferred(&mut self, eligible: &[BankNode]) {
        let ids = eligible
            .iter()
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();
        self.discard(&ids, Discard::Deferred);
    }

    /// The turn ended: drop what is left, and return the ids dropped.
    pub fn finish(&mut self) -> Vec<String> {
        let dropped = self.drop_unresolved(Discard::TurnEnd);
        self.bank = None;
        dropped
    }

    fn drop_unresolved(&mut self, reason: Discard) -> Vec<String> {
        let ids = self
            .bank
            .as_ref()
            .map(|bank| {
                bank.nodes
                    .iter()
                    .filter(|node| !self.resolved.contains(&node.id))
                    .map(|node| node.id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.discard(&ids, reason);
        ids
    }

    fn discard(&mut self, ids: &[String], reason: Discard) {
        if ids.is_empty() {
            return;
        }
        for id in ids {
            self.resolved.insert(id.clone());
        }
        self.tally.discarded += u32::try_from(ids.len()).unwrap_or(u32::MAX);
        harness_types::diag::info("bank", "discarded")
            .field("graph", self.hash().unwrap_or_default().to_owned())
            .field("nodes", ids.join(","))
            .field("reason", reason.as_str())
            .emit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schemas() -> Vec<Value> {
        vec![
            json!({"type": "function", "function": {"name": "run_process", "parameters": {
                "type": "object",
                "properties": {"command": {"type": "string"}, "timeout_ms": {"type": "integer"}},
                "required": ["command"],
                "additionalProperties": false
            }}}),
            json!({"type": "function", "function": {"name": "write_file", "parameters": {
                "type": "object",
                "properties": {"path": {"type": "string"}, "content": {"type": "string"}},
                "required": ["path", "content"],
                "additionalProperties": false
            }}}),
        ]
    }

    fn bank(nodes: &Value) -> Result<Bank, String> {
        parse_bank(&json!({"nodes": nodes}).to_string(), &schemas())
    }

    fn node(id: &str, after: &str, when: &str, command: &str) -> Value {
        json!({"id": id, "after": after, "when": when, "tool": "run_process", "arguments": {"command": command}})
    }

    #[test]
    fn a_chain_of_literal_calls_is_accepted_and_fingerprinted() {
        let accepted = bank(&json!([
            node("build", "root", "ok", "cargo build"),
            node("test", "build", "exit_code: 0", "cargo test"),
        ]))
        .expect("valid bank");
        assert_eq!(accepted.nodes.len(), 2);
        assert_eq!(accepted.nodes[1].guard, Guard::ExitCode(0));
        assert_eq!(accepted.hash.len(), 12);
        let again = bank(&json!([
            node("build", "root", "ok", "cargo build"),
            node("test", "build", "exit_code: 0", "cargo test"),
        ]))
        .expect("valid bank");
        assert_eq!(accepted.hash, again.hash, "the same graph hashes the same");
        assert!(accepted.author_tokens > 0);
    }

    #[test]
    fn a_bank_that_cannot_run_as_written_is_refused_with_its_reason() {
        let cases = [
            (
                json!([node("a", "b", "ok", "x"), node("b", "a", "ok", "y")]),
                "cycle",
            ),
            (json!([node("a", "a", "ok", "x")]), "itself"),
            (json!([node("a", "nowhere", "ok", "x")]), "neither"),
            (
                json!([{"id": "a", "after": "root", "when": "ok", "tool": "rm_rf", "arguments": {}}]),
                "not a tool",
            ),
            (
                json!([{"id": "a", "after": "root", "when": "ok", "tool": "run_process", "arguments": {"cmd": "x"}}]),
                "schema",
            ),
            (
                json!([{"id": "a", "after": "root", "when": "ok", "tool": "ask_user", "arguments": {}}]),
                "cannot be banked",
            ),
            (json!([node("a", "root", "passes", "x")]), "unknown guard"),
            (
                json!([node("a", "root", "exit_code: zero", "x")]),
                "whole number",
            ),
            (
                json!([node("a", "root", "output_contains: \"\"", "x")]),
                "non-empty",
            ),
            (json!([node("root", "root", "ok", "x")]), "not \"root\""),
            (
                json!([node("a", "root", "ok", "x"), node("a", "root", "ok", "y")]),
                "twice",
            ),
            (json!([]), "no nodes"),
        ];
        for (nodes, reason) in cases {
            let refused = bank(&nodes).expect_err("refused");
            assert!(refused.contains(reason), "{reason}: {refused}");
        }
        let many = (0..=MAX_BANK_NODES)
            .map(|index| node(&format!("n{index}"), "root", "ok", "x"))
            .collect::<Vec<_>>();
        assert!(
            bank(&json!(many))
                .expect_err("too many")
                .contains("at most")
        );
        assert!(parse_bank("{\"nodes\": 3}", &schemas()).is_err());
    }

    fn process(code: Option<i32>, stdout: &str) -> Receipt {
        Receipt {
            status: match code {
                Some(0) => Status::Ok,
                Some(_) => Status::Failed,
                None => Status::Unclear,
            },
            exit_code: code,
            text: stdout.to_owned(),
            ..Receipt::default()
        }
    }

    #[test]
    fn each_guard_reads_only_the_observed_result() {
        let passed = process(Some(0), "test result: ok. 3 passed");
        let failed = process(Some(101), "test result: FAILED. 1 failed");
        let denied = Receipt::unclear("denied by policy: test result: ok");
        let wrote = Receipt {
            changed: true,
            ..Receipt::default()
        };
        let cases = [
            (Guard::Ok, [true, false, false, true]),
            (Guard::Failed, [false, true, false, false]),
            (Guard::ExitCode(0), [true, false, false, false]),
            (Guard::ExitCode(101), [false, true, false, false]),
            (
                Guard::OutputContains("test result: ok".to_owned()),
                [true, false, false, false],
            ),
            (
                Guard::OutputLacks("FAILED".to_owned()),
                [true, false, false, true],
            ),
            (Guard::Changed, [false, false, false, true]),
        ];
        for (guard, expected) in cases {
            let held = [&passed, &failed, &denied, &wrote].map(|receipt| guard.holds(receipt));
            assert_eq!(held, expected, "{guard:?}");
        }
        assert_eq!(
            Guard::parse("output_contains: ok done").expect("bare"),
            Guard::OutputContains("ok done".to_owned())
        );
        assert_eq!(
            Guard::parse(&Guard::OutputLacks("a\"b".to_owned()).text()).expect("round trip"),
            Guard::OutputLacks("a\"b".to_owned())
        );
    }

    #[test]
    fn the_root_is_read_as_one_result() {
        let both = Receipt::combined(vec![Receipt::answered("wrote a"), process(Some(0), "ok")]);
        assert_eq!(both.status, Status::Ok);
        assert_eq!(both.exit_code, Some(0));
        let one_failed = Receipt::combined(vec![process(Some(1), "x"), process(Some(0), "y")]);
        assert_eq!(one_failed.status, Status::Failed);
        let unclear = Receipt::combined(vec![process(Some(0), "y"), Receipt::unclear("denied")]);
        assert_eq!(unclear.status, Status::Unclear);
        assert!(
            Receipt::combined(Vec::new()).status == Status::Ok,
            "no root calls: ok"
        );
    }

    #[test]
    fn a_node_runs_once_and_a_new_bank_replaces_the_rest() {
        let mut state = BankState::default();
        state.install(
            bank(&json!([
                node("build", "root", "ok", "cargo build"),
                node("log", "root", "failed", "cat build.log"),
                node("test", "build", "ok", "cargo test"),
            ]))
            .expect("bank"),
        );
        let ok = process(Some(0), "");
        let eligible = state.eligible(ROOT, &ok);
        assert_eq!(
            eligible.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["build"]
        );
        assert_eq!(state.tally.discarded, 1, "log's guard did not hold");
        state.dispatched("build", &eligible);
        assert!(
            state.eligible(ROOT, &ok).is_empty(),
            "the root's children are resolved"
        );
        let eligible = state.eligible("build", &ok);
        assert_eq!(eligible.len(), 1);
        state.dispatched("test", &eligible);
        assert!(
            state.eligible("build", &ok).is_empty(),
            "test runs at most once"
        );
        assert_eq!(
            state.tally.dispatched, 0,
            "the driver counts a bypass when it is certified"
        );
        // A new bank: the old one is gone, nothing of it is left to drop.
        let dropped =
            state.install(bank(&json!([node("again", "root", "ok", "cargo test")])).expect("bank"));
        assert!(dropped.is_empty());
        assert_eq!(state.finish(), ["again"]);
        assert_eq!(state.tally.authored, 2);
        assert_eq!(state.tally.nodes, 4);
        assert_eq!(state.tally.discarded, 2);
    }

    #[test]
    fn a_new_bank_drops_the_unrun_nodes_of_the_old_one() {
        let mut state = BankState::default();
        state.install(
            bank(&json!([
                node("a", "root", "ok", "x"),
                node("b", "a", "ok", "y")
            ]))
            .expect("bank"),
        );
        let dropped = state.install(bank(&json!([node("c", "root", "ok", "z")])).expect("bank"));
        assert_eq!(dropped, ["a", "b"]);
        assert_eq!(state.tally.discarded, 2);
    }
}
