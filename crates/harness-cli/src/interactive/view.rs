//! Rendering helpers: pure string production with no terminal state.

use std::time::Duration;

use super::events::{AppPhase, HistoryItem, ToolState};
use super::input::SLASH_COMMANDS;

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
        // An automatic continuation is marked as the app's own line: a reader must be
        // able to tell what they asked for from what the app said on their behalf.
        HistoryItem::Automatic { text } => vec![format!("[auto] {text}")],
        HistoryItem::Assistant { text } | HistoryItem::Message { text } => vec![text.clone()],
        HistoryItem::Thinking { .. } | HistoryItem::ToolOutput { .. } => Vec::new(),
        HistoryItem::Tool {
            name,
            summary,
            state,
        } => match state {
            ToolState::Started => vec![tool_line(name, summary)],
            ToolState::Ok { .. } => vec![tool_line(name, "ok")],
            ToolState::Failed { detail, .. } if detail.trim().is_empty() => {
                vec![tool_line(name, "failed")]
            }
            // The reason travels with the row: a bare `failed` told the reader nothing
            // about whether the call was malformed, denied or stale.
            ToolState::Failed { detail, .. } => vec![tool_line(name, &format!("failed: {detail}"))],
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

/// The `/help` page, built from the same table the suggestion menu draws.
///
/// One row is added by hand: `/skill:<name>`, which the menu lists skill by skill.
/// A command added to `SLASH_COMMANDS` appears here without anyone remembering to
/// write a second row, which is the point: the list a user sees while typing and
/// the list `/help` prints cannot drift apart.
#[must_use]
pub fn help_lines() -> Vec<String> {
    let mut lines = Vec::with_capacity(SLASH_COMMANDS.len() + 2);
    for command in SLASH_COMMANDS {
        let mut summary = command.summary.to_owned();
        if !command.aliases.is_empty() {
            summary = format!("{summary} (also {})", command.aliases.join(", "));
        }
        lines.push(usage_line(&command.usage(), &summary));
    }
    lines.push(usage_line(
        "/skill:<name> [task]",
        "Run a skill; typing /skill lists them",
    ));
    lines.push("Keys: /hotkeys".to_owned());
    lines
}

/// The `/help` card the TUI shows: every command, grouped, on one screen.
///
/// The full table does not fit the inline viewport, so the card lists every
/// command by name in its group; the suggestion menu shows what each one does
/// while typing `/`, and `/help all` opens the full table.
#[must_use]
pub fn help_card_lines() -> Vec<String> {
    let groups: [(&str, &[&str]); 5] = [
        (
            "Session",
            &[
                "/new", "/resume", "/name", "/session", "/compact", "/refine", "/context", "/copy",
                "/export", "/undo", "/diff",
            ],
        ),
        (
            "Model",
            &[
                "/model",
                "/effort",
                "/login",
                "/logout",
                "/cost",
                "/system-prompt",
            ],
        ),
        (
            "Run",
            &[
                "/goal",
                "/steer",
                "/mode",
                "/permissions",
                "/agents",
                "/more",
            ],
        ),
        (
            "Tools",
            &[
                "/skills",
                "/skill:<name>",
                "/mcp",
                "/hooks",
                "/reload",
                "/config",
                "/trust",
                "/init",
            ],
        ),
        (
            "Input",
            &[
                "/help",
                "/image",
                "/attach <path>",
                "@file",
                "!command",
                "/hotkeys",
                "/quit",
            ],
        ),
    ];
    let mut lines = groups
        .iter()
        .map(|(group, items)| format!("{group:<9}{}", items.join("  ")))
        .collect::<Vec<_>>();
    lines.push("Gõ / để xem mô tả từng lệnh · /help all để xem bảng đầy đủ".to_owned());
    lines
}

/// prime-agent's `/hotkeys`: every key the prompt understands.
#[must_use]
pub fn hotkey_lines() -> Vec<String> {
    [
        ("Enter", "send; in the / menu, pick the highlighted row"),
        ("Tab", "complete the highlighted command or argument"),
        ("Ctrl-J / Alt+Enter", "new line"),
        ("↑ ↓", "move in a menu, or recall earlier prompts"),
        ("Esc", "close a panel or menu; interrupt a run"),
        ("Ctrl-C", "cancel the run; twice on an empty prompt quits"),
        ("Ctrl-D", "quit on an empty prompt"),
        ("Ctrl-O", "cycle collapsed / details / expanded"),
        ("Ctrl-L", "redraw"),
        ("Ctrl-U / Ctrl-W", "erase to line start / erase a word"),
        ("Ctrl-V", "paste an image or path from the clipboard"),
        ("PgUp / PgDn", "scroll a panel"),
        ("@", "pick a file to mention"),
        ("!cmd / !!cmd", "run a shell command / only show its output"),
    ]
    .iter()
    .map(|(key, what)| usage_line(key, what))
    .collect()
}

/// One help row: the command in a fixed column, then what it does.
fn usage_line(usage: &str, summary: &str) -> String {
    format!("{usage:<HELP_COLUMN$}{summary}")
}

/// Cells the command column of `/help` occupies.
const HELP_COLUMN: usize = 17;

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
    let (summary, diff) = summary
        .split_once("\n[diff]\n")
        .map_or((summary, None), |(summary, diff)| (summary, Some(diff)));
    let mut lines = vec![
        format!("[approval] {action}: {summary}"),
        format!("           workspace: {workspace}"),
        format!("           scope: {scope} (request {request_id})"),
        "           answer y to run it once, a to allow every action for this turn, or n to refuse"
            .to_owned(),
    ];
    if let Some(diff) = diff {
        lines.push("           [diff]".to_owned());
        lines.extend(diff.lines().map(|line| format!("           {line}")));
    }
    lines
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
        cursor_cell, help_lines, plain_lines, prompt_line, prompt_lines, prompt_prefix, run_line,
        short_id, tool_line,
    };
    use crate::interactive::events::{AppPhase, HistoryItem, PauseReason, RunOutcome, ToolState};
    use crate::interactive::input::SLASH_COMMANDS;
    use std::time::Duration;

    #[test]
    fn g03_thinking_delta_never_enters_plain_transcript() {
        let item = HistoryItem::Thinking {
            text: "private reasoning".to_owned(),
        };
        assert!(plain_lines(&item).is_empty());
    }

    /// A bound is a pause, not a break: the measured turn printed
    /// `[run] failed: step limit reached · 8 steps · 8 tool calls`, which reads as if the
    /// work had been lost. It had not — every receipt was durable and the task continues.
    #[test]
    fn a_bounded_run_says_paused_and_a_break_still_says_failed() {
        let run = |outcome| HistoryItem::Run {
            outcome,
            steps: 8,
            tool_calls: 8,
            elapsed: Duration::from_millis(18_300),
        };
        assert_eq!(
            plain_lines(&run(RunOutcome::Paused(PauseReason::StepLimit))),
            vec!["[run] paused: step limit reached".to_owned()]
        );
        assert_eq!(
            plain_lines(&run(RunOutcome::Paused(PauseReason::Deadline))),
            vec!["[run] paused: deadline reached".to_owned()]
        );
        assert_eq!(
            plain_lines(&run(RunOutcome::Failed("provider unreachable".to_owned()))),
            vec!["[run] failed: provider unreachable".to_owned()]
        );
        assert_eq!(
            plain_lines(&run(RunOutcome::Done)),
            vec!["[run] done".to_owned()]
        );
    }

    /// A continuation is the app's line, not the user's: `> continue` would read as
    /// something the person typed.
    #[test]
    fn an_automatic_continuation_is_marked_as_the_app_speaking() {
        let item = HistoryItem::Automatic {
            text: "continue: the previous turn stopped at a bound".to_owned(),
        };
        assert_eq!(
            plain_lines(&item),
            vec!["[auto] continue: the previous turn stopped at a bound".to_owned()]
        );
    }

    /// The measured gap: the transcript said `[tool] list_files {"path": ""} failed` and
    /// nothing else, so a reader could not tell a malformed call from a policy denial.
    #[test]
    fn a_failed_tool_row_carries_the_reason_it_failed() {
        let failed = HistoryItem::Tool {
            name: "list_files".to_owned(),
            summary: "path=".to_owned(),
            state: ToolState::Failed {
                elapsed: Duration::from_millis(962),
                detail: "invalid_payload: optional tool path must not be blank".to_owned(),
            },
        };
        assert_eq!(
            plain_lines(&failed),
            vec![
                "[tool] list_files failed: invalid_payload: optional tool path must not be blank"
                    .to_owned()
            ]
        );

        // A producer that reports no reason keeps the old two-word line.
        let bare = HistoryItem::Tool {
            name: "list_files".to_owned(),
            summary: String::new(),
            state: ToolState::Failed {
                elapsed: Duration::from_millis(962),
                detail: String::new(),
            },
        };
        assert_eq!(
            plain_lines(&bare),
            vec!["[tool] list_files failed".to_owned()]
        );
    }

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
            "/help", "/session", "/status", "/new", "/model", "/login", "/logout", "/config",
            "/resume", "/quit", "/exit",
        ] {
            assert!(help.contains(command), "missing {command} in {help}");
        }
        let keys = super::hotkey_lines().join("\n");
        assert!(keys.contains("Ctrl-C"), "{keys}");
        assert!(keys.contains("Ctrl-D"), "{keys}");
        assert!(keys.contains("Ctrl-O"), "{keys}");
        assert!(
            help.contains("/help            List every command"),
            "the reference page keeps its column layout: {help}"
        );
    }

    /// `/help` is generated from the same table the suggestion menu draws, so the
    /// two cannot drift: a command added to the table is on the page, and the page
    /// names no command the menu would not offer.
    #[test]
    fn slash_the_help_page_and_the_menu_read_one_table() {
        let help = help_lines();
        let text = help.join("\n");
        for command in SLASH_COMMANDS {
            assert!(
                help.iter()
                    .any(|line| line.starts_with(&format!("{:<17}", command.usage()))
                        && line.contains(command.summary)
                        && command.aliases.iter().all(|alias| line.contains(alias))),
                "{} is offered by the menu but missing from /help: {text}",
                command.name
            );
        }

        // The one row written by hand: skills, which the menu lists by name.
        assert!(
            help.iter().any(|line| line.starts_with("/skill:<name>")),
            "{text}"
        );

        // Nothing else names a command: the footer is the only row that is not one.
        for line in &help {
            let first = line.split_whitespace().next().unwrap_or_default();
            if first.starts_with("/skill:") || first == "Keys:" {
                continue;
            }
            assert!(
                SLASH_COMMANDS.iter().any(|command| command.name == first),
                "the page names a command the table does not have: {line}"
            );
        }
        assert_eq!(
            help.len(),
            SLASH_COMMANDS.len() + 2,
            "one row per command, the skill row, and the footer"
        );
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
