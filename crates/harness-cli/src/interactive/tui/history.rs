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
use crate::interactive::events::{Detail, HistoryItem, RunOutcome, ToolState};
use crate::interactive::view;

/// How many lines of a tool's output a collapsed panel shows (prime-agent's
/// `tool-execution.ts`).
const TOOL_OUTPUT_PREVIEW_LINES: usize = 3;

/// Render one history item into styled rows, the way prime-agent's interactive
/// mode draws its chat (`modes/interactive/components`): a user message is a box on
/// `userMessageBg`, assistant prose is markdown with one cell of padding, a tool is
/// a panel on `toolPanelBg` headed `label · status`, its output collapsed to three
/// lines, and notices, errors and injected prompts are single styled lines.
///
/// The plain renderer keeps its own transcript format ([`view::plain_lines`]); this
/// is only how the TUI shows the same items.
#[must_use]
pub fn render(item: &HistoryItem, width: u16, theme: &Theme, detail: Detail) -> Vec<Line<'static>> {
    match item {
        HistoryItem::Banner { lines } => banner_rows(lines, width, theme),
        HistoryItem::User { text } => user_box(text, width, theme),
        HistoryItem::Automatic { text } => injected_prompt(text, width, theme),
        HistoryItem::Assistant { text } => {
            let mut rows = vec![Line::default()];
            rows.extend(padded(markdown::render(
                text,
                width.saturating_sub(2),
                theme,
            )));
            rows
        }
        // Collapsed mode hides reasoning, as prime-agent's overview does.
        HistoryItem::Thinking { .. } if detail == Detail::Collapsed => Vec::new(),
        HistoryItem::Thinking { text } => {
            // prime-agent shows reasoning as dim markdown, with no label.
            let dim = Theme {
                assistant: theme.dim,
                md_code: theme.dim,
                md_code_block: theme.dim,
                md_heading: theme.dim,
                ..*theme
            };
            padded(markdown::render(text, width.saturating_sub(2), &dim))
        }
        HistoryItem::Tool {
            name,
            summary,
            state,
        } => {
            let mut rows = vec![Line::default()];
            rows.push(tool_card(name, summary, state, theme));
            if let ToolState::Failed { detail, .. } = state
                && !detail.trim().is_empty()
            {
                rows.extend(panel_rows(
                    &format!("  {detail}"),
                    width,
                    theme.error,
                    theme,
                ));
            }
            rows
        }
        HistoryItem::ToolOutput { text, .. } => tool_output_rows(
            text,
            width,
            theme,
            if detail == Detail::Expanded {
                usize::MAX
            } else {
                TOOL_OUTPUT_PREVIEW_LINES
            },
        ),
        HistoryItem::Run {
            outcome,
            steps,
            tool_calls,
            elapsed,
            // A failed turn's reason can be long; it wraps rather than being cut off.
        } => wrap_spans(
            run_row(outcome, *steps, *tool_calls, *elapsed, theme).spans,
            width,
        ),
        // prime-agent shows no row for an admitted input: the user box is the record.
        HistoryItem::RunAccepted { .. } => Vec::new(),
        HistoryItem::Error { message } => error_rows(message, width, theme),
        HistoryItem::Message { text } => vec![Line::from(Span::raw(text.clone()))],
        HistoryItem::Notice { message } => notice_rows(message, width, theme),
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
        HistoryItem::ApprovalResolution { label, request_id } => {
            let granted = label == "granted";
            vec![Line::from(vec![
                Span::styled(
                    if granted { "✓ " } else { "✗ " },
                    if granted {
                        theme.tool_ok
                    } else {
                        theme.tool_failed
                    },
                ),
                Span::styled(format!("approval {label} "), theme.muted),
                Span::styled(request_id.clone(), theme.dim),
            ])]
        }
        HistoryItem::Sessions { lines } => lines
            .iter()
            .map(|line| Line::from(Span::raw(line.clone())))
            .collect(),
    }
}

