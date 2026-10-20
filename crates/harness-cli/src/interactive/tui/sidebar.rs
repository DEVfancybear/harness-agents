//! herdr's sidebar, drawn at the left of the fullscreen app: the workspace the
//! session belongs to (its project, branch, model and permission mode) and the
//! agents of the session, each with herdr's status glyph and word - working,
//! blocked, done or idle - so an agent that needs an answer is seen without
//! reading the conversation.
//!
//! ```text
//!  workspace                 │
//!  harness-agents          ● │  the project, with the session's status dot
//!    master                  │  the branch, in mauve
//!    ◆ deepseek-v4 · high    │
//!    ⚙ full-auto             │
//! ───────────────────────────│
//!  agents                  2 │
//!  ⣾ main           working  │
//!  ✓ verifier          done  │
//!      12 tools · read_file  │
//!                            │
//!  ← agents view             │
//! ```
//!
//! It is shown only on a console wide enough to keep the conversation at
//! [`MIN_CONTENT_COLUMNS`] or more beside it, and `terminal.sidebar: false` in
//! `settings.json` turns it off. A pure function of [`UiState`].

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::theme::Theme;
use super::widgets::composer::{char_width, display_width};
use crate::interactive::events::{AppPhase, Modal, UiState};

/// The sidebar's width: its rows, the divider, and one blank column that keeps
/// the conversation's boxes off the divider.
pub const WIDTH: u16 = 29;

/// The conversation keeps at least this many columns beside the sidebar.
pub const MIN_CONTENT_COLUMNS: u16 = 83;

/// How many columns the sidebar takes on a console `columns` wide.
#[must_use]
pub const fn width(columns: u16, enabled: bool) -> u16 {
    if enabled && columns >= WIDTH + MIN_CONTENT_COLUMNS {
        WIDTH
    } else {
        0
    }
}

/// herdr's agent states.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Working,
    Blocked,
    Done,
    Idle,
}

impl Status {
    const fn word(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Idle => "idle",
        }
    }

    fn glyph(self, tick: u64) -> &'static str {
        match self {
            Self::Working => Theme::spinner(tick),
            Self::Blocked => "◉",
            Self::Done => "✓",
            Self::Idle => "○",
        }
    }

    fn style(self, theme: &Theme) -> Style {
        match self {
            Self::Working => theme.warning,
            Self::Blocked => theme.error,
            Self::Done => theme.tool_ok,
            Self::Idle => theme.dim,
        }
    }
}

/// What the session's own agent is doing.
#[must_use]
pub fn main_status(state: &UiState) -> Status {
    if matches!(
        state.modal,
        Some(Modal::Approval { .. } | Modal::Question { .. } | Modal::McpElicitation { .. })
    ) {
        return Status::Blocked;
    }
    match state.phase {
        AppPhase::Running | AppPhase::Canceling => Status::Working,
        AppPhase::WaitingApproval
        | AppPhase::WaitingInput
        | AppPhase::WaitingMcpInput
        | AppPhase::SetupRequired => Status::Blocked,
        AppPhase::Booting | AppPhase::Ready | AppPhase::Closed => Status::Idle,
    }
}

/// Draw the sidebar into `area`: its rows, then the divider, then a blank column.
pub fn render(frame: &mut Frame, area: Rect, state: &UiState, theme: &Theme) {
    if area.width < 3 || area.height == 0 {
        return;
    }
    let inner = area.width - 2;
    let mut rows = lines(state, theme, usize::from(inner), usize::from(area.height));
    rows.truncate(usize::from(area.height));
    frame.render_widget(
        Paragraph::new(rows),
        Rect::new(area.x, area.y, inner, area.height),
    );
    let divider = vec![Line::from(Span::styled("│", theme.border)); usize::from(area.height)];
    frame.render_widget(
        Paragraph::new(divider),
        Rect::new(area.x + inner, area.y, 1, area.height),
    );
}

