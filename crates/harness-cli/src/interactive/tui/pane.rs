//! A delegate's pane, beside the conversation as herdr splits a pane: what the
//! agent the run delegated to is doing - the calls it starts, the first lines
//! each returns, its answer as it writes it - newest at the bottom, in a box
//! drawn in the accent like herdr's focused pane.
//!
//! ```text
//! ╭─ 2 explorer · working · 7 tools ──────╮
//! │ read_file: path=src/orders.ts         │
//! │   │ export function saveOrder(        │
//! │ The order flow validates the cart,    │
//! ╰────────────────────────── alt+1 back ─╯
//! ```

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use super::sidebar::Status;
use super::theme::Theme;
use super::widgets::composer::{char_width, display_width};
use crate::interactive::events::{ChildActivity, ChildLineKind};

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

/// Draw `activity`'s pane into `area`; `number` is its sidebar row.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    activity: &ChildActivity,
    status: Status,
    number: usize,
    theme: &Theme,
) {
    if area.width < 8 || area.height < 3 {
        return;
    }
    let calls = format!(
        "{} tool{}",
        activity.tool_calls,
        if activity.tool_calls == 1 { "" } else { "s" }
    );
    let title = Line::from(vec![
        Span::styled(format!(" {number} "), theme.dim),
        Span::styled(activity.name.clone(), theme.strong),
        Span::styled(" · ", theme.dim),
        Span::styled(status.word(), status.style(theme)),
        Span::styled(format!(" · {calls} "), theme.dim),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.accent)
        .title(title)
        .title_bottom(Line::from(Span::styled(" alt+1 back ", theme.dim)).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = usize::from(inner.width);
    let rows = usize::from(inner.height);
    let mut lines: Vec<Line<'static>> = activity
        .recent
        .iter()
        .map(|line| match line.kind {
            ChildLineKind::Call => match line.text.split_once(": ") {
                Some((name, rest)) => fit(
                    vec![
                        Span::styled(name.to_owned(), theme.accent.add_modifier(Modifier::BOLD)),
                        Span::styled(format!(" {rest}"), theme.muted),
                    ],
                    width,
                ),
                None => fit(
                    vec![Span::styled(
                        line.text.clone(),
                        theme.accent.add_modifier(Modifier::BOLD),
                    )],
                    width,
                ),
            },
            ChildLineKind::Failed => fit(vec![Span::styled(line.text.clone(), theme.error)], width),
            ChildLineKind::Output => fit(
                vec![
                    Span::styled("  │ ", theme.border),
                    Span::styled(line.text.clone(), theme.dim),
                ],
                width,
            ),
            ChildLineKind::Text => fit(
                vec![Span::styled(line.text.clone(), theme.assistant)],
                width,
            ),
        })
        .collect();
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "waiting for its first call…",
            theme.dim,
        )));
    }
    let skip = lines.len().saturating_sub(rows);
    frame.render_widget(Paragraph::new(lines.split_off(skip)), inner);
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
    use super::{Status, render, width};
    use crate::interactive::events::{ChildActivity, ChildLine, ChildLineKind};
    use crate::interactive::tui::theme::Theme;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    #[test]
    fn it_takes_two_fifths_of_a_wide_conversation_and_none_of_a_narrow_one() {
        assert_eq!(width(71), 0);
        assert_eq!(width(80), 32);
        assert_eq!(width(120), 48);
        assert_eq!(width(240), 64);
    }

    #[test]
    fn it_shows_the_newest_lines_of_what_the_delegate_did() {
        let line = |kind, text: &str| ChildLine {
            kind,
            text: text.to_owned(),
        };
        let activity = ChildActivity {
            name: "explorer".to_owned(),
            tool_calls: 2,
            latest: String::new(),
            recent: vec![
                line(ChildLineKind::Call, "read_file: path=old.rs"),
                line(ChildLineKind::Call, "read_file: path=src/orders.ts"),
                line(ChildLineKind::Output, "export function saveOrder("),
                line(ChildLineKind::Failed, "run_shell failed: denied"),
                line(ChildLineKind::Text, "The order flow validates the cart."),
            ],
        };
        let mut terminal = Terminal::new(TestBackend::new(44, 6)).expect("terminal");
        terminal
            .draw(|frame| {
                render(
                    frame,
                    Rect::new(0, 0, 44, 6),
                    &activity,
                    Status::Working,
                    2,
                    &Theme::plain(),
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let rows = (0..6)
            .map(|row| {
                (0..44)
                    .map(|col| buffer[(col, row)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let text = rows.join("\n");
        assert!(rows[0].contains("2 explorer · working · 2 tools"), "{text}");
        assert!(
            !text.contains("old.rs"),
            "four rows keep the newest four: {text}"
        );
        assert!(text.contains("read_file path=src/orders.ts"), "{text}");
        assert!(text.contains("│ export function saveOrder("), "{text}");
        assert!(text.contains("run_shell failed: denied"), "{text}");
        assert!(
            text.contains("The order flow validates the cart."),
            "{text}"
        );
        assert!(rows[5].contains("alt+1 back"), "{text}");
    }
}
