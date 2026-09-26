//! The status bar: one row that says what the app is doing.
//!
//! It never redraws on its own: the render loop only paints when the controller
//! produced an effect or a tick asked for one, and a tick asks only while a run is
//! active or an approval is pending (T05 acceptance: an idle loop draws nothing).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::super::theme::Theme;
use crate::interactive::events::{AppPhase, Modal, UiState};
use crate::interactive::view;

/// Draw the status bar.
pub fn render(frame: &mut Frame, area: Rect, state: &UiState, theme: &Theme) {
    if area.height == 0 {
        return;
    }
    let line = row(state, theme, area.width);
    frame.render_widget(
        ratatui::widgets::Paragraph::new(line).style(theme.status),
        area,
    );
}

/// Build the one status row.
#[must_use]
#[allow(clippy::too_many_lines, reason = "one arm per phase and panel")]
pub fn row(state: &UiState, theme: &Theme, width: u16) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let budget = usize::from(width);
    let mut used = 0_usize;
    // A span that does not fit is truncated, never dropped: a narrow console must
    // still say which model is selected.
    let mut push = |mut span: Span<'static>| {
        let cost = super::composer::display_width(&span.content);
        if used >= budget {
            return;
        }
        if used + cost > budget {
            let room = budget - used;
            if room == 0 {
                return;
            }
            let kept: String = span
                .content
                .chars()
                .scan(0_usize, |cells, character| {
                    *cells += super::composer::char_width(character);
                    Some((*cells, character))
                })
                .take_while(|(cells, _)| *cells <= room)
                .map(|(_, character)| character)
                .collect();
            if kept.is_empty() {
                return;
            }
            span.content = kept.into();
            spans.push(span);
            used = budget;
            return;
        }
        spans.push(span);
        used += cost;
    };

    match (&state.modal, state.phase) {
        (Some(Modal::Approval { expires_at, .. }), _) => {
            let remaining = expires_at.saturating_duration_since(std::time::Instant::now());
            push(Span::styled(
                format!(" approval · còn {}", view::clock_label(remaining)),
                theme.warning,
            ));
            push(Span::styled(" · y chạy · n từ chối".to_owned(), theme.dim));
        }
        (Some(Modal::Picker { items, selected }), _) => {
            push(Span::styled(
                format!(" sessions {}/{}", selected + 1, items.len().max(1)),
                theme.accent,
            ));
            push(Span::styled(
                " · ↑↓ chọn · Enter · Esc".to_owned(),
                theme.dim,
            ));
        }
        (Some(Modal::FilePicker { items, selected }), _) => {
            push(Span::styled(
                format!(" files {}/{}", selected + 1, items.len().max(1)),
                theme.accent,
            ));
            push(Span::styled(
                " · gõ lọc · ↑↓ · Enter · Esc".to_owned(),
                theme.dim,
            ));
        }
        (Some(Modal::Question { options, .. }), _) => {
            push(Span::styled(" question", theme.accent));
            if !options.is_empty() {
                push(Span::styled(
                    format!(" · {} numbered options", options.len()),
                    theme.dim,
                ));
            }
            push(Span::styled(" · Enter answers", theme.dim));
        }
        (Some(Modal::McpElicitation { .. }), _) => {
            push(Span::styled(" MCP input", theme.warning));
            push(Span::styled(
                " · JSON · decline · cancel".to_owned(),
                theme.dim,
            ));
        }
        (Some(Modal::Overlay { title, .. }), _) => {
            push(Span::styled(format!(" {title}"), theme.title));
            push(Span::styled(" · Esc đóng".to_owned(), theme.dim));
        }
        (None, AppPhase::Running | AppPhase::WaitingMcpInput | AppPhase::Canceling) => {
            // prime-agent's working line: what the agent is doing, how long it has
            // been at it, then the counters.
            let activity = if state.phase == AppPhase::Canceling {
                "Canceling"
            } else if !state.open_tools.is_empty() {
                "Executing"
            } else if state.live_text.is_empty() {
                "Thinking"
            } else {
                "Writing"
            };
            push(Span::styled(
                format!("{} ", Theme::spinner(state.tick)),
                if state.phase == AppPhase::Canceling {
                    theme.warning
                } else {
                    theme.accent
                },
            ));
            push(Span::styled(activity.to_owned(), theme.muted));
            if let Some(started) = state.run_started_at {
                push(Span::styled(
                    format!(" · {}", view::clock_label(started.elapsed())),
                    theme.muted,
                ));
            }
            push(Span::styled(" · esc to interrupt".to_owned(), theme.dim));
            // Put decisions and queued work before progress counters. At 60 cells
            // the right edge may be clipped, but the operator must still see when
            // the approval gate is open for the rest of this turn.
            if state.granted_for_run {
                push(Span::styled(" · tự động cả lượt", theme.warning));
            }
            if state.queued_input {
                push(Span::styled(" · queued (1)", theme.accent));
            }
            push(Span::styled(
                format!(" · step {}/{}", state.steps, state.max_steps),
                theme.dim,
            ));
            push(Span::styled(
                format!(" · tools {}/{}", state.tool_calls, state.max_tool_calls),
                theme.dim,
            ));
            if let Some(cost) = cost_label(state) {
                let label = format!(" · cost {cost}");
                push(Span::styled(label, theme.dim));
            }
            if let Some(context) = context_label(state) {
                push(Span::styled(format!(" · ctx {context}"), theme.dim));
            }
            if let Some(model) = short_model(state) {
                push(Span::styled(format!(" · {model}"), theme.dim));
            }
            if let Some(level) = &state.thinking {
                push(Span::styled(format!(" · thinking {level}"), theme.dim));
            }
        }
        (None, AppPhase::SetupRequired) => {
            push(Span::styled(" setup required".to_owned(), theme.error));
            if let Some(hint) = &state.setup_hint {
                let label = format!(" · {hint}");
                push(Span::raw(label));
            }
        }
        (None, _) => {
            // prime-agent's line above the prompt: quiet, dim, and it names the
            // detail mode ctrl+o cycles.
            // The model and the thinking level the next turn uses.
            if let Some(model) = short_model(state) {
                push(Span::styled(model, theme.muted));
            }
            if let Some(level) = &state.thinking {
                push(Span::styled(format!(" · thinking {level}"), theme.dim));
            }
            if let Some(cost) = cost_label(state) {
                let label = format!(" · {cost}");
                push(Span::styled(label, theme.dim));
            }
            if let Some(context) = context_label(state) {
                push(Span::styled(format!(" · ctx {context}"), theme.dim));
            }
            push(Span::styled(
                format!(" · {}", state.detail.hint()),
                theme.dim,
            ));
        }
    }

    if let Some(reason) = &state.fallback_reason {
        push(Span::styled(format!(" · {reason}"), theme.error));
    }

    Line::from(spans)
}

