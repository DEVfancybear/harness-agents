//! Viewport geometry.
//!
//! One frame is laid out top to bottom inside the inline viewport:
//!
//! ```text
//! +--------------------------------------+
//! | live block (text still streaming)    |  hidden when there is none
//! | modal (approval, picker, reference)  |  replaces the live block
//! | composer (1..8 rows, grows upward)   |
//! | status (exactly one row)             |
//! +--------------------------------------+
//! ```
//!
//! Everything here is pure arithmetic over the area and the [`UiState`], so the
//! layout can be unit tested without a terminal.

use ratatui::layout::Rect;

use super::STATUS_ROWS;
use super::theme::Theme;
use crate::interactive::events::{Modal, UiState};

/// The composer never grows past this many rows; it scrolls internally instead.
pub const MAX_COMPOSER_ROWS: u16 = 8;

/// The live block never grows past this many rows; older complete lines are
/// committed to the scrollback instead.
pub const MAX_LIVE_ROWS: u16 = 8;

/// Where each part of the frame goes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Plan {
    pub live: Option<Rect>,
    pub modal: Option<Rect>,
    pub composer: Rect,
    pub status: Rect,
    /// Where the terminal cursor belongs, when the composer owns the focus.
    pub cursor: Option<(u16, u16)>,
    /// The rows the composer is scrolled down by, when the draft is taller than
    /// the space the viewport can give it.
    pub composer_scroll: u16,
    /// Wrapped draft rows shared with the composer renderer. Computing them in
    /// the layout pass avoids two more full Unicode scans per frame.
    pub composer_lines: Vec<String>,
}

/// Lay one frame out inside `area`.
#[must_use]
pub fn plan(area: Rect, state: &UiState, _theme: &Theme) -> Plan {
    let width = area.width.max(1);
    let height = area.height.max(1);
    let status_height = STATUS_ROWS.min(height);
    let status = Rect {
        x: area.x,
        y: area.y + height - status_height,
        width,
        height: status_height,
    };

    let body_height = height.saturating_sub(status_height);
    // The composer grows with its content and takes what it needs first: a draft
    // the user is typing must never be squeezed by streamed text. One extra row
    // is the box's top border.
    let prefix_width = u16::try_from(crate::interactive::tui::widgets::composer::display_width(
        crate::interactive::view::prompt_prefix(state.phase),
    ))
    .unwrap_or(u16::MAX);
    let (composer_lines, composer_cursor) =
        crate::interactive::tui::widgets::composer::wrap_with_prefix(
            &state.buffer,
            state.cursor,
            width,
            prefix_width,
        );
    let wanted_rows = composer_rows(composer_lines.len());
    let shown_rows = wanted_rows.min(MAX_COMPOSER_ROWS.max(1));
    let composer_height = shown_rows.saturating_add(1).min(body_height.max(1));
    let composer = Rect {
        x: area.x,
        y: area.y + body_height.saturating_sub(composer_height),
        width,
        height: composer_height,
    };

    let upper = Rect {
        x: area.x,
        y: area.y,
        width,
        height: body_height.saturating_sub(composer_height),
    };

    let (modal, live) = if let Some(modal) = &state.modal {
        (
            Some(take_rows(upper, modal_rows(modal, upper.height))),
            None,
        )
    } else {
        let rows = live_rows(state, width).min(upper.height).min(MAX_LIVE_ROWS);
        let rect = take_rows(upper, rows);
        (None, if rect.height > 0 { Some(rect) } else { None })
    };

    // Rows the draft needs beyond the ones the box can show scroll out of view.
    let composer_scroll = wanted_rows.saturating_sub(composer.height.saturating_sub(1));
    let cursor = cursor_cell(
        state,
        composer,
        composer_scroll,
        composer_cursor,
        prefix_width,
    );

    Plan {
        live,
        modal,
        composer,
        status,
        cursor,
        composer_scroll,
        composer_lines,
    }
}

/// The bottom `rows` of `area`.
fn take_rows(area: Rect, rows: u16) -> Rect {
    let height = rows.min(area.height);
    Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(height),
        width: area.width,
        height,
    }
}

/// Rows a wrapped draft occupies, never less than one.
///
/// The draft is wrapped once in [`plan`], and this is the only place its height is
/// turned into rows, so the box the renderer paints and the box the layout
/// reserved cannot disagree.
#[must_use]
pub fn composer_rows(wrapped_lines: usize) -> u16 {
    u16::try_from(wrapped_lines).unwrap_or(u16::MAX).max(1)
}

/// Rows the live block needs for the text still streaming.
#[must_use]
pub fn live_rows(state: &UiState, width: u16) -> u16 {
    if state.modal.is_some() {
        return 0;
    }
    let mut rows: u16 = 0;
    if !state.live_text.is_empty() {
        for line in state.live_text.split('\n') {
            let cells = u16::try_from(crate::interactive::tui::widgets::composer::display_width(
                line,
            ))
            .unwrap_or(u16::MAX);
            rows = rows.saturating_add(cells.max(1).div_ceil(width.max(1)));
        }
    }
    if state.open_tool.is_some() {
        rows = rows.saturating_add(1);
    }
    rows.min(MAX_LIVE_ROWS)
}

/// Rows a modal wants, capped by the space available.
fn modal_rows(modal: &Modal, available: u16) -> u16 {
    let content = match modal {
        // The panel owns its own height: a row added to it without raising the
        // reservation would be clipped off the bottom of the viewport instead.
        Modal::Approval { .. } => super::widgets::approval::PANEL_ROWS,
        Modal::Picker { items, .. } => u16::try_from(items.len() + 2).unwrap_or(u16::MAX),
        Modal::Overlay { lines, .. } => u16::try_from(lines.len() + 2).unwrap_or(u16::MAX),
    };
    content.min(available)
}

