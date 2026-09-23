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
pub const PANEL_ROWS: u16 = 48;

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
    pub scroll: usize,
}

/// Draw the panel.
pub fn render(frame: &mut Frame, area: Rect, request: Proposal<'_>, theme: &Theme) {
    let lines = rows(&request, theme);
    // At the minimum supported console height the inline viewport gives this
    // panel only two rows. A full box would consume both with borders and hide
    // the proposed action entirely; the composer and status bar still show the
    // decision keys and countdown below this compact form.
    if area.height <= 3 {
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .take(usize::from(area.height))
                    .collect::<Vec<_>>(),
            ),
            area,
        );
        return;
    }
    let visible_rows = usize::from(area.height.saturating_sub(2));
    let max_scroll = lines.len().saturating_sub(visible_rows);
    let scroll = request.scroll.min(max_scroll);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.warning)
        .title(Span::styled(" DUYỆT HÀNH ĐỘNG ", theme.warning));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
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
    let (summary, confirmation, diff) =
        if let Some((summary, rest)) = request.summary.split_once("\n[always-allow]\n") {
            let (confirmation, diff) = rest
                .split_once("\n[diff]\n")
                .map_or((rest, None), |(confirmation, diff)| {
                    (confirmation, Some(diff))
                });
            (summary, Some(confirmation), diff)
        } else {
            let (summary, diff) = request
                .summary
                .split_once("\n[diff]\n")
                .map_or((request.summary, None), |(summary, diff)| {
                    (summary, Some(diff))
                });
            (summary, None, diff)
        };
    let mut header = vec![Span::styled(
        format!("[approval] {}: {summary}", request.action),
        theme.warning,
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
        "a cho phép mọi thao tác trong lượt này · A đề xuất rule lâu dài, Enter để xác nhận"
            .to_owned(),
        theme.dim,
    )]));
    if let Some(confirmation) = confirmation {
        lines.extend(
            confirmation
                .lines()
                .map(|line| Line::from(Span::styled(line.to_owned(), theme.dim))),
        );
    }
    if let Some(diff) = diff {
        lines.push(Line::from(Span::styled("[diff]".to_owned(), theme.dim)));
        lines.extend(diff.lines().map(|line| {
            let style = if line.starts_with('+') {
                theme.tool_ok
            } else if line.starts_with('-') {
                theme.tool_failed
            } else {
                theme.dim
            };
            Line::from(Span::styled(line.to_owned(), style))
        }));
    }
    lines
}

/// Height requested by the panel, including its border and fixed approval rows.
#[must_use]
pub fn requested_rows(summary: &str) -> u16 {
    let confirmation_lines = summary
        .split_once("\n[always-allow]\n")
        .map_or(0, |(_, rest)| {
            rest.split_once("\n[diff]\n")
                .map_or(rest.lines().count(), |(confirmation, _)| {
                    confirmation.lines().count()
                })
        });
    let diff_lines = summary
        .rsplit_once("\n[diff]\n")
        .map_or(0, |(_, diff)| diff.lines().count().saturating_add(1));
    u16::try_from(
        7_usize
            .saturating_add(confirmation_lines)
            .saturating_add(diff_lines),
    )
    .unwrap_or(PANEL_ROWS)
    .min(PANEL_ROWS)
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
            scroll: 0,
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

    /// The panel distinguishes a grant for this turn from a persistent rule, and
    /// tells the user that Enter confirms the proposed long-term pattern.
    #[test]
    fn t06_every_panel_offers_turn_and_persistent_grants_with_confirmation() {
        let text = plain_text(&rows(&request(Duration::from_mins(5)), &Theme::plain()));
        assert!(
            text.contains("a cho phép mọi thao tác trong lượt này"),
            "a write panel offers the turn grant: {text}"
        );
        assert!(
            text.contains("A đề xuất rule lâu dài, Enter để xác nhận"),
            "the persistent rule requires explicit confirmation: {text}"
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
            read.contains("A đề xuất rule lâu dài, Enter để xác nhận"),
            "the read panel describes the same confirmation step: {read}"
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
