//! Rendering helpers: pure string production with no terminal state.

use std::time::Duration;

use super::events::{AppPhase, HistoryItem, ToolState};

/// The plain lines one history item must produce.
///
/// This is the compatibility contract of the T02 refactor: for the same scripted
/// scenario it must return exactly the strings the pre-T02 controller pushed with
/// `Effect::WriteLine`/`WritePartial`, so the plain transcript stays
/// byte-identical (acceptance U20) and every existing assertion keeps its
/// meaning. Each arm therefore calls the very same helper the old code called.
#[must_use]
pub fn plain_lines(item: &HistoryItem) -> Vec<String> {
    match item {
        HistoryItem::Banner { lines } | HistoryItem::Sessions { lines } => lines.clone(),
        HistoryItem::User { text } => vec![format!("> {text}")],
        HistoryItem::Assistant { text } | HistoryItem::Message { text } => vec![text.clone()],
        HistoryItem::Tool {
            name,
            summary,
            state,
        } => match state {
            ToolState::Started => vec![tool_line(name, summary)],
            ToolState::Ok { .. } => vec![tool_line(name, "ok")],
            ToolState::Failed { .. } => vec![tool_line(name, "failed")],
        },
        HistoryItem::Run { outcome, .. } => vec![run_line(&outcome.label())],
        HistoryItem::RunAccepted { input_id } => {
            vec![run_line(&format!("accepted {}", short_id(input_id)))]
        }
        HistoryItem::Error { message } => vec![format!("[error] {message}")],
        HistoryItem::Notice { message } => vec![format!("[info] {message}")],
        HistoryItem::Approval {
            action,
            summary,
            workspace,
            scope,
            request_id,
        } => approval_lines(action, summary, workspace, scope, request_id),
        HistoryItem::ApprovalResolution { label, request_id } => {
            vec![format!("[approval] {label} {request_id}")]
        }
    }
}

/// A short duration label: milliseconds under a second, one decimal above it.
#[must_use]
pub fn seconds_label(elapsed: Duration) -> String {
    let millis = elapsed.as_millis();
    if millis < 1000 {
        format!("{millis}ms")
    } else {
        format!("{:.1}s", elapsed.as_secs_f64())
    }
}

/// `mm:ss` for the status bar clock.
#[must_use]
pub fn clock_label(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

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

/// The prompt as the rows a terminal must draw.
///
/// A buffer containing line breaks is one message, so it is drawn as one prompt
/// spanning several rows: the marker only precedes the first row, and every later
/// row is plain continuation text. Returning rows instead of one string keeps the
/// host responsible for terminals, and keeps this function pure.
#[must_use]
pub fn prompt_lines(phase: AppPhase, buffer: &str) -> Vec<String> {
    let prefix = prompt_prefix(phase);
    let mut lines: Vec<String> = Vec::new();
    for (index, segment) in buffer.split('\n').enumerate() {
        if index == 0 {
            lines.push(format!("{prefix}{segment}"));
        } else {
            lines.push(segment.to_owned());
        }
    }
    lines
}

/// Where the terminal cursor belongs inside a multi-line prompt.
///
/// `cursor` counts characters from the start of the buffer, never bytes, so a
/// Vietnamese character moves the cursor by one cell. The column of the first row
/// accounts for the prompt marker; continuation rows start at column zero.
#[must_use]
pub fn cursor_cell(phase: AppPhase, buffer: &str, cursor: usize) -> (usize, usize) {
    let prefix_width = prompt_prefix(phase).chars().count();
    let mut row = 0;
    let mut column = prefix_width;
    for (index, character) in buffer.chars().enumerate() {
        if index == cursor {
            break;
        }
        if character == '\n' {
            row += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    (row, column)
}

#[must_use]
pub fn help_lines() -> Vec<String> {
    vec![
        "/help            list these commands".to_owned(),
        "/status          show project, config, data and provider state".to_owned(),
        "/key             save the provider API key; the value is masked and never kept in history"
            .to_owned(),
        "/key <value>     save it in one line: less private because the value is visible while \
         typed; it is removed from recall history afterwards"
            .to_owned(),
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
    use super::{
        cursor_cell, help_lines, prompt_line, prompt_lines, prompt_prefix, run_line, short_id,
        tool_line,
    };
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
    fn h03_a_multiline_draft_is_one_prompt_with_rows_and_a_cursor_cell() {
        let buffer = "first line\nsecond\nthird";
        assert_eq!(
            prompt_lines(AppPhase::Ready, buffer),
            vec![
                "> first line".to_owned(),
                "second".to_owned(),
                "third".to_owned()
            ],
            "only the first row carries the marker"
        );

        // The marker is two cells wide, so cursor 0 sits after "> ".
        assert_eq!(cursor_cell(AppPhase::Ready, buffer, 0), (0, 2));
        assert_eq!(cursor_cell(AppPhase::Ready, buffer, 5), (0, 7));
        // The break itself belongs to the end of the first row.
        assert_eq!(cursor_cell(AppPhase::Ready, buffer, 10), (0, 12));
        // The first character after the break starts row 1 at column 0.
        assert_eq!(cursor_cell(AppPhase::Ready, buffer, 11), (1, 0));
        assert_eq!(cursor_cell(AppPhase::Ready, buffer, 17), (1, 6));
        assert_eq!(cursor_cell(AppPhase::Ready, buffer, 18), (2, 0));
        assert_eq!(cursor_cell(AppPhase::Ready, buffer, 23), (2, 5));

        // The busy marker is ".. " (three cells) while the ready marker is "> "
        // (two), so the cursor column follows the marker actually in use.
        assert_eq!(cursor_cell(AppPhase::Running, "x", 0), (0, 3));
        assert_eq!(prompt_prefix(AppPhase::Running).chars().count(), 3);
        assert_eq!(prompt_prefix(AppPhase::Ready).chars().count(), 2);

        // A single-line prompt stays exactly as it was.
        assert_eq!(prompt_lines(AppPhase::Ready, ""), vec!["> ".to_owned()]);
        assert_eq!(
            prompt_lines(AppPhase::Ready, "one line"),
            vec!["> one line".to_owned()]
        );
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
