use harness_types::{ErrorCode, HarnessError};
use serde::{Deserialize, Serialize};

use crate::{CodingToolAction, ToolKind};

/// A path-scoped policy result. Denial is monotonic: any matching deny wins
/// regardless of a deeper matching allow.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyEffect {
    Allow,
    Deny,
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

/// Versioned P3 policy. It always requires an explicit approval for an action
/// that is not denied; `Allow` only means the policy permits proposing it.
#[derive(Clone, Debug)]
pub struct ToolPolicy {
    revision: u64,
    rules: Vec<PolicyRule>,
    max_process_timeout_ms: u64,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            revision: 1,
            rules: Vec::new(),
            max_process_timeout_ms: 60_000,
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

    #[must_use]
    pub fn with_process_timeout_cap(mut self, timeout_ms: u64) -> Self {
        self.max_process_timeout_ms = timeout_ms;
        self
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Validate the original action shape, apply deterministic policy
    /// transforms, then validate the transformed action shape. This is the
    /// only path used by preparation and execution.
    pub fn transform_and_validate(
        &self,
        action: CodingToolAction,
    ) -> Result<CodingToolAction, HarnessError> {
        validate_action_shape(&action)?;
        let transformed = match action {
            CodingToolAction::RunProcess {
                executable,
                args,
                timeout_ms,
                isolation,
            } => CodingToolAction::RunProcess {
                executable,
                args,
                timeout_ms: timeout_ms.min(self.max_process_timeout_ms),
                isolation,
            },
            CodingToolAction::RunShell {
                command,
                timeout_ms,
                isolation,
            } => CodingToolAction::RunShell {
                command,
                timeout_ms: timeout_ms.min(self.max_process_timeout_ms),
                isolation,
            },
            action => action,
        };
        validate_action_shape(&transformed)?;
        Ok(transformed)
    }

    /// Returns a policy denial without converting an allow into implicit
    /// execution authority. A matching parent deny always wins.
    #[must_use]
    pub fn denial_for(&self, action: &CodingToolAction) -> Option<String> {
        let path = action.path_hint()?;
        self.rules
            .iter()
            .filter(|rule| rule_matches(&rule.path_prefix, path))
            .find(|rule| rule.effect == PolicyEffect::Deny)
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
            })
    }
}

fn validate_action_shape(action: &CodingToolAction) -> Result<(), HarnessError> {
    match action {
        CodingToolAction::ReadFile { path } | CodingToolAction::ApplyPatch { path, .. }
            if path.trim().is_empty() =>
        {
            Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "tool path must not be empty",
            ))
        }
        CodingToolAction::ListFiles { path }
        | CodingToolAction::SearchText { path, .. }
        | CodingToolAction::GitDiff { path }
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
        CodingToolAction::ApplyPatch { replacement, .. } if replacement.contains('\0') => {
            Err(HarnessError::new(
                ErrorCode::BinaryContentDenied,
                "replacement text must not contain NUL",
            ))
        }
        _ => Ok(()),
    }
}

fn rule_matches(prefix: &str, path: &str) -> bool {
    let prefix = prefix.trim_matches(&['/', '\\'][..]);
    let path = path.trim_matches(&['/', '\\'][..]);
    prefix.is_empty()
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('\\'))
}

#[allow(dead_code)]
fn _assert_tool_kind_is_exhaustive(kind: ToolKind) -> &'static str {
    kind.as_str()
}
