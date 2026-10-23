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
//!    ◈ full-auto             │
//! ───────────────────────────│
//!  agents        2 working · 3 │
//!  1 ⣾ main         working  │  on the selected background: in front
//!  2 ⣾ coder-c0cc21d working │  each child by its own name, working first
//!      12 tools · read_file  │
//!  3 ✓ explorer-d3cc6  done  │
//!                            │
//!  other agents            1 │
//!  3 ○ fix-tests       idle  │
//!                            │
//!  ← agents view    alt+1-9  │
//! ```
//!
//! A child's state is the session's child registry's - what the agents view
//! lists - and the session's own agent is working while children it started
//! still are, even after its run ended.
//!
//! Every agent row carries the number Alt reaches it with: the session's own
//! closes a delegate's pane, a delegate's opens it ([`super::pane`]), and the
//! project's other agents (read by [`super::roster`]) switch the terminal.
//! A click on a row does the same.
//!
//! It is shown only on a console wide enough to keep the conversation at
//! [`MIN_CONTENT_COLUMNS`] or more beside it, and `terminal.sidebar: false` in
//! `settings.json` turns it off. A pure function of [`UiState`].

use std::fmt::Write as _;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::theme::Theme;
use super::widgets::composer::{char_width, display_width};
use crate::interactive::events::{AppPhase, ChildActivity, ChildRunState, Modal, UiState};

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

/// herdr's agent states, and how a delegated child ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Working,
    /// Waiting for a place to run under `[agents] max_workers`: it has not
    /// made a call yet, so it is not drawn working.
    Queued,
    Blocked,
    Done,
    Failed,
    Stopped,
    Idle,
}

impl Status {
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Queued => "queued",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::Idle => "idle",
        }
    }

    fn glyph(self, tick: u64) -> &'static str {
        match self {
            Self::Working => Theme::spinner(tick),
            Self::Queued => "◌",
            Self::Blocked => "◉",
            Self::Done => "✓",
            Self::Failed => "✗",
            Self::Stopped => "■",
            Self::Idle => "○",
        }
    }

    #[must_use]
    pub const fn style(self, theme: &Theme) -> Style {
        match self {
            Self::Working => theme.warning,
            Self::Blocked | Self::Failed => theme.error,
            Self::Done => theme.tool_ok,
            Self::Queued | Self::Stopped | Self::Idle => theme.dim,
        }
    }

    /// Where a row sorts: what needs looking at first.
    const fn rank(self) -> u8 {
        match self {
            Self::Working => 0,
            Self::Blocked | Self::Queued => 1,
            Self::Failed => 2,
            Self::Done | Self::Stopped | Self::Idle => 3,
        }
    }
}

/// What the session's own agent is doing. A run that ended while children it
/// started still work is waiting on them: working, as the agents view says.
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
        AppPhase::Booting | AppPhase::Ready | AppPhase::Closed
            if state
                .child_activity
                .iter()
                .any(|child| child.state == ChildRunState::Running) =>
        {
            Status::Working
        }
        AppPhase::Booting | AppPhase::Ready | AppPhase::Closed => Status::Idle,
    }
}

/// A delegated child's state: the registry's, or - for a child it never
/// listed - working while the run is, done after.
#[must_use]
pub fn child_status(state: &UiState, child: &ChildActivity) -> Status {
    match child.state {
        ChildRunState::Running if child.queued => Status::Queued,
        ChildRunState::Running => Status::Working,
        ChildRunState::Failed => Status::Failed,
        ChildRunState::Stopped => Status::Stopped,
        ChildRunState::Unknown if state.phase.has_active_run() => Status::Working,
        ChildRunState::Done | ChildRunState::Unknown => Status::Done,
    }
}

/// The run's children in the sidebar's order, each with its state: working
/// first, then failed, then the rest, each group in the order they started.
#[must_use]
pub fn ordered_children(state: &UiState) -> Vec<(&ChildActivity, Status)> {
    let mut children = state
        .child_activity
        .iter()
        .map(|child| (child, child_status(state, child)))
        .collect::<Vec<_>>();
    children.sort_by_key(|(_, status)| status.rank());
    children
}

