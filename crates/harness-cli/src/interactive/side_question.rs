//! prime-agent's `/btw`: a question on the side of the conversation.
//!
//! The model is asked with the conversation so far as context - the session's
//! system prompt, its history and the side thread's earlier turns - and answers
//! without tools. Nothing is written to the session: the next turn of the main
//! conversation does not see the side thread (`core/side-question.ts`).

use harness_providers::{MessageRole, ProviderMessage};

/// prime-agent's instruction for the first side question, word for word.
pub const SIDE_QUESTION_INSTRUCTION: &str = "The user asked this via `/btw` — a temporary side thread cloned from the main conversation to answer a question without interrupting the main work. Tools (including `ipython`) are deactivated in this side thread and return an error if called; answer using only the conversation context above. The user may send follow-up side questions. Nothing here is added to the main session, so don't start or plan main-session work from this thread.";

/// prime-agent's bound on a side question's turns: a model that keeps calling
/// the refused tools gets three chances to answer.
pub const MAX_TURNS: usize = 3;

/// prime-agent's `SIDE_QUESTION_TOOL_BLOCKED`: the result every call gets.
pub const TOOL_BLOCKED: &str =
    "Tools are deactivated in this side thread. Answer from the conversation context.";

/// Answer each call of `response` with the refusal, so the model answers in
/// words on the next turn.
pub fn refuse_calls(
    messages: &mut Vec<ProviderMessage>,
    response: &harness_providers::ProviderResponse,
) {
    messages.push(ProviderMessage::assistant_with_calls(
        response.text.clone(),
        response
            .tool_calls
            .iter()
            .map(|call| {
                harness_providers::ProviderToolCall::new(
                    call.call_id.clone(),
                    call.name.clone(),
                    call.arguments.clone(),
                )
            })
            .collect(),
    ));
    for call in &response.tool_calls {
        messages.push(ProviderMessage::tool_result(
            call.call_id.clone(),
            TOOL_BLOCKED,
        ));
    }
}

/// prime-agent's side-question message: the instruction leads the first one.
#[must_use]
pub fn prompt(question: &str, first: bool) -> String {
    let body = if first {
        format!("{SIDE_QUESTION_INSTRUCTION}\n\n{question}")
    } else {
        question.to_owned()
    };
    format!("<side_question>\n{body}\n</side_question>")
}

/// The request's messages: the system prompt, the conversation, the side
/// thread so far, then the new question.
#[must_use]
pub fn messages(
    system_prompt: &str,
    summary: Option<&str>,
    history: Vec<ProviderMessage>,
    earlier: &[(String, String)],
    question: &str,
) -> Vec<ProviderMessage> {
    let mut messages = Vec::new();
    if !system_prompt.trim().is_empty() {
        messages.push(ProviderMessage::new(MessageRole::System, system_prompt));
    }
    if let Some(summary) = summary {
        messages.push(ProviderMessage::new(
            MessageRole::User,
            format!(
                "[The conversation before this point was compacted. Summary of it:]\n\n{summary}"
            ),
        ));
    }
    messages.extend(history);
    for (index, (asked, answered)) in earlier.iter().enumerate() {
        messages.push(ProviderMessage::new(
            MessageRole::User,
            prompt(asked, index == 0),
        ));
        messages.push(ProviderMessage::new(
            MessageRole::Assistant,
            answered.clone(),
        ));
    }
    messages.push(ProviderMessage::new(
        MessageRole::User,
        prompt(question, earlier.is_empty()),
    ));
    messages
}

#[cfg(test)]
mod tests {
    use super::{SIDE_QUESTION_INSTRUCTION, messages, prompt};
    use harness_providers::{MessageRole, ProviderMessage};

    /// The first side question carries prime-agent's instruction; a follow-up
    /// carries only itself, after the earlier side turns.
    #[test]
    fn q07_side_question_sends_history_and_the_thread() {
        let history = vec![
            ProviderMessage::new(MessageRole::User, "fix the parser"),
            ProviderMessage::new(MessageRole::Assistant, "fixed src/parser.rs"),
        ];
        let first = messages("system", None, history.clone(), &[], "which file?");
        assert_eq!(first.len(), 4);
        assert_eq!(first[0].role, MessageRole::System);
        assert_eq!(first[2].content, "fixed src/parser.rs");
        assert_eq!(
            first[3].content,
            format!(
                "<side_question>\n{SIDE_QUESTION_INSTRUCTION}\n\nwhich file?\n</side_question>"
            )
        );
        let earlier = vec![("which file?".to_owned(), "src/parser.rs".to_owned())];
        let follow = messages("system", Some("older work"), history, &earlier, "why?");
        assert!(follow[1].content.contains("older work"));
        assert_eq!(follow[4].content, prompt("which file?", true));
        assert_eq!(follow[5].content, "src/parser.rs");
        assert_eq!(follow[6].content, "<side_question>\nwhy?\n</side_question>");
    }
}
