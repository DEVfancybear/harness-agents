//! History rendering: one [`HistoryItem`] becomes the rows that go into the
//! scrollback above the viewport.
//!
//! The rows are produced **before** the insert, because `insert_before` needs the
//! height up front: the renderer wraps the text at the current console width and
//! hands the resulting count to the terminal.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::markdown;
use super::theme::Theme;
use crate::interactive::events::{HistoryItem, RunOutcome, ToolState};
use crate::interactive::view;

/// Render one history item into styled rows.
///
/// The text of every row matches [`view::plain_lines`] for the same item, so the
/// scrollback says exactly what the plain transcript says; only the styling is
/// added. That is what keeps the PTY assertions on `> `, `[tool] `, `[run] `,
/// `[approval] `, `[error] ` and `[info] ` valid on the TUI.
#[must_use]
pub fn render(item: &HistoryItem, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    match item {
        HistoryItem::Banner { lines } => lines
            .iter()
            .map(|line| Line::from(vec![Span::styled(line.clone(), theme.banner(line))]))
            .collect(),
        HistoryItem::User { text } => marker_rows("> ", text, width, theme.user, theme),
        // The app's own continuation keeps the marker a reader can tell apart from their
        // own words, in the same dim style the plain renderer prints `[auto] `.
        HistoryItem::Automatic { text } => marker_rows("[auto] ", text, width, theme.dim, theme),
        HistoryItem::Assistant { text } => markdown::render(text, width, theme),
        HistoryItem::Tool {
            name,
            summary,
            state,
        } => vec![tool_card(name, summary, state.clone(), theme)],
        HistoryItem::Run {
            outcome,
            steps,
            tool_calls,
            elapsed,
        } => vec![run_row(outcome, *steps, *tool_calls, *elapsed, theme)],
        HistoryItem::RunAccepted { input_id } => vec![Line::from(vec![Span::styled(
            view::run_line(&format!("accepted {}", view::short_id(input_id))),
            theme.dim,
        )])],
        HistoryItem::Error { message } => vec![Line::from(vec![Span::styled(
            format!("[error] {message}"),
            theme.error,
        )])],
        HistoryItem::Message { text } => vec![Line::from(Span::raw(text.clone()))],
        HistoryItem::Notice { message } => vec![Line::from(vec![Span::styled(
            format!("[info] {message}"),
            theme.dim,
        )])],
        HistoryItem::Approval {
            action,
            summary,
            workspace,
            scope,
            request_id,
        } => view::approval_lines(action, summary, workspace, scope, request_id)
            .into_iter()
            .map(|line| Line::from(vec![Span::styled(line, theme.dim)]))
            .collect(),
        HistoryItem::ApprovalResolution { label, request_id } => vec![Line::from(vec![
            Span::styled(
                format!("[approval] {label} "),
                if label == "granted" {
                    theme.tool_ok
                } else {
                    theme.tool_failed
                },
            ),
            Span::raw(request_id.clone()),
        ])],
        HistoryItem::Sessions { lines } => lines
            .iter()
            .map(|line| Line::from(Span::raw(line.clone())))
            .collect(),
    }
}

/// The plain user rows: the marker only precedes the first row.
fn marker_rows(
    marker: &str,
    text: &str,
    width: u16,
    style: Style,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        let head = if index == 0 {
            marker.to_owned()
        } else {
            String::new()
        };
        let mut spans = Vec::new();
        if !head.is_empty() {
            spans.push(Span::styled(
                head,
                theme.accent.add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::styled(line.to_owned(), style));
        for row in wrap_spans(spans, width) {
            rows.push(row);
        }
    }
    if rows.is_empty() {
        rows.push(Line::from(Span::styled(marker.to_owned(), theme.accent)));
    }
    rows
}

/// The tool card.
///
/// A started card shows the summary; a settled card shows `ok`/`failed` with the
/// duration the service measured. The plain form is still `[tool] name …`, which
/// is why the name is always present at the start of the row.
#[must_use]
pub fn tool_card(name: &str, summary: &str, state: ToolState, theme: &Theme) -> Line<'static> {
    let (status, style, detail) = match state {
        ToolState::Started => ("…".to_owned(), theme.dim, None),
        ToolState::Ok { elapsed } => (
            format!("ok {}", view::seconds_label(elapsed)),
            theme.tool_ok,
            None,
        ),
        ToolState::Failed { elapsed, detail } => (
            format!("failed {}", view::seconds_label(elapsed)),
            theme.tool_failed,
            (!detail.trim().is_empty()).then(|| detail.clone()),
        ),
    };
    let mut spans = vec![
        Span::styled("● ".to_owned(), theme.accent),
        Span::styled(format!("[tool] {name}"), style.add_modifier(Modifier::BOLD)),
    ];
    if !summary.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(summary.to_owned(), theme.dim));
    }
    spans.push(Span::raw("  "));
    spans.push(Span::styled(status, style));
    // The reason is dim like the summary: the card's colour already says it failed, and
    // a reader has to be able to tell a malformed call from a policy denial.
    if let Some(detail) = detail {
        spans.push(Span::styled(format!(" · {detail}"), theme.dim));
    }
    Line::from(spans)
}

