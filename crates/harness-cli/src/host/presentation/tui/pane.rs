//! A delegate's pane, beside the conversation as herdr splits a pane: what the
//! agent the run delegated to is doing, in a box drawn in the accent like
//! herdr's focused pane. Its latest message sits at the top; under it, newest
//! at the bottom, each call it made is one row in the transcript's card style -
//! its kind, its target (a path loses its middle, not its file name), its
//! outcome and how long it took - with at most two rows of what it returned.
//! ctrl+o's detail modes open the output, as they do the transcript's.
//!
//! ```text
//! ╭─ 2 explorer-d3cc6099 · working · 7 tools ────╮
//! │ The order flow validates the cart, then      │
//! │ saves it in one transaction.                 │
//! │ ──────────────────────────────────────────── │
//! │ ▤ Read file  src/…/orders/save.ts    ✓ 21ms  │
//! │   │ export function saveOrder(               │
//! │   │ import { db } from "../db";              │
//! │   ╰─ … 6 more lines                          │
//! │ ⌕ Search text  saveOrder               ⣾     │
//! ╰───────────────────────────────── alt+1 back ─╯
//! ```

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use super::card::{RAIL, RAIL_END};
use super::icons::ToolKind;
use super::sidebar::Status;
use super::theme::Theme;
use super::widgets::composer::{char_width, display_width};
use crate::interactive::events::{ChildActivity, ChildLine, ChildLineKind, Detail};
use crate::interactive::view;

/// The pane's columns beside a conversation `columns` wide, or none when the
/// conversation would be left too narrow to read.
#[must_use]
pub const fn width(columns: u16) -> u16 {
    if columns < 72 {
        return 0;
    }
    let share = columns * 2 / 5;
    if share < 32 {
        32
    } else if share > 64 {
        64
    } else {
        share
    }
}

/// Rows of a call's output a collapsed pane shows.
const OUTPUT_ROWS: usize = 2;
/// Rows of the latest message the top of the pane keeps.
const MESSAGE_ROWS: usize = 3;

/// What the pane draws: the child, its state, its sidebar number, the detail
/// mode, and the spinner's frame.
#[derive(Clone, Copy, Debug)]
pub struct View<'a> {
    pub activity: &'a ChildActivity,
    pub status: Status,
    pub number: usize,
    pub detail: Detail,
    pub tick: u64,
}