/// A child's name in `room` cells. A generated name (`coder-c0cc21d5`) keeps
/// its role and gives up the end of its id first, so `coder-c0cc` still
/// tells two coders apart.
fn child_label(name: &str, room: usize) -> String {
    if display_width(name) <= room {
        return name.to_owned();
    }
    if let Some((role, id)) = name.rsplit_once('-')
        && id.len() >= 4
        && id.chars().all(|c| c.is_ascii_hexdigit())
    {
        let keep = room.saturating_sub(display_width(role) + 1).min(id.len());
        if keep >= 4 {
            return format!("{role}-{}", &id[..keep]);
        }
    }
    truncate(name, room)
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
    /// The spinner frame of the other agents' rows, from the screen's own
    /// clock: the session's tick stands still while it is idle, and a busy
    /// agent elsewhere showed one frozen frame.
    pub spin: u64,
}

/// The rows Alt+1..9 reach, in their order: the session, its delegates, then
/// the project's other agents.
#[must_use]
pub fn targets(state: &UiState, others: &[super::roster::Entry]) -> Vec<Target> {
    std::iter::once(Target::Main)
        .chain(
            ordered_children(state)
                .into_iter()
                .map(|(activity, _)| Target::Child(activity.name.clone())),
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
    let header = |text: &str, label: &str| -> Line<'static> {
        let label = label.to_owned();
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
    let here = place(find(state, "Branch: "), find(state, "Worktree: "));
    if !here.is_empty() {
        rows.push(detail(vec![Span::styled(here, theme.branch)]));
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
        rows.push(detail(vec![Span::styled(format!("◈ {mode}"), style)]));
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
    let children = ordered_children(state);
    let working = std::iter::once(main)
        .chain(children.iter().map(|(_, status)| *status))
        .filter(|status| *status == Status::Working)
        .count();
    let total = 1 + children.len();
    // How many work, beside how many there are: four coders running among
    // nine children is what the agents view says too.
    let count = if working > 0 && working < total {
        format!("{working} working · {total}")
    } else {
        total.to_string()
    };
    rows.push(header("agents", &count));
    let agent =
        |number: usize, status: Status, name: &str, active: bool, tick: u64| -> Line<'static> {
            let word = status.word();
            let name = child_label(name, width.saturating_sub(5 + word.len() + 2));
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
                Span::styled(status.glyph(tick), status.style(theme)),
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
    // herdr's agent row is two lines: the name, then its state in its colour
    // (and here the model and thinking level it runs on). Where the agents do
    // not all fit that way, each is one line with its state at the right.
    let footer_rows = 2 + usize::from(!extra.others.is_empty()) * (2 + extra.others.len().min(3));
    let room = height.saturating_sub(rows.len() + footer_rows);
    let two_lines = 2 * (1 + children.len()) <= room;
    // herdr's two lines: the name in front, the state under it.
    let pair = |number: usize, status: Status, name: &str, below: String, active: bool| {
        let first = Line::from(vec![
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
                truncate(name, width.saturating_sub(6)),
                if status == Status::Idle {
                    theme.muted
                } else {
                    theme.strong
                },
            ),
        ]);
        let mut spans = vec![
            Span::raw("     "),
            Span::styled(status.word(), status.style(theme)),
        ];
        if !below.is_empty() {
            spans.push(Span::styled(format!(" · {below}"), theme.dim));
        }
        let second = Line::from(fit(spans, width.saturating_sub(1)));
        if active {
            [first.style(theme.selection), second.style(theme.selection)]
        } else {
            [first, second]
        }
    };
    hits.push((rows.len(), Target::Main));
    if two_lines {
        rows.extend(pair(1, main, "main", String::new(), extra.focus.is_none()));
    } else {
        rows.push(agent(1, main, "main", extra.focus.is_none(), state.tick));
    }
    // Room left above the footer and the other agents. Every child gets its
    // row before any gets one for what it last did, and the working ones get
    // theirs first; the children that do not fit are counted.
    let room = height.saturating_sub(rows.len() + footer_rows);
    let per_child = if two_lines { 2 } else { 1 };
    let (listed, hidden) = if children.len() * per_child > room {
        let listed = room.saturating_sub(1) / per_child;
        (listed, children.len() - listed)
    } else {
        (children.len(), 0)
    };
    let working_children = children
        .iter()
        .take(listed)
        .filter(|(_, status)| *status == Status::Working)
        .count();
    let used = listed * per_child;
    let details = |status: Status| {
        used + listed <= room || (status == Status::Working && used + working_children <= room)
    };
    for (index, (activity, status)) in children.iter().take(listed).enumerate() {
        hits.push((rows.len(), Target::Child(activity.name.clone())));
        let active = extra.focus == Some(activity.name.as_str());
        let tools = format!(
            "{} tool{}",
            activity.tool_calls,
            if activity.tool_calls == 1 { "" } else { "s" }
        );
        if two_lines {
            rows.extend(pair(index + 2, *status, &activity.name, tools, active));
            // The model and thinking level it runs on: the level is kept
            // whole, the model gives way.
            if details(*status) && !activity.model.is_empty() {
                rows.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled("◆ ", theme.accent),
                    Span::styled(model_effort(activity, width.saturating_sub(8)), theme.muted),
                ]));
            }
            // A coder in a worktree of its own says which, under its model.
            let workplace = place(activity.branch.as_deref(), activity.worktree.as_deref());
            if details(*status) && !workplace.is_empty() {
                let mut all = vec![Span::raw("     ")];
                all.extend(fit(
                    vec![Span::styled(workplace, theme.branch)],
                    width.saturating_sub(6),
                ));
                rows.push(Line::from(all));
            }
            continue;
        }
        rows.push(agent(
            index + 2,
            *status,
            &activity.name,
            active,
            state.tick,
        ));
        if details(*status) {
            let mut spans = vec![Span::styled(tools, theme.dim)];
            if !activity.latest.is_empty() {
                spans.push(Span::styled(format!(" · {}", activity.latest), theme.dim));
            }
            let mut all = vec![Span::raw("     ")];
            all.extend(fit(spans, width.saturating_sub(6)));
            rows.push(Line::from(all));
        }
    }
    if hidden > 0 {
        let working = children
            .iter()
            .skip(listed)
            .filter(|(_, status)| *status == Status::Working)
            .count();
        let mut text = format!("     +{hidden} more");
        if working > 0 {
            let _ = write!(text, " · {working} working");
        }
        rows.push(Line::from(Span::styled(truncate(&text, width), theme.dim)));
    }

    // The other agents the agents view lists: the project's, then those of
    // other folders, each folder named under its agent. A number or a click
    // switches to one; those that do not fit are counted, never left out
    // without a word.
    let fits = |rows: &Vec<Line<'static>>, more: usize| rows.len() + more < height;
    let mut left_out = 0;
    for (title, here) in [("other agents", true), ("other folders", false)] {
        let group = extra
            .others
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.folder.is_none() == here)
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }
        let later = if here {
            extra
                .others
                .iter()
                .filter(|entry| entry.folder.is_some())
                .count()
        } else {
            0
        };
        if left_out > 0 || !fits(&rows, 3) {
            left_out += group.len();
            continue;
        }
        rows.push(Line::default());
        rows.push(header(title, &group.len().to_string()));
        for (at, (index, entry)) in group.iter().enumerate() {
            let after = group.len() - at - 1 + later;
            if !fits(&rows, 1 + usize::from(after > 0)) {
                left_out += group.len() - at;
                break;
            }
            hits.push((rows.len(), Target::Agent(entry.id.clone())));
            rows.push(agent(
                children.len() + 2 + index,
                entry.status,
                &entry.name,
                false,
                extra.spin,
            ));
            if let Some(folder) = &entry.folder
                && fits(&rows, 1 + after)
            {
                rows.push(Line::from(vec![
                    Span::raw("     "),
                    Span::styled(
                        truncate(&format!("⌂ {folder}"), width.saturating_sub(6)),
                        theme.dim,
                    ),
                ]));
            }
        }
    }
    if left_out > 0 && fits(&rows, 1) {
        rows.push(Line::from(Span::styled(
            truncate(&format!("     +{left_out} more · /agents"), width),
            theme.dim,
        )));
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

/// Where an agent works, as every view writes it: `⎇ branch`, and the
/// worktree's folder after it when the agent works in a worktree of its own
/// (`⎇ feat/x · ha-078`). Empty when neither is known.
#[must_use]
pub fn place(branch: Option<&str>, worktree: Option<&str>) -> String {
    match (branch, worktree) {
        (Some(branch), Some(tree)) => format!("⎇ {branch} · {tree}"),
        (Some(branch), None) => format!("⎇ {branch}"),
        (None, Some(tree)) => format!("⎇ {tree}"),
        (None, None) => String::new(),
    }
}

/// A child's model and thinking level in `room` cells: the level whole, the
/// model cut before it.
pub fn model_effort(child: &ChildActivity, room: usize) -> String {
    let label = child.model_label();
    if display_width(&label) <= room {
        return label;
    }
    match label.rsplit_once(' ') {
        Some((model, level)) if child.thinking.as_deref() == Some(level) => {
            let model = truncate(model, room.saturating_sub(display_width(level) + 1));
            format!("{model} {level}")
        }
        _ => truncate(&label, room),
    }
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
        Extra, MIN_CONTENT_COLUMNS, Status, Target, WIDTH, main_status, ordered_children, render,
        targets, width,
    };
    use crate::interactive::agents::protocol::SubagentInfo;
    use crate::interactive::events::{AppPhase, ChildActivity, UiState, sync_child_states};
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

    /// Each row with the one under it: herdr's agent row is two lines, the
    /// name and then the state.
    fn agent_rows(rows: &[String]) -> Vec<String> {
        rows.windows(2)
            .filter(|pair| {
                pair[0].starts_with(' ')
                    && pair[0].chars().nth(1).is_some_and(|c| c.is_ascii_digit())
            })
            .map(|pair| format!("{} {}", pair[0], pair[1]))
            .collect()
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
            folder: None,
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
                spin: 0,
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
        assert!(text.contains("◈ full-auto"), "{text}");
        assert!(
            agent_rows(&rows)
                .iter()
                .any(|row| row.contains("main") && row.contains("working")),
            "{text}"
        );
        assert!(
            agent_rows(&rows)
                .iter()
                .any(|row| row.contains("verifier") && row.contains("working")),
            "{text}"
        );
        assert!(text.contains("working · 12 tools"), "{text}");
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

    /// The children of the session the bug was seen in: the agents view had
    /// the conversation running with "9 subagents (4 running)" while the
    /// sidebar said `1 main idle`, `2 verifier done`, `3 coder done` - one row
    /// per role, each with the state of an older child.
    fn nine_children() -> Vec<SubagentInfo> {
        [
            ("verifier-b105bf50", "running"),
            ("explorer-d3cc6099", "completed"),
            ("explorer-8a0ec09b", "completed"),
            ("coder-7ce2ed20", "completed"),
            ("coder-10da2461", "completed"),
            ("coder-c0cc21d5", "running"),
            ("coder-6bcf5921", "completed"),
            ("coder-243a54dc", "running"),
            ("coder-64d1e8a3", "running"),
        ]
        .into_iter()
        .map(|(name, status)| SubagentInfo {
            branch: None,
            worktree: None,
            queued: false,
            name: name.to_owned(),
            status: status.to_owned(),
            model: "opencode-go/deepseek-v4.1-flash".to_owned(),
            thinking: Some("high".to_owned()),
            depth: 1,
            program: String::new(),
        })
        .collect()
    }

    #[test]
    fn nine_children_are_listed_each_with_its_own_state_running_first() {
        // The run ended; its children still work. The pane lines named some
        // of them (in the order they started), the registry lists them all.
        let mut waiting = state(AppPhase::Ready);
        waiting.child_activity = [
            "explorer-d3cc6099",
            "verifier-b105bf50",
            "coder-7ce2ed20",
            "coder-c0cc21d5",
        ]
        .into_iter()
        .map(|name| ChildActivity {
            name: name.to_owned(),
            tool_calls: 3,
            ..ChildActivity::default()
        })
        .collect();
        assert!(sync_child_states(
            &mut waiting.child_activity,
            &nine_children()
        ));
        assert!(
            !sync_child_states(&mut waiting.child_activity, &nine_children()),
            "nothing changed the second time"
        );
        assert_eq!(
            waiting.child_activity.len(),
            6,
            "a running child that sent nothing yet is listed; a finished one that never showed is not"
        );
        assert_eq!(
            main_status(&waiting),
            Status::Working,
            "main waits on its running children"
        );
        assert_eq!(
            targets(&waiting, &[]),
            vec![
                Target::Main,
                Target::Child("verifier-b105bf50".to_owned()),
                Target::Child("coder-c0cc21d5".to_owned()),
                Target::Child("coder-243a54dc".to_owned()),
                Target::Child("coder-64d1e8a3".to_owned()),
                Target::Child("explorer-d3cc6099".to_owned()),
                Target::Child("coder-7ce2ed20".to_owned()),
            ],
            "running first, each group in the order it started"
        );
        let rows = screen(&waiting, 40);
        let text = rows.join("\n");
        // herdr's two lines: the name, then the state under it.
        let row = |number: usize| {
            let at = rows
                .iter()
                .position(|row| row.starts_with(&format!(" {number} ")))
                .unwrap_or_else(|| panic!("alt+{number}: {text}"));
            format!("{} {}", rows[at], rows[at + 1])
        };
        assert!(
            rows.iter()
                .any(|row| row.contains("agents") && row.contains("5 working · 7")),
            "{text}"
        );
        for (number, name, word) in [
            (1, "main", "working"),
            (2, "verifier-b105", "working"),
            (3, "coder-c0cc", "working"),
            (4, "coder-243a", "working"),
            (5, "coder-64d1", "working"),
            (6, "explorer-d3cc", "done"),
            (7, "coder-7ce2", "done"),
        ] {
            assert!(
                row(number).contains(name) && row(number).contains(word),
                "alt+{number} is {name}, {word}: {text}"
            );
        }
        // A child says the model and thinking level it runs on; the level
        // stays whole when the model is cut.
        let model = rows
            .iter()
            .position(|row| row.starts_with(" 2 "))
            .map(|at| rows[at + 2].clone())
            .unwrap_or_default();
        assert!(
            model.contains("◆ deepseek") && model.trim_end().ends_with("high │"),
            "{text}"
        );

        // Once every child settled, main is idle and the failed one sorts
        // before the done ones.
        let mut settled = nine_children();
        for row in &mut settled {
            row.status = "completed".to_owned();
        }
        settled[5].status = "error".to_owned();
        assert!(sync_child_states(&mut waiting.child_activity, &settled));
        assert_eq!(main_status(&waiting), Status::Idle);
        let text = screen(&waiting, 40).join("\n");
        assert!(text.contains(" 2 ✗ coder-c0cc21d5"), "{text}");
        assert!(text.contains("failed"), "{text}");
    }

    /// A child waiting for a place under `[agents] max_workers` has made no
    /// call: it was drawn working beside the ones that were.
    #[test]
    fn a_queued_child_is_drawn_queued_not_working() {
        let mut run = state(AppPhase::Running);
        run.child_activity = vec![ChildActivity {
            name: "verifier-cde190d4".to_owned(),
            state: crate::interactive::events::ChildRunState::Running,
            queued: true,
            ..ChildActivity::default()
        }];
        let status = super::child_status(&run, &run.child_activity[0]);
        assert_eq!(status, Status::Queued);
        assert_eq!(status.word(), "queued");
        run.child_activity[0].queued = false;
        assert_eq!(
            super::child_status(&run, &run.child_activity[0]),
            Status::Working
        );
    }

    /// Seen in a later session: the sidebar said `main working` and
    /// `explorer working · 114 tools`, its pane "2 explorer · working", while
    /// the agents view said "22 subagents (0 running, 7 failed)".
    #[test]
    fn a_child_the_registry_shows_settled_is_never_drawn_working() {
        let mut ended = state(AppPhase::Running);
        ended.child_activity = vec![
            ChildActivity {
                name: "explorer".to_owned(),
                tool_calls: 114,
                ..ChildActivity::default()
            },
            ChildActivity {
                name: "explorer-0d1e2f3a".to_owned(),
                tool_calls: 9,
                state: crate::interactive::events::ChildRunState::Running,
                ..ChildActivity::default()
            },
        ];
        let rows = (0..22)
            .map(|index| SubagentInfo {
                branch: None,
                worktree: None,
                queued: false,
                name: if index == 0 {
                    "explorer-0d1e2f3a".to_owned()
                } else {
                    format!("coder-{index:08x}")
                },
                status: if index % 3 == 0 { "error" } else { "completed" }.to_owned(),
                model: "m".to_owned(),
                thinking: None,
                depth: 1,
                program: String::new(),
            })
            .collect::<Vec<_>>();
        assert!(sync_child_states(&mut ended.child_activity, &rows));
        let shown = ordered_children(&ended)
            .into_iter()
            .map(|(child, status)| (child.name.as_str(), status))
            .collect::<Vec<_>>();
        assert_eq!(
            shown,
            vec![
                ("explorer-0d1e2f3a", Status::Failed),
                ("explorer", Status::Done),
            ],
            "nothing is working while the run still is"
        );
        let text = screen(&ended, 30).join("\n");
        assert!(!text.contains("explorer… working"), "{text}");
        assert!(
            !agent_rows(&text.lines().map(str::to_owned).collect::<Vec<_>>())
                .iter()
                .any(|row| row.contains("explorer") && row.contains("working")),
            "{text}"
        );
        // A child that left the registry (deleted) is not running either.
        let mut gone = state(AppPhase::Ready);
        gone.child_activity = vec![ChildActivity {
            name: "coder-c0cc21d5".to_owned(),
            state: crate::interactive::events::ChildRunState::Running,
            ..ChildActivity::default()
        }];
        assert_eq!(main_status(&gone), Status::Working);
        assert!(sync_child_states(&mut gone.child_activity, &[]));
        assert_eq!(main_status(&gone), Status::Idle, "nothing runs any more");
    }

    #[test]
    fn children_that_do_not_fit_are_counted_and_keep_their_numbers() {
        let mut busy = state(AppPhase::Running);
        sync_child_states(&mut busy.child_activity, &nine_children());
        let others = vec![Entry {
            id: "a7".to_owned(),
            name: "fix-tests".to_owned(),
            status: Status::Idle,
            folder: None,
        }];
        let (rows, hits) = screen_with(
            &busy,
            16,
            Extra {
                others: &others,
                focus: None,
                spin: 0,
            },
        );
        let text = rows.join("\n");
        assert!(
            rows.iter()
                .any(|row| row.contains(" more · ") && row.contains(" working")),
            "the children left out are counted: {text}"
        );
        let listed = hits
            .iter()
            .filter(|(_, target)| matches!(target, Target::Child(_)))
            .count();
        assert!(listed < 4, "{text}");
        let all = targets(&busy, &others);
        for (row, target) in &hits {
            let number = all.iter().position(|t| t == target).expect("a target") + 1;
            assert!(
                rows[usize::from(*row)].starts_with(&format!(" {number} ")),
                "row {row} is reached with alt+{number}: {text}"
            );
        }
    }

    /// The ninja project's four idle agents: the agents view listed them all,
    /// the sidebar `main` and `0e0dde4f` only. Every one is listed now - the
    /// project's, then the other folders' with their folder - and those that
    /// do not fit are counted.
    #[test]
    fn every_agent_the_agents_view_lists_is_in_the_sidebar() {
        let ready = state(AppPhase::Ready);
        let entry = |id: &str, name: &str, folder: Option<&str>| Entry {
            id: id.to_owned(),
            name: name.to_owned(),
            status: Status::Idle,
            folder: folder.map(str::to_owned),
        };
        let others = vec![
            entry("0e0dde4f", "0e0dde4f", None),
            entry("h2", "hello", Some("scratch")),
            entry("h1", "hello", Some("web")),
        ];
        let (rows, hits) = screen_with(
            &ready,
            30,
            Extra {
                others: &others,
                focus: None,
                spin: 0,
            },
        );
        let text = rows.join("\n");
        assert!(text.contains("other agents"), "{text}");
        assert!(text.contains("other folders"), "{text}");
        assert!(
            text.contains("⌂ scratch") && text.contains("⌂ web"),
            "{text}"
        );
        assert_eq!(
            hits.iter()
                .filter(|(_, target)| matches!(target, Target::Agent(_)))
                .count(),
            3,
            "main and the three others, as the agents view: {text}"
        );
        assert!(
            rows.iter()
                .any(|row| row.starts_with(" 4 ") && row.contains("hello")),
            "{text}"
        );

        // A short sidebar counts the agents it has no room for.
        let (rows, hits) = screen_with(
            &ready,
            16,
            Extra {
                others: &others,
                focus: None,
                spin: 0,
            },
        );
        let text = rows.join("\n");
        let listed = hits
            .iter()
            .filter(|(_, target)| matches!(target, Target::Agent(_)))
            .count();
        assert!(listed < 3, "{text}");
        assert!(
            text.contains(&format!("+{} more · /agents", 3 - listed)),
            "{text}"
        );
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
