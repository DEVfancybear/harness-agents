//! The feature list as a harness primitive, not a memo.
//!
//! `.harness/features.json` holds the project's features, each a behavior, the
//! commands that verify it, a state (`not_started`, `active`, `blocked`,
//! `passing`) and the evidence of its last verification. The model works it
//! through the `feature` tool: it adds features, starts one, blocks one with a
//! reason - but it cannot declare one passing. `verify` asks the harness to run
//! the feature's commands (or the project's `[verify]` checks when it has none),
//! and only a passing run moves the feature to `passing`, with the evidence the
//! harness recorded. One feature is active at a time, so the agent finishes a
//! feature before it starts the next.
//!
//! A feature whose file says `passing` without the harness's evidence counts as
//! unverified: editing the file does not make work done.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_providers::CancellationToken;
use harness_session::{ContextBlock, ContextBlockKind};
use harness_tools::{
    CodingToolAction, ExternalToolCatalog, ExternalToolDispatcher, ExternalTools,
    ToolDispatchAuthorization, ToolOutput,
};
use harness_types::{ErrorCode, HarnessError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc::UnboundedSender;

use super::events::SessionEvent;

/// Where the list lives, relative to the workspace root.
pub const FEATURES_FILE: &str = ".harness/features.json";

/// Who records evidence: only the harness's own runs count.
const VERIFIED_BY: &str = "ha";

/// The time limit of one feature verification command.
const FEATURE_COMMAND_TIMEOUT_MS: u64 = 300_000;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    #[serde(alias = "not-started", alias = "todo")]
    NotStarted,
    #[serde(alias = "in_progress", alias = "in-progress")]
    Active,
    Blocked,
    Passing,
}

impl State {
    const fn label(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Active => "active",
            Self::Blocked => "blocked",
            Self::Passing => "passing",
        }
    }
}