/// Draw the child's pane into `area`.
pub fn render(frame: &mut Frame, area: Rect, pane: View<'_>, theme: &Theme) {
    if area.width < 8 || area.height < 3 {
        return;
    }
    let activity = pane.activity;
    let calls = format!(
        "{} tool{}",
        activity.tool_calls,
        if activity.tool_calls == 1 { "" } else { "s" }
    );
    let mut title = vec![
        Span::styled(format!(" {} ", pane.number), theme.dim),
        Span::styled(activity.name.clone(), theme.strong),
        Span::styled(" · ", theme.dim),
        Span::styled(pane.status.word(), pane.status.style(theme)),
        Span::styled(format!(" · {calls} "), theme.dim),
    ];
    // A narrow pane keeps the name and the state; the sidebar counts the tools.
    let title_cells: usize = title.iter().map(|span| display_width(&span.content)).sum();
    if title_cells + 2 > usize::from(area.width) {
        title.pop();
        title.push(Span::raw(" "));
    }
    let title = Line::from(title);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.accent)
        .title(title)
        .title_bottom(Line::from(Span::styled(" alt+1 back ", theme.dim)).right_aligned());
    // herdr's pane footer: the model and thinking level the agent runs on,
    // and the branch and worktree of a coder that works in one of its own.
    let model = activity.model_label();
    let workplace = super::sidebar::place(activity.branch.as_deref(), activity.worktree.as_deref());
    if !model.is_empty() || !workplace.is_empty() {
        let mut room = usize::from(area.width).saturating_sub(18);
        let mut footer = Vec::new();
        if !model.is_empty() {
            let model = super::sidebar::model_effort(activity, room);
            room = room.saturating_sub(display_width(&model) + 3);
            footer.push(Span::styled(" ◆ ", theme.accent));
            footer.push(Span::styled(format!("{model} "), theme.muted));
        }
        if !workplace.is_empty() && room > 4 {
            let workplace: String = workplace.chars().take(room - 4).collect();
            footer.push(Span::styled(format!(" ⎇ {workplace} "), theme.branch));
        }
        block = block.title_bottom(Line::from(footer).left_aligned());
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = lines(
        pane,
        usize::from(inner.width),
        usize::from(inner.height),
        theme,
    );
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One thing the child did, as the pane lists it.
#[derive(Debug, Default)]
struct Call<'a> {
    id: &'a str,
    name: &'a str,
    summary: &'a str,
    output: Vec<&'a str>,
    /// `Some((ok, millis, why))` once it settled.
    settled: Option<(bool, u64, &'a str)>,
}

#[derive(Debug)]
enum Entry<'a> {
    Call(Call<'a>),
    Notice(&'a str),
    Text(Vec<&'a str>),
}

/// The child's lines as calls, notices and messages, oldest first. A line of
/// output or an outcome joins its call by id; one without an id joins the
/// newest call.
fn entries(recent: &[ChildLine]) -> Vec<Entry<'_>> {
    let mut entries: Vec<Entry<'_>> = Vec::new();
    let call_of = |entries: &mut Vec<Entry<'_>>, id: &str| -> Option<usize> {
        entries.iter().rposition(|entry| match entry {
            Entry::Call(call) => id.is_empty() || call.id == id,
            _ => false,
        })
    };
    for line in recent {
        match line.kind {
            ChildLineKind::Call => {
                let (name, summary) = line
                    .text
                    .split_once(": ")
                    .unwrap_or((line.text.as_str(), ""));
                entries.push(Entry::Call(Call {
                    id: &line.call,
                    name,
                    summary,
                    ..Call::default()
                }));
            }
            ChildLineKind::Output => match call_of(&mut entries, &line.call) {
                Some(index) => {
                    if let Entry::Call(call) = &mut entries[index] {
                        call.output.push(&line.text);
                    }
                }
                None => entries.push(Entry::Notice(&line.text)),
            },
            ChildLineKind::Settled => {
                if let Some(index) = call_of(&mut entries, &line.call)
                    && let Entry::Call(call) = &mut entries[index]
                {
                    call.settled = Some(settled(&line.text));
                }
            }
            ChildLineKind::Failed => entries.push(Entry::Notice(&line.text)),
            ChildLineKind::Text => match entries.last_mut() {
                Some(Entry::Text(text)) => text.push(&line.text),
                _ => entries.push(Entry::Text(vec![&line.text])),
            },
        }
    }
    entries
}

/// `ok 206` or `failed 1200: why`, as the child's observer writes it.
fn settled(text: &str) -> (bool, u64, &str) {
    let (head, why) = text.split_once(": ").unwrap_or((text, ""));
    let mut words = head.split_whitespace();
    let ok = words.next() == Some("ok");
    let millis = words.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    (ok, millis, why)
}

/// The pane's rows, `width` cells each, at most `height`: the latest message,
/// a rule, then the newest calls that fit.
fn lines(pane: View<'_>, width: usize, height: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut entries = entries(&pane.activity.recent);
    // The newest message heads the pane, not the bottom of its list.
    let latest = entries
        .iter()
        .rposition(|entry| matches!(entry, Entry::Text(_)))
        .map(|index| entries.remove(index));
    let mut top: Vec<Line<'static>> = Vec::new();
    if let Some(Entry::Text(text)) = latest {
        let wrapped = super::markdown::wrap_spans(
            vec![Span::styled(text.join(" "), theme.assistant)],
            u16::try_from(width).unwrap_or(u16::MAX),
        );
        let keep = if pane.detail == Detail::Collapsed {
            MESSAGE_ROWS
        } else {
            height / 2
        };
        // A long message keeps its newest rows: it is still being written.
        let skip = wrapped.len().saturating_sub(keep.max(1));
        top.extend(wrapped.into_iter().skip(skip));
        top.push(Line::from(Span::styled("─".repeat(width), theme.border)));
    }
    let mut body: Vec<Line<'static>> = Vec::new();
    let collapsed = pane.detail == Detail::Collapsed;
    let mut index = 0;
    while index < entries.len() {
        let entry = &entries[index];
        index += 1;
        match entry {
            // herdr's run of one tool: `Read 4 files`, each target under it.
            Entry::Call(call) if collapsed && groupable(call) => {
                let mut run = vec![call];
                while let Some(Entry::Call(next)) = entries.get(index)
                    && next.name == call.name
                    && groupable(next)
                {
                    run.push(next);
                    index += 1;
                }
                if run.len() == 1 {
                    body.extend(call_rows(call, pane, width, theme));
                } else {
                    body.extend(group_rows(&run, width, theme));
                }
            }
            Entry::Call(call) => body.extend(call_rows(call, pane, width, theme)),
            Entry::Notice(text) => body.push(fit(
                vec![Span::styled(format!("! {text}"), theme.error)],
                width,
            )),
            Entry::Text(text) => {
                if pane.detail == Detail::Collapsed {
                    body.push(fit(
                        vec![
                            Span::styled("✦ ", theme.dim),
                            Span::styled(text.join(" "), theme.muted),
                        ],
                        width,
                    ));
                } else {
                    body.extend(super::markdown::wrap_spans(
                        vec![Span::styled(text.join(" "), theme.muted)],
                        u16::try_from(width).unwrap_or(u16::MAX),
                    ));
                }
            }
        }
    }
    if body.is_empty() && top.is_empty() {
        body.push(Line::from(Span::styled(
            if pane.activity.queued {
                "queued: waiting for a place to run ([agents] max_workers)"
            } else {
                "waiting for its first call…"
            },
            theme.dim,
        )));
    }
    // In a short pane the message gives up its oldest rows to the calls, down
    // to half the pane; below two rows it goes, rule and all.
    let top_room = height.saturating_sub(body.len().min(height / 2));
    if top.len() > top_room {
        if top_room >= 2 {
            top.drain(..top.len() - top_room);
        } else {
            top.clear();
        }
    }
    let skip = body.len().saturating_sub(height - top.len());
    top.extend(body.into_iter().skip(skip));
    top
}

/// A call's header row, its failure, and what it returned.
fn call_rows(call: &Call<'_>, pane: View<'_>, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let kind = ToolKind::of(call.name);
    let base = kind.style(theme);
    let (tail, tail_style) = match call.settled {
        Some((true, millis, _)) => (format!("✓ {}", label(millis)), theme.tool_ok),
        Some((false, millis, _)) => (format!("✗ {}", label(millis)), theme.tool_failed),
        None if pane.status == Status::Working => (Theme::spinner(pane.tick).to_owned(), base),
        // It stopped before the call settled.
        None => ("·".to_owned(), theme.dim),
    };
    let (title, arguments) = super::card::call_parts(call.name, call.summary);
    let head = format!("{} {title}", kind.glyph());
    let tail_cells = display_width(&tail);
    let mut spans = vec![Span::styled(
        head.clone(),
        base.add_modifier(Modifier::BOLD),
    )];
    let mut used = display_width(&head);
    // The target first: a path keeps its file name, losing its middle.
    let room = width.saturating_sub(used + tail_cells + 3);
    if let Some(target) = &arguments.target
        && room > 1
    {
        let target = target.replace(['\n', '\r'], " ");
        let shown = if looks_like_path(&target) {
            middle(&target, room)
        } else {
            end(&target, room)
        };
        used += 2 + display_width(&shown);
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            shown,
            theme.strong.remove_modifier(Modifier::BOLD),
        ));
    }
    let room = width.saturating_sub(used + tail_cells + 3);
    if !arguments.rest.is_empty() && room > 3 {
        let rest = end(&arguments.rest.join(" ").replace(['\n', '\r'], " "), room);
        used += 1 + display_width(&rest);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(rest, theme.dim));
    }
    spans.push(Span::raw(
        " ".repeat(width.saturating_sub(used + tail_cells).max(1)),
    ));
    spans.push(Span::styled(tail, tail_style));
    let mut rows = vec![fit(spans, width)];
    if let Some((false, _, why)) = call.settled
        && !why.is_empty()
    {
        rows.push(fit(
            vec![
                Span::styled(RAIL, theme.error),
                Span::styled(why.to_owned(), theme.error),
            ],
            width,
        ));
    }
    // The observer sends a few lines and counts the rest (`… 12 more lines`).
    let (mut output, mut more) = (call.output.clone(), 0_usize);
    if let Some(last) = output.last()
        && let Some(count) = last
            .strip_prefix("… ")
            .and_then(|rest| rest.strip_suffix(" more lines"))
            .and_then(|count| count.parse::<usize>().ok())
    {
        more = count;
        output.pop();
    }
    let shown = match pane.detail {
        Detail::Collapsed => OUTPUT_ROWS,
        Detail::Details | Detail::Expanded => output.len(),
    };
    more += output.len().saturating_sub(shown);
    let rail = super::card::RAIL_CELLS;
    for line in output.iter().take(shown) {
        rows.push(fit(
            vec![
                Span::styled(RAIL, base),
                Span::styled((*line).to_owned(), theme.dim),
            ],
            width.max(rail + 1),
        ));
    }
    if more > 0 {
        rows.push(fit(
            vec![
                Span::styled(RAIL_END, base),
                Span::styled(
                    format!("… {more} more line{}", if more == 1 { "" } else { "s" }),
                    theme.dim,
                ),
            ],
            width,
        ));
    }
    rows
}

