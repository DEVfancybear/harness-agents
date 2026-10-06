//! History rendering: one [`HistoryItem`] becomes the rows that go into the
//! scrollback above the viewport.
//!
//! The rows are produced **before** the insert, because `insert_before` needs the
//! height up front: the renderer wraps the text at the current console width and
//! hands the resulting count to the terminal.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::highlight::{self, Language};
use super::markdown;
use super::theme::Theme;
use crate::interactive::events::{Detail, HistoryItem, RunOutcome, ToolState};
use crate::interactive::view;

/// How many lines of a tool's output a collapsed panel shows (prime-agent's
/// `tool-execution.ts`).
const TOOL_OUTPUT_PREVIEW_LINES: usize = 3;

/// Render history with speaker labels and indented tool rows. Collapsed tools
/// show three output lines; [`Detail::Expanded`] shows the complete input and
/// output in separate boxes. Notices and errors retain their own styles.
///
/// The plain renderer keeps its own transcript format ([`view::plain_lines`]); this
/// is only how the TUI shows the same items.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one arm per kind of history item, each short"
)]
pub fn render(item: &HistoryItem, width: u16, theme: &Theme, detail: Detail) -> Vec<Line<'static>> {
    match item {
        HistoryItem::Banner { lines } => banner_rows(lines, width, theme),
        HistoryItem::User { text } => user_box(text, width, theme),
        HistoryItem::Automatic { text } => injected_prompt(text, width, theme),
        HistoryItem::Assistant { text } => assistant_message(text, width, theme),
        // Collapsed mode hides reasoning, as prime-agent's overview does.
        HistoryItem::Thinking { .. } if detail == Detail::Collapsed => Vec::new(),
        HistoryItem::Thinking { text } => {
            // prime-agent shows reasoning as dim markdown, with no label.
            let dim = Theme {
                assistant: theme.dim,
                md_code: theme.dim,
                md_code_block: theme.dim,
                md_heading: theme.dim,
                diff_added: theme.dim,
                diff_removed: theme.dim,
                syntax: super::theme::SyntaxStyles::uniform(theme.dim),
                ..*theme
            };
            let mut rows = vec![
                Line::default(),
                Line::from(vec![
                    Span::styled("  ✧ ", theme.refinement),
                    Span::styled("suy nghĩ", theme.refinement.add_modifier(Modifier::ITALIC)),
                ]),
            ];
            rows.extend(
                markdown::render(text, width.saturating_sub(RAIL_CELLS), &dim)
                    .into_iter()
                    .map(|line| {
                        let mut spans = vec![Span::styled("  ┆ ", theme.dim)];
                        spans.extend(line.spans);
                        Line::from(spans).style(line.style)
                    }),
            );
            rows
        }
        // prime-agent's ctrl+o changes how much of a call's output is shown, not
        // how the call looks: the card is the same in every mode.
        HistoryItem::Tool {
            name,
            summary,
            input,
            state,
        } => {
            let mut rows = vec![
                Line::default(),
                tool_card(name, summary, state, width, theme),
            ];
            if let ToolState::Failed { detail, .. } = state
                && !detail.trim().is_empty()
            {
                rows.extend(railed_body(
                    vec![vec![Span::styled(detail.clone(), theme.error)]],
                    width,
                    theme.error,
                    theme,
                ));
            }
            // Expanded mode opens a call: what it was asked to do, then (in the
            // output row that follows) what it returned.
            if detail == Detail::Expanded && !input.trim().is_empty() {
                rows.extend(labeled_body(
                    "input",
                    input_lines(input, theme),
                    width,
                    rail_style(name, theme),
                    theme,
                ));
            }
            rows
        }
        HistoryItem::ToolOutput { name, text, path } => {
            output_rows(name, text, path.as_deref(), width, theme, detail)
        }
        HistoryItem::Run {
            outcome,
            steps,
            tool_calls,
            elapsed,
            // A failed turn's reason can be long; it wraps rather than being cut off.
        } => run_rows(outcome, *steps, *tool_calls, *elapsed, width, theme),
        // prime-agent shows no row for an admitted input: the user box is the record.
        HistoryItem::RunAccepted { .. } => Vec::new(),
        HistoryItem::Error { message } => error_rows(message, width, theme, detail),
        HistoryItem::Message { text } => vec![Line::from(Span::raw(text.clone()))],
        HistoryItem::Notice { message } => notice_rows(message, width, theme),
        HistoryItem::AgentExchange { from, to, text } => {
            agent_exchange_rows(from, to, text, width, theme, detail)
        }
        HistoryItem::Refinement {
            header,
            summary,
            details,
        } => refinement_rows(header, summary, details, width, theme, detail),
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

fn assistant_message(text: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let mut rows = vec![Line::default()];
    rows.extend(assistant_rows(text, width, theme));
    rows
}

/// Indent rendered rows by one cell, as prime-agent pads assistant text.
/// An answer's rows: markdown one cell in from each edge. The live block draws
/// the text still streaming with this too, so a line keeps its place and its
/// wrapping when the answer is committed to the scrollback.
#[must_use]
pub fn assistant_rows(text: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    railed(
        markdown::render(text, width.saturating_sub(RAIL_CELLS), theme),
        theme.rail_assistant,
    )
}

/// prime-agent's `setChatDetail`: overview previews every output; details also
/// opens the edit tools' diffs (`editDiffsExpanded`); all opens every output
/// (`toolOutputExpanded`).
fn output_rows(
    name: &str,
    text: &str,
    path: Option<&str>,
    width: u16,
    theme: &Theme,
    detail: Detail,
) -> Vec<Line<'static>> {
    let whole = match detail {
        Detail::Collapsed => false,
        Detail::Details => is_edit_tool(name),
        Detail::Expanded => true,
    };
    let rail = rail_style(name, theme);
    if detail == Detail::Expanded {
        let body = text
            .split_once('\n')
            .filter(|(first, _)| first.trim_end().ends_with(':'))
            .map_or(text, |(_, body)| body)
            .trim_end();
        labeled_body(
            "output",
            styled_lines(body, path, theme.muted, theme),
            width,
            rail,
            theme,
        )
    } else if whole {
        whole_output_rows(text, path, width, theme, rail)
    } else {
        tool_output_rows(text, path, width, theme, TOOL_OUTPUT_PREVIEW_LINES, rail)
    }
}

/// The tools whose output is a diff of the file they changed: prime-agent's
/// `isBuiltInEditTool`, whose diffs details mode opens.
fn is_edit_tool(name: &str) -> bool {
    matches!(name, "edit_file" | "write_file" | "apply_patch")
}

/// Streaming flushes are fragments of one answer, not new speakers.
pub fn render_fragment(
    item: &HistoryItem,
    width: u16,
    theme: &Theme,
    detail: Detail,
    continuing: bool,
) -> Vec<Line<'static>> {
    if continuing && let HistoryItem::Assistant { text } = item {
        assistant_rows(text, width, theme)
    } else {
        render(item, width, theme, detail)
    }
}

