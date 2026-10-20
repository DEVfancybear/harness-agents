//! What a hook answers, read the way Claude Code reads it.
//!
//! A hook is a command that gets one JSON object on stdin and answers with its
//! exit code and, optionally, one JSON object on stdout:
//!
//! - exit `0` with no JSON: go on;
//! - exit `2`: block, the first line of stdout (else stderr) is the reason;
//! - any other exit, a timeout or a hook that cannot start: a failure. A
//!   `pre_tool_use` failure blocks the call (a guard that did not answer is
//!   not a guard that said yes); any other event reports it and goes on;
//! - exit `0` with a JSON object on stdout: Claude Code's common fields -
//!   `continue: false` with `stopReason` ends the turn, `systemMessage` is shown
//!   to the user, `decision: "block"` with `reason` blocks - and its
//!   `hookSpecificOutput` - `permissionDecision` (`"deny"` blocks, `"ask"`
//!   asks the user even where the policy would allow; `"allow"` cannot skip an
//!   approval here), `permissionDecisionReason`, `updatedInput` and
//!   `additionalContext`.
//!
//! These are prime-agent's agent-loop hook powers - block a call, add to what a
//! result tells the model, stop the run, keep the loop going with a message -
//! offered through the hook shape ha already has.

use std::path::Path;

use harness_providers::CancellationToken;
use serde_json::Value;

use crate::{ConfiguredToolHook, process};

/// The longest reason or context one hook can hand over, in characters.
const MAX_HOOK_TEXT: usize = 4_000;

/// Every event a hook can be configured for.
pub const HOOK_EVENTS: &[&str] = &[
    "pre_tool_use",
    "post_tool_use",
    "stop",
    "subagent_stop",
    "notification",
    "user_prompt_submit",
    "session_start",
    "session_end",
    "pre_compact",
];

/// What the hooks of one event answered together.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookResponse {
    /// Why the action, the prompt or the stop is blocked; the first hook that
    /// blocks wins.
    pub block: Option<String>,
    /// A hook asked for the user's approval (`permissionDecision: "ask"`).
    pub ask: bool,
    /// The tool input a `pre_tool_use` hook replaced (`updatedInput`).
    pub updated_input: Option<Value>,
    /// Text for the model (`additionalContext`, or plain stdout where Claude
    /// Code reads it as context).
    pub context: Vec<String>,
    /// The turn ends after this event (`continue: false`), with this reason.
    pub stop: Option<String>,
    /// Messages for the user (`systemMessage`) and failures of hooks that did
    /// not answer.
    pub notices: Vec<String>,
    /// What the host itself adds for the model, shown apart from the hooks'
    /// feedback (instructions of a directory the call reached).
    pub host_context: Vec<String>,
}

impl HookResponse {
    fn merge(&mut self, other: Self) {
        if self.block.is_none() {
            self.block = other.block;
        }
        self.ask |= other.ask;
        if other.updated_input.is_some() {
            self.updated_input = other.updated_input;
        }
        self.context.extend(other.context);
        if self.stop.is_none() {
            self.stop = other.stop;
        }
        self.notices.extend(other.notices);
        self.host_context.extend(other.host_context);
    }

    /// The context lines joined for the model, or `None` when there are none.
    #[must_use]
    pub fn context_text(&self) -> Option<String> {
        (!self.context.is_empty()).then(|| self.context.join("\n"))
    }

    /// The host's own context for the model, when there is any.
    #[must_use]
    pub fn host_context_text(&self) -> Option<String> {
        (!self.host_context.is_empty()).then(|| self.host_context.join("\n\n"))
    }
}

/// Whether `matcher` selects `tool_name`: `*`, names joined with `|`, or a
/// regular expression, as Claude Code matches (`mcp__.*`, `Edit|Write`).
#[must_use]
pub fn hook_matches(matcher: Option<&str>, tool_name: &str) -> bool {
    // Matchers come from configuration, so there are few of them, and every
    // tool call checks each one: compile each once per process.
    static COMPILED: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Option<regex::Regex>>>,
    > = std::sync::OnceLock::new();
    let Some(matcher) = matcher.map(str::trim).filter(|matcher| !matcher.is_empty()) else {
        return true;
    };
    if matcher
        .split('|')
        .map(str::trim)
        .any(|pattern| pattern == "*" || pattern == tool_name)
    {
        return true;
    }
    let compiled = COMPILED.get_or_init(Default::default);
    let Ok(mut compiled) = compiled.lock() else {
        return regex::Regex::new(&format!("^(?:{matcher})$"))
            .is_ok_and(|pattern| pattern.is_match(tool_name));
    };
    compiled
        .entry(matcher.to_owned())
        .or_insert_with(|| regex::Regex::new(&format!("^(?:{matcher})$")).ok())
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(tool_name))
}