/// Indent rendered rows by one cell, as prime-agent pads assistant text.
fn padded(rows: Vec<Line<'static>>) -> Vec<Line<'static>> {
    rows.into_iter()
        .map(|line| {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(line.spans);
            Line::from(spans).style(line.style)
        })
        .collect()
}

/// prime-agent's `UserMessageComponent`: a box on `userMessageBg`, two cells of
/// padding on each side and one row above and below.
fn user_box(text: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(4).max(1);
    let mut rows = vec![Line::default(), Line::default().style(theme.user_box)];
    for line in text.split('\n') {
        for row in wrap_spans(vec![Span::styled(line.to_owned(), theme.user_box)], inner) {
            let mut spans = vec![Span::styled("  ", theme.user_box)];
            spans.extend(row.spans);
            rows.push(Line::from(spans).style(theme.user_box));
        }
    }
    rows.push(Line::default().style(theme.user_box));
    rows
}

/// prime-agent's injected prompts (`injected-prompt-message.ts`): a heartbeat is
/// `♥ Heartbeat prompt · <schedule>`, a goal continuation `◆ Goal · <objective>`,
/// anything else the app sent by itself `◆ <first line>`.
fn injected_prompt(text: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let first = text.lines().next().unwrap_or_default();
    let (marker, marker_style, label) = if let Some(rest) = first.strip_prefix("[heartbeat: ") {
        let schedule = rest.split(" run#").next().unwrap_or(rest);
        ("♥ ", theme.error, format!("Heartbeat prompt · {schedule}"))
    } else if let Some(objective) = first.strip_prefix("Continue working toward the goal: ") {
        ("◆ ", theme.accent, format!("Goal · {objective}"))
    } else {
        ("◆ ", theme.accent, first.to_owned())
    };
    let mut rows = vec![Line::default()];
    rows.extend(wrap_spans(
        vec![
            Span::styled(marker, marker_style),
            Span::styled(label, theme.muted),
        ],
        width,
    ));
    rows
}

/// prime-agent's `⚠ Error: msg`, after a blank row.
fn error_rows(message: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let mut rows = vec![Line::default()];
    rows.extend(wrap_spans(
        vec![Span::styled(format!("⚠ Error: {message}"), theme.error)],
        width,
    ));
    rows
}

/// A status line in `dim`; a warning (`goal paused`, `refine failed`, ...) in the
/// warning colour with prime-agent's `⚠`, and a refinement in its own colour.
fn notice_rows(message: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let lowered = message.to_ascii_lowercase();
    let spans = if message.starts_with("refine ") {
        let (head, body) = message.split_once(": ").unwrap_or((message, ""));
        let mut spans = vec![
            Span::styled("◆ ", theme.refinement),
            Span::styled(
                head.to_owned(),
                theme.refinement.add_modifier(Modifier::BOLD),
            ),
        ];
        if !body.is_empty() {
            spans.push(Span::styled(format!(" · {body}"), theme.muted));
        }
        spans
    } else if lowered.contains("failed")
        || lowered.contains("could not")
        || lowered.contains("skipped")
    {
        vec![Span::styled(format!("⚠ {message}"), theme.warning)]
    } else {
        vec![Span::styled(message.to_owned(), theme.dim)]
    };
    wrap_spans(spans, width)
}

/// Rows on the tool panel background, padded to the full width.
fn panel_rows(text: &str, width: u16, style: Style, theme: &Theme) -> Vec<Line<'static>> {
    wrap_spans(vec![Span::styled(text.to_owned(), style)], width)
        .into_iter()
        .map(|line| line.style(theme.panel))
        .collect()
}

/// The collapsed body of a tool panel: the first lines of what the tool returned,
/// then `… N more lines` - prime-agent's collapsed tool output.
fn tool_output_rows(text: &str, width: u16, theme: &Theme, shown: usize) -> Vec<Line<'static>> {
    // The first line repeats the tool's name (`read_file:`), which the header says.
    let body = text.split_once(":\n").map_or(text, |(_, body)| body);
    let lines = body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    for line in lines.iter().take(shown) {
        let clipped = clip(line, usize::from(width.saturating_sub(4)));
        rows.push(
            Line::from(vec![Span::raw("  "), Span::styled(clipped, theme.muted)])
                .style(theme.panel),
        );
    }
    let more = lines.len().saturating_sub(shown);
    if more > 0 {
        rows.push(
            Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("… {more} more lines"), theme.dim),
            ])
            .style(theme.panel),
        );
    }
    rows
}

/// One row of at most `cells` cells, cut with `…`.
fn clip(text: &str, cells: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for character in text.chars() {
        let width = super::widgets::composer::char_width(character);
        if used + width > cells.saturating_sub(1) {
            out.push('…');
            return out;
        }
        out.push(character);
        used += width;
    }
    out
}