/// The end-of-turn row, including the summary the plain renderer omits.
#[must_use]
pub fn run_row(
    outcome: &RunOutcome,
    steps: u32,
    tool_calls: u32,
    elapsed: std::time::Duration,
    theme: &Theme,
) -> Line<'static> {
    let style = match outcome {
        RunOutcome::Done => theme.tool_ok,
        // Neither a cancel nor a bound is a red line: nothing broke, and what the turn
        // did is durable.
        RunOutcome::Canceled | RunOutcome::Paused(_) => theme.dim,
        RunOutcome::Failed(_) => theme.tool_failed,
    };
    Line::from(vec![
        Span::styled(
            view::run_line(&outcome.label()),
            style.add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " · {steps} steps · {tool_calls} tool calls · {}",
                view::seconds_label(elapsed)
            ),
            theme.dim,
        ),
    ])
}

/// Break spans into rows of at most `width` cells.
///
/// Width is measured in terminal cells after NFC, so Vietnamese text cannot push
/// a row past the console width.
fn wrap_spans(spans: Vec<Span<'static>>, width: u16) -> Vec<Line<'static>> {
    let limit = usize::from(width.max(1));
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut used = 0_usize;
    for span in spans {
        let style = span.style;
        let mut chunk = String::new();
        for character in span.content.chars() {
            let cells = super::widgets::composer::char_width(character);
            if used + cells > limit {
                if !chunk.is_empty() {
                    current.push(Span::styled(std::mem::take(&mut chunk), style));
                }
                rows.push(Line::from(std::mem::take(&mut current)));
                used = 0;
            }
            chunk.push(character);
            used += cells;
        }
        if !chunk.is_empty() {
            current.push(Span::styled(chunk, style));
        }
    }
    if !current.is_empty() || rows.is_empty() {
        rows.push(Line::from(current));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::{render, wrap_spans};
    use crate::interactive::events::{HistoryItem, RunOutcome, ToolState};
    use crate::interactive::tui::markdown::plain_text;
    use crate::interactive::tui::theme::Theme;
    use crate::interactive::view;
    use ratatui::text::{Line, Span};
    use std::time::Duration;

    /// U20 in the TUI: the visible text of a history row is the plain line.
    #[test]
    fn t04_history_rows_repeat_the_plain_text() {
        let theme = Theme::plain();
        let items = vec![
            HistoryItem::User {
                text: "sửa lỗi parser".to_owned(),
            },
            HistoryItem::Tool {
                name: "read_file".to_owned(),
                summary: "path=a.rs".to_owned(),
                state: ToolState::Started,
            },
            HistoryItem::Tool {
                name: "read_file".to_owned(),
                summary: String::new(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(12),
                },
            },
            HistoryItem::Error {
                message: "boom".to_owned(),
            },
            HistoryItem::Notice {
                message: "hi".to_owned(),
            },
            HistoryItem::RunAccepted {
                input_id: "input_0192f0aa-bbcc-7ddd-8eee-ffff00001111".to_owned(),
            },
        ];
        for item in items {
            let rows = plain_text(&render(&item, 80, &theme));
            let plain = view::plain_lines(&item);
            // A settled tool card is one styled card, so its plain lines are not
            // one row; every other item must still carry its exact plain line.
            for line in &plain {
                let carried = rows.contains(line.as_str())
                    || line.split_whitespace().all(|part| rows.contains(part));
                assert!(
                    carried,
                    "the TUI row must carry the plain line {line:?}, got:\n{rows}"
                );
            }
        }
    }

    #[test]
    fn t04_a_settled_tool_card_carries_its_duration() {
        let theme = Theme::plain();
        let card = render(
            &HistoryItem::Tool {
                name: "apply_patch".to_owned(),
                summary: String::new(),
                state: ToolState::Failed {
                    elapsed: Duration::from_millis(3100),
                    detail: String::new(),
                },
            },
            80,
            &theme,
        );
        let text = plain_text(&card);
        assert!(text.contains("[tool] apply_patch"), "{text}");
        assert!(text.contains("failed 3.1s"), "{text}");
    }

    /// The card says why it failed, next to the duration the service measured.
    #[test]
    fn t04_a_failed_tool_card_carries_the_reason() {
        let card = render(
            &HistoryItem::Tool {
                name: "list_files".to_owned(),
                summary: "path=".to_owned(),
                state: ToolState::Failed {
                    elapsed: Duration::from_millis(962),
                    detail: "invalid_payload: optional tool path must not be blank".to_owned(),
                },
            },
            120,
            &Theme::plain(),
        );
        let text = plain_text(&card);
        assert!(text.contains("failed 962ms"), "{text}");
        assert!(
            text.contains("invalid_payload: optional tool path must not be blank"),
            "the reader has to be able to tell a malformed call from a denial: {text}"
        );
    }

    #[test]
    fn t04_the_run_row_includes_the_summary() {
        let row = render(
            &HistoryItem::Run {
                outcome: RunOutcome::Done,
                steps: 3,
                tool_calls: 2,
                elapsed: Duration::from_millis(14_200),
            },
            80,
            &Theme::plain(),
        );
        let text = plain_text(&row);
        assert!(text.starts_with("[run] done"), "{text}");
        assert!(text.contains("3 steps · 2 tool calls · 14.2s"), "{text}");
    }

    #[test]
    fn t04_wrapping_measures_cells_not_bytes() {
        let rows = wrap_spans(vec![Span::raw("ệệệệ")], 2);
        assert_eq!(
            rows.len(),
            2,
            "four one-cell characters wrap at two per row"
        );
        let line: Line<'static> = rows[0].clone();
        assert_eq!(plain_text(&[line]), "ệệ");
    }
}