/// `1 step`, `3 steps`.
fn counted(count: u64, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// The operator's own turn: a rail with the label on it, then the text on the
/// message card.
///
/// The rail is what makes a turn's start findable while scrolling, and the card -
/// the row's own style, so it reaches the right edge of the console - is what
/// separates what the operator asked for from what the agent answered.
fn user_box(text: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(RAIL_CELLS).max(1);
    let mut rows = vec![Line::default()];
    for line in text.split('\n') {
        for row in wrap_spans(vec![Span::styled(line.to_owned(), theme.user)], inner) {
            let mut spans = vec![Span::styled(RAIL, theme.rail_user)];
            spans.extend(row.spans);
            rows.push(Line::from(spans).style(theme.user_box));
        }
    }
    rows.push(Line::default());
    rows
}

/// The rail in front of a turn's label and body: two cells of margin, the rail,
/// and the cell that separates it from the text.
const RAIL: &str = "  ▎ ";
/// Cells [`RAIL`] occupies, and therefore what a railed body gives up.
const RAIL_CELLS: u16 = 4;

/// Prefix every row of a body with the rail, so an answer is as findable as the
/// question above it.
fn railed(rows: Vec<Line<'static>>, style: Style) -> Vec<Line<'static>> {
    rows.into_iter()
        .map(|line| {
            let mut spans = vec![Span::styled(RAIL, style)];
            spans.extend(line.spans);
            Line::from(spans).style(line.style)
        })
        .collect()
}

/// The two cells every history row starts with.
///
/// One left edge for the whole transcript: a notice or an error is not the only
/// thing touching the console's border, and a wrapped one keeps the margin on its
/// continuation rows too.
const MARGIN: &str = "  ";

/// Wrap a row that starts at the margin, hanging its continuations under the text.
fn margined(spans: Vec<Span<'static>>, width: u16) -> Vec<Line<'static>> {
    let mut all = vec![Span::raw(MARGIN)];
    all.extend(spans);
    wrap_words(all, usize::from(width.saturating_sub(2)), 2)
}

/// Cells a row's spans occupy.
fn spans_cells(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .map(|span| super::widgets::composer::display_width(&span.content))
        .sum()
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
    rows.extend(margined(
        vec![
            Span::styled(marker, marker_style),
            Span::styled(label, theme.muted),
        ],
        width,
    ));
    rows
}

/// prime-agent's `⚠ Error: msg`, after a blank row.
/// prime-agent's `startsStackContext`: the leading rows of a traceback.
fn starts_stack_context(line: &str) -> bool {
    line.starts_with("Traceback ")
        || (line.starts_with("File ") && line.contains(", line "))
        || (line.starts_with("Cell In[") && line.contains(", line "))
        || line.starts_with("---->")
}

/// prime-agent's `summarizeErrorDetails`: the first line, or the last line
/// outside the stack context when the error opens with a traceback.
fn summarize_error(text: &str) -> String {
    let lines = text
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    let Some(first) = lines.first() else {
        return "Error".to_owned();
    };
    if lines.len() > 1 && starts_stack_context(first.trim()) {
        return lines
            .iter()
            .rev()
            .find(|line| {
                !starts_stack_context(line.trim_start())
                    && !line.starts_with(' ')
                    && !line.starts_with('\t')
            })
            .map_or_else(|| "Error".to_owned(), |line| line.trim().to_owned());
    }
    first.trim().to_owned()
}

/// prime-agent's `CollapsibleErrorComponent`: a multi-line error shows its
/// summary line until the detail mode is expanded (Ctrl+O); an error ending
/// in the login hint reads as one line, as prime-agent's
/// `formatInlineLoginRecoveryMessage` does.
fn error_rows(
    message: &str,
    width: u16,
    theme: &Theme,
    detail: super::super::events::Detail,
) -> Vec<Line<'static>> {
    const LOGIN_RECOVERY: &str = "Run /login to update credentials.";
    let normalized = message.replace("\r\n", "\n").replace('\r', "\n");
    let normalized = normalized.trim_end();
    let inline_login = normalized
        .strip_suffix(&format!("\n\n{LOGIN_RECOVERY}"))
        .map(str::trim_end)
        .filter(|base| !base.is_empty() && !base.contains('\n'))
        .map(|base| format!("{base} · {LOGIN_RECOVERY}"));
    let shown = match inline_login {
        Some(line) => line,
        None if normalized.contains('\n') && detail != super::super::events::Detail::Expanded => {
            summarize_error(normalized)
        }
        None => normalized.to_owned(),
    };
    let message = shown.as_str();
    let mut rows = vec![Line::default()];
    rows.extend(margined(
        vec![
            Span::styled(" ✗ ERROR ", theme.badge_error),
            Span::styled(format!("  {message}"), theme.error),
        ],
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
        vec![
            Span::styled("⚠ ", theme.warning),
            Span::styled(message.to_owned(), theme.warning),
        ]
    } else {
        // A notice can span lines (a sign-in shows its URL on a line of its own),
        // and a URL is drawn as a link so it stands out from the text around it.
        return message
            .lines()
            .enumerate()
            .flat_map(|(index, line)| {
                let spans = line
                    .split_inclusive(' ')
                    .map(|word| {
                        if word.starts_with("https://") || word.starts_with("http://") {
                            Span::styled(
                                word.to_owned(),
                                theme.md_link.add_modifier(Modifier::UNDERLINED),
                            )
                        } else {
                            Span::styled(word.to_owned(), theme.dim)
                        }
                    })
                    .collect::<Vec<_>>();
                let mut with_icon = vec![if index == 0 {
                    Span::styled("ℹ ", theme.info)
                } else {
                    Span::raw("  ")
                }];
                with_icon.extend(spans);
                margined(with_icon, width)
            })
            .collect();
    };
    margined(spans, width)
}

/// prime-agent's `RefinementOutcomeMessageComponent`: `◆ header` and the summary;
/// the details and expanded modes add the refinement's id and every edit.
/// prime-agent's agent message row (`agentMessageSummaryLine`,
/// `agentMessageBodyLines`): `◆ agent message · from → to · start`, and in the
/// details and expanded modes the whole message under a `╰─` gutter.
fn agent_exchange_rows(
    from: &str,
    to: &str,
    text: &str,
    width: u16,
    theme: &Theme,
    detail: Detail,
) -> Vec<Line<'static>> {
    const PREVIEW_CHARS: usize = 80;
    let mut spans = vec![
        Span::styled("◆ ".to_owned(), theme.accent),
        Span::styled("agent message".to_owned(), theme.muted),
        Span::styled(format!(" · {from} → {to}"), theme.dim),
    ];
    if detail == Detail::Collapsed {
        let first = text
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default();
        let mut preview = first.chars().take(PREVIEW_CHARS).collect::<String>();
        if first.chars().count() > PREVIEW_CHARS || text.lines().nth(1).is_some() {
            preview.push('…');
        }
        spans.push(Span::styled(format!(" · {preview}"), theme.dim));
    }
    let mut rows = margined(spans, width);
    if detail != Detail::Collapsed {
        let lines = text.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().enumerate() {
            let gutter = if index == 0 { "╰─ " } else { "   " };
            rows.extend(margined(
                vec![
                    Span::styled(gutter.to_owned(), theme.dim),
                    Span::styled((*line).to_owned(), theme.assistant),
                ],
                width,
            ));
        }
    }
    rows
}

fn refinement_rows(
    header: &str,
    summary: &str,
    details: &[String],
    width: u16,
    theme: &Theme,
    detail: Detail,
) -> Vec<Line<'static>> {
    let mut rows = vec![Line::default()];
    rows.extend(margined(
        vec![Span::styled(
            format!("◆ {header}"),
            theme.refinement.add_modifier(Modifier::BOLD),
        )],
        width,
    ));
    for line in summary.lines() {
        rows.extend(margined(
            vec![Span::styled(line.to_owned(), theme.muted)],
            width,
        ));
    }
    if detail != Detail::Collapsed && !details.is_empty() {
        rows.push(Line::default());
        for line in details {
            rows.extend(margined(vec![Span::styled(line.clone(), theme.dim)], width));
        }
    }
    rows
}

/// The rail down a card's body: its family's colour, dimmed, so a body reads as
/// belonging to its header without competing with it.
fn rail_style(name: &str, theme: &Theme) -> Style {
    super::icons::ToolKind::of(name)
        .style(theme)
        .add_modifier(Modifier::DIM)
}

/// Lines of a card's body hung from its rail: every row but the last after `│`, the
/// last after `╰─`, so the body visibly ends where the card does.
fn railed_body(
    lines: Vec<Vec<Span<'static>>>,
    width: u16,
    rail: Style,
    _theme: &Theme,
) -> Vec<Line<'static>> {
    let room = usize::from(width)
        .saturating_sub(super::card::RAIL_CELLS + 1)
        .max(1);
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    for spans in lines {
        for row in wrap_words(spans, room, 0) {
            rows.push(row.spans);
        }
    }
    let last = rows.len().saturating_sub(1);
    rows.into_iter()
        .enumerate()
        .map(|(index, spans)| {
            let prefix = if index == last {
                super::card::RAIL_END
            } else {
                super::card::RAIL
            };
            let mut all = vec![Span::styled(prefix, rail)];
            all.extend(spans);
            Line::from(all)
        })
        .collect()
}