/// The sidebar's rows, `width` cells each, at most `height` of them.
#[allow(clippy::too_many_lines, reason = "one panel, top to bottom")]
fn lines(state: &UiState, theme: &Theme, width: usize, height: usize) -> Vec<Line<'static>> {
    let header = |text: &str| -> Line<'static> {
        Line::from(Span::styled(
            format!(" {text}"),
            theme.dim.add_modifier(Modifier::BOLD),
        ))
    };
    let main = main_status(state);
    let mut rows = vec![header("workspace")];

    // The workspace: its name on the active row's background, with the dot
    // herdr puts beside a space whose agents are busy or waiting.
    let project = find(state, "Project: ")
        .map(|value| value.split("    Provider:").next().unwrap_or(value))
        .and_then(|path| {
            path.trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .map(str::to_owned)
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "workspace".to_owned());
    let dot = match main {
        Status::Idle => Span::raw(" "),
        status => Span::styled("●", status.style(theme)),
    };
    let name = truncate(&project, width.saturating_sub(4));
    let gap = width.saturating_sub(display_width(&name) + 3);
    rows.push(
        Line::from(vec![
            Span::raw(" "),
            Span::styled(name, theme.strong),
            Span::raw(" ".repeat(gap)),
            dot,
            Span::raw(" "),
        ])
        .style(theme.user_box),
    );
    let detail = |spans: Vec<Span<'static>>| -> Line<'static> {
        let mut all = vec![Span::raw("   ")];
        all.extend(fit(spans, width.saturating_sub(3)));
        Line::from(all)
    };
    if let Some(branch) = find(state, "Branch: ") {
        rows.push(detail(vec![Span::styled(branch.to_owned(), theme.branch)]));
    }
    if let Some(model) = super::widgets::status::short_model(state) {
        let mut spans = vec![
            Span::styled("◆ ", theme.accent),
            Span::styled(model, theme.muted),
        ];
        // The level only beside a model it leaves whole: a cut name says less.
        if let Some(level) = &state.thinking {
            let level = format!(" · {level}");
            let used: usize = spans.iter().map(|span| display_width(&span.content)).sum();
            if used + display_width(&level) <= width.saturating_sub(3) {
                spans.push(Span::styled(level, theme.dim));
            }
        }
        rows.push(detail(spans));
    }
    if let Some(mode) = find(state, "Permissions: ") {
        let style = if mode.contains("full-auto") {
            theme.warning
        } else {
            theme.dim
        };
        rows.push(detail(vec![Span::styled(format!("⚙ {mode}"), style)]));
    }
    // A chat named after its workspace says nothing the row above did not.
    if let Some(chat) = find(state, "Chat: ").filter(|chat| *chat != project) {
        rows.push(detail(vec![Span::styled(format!("✦ {chat}"), theme.muted)]));
    }
    if let Some(goal) = &state.goal {
        rows.push(detail(vec![Span::styled(
            format!("◎ {goal}"),
            theme.accent,
        )]));
    }

    // The agents: the session's own, then each agent it delegated to.
    rows.push(Line::default());
    rows.push(Line::from(Span::styled("─".repeat(width), theme.border)));
    let count = 1 + state.child_activity.len();
    let label = count.to_string();
    rows.push(Line::from(vec![
        Span::styled(" agents", theme.dim.add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(width.saturating_sub(8 + label.len() + 1))),
        Span::styled(label, theme.dim),
    ]));
    let agent = |status: Status, name: &str| -> Line<'static> {
        let word = status.word();
        let name = truncate(name, width.saturating_sub(5 + word.len() + 1));
        let gap = width.saturating_sub(3 + display_width(&name) + word.len() + 1);
        Line::from(vec![
            Span::raw(" "),
            Span::styled(status.glyph(state.tick), status.style(theme)),
            Span::raw(" "),
            Span::styled(
                name,
                if status == Status::Idle {
                    theme.muted
                } else {
                    theme.strong
                },
            ),
            Span::raw(" ".repeat(gap)),
            Span::styled(word, status.style(theme)),
            Span::raw(" "),
        ])
    };
    rows.push(agent(main, "main"));
    let child = if state.phase.has_active_run() {
        Status::Working
    } else {
        Status::Done
    };
    // Room left above the footer: an agent takes two rows while there is room
    // for what it last did, one row otherwise.
    let footer_rows = 2;
    for activity in &state.child_activity {
        let left = height.saturating_sub(rows.len() + footer_rows);
        if left == 0 {
            break;
        }
        rows.push(agent(child, &activity.name));
        if left >= 2 {
            let mut spans = vec![Span::styled(
                format!(
                    "{} tool{}",
                    activity.tool_calls,
                    if activity.tool_calls == 1 { "" } else { "s" }
                ),
                theme.dim,
            )];
            if !activity.latest.is_empty() {
                spans.push(Span::styled(format!(" · {}", activity.latest), theme.dim));
            }
            let mut all = vec![Span::raw("     ")];
            all.extend(fit(spans, width.saturating_sub(6)));
            rows.push(Line::from(all));
        }
    }

    // The way to every agent, on the last row.
    if height > rows.len() {
        rows.resize(height - 1, Line::default());
        rows.push(Line::from(vec![
            Span::styled(" ←", theme.accent),
            Span::styled(" agents view", theme.dim),
        ]));
    }
    rows
}

fn find<'a>(state: &'a UiState, prefix: &str) -> Option<&'a str> {
    state
        .header
        .iter()
        .find_map(|line| line.strip_prefix(prefix))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// `text` cut to `width` cells, an ellipsis standing in for what was cut.
fn truncate(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_owned();
    }
    let mut kept = String::new();
    let mut used = 0;
    for character in text.chars() {
        let cells = char_width(character);
        if used + cells + 1 > width {
            break;
        }
        kept.push(character);
        used += cells;
    }
    kept.push('…');
    kept
}

