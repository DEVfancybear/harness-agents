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
    frame.render_widget(ratatui::widgets::Paragraph::new(line), area);
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
    let mut push = |mut span: Span<'static>, cost: usize| {
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
            push(
                Span::styled(
                    format!(" approval · còn {}", view::clock_label(remaining)),
                    theme.tool_ok,
                ),
                22,
            );
            push(
                Span::styled(" · y chạy · n từ chối".to_owned(), theme.dim),
                22,
            );
        }
        (Some(Modal::Picker { items, selected }), _) => {
            push(
                Span::styled(
                    format!(" sessions {}/{}", selected + 1, items.len().max(1)),
                    theme.accent,
                ),
                16,
            );
            push(
                Span::styled(" · ↑↓ chọn · Enter · Esc".to_owned(), theme.dim),
                26,
            );
        }
        (Some(Modal::Overlay { title, .. }), _) => {
            push(Span::styled(format!(" {title}"), theme.title), 12);
            push(Span::styled(" · Esc đóng".to_owned(), theme.dim), 12);
        }
        (None, AppPhase::Running | AppPhase::Canceling) => {
            push(
                Span::styled(
                    format!(" {} running", Theme::spinner(state.tick)),
                    theme.accent,
                ),
                12,
            );
            push(
                Span::styled(
                    format!(" · step {}/{}", state.steps, state.max_steps),
                    theme.dim,
                ),
                16,
            );
            push(
                Span::styled(
                    format!(" · tools {}/{}", state.tool_calls, state.max_tool_calls),
                    theme.dim,
                ),
                16,
            );
            if let Some(started) = state.run_started_at {
                push(
                    Span::styled(
                        format!(" · {}", view::clock_label(started.elapsed())),
                        theme.dim,
                    ),
                    8,
                );
            }
            // An open gate is state the operator has to be able to see: after the
            // panel closes there is nothing else on screen that says actions -
            // including writes and commands - are running without being asked about.
            if state.granted_for_run {
                push(Span::styled(" · tự động cả lượt", theme.tool_ok), 18);
            }
            if let Some(cost) = cost_label(state) {
                let label = format!(" · cost {cost}");
                push(
                    Span::styled(label.clone(), theme.dim),
                    super::composer::display_width(&label),
                );
            }
            push(Span::styled(" · Ctrl-C hủy".to_owned(), theme.dim), 14);
        }
        (None, AppPhase::SetupRequired) => {
            push(Span::styled(" setup required".to_owned(), theme.error), 18);
            if let Some(hint) = &state.setup_hint {
                let label = format!(" · {hint}");
                let cost = super::composer::display_width(&label);
                push(Span::raw(label), cost);
            }
        }
        (None, _) => {
            push(Span::styled(" ready".to_owned(), theme.tool_ok), 8);
            if let Some(model) = model_label(state) {
                // The cost is measured in cells, not bytes: the separator is a
                // multi-byte character and a Vietnamese label is not ASCII.
                let cost = super::composer::display_width(&model) + 3;
                push(Span::styled(format!(" · {model}"), theme.dim), cost);
            }
            if let Some(cost) = cost_label(state) {
                let label = format!(" · cost {cost}");
                push(
                    Span::styled(label.clone(), theme.dim),
                    super::composer::display_width(&label),
                );
            }
            push(Span::styled(" · /help".to_owned(), theme.dim), 8);
        }
    }

    if let Some(reason) = &state.fallback_reason {
        push(
            Span::styled(format!(" · {reason}"), theme.error),
            reason.len() + 3,
        );
    }

    Line::from(spans)
}

fn cost_label(state: &UiState) -> Option<String> {
    state
        .header
        .iter()
        .find_map(|line| line.strip_prefix("Cost: ").map(str::to_owned))
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
            open_tool: None,
            modal: None,
            granted_for_run: false,
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
        }
    }

    #[test]
    fn t05_a_running_status_reports_spinner_steps_tools_and_the_clock() {
        let mut state = state(AppPhase::Running);
        state.steps = 2;
        state.tool_calls = 3;
        state.run_started_at = Some(Instant::now());
        let text = plain_text(&[row(&state, &Theme::plain(), 120)]);
        assert!(text.contains("running"), "{text}");
        assert!(text.contains("step 2/8"), "{text}");
        assert!(text.contains("tools 3/16"), "{text}");
        assert!(text.contains("00:00"), "{text}");
        assert!(text.contains("Ctrl-C"), "{text}");
    }

    #[test]
    fn t05_an_idle_status_names_the_model_and_the_help_command() {
        let state = state(AppPhase::Ready);
        let text = plain_text(&[row(&state, &Theme::plain(), 200)]);
        assert!(text.contains("ready"), "{text}");
        assert!(
            text.contains("deepseek-chat"),
            "the model label is shown: {text}"
        );
        assert!(text.contains("/help"), "{text}");
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
