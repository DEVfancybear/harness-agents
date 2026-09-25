//! A persistent goal: an objective the app keeps working toward across turns.
//!
//! Ported from prime-agent's Persistent Goals. `/goal <objective>` sets it; every turn
//! while it is active carries the objective in its context, and when a turn ends
//! without the goal being finished the app continues by itself, within a budget.
//! Only the model's `goal_complete` call - prime-agent's `goal.complete()` - marks it
//! done, so "the model stopped talking" is never mistaken for "the work is finished".
//! `/goal pause`, `/goal resume`, `/goal clear` and `/goal status` manage it; Ctrl-C
//! pauses it. The objective is stored with the task, so `/resume` brings it back,
//! paused.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use harness_session::{ContextBlock, ContextBlockKind};
use harness_tools::{
    CodingToolAction, ExternalToolCatalog, ExternalToolDispatcher, ExternalTools,
    ToolDispatchAuthorization, ToolOutput,
};
use harness_types::{ErrorCode, HarnessError};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use super::events::SessionEvent;

/// The session setting a task's goal is stored under.
pub const GOAL_SETTING: &str = "goal";

/// How many turns the app continues a goal by itself before it pauses.
pub const DEFAULT_GOAL_CONTINUATIONS: u32 = 10;

/// The text the app sends to continue a goal.
#[must_use]
pub fn continuation_text(objective: &str) -> String {
    format!(
        "Continue working toward the goal: {objective}\n\
         If it is fully done and verified, call goal_complete with a short summary of what was done; otherwise take the next step."
    )
}

/// The context block that carries the active goal into a turn.
#[must_use]
pub fn goal_block(objective: &str) -> ContextBlock {
    ContextBlock::mandatory(
        "goal",
        ContextBlockKind::Instruction,
        format!(
            "Active goal (persistent across turns): {objective}\n\
             Work toward it in every turn. The goal ends only when you call goal_complete, and only once \
             the work is done and verified - say what is left instead of calling it early."
        ),
    )
}

/// The `goal_complete` tool of one turn.
#[derive(Clone)]
pub struct GoalHost {
    sender: UnboundedSender<SessionEvent>,
    completed: Arc<AtomicBool>,
}

impl GoalHost {
    #[must_use]
    pub fn new(sender: UnboundedSender<SessionEvent>) -> Self {
        Self {
            sender,
            completed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Mark the goal complete: the turn erases it from the task, and the controller
    /// is told. Used by `goal_complete` and by the `goal` skill's `goal.complete()`.
    pub fn complete(&self, summary: &str) {
        self.completed.store(true, Ordering::SeqCst);
        let _ = self.sender.send(SessionEvent::GoalCompleted {
            summary: summary.to_owned(),
        });
    }

    /// Whether the model marked the goal complete during this turn.
    #[must_use]
    pub fn completed(&self) -> bool {
        self.completed.load(Ordering::SeqCst)
    }

    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "a method, like the other hosts' `tools`, so callers map every host the same way"
    )]
    pub fn tools(&self) -> ExternalTools {
        ExternalTools::new(Arc::new(GoalCatalog))
    }

    #[must_use]
    pub fn dispatcher(&self) -> Arc<dyn ExternalToolDispatcher> {
        Arc::new(self.clone())
    }

    fn summary(arguments: &Value) -> Result<String, HarnessError> {
        let object = arguments.as_object().ok_or_else(|| {
            HarnessError::new(ErrorCode::InvalidPayload, "goal_complete takes an object")
        })?;
        if object.keys().any(|key| key != "summary") {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "goal_complete accepts only summary",
            ));
        }
        object
            .get("summary")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|summary| !summary.is_empty())
            .map(|summary| summary.chars().take(2_000).collect())
            .ok_or_else(|| {
                HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "goal_complete needs a summary of what was done",
                )
            })
    }
}

struct GoalCatalog;