/// `spans` cut to `width` cells in all.
fn fit(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut used = 0;
    let mut kept = Vec::new();
    for span in spans {
        let cells = display_width(&span.content);
        if used + cells <= width {
            used += cells;
            kept.push(span);
            continue;
        }
        let room = width.saturating_sub(used);
        if room > 0 {
            kept.push(Span::styled(truncate(&span.content, room), span.style));
        }
        break;
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::{MIN_CONTENT_COLUMNS, Status, WIDTH, main_status, render, width};
    use crate::interactive::events::{AppPhase, ChildActivity, UiState};
    use crate::interactive::tui::theme::Theme;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use std::time::Duration;

    fn state(phase: AppPhase) -> UiState {
        UiState {
            phase,
            setup_required: false,
            setup_hint: None,
            header: vec![
                "Project: F:\\work\\harness-agents    Provider: x".to_owned(),
                "Branch: feat/sidebar".to_owned(),
                "Service: deepseek-v4 via api".to_owned(),
                "Permissions: full-auto".to_owned(),
            ],
            buffer: String::new(),
            cursor: 0,
            live_text: String::new(),
            open_tools: Vec::new(),
            live_tool_output: Vec::new(),
            child_activity: Vec::new(),
            activity: None,
            modal: None,
            granted_for_run: false,
            queued_input: false,
            queued_count: 0,
            queued_previews: Vec::new(),
            writing_code: false,
            provider_wait: None,
            last_request: None,
            run_started_at: None,
            last_run_elapsed: Duration::ZERO,
            steps: 0,
            max_steps: 8,
            tool_calls: 0,
            max_tool_calls: 16,
            suggestions: Vec::new(),
            suggestion_selected: 0,
            fallback_reason: None,
            tick: 0,
            detail: crate::interactive::events::Detail::default(),
            thinking: Some("high".to_owned()),
            service_tier: None,
            goal: None,
        }
    }

    fn screen(state: &UiState, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, height)).expect("terminal");
        terminal
            .draw(|frame| {
                render(
                    frame,
                    Rect::new(0, 0, WIDTH, height),
                    state,
                    &Theme::plain(),
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|row| {
                (0..WIDTH)
                    .map(|col| buffer[(col, row)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn it_needs_room_for_the_conversation_beside_it() {
        assert_eq!(width(WIDTH + MIN_CONTENT_COLUMNS, true), WIDTH);
        assert_eq!(width(WIDTH + MIN_CONTENT_COLUMNS - 1, true), 0);
        assert_eq!(width(200, false), 0, "terminal.sidebar: false");
    }

    #[test]
    fn the_session_agent_is_working_blocked_or_idle() {
        assert_eq!(main_status(&state(AppPhase::Running)), Status::Working);
        assert_eq!(
            main_status(&state(AppPhase::WaitingApproval)),
            Status::Blocked
        );
        assert_eq!(main_status(&state(AppPhase::WaitingInput)), Status::Blocked);
        assert_eq!(main_status(&state(AppPhase::Ready)), Status::Idle);
    }

    #[test]
    fn it_names_the_workspace_and_every_agent_with_its_state() {
        let mut running = state(AppPhase::Running);
        running.child_activity = vec![ChildActivity {
            name: "verifier".to_owned(),
            tool_calls: 12,
            latest: "read_file".to_owned(),
        }];
        let rows = screen(&running, 20);
        let text = rows.join("\n");
        assert!(rows[1].contains("harness-agents"), "{text}");
        assert!(text.contains("feat/sidebar"), "{text}");
        assert!(text.contains("◆ deepseek-v4 · high"), "{text}");
        assert!(text.contains("⚙ full-auto"), "{text}");
        assert!(
            rows.iter()
                .any(|row| row.contains("main") && row.contains("working")),
            "{text}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("verifier") && row.contains("working")),
            "{text}"
        );
        assert!(text.contains("12 tools · read_file"), "{text}");
        assert!(rows[19].contains("agents view"), "the footer: {text}");
        assert!(
            rows.iter().all(|row| row.trim_end().ends_with('│')),
            "the divider runs the full height: {text}"
        );

        // Once the run ends its delegates are done and the session is idle.
        running.phase = AppPhase::Ready;
        let text = screen(&running, 20).join("\n");
        assert!(text.contains("✓ verifier"), "{text}");
        assert!(text.contains("○ main"), "{text}");
    }

    #[test]
    fn a_short_sidebar_keeps_its_rows_inside_the_area() {
        let mut busy = state(AppPhase::Running);
        busy.child_activity = (0..10)
            .map(|index| ChildActivity {
                name: format!("coder-{index}"),
                tool_calls: index,
                latest: String::new(),
            })
            .collect();
        let rows = screen(&busy, 12);
        assert_eq!(rows.len(), 12);
        assert!(rows[11].contains("agents view"), "{}", rows.join("\n"));
    }
}