/// A body with a small label as its first row: `⌥ input`, `⌥ output`.
fn labeled_body(
    label: &str,
    mut lines: Vec<Vec<Span<'static>>>,
    width: u16,
    rail: Style,
    theme: &Theme,
) -> Vec<Line<'static>> {
    lines.insert(
        0,
        vec![Span::styled(
            format!("⌥ {label}"),
            theme.dim.add_modifier(Modifier::ITALIC),
        )],
    );
    railed_body(lines, width, rail, theme)
}

/// A call's input, one `key: value` row per argument when it is a JSON object, and
/// as it came otherwise. A multi-line value (a patch, a script) keeps its lines.
fn input_lines(input: &str, theme: &Theme) -> Vec<Vec<Span<'static>>> {
    let key = theme.md_heading;
    let value = theme.muted;
    let Ok(serde_json::Value::Object(fields)) = serde_json::from_str::<serde_json::Value>(input)
    else {
        return input
            .lines()
            .map(|line| vec![Span::styled(line.to_owned(), value)])
            .collect();
    };
    let mut rows = Vec::new();
    for (name, field) in fields {
        let text = match field {
            serde_json::Value::String(text) => text,
            other => other.to_string(),
        };
        let mut parts = text.lines();
        rows.push(vec![
            Span::styled(format!("{name}: "), key),
            Span::styled(parts.next().unwrap_or_default().to_owned(), value),
        ]);
        for more in parts {
            rows.push(vec![Span::raw("  "), Span::styled(more.to_owned(), value)]);
        }
    }
    rows
}

/// What a tool returned, all of it, under its card: prime-agent's expanded tool
/// output. Every line is kept - blank ones too, they are part of a file - and a
/// long line wraps instead of being cut.
fn whole_output_rows(
    text: &str,
    path: Option<&str>,
    width: u16,
    theme: &Theme,
    rail: Style,
) -> Vec<Line<'static>> {
    // As in the preview, the first line repeats the call (`read_file src/a.rs:`).
    let body = text
        .split_once('\n')
        .filter(|(first, _)| first.trim_end().ends_with(':'))
        .map_or(text, |(_, body)| body)
        .trim_end();
    railed_body(
        styled_lines(body, path, theme.muted, theme),
        width,
        rail,
        theme,
    )
}

/// The collapsed body of a tool card: the first lines of what the tool returned,
/// then `… N more lines` - prime-agent's collapsed tool output.
fn tool_output_rows(
    text: &str,
    path: Option<&str>,
    width: u16,
    theme: &Theme,
    shown: usize,
    rail: Style,
) -> Vec<Line<'static>> {
    // The first line repeats the tool's name (`read_file src/a.rs:`), which the
    // header says.
    let body = text
        .split_once('\n')
        .filter(|(first, _)| first.trim_end().ends_with(':'))
        .map_or(text, |(_, body)| body);
    let total = body.lines().filter(|line| !line.trim().is_empty()).count();
    // Only the lines up to the last one shown are highlighted: a long file's
    // preview does not parse the whole file.
    let mut kept = 0;
    let head = body
        .lines()
        .take_while(|line| {
            let take = kept < shown;
            if !line.trim().is_empty() {
                kept += 1;
            }
            take
        })
        .collect::<Vec<_>>()
        .join("\n");
    let room = usize::from(width).saturating_sub(super::card::RAIL_CELLS + 1);
    let mut lines: Vec<Vec<Span<'static>>> = styled_lines(&head, path, theme.muted, theme)
        .into_iter()
        .filter(|spans| spans.iter().any(|span| !span.content.trim().is_empty()))
        .take(shown)
        .map(|line| clip_spans(line, room))
        .collect();
    let more = total.saturating_sub(shown);
    if more > 0 {
        lines.push(vec![Span::styled(
            format!("… {more} more lines"),
            theme.dim,
        )]);
    }
    railed_body(lines, width, rail, theme)
}

/// The language code in `path` is written in, when the theme has colours to
/// show it with.
fn code_language(path: Option<&str>, theme: &Theme) -> Option<Language> {
    if theme.color {
        path.and_then(highlight::language_for_path)
    } else {
        None
    }
}

/// A block of text as styled lines, one per line and not yet wrapped.
///
/// A diff is coloured by its markers, with the code in each line highlighted in
/// its file's language - the file a `diff --git`, `+++` or `*** Update File:`
/// header names, or else the call's own `path` - as prime-agent draws a diff.
/// File contents (an output's numbered rows) are highlighted in
/// the language of `path`. Everything else keeps `style`: without a language
/// nothing is guessed, so prose is never coloured as code.
fn styled_lines(
    text: &str,
    path: Option<&str>,
    style: Style,
    theme: &Theme,
) -> Vec<Vec<Span<'static>>> {
    let lines: Vec<String> = text
        .lines()
        .map(|line| untab(line.trim_end_matches('\r')))
        .collect();
    let language = code_language(path, theme);
    let numbered = numbered_diff(&lines);
    if numbered || looks_like_diff(text) {
        return diff_lines(&lines, numbered, language, style, theme);
    }
    let plain = |lines: &[String]| {
        lines
            .iter()
            .map(|line| vec![Span::styled(line.clone(), style)])
            .collect()
    };
    let Some(language) = language else {
        return plain(&lines);
    };
    {
        let rows: Vec<Option<usize>> = lines.iter().map(|line| numbered_row(line)).collect();
        if rows.iter().all(Option::is_none) {
            return plain(&lines);
        }
        let code: Vec<&str> = lines
            .iter()
            .zip(&rows)
            .filter_map(|(line, row)| row.map(|at| &line[at..]))
            .collect();
        let mut highlighted = highlight::highlight(language, &code).into_iter();
        lines
            .iter()
            .zip(rows)
            .map(|(line, row)| match row {
                Some(at) => {
                    let mut spans = vec![Span::styled(line[..at].to_owned(), theme.dim)];
                    let runs = highlighted.next().unwrap_or_default();
                    spans.extend(code_spans(runs, theme.syntax.plain, None, theme));
                    spans
                }
                None => vec![Span::styled(line.clone(), style)],
            })
            .collect()
    }
}