/// The hooks configured for `event` that select `tool_name` (every one of
/// them when the event has no tool).
pub fn hooks_for<'a>(
    hooks: &'a [ConfiguredToolHook],
    event: &'a str,
    tool_name: Option<&'a str>,
) -> impl Iterator<Item = &'a ConfiguredToolHook> {
    hooks.iter().filter(move |hook| {
        hook.event == event
            && tool_name.is_none_or(|name| hook_matches(hook.matcher.as_deref(), name))
    })
}

/// Run the hooks of `event` one after another and merge what they answered.
///
/// `fail_closed` turns a hook that failed (a crash, a timeout, an exit other
/// than 0 or 2) into a block, which is how `pre_tool_use` has always treated it.
pub async fn run_hooks(
    hooks: &[ConfiguredToolHook],
    event: &str,
    tool_name: Option<&str>,
    payload: &Value,
    fail_closed: bool,
    cancellation: &CancellationToken,
) -> HookResponse {
    let mut merged = HookResponse::default();
    for hook in hooks_for(hooks, event, tool_name) {
        match run_one(hook, payload.clone(), cancellation.clone()).await {
            Ok(response) => {
                let blocked = response.block.is_some();
                let mut response = response;
                if let Some(reason) = &mut response.block {
                    // Outside `pre_tool_use` the reason is also the user's to read:
                    // it goes back to the model, or sends a stopping turn on.
                    if event != "pre_tool_use" {
                        response
                            .notices
                            .push(format!("{event} hook from {}: {reason}", hook.source));
                    }
                    *reason = format!("{reason} ({})", hook.source);
                }
                merged.merge(response);
                // As before: the first hook that blocks a call decides it.
                if blocked && event == "pre_tool_use" {
                    break;
                }
            }
            Err(reason) if fail_closed => {
                merged.merge(HookResponse {
                    block: Some(format!("{reason} ({})", hook.source)),
                    ..HookResponse::default()
                });
                break;
            }
            Err(reason) => merged.notices.push(format!(
                "{event} hook from {} failed: {reason}",
                hook.source
            )),
        }
    }
    merged
}

async fn run_one(
    hook: &ConfiguredToolHook,
    mut payload: Value,
    cancellation: CancellationToken,
) -> Result<HookResponse, String> {
    payload["event"] = Value::String(hook.event.clone());
    payload["hook_event_name"] = Value::String(hook.event.clone());
    let input =
        serde_json::to_vec(&payload).map_err(|_| "hook input is invalid JSON".to_owned())?;
    let cwd = payload["cwd"]
        .as_str()
        .map_or_else(|| Path::new("."), Path::new);
    let result = process::run_hook_command(
        cwd,
        &hook.command,
        &hook.args,
        &input,
        hook.timeout_seconds.min(60).saturating_mul(1000),
        cancellation,
    )
    .await
    .map_err(|error| error.to_string())?;
    if result.status == process::HookProcessStatus::TimedOut {
        return Err("hook timed out".to_owned());
    }
    if result.status == process::HookProcessStatus::Canceled {
        return Err("hook was canceled".to_owned());
    }
    match result.exit_code {
        Some(0) => Ok(read_answer(&hook.event, &result.stdout)),
        Some(2) => Ok(HookResponse {
            block: Some(
                result
                    .stdout
                    .trim()
                    .lines()
                    .next()
                    .or_else(|| result.stderr.trim().lines().next())
                    .filter(|line| !line.is_empty())
                    .map_or_else(
                        || "hook exited with status 2".to_owned(),
                        |line| line.chars().take(512).collect::<String>(),
                    ),
            ),
            ..HookResponse::default()
        }),
        code => Err(format!("hook exited with status {code:?}")),
    }
}

fn bounded(text: &str) -> String {
    text.trim().chars().take(MAX_HOOK_TEXT).collect()
}

/// Read what a hook that exited 0 printed.
fn read_answer(event: &str, stdout: &str) -> HookResponse {
    let trimmed = stdout.trim();
    let Some(Value::Object(answer)) = trimmed
        .starts_with('{')
        .then(|| serde_json::from_str::<Value>(trimmed).ok())
        .flatten()
    else {
        // Claude Code adds plain stdout to the context for these two events only.
        return HookResponse {
            context: if matches!(event, "user_prompt_submit" | "session_start")
                && !trimmed.is_empty()
            {
                vec![bounded(trimmed)]
            } else {
                Vec::new()
            },
            ..HookResponse::default()
        };
    };
    let text = |value: Option<&Value>| value.and_then(Value::as_str).map(bounded);
    let mut response = HookResponse::default();
    if answer.get("continue").and_then(Value::as_bool) == Some(false) {
        response.stop = Some(
            text(answer.get("stopReason"))
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| "a hook stopped the turn".to_owned()),
        );
    }
    if let Some(message) = text(answer.get("systemMessage")).filter(|text| !text.is_empty()) {
        response.notices.push(message);
    }
    if answer.get("decision").and_then(Value::as_str) == Some("block") {
        response.block = Some(
            text(answer.get("reason"))
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| "blocked by hook".to_owned()),
        );
    }
    if let Some(Value::Object(specific)) = answer.get("hookSpecificOutput") {
        let reason = text(specific.get("permissionDecisionReason")).filter(|text| !text.is_empty());
        match specific.get("permissionDecision").and_then(Value::as_str) {
            Some("deny") => {
                response.block = Some(reason.unwrap_or_else(|| "denied by hook".to_owned()));
            }
            Some("ask") => response.ask = true,
            _ => {}
        }
        if let Some(input @ Value::Object(_)) = specific.get("updatedInput") {
            response.updated_input = Some(input.clone());
        }
        if let Some(context) =
            text(specific.get("additionalContext")).filter(|text| !text.is_empty())
        {
            response.context.push(context);
        }
    }
    response
}