/// What the harness recorded when a feature's verification passed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Evidence {
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub checks: String,
    pub verified_by: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Feature {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub behavior: String,
    /// Shell commands; all must exit 0. Empty means the project's checks.
    #[serde(default)]
    pub verification: Vec<String>,
    #[serde(default, alias = "status")]
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
    /// Fields ha does not use (priority, notes, ...) are kept as written.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Feature {
    /// Passing, with the harness's own evidence.
    #[must_use]
    pub fn verified(&self) -> bool {
        self.state == State::Passing
            && self
                .evidence
                .as_ref()
                .is_some_and(|evidence| evidence.verified_by == VERIFIED_BY)
    }

    fn state_label(&self) -> &'static str {
        if self.state == State::Passing && !self.verified() {
            "passing?"
        } else {
            self.state.label()
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct FeatureList {
    #[serde(default)]
    pub features: Vec<Feature>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Counts of the list, for the prompt and `/features`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Counts {
    pub total: usize,
    pub verified: usize,
    pub active: usize,
    pub blocked: usize,
    pub not_started: usize,
    /// Marked passing without the harness's evidence.
    pub unverified: usize,
}

impl FeatureList {
    #[must_use]
    pub fn counts(&self) -> Counts {
        let mut counts = Counts {
            total: self.features.len(),
            ..Counts::default()
        };
        for feature in &self.features {
            match feature.state {
                State::Passing if feature.verified() => counts.verified += 1,
                State::Passing => counts.unverified += 1,
                State::Active => counts.active += 1,
                State::Blocked => counts.blocked += 1,
                State::NotStarted => counts.not_started += 1,
            }
        }
        counts
    }

    fn find(&self, id: &str) -> Option<usize> {
        self.features
            .iter()
            .position(|feature| feature.id.eq_ignore_ascii_case(id.trim()))
    }

    fn next_id(&self) -> String {
        let highest = self
            .features
            .iter()
            .filter_map(|feature| {
                feature
                    .id
                    .strip_prefix('F')
                    .and_then(|number| number.parse::<u32>().ok())
            })
            .max()
            .unwrap_or(0);
        format!("F{:02}", highest + 1)
    }

    /// One line per feature, with the counts and the verified completion rate.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        if self.features.is_empty() {
            return vec![format!(
                "{FEATURES_FILE} has no features yet; the agent adds them with the feature tool"
            )];
        }
        let counts = self.counts();
        let activated = counts.verified + counts.unverified + counts.active + counts.blocked;
        let mut lines = vec![format!(
            "{} of {} passing (verified){}; {} active, {} blocked, {} not started{}",
            counts.verified,
            counts.total,
            if activated == 0 {
                String::new()
            } else {
                format!(" · verified completion {}/{activated}", counts.verified)
            },
            counts.active,
            counts.blocked,
            counts.not_started,
            if counts.unverified == 0 {
                String::new()
            } else {
                format!(
                    ", {} marked passing without harness evidence (passing?)",
                    counts.unverified
                )
            }
        )];
        lines.push(String::new());
        for feature in &self.features {
            let mut line = format!(
                "{:<11} {}  {}",
                feature.state_label(),
                feature.id,
                feature.title
            );
            if let Some(blocker) = &feature.blocker
                && feature.state == State::Blocked
            {
                let _ = write!(line, "  (blocked: {blocker})");
            }
            lines.push(line);
            if let Some(evidence) = feature.evidence.as_ref().filter(|_| feature.verified()) {
                lines.push(format!(
                    "            verified {}{}: {}",
                    evidence.at,
                    evidence
                        .commit
                        .as_deref()
                        .map(|commit| format!(" at {commit}"))
                        .unwrap_or_default(),
                    evidence.checks
                ));
            }
        }
        lines
    }

    /// The list in the turn's context: where things stand and the rules.
    #[must_use]
    pub fn summary(&self) -> String {
        let counts = self.counts();
        let mut text = format!(
            "Feature list ({FEATURES_FILE}): {} of {} passing (verified by the harness), {} active, {} blocked, {} not started.",
            counts.verified, counts.total, counts.active, counts.blocked, counts.not_started
        );
        if counts.unverified > 0 {
            let _ = write!(
                text,
                " {} are marked passing without harness evidence and count as unverified.",
                counts.unverified
            );
        }
        if let Some(active) = self
            .features
            .iter()
            .find(|feature| feature.state == State::Active)
        {
            let _ = write!(text, "\nActive: {} - {}", active.id, active.title);
            if !active.behavior.is_empty() {
                let _ = write!(text, " ({})", active.behavior);
            }
        } else if let Some(next) = self
            .features
            .iter()
            .find(|feature| feature.state == State::NotStarted)
        {
            let _ = write!(text, "\nNext not started: {} - {}", next.id, next.title);
        }
        text.push_str("\nWork on one feature at a time and stay inside its scope. Use the feature tool: start one, and when it is done call verify - the harness runs its verification and only a pass makes it passing. Do not edit the file to change states.");
        text
    }
}

fn path(root: &Path) -> PathBuf {
    root.join(FEATURES_FILE)
}

/// The list, or `None` when the project has none.
///
/// # Errors
/// The file exists but cannot be read or parsed.
pub fn load(root: &Path) -> Result<Option<FeatureList>, String> {
    let path = path(root);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{FEATURES_FILE} cannot be read: {error}")),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| format!("{FEATURES_FILE} is not a valid feature list: {error}"))
}

fn save(root: &Path, list: &FeatureList) -> Result<(), String> {
    let path = path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{FEATURES_FILE} cannot be written: {error}"))?;
    }
    let mut text = serde_json::to_string_pretty(list)
        .map_err(|error| format!("{FEATURES_FILE} cannot be encoded: {error}"))?;
    text.push('\n');
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)
        .and_then(|()| std::fs::rename(&temporary, &path))
        .map_err(|error| format!("{FEATURES_FILE} cannot be written: {error}"))
}

/// The context block a turn carries when the project keeps a feature list.
#[must_use]
pub fn context_block(root: &Path) -> Option<ContextBlock> {
    let list = load(root).ok().flatten()?;
    Some(ContextBlock::mandatory(
        "features",
        ContextBlockKind::Instruction,
        list.summary(),
    ))
}

/// `git rev-parse --short HEAD`, marked when the worktree has uncommitted changes.
async fn commit(root: &Path) -> Option<String> {
    let run = |args: &'static [&'static str]| {
        tokio::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .output()
    };
    let head = run(&["rev-parse", "--short", "HEAD"]).await.ok()?;
    if !head.status.success() {
        return None;
    }
    let mut commit = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    if let Ok(status) = run(&["--no-optional-locks", "status", "--porcelain"]).await
        && !status.stdout.is_empty()
    {
        commit.push_str(" + uncommitted changes");
    }
    Some(commit)
}

