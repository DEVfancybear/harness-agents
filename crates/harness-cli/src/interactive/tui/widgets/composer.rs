//! The composer: a multi-row input box at the bottom of the viewport.
//!
//! Width is measured in **terminal cells after NFC**, so a Vietnamese character
//! moves the cursor by the cells it really occupies and a decomposed sequence
//! cannot shift it. Rows are wrapped at the current console width; when the draft
//! needs more rows than the viewport allows, the box scrolls internally instead of
//! growing (T01 measured that the viewport height cannot change while the app
//! runs).
//!
//! The first row always starts with the phase marker (`> ` or `.. `): that is the
//! D5 text landmark the PTY acceptance cases assert on.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_normalization::UnicodeNormalization;
use unicode_width::UnicodeWidthChar;

use super::super::layout::Plan;
use super::super::markdown;
use super::super::theme::Theme;
use crate::interactive::events::{Modal, UiState};
use crate::interactive::view;

/// Cells one character occupies, after NFC.
#[must_use]
pub fn char_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}

/// Cells a string occupies, after NFC.
#[must_use]
pub fn display_width(text: &str) -> usize {
    // Normalize the whole string once. Normalizing every scalar separately both
    // allocates per character and cannot compose a base character with the
    // combining marks that follow it.
    text.nfc().map(char_width).sum()
}

/// Where a character index lands after wrapping at `width` cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cell {
    pub row: u16,
    pub column: u16,
}

/// Wrap one draft into rows of at most `width` cells, tracking the cursor.
///
/// The T03 tests use this un-prefixed form to pin the wrapping arithmetic; the
/// frame itself calls `wrap_with_prefix`, because the marker is part of row zero.
#[cfg(test)]
#[must_use]
pub fn wrap(buffer: &str, cursor: usize, width: u16) -> (Vec<String>, Cell) {
    wrap_with_prefix(buffer, cursor, width, 0)
}

