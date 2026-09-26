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
/// lines, and notices, errors and injected prompts are single styled lines. In
/// [`Detail::Expanded`] a tool is drawn as Claude Code's transcript view draws it:
/// its whole input and its whole output, each in a box.
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
            input,
            state,
        } => {
            let mut rows = vec![Line::default()];
            if detail == Detail::Expanded {
                rows.extend(expanded_tool(name, summary, input, state, width, theme));
            } else {
                rows.push(tool_card(name, summary, state, theme));
            }
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
        HistoryItem::ToolOutput { text, .. } if detail == Detail::Expanded => {
            output_box(text, width, theme)
        }
        HistoryItem::ToolOutput { text, .. } => {
            tool_output_rows(text, width, theme, TOOL_OUTPUT_PREVIEW_LINES)
        }
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
        // A notice can span lines (a sign-in shows its URL on a line of its own),
        // and a URL is drawn as a link so it stands out from the text around it.
        return message
            .lines()
            .flat_map(|line| {
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
                wrap_spans(spans, width)
            })
            .collect();
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

/// One logical row of an expanded box: its spans, and how many cells a row it
/// wraps onto is indented, so a long command continues under its own text
/// instead of under the `$ `.
type BoxRow = (Vec<Span<'static>>, usize);

/// Cells between the left edge and an expanded box, matching the tool card.
const BOX_INDENT: usize = 2;

/// The expanded form of a tool call, after Claude Code's transcript view (ctrl+o):
/// the card with a collapse chevron and the call's one-line title, then the whole
/// input in a box - a shell-like call as `$ command`, a replacement pair as a diff,
/// anything else as its arguments. Nothing in it is cut: expanded is where a reader
/// goes to see exactly what ran.
fn expanded_tool(
    name: &str,
    summary: &str,
    input: &str,
    state: &ToolState,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let arguments = serde_json::from_str::<serde_json::Value>(input).ok();
    let fields = arguments.as_ref().and_then(serde_json::Value::as_object);
    // A call that says what it is for (MCP tools and shell tools often carry a
    // `description`) is titled by that sentence, as Claude Code titles a Bash call;
    // otherwise the card's summary is the best one-line account there is.
    let title = fields
        .and_then(|fields| fields.get("description"))
        .and_then(serde_json::Value::as_str)
        .and_then(|text| text.lines().map(str::trim).find(|line| !line.is_empty()))
        .map_or_else(|| summary.to_owned(), str::to_owned);
    let mut header = running_card(name, &title, state, 1, theme);
    header.spans.insert(1, Span::styled("▾ ", theme.accent));
    let mut rows = vec![header];
    let body = input_rows(input, fields, theme);
    if !body.is_empty() {
        rows.extend(framed(None, body, width, theme));
    }
    rows
}

/// The rows of a call's input.
///
/// The only rules are about the shape of the arguments, never the tool's name: a
/// `command` (or an `executable` with its `args`) is what a shell-like tool runs and
/// is drawn as a prompt line; an `old_*`/`new_*` pair of strings is a replacement
/// and is drawn as the diff it makes; every other field is `key: value`, a
/// multi-line value on rows of its own. A `description` is already the title.
fn input_rows(
    input: &str,
    fields: Option<&serde_json::Map<String, serde_json::Value>>,
    theme: &Theme,
) -> Vec<BoxRow> {
    use serde_json::Value;
    let Some(fields) = fields else {
        // Not a JSON object: arguments that never became JSON, or a provider that
        // sends plain text. They are shown as they came.
        let raw = input.trim();
        return if raw.is_empty() {
            Vec::new()
        } else {
            text_rows(raw, theme.muted, 0, theme)
        };
    };
    let mut rows = Vec::new();
    let mut drawn = vec!["description".to_owned()];
    if let Some((command, used)) = shell_command(fields) {
        drawn.extend(used.iter().map(|key| (*key).to_owned()));
        for (index, line) in command.lines().enumerate() {
            let marker = if index == 0 { "$ " } else { "  " };
            rows.push((
                vec![
                    Span::styled(marker, theme.dim),
                    Span::styled(untab(line), theme.bash),
                ],
                2,
            ));
        }
    }
    for (key, value) in fields {
        let Some(rest) = key.strip_prefix("old_") else {
            continue;
        };
        let new_key = format!("new_{rest}");
        let (Some(old), Some(new)) = (value.as_str(), fields.get(&new_key).and_then(Value::as_str))
        else {
            continue;
        };
        for line in old.lines() {
            rows.push((
                vec![Span::styled(
                    format!("- {}", untab(line)),
                    theme.diff_removed,
                )],
                2,
            ));
        }
        for line in new.lines() {
            rows.push((
                vec![Span::styled(format!("+ {}", untab(line)), theme.diff_added)],
                2,
            ));
        }
        drawn.push(key.clone());
        drawn.push(new_key);
    }
    for (key, value) in fields {
        if value.is_null() || drawn.contains(key) {
            continue;
        }
        match value {
            Value::String(text) if text.contains('\n') => {
                rows.push((vec![Span::styled(format!("{key}:"), theme.dim)], 0));
                rows.extend(text_rows(text, theme.muted, 2, theme));
            }
            other => {
                let text = match other {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                rows.push((
                    vec![
                        Span::styled(format!("{key}: "), theme.dim),
                        Span::styled(untab(&text), theme.muted),
                    ],
                    2,
                ));
            }
        }
    }
    rows
}

/// What a shell-like call runs, and the fields that said so: a `command` string (or
/// a list of words), or an `executable` followed by its `args`.
fn shell_command(
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Option<(String, &'static [&'static str])> {
    use serde_json::Value;
    let words = |value: &Value| -> Option<String> {
        match value {
            Value::String(text) => Some(text.clone()),
            Value::Array(items) => items
                .iter()
                .map(|item| item.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .map(|words| words.join(" ")),
            _ => None,
        }
    };
    if let Some(command) = fields.get("command").and_then(words) {
        return Some((command, &["command"]));
    }
    let executable = fields.get("executable").and_then(Value::as_str)?;
    let args = fields.get("args").and_then(words).unwrap_or_default();
    let command = if args.is_empty() {
        executable.to_owned()
    } else {
        format!("{executable} {args}")
    };
    Some((command, &["executable", "args"]))
}

/// Rows for a block of text, `indent` cells in, every line kept - blank ones too,
/// since they are part of what a file or a command printed. Text that reads as a
/// diff is coloured by its line markers.
fn text_rows(text: &str, style: Style, indent: usize, theme: &Theme) -> Vec<BoxRow> {
    let diff = looks_like_diff(text);
    text.lines()
        .map(|line| {
            let line = untab(line.trim_end_matches('\r'));
            let style = if !diff {
                style
            } else if line.starts_with("+++") || line.starts_with("---") {
                theme.dim
            } else if line.starts_with('+') {
                theme.diff_added
            } else if line.starts_with('-') {
                theme.diff_removed
            } else if line.starts_with("@@") {
                theme.accent
            } else {
                style
            };
            let mut spans = Vec::new();
            if indent > 0 {
                spans.push(Span::raw(" ".repeat(indent)));
            }
            spans.push(Span::styled(line, style));
            (spans, indent)
        })
        .collect()
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

/// The whole of what a tool returned, in a box of its own under the call (the
/// expanded view): every line, wrapped rather than cut.
fn output_box(text: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let body = text.trim_end();
    let body = body.trim_start_matches(['\n', '\r']);
    if body.trim().is_empty() {
        return Vec::new();
    }
    framed(
        Some("output"),
        text_rows(body, theme.muted, 0, theme),
        width,
        theme,
    )
}

/// Frame rows in the single-line box the overlays draw (`┌─┐ │ └─┘`), two cells in
/// and as wide as the console, wrapping each row inside it.
///
/// Too narrow a console has no room for a frame; the rows then go out bare,
/// indented like collapsed output, so nothing is lost to the border.
fn framed(title: Option<&str>, rows: Vec<BoxRow>, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    const MIN_INNER: usize = 8;
    let width = usize::from(width);
    let indent = " ".repeat(BOX_INDENT);
    let inner = width.saturating_sub(BOX_INDENT + 4);
    if inner < MIN_INNER {
        let limit = width.saturating_sub(BOX_INDENT).max(1);
        return rows
            .into_iter()
            .flat_map(|(spans, hang)| wrap_words(spans, limit, hang))
            .map(|line| {
                let mut spans = vec![Span::raw(indent.clone())];
                spans.extend(line.spans);
                Line::from(spans)
            })
            .collect();
    }
    let rule = |cells: usize| "─".repeat(cells);
    let mut top = vec![Span::styled(format!("{indent}┌"), theme.border)];
    let mut used = 0;
    if let Some(title) = title {
        let label = format!(" {title} ");
        used = super::widgets::composer::display_width(&label);
        top.push(Span::styled(label, theme.dim));
    }
    top.push(Span::styled(
        format!("{}┐", rule((inner + 2).saturating_sub(used))),
        theme.border,
    ));
    let mut lines = vec![Line::from(top)];
    for (spans, hang) in rows {
        for row in wrap_words(spans, inner, hang) {
            let cells = row
                .spans
                .iter()
                .flat_map(|span| span.content.chars())
                .map(super::widgets::composer::char_width)
                .sum::<usize>();
            let mut spans = vec![Span::styled(format!("{indent}│ "), theme.border)];
            spans.extend(row.spans);
            spans.push(Span::raw(" ".repeat(inner.saturating_sub(cells))));
            spans.push(Span::styled(" │", theme.border));
            lines.push(Line::from(spans));
        }
    }
    lines.push(Line::from(Span::styled(
        format!("{indent}└{}┘", rule(inner + 2)),
        theme.border,
    )));
    lines
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
        assert!(user.contains("  sửa lỗi parser"), "{user}");
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
                input: String::new(),
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
            "Sign in:\nhttps://auth.example/authorize?x=1\nEsc cancels."
        );
        let link = &rows[1].spans[0];
        assert!(
            link.style.add_modifier.contains(Modifier::UNDERLINED),
            "{link:?}"
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
            plain_text(&render(&output, 80, &theme, Detail::Details)),
            "  1\n  2\n  3\n  … 1 more lines"
        );
        assert_eq!(
            plain_text(&render(&output, 20, &theme, Detail::Expanded)),
            [
                "  ┌ output ────────┐",
                "  │ t:             │",
                "  │ 1              │",
                "  │ 2              │",
                "  │ 3              │",
                "  │ 4              │",
                "  └────────────────┘",
            ]
            .join("\n")
        );
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

    fn cells(text: &str) -> usize {
        crate::interactive::tui::widgets::composer::display_width(text)
    }

    /// Expanded (ctrl+o), as Claude Code's transcript view: a chevron card, then
    /// the whole command in a box - `$ `, wrapped under its own text, never cut -
    /// and the other arguments after it. Every row fits the console exactly.
    #[test]
    fn an_expanded_shell_call_shows_its_whole_command_in_a_box() {
        let text = plain_text(&render(
            &shell_call(),
            48,
            &Theme::plain(),
            Detail::Expanded,
        ));
        let rows = text.lines().collect::<Vec<_>>();
        assert_eq!(rows[0], "", "a blank row separates calls");
        assert!(
            rows[1].starts_with("  ▾ run_shell · done · command=cargo test"),
            "{text}"
        );
        assert!(rows[1].ends_with(" · 1.2s"), "{text}");
        assert_eq!(rows[2], format!("  ┌{}┐", "─".repeat(44)), "{text}");
        assert_eq!(
            inside(&rows[3..rows.len() - 1]),
            [
                "$ cargo test --workspace --locked",
                "  --no-fail-fast -- --test-threads=1",
                "timeout_ms: 60000",
            ],
            "{text}"
        );
        assert_eq!(rows[rows.len() - 1], format!("  └{}┘", "─".repeat(44)));
        for row in &rows[2..] {
            assert_eq!(cells(row), 48, "{row:?}");
        }
    }

    /// The text of framed rows, without the frame and the padding.
    fn inside(rows: &[&str]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                row.strip_prefix("  │ ")
                    .and_then(|row| row.strip_suffix(" │"))
                    .unwrap_or_else(|| panic!("not a framed row: {row:?}"))
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    /// Collapsed and details keep today's one-line card: the full input is only
    /// drawn in expanded mode.
    #[test]
    fn the_full_input_only_shows_in_expanded_mode() {
        for detail in [Detail::Collapsed, Detail::Details] {
            let text = plain_text(&render(&shell_call(), 100, &Theme::plain(), detail));
            assert_eq!(
                text,
                "\n  run_shell · done · command=cargo test --workspace --locked timeout_ms=60000 · 1.2s"
            );
        }
    }

    /// A call's `description` is its title, and it is not repeated in the box.
    #[test]
    fn a_described_call_is_titled_by_its_description() {
        let text = plain_text(&render(
            &HistoryItem::Tool {
                name: "Bash".to_owned(),
                summary: "command=ls description=List files".to_owned(),
                input: r#"{"command":"ls -la","description":"List files"}"#.to_owned(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(5),
                },
            },
            40,
            &Theme::plain(),
            Detail::Expanded,
        ));
        assert!(text.contains("▾ Bash · done · List files · 5ms"), "{text}");
        assert!(text.contains("│ $ ls -la"), "{text}");
        assert_eq!(text.matches("List files").count(), 1, "{text}");
    }

    /// A file tool shows its arguments as `key: value`; its output box holds every
    /// line, blank ones included, where collapsed mode would stop after three.
    #[test]
    fn an_expanded_file_call_shows_its_arguments_and_all_of_its_output() {
        let theme = Theme::plain();
        let call = plain_text(&render(
            &HistoryItem::Tool {
                name: "read_file".to_owned(),
                summary: "path=src/lib.rs offset=10".to_owned(),
                input: r#"{"path":"src/lib.rs","offset":10,"limit":null}"#.to_owned(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(12),
                },
            },
            40,
            &theme,
            Detail::Expanded,
        ));
        assert!(call.contains("│ offset: 10"), "{call}");
        assert!(call.contains("│ path: src/lib.rs"), "{call}");
        assert!(
            !call.contains("limit"),
            "a null argument was never sent: {call}"
        );
        let output = plain_text(&render(
            &HistoryItem::ToolOutput {
                name: "read_file".to_owned(),
                text: "read_file src/lib.rs:\nfn a() {}\n\nfn b() {}\nfn c() {}\nfn d() {}\n"
                    .to_owned(),
            },
            40,
            &theme,
            Detail::Expanded,
        ));
        let rows = output.lines().collect::<Vec<_>>();
        assert_eq!(
            inside(&rows[1..rows.len() - 1]),
            [
                "read_file src/lib.rs:",
                "fn a() {}",
                "",
                "fn b() {}",
                "fn c() {}",
                "fn d() {}"
            ],
            "{output}"
        );
    }

    /// A replacement pair is drawn as the diff it makes, in the diff colours.
    #[test]
    fn an_edit_shows_the_diff_it_makes() {
        let theme = Theme::colored();
        let rows = render(
            &HistoryItem::Tool {
                name: "edit_file".to_owned(),
                summary: "path=a.rs".to_owned(),
                input: r#"{"path":"a.rs","old_string":"let x = 1;","new_string":"let x = 2;\nlet y = 3;"}"#
                    .to_owned(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(3),
                },
            },
            60,
            &theme,
            Detail::Expanded,
        );
        let text = plain_text(&rows);
        assert!(text.contains("│ - let x = 1;"), "{text}");
        assert!(text.contains("│ + let x = 2;"), "{text}");
        assert!(text.contains("│ + let y = 3;"), "{text}");
        assert!(!text.contains("old_string"), "{text}");
        let added = rows
            .iter()
            .flat_map(|row| row.spans.iter())
            .find(|span| span.content.starts_with('+'))
            .expect("added line");
        assert_eq!(added.style, theme.diff_added);
    }

    /// Vietnamese and CJK text wraps by cells, so the right border stays in line.
    #[test]
    fn a_box_measures_wide_text_in_cells() {
        let text = plain_text(&render(
            &HistoryItem::ToolOutput {
                name: "t".to_owned(),
                text: "sửa lỗi phân tích cú pháp 解析器错误 解析器错误".to_owned(),
            },
            24,
            &Theme::plain(),
            Detail::Expanded,
        ));
        for row in text.lines() {
            assert_eq!(cells(row), 24, "{row:?} in\n{text}");
        }
    }

    /// Too narrow for a frame, the input still shows, indented and unframed.
    #[test]
    fn a_narrow_console_drops_the_frame_not_the_input() {
        let text = plain_text(&render(
            &shell_call(),
            12,
            &Theme::plain(),
            Detail::Expanded,
        ));
        assert!(!text.contains('┌'), "{text}");
        assert!(text.contains("\n  $ cargo"), "{text}");
        assert!(text.contains("timeout_ms"), "{text}");
        for row in text.lines().skip(2) {
            assert!(cells(row) <= 12, "{row:?}");
        }
    }

    /// Arguments that never became JSON are shown as they came.
    #[test]
    fn input_that_is_not_json_is_shown_verbatim() {
        let text = plain_text(&render(
            &HistoryItem::Tool {
                name: "t".to_owned(),
                summary: String::new(),
                input: "{\"path\": \"a".to_owned(),
                state: ToolState::Failed {
                    elapsed: Duration::from_millis(1),
                    detail: "invalid_payload".to_owned(),
                },
            },
            40,
            &Theme::plain(),
            Detail::Expanded,
        ));
        assert!(text.contains("│ {\"path\": \"a"), "{text}");
        assert!(text.contains("invalid_payload"), "{text}");
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