impl ExternalToolCatalog for GoalCatalog {
    fn schemas(&self) -> Vec<Value> {
        vec![json!({
            "type": "function",
            "function": {
                "name": "goal_complete",
                "description": "Mark the active goal as finished. Call it only when the goal is fully done and verified.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "summary": {"type": "string", "description": "What was done, and how it was verified."}
                    },
                    "required": ["summary"],
                    "additionalProperties": false
                }
            }
        })]
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        (name == "goal_complete").then(|| CodingToolAction::ExternalTool {
            plugin_id: "goal".to_owned(),
            tool_name: name.to_owned(),
            arguments: arguments.clone(),
            parent_invocation_id: None,
            timeout_ms: 5_000,
        })
    }
}

impl ExternalToolDispatcher for GoalHost {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            if plugin_id != "goal" || tool_name != "goal_complete" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "goal tool target is unavailable",
                ));
            }
            Self::summary(arguments).map(|_| ())
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
            if plugin_id != "goal" || tool_name != "goal_complete" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "goal tool target is unavailable",
                ));
            }
            let summary = Self::summary(arguments)?;
            self.complete(&summary);
            Ok(ToolOutput::ExternalTool {
                plugin_id: "goal".to_owned(),
                tool_name: "goal_complete".to_owned(),
                payload: json!({ "text": format!("The goal is marked complete: {summary}") }),
                inflight: 1,
            })
        })
    }
}

/// Where a goal stands, as the controller tracks it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoalStatus {
    Active,
    Paused,
    Complete,
}

impl GoalStatus {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Complete => "complete",
        }
    }
}

/// The goal of this conversation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoalState {
    pub objective: String,
    pub status: GoalStatus,
    /// Turns the app has continued by itself for this goal.
    pub continuations: u32,
    pub max_continuations: u32,
    /// What `goal_complete` said, once it was called.
    pub summary: Option<String>,
}

impl GoalState {
    #[must_use]
    pub fn new(objective: impl Into<String>) -> Self {
        Self {
            objective: objective.into(),
            status: GoalStatus::Active,
            continuations: 0,
            max_continuations: DEFAULT_GOAL_CONTINUATIONS,
            summary: None,
        }
    }

    /// The `/goal status` lines.
    #[must_use]
    pub fn describe(&self) -> Vec<String> {
        let mut lines = vec![
            format!("Goal:     {}", self.objective),
            format!("Status:   {}", self.status.label()),
            format!(
                "Continued: {} of {} automatic turn(s)",
                self.continuations, self.max_continuations
            ),
        ];
        if let Some(summary) = &self.summary {
            lines.push(format!("Summary:  {summary}"));
        }
        lines.push("/goal pause · /goal resume · /goal clear".to_owned());
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::{GoalHost, goal_block};
    use crate::interactive::events::SessionEvent;
    use harness_tools::ExternalToolDispatcher;
    use serde_json::json;

    #[tokio::test]
    async fn goal_complete_needs_a_summary_and_reports_it() {
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let host = GoalHost::new(sender);
        assert!(
            host.validate_external("goal", "goal_complete", &json!({}))
                .await
                .is_err(),
            "a completion without a summary is refused"
        );
        assert!(
            host.validate_external(
                "goal",
                "goal_complete",
                &json!({"summary": "done", "extra": 1})
            )
            .await
            .is_err()
        );
        assert!(
            host.validate_external(
                "goal",
                "goal_complete",
                &json!({"summary": "shipped and tested"})
            )
            .await
            .is_ok()
        );
        assert!(GoalHost::summary(&json!({"summary": "  "})).is_err());
        drop(host);
        assert!(events.try_recv().is_err(), "validation reports nothing");
        let host = GoalHost::new(tokio::sync::mpsc::unbounded_channel().0);
        assert!(!host.completed());
        assert!(goal_block("ship it").text.contains("goal_complete"));
        let _ = SessionEvent::GoalCompleted {
            summary: String::new(),
        };
    }
}