/// Wrap a draft while reserving cells for the marker on the first row.
///
/// Layout and rendering call this once per frame and share its result. Later
/// rows get the full width; only row zero pays for `> ` / `.. `.
#[must_use]
pub fn wrap_with_prefix(
    buffer: &str,
    cursor: usize,
    width: u16,
    first_row_prefix: u16,
) -> (Vec<String>, Cell) {
    let limit = usize::from(width.max(1));
    let first_limit = usize::from(width.saturating_sub(first_row_prefix).max(1));
    let mut rows: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut used = 0_usize;
    let mut cursor_cell = Cell { row: 0, column: 0 };
    let mut characters = 0_usize;
    for character in buffer.nfc() {
        let index = characters;
        if index == cursor {
            cursor_cell = Cell {
                row: u16::try_from(rows.len()).unwrap_or(u16::MAX),
                column: u16::try_from(used).unwrap_or(u16::MAX),
            };
        }
        if character == '\n' {
            rows.push(std::mem::take(&mut current));
            used = 0;
            characters += 1;
            continue;
        }
        let cells = char_width(character);
        let row_limit = if rows.is_empty() { first_limit } else { limit };
        if used > 0 && used + cells > row_limit {
            rows.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push(character);
        used += cells;
        characters += 1;
    }
    let row_limit = if rows.is_empty() { first_limit } else { limit };
    if used >= row_limit && cursor >= characters {
        rows.push(std::mem::take(&mut current));
        used = 0;
    }
    if cursor >= characters {
        cursor_cell = Cell {
            row: u16::try_from(rows.len()).unwrap_or(u16::MAX),
            column: u16::try_from(used).unwrap_or(u16::MAX),
        };
    }
    rows.push(current);
    (rows, cursor_cell)
}

/// Draw the composer box.
pub fn render(frame: &mut Frame, plan: &Plan, state: &UiState, theme: &Theme) {
    if plan.composer.height == 0 {
        return;
    }
    let prefix = view::prompt_prefix(state.phase);
    // The rules are the first and last rows of the box, so the content starts one
    // row down and the scroll offset counts content rows only.
    let visible_rows = plan.composer.height.saturating_sub(2);
    let visible: Vec<&String> = plan
        .composer_lines
        .iter()
        .skip(usize::from(plan.composer_scroll))
        .take(usize::from(visible_rows))
        .collect();

    let mut lines: Vec<Line<'static>> = Vec::new();
    // One run per row, marker included. A styled marker would make ratatui emit a
    // cursor move between the marker and the draft, and the transcript - which is
    // the evidence the PTY acceptance cases read - would then contain `> ` and the
    // draft as two separate writes instead of the D5 landmark `> text`.
    if state.buffer.is_empty() {
        // The placeholder is one span, so the marker and the hint stay adjacent.
        lines.push(Line::from(Span::styled(
            format!("{prefix}Nhập yêu cầu"),
            theme.dim.add_modifier(Modifier::ITALIC),
        )));
    } else {
        for (index, row) in visible.iter().enumerate() {
            let absolute = index + usize::from(plan.composer_scroll);
            let body = if absolute == 0 {
                format!("{prefix}{row}")
            } else {
                (*row).clone()
            };
            lines.push(Line::from(body));
        }
    }
    let border_style = if state.modal.is_some() {
        theme.border
    } else {
        theme.composer_border
    };
    // prime-agent's editor sits between two plain rules; a draft taller than the
    // box says how many rows are scrolled out of view, on the rule.
    let mut block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(border_style);
    if plan.composer_scroll > 0 {
        block = block.title(Span::styled(
            format!("─── ↑ {} more ", plan.composer_scroll),
            theme.border,
        ));
    }
    // A panel or the menu owns the keyboard, so its keys go on the lower rule, the
    // way prime-agent's selectors name theirs; the idle editor stays bare.
    if state.modal.is_some() || !state.suggestions.is_empty() {
        block = block.title_bottom(Span::styled(hint(state), theme.dim));
    }
    frame.render_widget(Paragraph::new(lines).block(block), plan.composer);
}

/// The short hint shown on the composer's lower rule while a panel or the menu is up.
///
/// The composer is not focused while a panel owns the keyboard, so the hint names
/// the panel and the keys that close it instead of describing the composer's own
/// keys - which would not work while the panel is up.
#[must_use]
pub fn hint(state: &UiState) -> String {
    match &state.modal {
        // Every panel offers the same three answers, because `a` now covers every
        // kind: naming it only where it used to work would hide the very key that
        // ends the interruptions.
        Some(Modal::Approval { .. }) => " DUYỆT · y chạy · a cả lượt · n từ chối ".to_owned(),
        Some(Modal::Picker { .. }) => " PHIÊN · ↑↓ chọn · Enter · Esc ".to_owned(),
        Some(Modal::FilePicker { .. }) => " TỆP · ↑↓ chọn · Enter · Esc ".to_owned(),
        Some(Modal::Question { options, .. }) => {
            if options.is_empty() {
                " TRẢ LỜI · Enter gửi ".to_owned()
            } else {
                " TRẢ LỜI · phím số hoặc nhập · Enter ".to_owned()
            }
        }
        Some(Modal::McpElicitation {
            requested_schema, ..
        }) => {
            if requested_schema.is_some() {
                " MCP INPUT · JSON · decline · cancel · Enter gửi ".to_owned()
            } else {
                " MCP URL · done · decline · cancel · Enter gửi ".to_owned()
            }
        }
        Some(Modal::Overlay { .. }) => " THAM KHẢO · PgUp/PgDn · Esc đóng ".to_owned(),
        None => {
            // The menu is what the user is looking at while it is up, so its keys
            // are what the border names - even during a run, where the run's own
            // keys would otherwise be the only thing the border said.
            if !state.suggestions.is_empty() {
                return format!(
                    " LỆNH · ↑↓ chọn · Tab/Enter nhận · Esc · {} mục ",
                    state.suggestions.len()
                );
            }
            if state.phase.has_active_run() {
                return " ĐANG CHẠY · Ctrl-C hủy · nhập để xếp hàng ".to_owned();
            }
            " SOẠN THẢO · Enter gửi · Ctrl-J xuống dòng · /help ".to_owned()
        }
    }
}

/// Draw the live block: model text that has not been committed yet.
pub fn render_live(frame: &mut Frame, area: Rect, state: &UiState, theme: &Theme) {
    if area.height == 0 || (state.live_text.is_empty() && state.open_tool.is_none()) {
        return;
    }
    let mut lines = markdown::render(&state.live_text, area.width, theme);
    if state.live_text.is_empty() {
        lines.clear();
    }
    if let Some((name, summary)) = &state.open_tool {
        lines.push(super::super::history::running_card(
            name,
            summary,
            &crate::interactive::events::ToolState::Started,
            state.tick,
            theme,
        ));
    }
    let start = lines.len().saturating_sub(usize::from(area.height));
    let visible: Vec<Line<'static>> = lines[start..].to_vec();
    frame.render_widget(Paragraph::new(visible).wrap(Wrap { trim: false }), area);
}

#[cfg(test)]
mod tests {
    use super::{Cell, char_width, display_width, hint, wrap};
    use crate::interactive::events::{AppPhase, Modal, UiState};
    use crate::interactive::tui::theme::Theme;
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
        }
    }

    #[test]
    fn t03_vietnamese_text_is_measured_in_cells_after_nfc() {
        assert_eq!(
            char_width('ệ'),
            1,
            "a composed Vietnamese letter is one cell"
        );
        // Decomposed "e" + combining circumflex + combining dot: NFC folds it.
        assert_eq!(display_width("e\u{0302}\u{0323}"), 1);
        assert_eq!(display_width("sửa lỗi"), 7);
        assert_eq!(char_width('日'), 2, "a wide character is two cells");
    }

    #[test]
    fn t03_wrapping_places_the_cursor_on_the_right_row_and_column() {
        let (rows, cursor) = wrap("abcdef", 0, 4);
        assert_eq!(rows, vec!["abcd".to_owned(), "ef".to_owned()]);
        assert_eq!(cursor, Cell { row: 0, column: 0 });

        let (_, cursor) = wrap("abcdef", 5, 4);
        assert_eq!(cursor, Cell { row: 1, column: 1 });

        let (_, cursor) = wrap("abcdef", 6, 4);
        assert_eq!(
            cursor,
            Cell { row: 1, column: 2 },
            "the end of the text is a cursor position too"
        );
    }

    #[test]
    fn t03_wrapping_honours_explicit_line_breaks_and_wide_characters() {
        let (rows, cursor) = wrap("ab\ncd", 5, 10);
        assert_eq!(rows, vec!["ab".to_owned(), "cd".to_owned()]);
        assert_eq!(cursor, Cell { row: 1, column: 2 });

        let (rows, _) = wrap("日本語", 3, 4);
        assert_eq!(
            rows,
            vec!["日本".to_owned(), "語".to_owned()],
            "two wide characters fill a four-cell row"
        );
    }

    /// The border names the keys that work *now*. While the menu is up those are
    /// the menu's keys - even during a run, where the run's own keys would
    /// otherwise be the only thing the border said.
    #[test]
    fn t03_the_hint_follows_the_phase_the_menu_and_the_completion() {
        let mut idle = state(AppPhase::Ready);
        assert!(hint(&idle).contains("Ctrl-J"));

        idle.buffer = "/res".to_owned();
        idle.cursor = 4;
        idle.suggestions = crate::interactive::input::matching("/res");
        let menu = hint(&idle);
        assert!(
            menu.contains("Tab/Enter") && menu.contains("Esc"),
            "the menu's own keys: {menu}"
        );
        assert!(
            menu.contains('1'),
            "the count says how many matched: {menu}"
        );

        let running = state(AppPhase::Running);
        assert!(hint(&running).contains("Ctrl-C"));
    }

    #[test]
    fn t03_the_marker_style_comes_from_the_theme() {
        assert_eq!(Theme::plain().accent.fg, None);
        assert!(Theme::colored().accent.fg.is_some());
    }

    /// Seen on a real screen: the hint said "answer the panel above" whatever the panel
    /// was, which named no key and read as if the panel were somewhere else.
    #[test]
    fn t03_the_hint_names_the_keys_of_the_panel_that_is_open() {
        let mut asking = state(AppPhase::WaitingApproval);
        asking.modal = Some(Modal::Approval {
            request_id: "req-1".to_owned(),
            action: "apply_patch".to_owned(),
            summary: "path=a.rs".to_owned(),
            workspace: "C:/w".to_owned(),
            scope: "once".to_owned(),
            expires_at: std::time::Instant::now(),
            read_only: false,
            scroll: 0,
        });
        let approval = hint(&asking);
        assert!(
            approval.contains('y') && approval.contains('n'),
            "{approval}"
        );
        assert!(
            !approval.contains("Ctrl-J"),
            "the composer's own keys do not work while a panel is up: {approval}"
        );

        let mut picker = state(AppPhase::Ready);
        picker.modal = Some(Modal::Picker {
            items: vec!["one".to_owned()],
            selected: 0,
        });
        assert!(hint(&picker).contains("Esc"), "{}", hint(&picker));
    }
}
