//! prime-agent's branch summary (`compaction/branch-summarization.ts`): when
//! `/tree` goes back to an earlier turn, the turns being left can be summarised,
//! and the summary reaches the model with the next message, so the conversation
//! remembers what the abandoned branch found.

use std::fmt::Write as _;
use std::sync::Arc;

use harness_providers::{MessageRole, ModelProvider, ProviderMessage};
use harness_runtime::SummaryProvider;
use harness_store_sqlite::SqliteStore;
use harness_types::SessionId;

/// prime-agent's `SUMMARIZATION_SYSTEM_PROMPT`.
const SYSTEM: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI coding assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

/// prime-agent's `BRANCH_SUMMARY_PROMPT`.
const PROMPT: &str = "Create a structured summary of this conversation branch for context when returning later.\n\nUse this EXACT format:\n\n## Goal\n[What was the user trying to accomplish in this branch?]\n\n## Constraints & Preferences\n- [Any constraints, preferences, or requirements mentioned]\n- [Or \"(none)\" if none were mentioned]\n\n## Progress\n### Done\n- [x] [Completed tasks/changes]\n\n### In Progress\n- [ ] [Work that was started but not finished]\n\n### Blocked\n- [Issues preventing progress, if any]\n\n## Key Decisions\n- **[Decision]**: [Brief rationale]\n\n## Next Steps\n1. [What should happen next to continue this work]\n\nKeep each section concise. Preserve exact file paths, function names, and error messages.";

/// prime-agent's `BRANCH_SUMMARY_PREAMBLE`.
const PREAMBLE: &str = "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n";

/// prime-agent's completion budget for the summary call.
const BUDGET_TOKENS: u64 = 2048;

/// A branch summary the next turn writes: the turn being left, and the user's
/// extra focus (prime-agent's "Summarize with custom prompt").
#[derive(Clone, Debug)]
pub struct BranchPlan {
    pub leaf: SessionId,
    pub focus: Option<String>,
}

/// The messages of the abandoned branch: what the conversation ending at `leaf`
/// holds past the one ending at `target`.
async fn abandoned(
    store: &SqliteStore,
    leaf: &SessionId,
    target: &SessionId,
) -> Result<Vec<ProviderMessage>, String> {
    let from_leaf = harness_runtime::conversation_history(store, leaf)
        .await
        .map_err(|error| error.to_string())?
        .messages;
    let up_to_target = harness_runtime::conversation_history(store, target)
        .await
        .map_err(|error| error.to_string())?
        .messages;
    Ok(if from_leaf.starts_with(&up_to_target) {
        from_leaf[up_to_target.len()..].to_vec()
    } else {
        from_leaf
    })
}

/// The prompt prime-agent sends: the branch as a transcript, then its format.
fn prompt(messages: &[ProviderMessage], focus: Option<&str>) -> String {
    let mut transcript = String::new();
    for message in messages {
        let role = match message.role {
            MessageRole::User => "User",
            MessageRole::Assistant => "Assistant",
            MessageRole::Tool => "Tool result",
            MessageRole::System => "System",
        };
        let text: String = message.content.chars().take(6_000).collect();
        let _ = writeln!(transcript, "{role}: {text}");
        for call in &message.tool_calls {
            let arguments: String = call.arguments.chars().take(400).collect();
            let _ = writeln!(transcript, "Assistant called {}({arguments})", call.name);
        }
    }
    let mut prompt = format!("{SYSTEM}\n\n<conversation>\n{transcript}</conversation>\n\n{PROMPT}");
    if let Some(focus) = focus.filter(|focus| !focus.trim().is_empty()) {
        let _ = write!(prompt, "\n\nAdditional focus: {}", focus.trim());
    }
    prompt
}

/// Summarise the branch from `plan.leaf` back to `target` with `provider`, as
/// prime-agent's stored summary (preamble, then the model's text). `None` when
/// the branch holds nothing to summarise.
///
/// # Errors
/// The store cannot be read or the model cannot write the summary.
pub async fn summarize(
    store: &SqliteStore,
    plan: &BranchPlan,
    target: &SessionId,
    provider: Arc<dyn ModelProvider>,
) -> Result<Option<String>, String> {
    let messages = abandoned(store, &plan.leaf, target).await?;
    if messages.is_empty() {
        return Ok(None);
    }
    let prompt = prompt(&messages, plan.focus.as_deref());
    let summarizer = harness_runtime::ModelSummaryProvider::new(provider);
    let summary = tokio::task::spawn_blocking(move || {
        summarizer.summarize_conversation(&prompt, BUDGET_TOKENS)
    })
    .await
    .map_err(|_| "the branch summary did not complete".to_owned())?
    .map_err(|error| error.to_string())?;
    Ok(Some(format!("{PREAMBLE}{summary}")))
}

/// The summary as the model reads it, prime-agent's `BRANCH_SUMMARY_PREFIX` and
/// `_SUFFIX`, ahead of the next message.
#[must_use]
pub fn wrap(summary: &str, message: &str) -> String {
    format!(
        "[branch-summary]\n\nThe following is a summary of a branch that this conversation came back from:\n\n<summary>\n{summary}\n</summary>\n\n{message}"
    )
}

#[cfg(test)]
mod tests {
    use super::{prompt, wrap};
    use harness_providers::{MessageRole, ProviderMessage};

    #[test]
    fn the_prompt_is_prime_agents_with_the_branch_and_the_focus() {
        let messages = vec![
            ProviderMessage::new(MessageRole::User, "try the cache"),
            ProviderMessage::new(MessageRole::Assistant, "the cache is slower"),
        ];
        let text = prompt(&messages, Some("performance"));
        assert!(text.starts_with("You are a context summarization assistant."));
        assert!(text.contains(
            "<conversation>\nUser: try the cache\nAssistant: the cache is slower\n</conversation>"
        ));
        assert!(text.contains("## Goal") && text.contains("## Next Steps"));
        assert!(text.ends_with("\n\nAdditional focus: performance"));
        assert!(!prompt(&messages, None).contains("Additional focus"));
    }

    #[test]
    fn the_model_reads_the_summary_ahead_of_the_next_message() {
        assert_eq!(
            wrap("S", "next"),
            "[branch-summary]\n\nThe following is a summary of a branch that this conversation came back from:\n\n<summary>\nS\n</summary>\n\nnext"
        );
    }
}
