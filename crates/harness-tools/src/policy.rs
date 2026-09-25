use harness_types::{ErrorCode, HarnessError};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::{Arc, RwLock};

use crate::{CodingToolAction, ToolKind};

/// A path-scoped policy result. Denial is monotonic: any matching deny wins
/// regardless of a deeper matching allow.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyEffect {
    Allow,
    Deny,
}

/// Interactive approval policy. All modes still pass through path validation,
/// denial rules, workspace revalidation, and durable receipts.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyMode {
    #[default]
    Ask,
    AutoEdit,
    FullAuto,
}

impl PolicyMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::AutoEdit => "auto-edit",
            Self::FullAuto => "full-auto",
        }
    }
}

impl FromStr for PolicyMode {
    type Err = HarnessError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "ask" => Ok(Self::Ask),
            "auto-edit" => Ok(Self::AutoEdit),
            "full-auto" => Ok(Self::FullAuto),
            _ => Err(HarnessError::new(
                ErrorCode::ConfigParseError,
                "approval mode must be ask, auto-edit, or full-auto",
            )),
        }
    }
}

/// Result of applying the single `ToolPolicy` to one already validated action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Decision {
    /// Protected paths and workspace escapes are stopped before a panel exists.
    Blocked(String),
    Deny(String),
    Allow {
        reason: String,
    },
    Ask,
}

/// A `tool(pattern)` rule. The pattern is matched against the complete action
/// target, such as `run_shell(cargo test *)` or `read_file(docs/**)`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolPatternRule {
    pub pattern: String,
    pub effect: PolicyEffect,
    pub reason: String,
}

/// Rules confirmed while a turn is running. This remains rule data owned by the
/// existing `ToolPolicy`; sharing it only lets the approval host add a confirmed
/// pattern after the action currently on screen has settled.
#[derive(Clone, Debug, Default)]
pub struct ToolPolicyRules {
    state: Arc<RwLock<ToolPolicyRulesState>>,
}

#[derive(Debug, Default)]
struct ToolPolicyRulesState {
    revision: u64,
    rules: Vec<ToolPatternRule>,
}

impl ToolPolicyRules {
    /// Clear turn-scoped rules before a new user input is admitted.
    pub fn clear(&self) {
        if let Ok(mut state) = self.state.write()
            && !state.rules.is_empty()
        {
            state.rules.clear();
            state.revision = state.revision.saturating_add(1);
        }
    }

    /// Add a rule after the user confirmed its exact displayed pattern.
    pub fn add(&self, rule: ToolPatternRule) -> Result<(), HarnessError> {
        validate_tool_pattern(&rule.pattern)?;
        let mut state = self.state.write().map_err(|_| {
            HarnessError::new(ErrorCode::PolicyDenied, "tool policy rules are unavailable")
        })?;
        if !state.rules.contains(&rule) {
            state.rules.push(rule);
            state.revision = state.revision.saturating_add(1);
        }
        Ok(())
    }

    fn snapshot(&self) -> Option<(u64, Vec<ToolPatternRule>)> {
        self.state
            .read()
            .ok()
            .map(|state| (state.revision, state.rules.clone()))
    }
}

/// Validate that a configured string is a complete `tool(pattern)` glob.
pub fn validate_tool_pattern(pattern: &str) -> Result<(), HarnessError> {
    let trimmed = pattern.trim();
    let Some(open) = trimmed.find('(') else {
        return Err(HarnessError::new(
            ErrorCode::ConfigParseError,
            "tool rule must use tool(pattern), for example run_shell(cargo test *)",
        ));
    };
    if !trimmed.ends_with(')') || open == 0 || trimmed[..open].contains(char::is_whitespace) {
        return Err(HarnessError::new(
            ErrorCode::ConfigParseError,
            "tool rule must use tool(pattern), for example run_shell(cargo test *)",
        ));
    }
    globset::GlobBuilder::new(trimmed)
        .build()
        .map_err(|_| HarnessError::new(ErrorCode::ConfigParseError, "tool rule glob is invalid"))?;
    Ok(())
}

impl ToolPatternRule {
    #[must_use]
    pub fn allow(pattern: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            effect: PolicyEffect::Allow,
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn deny(pattern: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            effect: PolicyEffect::Deny,
            reason: reason.into(),
        }
    }
}