/// Where the cursor belongs inside the composer.
///
/// The composer is not focused while a modal that owns the keyboard is open, so
/// no cursor is reported and the modal keeps the whole frame.
fn cursor_cell(
    state: &UiState,
    composer: Rect,
    scroll: u16,
    cell: crate::interactive::tui::widgets::composer::Cell,
    prefix_width: u16,
) -> Option<(u16, u16)> {
    if state.modal.is_some() {
        return None;
    }
    let visible_row = cell.row.saturating_sub(scroll);
    if visible_row >= composer.height.saturating_sub(1) {
        return None;
    }
    // The box's first row is its top border, so content row 0 is one row down.
    Some((
        composer.x
            + cell
                .column
                .saturating_add(if cell.row == 0 { prefix_width } else { 0 })
                .min(composer.width.saturating_sub(1)),
        composer.y + 1 + visible_row,
    ))
}

#[cfg(test)]
mod tests {
    use super::{MAX_COMPOSER_ROWS, composer_rows, live_rows, plan};
    use crate::interactive::events::{AppPhase, Modal, UiState};
    use crate::interactive::tui::theme::Theme;
    use ratatui::layout::Rect;
    use std::time::Duration;

    fn state(phase: AppPhase) -> UiState {
        UiState {
            phase,
            setup_required: false,
            setup_hint: None,
            header: Vec::new(),
            buffer: String::new(),
            cursor: 0,
            live_text: String::new(),
            open_tool: None,
            modal: None,
            reads_for_run: false,
            last_request: None,
            run_started_at: None,
            last_run_elapsed: Duration::ZERO,
            steps: 0,
            max_steps: 8,
            tool_calls: 0,
            max_tool_calls: 16,
            completion: Vec::new(),
            fallback_reason: None,
            tick: 0,
        }
    }

    #[test]
    fn t02_status_is_always_the_last_row_and_composer_sits_above_it() {
        let area = Rect::new(0, 0, 80, 12);
        let outline = plan(area, &state(AppPhase::Ready), &Theme::plain());
        assert_eq!(outline.status.y, 11);
        assert_eq!(outline.status.height, 1);
        assert_eq!(
            outline.composer.height, 2,
            "one content row plus the top border"
        );
        assert_eq!(outline.composer.y, 9);
        assert_eq!(
            outline.cursor,
            Some((2, 10)),
            "the marker sits on the content row"
        );
    }
    #[test]
    fn t02_a_multiline_draft_grows_the_composer_upwards() {
        let mut state = state(AppPhase::Ready);
        state.buffer = "one\ntwo\nthree".to_owned();
        state.cursor = state.buffer.chars().count();
        assert_eq!(composer_rows(3), 3);

        let outline = plan(Rect::new(0, 0, 80, 12), &state, &Theme::plain());
        assert_eq!(outline.composer.height, 4);
        assert_eq!(outline.composer.y, 7);
        assert_eq!(outline.cursor.map(|(_, y)| y), Some(10));
    }

    #[test]
    fn t02_the_composer_scrolls_instead_of_growing_past_its_budget() {
        let mut state = state(AppPhase::Ready);
        state.buffer = (0..20)
            .map(|index| format!("row {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        state.cursor = state.buffer.chars().count();
        let outline = plan(Rect::new(0, 0, 80, 12), &state, &Theme::plain());
        assert_eq!(outline.composer.height, MAX_COMPOSER_ROWS + 1);
        assert_eq!(
            outline.composer_scroll, 12,
            "twenty rows minus the eight the box shows"
        );
        assert_eq!(
            outline.cursor.map(|(_, y)| y),
            Some(outline.composer.y + 1 + 7),
            "the cursor follows the visible window, below the box border"
        );
    }

    #[test]
    fn t03_layout_uses_the_same_wrap_for_rows_and_cursor() {
        let mut state = state(AppPhase::Ready);
        state.buffer = "abcde".to_owned();
        state.cursor = 5;
        let outline = plan(Rect::new(0, 0, 6, 8), &state, &Theme::plain());
        assert_eq!(
            outline.composer_lines,
            ["abcd".to_owned(), "e".to_owned()],
            "the first row reserves two cells for the prompt marker"
        );
        assert_eq!(
            outline.cursor,
            Some((1, outline.composer.y + 2)),
            "the cursor uses the shared wrapped rows instead of the unwrapped prompt"
        );
    }

    #[test]
    fn t02_the_live_block_sits_above_the_composer_and_a_modal_replaces_it() {
        let mut state = state(AppPhase::Running);
        state.live_text = "streaming".to_owned();
        let outline = plan(Rect::new(0, 0, 80, 12), &state, &Theme::plain());
        let live = outline.live.expect("a live block while text streams");
        assert_eq!(live.y + live.height, outline.composer.y);
        assert_eq!(live.height, 1);
        assert!(outline.modal.is_none());

        state.modal = Some(Modal::Approval {
            request_id: "req".to_owned(),
            action: "apply_patch".to_owned(),
            summary: "path=a.rs".to_owned(),
            workspace: "C:/w".to_owned(),
            scope: "once".to_owned(),
            expires_at: std::time::Instant::now(),
            read_only: false,
        });
        let outline = plan(Rect::new(0, 0, 80, 12), &state, &Theme::plain());
        assert!(outline.modal.is_some(), "the modal takes the upper region");
        assert!(outline.live.is_none(), "and the live block is hidden");
        assert_eq!(outline.cursor, None, "the composer loses focus");
    }

    #[test]
    fn t02_live_rows_are_capped() {
        let mut state = state(AppPhase::Running);
        state.live_text = (0..50)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(live_rows(&state, 80), super::MAX_LIVE_ROWS);
    }
}