/// Highlighted runs as spans, each on `background` when there is one.
fn code_spans(
    runs: Vec<highlight::Run>,
    plain: Style,
    background: Option<Style>,
    theme: &Theme,
) -> Vec<Span<'static>> {
    runs.into_iter()
        .map(|(kind, text)| {
            let style = highlight::style(kind, plain, theme);
            Span::styled(text, background.map_or(style, |bg| style.patch(bg)))
        })
        .collect()
}

/// A diff's lines: headers dim, hunk ranges in the accent, changed lines behind
/// their `+`/`-` in the diff colours, and the code in them highlighted.
fn diff_lines(
    lines: &[String],
    numbered: bool,
    mut language: Option<Language>,
    style: Style,
    theme: &Theme,
) -> Vec<Vec<Span<'static>>> {
    lines
        .iter()
        .map(|line| {
            if let Some(header) = diff_header(line) {
                if let Header::File(file) = header
                    && let Some(found) = code_language(Some(file), theme)
                {
                    language = Some(found);
                }
                return vec![Span::styled(line.clone(), theme.dim)];
            }
            if line.starts_with("@@") {
                return vec![Span::styled(line.clone(), theme.accent)];
            }
            let mut characters = line.chars();
            let marker = characters.next();
            let rest = characters.as_str();
            let (gutter, content) = if numbered {
                numbered_diff_row(line).map_or(("", rest), |at| (&line[1..at], &line[at..]))
            } else {
                ("", rest)
            };
            match marker {
                Some(marker @ ('+' | '-' | ' ')) => {
                    diff_row(marker, gutter, content, language, style, theme)
                }
                _ => vec![Span::styled(line.clone(), style)],
            }
        })
        .collect()
}

/// One line of a diff: its marker and gutter in the diff colour, then its code.
/// A changed line sits on the diff background, as prime-agent's block rows do.
fn diff_row(
    marker: char,
    gutter: &str,
    content: &str,
    language: Option<Language>,
    style: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let (color, background) = match marker {
        '+' => (theme.diff_added, Some(theme.diff_added_bg)),
        '-' => (theme.diff_removed, Some(theme.diff_removed_bg)),
        _ => (style, None),
    };
    let on = |style: Style| background.map_or(style, |bg| style.patch(bg));
    let head = if marker == ' ' {
        Span::styled(format!(" {gutter}"), theme.dim)
    } else {
        Span::styled(format!("{marker}{gutter}"), on(color))
    };
    let mut spans = vec![head];
    match language {
        Some(language) => {
            let runs = highlight::highlight_each(language, &[content])
                .pop()
                .unwrap_or_default();
            spans.extend(code_spans(runs, theme.syntax.plain, background, theme));
        }
        None => spans.push(Span::styled(content.to_owned(), on(color))),
    }
    spans
}

/// A diff's header line.
enum Header<'a> {
    /// A header that names the file the hunks below it change.
    File(&'a str),
    /// Any other header (`index ...`, `*** Begin Patch`, `/dev/null`).
    Other,
}

/// Whether a line heads a diff's file, and the file it names if it names one.
fn diff_header(line: &str) -> Option<Header<'_>> {
    for prefix in ["+++ ", "--- "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let file = rest.split('\t').next().unwrap_or(rest).trim();
            return Some(if file == "/dev/null" {
                Header::Other
            } else {
                Header::File(file)
            });
        }
    }
    if let Some(rest) = line.strip_prefix("diff --git ") {
        return Some(
            rest.split_whitespace()
                .last()
                .map_or(Header::Other, Header::File),
        );
    }
    for prefix in [
        "*** Update File: ",
        "*** Add File: ",
        "*** Delete File: ",
        "*** Move to: ",
    ] {
        if let Some(file) = line.strip_prefix(prefix) {
            return Some(Header::File(file.trim()));
        }
    }
    if line.starts_with("*** ") || line.starts_with("index ") {
        return Some(Header::Other);
    }
    None
}

/// Where the code starts in a numbered file row (`12: code`), if it is one.
fn numbered_row(line: &str) -> Option<usize> {
    let digits = line.trim_start_matches(' ');
    let lead = line.len() - digits.len();
    let count = digits.bytes().take_while(u8::is_ascii_digit).count();
    if count == 0 {
        return None;
    }
    let rest = &digits[count..];
    if rest.starts_with(": ") {
        Some(lead + count + 2)
    } else if rest == ":" {
        Some(lead + count + 1)
    } else {
        None
    }
}

/// Where the code starts in a numbered diff row (`+ 12 code`, `-  3 code`,
/// `  12 code`), as an edit reports the change it made.
fn numbered_diff_row(line: &str) -> Option<usize> {
    let rest = line.strip_prefix(['+', '-', ' '])?;
    let digits = rest.trim_start_matches(' ');
    let count = digits.bytes().take_while(u8::is_ascii_digit).count();
    if count == 0 {
        return None;
    }
    let after = &digits[count..];
    let at = line.len() - after.len();
    if after.is_empty() {
        Some(at)
    } else if after.starts_with(' ') {
        Some(at + 1)
    } else {
        None
    }
}

/// Whether lines are an edit's numbered diff: after its header, every row is a
/// numbered diff row, a `...` gap or the truncation notice, and some row changed.
fn numbered_diff(lines: &[String]) -> bool {
    let Some(first) = lines
        .iter()
        .position(|line| numbered_diff_row(line).is_some())
    else {
        return false;
    };
    let rows = &lines[first..];
    rows.iter()
        .any(|line| line.starts_with(['+', '-']) && numbered_diff_row(line).is_some())
        && rows.iter().all(|line| {
            numbered_diff_row(line).is_some()
                || line.trim() == "..."
                || line.trim().is_empty()
                || line.starts_with("[diff truncated")
        })
}

/// Whether text is a patch: a unified diff hunk, or the `*** Begin Patch` envelope.
fn looks_like_diff(text: &str) -> bool {
    text.lines()
        .any(|line| line.starts_with("@@") || line.starts_with("*** Begin Patch"))
}

/// A tab has no cell width of its own; four spaces keep indented code readable.
fn untab(line: &str) -> String {
    line.replace('\t', "    ")
}