/// A normalized relative-path policy rule. Empty prefix means the workspace
/// root and therefore matches every path-scoped action.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PolicyRule {
    pub path_prefix: String,
    pub effect: PolicyEffect,
    pub reason: String,
}

impl PolicyRule {
    #[must_use]
    pub fn allow(path_prefix: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            path_prefix: path_prefix.into(),
            effect: PolicyEffect::Allow,
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn deny(path_prefix: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            path_prefix: path_prefix.into(),
            effect: PolicyEffect::Deny,
            reason: reason.into(),
        }
    }
}

/// Versioned P3 policy. It is the one authority for tool rules and approval mode.
#[derive(Clone, Debug)]
pub struct ToolPolicy {
    revision: u64,
    rules: Vec<PolicyRule>,
    tool_rules: Vec<ToolPatternRule>,
    turn_rules: ToolPolicyRules,
    mode: PolicyMode,
    max_process_timeout_ms: u64,
    /// Secret references the operator has exposed to tool processes. It is host
    /// configuration, never model input: with the default empty list, every
    /// `secret://` request is refused before an approval can even be proposed.
    granted_secrets: Vec<String>,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            revision: 1,
            rules: Vec::new(),
            tool_rules: Vec::new(),
            turn_rules: ToolPolicyRules::default(),
            mode: PolicyMode::Ask,
            max_process_timeout_ms: 60_000,
            granted_secrets: Vec::new(),
        }
    }
}

impl ToolPolicy {
    #[must_use]
    pub fn new(revision: u64, rules: Vec<PolicyRule>) -> Self {
        Self {
            revision,
            rules,
            ..Self::default()
        }
    }

    /// Set the approval mode within this same policy authority.
    #[must_use]
    pub fn with_mode(mut self, mode: PolicyMode) -> Self {
        if self.mode != mode {
            self.revision = self.revision.saturating_add(1);
            self.mode = mode;
        }
        self
    }

    /// Add allow/deny tool patterns. A policy revision changes whenever its
    /// effective rules change, invalidating any prepared stale approval.
    #[must_use]
    pub fn with_tool_rules(mut self, rules: Vec<ToolPatternRule>) -> Self {
        if !rules.is_empty() {
            self.revision = self.revision.saturating_add(rules.len() as u64);
            self.tool_rules.extend(rules);
        }
        self
    }

    /// Share turn-scoped confirmed rules with the approval host.
    #[must_use]
    pub fn with_turn_rules(mut self, rules: ToolPolicyRules) -> Self {
        self.turn_rules = rules;
        self
    }

    #[must_use]
    pub const fn mode(&self) -> PolicyMode {
        self.mode
    }

    #[must_use]
    pub fn tool_rules(&self) -> Vec<ToolPatternRule> {
        let mut rules = self.tool_rules.clone();
        if let Some((_, turn_rules)) = self.turn_rules.snapshot() {
            rules.extend(turn_rules);
        }
        rules
    }

