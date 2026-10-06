//! prime-agent's queued-message strip (`queued.rs`, TS
//! `updatePendingMessagesDisplay`): above the composer, one dim preview row
//! per queued message - its lane label and its first line - and one hint row
//! below them. The strip is gone when the queue is empty.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::super::theme::Theme;
use crate::interactive::events::UiState;

/// The hint under the previews: prime-agent names its browse key; ha's queue
/// is edited with `/queue`.
pub const HINT: &str = "\u{2570}\u{2500} /queue to edit queued messages";

/// Rows the strip wants: every preview and the hint, none when nothing waits.
#[must_use]
pub fn rows(state: &UiState) -> u16 {
    if state.queued_previews.is_empty() {
        return 0;
    }
    u16::try_from(state.queued_previews.len() + 1).unwrap_or(u16::MAX)
}

/// Draw the strip into `area`: the newest previews that fit, then the hint.
pub fn render(frame: &mut Frame, area: Rect, state: &UiState, theme: &Theme) {
    if area.height == 0 || area.width < 2 {
        return;
    }
    let width = usize::from(area.width - 1);
    let previews = usize::from(area.height - 1);
    let skip = state.queued_previews.len().saturating_sub(previews);
    let mut lines = state.queued_previews[skip..]
        .iter()
        .map(|preview| row(preview, width, theme))
        .collect::<Vec<_>>();
    lines.push(row(HINT, width, theme));
    frame.render_widget(Paragraph::new(lines), area);
}

/// One dim row with prime-agent's one-cell left pad, cut with `...`.
fn row(text: &str, width: usize, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        Span::styled(truncate(text, width), theme.dim),
    ])
}

/// `text` cut to `width` cells, ending in `...` when it was cut.
fn truncate(text: &str, width: usize) -> String {
    if super::composer::display_width(text) <= width {
        return text.to_owned();
    }
    let room = width.saturating_sub(3);
    let mut shown = String::new();
    let mut used = 0;
    for character in text.chars() {
        let cells = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if used + cells > room {
            break;
        }
        used += cells;
        shown.push(character);
    }
    shown.push_str(&"...".chars().take(width).collect::<String>());
    shown
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn a_long_preview_is_cut_with_an_ellipsis() {
        assert_eq!(truncate("Steering: short", 40), "Steering: short");
        assert_eq!(truncate("Follow-up: a long message", 14), "Follow-up: ...");
    }
}