/// A call that can join a run of its tool: it settled, and it read rather
/// than printed what matters (a failure, or a command's output, stays a card).
fn groupable(call: &Call<'_>) -> bool {
    matches!(call.settled, Some((true, _, _)))
        && matches!(
            ToolKind::of(call.name),
            ToolKind::Read | ToolKind::Search | ToolKind::List
        )
}

/// Targets a run of calls lists before it counts the rest.
const RUN_TARGETS: usize = 4;

/// herdr's run of one tool's calls: `▤ Read file ×4`, its time at the right,
/// then each call's target under it, a path losing its middle.
fn group_rows(run: &[&Call<'_>], width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let first = run[0];
    let kind = ToolKind::of(first.name);
    let base = kind.style(theme);
    let (title, _) = super::card::call_parts(first.name, first.summary);
    let millis = run
        .iter()
        .filter_map(|call| call.settled.map(|(_, millis, _)| millis))
        .sum();
    let tail = format!("✓ {}", label(millis));
    let head = format!("{} {title} ×{}", kind.glyph(), run.len());
    let fill = width
        .saturating_sub(display_width(&head) + display_width(&tail))
        .max(1);
    let mut rows = vec![fit(
        vec![
            Span::styled(head, base.add_modifier(Modifier::BOLD)),
            Span::raw(" ".repeat(fill)),
            Span::styled(tail, theme.tool_ok),
        ],
        width,
    )];
    let room = width.saturating_sub(4);
    for (index, call) in run.iter().take(RUN_TARGETS).enumerate() {
        let (_, arguments) = super::card::call_parts(call.name, call.summary);
        let target = arguments
            .target
            .or_else(|| arguments.rest.first().cloned())
            .unwrap_or_default()
            .replace(['\n', '\r'], " ");
        let shown = if looks_like_path(&target) {
            middle(&target, room)
        } else {
            end(&target, room)
        };
        rows.push(Line::from(vec![
            Span::styled(if index == 0 { "  ↳ " } else { "    " }, theme.dim),
            Span::styled(shown, theme.muted),
        ]));
    }
    if run.len() > RUN_TARGETS {
        rows.push(Line::from(Span::styled(
            format!("    … {} more (ctrl+o)", run.len() - RUN_TARGETS),
            theme.dim,
        )));
    }
    rows
}

fn label(millis: u64) -> String {
    view::seconds_label(std::time::Duration::from_millis(millis))
}

/// A target that names a file or a folder.
fn looks_like_path(text: &str) -> bool {
    !text.contains(' ') && (text.contains('/') || text.contains('\\'))
}

/// `text` in `room` cells, cut at the end.
fn end(text: &str, room: usize) -> String {
    if display_width(text) <= room {
        return text.to_owned();
    }
    let mut kept = String::new();
    let mut used = 0;
    for character in text.chars() {
        let cells = char_width(character);
        if used + cells + 1 > room {
            break;
        }
        kept.push(character);
        used += cells;
    }
    kept.push('…');
    kept
}

/// A path in `room` cells, cut in the middle: its first folders and as many
/// of its last parts as fit, the file name whole whenever it fits.
fn middle(path: &str, room: usize) -> String {
    if display_width(path) <= room {
        return path.to_owned();
    }
    let separators = ['/', '\\'];
    // The tail: whole trailing parts, at most two thirds of the room.
    let mut tail_start = path.len();
    for (index, _) in path.rmatch_indices(separators) {
        if display_width(&path[index..]) + 1 > room * 2 / 3 && tail_start < path.len() {
            break;
        }
        tail_start = index;
        if display_width(&path[index..]) + 1 > room * 2 / 3 {
            break;
        }
    }
    let tail = &path[tail_start..];
    let tail_cells = display_width(tail);
    if tail_cells + 1 >= room {
        // Not even the file name fits: keep its end.
        let mut kept: Vec<char> = Vec::new();
        let mut used = 1;
        for character in tail.chars().rev() {
            let cells = char_width(character);
            if used + cells > room {
                break;
            }
            kept.push(character);
            used += cells;
        }
        return std::iter::once('…').chain(kept.into_iter().rev()).collect();
    }
    let mut head = String::new();
    let mut used = tail_cells + 1;
    for character in path.chars() {
        let cells = char_width(character);
        if used + cells > room {
            break;
        }
        head.push(character);
        used += cells;
    }
    format!("{head}…{tail}")
}

/// `spans` cut to `width` cells, an ellipsis standing in for what was cut.
fn fit(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let mut used = 0;
    let mut kept = Vec::new();
    for span in spans {
        let cells = display_width(&span.content);
        if used + cells <= width {
            used += cells;
            kept.push(span);
            continue;
        }
        let mut text = String::new();
        for character in span.content.chars() {
            let cells = char_width(character);
            if used + cells + 1 > width {
                break;
            }
            text.push(character);
            used += cells;
        }
        text.push('…');
        kept.push(Span::styled(text, span.style));
        break;
    }
    Line::from(kept)
}

#[cfg(test)]
mod tests {
    use super::{Status, View, middle, render, width};
    use crate::interactive::events::{ChildActivity, ChildLine, ChildLineKind, Detail};
    use crate::interactive::tui::theme::Theme;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    fn line(kind: ChildLineKind, call: &str, text: &str) -> ChildLine {
        ChildLine {
            kind,
            text: text.to_owned(),
            call: call.to_owned(),
        }
    }

    fn screen(activity: &ChildActivity, detail: Detail, columns: u16, rows: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(columns, rows)).expect("terminal");
        terminal
            .draw(|frame| {
                render(
                    frame,
                    Rect::new(0, 0, columns, rows),
                    View {
                        activity,
                        status: Status::Working,
                        number: 2,
                        detail,
                        tick: 0,
                    },
                    &Theme::plain(),
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (0..rows)
            .map(|row| {
                (0..columns)
                    .map(|col| buffer[(col, row)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn it_takes_two_fifths_of_a_wide_conversation_and_none_of_a_narrow_one() {
        assert_eq!(width(71), 0);
        assert_eq!(width(80), 32);
        assert_eq!(width(120), 48);
        assert_eq!(width(240), 64);
    }

    /// Seen in a real run: `3 explorer · working · 49 tools`, every call's
    /// output cut at the pane's edge and a long one filling the pane. A call
    /// is one row with its outcome and time, its output two rows and a count,
    /// and the child's latest message heads the pane.
    #[test]
    fn each_call_is_one_row_with_its_outcome_and_a_folded_output() {
        let output = (1..=8)
            .map(|n| format!("{n}: line {n} of the file"))
            .chain(std::iter::once("… 40 more lines".to_owned()));
        let mut recent = vec![
            line(ChildLineKind::Call, "c0", "run_shell: command=ls"),
            line(ChildLineKind::Settled, "c0", "ok 3"),
            line(
                ChildLineKind::Call,
                "c1",
                "read_file: path=crates/harness-cli/src/host/presentation/tui/pane.rs limit=80",
            ),
        ];
        recent.extend(output.map(|text| line(ChildLineKind::Output, "c1", &text)));
        recent.extend([
            line(ChildLineKind::Settled, "c1", "ok 206"),
            line(ChildLineKind::Call, "c2", "run_shell: command=cargo test"),
            line(
                ChildLineKind::Settled,
                "c2",
                "failed 1200: denied by policy",
            ),
            line(
                ChildLineKind::Text,
                "",
                "The order flow validates the cart,",
            ),
            line(ChildLineKind::Text, "", "then saves it in one transaction."),
            line(ChildLineKind::Call, "c3", "search_text: query=saveOrder"),
        ]);
        let activity = ChildActivity {
            name: "explorer-d3cc6099".to_owned(),
            tool_calls: 49,
            recent,
            model: "opencode-go/deepseek-v4.1-flash".to_owned(),
            thinking: Some("high".to_owned()),
            ..ChildActivity::default()
        };
        let rows = screen(&activity, Detail::Collapsed, 48, 16);
        let text = rows.join("\n");
        assert!(
            rows[0].contains("2 explorer-d3cc6099 · working · 49 tools"),
            "{text}"
        );
        assert!(
            rows[1].contains("The order flow validates the cart,"),
            "the latest message heads the pane: {text}"
        );
        let read = rows
            .iter()
            .find(|row| row.contains("Read file") && row.contains("pane.rs"))
            .unwrap_or_else(|| panic!("the call keeps its file name: {text}"));
        assert!(read.contains("crates/"), "{text}");
        assert!(read.contains('…'), "the path loses its middle: {text}");
        assert!(read.contains("✓ 206ms"), "{text}");
        assert!(text.contains("│ 1: line 1 of the file"), "{text}");
        assert!(text.contains("│ 2: line 2 of the file"), "{text}");
        assert!(!text.contains("3: line 3"), "two rows of output: {text}");
        assert!(text.contains("╰─ … 46 more lines"), "{text}");
        assert!(text.contains("✗ 1.2s"), "{text}");
        assert!(text.contains("denied by policy"), "{text}");
        assert!(
            rows.iter()
                .any(|row| row.contains("Search text") && row.contains("saveOrder")),
            "the running call is the newest row: {text}"
        );
        assert!(rows[15].contains("alt+1 back"), "{text}");
        assert!(
            rows[15].contains("◆ deepseek-v4.1-flash high"),
            "the footer says the model and thinking level: {text}"
        );

        // ctrl+o opens a call's output.
        let text = screen(&activity, Detail::Details, 48, 24).join("\n");
        assert!(text.contains("8: line 8 of the file"), "{text}");
        assert!(text.contains("… 40 more lines"), "{text}");
    }

    /// A coder in a worktree of its own says which: its branch and folder
    /// follow the model in the footer. One in the user's checkout says none.
    #[test]
    fn a_coder_in_its_own_worktree_names_its_branch_and_folder() {
        let coder = ChildActivity {
            name: "coder-4b27c5cd".to_owned(),
            model: "openai-codex/gpt-6.1-sol".to_owned(),
            thinking: Some("high".to_owned()),
            branch: Some("ha/coder-4b27c5cd".to_owned()),
            worktree: Some("wt-4b27c5cd".to_owned()),
            ..ChildActivity::default()
        };
        let rows = screen(&coder, Detail::Collapsed, 72, 8);
        let text = rows.join("\n");
        assert!(rows[7].contains("◆ gpt-6.1-sol high"), "{text}");
        assert!(
            rows[7].contains("⎇ ha/coder-4b27c5cd · wt-4b27c5cd"),
            "{text}"
        );
        let explorer = ChildActivity {
            branch: None,
            worktree: None,
            ..coder
        };
        let text = screen(&explorer, Detail::Collapsed, 72, 8).join("\n");
        assert!(!text.contains('⎇'), "{text}");
    }

    /// herdr's run of one tool: `Read 4 files` and the paths under it, not
    /// four cards with the files' first lines.
    #[test]
    fn a_run_of_reads_is_one_row_and_its_paths() {
        let mut recent = Vec::new();
        for n in 0..6 {
            let call = format!("c{n}");
            recent.push(line(
                ChildLineKind::Call,
                &call,
                &format!("read_file: path=crates/harness-cli/src/host/presentation/tui/file{n}.rs"),
            ));
            recent.push(line(ChildLineKind::Output, &call, "use std::io;"));
            recent.push(line(ChildLineKind::Settled, &call, "ok 100"));
        }
        recent.push(line(
            ChildLineKind::Call,
            "c9",
            "run_shell: command=cargo test",
        ));
        recent.push(line(ChildLineKind::Output, "c9", "test result: ok"));
        recent.push(line(ChildLineKind::Settled, "c9", "ok 4000"));
        let activity = ChildActivity {
            name: "explorer-d3cc6099".to_owned(),
            tool_calls: 7,
            recent,
            ..ChildActivity::default()
        };
        let rows = screen(&activity, Detail::Collapsed, 48, 14);
        let text = rows.join("\n");
        let head = rows
            .iter()
            .find(|row| row.contains("Read file ×6"))
            .unwrap_or_else(|| panic!("{text}"));
        assert!(head.contains("✓ 600ms"), "{text}");
        assert!(text.contains("↳ crates/"), "{text}");
        assert!(
            text.contains("file0.rs") && text.contains("file3.rs"),
            "{text}"
        );
        assert!(
            !text.contains("file4.rs"),
            "four paths, then a count: {text}"
        );
        assert!(text.contains("… 2 more (ctrl+o)"), "{text}");
        assert!(
            !text.contains("use std::io;"),
            "a read's content stays folded: {text}"
        );
        assert!(
            text.contains("│ test result: ok"),
            "a command keeps its output: {text}"
        );

        // ctrl+o shows every call.
        let text = screen(&activity, Detail::Details, 48, 40).join("\n");
        assert!(
            text.contains("file5.rs") && text.contains("use std::io;"),
            "{text}"
        );
    }

    #[test]
    fn a_short_pane_keeps_the_newest_calls() {
        let recent = (0..20)
            .flat_map(|n| {
                [
                    line(
                        ChildLineKind::Call,
                        &format!("c{n}"),
                        &format!("run_shell: command=touch f{n}.rs"),
                    ),
                    line(ChildLineKind::Settled, &format!("c{n}"), "ok 1"),
                ]
            })
            .collect();
        let activity = ChildActivity {
            name: "coder-c0cc21d5".to_owned(),
            tool_calls: 20,
            recent,
            ..ChildActivity::default()
        };
        let text = screen(&activity, Detail::Collapsed, 40, 8).join("\n");
        assert!(text.contains("f19.rs"), "{text}");
        assert!(!text.contains("f10.rs"), "{text}");
    }

    #[test]
    fn a_path_loses_its_middle_not_its_file_name() {
        let path = "crates/harness-cli/src/host/presentation/tui/pane.rs";
        let shown = middle(path, 30);
        assert!(shown.starts_with("crates/"), "{shown}");
        assert!(shown.ends_with("/tui/pane.rs"), "{shown}");
        assert!(shown.chars().count() <= 30, "{shown}");
        assert_eq!(middle("src/a.rs", 30), "src/a.rs");
        let tight = middle(path, 8);
        assert!(
            tight.starts_with('…') && tight.ends_with("pane.rs"),
            "{tight}"
        );
    }
}