/// How full the context is, once a response has said.
fn context_label(state: &UiState) -> Option<String> {
    state
        .header
        .iter()
        .find_map(|line| line.strip_prefix("Context: ").map(str::to_owned))
}

fn cost_label(state: &UiState) -> Option<String> {
    state
        .header
        .iter()
        .find_map(|line| line.strip_prefix("Cost: ").map(str::to_owned))
        // No price known is not a cost worth a place on the line.
        .filter(|label| label != "n/a")
}

/// The model name without the endpoint it is reached through.
fn short_model(state: &UiState) -> Option<String> {
    let model = model_label(state)?;
    Some(
        model
            .split_once(" via ")
            .map_or(model.as_str(), |(name, _)| name)
            .to_owned(),
    )
}

/// The model label, when the header names one.
///
/// The caller measures and truncates it for the console width; this function
/// never drops a label for being long, because a narrow console must still say
/// which model is selected.
#[must_use]
pub fn model_label(state: &UiState) -> Option<String> {
    state
        .header
        .iter()
        .find_map(|line| line.strip_prefix("Service: "))
        .or_else(|| {
            state
                .header
                .iter()
                .find_map(|line| line.split_once("Provider: ").map(|(_, provider)| provider))
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{model_label, row};
    use crate::interactive::events::{AppPhase, Modal, UiState};
    use crate::interactive::tui::markdown::plain_text;
    use crate::interactive::tui::theme::Theme;
    use std::time::{Duration, Instant};

    fn state(phase: AppPhase) -> UiState {
        UiState {
            phase,
            setup_required: false,
            setup_hint: None,
            header: vec!["Provider: deepseek-chat via https://api.deepseek.com".to_owned()],
            buffer: String::new(),
            cursor: 0,
            live_text: String::new(),
            open_tools: Vec::new(),
            modal: None,
            granted_for_run: false,
            queued_input: false,
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
            thinking: None,
        }
    }

    #[test]
    fn t05_a_running_status_reports_spinner_steps_tools_and_the_clock() {
        let mut state = state(AppPhase::Running);
        state.steps = 2;
        state.tool_calls = 3;
        state.run_started_at = Some(Instant::now());
        let text = plain_text(&[row(&state, &Theme::plain(), 120)]);
        assert!(text.contains("Thinking"), "{text}");
        assert!(text.contains("step 2/8"), "{text}");
        assert!(text.contains("tools 3/16"), "{text}");
        assert!(text.contains("00:00"), "{text}");
        assert!(text.contains("esc to interrupt"), "{text}");
    }

    #[test]
    fn t05_an_idle_status_names_the_model_and_the_detail_mode() {
        let state = state(AppPhase::Ready);
        let text = plain_text(&[row(&state, &Theme::plain(), 200)]);
        assert!(
            text.contains("deepseek-chat"),
            "the model label is shown: {text}"
        );
        assert!(text.contains("Collapsed mode (ctrl+o to expand)"), "{text}");
        assert_eq!(
            model_label(&state).as_deref(),
            Some("deepseek-chat via https://api.deepseek.com")
        );

        let mut real_header = state;
        real_header.header = vec![
            "Project: C:/work    Provider: credential from DEEPSEEK_API_KEY".to_owned(),
            "Service: deepseek-chat via https://api.deepseek.com".to_owned(),
        ];
        assert_eq!(
            model_label(&real_header).as_deref(),
            Some("deepseek-chat via https://api.deepseek.com"),
            "the runtime Service line outranks bootstrap's credential description"
        );
    }

    #[test]
    fn t05_setup_required_says_what_is_missing() {
        let mut state = state(AppPhase::SetupRequired);
        state.header = vec!["Provider: setup required (no provider configured)".to_owned()];
        state.setup_hint =
            Some("provider credentials are missing; set DEEPSEEK_API_KEY and restart".to_owned());
        let text = plain_text(&[row(&state, &Theme::plain(), 200)]);
        assert!(text.contains("setup required"), "{text}");
        assert!(text.contains("DEEPSEEK_API_KEY"), "{text}");
    }

    #[test]
    fn t05_an_approval_shows_the_countdown_and_the_answer_keys() {
        let mut state = state(AppPhase::WaitingApproval);
        state.modal = Some(Modal::Approval {
            request_id: "req".to_owned(),
            action: "apply_patch".to_owned(),
            summary: "path=a.rs".to_owned(),
            workspace: "C:/w".to_owned(),
            scope: "once".to_owned(),
            expires_at: Instant::now() + Duration::from_mins(5),
            read_only: false,
            scroll: 0,
        });
        let text = plain_text(&[row(&state, &Theme::plain(), 200)]);
        assert!(text.contains("approval"), "{text}");
        assert!(text.contains("còn 04:"), "a real countdown: {text}");
        assert!(text.contains("y chạy"), "{text}");
    }

    #[test]
    fn t05_the_bar_never_exceeds_the_console_width() {
        let mut state = state(AppPhase::Running);
        state.steps = 2;
        let line = row(&state, &Theme::plain(), 20);
        let text = plain_text(&[line]);
        assert!(
            crate::interactive::tui::widgets::composer::display_width(&text) <= 20,
            "must fit in 20 cells: {text:?}"
        );
    }
}