/// Styled spans cut to at most `cells` cells, ending in `…` when cut.
fn clip_spans(spans: Vec<Span<'static>>, cells: usize) -> Vec<Span<'static>> {
    let total = spans
        .iter()
        .flat_map(|span| span.content.chars())
        .map(super::widgets::composer::char_width)
        .sum::<usize>();
    if total <= cells {
        return spans;
    }
    let mut out = Vec::new();
    let mut used = 0;
    for span in spans {
        let mut text = String::new();
        for character in span.content.chars() {
            let width = super::widgets::composer::char_width(character);
            if used + width > cells.saturating_sub(1) {
                if !text.is_empty() {
                    out.push(Span::styled(text, span.style));
                }
                out.push(Span::styled("…", span.style));
                return out;
            }
            text.push(character);
            used += width;
        }
        out.push(Span::styled(text, span.style));
    }
    out
}

/// The session heading: project, model, permission mode and any setup guidance.
#[allow(
    clippy::too_many_lines,
    reason = "one card built top to bottom: title, badges, path, keys, setup notes"
)]
fn banner_rows(lines: &[String], width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let project = lines
        .iter()
        .find_map(|line| line.strip_prefix("Project: "))
        .map(|value| value.split("    Provider:").next().unwrap_or(value));
    let name = project
        .and_then(|path| {
            path.trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
        })
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace");
    let version = lines
        .iter()
        .find(|line| line.starts_with("Harness Agents"))
        .cloned()
        .unwrap_or_default();
    // One card of identity: which workspace this session belongs to, the build it
    // runs, the model it will ask, how much it may do without being asked, the
    // branch, and the keys that open everything else.
    let inner = usize::from(width).saturating_sub(6).clamp(24, 96);
    let border = theme.composer_border;
    let mut rows = vec![Line::default()];
    // The top edge carries the title on the left and the build on the right.
    let title = format!(" ✦ ha / {name} ");
    let build = if version.is_empty() {
        String::new()
    } else {
        format!(" {version} ")
    };
    let used = 1
        + super::widgets::composer::display_width(&title)
        + super::widgets::composer::display_width(&build);
    let fill = (inner + 2).saturating_sub(used + 1).max(1);
    rows.push(Line::from(vec![
        Span::styled("  ╭─", border),
        Span::styled(title, theme.accent.add_modifier(Modifier::BOLD)),
        Span::styled("─".repeat(fill), border),
        Span::styled(build, theme.dim),
        Span::styled("─╮", border),
    ]));
    let row = |spans: Vec<Span<'static>>| -> Line<'static> {
        let cells = spans_cells(&spans);
        let mut all = vec![Span::styled("  │ ", border)];
        all.extend(spans);
        all.push(Span::raw(" ".repeat(inner.saturating_sub(cells))));
        all.push(Span::styled(" │", border));
        Line::from(all)
    };
    if let Some(model) = lines.iter().find_map(|line| line.strip_prefix("Service: ")) {
        let model = model.split(" via ").next().unwrap_or(model);
        let permissions = lines
            .iter()
            .find_map(|line| line.strip_prefix("Permissions: "));
        let mut spans = vec![Span::styled(format!(" ◆ {model} "), theme.badge_accent)];
        if let Some(mode) = permissions {
            let style = if mode.contains("full-auto") {
                theme.badge_warn
            } else {
                theme.badge_ok
            };
            spans.push(Span::raw(" "));
            spans.push(Span::styled(format!(" ⚙ {mode} "), style));
        }
        if let Some(level) = lines
            .iter()
            .find_map(|line| line.strip_prefix("Thinking: "))
        {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(format!(" ✧ {level} "), theme.badge_info));
        }
        if let Some(git) = lines.iter().find_map(|line| line.strip_prefix("Git: "))
            && !git.trim().is_empty()
        {
            let git: String = git.trim().chars().take(28).collect();
            spans.push(Span::raw(" "));
            spans.push(Span::styled(format!(" ⎇ {git} "), theme.chip));
        }
        rows.push(row(spans));
    }
    if let Some(project) = project {
        // A long path wraps onto further rows, never cut: it is what tells two
        // sessions apart.
        let room = inner.saturating_sub(2).max(8);
        let characters: Vec<char> = project.chars().collect();
        for (index, chunk) in characters.chunks(room).enumerate() {
            rows.push(row(vec![
                Span::styled(if index == 0 { "⌂ " } else { "  " }, theme.dim),
                Span::styled(chunk.iter().collect::<String>(), theme.muted),
            ]));
        }
    }
    rows.push(row(vec![
        Span::styled("/", theme.accent),
        Span::styled(" lệnh  ", theme.dim),
        Span::styled("@", theme.accent),
        Span::styled(" tệp  ", theme.dim),
        Span::styled("Ctrl+O", theme.accent),
        Span::styled(" chi tiết  ", theme.dim),
        Span::styled("Ctrl+C", theme.accent),
        Span::styled(" dừng", theme.dim),
    ]));
    rows.push(Line::from(vec![
        Span::styled("  ╰", border),
        Span::styled("─".repeat(inner + 2), border),
        Span::styled("╯", border),
    ]));
    // Setup errors and sign-in instructions stay visible. Configuration paths
    // are available through /config rather than filling the opening screen.
    for line in lines.iter().filter(|line| {
        !line.is_empty()
            && ![
                "Harness Agents",
                "Project:",
                "Session:",
                "Git:",
                "AGENTS.md:",
                "Config:",
                "Data:",
                "Store:",
                "Service:",
                "Permissions:",
                "Thinking:",
                "Nhập yêu cầu.",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
    }) {
        rows.extend(wrap_spans(
            vec![
                Span::styled("  ⚠ ", theme.warning),
                Span::styled(line.clone(), theme.warning),
            ],
            width,
        ));
    }
    rows
}
/// A tool panel header: prime-agent's `label · status` on `toolPanelBg` - `running`
/// with its diamond marker in `bashMode`, `done` in `success`, `error` in `error` -
/// followed by the arguments and the duration in dim. The Python REPL gets
/// prime-agent's ipython-cell summary: `✓ python · <code> · <duration>`.
#[must_use]
pub fn tool_card(
    name: &str,
    summary: &str,
    state: &ToolState,
    width: u16,
    theme: &Theme,
) -> Line<'static> {
    running_card(name, summary, state, 1, width, theme)
}

/// [`tool_card`] at animation frame `tick`: a running tool's spinner turns.
#[must_use]
pub fn running_card(
    name: &str,
    summary: &str,
    state: &ToolState,
    tick: u64,
    width: u16,
    theme: &Theme,
) -> Line<'static> {
    let outcome = match state {
        ToolState::Started => super::card::Outcome::Running(Theme::spinner(tick)),
        ToolState::Ok { elapsed } => super::card::Outcome::Done(view::seconds_label(*elapsed)),
        ToolState::Failed { elapsed, .. } => {
            super::card::Outcome::Failed(view::seconds_label(*elapsed))
        }
    };
    super::card::header(name, summary, &outcome, width, theme)
}

