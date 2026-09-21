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
    /// Whether the action only reads. The panel says so next to the proposal: it no
    /// longer decides which keys are offered - `a` covers every kind - but a reader
    /// still has the right to know whether the thing in front of them can write.
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
    let mut header = vec![Span::styled(
        format!("[approval] {}: {}", request.action, request.summary),
        theme.tool_ok,
    )];
    if request.read_only {
        header.push(Span::styled(" · chỉ đọc".to_owned(), theme.dim));
    }
    header.push(Span::styled(
        format!("  (còn {})", view::clock_label(remaining)),
        theme.dim,
    ));
    let mut lines = vec![Line::from(header)];
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
    // `a` is offered on every panel, read-only or not, and it says exactly how far
    // it reaches: from here on this turn runs without asking, including file writes
    // and commands. A key whose text promised less than it did would be worse than
    // no key at all.
    lines.push(Line::from(vec![Span::styled(
        "a cho phép mọi thao tác trong lượt này (kể cả ghi file và chạy lệnh)".to_owned(),
        theme.dim,
    )]));
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

    /// Measured complaint: a turn of `git log`, `git status`, `git diff` asked about
    /// every command, and the panel offered no key that ended the questions. The `a`
    /// row is now on every panel, and it says how far it reaches - including writes
    /// and commands - so the key cannot do more than its own text promises.
    #[test]
    fn t06_every_panel_offers_the_turn_grant_and_says_how_far_it_reaches() {
        let text = plain_text(&rows(&request(Duration::from_mins(5)), &Theme::plain()));
        assert!(
            text.contains("a cho phép mọi thao tác trong lượt này (kể cả ghi file và chạy lệnh)"),
            "a write panel offers the turn grant and names what it covers: {text}"
        );

        let read = plain_text(&rows(
            &Proposal {
                read_only: true,
                ..request(Duration::from_mins(5))
            },
            &Theme::plain(),
        ));
        assert!(
            read.contains("a cho phép mọi thao tác trong lượt này"),
            "and so does a read panel: {read}"
        );
        assert!(
            read.contains("· chỉ đọc"),
            "a read-only proposal still says so, even though the key no longer depends on it: {read}"
        );
        assert!(
            !text.contains("· chỉ đọc"),
            "a patch is not marked read-only: {text}"
        );
    }
}