    /// Evaluate deny rules first, then explicit allows, then the selected mode.
    /// Workspace containment/protected-path checks remain in `prepare` and run
    /// before this method or any approval UI.
    #[must_use]
    pub fn decide(&self, action: &CodingToolAction) -> Decision {
        if self.turn_rules.snapshot().is_none() {
            return Decision::Deny(
                "tool permission rules are unavailable; refusing the action".to_owned(),
            );
        }
        if let Some(reason) = self.denial_for(action) {
            return Decision::Deny(reason);
        }

        let target = tool_pattern_target(action);
        let tool_rules = self.tool_rules();
        if let Some(rule) = tool_rules.iter().find(|rule| {
            rule.effect == PolicyEffect::Deny && pattern_matches(&rule.pattern, &target)
        }) {
            return Decision::Deny(if rule.reason.trim().is_empty() {
                rule.pattern.clone()
            } else {
                rule.reason.clone()
            });
        }
        if let Some(rule) = tool_rules.iter().find(|rule| {
            rule.effect == PolicyEffect::Allow && pattern_matches(&rule.pattern, &target)
        }) {
            return Decision::Allow {
                reason: format!("rule {}", rule.pattern),
            };
        }

        // Reading the trusted skill catalogue changes nothing: listing skills,
        // loading one's instructions and reading its own files. Asking before each of
        // them put an approval panel between the model and every skill, and full-auto
        // did not cover them either, so skills were used far less than they matched.
        // Deny rules above still apply.
        if let CodingToolAction::ExternalTool {
            plugin_id,
            tool_name,
            ..
        } = action
            && ((plugin_id == "skill"
                && matches!(
                    tool_name.as_str(),
                    "list_skills" | "activate_skill" | "read_skill_file"
                ))
                // Marking the user's own goal complete changes only host state.
                || (plugin_id == "goal" && tool_name == "goal_complete"))
        {
            return Decision::Allow {
                reason: "skill catalogue (read-only)".to_owned(),
            };
        }
        // A web search sends a query to the search provider and nothing else, like the
        // prompt sent to the model provider. Opening a URL is different: the URL itself
        // can carry data out, so web_fetch asks unless the mode or a rule allows it.
        let web = matches!(action, CodingToolAction::ExternalTool { plugin_id, .. } if plugin_id == "web");
        if web
            && matches!(action, CodingToolAction::ExternalTool { tool_name, .. } if tool_name == "web_search")
        {
            return Decision::Allow {
                reason: "web search (query only)".to_owned(),
            };
        }

        match self.mode {
            PolicyMode::AutoEdit if auto_edit_action(action) => Decision::Allow {
                reason: "mode auto-edit".to_owned(),
            },
            PolicyMode::FullAuto
                if web || !matches!(action, CodingToolAction::ExternalTool { .. }) =>
            {
                Decision::Allow {
                    reason: "mode full-auto".to_owned(),
                }
            }
            PolicyMode::Ask | PolicyMode::AutoEdit | PolicyMode::FullAuto => Decision::Ask,
        }
    }

    #[must_use]
    pub fn with_process_timeout_cap(mut self, timeout_ms: u64) -> Self {
        self.max_process_timeout_ms = timeout_ms;
        self
    }

