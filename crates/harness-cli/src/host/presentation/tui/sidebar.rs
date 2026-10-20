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
//!  1 ⣾ main         working  │  on the selected background: in front
//!  2 ✓ verifier        done  │
//!      12 tools · read_file  │
//!                            │
//!  other agents            1 │
//!  3 ○ fix-tests       idle  │
//!                            │
//!  ← agents view    alt+1-9  │
//! ```
//!
//! Every agent row carries the number Alt reaches it with: the session's own
//! closes a delegate's pane, a delegate's opens it ([`super::pane`]), and the
//! project's other agents (read by [`super::roster`]) switch the terminal.
//! A click on a row does the same.
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
    #[must_use]
    pub const fn word(self) -> &'static str {
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

    #[must_use]
    pub const fn style(self, theme: &Theme) -> Style {
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

/// What a numbered row of the sidebar opens (Alt+its number, or a click).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    /// The session's own conversation: closes a delegate's pane.
    Main,
    /// A delegate of the run: opens its pane beside the conversation.
    Child(String),
    /// Another agent of the project: the terminal switches to it.
    Agent(String),
}

/// What the sidebar shows beyond the session's own state: the project's other
/// agents and the delegate whose pane is open.
#[derive(Clone, Copy, Debug, Default)]
pub struct Extra<'a> {
    pub others: &'a [super::roster::Entry],
    pub focus: Option<&'a str>,
}

/// The rows Alt+1..9 reach, in their order: the session, its delegates, then
/// the project's other agents.
#[must_use]
pub fn targets(state: &UiState, others: &[super::roster::Entry]) -> Vec<Target> {
    std::iter::once(Target::Main)
        .chain(
            state
                .child_activity
                .iter()
                .map(|activity| Target::Child(activity.name.clone())),
        )
        .chain(others.iter().map(|entry| Target::Agent(entry.id.clone())))
        .collect()
}

/// Draw the sidebar into `area`: its rows, then the divider, then a blank
/// column. Returns the screen row of every agent row and what it opens.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    state: &UiState,
    extra: Extra<'_>,
    theme: &Theme,
) -> Vec<(u16, Target)> {
    if area.width < 3 || area.height == 0 {
        return Vec::new();
    }
    let inner = area.width - 2;
    let (mut rows, hits) = lines(
        state,
        extra,
        theme,
        usize::from(inner),
        usize::from(area.height),
    );
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
    hits.into_iter()
        .filter(|(row, _)| *row < usize::from(area.height))
        .map(|(row, target)| (area.y + u16::try_from(row).unwrap_or(u16::MAX), target))
        .collect()
}

