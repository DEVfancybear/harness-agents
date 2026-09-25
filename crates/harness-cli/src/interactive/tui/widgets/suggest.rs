//! The slash-command menu, drawn directly above the composer.
//!
//! It is deliberately **not** a modal: the composer keeps the keyboard, the draft
//! stays on screen and every keystroke keeps narrowing the list. What the user
//! sees is a row per matching command - the highlighted one marked with `❯` - so
//! typing `/` answers "what can I do here?" without running anything.
//!
//! The command column is padded to [`NAME_COLUMN`] so the summaries line up. On a
//! console narrower than a row, ratatui clips the tail: the name comes first, so
//! what can be lost is the description, never the command.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::super::layout::MAX_SUGGEST_ROWS;
use super::super::theme::Theme;
use crate::interactive::events::UiState;

/// Cells the command column occupies, so every summary starts in the same place.
pub const NAME_COLUMN: usize = 17;

/// The widest the name column grows for long labels.
const MAX_NAME_COLUMN: usize = 32;

/// The rows the menu shows, with the highlighted one marked.
#[must_use]
pub fn rows(state: &UiState, theme: &Theme) -> Vec<Line<'static>> {
    let (start, end) = window(state.suggestions.len(), state.suggestion_selected);
    // The name column fits the longest label on screen (a skill name can be long),
    // within bounds, so the descriptions line up.
    let column = state.suggestions[start..end]
        .iter()
        .map(|item| item.label.chars().count() + 2)
        .max()
        .unwrap_or(NAME_COLUMN)
        .clamp(NAME_COLUMN, MAX_NAME_COLUMN);
    let mut lines = Vec::new();
    for index in start..end {
        let item = &state.suggestions[index];
        let selected = index == state.suggestion_selected;
        let style = if selected { theme.selection } else { theme.dim };
        let marker = if selected { "❯ " } else { "  " };
        let mut spans = vec![
            Span::styled(marker.to_owned(), style),
            Span::styled(format!("{:<column$}", item.label), style),
            Span::styled(item.description.clone(), style),
        ];
        if let Some(tag) = &item.tag {
            spans.push(Span::styled(format!(" ({tag})"), theme.dim));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// The slice of the list the menu shows, so the highlight is always in view.
///
/// With more matches than rows the window slides: it holds still while the
/// highlight moves inside it, then follows one row at a time. `selected` is
/// clamped, so a caller can hand over any index.
#[must_use]
pub fn window(len: usize, selected: usize) -> (usize, usize) {
    let cap = usize::from(MAX_SUGGEST_ROWS).min(len);
    if cap == 0 {
        return (0, 0);
    }
    let start = selected.min(len - 1).saturating_sub(cap - 1).min(len - cap);
    (start, start + cap)
}

/// Draw the menu.
pub fn render(frame: &mut Frame, area: Rect, state: &UiState, theme: &Theme) {
    if area.height == 0 || state.suggestions.is_empty() {
        return;
    }
    frame.render_widget(Paragraph::new(rows(state, theme)), area);
}

#[cfg(test)]
mod tests {
    use super::{NAME_COLUMN, rows, window};
    use crate::interactive::events::{AppPhase, UiState};
    use crate::interactive::input::matching;
    use crate::interactive::tui::layout::MAX_SUGGEST_ROWS;
    use crate::interactive::tui::markdown::plain_text;
    use crate::interactive::tui::theme::Theme;
    use std::time::Duration;

    fn state(prefix: &str, selected: usize) -> UiState {
        UiState {
            phase: AppPhase::Ready,
            setup_required: false,
            setup_hint: None,
            header: Vec::new(),
            buffer: prefix.to_owned(),
            cursor: prefix.chars().count(),
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
            suggestions: matching(prefix),
            suggestion_selected: selected,
            fallback_reason: None,
            tick: 0,
            detail: crate::interactive::events::Detail::default(),
            thinking: None,
        }
    }

    /// Typing `/` is a question, and the menu is the answer. Every command this
    /// revision understands has a row that names it and says what it does - and
    /// every one of them is reachable, which is what moving the highlight proves.
    #[test]
    fn slash_every_command_has_a_row_that_says_what_it_does() {
        for (index, command) in crate::interactive::input::SLASH_COMMANDS.iter().enumerate() {
            let text = plain_text(&rows(&state("/", index), &Theme::plain()));
            assert!(
                text.contains(command.name),
                "the row for {} is not on screen when it is highlighted: {text}",
                command.name
            );
            assert!(
                text.contains(command.summary),
                "the row for {} does not say what it does: {text}",
                command.name
            );
        }
        assert!(
            plain_text(&rows(&state("/", 0), &Theme::plain())).starts_with("❯ /model"),
            "the arrow starts on the first match"
        );
    }

    /// A command that takes an argument shows it, so the row reads as the thing to
    /// type rather than as a name to guess the rest of.
    #[test]
    fn slash_a_row_shows_the_argument_a_command_takes() {
        let text = plain_text(&rows(&state("/at", 0), &Theme::plain()));
        assert!(text.contains("/attach <path>"), "{text}");
        assert_eq!(
            plain_text(&rows(&state("/res", 0), &Theme::plain())).trim_start_matches("❯ "),
            format!(
                "{:<NAME_COLUMN$}{}",
                "/resume [id]", "Open the session picker, or resume a session by id"
            ),
            "the name column is padded so the summaries line up"
        );
    }

    /// Only one row carries the arrow, and it is the one the controller would
    /// accept: the highlight and the accepted command cannot disagree.
    #[test]
    fn slash_the_highlight_moves_with_the_selection() {
        let first = plain_text(&rows(&state("/", 0), &Theme::plain()));
        let second = plain_text(&rows(&state("/", 1), &Theme::plain()));
        assert!(first.starts_with("❯ /model"), "{first}");
        assert!(second.starts_with("  /model"), "{second}");
        assert!(second.contains("\n❯ /effort"), "{second}");
        assert_eq!(
            first.matches('❯').count(),
            1,
            "exactly one row is highlighted"
        );
    }

    /// Past the cap the list is a window, not a truncation: the highlight is always
    /// inside it, so a command selected with the arrow keys is always visible.
    #[test]
    fn slash_a_long_list_windows_around_the_highlight() {
        let len = crate::interactive::input::SLASH_COMMANDS.len();
        assert!(
            len > usize::from(MAX_SUGGEST_ROWS),
            "the window is exercised"
        );
        assert_eq!(window(len, 0), (0, usize::from(MAX_SUGGEST_ROWS)));
        assert_eq!(
            window(len, 5),
            (0, usize::from(MAX_SUGGEST_ROWS)),
            "the window holds still while the highlight moves inside it"
        );
        assert_eq!(
            window(len, 6),
            (1, 1 + usize::from(MAX_SUGGEST_ROWS)),
            "then it follows the highlight one row at a time"
        );
        assert_eq!(
            window(len, len - 1),
            (len - usize::from(MAX_SUGGEST_ROWS), len),
            "the last command is always reachable"
        );
        assert_eq!(window(0, 0), (0, 0), "an empty list has no window");
        assert_eq!(
            window(len, usize::MAX),
            (len - usize::from(MAX_SUGGEST_ROWS), len)
        );
    }
}