    /// Expose secret references to this host's tool processes.
    #[must_use]
    pub fn with_granted_secrets(mut self, secrets: Vec<String>) -> Self {
        self.granted_secrets = secrets;
        self
    }

    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision.saturating_add(
            self.turn_rules
                .snapshot()
                .map_or(u64::MAX, |(revision, _)| revision),
        )
    }

    /// Whether the host has exposed this exact reference to a tool process.
    #[must_use]
    pub fn allows_secret(&self, reference: &str) -> bool {
        self.granted_secrets
            .iter()
            .any(|granted| granted == reference)
    }

    /// Validate the original action shape, apply deterministic policy
    /// transforms, then validate the transformed action shape. This is the
    /// only path used by preparation and execution.
    pub fn transform_and_validate(
        &self,
        action: CodingToolAction,
    ) -> Result<CodingToolAction, HarnessError> {
        validate_action_shape(&action)?;
        self.validate_secret_grants(&action)?;
        let transformed = match action {
            CodingToolAction::RunProcess {
                executable,
                args,
                timeout_ms,
                isolation,
                env,
            } => CodingToolAction::RunProcess {
                executable,
                args,
                timeout_ms: timeout_ms.min(self.max_process_timeout_ms),
                isolation,
                env,
            },
            CodingToolAction::RunShell {
                command,
                timeout_ms,
                isolation,
                env,
            } => CodingToolAction::RunShell {
                command,
                timeout_ms: timeout_ms.min(self.max_process_timeout_ms),
                isolation,
                env,
            },
            action => action,
        };
        validate_action_shape(&transformed)?;
        Ok(transformed)
    }

    /// Refuse a process action that names a secret the host has not exposed.
    ///
    /// This runs before a proposal exists, so an un-granted reference can never
    /// be approved, never reaches an executor, and never becomes an intent.
    fn validate_secret_grants(&self, action: &CodingToolAction) -> Result<(), HarnessError> {
        for binding in env_bindings_of(action) {
            if !self.allows_secret(&binding.reference) {
                return Err(HarnessError::new(
                    ErrorCode::SecretNotGranted,
                    format!(
                        "secret reference {} is not exposed to this host's tool processes",
                        binding.reference
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Returns a policy denial without converting an allow into implicit
    /// execution authority. A matching parent deny always wins.
    #[must_use]
    pub fn denial_for(&self, action: &CodingToolAction) -> Option<String> {
        if self.turn_rules.snapshot().is_none() {
            return Some("tool permission rules are unavailable; refusing the action".to_owned());
        }
        let recursive = matches!(
            action,
            CodingToolAction::ListFiles { .. }
                | CodingToolAction::SearchText { .. }
                | CodingToolAction::Glob { .. }
                | CodingToolAction::GitDiff { .. }
                | CodingToolAction::GitLog { .. }
                | CodingToolAction::GitStatus
        );
        let path = action
            .path_hint()
            .or(if recursive { Some("") } else { None });
        let path_denial = path
            .and_then(|path| {
                self.rules
                    .iter()
                    .filter(|rule| {
                        rule_matches(&rule.path_prefix, path)
                            || (recursive && rule_matches(path, &rule.path_prefix))
                    })
                    .find(|rule| rule.effect == PolicyEffect::Deny)
            })
            .map(|rule| {
                if rule.reason.trim().is_empty() {
                    format!(
                        "policy denied {} at {}",
                        action.kind().as_str(),
                        rule.path_prefix
                    )
                } else {
                    rule.reason.clone()
                }
            });
        if let Some(reason) = path_denial {
            return Some(reason);
        }
        let target = tool_pattern_target(action);
        self.tool_rules()
            .into_iter()
            .find(|rule| {
                rule.effect == PolicyEffect::Deny && pattern_matches(&rule.pattern, &target)
            })
            .map(|rule| {
                if rule.reason.trim().is_empty() {
                    rule.pattern.clone()
                } else {
                    rule.reason.clone()
                }
            })
    }
}

fn pattern_matches(pattern: &str, target: &str) -> bool {
    globset::GlobBuilder::new(pattern)
        .case_insensitive(cfg!(windows))
        .build()
        .ok()
        .is_some_and(|glob| glob.compile_matcher().is_match(target))
}

fn tool_pattern_target(action: &CodingToolAction) -> String {
    match action {
        CodingToolAction::RunProcess {
            executable, args, ..
        } => format!(
            "run_process({})",
            std::iter::once(executable.as_str())
                .chain(args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        CodingToolAction::RunShell { command, .. } => format!("run_shell({command})"),
        CodingToolAction::ExternalTool { tool_name, .. } => format!("{tool_name}()"),
        CodingToolAction::Glob { pattern, .. } => format!("glob({pattern})"),
        _ => format!(
            "{}({})",
            action.kind().as_str(),
            action.path_hint().unwrap_or("")
        ),
    }
}

/// Stable `tool(pattern)` proposal used by the approval panel's explicit
/// always-allow confirmation.
#[must_use]
pub fn tool_pattern_for_action(action: &CodingToolAction) -> String {
    tool_pattern_target(action)
}

fn auto_edit_action(action: &CodingToolAction) -> bool {
    !matches!(
        action,
        CodingToolAction::RunProcess { .. }
            | CodingToolAction::RunShell { .. }
            | CodingToolAction::ExternalTool { .. }
            | CodingToolAction::TaskUpdate { .. }
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive shape match keeps every tool's typed validation fail-closed"
)]
fn validate_action_shape(action: &CodingToolAction) -> Result<(), HarnessError> {
    match action {
        CodingToolAction::ReadFile { path, .. }
        | CodingToolAction::ApplyPatch { path, .. }
        | CodingToolAction::WriteFile { path, .. }
        | CodingToolAction::EditFile { path, .. }
            if path.trim().is_empty() =>
        {
            Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "tool path must not be empty",
            ))
        }
        CodingToolAction::ListFiles { path }
        | CodingToolAction::SearchText { path, .. }
        | CodingToolAction::Glob { path, .. }
        | CodingToolAction::GitDiff { path }
        | CodingToolAction::GitLog { path, .. }
            if path.as_deref().is_some_and(|value| value.trim().is_empty()) =>
        {
            Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "optional tool path must not be blank",
            ))
        }
        CodingToolAction::SearchText { query, .. } if query.trim().is_empty() => Err(
            HarnessError::new(ErrorCode::InvalidPayload, "search query must not be empty"),
        ),
        CodingToolAction::Glob { pattern, .. } if pattern.trim().is_empty() => Err(
            HarnessError::new(ErrorCode::InvalidPayload, "glob pattern must not be empty"),
        ),
        CodingToolAction::EditFile { old_string, .. } if old_string.is_empty() => {
            Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "edit_file old_string must not be empty",
            ))
        }
        CodingToolAction::EditFile {
            old_string,
            new_string,
            ..
        } if old_string.contains('\0') || new_string.contains('\0') => Err(HarnessError::new(
            ErrorCode::BinaryContentDenied,
            "edit_file text must not contain NUL",
        )),
        CodingToolAction::WriteFile { content, .. }
            if content.contains('\0') || content.len() > 1024 * 1024 =>
        {
            Err(HarnessError::new(
                if content.contains('\0') {
                    ErrorCode::BinaryContentDenied
                } else {
                    ErrorCode::OutputLimitExceeded
                },
                "write_file content must be UTF-8 text no larger than 1 MiB",
            ))
        }
        CodingToolAction::RunProcess {
            executable,
            timeout_ms,
            ..
        } if executable.trim().is_empty() || *timeout_ms == 0 => Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "structured process executable and positive timeout are required",
        )),
        CodingToolAction::RunShell {
            command,
            timeout_ms,
            ..
        } if command.trim().is_empty() || *timeout_ms == 0 => Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "explicit shell command and positive timeout are required",
        )),
        CodingToolAction::TaskUpdate { note } if note.trim().is_empty() => Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "task update note must not be empty",
        )),
        CodingToolAction::ReadProcessOutput { artifact_id, .. }
            if artifact_id.trim().is_empty() =>
        {
            Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "captured artifact id must not be empty",
            ))
        }
        CodingToolAction::ReadProcessOutput { length, .. }
            if *length == 0 || *length > crate::PROCESS_OUTPUT_PAGE_MAX_BYTES =>
        {
            Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                format!(
                    "captured output page must be between 1 and {} bytes",
                    crate::PROCESS_OUTPUT_PAGE_MAX_BYTES
                ),
            ))
        }
        CodingToolAction::ApplyPatch { replacement, .. } if replacement.contains('\0') => {
            Err(HarnessError::new(
                ErrorCode::BinaryContentDenied,
                "replacement text must not contain NUL",
            ))
        }
        _ => Ok(()),
    }
}