/// A compact welcome card in scrollback. Keep every original header value visible
/// and wrap long paths before inserting rows; the left rail is only decoration.
fn banner_rows(lines: &[String], width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let rule_width = usize::from(width.saturating_sub(2).min(48));
    if rule_width > 0 {
        rows.push(Line::from(Span::styled(
            format!("  {}", "─".repeat(rule_width)),
            theme.border,
        )));
    }
    for (index, line) in lines.iter().enumerate() {
        let mut spans = vec![Span::styled(
            if index == 0 { "  ◆  " } else { "  │  " },
            if index == 0 {
                theme.accent
            } else {
                theme.border
            },
        )];
        if index == 0 {
            spans.push(Span::styled(line.clone(), theme.title));
        } else if let Some((label, value)) = line.split_once(':') {
            spans.push(Span::styled(format!("{label}:"), theme.muted));
            spans.push(Span::styled(value.to_owned(), theme.dim));
        } else {
            spans.push(Span::styled(line.clone(), theme.dim));
        }
        rows.extend(wrap_spans(spans, width));
    }
    if rule_width > 0 {
        rows.push(Line::from(Span::styled(
            format!("  {}", "─".repeat(rule_width)),
            theme.border,
        )));
    }
    rows
}

/// A tool panel header: prime-agent's `label · status` on `toolPanelBg` - `running`
/// with its diamond marker in `bashMode`, `done` in `success`, `error` in `error` -
/// followed by the arguments and the duration in dim. The Python REPL gets
/// prime-agent's ipython-cell summary: `✓ python · <code> · <duration>`.
#[must_use]
pub fn tool_card(name: &str, summary: &str, state: &ToolState, theme: &Theme) -> Line<'static> {
    running_card(name, summary, state, 1, theme)
}

/// [`tool_card`] at animation frame `tick`: a running tool's diamond pulses, as
/// prime-agent's working icon does.
#[must_use]
pub fn running_card(
    name: &str,
    summary: &str,
    state: &ToolState,
    tick: u64,
    theme: &Theme,
) -> Line<'static> {
    let (marker, status, style, duration) = match state {
        ToolState::Started => (Theme::working(tick), "running", theme.bash, None),
        ToolState::Ok { elapsed } => (
            "✓",
            "done",
            theme.tool_ok,
            Some(view::seconds_label(*elapsed)),
        ),
        ToolState::Failed { elapsed, .. } => (
            "✗",
            "error",
            theme.tool_failed,
            Some(view::seconds_label(*elapsed)),
        ),
    };
    let separator = || Span::styled(" · ", theme.dim);
    let mut spans = vec![Span::raw("  ")];
    if name == "ipython" {
        let code = summary.strip_prefix("code=").unwrap_or(summary);
        spans.push(Span::styled(format!("{marker} "), style));
        spans.push(Span::styled("python", theme.muted));
        if !code.is_empty() {
            spans.push(separator());
            spans.push(Span::styled(code.to_owned(), theme.dim));
        }
    } else {
        spans.push(Span::styled(name.to_owned(), theme.muted));
        spans.push(separator());
        if matches!(state, ToolState::Started) {
            spans.push(Span::styled(format!("{marker} "), style));
        }
        spans.push(Span::styled(status, style));
        if !summary.is_empty() {
            spans.push(separator());
            spans.push(Span::styled(summary.to_owned(), theme.dim));
        }
    }
    if let Some(duration) = duration {
        spans.push(separator());
        spans.push(Span::styled(duration, theme.dim));
    }
    Line::from(spans).style(theme.panel)
}