/// The sidebar's rows, `width` cells each, at most `height` of them, and the
/// row of every agent with what it opens.
#[allow(clippy::too_many_lines, reason = "one panel, top to bottom")]
fn lines(
    state: &UiState,
    extra: Extra<'_>,
    theme: &Theme,
    width: usize,
    height: usize,
) -> (Vec<Line<'static>>, Vec<(usize, Target)>) {
    let header = |text: &str, count: usize| -> Line<'static> {
        let label = count.to_string();
        Line::from(vec![
            Span::styled(format!(" {text}"), theme.dim.add_modifier(Modifier::BOLD)),
            Span::raw(" ".repeat(width.saturating_sub(1 + display_width(text) + label.len() + 1))),
            Span::styled(label, theme.dim),
        ])
    };
    let main = main_status(state);
    let mut rows = vec![Line::from(Span::styled(
        " workspace",
        theme.dim.add_modifier(Modifier::BOLD),
    ))];
    let mut hits = Vec::new();

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

    // The agents: the session's own, then each agent it delegated to. Each
    // row carries the number Alt reaches it with; the one whose conversation
    // is in front is on the selected row's background.
    rows.push(Line::default());
    rows.push(Line::from(Span::styled("─".repeat(width), theme.border)));
    rows.push(header("agents", 1 + state.child_activity.len()));
    let mut number = 0_usize;
    let mut agent = |status: Status, name: &str, active: bool| -> Line<'static> {
        number += 1;
        let word = status.word();
        let name = truncate(name, width.saturating_sub(7 + word.len() + 1));
        let gap = width.saturating_sub(5 + display_width(&name) + word.len() + 1);
        let line = Line::from(vec![
            Span::styled(
                if number <= 9 {
                    format!(" {number} ")
                } else {
                    "   ".to_owned()
                },
                theme.dim,
            ),
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
        ]);
        if active {
            line.style(theme.selection)
        } else {
            line
        }
    };
    hits.push((rows.len(), Target::Main));
    rows.push(agent(main, "main", extra.focus.is_none()));
    let child = if state.phase.has_active_run() {
        Status::Working
    } else {
        Status::Done
    };
    // Room left above the footer and the other agents: a delegate takes two
    // rows while there is room for what it last did, one row otherwise.
    let footer_rows = 2 + usize::from(!extra.others.is_empty()) * (2 + extra.others.len().min(3));
    for activity in &state.child_activity {
        let left = height.saturating_sub(rows.len() + footer_rows);
        if left == 0 {
            break;
        }
        hits.push((rows.len(), Target::Child(activity.name.clone())));
        rows.push(agent(
            child,
            &activity.name,
            extra.focus == Some(activity.name.as_str()),
        ));
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

    // The project's other agents: a number or a click switches to one.
    if !extra.others.is_empty() && height > rows.len() + 4 {
        rows.push(Line::default());
        rows.push(header("other agents", extra.others.len()));
        for entry in extra.others {
            if height <= rows.len() + 2 {
                break;
            }
            hits.push((rows.len(), Target::Agent(entry.id.clone())));
            rows.push(agent(entry.status, &entry.name, false));
        }
    }

    // The way to every agent, on the last row.
    if height > rows.len() {
        rows.resize(height - 1, Line::default());
        let keys = "alt+1-9 ";
        rows.push(Line::from(vec![
            Span::styled(" ←", theme.accent),
            Span::styled(" agents view", theme.dim),
            Span::raw(" ".repeat(width.saturating_sub(14 + keys.len()))),
            Span::styled(keys, theme.dim),
        ]));
    }
    (rows, hits)
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
    use super::{
        Extra, MIN_CONTENT_COLUMNS, Status, Target, WIDTH, main_status, render, targets, width,
    };
    use crate::interactive::events::{AppPhase, ChildActivity, UiState};
    use crate::interactive::tui::roster::Entry;
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
        screen_with(state, height, Extra::default()).0
    }

    fn screen_with(
        state: &UiState,
        height: u16,
        extra: Extra<'_>,
    ) -> (Vec<String>, Vec<(u16, Target)>) {
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, height)).expect("terminal");
        let mut hits = Vec::new();
        terminal
            .draw(|frame| {
                hits = render(
                    frame,
                    Rect::new(0, 0, WIDTH, height),
                    state,
                    extra,
                    &Theme::plain(),
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let rows = (0..height)
            .map(|row| {
                (0..WIDTH)
                    .map(|col| buffer[(col, row)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect();
        (rows, hits)
    }

    #[test]
    fn every_agent_row_is_numbered_and_its_row_opens_it() {
        let mut running = state(AppPhase::Running);
        running.child_activity = vec![ChildActivity {
            name: "verifier".to_owned(),
            ..ChildActivity::default()
        }];
        let others = vec![Entry {
            id: "a7".to_owned(),
            name: "fix-tests".to_owned(),
            status: Status::Idle,
        }];
        assert_eq!(
            targets(&running, &others),
            vec![
                Target::Main,
                Target::Child("verifier".to_owned()),
                Target::Agent("a7".to_owned()),
            ]
        );
        let (rows, hits) = screen_with(
            &running,
            24,
            Extra {
                others: &others,
                focus: Some("verifier"),
            },
        );
        let text = rows.join("\n");
        let row_of = |needle: &str| {
            u16::try_from(
                rows.iter()
                    .position(|row| row.contains(needle))
                    .expect(needle),
            )
            .expect("row")
        };
        assert!(rows[usize::from(row_of("main"))].contains(" 1 "), "{text}");
        assert!(
            rows[usize::from(row_of("verifier"))].contains(" 2 "),
            "{text}"
        );
        assert!(text.contains("other agents"), "{text}");
        assert!(
            rows[usize::from(row_of("fix-tests"))].contains(" 3 ○ fix-tests"),
            "{text}"
        );
        assert_eq!(
            hits,
            vec![
                (row_of("main"), Target::Main),
                (row_of("verifier"), Target::Child("verifier".to_owned())),
                (row_of("fix-tests"), Target::Agent("a7".to_owned())),
            ]
        );
        assert!(text.contains("alt+1-9"), "{text}");
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
            ..ChildActivity::default()
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
                ..ChildActivity::default()
            })
            .collect();
        let rows = screen(&busy, 12);
        assert_eq!(rows.len(), 12);
        assert!(rows[11].contains("agents view"), "{}", rows.join("\n"));
    }
}