/// The environment bindings of an action, or nothing for an action that starts
/// no process.
fn env_bindings_of(action: &CodingToolAction) -> &[crate::EnvBinding] {
    match action {
        CodingToolAction::RunProcess { env, .. } | CodingToolAction::RunShell { env, .. } => env,
        _ => &[],
    }
}

fn rule_matches(prefix: &str, path: &str) -> bool {
    let prefix = policy_components(prefix);
    let path = policy_components(path);
    path.starts_with(&prefix)
}

fn policy_components(path: &str) -> Vec<String> {
    path.split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .map(|part| {
            #[cfg(windows)]
            {
                part.to_lowercase()
            }
            #[cfg(not(windows))]
            {
                part.to_owned()
            }
        })
        .collect()
}

#[allow(dead_code)]
fn _assert_tool_kind_is_exhaustive(kind: ToolKind) -> &'static str {
    kind.as_str()
}

#[cfg(test)]
mod g05_policy_tests {
    use super::{Decision, PolicyMode, ToolPatternRule, ToolPolicy, ToolPolicyRules};
    use crate::{CodingToolAction, IsolationMode};

    fn process(executable: &str, args: &[&str]) -> CodingToolAction {
        CodingToolAction::RunProcess {
            executable: executable.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            timeout_ms: 5_000,
            isolation: IsolationMode::BestEffort,
            env: Vec::new(),
        }
    }

    #[test]
    fn g05_deny_rule_beats_allow_rule() {
        let policy = ToolPolicy::new(1, Vec::new()).with_tool_rules(vec![
            ToolPatternRule::allow("write_file(src/**)", "allow source edits"),
            ToolPatternRule::deny("write_file(src/secrets/**)", "protected by deny rule"),
        ]);
        let action = CodingToolAction::WriteFile {
            path: "src/secrets/token.txt".to_owned(),
            content: "redacted".to_owned(),
            expected_hash: None,
        };

        assert_eq!(
            policy.decide(&action),
            Decision::Deny("protected by deny rule".to_owned())
        );
    }

