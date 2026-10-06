//! Viewport geometry.
//!
//! One frame is laid out inside the inline viewport and **anchored to its last
//! row**, so the composer sits at the bottom of the console - where every other
//! terminal chat puts it - and the rows a short frame does not need stay empty
//! *above* the conversation block instead of below the status line:
//!
//! ```text
//! +--------------------------------------+
//! | (slack: empty rows the frame did not  |  the live block grows up into it
//! |  need; the live block grows into it)  |
//! | live block (text still streaming)    |  hidden when there is none
//! | modal (approval, picker, reference)  |  replaces the live block
//! | rounded composer (1..8 input rows)  |
//! | contextual keyboard hints           |
//! | status (exactly one row)             |  the last row of the console
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

/// The slash-command menu never shows more rows than this.
///
/// It sits directly above the composer, inside the same region the live block
/// uses, so the list a user is choosing from must not swallow the whole frame:
/// past this many matches the window follows the highlight ([`super::widgets::suggest`]),
/// which is why the cap is a window and not a truncation.
pub const MAX_SUGGEST_ROWS: u16 = 6;

/// Where each part of the frame goes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Plan {
    pub live: Option<Rect>,
    pub modal: Option<Rect>,
    /// The slash-command menu, when the composer is offering one.
    pub suggest: Option<Rect>,
    /// prime-agent's queued-message strip, while messages wait.
    pub queue: Option<Rect>,
    pub composer: Rect,
    pub status: Rect,
    pub hints: Rect,
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
    let empty = Rect::new(area.x, area.y, 0, 0);
    if area.width < 12 || area.height < 5 || (state.modal.is_some() && area.height < 7) {
        return Plan {
            live: None,
            modal: None,
            suggest: None,
            queue: None,
            composer: area,
            status: empty,
            hints: empty,
            cursor: None,
            composer_scroll: 0,
            composer_lines: Vec::new(),
        };
    }
    let prefix_width = u16::try_from(super::widgets::composer::display_width(
        crate::interactive::view::prompt_prefix(state.phase),
    ))
    .unwrap_or(u16::MAX);
    // The rounded border and one space of padding on each side occupy four cells.
    let (composer_lines, composer_cursor) = super::widgets::composer::wrap_with_prefix(
        &state.buffer,
        state.cursor,
        area.width - 4,
        prefix_width,
    );
    let wanted_rows = composer_rows(composer_lines.len());
    // Give an idle draft breathing room; menus and short terminals reclaim it.
    let minimum = if area.height >= 10 && state.modal.is_none() && state.suggestions.is_empty() {
        2
    } else {
        1
    };
    let reserved = if state.modal.is_some() {
        2
    } else {
        u16::from(!state.suggestions.is_empty())
    };
    let shown_rows = wanted_rows
        .max(minimum)
        .min(MAX_COMPOSER_ROWS)
        .min(area.height.saturating_sub(4 + reserved).max(1));
    let composer_height = shown_rows + 2;
    let available = area.height - composer_height - STATUS_ROWS - 1;
    let mut y = area.y;
    let mut take = |height| {
        let rect = Rect::new(area.x, y, area.width, height);
        y += height;
        rect
    };
    let (live, modal, queue, suggest) = if let Some(modal) = &state.modal {
        (None, Some(take(modal_rows(modal, available))), None, None)
    } else {
        let menu_rows = u16::try_from(state.suggestions.len())
            .unwrap_or(u16::MAX)
            .min(MAX_SUGGEST_ROWS)
            .min(available);
        // The queued strip comes before the live block: what waits stays in
        // view, and the running output gets the rows left over.
        let queue_rows = super::widgets::queue::rows(state).min(available - menu_rows);
        // One blank row keeps the running output off the composer's border,
        // when there is a row to spare for it.
        let room = available - menu_rows - queue_rows;
        let rows = live_rows(state, area.width).min(room.saturating_sub(u16::from(room >= 2)));
        let live = (rows > 0).then(|| take(rows));
        let _spacer = live.filter(|_| room >= 2).map(|_| take(1));
        let queue = (queue_rows > 0).then(|| take(queue_rows));
        let suggest = (menu_rows > 0).then(|| take(menu_rows));
        (live, None, queue, suggest)
    };
    let composer = take(composer_height);
    let hints = take(1);
    let status = take(STATUS_ROWS);
    // Anchor the block to the bottom of the area: what the frame did not use is
    // slack above the live block, never a gap between the status line and the
    // bottom of the console.
    // The rows were taken one after another from the top of the area.
    let slack = area
        .height
        .saturating_sub(status.y + status.height - area.y);
    let (live, modal, queue, suggest, composer, hints, status) = (
        live.map(|rect| drop_rows(rect, slack)),
        modal.map(|rect| drop_rows(rect, slack)),
        queue.map(|rect| drop_rows(rect, slack)),
        suggest.map(|rect| drop_rows(rect, slack)),
        drop_rows(composer, slack),
        drop_rows(hints, slack),
        drop_rows(status, slack),
    );
    // Follow the cursor even when editing an earlier line in a long draft.
    let composer_scroll = wanted_rows
        .saturating_sub(shown_rows)
        .min(composer_cursor.row);
    // `composer` is already shifted by the anchor, so the cell the cursor lands in
    // is already in screen coordinates too.
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
        suggest,
        queue,
        composer,
        status,
        hints,
        cursor,
        composer_scroll,
        composer_lines,
    }
}
/// The same rect `rows` further down: how the bottom anchor moves a block.
fn drop_rows(rect: Rect, rows: u16) -> Rect {
    Rect::new(rect.x, rect.y.saturating_add(rows), rect.width, rect.height)
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

/// Rows the live block needs for the text still streaming, wrapped the way the
/// answer is drawn: one cell in from each edge.
#[must_use]
pub fn live_rows(state: &UiState, width: u16) -> u16 {
    let width = width.saturating_sub(2);
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
    rows = rows.saturating_add(u16::try_from(state.open_tools.len()).unwrap_or(u16::MAX));
    rows = rows.saturating_add(u16::try_from(state.live_tool_output.len()).unwrap_or(u16::MAX));
    rows.min(MAX_LIVE_ROWS)
}

/// Rows a modal wants, capped by the space available.
fn modal_rows(modal: &Modal, available: u16) -> u16 {
    let content = match modal {
        // The panel owns its own height: a row added to it without raising the
        // reservation would be clipped off the bottom of the viewport instead.
        Modal::Approval { summary, .. } => super::widgets::approval::requested_rows(summary),
        Modal::Picker { items, .. } | Modal::FilePicker { items, .. } => {
            u16::try_from(items.len() + 2).unwrap_or(u16::MAX)
        }
        Modal::Question {
            prompt, options, ..
        } => u16::try_from(prompt.lines().count() + options.len() + 3).unwrap_or(u16::MAX),
        Modal::McpElicitation {
            message,
            requested_schema,
        } => {
            let schema_lines = requested_schema
                .as_ref()
                .and_then(|schema| serde_json::to_string_pretty(schema).ok())
                .map_or(0, |schema| schema.lines().count());
            u16::try_from(message.lines().count() + schema_lines + 4).unwrap_or(u16::MAX)
        }
        Modal::Overlay { lines, rich, .. } => {
            let rows = rich.as_ref().map_or(lines.len(), |rich| {
                rich.iter()
                    .map(|row| match row {
                        crate::interactive::events::RefLine::Item { detail, .. } => {
                            if detail.is_empty() { 1 } else { 2 }
                        }
                        _ => 1,
                    })
                    .sum()
            });
            u16::try_from(rows + 2).unwrap_or(u16::MAX)
        }
    };
    content.min(available)
}

/// Where the cursor belongs inside the composer.
///
/// The composer is not focused while a modal that owns the keyboard is open, so
/// no cursor is reported and the modal keeps the whole frame. A question or an
/// MCP form is answered in the composer, which keeps the cursor.
fn cursor_cell(
    state: &UiState,
    composer: Rect,
    scroll: u16,
    cell: crate::interactive::tui::widgets::composer::Cell,
    prefix_width: u16,
) -> Option<(u16, u16)> {
    if state
        .modal
        .as_ref()
        .is_some_and(|modal| !modal.takes_typing())
    {
        return None;
    }
    let visible_row = cell.row.saturating_sub(scroll);
    if cell.row < scroll || visible_row >= composer.height.saturating_sub(2) {
        return None;
    }
    // The box's first row is its top border, so content row 0 is one row down.
    Some((
        composer.x
            + 2
            + cell
                .column
                .saturating_add(if cell.row == 0 { prefix_width } else { 0 })
                .min(composer.width.saturating_sub(5)),
        composer.y + 1 + visible_row,
    ))
}

#[cfg(test)]
mod tests {
    use super::{MAX_COMPOSER_ROWS, composer_rows, live_rows, plan};
    use crate::interactive::events::{AppPhase, Modal, UiState};
    use crate::interactive::input::matching;
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
            open_tools: Vec::new(),
            live_tool_output: Vec::new(),
            modal: None,
            granted_for_run: false,
            queued_input: false,
            queued_count: 0,
            queued_previews: Vec::new(),
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
            thinking: None,
            service_tier: None,
            goal: None,
        }
    }

    #[test]
    /// prime-agent's queued strip sits directly above the composer: one row
    /// per queued message and the hint.
    fn the_queued_strip_sits_above_the_composer() {
        let area = Rect::new(0, 0, 80, 16);
        let mut queued = state(AppPhase::Running);
        queued.queued_previews = vec![
            "Steering: look again".to_owned(),
            "Follow-up: then test".to_owned(),
        ];
        let outline = plan(area, &queued, &Theme::plain());
        let strip = outline.queue.expect("a strip while messages wait");
        assert_eq!(strip.height, 3);
        assert_eq!(strip.y + strip.height, outline.composer.y);
        assert!(
            plan(area, &state(AppPhase::Running), &Theme::plain())
                .queue
                .is_none()
        );
    }

    #[test]
    /// The draft follows the conversation, then keyboard hints and status.
    fn t02_the_editor_precedes_hints_and_status() {
        let area = Rect::new(0, 0, 80, 12);
        let outline = plan(area, &state(AppPhase::Ready), &Theme::plain());
        assert_eq!(
            outline.status.y, 11,
            "status is the last row: the block is anchored to the bottom"
        );
        assert_eq!(outline.status.height, 1);
        assert_eq!(
            outline.composer.height, 4,
            "two content rows inside the rounded box"
        );
        assert_eq!(
            outline.composer.y, 6,
            "the rows the frame did not need are slack above the block"
        );
        assert_eq!(
            outline.cursor,
            Some((4, 7)),
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
        assert_eq!(outline.composer.height, 5);
        assert_eq!(outline.status.y, 11);
        assert_eq!(
            outline.composer.y, 5,
            "the editor is anchored to the bottom, not to the conversation"
        );
        assert_eq!(outline.cursor.map(|(_, y)| y), Some(8));
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
        assert_eq!(outline.composer.height, MAX_COMPOSER_ROWS + 2);
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
        state.buffer = "abcdefg".to_owned();
        state.cursor = 7;
        let outline = plan(Rect::new(0, 0, 12, 8), &state, &Theme::plain());
        assert_eq!(
            outline.composer_lines,
            ["abcdef".to_owned(), "g".to_owned()],
            "the first row reserves two cells for the prompt marker"
        );
        assert_eq!(
            outline.cursor,
            Some((3, outline.composer.y + 2)),
            "the cursor uses the shared wrapped rows instead of the unwrapped prompt"
        );
    }

    #[test]
    fn t02_the_live_block_sits_above_the_composer_and_a_modal_replaces_it() {
        let mut state = state(AppPhase::Running);
        state.live_text = "streaming".to_owned();
        let outline = plan(Rect::new(0, 0, 80, 12), &state, &Theme::plain());
        let live = outline.live.expect("a live block while text streams");
        assert_eq!(
            live.y + live.height + 1,
            outline.composer.y,
            "one blank row between the running output and the composer"
        );
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
            scroll: 0,
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

    #[test]
    fn redesign_a_long_draft_cannot_hide_an_approval() {
        let mut state = state(AppPhase::WaitingApproval);
        state.buffer = "draft\n".repeat(20);
        state.cursor = state.buffer.chars().count();
        state.modal = Some(Modal::Approval {
            request_id: "request".into(),
            action: "apply_patch".into(),
            summary: "src/auth.rs".into(),
            workspace: "project".into(),
            scope: "once".into(),
            expires_at: std::time::Instant::now(),
            read_only: false,
            scroll: 0,
        });
        for height in 7..25 {
            let outline = plan(Rect::new(0, 0, 80, height), &state, &Theme::plain());
            assert!(outline.modal.unwrap().height >= 2);
            assert!(outline.cursor.is_none());
        }
    }

    /// The menu belongs to the draft, so it sits directly above the composer and
    /// the composer keeps the cursor: the user is still typing, not browsing.
    #[test]
    fn slash_the_menu_sits_above_the_composer_which_keeps_the_cursor() {
        let mut state = state(AppPhase::Ready);
        state.buffer = "/resu".to_owned();
        state.cursor = 5;
        state.suggestions = matching("/resu");
        let outline = plan(Rect::new(0, 0, 80, 12), &state, &Theme::plain());
        let menu = outline.suggest.expect("the menu has a row");
        assert_eq!(menu.height, 1, "one match, one row");
        assert_eq!(menu.y + menu.height, outline.composer.y);
        assert!(
            outline.cursor.is_some(),
            "the composer is still the focused widget"
        );
    }

    /// A long list takes its rows from the live block, never from the composer, and
    /// never more than its cap: the row the user is choosing from must stay visible.
    #[test]
    fn slash_a_long_list_is_capped_and_the_draft_is_never_squeezed() {
        let mut state = state(AppPhase::Running);
        state.buffer = "/".to_owned();
        state.cursor = 1;
        state.live_text = "streaming".to_owned();
        state.suggestions = matching("/");
        let outline = plan(Rect::new(0, 0, 80, 12), &state, &Theme::plain());
        let menu = outline.suggest.expect("the menu is drawn");
        assert_eq!(menu.height, super::MAX_SUGGEST_ROWS);
        assert_eq!(
            outline.composer.height, 3,
            "the draft keeps its row and its rules"
        );
        let live = outline.live.expect("the live text still has a row");
        assert!(
            live.y + live.height <= menu.y,
            "the live block stays above it"
        );
    }

    /// A panel that owns the keyboard replaces the menu with itself, which is the
    /// same condition the controller checks before letting a key act on the list.
    #[test]
    fn slash_a_panel_hides_the_menu() {
        let mut state = state(AppPhase::Ready);
        state.buffer = "/re".to_owned();
        state.cursor = 3;
        state.suggestions = matching("/re");
        state.modal = Some(Modal::Overlay {
            title: "/help".to_owned(),
            lines: vec!["/help  list these commands".to_owned()],
            rich: None,
            scroll: 0,
        });
        let outline = plan(Rect::new(0, 0, 80, 12), &state, &Theme::plain());
        assert!(outline.modal.is_some());
        assert!(
            outline.suggest.is_none(),
            "a menu nobody can see must not be drawn"
        );
    }
}
