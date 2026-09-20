//! The approval panel.
//!
//! It shows exactly what the gate proposed - action, summary, workspace, scope and
//! request id - and counts down to the deadline the gate itself reported. The
//! timeout is never duplicated here: the panel reads `expires_at` from the event,
//! so a change to the gate's timeout cannot leave the UI lying about it.

use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::super::theme::Theme;
use crate::interactive::view;

/// Rows the panel needs, borders included.
///
/// The layout reserves exactly this, so a row added here without updating it would
/// be clipped off the bottom of the viewport instead of wrapping into view.
pub const PANEL_ROWS: u16 = 7;

/// Everything the panel shows about one pending request.
#[derive(Clone, Copy, Debug)]
pub struct Proposal<'a> {
    pub request_id: &'a str,
    pub action: &'a str,
    pub summary: &'a str,
    pub workspace: &'a str,
    pub scope: &'a str,
    pub expires_at: Instant,
    /// Whether the action only reads. The wider grant is offered only here, because
    /// it can only ever cover read-only actions - offering it on a write would name
    /// a key that does less than it says.
    pub read_only: bool,
}

/// Draw the panel.
pub fn render(frame: &mut Frame, area: Rect, request: Proposal<'_>, theme: &Theme) {
    let lines = rows(&request, theme);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.tool_ok)
        .title(Span::styled(" approval ", theme.title));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The panel rows.
///
/// The first row repeats the plain `approval_lines` header so the text landmark
/// `[approval] ` survives on screen while the panel is open.
#[must_use]
pub fn rows(request: &Proposal<'_>, theme: &Theme) -> Vec<Line<'static>> {
    let remaining = request.expires_at.saturating_duration_since(Instant::now());
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("[approval] {}: {}", request.action, request.summary),
            theme.tool_ok,
        ),
        Span::styled(
            format!("  (còn {})", view::clock_label(remaining)),
            theme.dim,
        ),
    ])];
    lines.push(Line::from(vec![
        Span::styled("workspace: ".to_owned(), theme.dim),
        Span::raw(request.workspace.to_owned()),
    ]));
    lines.push(Line::from(vec![
        Span::styled("scope: ".to_owned(), theme.dim),
        Span::raw(format!(
            "{} (request {})",
            request.scope, request.request_id
        )),
    ]));
    lines.push(Line::from(vec![Span::styled(
        "y chạy một lần · n từ chối · hết hạn thì không chạy".to_owned(),
        theme.dim,
    )]));
    // The wider grant is named only when it would cover something, and it is named
    // last so the two keys that always exist keep the position they had.
    if request.read_only {
        lines.push(Line::from(vec![Span::styled(
            "a cho phép mọi thao tác chỉ-đọc trong lượt này".to_owned(),
            theme.dim,
        )]));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::{Proposal, rows};
    use crate::interactive::tui::markdown::plain_text;
    use crate::interactive::tui::theme::Theme;
    use std::time::{Duration, Instant};

    fn request(expires_in: Duration) -> Proposal<'static> {
        Proposal {
            request_id: "req-1",
            action: "apply_patch",
            summary: "path=src/parser.rs",
            workspace: "C:/work/project",
            scope: "once",
            expires_at: Instant::now() + expires_in,
            read_only: false,
        }
    }

    #[test]
    fn t06_the_panel_names_the_action_scope_and_request() {
        let text = plain_text(&rows(&request(Duration::from_mins(5)), &Theme::plain()));
        assert!(
            text.contains("[approval] apply_patch: path=src/parser.rs"),
            "{text}"
        );
        assert!(text.contains("workspace: C:/work/project"), "{text}");
        assert!(text.contains("scope: once (request req-1)"), "{text}");
        assert!(text.contains("y chạy một lần"), "{text}");
    }

    #[test]
    fn t06_the_countdown_comes_from_the_event_deadline() {
        let text = plain_text(&rows(&request(Duration::from_secs(30)), &Theme::plain()));
        assert!(
            text.contains("còn 00:30") || text.contains("còn 00:29"),
            "the countdown reflects the reported deadline: {text}"
        );
    }
}