/// The end-of-turn line: a rule that closes the turn, the outcome, and the
/// counters, in the warning colour when the turn stopped short and the error
/// colour when it failed.
///
/// The rule is what makes the end of a turn findable while scrolling back through
/// a long session, and the counters stay in dim so the outcome is what the eye
/// lands on.
fn run_rows(
    outcome: &RunOutcome,
    steps: u32,
    tool_calls: u32,
    elapsed: std::time::Duration,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let (glyph, badge) = match outcome {
        RunOutcome::Done => ("✓", theme.badge_ok),
        RunOutcome::Canceled
        | RunOutcome::Paused(_)
        | RunOutcome::WaitingInput { .. }
        | RunOutcome::ExternalWait => ("◐", theme.badge_warn),
        RunOutcome::Blocked(_) | RunOutcome::Failed(_) => ("✗", theme.badge_error),
    };
    let mut spans = vec![
        Span::styled("  ", theme.rule),
        Span::styled(format!(" {glyph} {} ", outcome.label()), badge),
        Span::styled(
            format!(
                "  ⟳ {}  ⚙ {}  ◷ {} ",
                counted(u64::from(steps), "step", "steps"),
                counted(u64::from(tool_calls), "tool call", "tool calls"),
                view::seconds_label(elapsed)
            ),
            theme.dim,
        ),
    ];
    let fill = usize::from(width).saturating_sub(spans_cells(&spans) + 1);
    if fill > 0 {
        spans.push(Span::styled("─".repeat(fill), theme.rule));
    }
    wrap_spans(spans, width)
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

/// [`wrap_spans`] for the rows of an expanded box: it breaks between words where
/// it can, so a wrapped command or path reads as whole tokens (`--no-fail-fast`
/// stays in one piece), and every row after the first starts `hang` cells in, so
/// a continuation stays under its own text rather than under the `$ `. A word
/// wider than a row is still broken by cells.
fn wrap_words(spans: Vec<Span<'static>>, width: usize, hang: usize) -> Vec<Line<'static>> {
    let limit = width.max(1);
    // Two cells is the widest character; a hang must still leave room for it.
    let hang = if hang + 2 <= limit { hang } else { 0 };
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut used = 0_usize;
    let mut row_start = 0_usize;
    let next_row = |rows: &mut Vec<Line<'static>>, current: &mut Vec<Span<'static>>| {
        rows.push(Line::from(std::mem::take(current)));
        if hang > 0 {
            current.push(Span::raw(" ".repeat(hang)));
        }
    };
    for span in spans {
        let style = span.style;
        // Words keep the spaces that follow them.
        for word in span.content.split_inclusive(' ') {
            let visible = word
                .trim_end_matches(' ')
                .chars()
                .map(super::widgets::composer::char_width)
                .sum::<usize>();
            // A word that does not fit moves to the next row, unless the row holds
            // nothing yet or the word would not fit on a fresh row either.
            if used > row_start && used + visible > limit && hang + visible <= limit {
                next_row(&mut rows, &mut current);
                used = hang;
                row_start = hang;
            }
            let mut chunk = String::new();
            for character in word.chars() {
                let cells = super::widgets::composer::char_width(character);
                if used + cells > limit {
                    // A space at the end of a row is dropped, not wrapped.
                    if character == ' ' {
                        continue;
                    }
                    if !chunk.is_empty() {
                        current.push(Span::styled(std::mem::take(&mut chunk), style));
                    }
                    next_row(&mut rows, &mut current);
                    used = hang;
                    row_start = hang;
                }
                chunk.push(character);
                used += cells;
            }
            if !chunk.is_empty() {
                current.push(Span::styled(chunk, style));
            }
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

    /// prime-agent's collapsible error: a traceback shows its last line until
    /// the detail mode is expanded; the login hint stays on the line.
    #[test]
    fn a_multi_line_error_collapses_to_its_summary() {
        assert_eq!(
            super::summarize_error(
                "Traceback (most recent call last):\n  File \"x.py\", line 1, in <module>\nValueError: bad"
            ),
            "ValueError: bad"
        );
        assert_eq!(super::summarize_error("first\nsecond"), "first");
        let text = |detail| {
            super::error_rows(
                "boom\nat frame one\nat frame two",
                80,
                &crate::interactive::tui::theme::Theme::plain(),
                detail,
            )
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
        };
        let collapsed = text(crate::interactive::events::Detail::Collapsed);
        assert!(
            collapsed.contains("boom") && !collapsed.contains("frame two"),
            "{collapsed}"
        );
        assert!(text(crate::interactive::events::Detail::Expanded).contains("frame two"));
        let login = super::error_rows(
            "401 Unauthorized\n\nRun /login to update credentials.",
            80,
            &crate::interactive::tui::theme::Theme::plain(),
            crate::interactive::events::Detail::Collapsed,
        )
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
        assert!(
            login.contains("401 Unauthorized · Run /login to update credentials."),
            "{login}"
        );
    }
    use crate::interactive::events::{Detail, HistoryItem, RunOutcome, ToolState};
    use crate::interactive::tui::markdown::plain_text;
    use crate::interactive::tui::theme::Theme;
    use ratatui::style::Modifier;
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
        assert!(user.contains("  ▎ sửa lỗi parser"), "{user}");
        let card = plain_text(&render(
            &HistoryItem::Tool {
                name: "read_file".to_owned(),
                summary: "path=a.rs".to_owned(),
                input: String::new(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(12),
                },
            },
            80,
            &theme,
            Detail::Collapsed,
        ));
        assert!(card.contains("▤ Read file  a.rs"), "{card}");
        assert!(card.trim_end().ends_with("✓ 12ms"), "{card}");
        let output = plain_text(&render(
            &HistoryItem::ToolOutput {
                name: "read_file".to_owned(),
                path: None,
                text: "read_file:\none\ntwo\nthree\nfour\nfive".to_owned(),
            },
            80,
            &theme,
            Detail::Collapsed,
        ));
        assert_eq!(output, "  │ one\n  │ two\n  │ three\n  ╰─ … 2 more lines");
        let error = plain_text(&render(
            &HistoryItem::Error {
                message: "boom".to_owned(),
            },
            80,
            &theme,
            Detail::Collapsed,
        ));
        assert!(
            error.contains("✗ ERROR") && error.trim_end().ends_with("boom"),
            "{error}"
        );
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
    fn redesign_history_has_speaker_labels_and_tool_markers() {
        let theme = Theme::plain();
        let user = plain_text(&render(
            &HistoryItem::User {
                text: "sửa lỗi".into(),
            },
            80,
            &theme,
            Detail::Collapsed,
        ));
        assert_eq!(user, "\n  ▎ sửa lỗi\n");
        let answer = plain_text(&render(
            &HistoryItem::Assistant {
                text: "Đã sửa.".into(),
            },
            80,
            &theme,
            Detail::Collapsed,
        ));
        assert!(answer.starts_with("\n  ▎ Đã sửa."), "{answer}");
        let tool = plain_text(&render(
            &HistoryItem::Tool {
                name: "read_file".into(),
                summary: "src/auth.rs".into(),
                input: String::new(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(12),
                },
            },
            80,
            &theme,
            Detail::Collapsed,
        ));
        assert!(tool.contains("  ▤ Read file  src/auth.rs"), "{tool}");
        assert!(tool.trim_end().ends_with("✓ 12ms"), "{tool}");
    }

    #[test]
    fn redesign_banner_is_compact_and_preserves_setup_guidance() {
        let lines = vec![
            String::new(),
            "Harness Agents 0.1.0".into(),
            "Project: C:/work/harness-agents    Provider: credential present".into(),
            "Config: C:/config.toml".into(),
            "Store: C:/store".into(),
            "Service: deepseek-v4-flash via https://api.deepseek.com".into(),
            "Log in with /login".into(),
        ];
        let rows = super::banner_rows(&lines, 80, &Theme::plain());
        let text = plain_text(&rows);
        assert!(text.contains("ha / harness-agents"), "{text}");
        assert!(text.contains("deepseek-v4-flash"));
        assert!(text.contains("Log in with /login"));
        assert!(!text.contains("Store:") && !text.contains("Config:"));
    }

    #[test]
    fn a_failed_tool_card_carries_its_duration_and_reason() {
        let text = plain_text(&render(
            &HistoryItem::Tool {
                name: "list_files".to_owned(),
                summary: "path=".to_owned(),
                input: String::new(),
                state: ToolState::Failed {
                    elapsed: Duration::from_millis(962),
                    detail: "invalid_payload: optional tool path must not be blank".to_owned(),
                },
            },
            120,
            &Theme::plain(),
            Detail::Collapsed,
        ));
        assert!(text.contains("▥ List files"), "{text}");
        assert!(text.contains("✗ 962ms"), "{text}");
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
                input: String::new(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(1200),
                },
            },
            80,
            &Theme::plain(),
            Detail::Collapsed,
        ));
        assert!(text.contains("λ Python  print(1)"), "{text}");
        assert!(text.trim_end().ends_with("✓ 1.2s"), "{text}");
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
            row.contains("✓ done") && row.contains("3 steps") && row.contains("2 tool calls"),
            "the turn closes on a row that names its outcome: {row}"
        );
        assert!(row.contains("14.2s") && row.contains('─'), "{row}");
    }

    /// A sign-in notice keeps its lines, and the URL is its own, underlined row.
    #[test]
    fn a_notice_keeps_its_lines_and_draws_a_url_as_a_link() {
        let theme = Theme::colored();
        let rows = render(
            &HistoryItem::Notice {
                message: "Sign in:\nhttps://auth.example/authorize?x=1\nEsc cancels.".to_owned(),
            },
            80,
            &theme,
            Detail::Collapsed,
        );
        assert_eq!(
            plain_text(&rows),
            "  ℹ Sign in:\n    https://auth.example/authorize?x=1\n    Esc cancels."
        );
        let link = rows[1]
            .spans
            .iter()
            .find(|span| span.style.add_modifier.contains(Modifier::UNDERLINED))
            .expect("the URL is drawn as a link");
        assert!(link.content.starts_with("https://"), "{link:?}");
    }

    /// ctrl+o, prime-agent's `setChatDetail`: overview hides reasoning and
    /// previews every output; details shows reasoning and opens the edit tools'
    /// diffs; all opens every output. The call's card is the same in all three.
    #[test]
    fn the_detail_mode_decides_what_a_row_shows() {
        let theme = Theme::plain();
        let thinking = HistoryItem::Thinking {
            text: "plan".to_owned(),
        };
        assert!(render(&thinking, 80, &theme, Detail::Collapsed).is_empty());
        assert!(plain_text(&render(&thinking, 80, &theme, Detail::Details)).contains("plan"));
        assert!(plain_text(&render(&thinking, 80, &theme, Detail::Expanded)).contains("plan"));
        let output = |name: &str| HistoryItem::ToolOutput {
            name: name.to_owned(),
            path: None,
            text: "t:\n1\n2\n3\n4".to_owned(),
        };
        let preview = "  │ 1\n  │ 2\n  │ 3\n  ╰─ … 1 more lines";
        let whole = "  │ 1\n  │ 2\n  │ 3\n  ╰─ 4";
        let opened = "  │ ⌥ output\n  │ 1\n  │ 2\n  │ 3\n  ╰─ 4";
        for (name, detail, expected) in [
            ("read_file", Detail::Collapsed, preview),
            ("read_file", Detail::Details, preview),
            ("read_file", Detail::Expanded, opened),
            ("edit_file", Detail::Collapsed, preview),
            ("edit_file", Detail::Details, whole),
            ("edit_file", Detail::Expanded, opened),
        ] {
            assert_eq!(
                plain_text(&render(&output(name), 80, &theme, detail)),
                expected,
                "{name} in {detail:?}"
            );
        }
    }

    fn shell_call() -> HistoryItem {
        HistoryItem::Tool {
            name: "run_shell".to_owned(),
            summary: "command=cargo test --workspace --locked timeout_ms=60000".to_owned(),
            input: r#"{"command":"cargo test --workspace --locked --no-fail-fast -- --test-threads=1","timeout_ms":60000}"#
                .to_owned(),
            state: ToolState::Ok {
                elapsed: Duration::from_millis(1200),
            },
        }
    }

    /// prime-agent's ctrl+o does not redraw a call as boxes: the card is one line
    /// in every mode, and only how much output follows it changes.
    #[test]
    fn a_call_is_the_same_card_in_every_mode() {
        for detail in [Detail::Collapsed, Detail::Details, Detail::Expanded] {
            let text = plain_text(&render(&shell_call(), 100, &Theme::plain(), detail));
            assert!(
                text.contains("\n  ❯ Run shell  cargo test --workspace --locked  timeout_ms=60000"),
                "{detail:?}: {text}"
            );
            // Only Expanded goes on to open the call: its input, under the card.
            assert_eq!(
                text.contains("⌥ input"),
                detail == Detail::Expanded,
                "{detail:?}: {text}"
            );
            if detail == Detail::Expanded {
                assert!(text.contains("timeout_ms: 60000"), "{text}");
                assert!(text.contains("--no-fail-fast"), "{text}");
            } else {
                assert!(text.trim_end().ends_with("✓ 1.2s"), "{detail:?}: {text}");
            }
        }
    }

    /// Expanded keeps every line of what a tool returned, blank ones included,
    /// and wraps a long one instead of cutting it.
    #[test]
    fn expanded_output_keeps_every_line_and_wraps() {
        let text = plain_text(&render(
            &HistoryItem::ToolOutput {
                name: "read_file".to_owned(),
                path: None,
                text: "read_file src/lib.rs:\nfn a() {}\n\nfn b() {}\nfn c() {}\nlet long = sửa lỗi phân tích cú pháp 解析器错误;\n"
                    .to_owned(),
            },
            24,
            &Theme::plain(),
            Detail::Expanded,
        ));
        let rows = text.lines().collect::<Vec<_>>();
        assert_eq!(
            &rows[..5],
            [
                "  │ ⌥ output",
                "  │ fn a() {}",
                "  │ ",
                "  │ fn b() {}",
                "  │ fn c() {}"
            ],
            "{text}"
        );
        assert!(rows.len() > 5, "the long line wraps: {text}");
        assert!(!text.contains('…'), "nothing is cut: {text}");
        for row in &rows {
            assert!(cells(row) <= 24, "{row:?}");
        }
    }

    fn cells(text: &str) -> usize {
        crate::interactive::tui::widgets::composer::display_width(text)
    }

    fn truecolor() -> Theme {
        Theme::prime(crate::interactive::tui::theme::Depth::TrueColor)
    }

    /// The last span that reads `text` (wrapping splits a run at its spaces).
    fn span_with<'a>(rows: &'a [Line<'static>], text: &str) -> &'a Span<'static> {
        rows.iter()
            .flat_map(|row| row.spans.iter())
            .rfind(|span| span.content.trim_end() == text)
            .unwrap_or_else(|| panic!("no span {text:?} in {rows:?}"))
    }

    fn output(text: &str, path: Option<&str>, theme: &Theme) -> Vec<Line<'static>> {
        render(
            &HistoryItem::ToolOutput {
                name: "t".to_owned(),
                text: text.to_owned(),
                path: path.map(str::to_owned),
            },
            80,
            theme,
            Detail::Expanded,
        )
    }

    /// A git diff names its file; the code in each changed line is highlighted in
    /// that file's language, on the diff background, behind its marker.
    #[test]
    fn a_git_diff_is_highlighted_in_the_language_of_its_file() {
        let theme = truecolor();
        let rows = output(
            "git diff:
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1 @@
-let x = 1;
+let x = \"two\";
 context();",
            None,
            &theme,
        );
        let text = plain_text(&rows);
        assert!(
            text.contains("  │ +let x = \"two\";"),
            "text is unchanged: {text}"
        );
        let keyword = rows
            .iter()
            .flat_map(|row| row.spans.iter())
            .filter(|span| span.content.as_ref() == "let")
            .map(|span| span.style)
            .collect::<Vec<_>>();
        assert_eq!(
            keyword,
            vec![
                theme.syntax.keyword.patch(theme.diff_removed_bg),
                theme.syntax.keyword.patch(theme.diff_added_bg),
            ]
        );
        assert_eq!(
            span_with(&rows, "\"two\"").style,
            theme.syntax.string.patch(theme.diff_added_bg)
        );
        assert_eq!(
            span_with(&rows, "+").style,
            theme.diff_added.patch(theme.diff_added_bg)
        );
        assert_eq!(span_with(&rows, "@@").style, theme.accent);
    }

    /// A file read shows numbered rows: the numbers stay dim, the code is
    /// highlighted in the language of the file the call read.
    #[test]
    fn a_read_file_is_highlighted_in_the_language_of_its_path() {
        let theme = truecolor();
        let rows = output(
            "read_file src/app.py:
1: def run():
2:     return 42",
            Some("src/app.py"),
            &theme,
        );
        assert_eq!(span_with(&rows, "def").style, theme.syntax.keyword);
        assert_eq!(span_with(&rows, "42").style, theme.syntax.number);
        assert_eq!(span_with(&rows, "1:").style, theme.dim);
        // The collapsed preview is highlighted the same way.
        let collapsed = render(
            &HistoryItem::ToolOutput {
                name: "read_file".to_owned(),
                text: "read_file src/app.py:
1: def run():
2:     return 42"
                    .to_owned(),
                path: Some("src/app.py".to_owned()),
            },
            80,
            &theme,
            Detail::Collapsed,
        );
        assert_eq!(
            plain_text(&collapsed),
            "  │ 1: def run():
  ╰─ 2:     return 42"
        );
        assert_eq!(span_with(&collapsed, "def").style, theme.syntax.keyword);
    }

    /// An edit reports the change as a numbered diff (`+ 3 code`).
    #[test]
    fn an_edit_diff_is_highlighted_behind_its_numbers() {
        let theme = truecolor();
        let rows = output(
            "edit_file a.py: h1 -> h2 (1 replacement(s))
  2 import os
- 3 x = 1
+ 3 x = 2",
            Some("a.py"),
            &theme,
        );
        assert_eq!(
            span_with(&rows, "2").style,
            theme.syntax.number.patch(theme.diff_added_bg)
        );
        assert_eq!(
            span_with(&rows, "+").style,
            theme.diff_added.patch(theme.diff_added_bg)
        );
        assert_eq!(span_with(&rows, "import").style, theme.syntax.keyword);
    }

    /// prime-agent's agent message row: who talked to whom and the start of
    /// it; details show the whole message under a gutter.
    #[test]
    fn an_agent_exchange_is_one_row_and_its_body_in_details() {
        let theme = Theme::plain();
        let item = HistoryItem::AgentExchange {
            from: "reviewer".to_owned(),
            to: "coder".to_owned(),
            text: "found two bugs\nline two".to_owned(),
        };
        let text = |detail| {
            super::render_fragment(&item, 80, &theme, detail, false)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        };
        let collapsed = text(Detail::Collapsed);
        assert_eq!(collapsed.len(), 1, "{collapsed:?}");
        assert!(
            collapsed[0].contains("◆ agent message · reviewer → coder · found two bugs…"),
            "{collapsed:?}"
        );
        let details = text(Detail::Details);
        assert!(
            details.iter().any(|row| row.contains("╰─ found two bugs")),
            "{details:?}"
        );
        assert!(
            details.iter().any(|row| row.contains("line two")),
            "{details:?}"
        );
    }

    /// Without a language nothing is guessed, and without colour nothing changes.
    #[test]
    fn output_without_a_language_or_colour_stays_as_it_was() {
        let theme = truecolor();
        let rows = output(
            "process ok
stdout:
fn main() {}",
            None,
            &theme,
        );
        assert_eq!(span_with(&rows, "fn").style, theme.muted);
        let plain = Theme::plain();
        let text = "read_file a.rs:
1: fn a() {}";
        assert_eq!(
            plain_text(&output(text, Some("a.rs"), &plain)),
            plain_text(&output(text, Some("a.rs"), &truecolor())),
            "highlighting never changes the text"
        );
        assert!(
            output(text, Some("a.rs"), &plain)
                .iter()
                .flat_map(|row| row.spans.iter())
                .all(|span| span.style.fg.is_none() && span.style.bg.is_none())
        );
    }

    /// A refinement is prime-agent's row: `◆ Harness refined` and its summary;
    /// the id and the edits, one per line, only in details and expanded. (The
    /// screenshot showed one notice paragraph with the id in front and every edit
    /// run together, cut mid-word.)
    #[test]
    fn a_refinement_is_a_header_a_summary_and_details_on_demand() {
        let item = HistoryItem::Refinement {
            header: "Harness refined".to_owned(),
            summary: "Consolidated the review into memory".to_owned(),
            details: vec![
                "Harness refined · 2 edits applied · Refinement refine_179 · local".to_owned(),
                "Updated local memory `review_v3`".to_owned(),
                "Created local prompt `verify_claims`".to_owned(),
            ],
        };
        let theme = Theme::plain();
        let collapsed = plain_text(&render(&item, 80, &theme, Detail::Collapsed));
        assert_eq!(
            collapsed,
            "\n  ◆ Harness refined\n  Consolidated the review into memory"
        );
        let details = plain_text(&render(&item, 80, &theme, Detail::Details));
        let rows = details.lines().collect::<Vec<_>>();
        assert!(
            rows.contains(&"  Updated local memory `review_v3`"),
            "{details}"
        );
        assert!(
            rows.contains(&"  Created local prompt `verify_claims`"),
            "{details}"
        );
        assert!(details.contains("refine_179"), "{details}");
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
