//! Rendering helpers: pure string production with no terminal state.

use super::events::AppPhase;

/// The prompt marker shows the phase, so a busy prompt never looks idle.
#[must_use]
pub fn prompt_prefix(phase: AppPhase) -> &'static str {
    match phase {
        AppPhase::Running | AppPhase::Canceling => ".. ",
        _ => "> ",
    }
}

#[must_use]
pub fn prompt_line(phase: AppPhase, buffer: &str) -> String {
    format!("{}{buffer}", prompt_prefix(phase))
}

#[must_use]
pub fn help_lines() -> Vec<String> {
    vec![
        "/help            list these commands".to_owned(),
        "/status          show project, config, data and provider state".to_owned(),
        "/new             start a new session when nothing is running".to_owned(),
        "/model           show which model the next run would use".to_owned(),
        "/config          show the resolved configuration and data files".to_owned(),
        "/resume <id>     resume a persisted session".to_owned(),
        "/exit            leave the app".to_owned(),
        "Ctrl-C cancels an active run or clears an idle prompt; Ctrl-D on an empty line exits."
            .to_owned(),
    ]
}

#[must_use]
pub fn tool_line(name: &str, detail: &str) -> String {
    if detail.is_empty() {
        format!("[tool] {name}")
    } else {
        format!("[tool] {name} {detail}")
    }
}

/// The proposal block shown before a gated action runs.
#[must_use]
pub fn approval_lines(
    action: &str,
    summary: &str,
    workspace: &str,
    scope: &str,
    request_id: &str,
) -> Vec<String> {
    vec![
        format!("[approval] {action}: {summary}"),
        format!("           workspace: {workspace}"),
        format!("           scope: {scope} (request {request_id})"),
        "           answer y to run it once, or n to refuse".to_owned(),
    ]
}

#[must_use]
pub fn run_line(label: &str) -> String {
    format!("[run] {label}")
}

/// Short tail of a contract id, so the transcript stays readable.
#[must_use]
pub fn short_id(id: &str) -> String {
    let total = id.chars().count();
    let tail: String = id.chars().skip(total.saturating_sub(8)).collect();
    format!("...{tail}")
}

#[cfg(test)]
mod tests {
    use super::{help_lines, prompt_line, prompt_prefix, run_line, short_id, tool_line};
    use crate::interactive::events::AppPhase;

    #[test]
    fn h03_prompt_marks_a_busy_phase_and_stays_readable() {
        assert_eq!(prompt_prefix(AppPhase::Ready), "> ");
        assert_eq!(prompt_prefix(AppPhase::SetupRequired), "> ");
        assert_eq!(prompt_prefix(AppPhase::Running), ".. ");
        assert_eq!(prompt_prefix(AppPhase::Canceling), ".. ");
        assert_eq!(prompt_line(AppPhase::Ready, "sửa lỗi"), "> sửa lỗi");
    }

    #[test]
    fn h03_help_lists_the_commands_the_plan_requires() {
        let help = help_lines().join("\n");
        for command in [
            "/help", "/status", "/new", "/model", "/config", "/resume", "/exit",
        ] {
            assert!(help.contains(command), "missing {command} in {help}");
        }
        assert!(help.contains("Ctrl-C"));
        assert!(help.contains("Ctrl-D"));
    }

    #[test]
    fn h03_tool_and_run_lines_and_short_ids_are_stable() {
        assert_eq!(
            tool_line("read_file", "path=a.rs"),
            "[tool] read_file path=a.rs"
        );
        assert_eq!(tool_line("read_file", ""), "[tool] read_file");
        assert_eq!(run_line("done"), "[run] done");
        assert_eq!(
            short_id("input_0192f0aa-bbcc-7ddd-8eee-ffff00001111"),
            "...00001111"
        );
        assert_eq!(short_id("tiny"), "...tiny");
    }
}