    /// Loading a trusted skill changes nothing, so it does not open a panel; a tool
    /// of any other plugin still asks, and a deny rule still wins.
    #[test]
    fn reading_the_skill_catalogue_needs_no_approval_but_other_plugins_do() {
        let external = |plugin: &str, tool: &str| CodingToolAction::ExternalTool {
            plugin_id: plugin.to_owned(),
            tool_name: tool.to_owned(),
            arguments: serde_json::json!({}),
            parent_invocation_id: None,
            timeout_ms: 5_000,
        };
        let policy = ToolPolicy::new(1, Vec::new());
        for tool in ["list_skills", "activate_skill", "read_skill_file"] {
            assert!(
                matches!(
                    policy.decide(&external("skill", tool)),
                    Decision::Allow { .. }
                ),
                "{tool}"
            );
        }
        assert_eq!(
            policy.decide(&external("mcp", "list_skills")),
            Decision::Ask
        );
        // Search sends a query only; opening a URL asks, unless the mode allows it.
        assert!(matches!(
            policy.decide(&external("web", "web_search")),
            Decision::Allow { .. }
        ));
        assert_eq!(policy.decide(&external("web", "web_fetch")), Decision::Ask);
        assert!(matches!(
            ToolPolicy::new(1, Vec::new())
                .with_mode(PolicyMode::FullAuto)
                .decide(&external("web", "web_fetch")),
            Decision::Allow { .. }
        ));
        assert_eq!(
            policy.decide(&external("skill", "write_anything")),
            Decision::Ask
        );
        // Marking the user's goal complete changes host state only.
        assert!(matches!(
            policy.decide(&external("goal", "goal_complete")),
            Decision::Allow { .. }
        ));
        assert_eq!(policy.decide(&external("goal", "other")), Decision::Ask);
        let denied = ToolPolicy::new(1, Vec::new()).with_tool_rules(vec![ToolPatternRule::deny(
            "activate_skill()",
            "no skills here",
        )]);
        assert!(matches!(
            denied.decide(&external("skill", "activate_skill")),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn g05_auto_edit_never_auto_runs_process_or_shell() {
        let policy = ToolPolicy::new(1, Vec::new()).with_mode(PolicyMode::AutoEdit);
        let shell = CodingToolAction::RunShell {
            command: "cargo test".to_owned(),
            timeout_ms: 5_000,
            isolation: IsolationMode::BestEffort,
            env: Vec::new(),
        };
        let write = CodingToolAction::WriteFile {
            path: "src/main.rs".to_owned(),
            content: "fn main() {}".to_owned(),
            expected_hash: None,
        };

        assert_eq!(policy.decide(&process("cargo", &["test"])), Decision::Ask);
        assert_eq!(policy.decide(&shell), Decision::Ask);
        assert_eq!(
            policy.decide(&write),
            Decision::Allow {
                reason: "mode auto-edit".to_owned()
            }
        );
    }

    #[test]
    fn g05_rule_pattern_matches_args_not_tool_name_only() {
        let policy = ToolPolicy::new(1, Vec::new()).with_tool_rules(vec![ToolPatternRule::allow(
            "run_process(cargo test *)",
            "test command",
        )]);

        assert!(matches!(
            policy.decide(&process("cargo", &["test", "--locked"])),
            Decision::Allow { .. }
        ));
        assert_eq!(
            policy.decide(&process("cargo", &["publish"])),
            Decision::Ask
        );
    }

    #[test]
    fn g05_confirmed_rule_applies_to_following_actions_in_current_turn() {
        let turn_rules = ToolPolicyRules::default();
        let policy = ToolPolicy::default().with_turn_rules(turn_rules.clone());
        let action = process("cargo", &["test", "--locked"]);
        let before = policy.revision();
        assert_eq!(policy.decide(&action), Decision::Ask);

        turn_rules
            .add(ToolPatternRule::allow(
                "run_process(cargo test *)",
                "user-confirmed rule",
            ))
            .expect("the panel pattern is valid");

        assert!(
            policy.revision() > before,
            "a rule change advances revision"
        );
        assert_eq!(
            policy.decide(&action),
            Decision::Allow {
                reason: "rule run_process(cargo test *)".to_owned()
            },
            "the confirmed rule is visible to the same policy used by the driver"
        );
    }
}