/// The `feature` tool of one turn.
#[derive(Clone)]
pub struct FeatureHost {
    root: PathBuf,
    project_checks: Vec<super::verify::Check>,
    cancellation: CancellationToken,
    sender: UnboundedSender<SessionEvent>,
}

impl FeatureHost {
    #[must_use]
    pub fn new(
        root: PathBuf,
        project_checks: Vec<super::verify::Check>,
        cancellation: CancellationToken,
        sender: UnboundedSender<SessionEvent>,
    ) -> Self {
        Self {
            root,
            project_checks,
            cancellation,
            sender,
        }
    }

    #[must_use]
    pub fn tools(&self) -> ExternalTools {
        ExternalTools::new(Arc::new(FeatureCatalog {
            root: self.root.clone(),
        }))
    }

    #[must_use]
    pub fn dispatcher(&self) -> Arc<dyn ExternalToolDispatcher> {
        Arc::new(self.clone())
    }

    fn refused(message: impl Into<String>) -> HarnessError {
        HarnessError::new(ErrorCode::InvalidPayload, message)
    }

    fn text(arguments: &Value, key: &str) -> Option<String> {
        arguments[key]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    }

    fn commands(arguments: &Value, key: &str) -> Result<Option<Vec<String>>, HarnessError> {
        match &arguments[key] {
            Value::Null => Ok(None),
            Value::Array(items) => items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::trim)
                        .filter(|command| !command.is_empty())
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            Self::refused(format!("{key} must be a list of shell commands"))
                        })
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            _ => Err(Self::refused(format!(
                "{key} must be a list of shell commands"
            ))),
        }
    }

    fn feature_id(list: &FeatureList, arguments: &Value) -> Result<usize, HarnessError> {
        let id = Self::text(arguments, "id").ok_or_else(|| Self::refused("id is required"))?;
        list.find(&id).ok_or_else(|| {
            Self::refused(format!(
                "there is no feature {id}; the list has {}",
                list.features
                    .iter()
                    .map(|feature| feature.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
    }

    /// Run one action and return the text the model reads.
    async fn run(&self, arguments: &Value) -> Result<String, HarnessError> {
        let action = arguments["action"].as_str().unwrap_or_default();
        let mut list = load(&self.root).map_err(Self::refused)?.unwrap_or_default();
        match action {
            "list" => Ok(list.lines().join("\n")),
            "add" => {
                let title = Self::text(arguments, "title")
                    .ok_or_else(|| Self::refused("add needs a title"))?;
                let id = Self::text(arguments, "id").unwrap_or_else(|| list.next_id());
                if list.find(&id).is_some() {
                    return Err(Self::refused(format!("feature {id} already exists")));
                }
                let verification = Self::commands(arguments, "verification")?.unwrap_or_default();
                list.features.push(Feature {
                    id: id.clone(),
                    title: title.clone(),
                    behavior: Self::text(arguments, "behavior").unwrap_or_default(),
                    verification,
                    state: State::NotStarted,
                    evidence: None,
                    blocker: None,
                    extra: Map::new(),
                });
                save(&self.root, &list).map_err(Self::refused)?;
                Ok(format!("added {id}: {title} (not_started)"))
            }
            "start" => {
                let index = Self::feature_id(&list, arguments)?;
                if let Some(active) = list.features.iter().find(|feature| {
                    feature.state == State::Active && feature.id != list.features[index].id
                }) {
                    return Err(Self::refused(format!(
                        "{} is active: verify it to passing, or block it with a reason, before starting another - one feature at a time",
                        active.id
                    )));
                }
                let feature = &mut list.features[index];
                if feature.verified() {
                    return Err(Self::refused(format!(
                        "{} is already passing; add a new feature for new work",
                        feature.id
                    )));
                }
                feature.state = State::Active;
                feature.blocker = None;
                let text = format!("{} is active: {}", feature.id, feature.title);
                save(&self.root, &list).map_err(Self::refused)?;
                Ok(text)
            }
            "block" => {
                let index = Self::feature_id(&list, arguments)?;
                let reason = Self::text(arguments, "reason")
                    .ok_or_else(|| Self::refused("block needs the reason it is blocked"))?;
                let feature = &mut list.features[index];
                if feature.verified() {
                    return Err(Self::refused(format!("{} is passing", feature.id)));
                }
                feature.state = State::Blocked;
                feature.blocker = Some(reason.clone());
                let text = format!("{} is blocked: {reason}", feature.id);
                save(&self.root, &list).map_err(Self::refused)?;
                Ok(text)
            }
            "update" => {
                let index = Self::feature_id(&list, arguments)?;
                let feature = &mut list.features[index];
                if feature.verified() {
                    return Err(Self::refused(format!(
                        "{} is passing; its definition stays as it was verified - add a new feature for new work",
                        feature.id
                    )));
                }
                if let Some(title) = Self::text(arguments, "title") {
                    feature.title = title;
                }
                if let Some(behavior) = Self::text(arguments, "behavior") {
                    feature.behavior = behavior;
                }
                if let Some(verification) = Self::commands(arguments, "verification")? {
                    feature.verification = verification;
                }
                let text = format!("updated {}", feature.id);
                save(&self.root, &list).map_err(Self::refused)?;
                Ok(text)
            }
            "verify" => self.verify(list, arguments).await,
            _ => Err(Self::refused(
                "action must be list, add, start, block, update or verify",
            )),
        }
    }

    async fn verify(
        &self,
        mut list: FeatureList,
        arguments: &Value,
    ) -> Result<String, HarnessError> {
        let index = Self::feature_id(&list, arguments)?;
        let feature = list.features[index].clone();
        // The commands the approval showed are the ones that run: a list edited
        // after the approval is refused rather than run unseen.
        let approved = Self::commands(arguments, "commands")?.unwrap_or_default();
        if approved != feature.verification {
            return Err(Self::refused(format!(
                "{}'s verification changed after it was approved; verify again",
                feature.id
            )));
        }
        let checks = if feature.verification.is_empty() {
            self.project_checks.clone()
        } else {
            feature
                .verification
                .iter()
                .enumerate()
                .map(|(number, command)| super::verify::Check {
                    name: format!("{} #{}", feature.id, number + 1),
                    command: command.clone(),
                    hint: None,
                    timeout_ms: FEATURE_COMMAND_TIMEOUT_MS,
                })
                .collect()
        };
        if checks.is_empty() {
            return Err(Self::refused(format!(
                "{} has no verification commands and the project has no [verify] checks; give it commands with update (verification: [...]) first",
                feature.id
            )));
        }
        let _ = self.sender.send(SessionEvent::Notice {
            message: format!("feature {}: running {} check(s)", feature.id, checks.len()),
        });
        let report = super::verify::run(&self.root, &checks, &self.cancellation).await;
        if let Some(failure) = report.failure_text() {
            let _ = self.sender.send(SessionEvent::Notice {
                message: format!(
                    "feature {}: verification failed; it is not passing",
                    feature.id
                ),
            });
            return Ok(format!("{} is NOT passing. {failure}", feature.id));
        }
        // The file may have changed while the checks ran: record the result on
        // what is there now.
        list = load(&self.root).map_err(Self::refused)?.unwrap_or_default();
        let Some(index) = list.find(&feature.id) else {
            return Err(Self::refused(format!(
                "{} disappeared from the list while it was verified",
                feature.id
            )));
        };
        let evidence = Evidence {
            at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            commit: commit(&self.root).await,
            checks: report.evidence(),
            verified_by: VERIFIED_BY.to_owned(),
        };
        let entry = &mut list.features[index];
        entry.state = State::Passing;
        entry.blocker = None;
        entry.evidence = Some(evidence.clone());
        save(&self.root, &list).map_err(Self::refused)?;
        let _ = self.sender.send(SessionEvent::Notice {
            message: format!("feature {} is passing (verified)", feature.id),
        });
        let mut text = format!(
            "{} is passing, verified by the harness: {}.",
            feature.id, evidence.checks
        );
        if let Some(next) = list
            .features
            .iter()
            .find(|feature| feature.state == State::NotStarted)
        {
            let _ = write!(text, " Next not started: {} - {}.", next.id, next.title);
        }
        Ok(text)
    }
}

struct FeatureCatalog {
    root: PathBuf,
}

impl ExternalToolCatalog for FeatureCatalog {
    fn schemas(&self) -> Vec<Value> {
        vec![json!({
            "type": "function",
            "function": {
                "name": "feature",
                "description": "The project's feature list (.harness/features.json), the record of what is done. list shows it; add records a feature with the shell commands that verify its behavior; start makes one active (one at a time); block records why it cannot go on; update changes one that is not passing; verify asks the harness to run its verification - only a passing run makes it passing, with evidence. You cannot mark a feature passing yourself.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "enum": ["list", "add", "start", "block", "update", "verify"]},
                        "id": {"type": "string", "description": "The feature, such as F03 (add picks the next id when omitted)."},
                        "title": {"type": "string", "description": "A user-visible behavior in a few words, small enough for one session."},
                        "behavior": {"type": "string", "description": "What a user can do when it works."},
                        "verification": {"type": "array", "items": {"type": "string"}, "description": "Shell commands that exit 0 only when the behavior works end to end; empty means the project's [verify] checks."},
                        "reason": {"type": "string", "description": "Why the feature is blocked."}
                    },
                    "required": ["action"],
                    "additionalProperties": false
                }
            }
        })]
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        if name != "feature" {
            return None;
        }
        let mut arguments = arguments.clone();
        // A verification runs the feature's own commands: they ride in the
        // request, so the approval shows what will run.
        if arguments["action"] == "verify"
            && let Some(object) = arguments.as_object_mut()
        {
            object.remove("commands");
            let commands = arguments_feature(&self.root, object.get("id"))
                .map(|feature| feature.verification)
                .unwrap_or_default();
            if !commands.is_empty() {
                object.insert("commands".to_owned(), json!(commands));
            }
        }
        Some(CodingToolAction::ExternalTool {
            plugin_id: "feature".to_owned(),
            tool_name: "feature".to_owned(),
            arguments,
            parent_invocation_id: None,
            timeout_ms: 3_600_000,
        })
    }
}

fn arguments_feature(root: &Path, id: Option<&Value>) -> Option<Feature> {
    let id = id?.as_str()?;
    let list = load(root).ok().flatten()?;
    list.find(id).map(|index| list.features[index].clone())
}

impl ExternalToolDispatcher for FeatureHost {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            if plugin_id != "feature" || tool_name != "feature" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "feature tool target is unavailable",
                ));
            }
            let Some(object) = arguments.as_object() else {
                return Err(Self::refused("feature takes an object"));
            };
            if let Some(key) = object.keys().find(|key| {
                !matches!(
                    key.as_str(),
                    "action" | "id" | "title" | "behavior" | "verification" | "reason" | "commands"
                )
            }) {
                return Err(Self::refused(format!("feature does not take {key}")));
            }
            Ok(())
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
            if plugin_id != "feature" || tool_name != "feature" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "feature tool target is unavailable",
                ));
            }
            let text = self.run(arguments).await?;
            Ok(ToolOutput::ExternalTool {
                plugin_id: "feature".to_owned(),
                tool_name: "feature".to_owned(),
                payload: json!({ "text": text }),
                inflight: 1,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{FEATURES_FILE, FeatureHost, State, load};
    use harness_tools::ExternalToolCatalog as _;
    use serde_json::{Value, json};

    fn host(root: &std::path::Path) -> FeatureHost {
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        FeatureHost::new(
            root.to_path_buf(),
            Vec::new(),
            harness_providers::CancellationToken::new(),
            sender,
        )
    }

    /// What the model's call becomes after the catalog resolved it.
    fn resolved(host: &FeatureHost, arguments: &Value) -> Value {
        let catalog = super::FeatureCatalog {
            root: host.root.clone(),
        };
        match catalog.resolve("feature", arguments) {
            Some(harness_tools::CodingToolAction::ExternalTool { arguments, .. }) => arguments,
            _ => panic!("feature resolves"),
        }
    }

    #[tokio::test]
    async fn a_feature_passes_only_through_a_harness_run() {
        let root = tempfile::tempdir().expect("root");
        let host = host(root.path());
        let added = host
            .run(&json!({"action": "add", "title": "greets", "verification": ["echo hello"]}))
            .await
            .expect("add");
        assert_eq!(added, "added F01: greets (not_started)");
        host.run(&json!({"action": "add", "title": "fails", "verification": ["exit 4"]}))
            .await
            .expect("add second");
        host.run(&json!({"action": "start", "id": "F01"}))
            .await
            .expect("start");
        // One feature at a time.
        let refused = host
            .run(&json!({"action": "start", "id": "F02"}))
            .await
            .expect_err("wip");
        assert!(refused.to_string().contains("F01 is active"), "{refused}");
        let passed = host
            .run(&resolved(&host, &json!({"action": "verify", "id": "F01"})))
            .await
            .expect("verify");
        assert!(
            passed.contains("F01 is passing, verified by the harness"),
            "{passed}"
        );
        assert!(passed.contains("Next not started: F02"), "{passed}");
        let list = load(root.path()).expect("load").expect("list");
        assert!(list.features[0].verified());
        // A failing check leaves the feature where it was and says why.
        host.run(&json!({"action": "start", "id": "F02"}))
            .await
            .expect("start second");
        let failed = host
            .run(&resolved(&host, &json!({"action": "verify", "id": "F02"})))
            .await
            .expect("verify second");
        assert!(failed.contains("F02 is NOT passing"), "{failed}");
        assert!(failed.contains("exited 4"), "{failed}");
        let list = load(root.path()).expect("load").expect("list");
        assert_eq!(list.features[1].state, State::Active);
        // A passing feature keeps the definition it was verified with.
        let refused = host
            .run(&json!({"action": "update", "id": "F01", "verification": ["true"]}))
            .await
            .expect_err("update passing");
        assert!(refused.to_string().contains("is passing"), "{refused}");
    }

    #[tokio::test]
    async fn commands_changed_after_approval_do_not_run() {
        let root = tempfile::tempdir().expect("root");
        let host = host(root.path());
        host.run(&json!({"action": "add", "title": "x", "verification": ["echo one"]}))
            .await
            .expect("add");
        let approved = resolved(&host, &json!({"action": "verify", "id": "F01"}));
        assert_eq!(approved["commands"], json!(["echo one"]));
        host.run(&json!({"action": "update", "id": "F01", "verification": ["echo two"]}))
            .await
            .expect("update");
        let refused = host.run(&approved).await.expect_err("changed");
        assert!(
            refused
                .to_string()
                .contains("changed after it was approved")
        );
        // A model cannot smuggle its own command list into the request either.
        let smuggled = resolved(
            &host,
            &json!({"action": "verify", "id": "F01", "commands": ["true"]}),
        );
        assert_eq!(smuggled["commands"], json!(["echo two"]));
    }

    #[test]
    fn passing_without_harness_evidence_is_unverified() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join(".harness")).expect("dir");
        std::fs::write(
            root.path().join(FEATURES_FILE),
            r#"{"project": "x", "features": [
                {"id": "chat-001", "title": "New chat", "status": "passing", "priority": 1},
                {"id": "chat-002", "title": "Send", "status": "in_progress"}
            ]}"#,
        )
        .expect("write");
        let list = load(root.path()).expect("load").expect("list");
        let counts = list.counts();
        assert_eq!(counts.verified, 0);
        assert_eq!(counts.unverified, 1);
        assert_eq!(counts.active, 1);
        assert_eq!(list.features[0].extra["priority"], json!(1));
        assert_eq!(list.extra["project"], json!("x"));
        let summary = list.summary();
        assert!(summary.contains("0 of 2 passing"), "{summary}");
        assert!(summary.contains("Active: chat-002 - Send"), "{summary}");
        assert!(list.lines().join("\n").contains("passing?"));
    }

    #[tokio::test]
    async fn without_any_verification_a_feature_cannot_be_verified() {
        let root = tempfile::tempdir().expect("root");
        let host = host(root.path());
        host.run(&json!({"action": "add", "title": "x"}))
            .await
            .expect("add");
        let refused = host
            .run(&resolved(&host, &json!({"action": "verify", "id": "F01"})))
            .await
            .expect_err("nothing to run");
        assert!(refused.to_string().contains("no verification commands"));
    }
}