/// Bound a payload to the 8 KiB a hook's stdin takes: the fields named by
/// `pointers` (JSON pointers, the tool output first, then its input) are cut
/// to a preview, in that order, until the payload fits.
#[must_use]
pub fn fit_payload(mut payload: Value, pointers: &[&str]) -> Value {
    const LIMIT: usize = 8 * 1024;
    let size =
        |payload: &Value| serde_json::to_vec(payload).map_or(usize::MAX, |bytes| bytes.len());
    // Compact JSON serializes a value the same wherever it sits, so the whole
    // payload is measured once and each cut adjusts the total by the slot's own
    // size: a megabyte of tool output is no longer re-serialized on every
    // shrinking pass.
    let mut total = size(&payload);
    for pointer in pointers {
        let Some(original) = payload.pointer(pointer).map(|value| match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        }) else {
            continue;
        };
        let mut slot_size = payload.pointer(pointer).map_or(0, size);
        let mut keep = original.chars().count();
        while total > LIMIT {
            keep = if keep > 256 { keep * 3 / 4 } else { 0 };
            let cut = if keep == 0 {
                serde_json::json!({ "truncated": true })
            } else {
                let preview: String = original.chars().take(keep).collect();
                serde_json::json!({ "truncated": true, "preview": preview })
            };
            let cut_size = size(&cut);
            if let Some(slot) = payload.pointer_mut(pointer) {
                *slot = cut;
                total = total.saturating_sub(slot_size).saturating_add(cut_size);
                slot_size = cut_size;
            }
            if keep == 0 {
                break;
            }
        }
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::{fit_payload, hook_matches, read_answer};
    use serde_json::json;

    #[test]
    fn matchers_take_names_alternatives_and_patterns() {
        assert!(hook_matches(None, "write_file"));
        assert!(hook_matches(Some("*"), "write_file"));
        assert!(hook_matches(Some("edit_file|write_file"), "write_file"));
        assert!(hook_matches(Some("mcp__.*"), "mcp__github__create_issue"));
        assert!(!hook_matches(Some("read_file"), "read_file_range"));
        assert!(!hook_matches(Some("edit_file|write_file"), "run_shell"));
    }

    #[test]
    fn a_json_answer_is_read_as_claude_code_reads_it() {
        let answer = read_answer(
            "pre_tool_use",
            &json!({
                "continue": false,
                "stopReason": "budget spent",
                "systemMessage": "checked",
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": "no writes to vendor/",
                    "updatedInput": {"path": "src/a.rs"},
                    "additionalContext": "vendor/ is generated"
                }
            })
            .to_string(),
        );
        assert_eq!(answer.block.as_deref(), Some("no writes to vendor/"));
        assert_eq!(answer.stop.as_deref(), Some("budget spent"));
        assert_eq!(answer.notices, ["checked"]);
        assert_eq!(answer.updated_input, Some(json!({"path": "src/a.rs"})));
        assert_eq!(answer.context, ["vendor/ is generated"]);
        let asked = read_answer(
            "pre_tool_use",
            r#"{"hookSpecificOutput":{"permissionDecision":"ask"}}"#,
        );
        assert!(asked.ask && asked.block.is_none());
        let stop = read_answer("stop", r#"{"decision":"block","reason":"tests fail"}"#);
        assert_eq!(stop.block.as_deref(), Some("tests fail"));
    }

    #[test]
    fn plain_stdout_is_context_only_where_claude_code_reads_it() {
        assert_eq!(
            read_answer("user_prompt_submit", "branch: main\n").context,
            ["branch: main"]
        );
        assert!(read_answer("post_tool_use", "noise").context.is_empty());
    }

    #[test]
    fn a_large_payload_is_cut_to_the_stdin_limit() {
        let payload = fit_payload(
            json!({"tool_input": {"content": "x".repeat(20_000)}, "tool_response": "y".repeat(20_000)}),
            &["/tool_response", "/tool_input"],
        );
        assert!(payload.to_string().len() <= 8 * 1024);
        assert_eq!(payload["tool_response"]["truncated"], json!(true));
    }
}