/// The end-of-turn line: a dim status, as prime-agent's `showStatus`, in the
/// warning colour when the turn stopped short and the error colour when it failed.
#[must_use]
pub fn run_row(
    outcome: &RunOutcome,
    steps: u32,
    tool_calls: u32,
    elapsed: std::time::Duration,
    theme: &Theme,
) -> Line<'static> {
    let style = match outcome {
        RunOutcome::Done => theme.dim,
        RunOutcome::Canceled
        | RunOutcome::Paused(_)
        | RunOutcome::WaitingInput { .. }
        | RunOutcome::ExternalWait => theme.warning,
        RunOutcome::Blocked(_) | RunOutcome::Failed(_) => theme.error,
    };
    Line::from(vec![
        Span::styled(outcome.label(), style),
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
    use crate::interactive::events::{Detail, HistoryItem, RunOutcome, ToolState};
    use crate::interactive::tui::markdown::plain_text;
    use crate::interactive::tui::theme::Theme;
    use ratatui::text::{Line, Span};
    use std::time::Duration;

    /// prime-agent's chat rows: a user box, a tool panel `label · status`, its
    /// output collapsed to three lines, `⚠ Error:`, and no row for an admitted input.
    #[test]
    fn rows_follow_prime_agents_chat() {
        let theme = Theme::plain();
        let user = plain_text(&render(
            &HistoryItem::User {
                text: "sửa lỗi parser".to_owned(),
            },
            40,
            &theme,
            Detail::Collapsed,
        ));
        assert!(user.contains("  sửa lỗi parser"), "{user}");
        let card = plain_text(&render(
            &HistoryItem::Tool {
                name: "read_file".to_owned(),
                summary: "path=a.rs".to_owned(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(12),
                },
            },
            80,
            &theme,
            Detail::Collapsed,
        ));
        assert!(
            card.contains("read_file · done · path=a.rs · 12ms"),
            "{card}"
        );
        let output = plain_text(&render(
            &HistoryItem::ToolOutput {
                name: "read_file".to_owned(),
                text: "read_file:\none\ntwo\nthree\nfour\nfive".to_owned(),
            },
            80,
            &theme,
            Detail::Collapsed,
        ));
        assert_eq!(output, "  one\n  two\n  three\n  … 2 more lines");
        let error = plain_text(&render(
            &HistoryItem::Error {
                message: "boom".to_owned(),
            },
            80,
            &theme,
            Detail::Collapsed,
        ));
        assert!(error.ends_with("⚠ Error: boom"), "{error}");
        assert!(
            render(
                &HistoryItem::RunAccepted {
                    input_id: "input_1".to_owned()
                },
                80,
                &theme,
                Detail::Collapsed
            )
            .is_empty()
        );
    }

    #[test]
    fn a_failed_tool_card_carries_its_duration_and_reason() {
        let text = plain_text(&render(
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
            Detail::Collapsed,
        ));
        assert!(
            text.contains("list_files · error · path= · 962ms"),
            "{text}"
        );
        assert!(
            text.contains("invalid_payload: optional tool path must not be blank"),
            "{text}"
        );
    }

    /// The Python REPL gets prime-agent's ipython-cell summary.
    #[test]
    fn an_ipython_cell_is_summarised_like_prime_agent() {
        let text = plain_text(&render(
            &HistoryItem::Tool {
                name: "ipython".to_owned(),
                summary: "code=print(1)".to_owned(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(1200),
                },
            },
            80,
            &Theme::plain(),
            Detail::Collapsed,
        ));
        assert!(text.contains("✓ python · print(1) · 1.2s"), "{text}");
    }

    #[test]
    fn injected_prompts_and_the_run_row_are_status_lines() {
        let theme = Theme::plain();
        let beat = plain_text(&render(
            &HistoryItem::Automatic {
                text: "[heartbeat: every 5m run#2]\n\ncheck ci".to_owned(),
            },
            80,
            &theme,
            Detail::Collapsed,
        ));
        assert!(beat.contains("♥ Heartbeat prompt · every 5m"), "{beat}");
        let goal = plain_text(&render(
            &HistoryItem::Automatic {
                text: "Continue working toward the goal: ship it\nmore".to_owned(),
            },
            80,
            &theme,
            Detail::Collapsed,
        ));
        assert!(goal.contains("◆ Goal · ship it"), "{goal}");
        let row = plain_text(&render(
            &HistoryItem::Run {
                outcome: RunOutcome::Done,
                steps: 3,
                tool_calls: 2,
                elapsed: Duration::from_millis(14_200),
            },
            80,
            &theme,
            Detail::Collapsed,
        ));
        assert!(
            row.starts_with("done · 3 steps · 2 tool calls · 14.2s"),
            "{row}"
        );
    }

    /// ctrl+o: collapsed hides reasoning; expanded shows every output line.
    #[test]
    fn the_detail_mode_decides_what_a_row_shows() {
        let theme = Theme::plain();
        let thinking = HistoryItem::Thinking {
            text: "plan".to_owned(),
        };
        assert!(render(&thinking, 80, &theme, Detail::Collapsed).is_empty());
        assert!(plain_text(&render(&thinking, 80, &theme, Detail::Details)).contains("plan"));
        let output = HistoryItem::ToolOutput {
            name: "t".to_owned(),
            text: "t:\n1\n2\n3\n4".to_owned(),
        };
        assert_eq!(
            plain_text(&render(&output, 80, &theme, Detail::Expanded)),
            "  1\n  2\n  3\n  4"
        );
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
