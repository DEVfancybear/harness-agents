//! The session picker opened by `/resume` with no argument.
//!
//! Escape closes it without changing the source, which is what acceptance U08
//! asserts: the picker is a menu, not a command.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use super::super::theme::Theme;

/// Draw the picker with the highlighted row marked.
pub fn render(frame: &mut Frame, area: Rect, items: &[String], selected: usize, theme: &Theme) {
    render_named(frame, area, items, selected, theme, " chọn session ");
}

/// Draw the workspace file picker opened by `@`.
pub fn render_files(
    frame: &mut Frame,
    area: Rect,
    items: &[String],
    selected: usize,
    theme: &Theme,
) {
    render_named(frame, area, items, selected, theme, " chọn file ");
}

/// Draw prime-agent's tree selector (`/tree`) or fork selector (`/fork`).
pub fn render_turns(
    frame: &mut Frame,
    area: Rect,
    items: &[String],
    selected: usize,
    fork: bool,
    theme: &Theme,
) {
    let title = if fork {
        " fork từ tin nhắn "
    } else {
        " cây hội thoại "
    };
    render_named(frame, area, items, selected, theme, title);
}

/// prime-agent's `/tree` selector: the tree rows (cursor, gutters and
/// connectors, the active-path dot, the label, the role and the text), the
/// `(n/total)` footer with the filter, and the label, summary or custom
/// prompt input under them.
pub fn render_tree(
    frame: &mut Frame,
    area: Rect,
    tree: &crate::interactive::events::TreeModal,
    theme: &Theme,
) {
    use crate::interactive::events::TreePrompt;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if tree.rows.is_empty() {
        lines.push(Line::from(Span::styled("  No entries found", theme.muted)));
    }
    for row in &tree.rows {
        let mut spans = vec![
            Span::raw(if row.selected { "› " } else { "  " }),
            Span::styled(row.prefix.clone(), theme.dim),
        ];
        if row.folded {
            spans.push(Span::styled("⊞ ", theme.accent));
        }
        if row.active {
            spans.push(Span::styled("• ", theme.accent));
        }
        if let Some(label) = &row.label {
            spans.push(Span::styled(format!("[{label}] "), theme.warning));
        }
        if let Some(time) = &row.label_time {
            spans.push(Span::styled(
                format!("{} ", time.get(..16).unwrap_or(time)),
                theme.muted,
            ));
        }
        if row.user {
            spans.push(Span::styled("user: ", theme.accent));
            spans.push(Span::raw(row.text.clone()));
        } else {
            spans.push(Span::styled("assistant: ", theme.tool_ok));
            if row.empty {
                spans.push(Span::styled("(no content)", theme.muted));
            } else {
                spans.push(Span::raw(row.text.clone()));
            }
        }
        if row.selected {
            for span in &mut spans {
                span.style = span.style.patch(theme.selection);
            }
        }
        lines.push(Line::from(spans));
    }
    let search = if tree.search.is_empty() {
        String::new()
    } else {
        format!("  search: {}", tree.search)
    };
    lines.push(Line::from(Span::styled(
        format!("  {}{search}", tree.footer),
        theme.muted,
    )));
    match &tree.prompt {
        Some(TreePrompt::Label { input }) => {
            lines.push(Line::from(Span::styled(
                "  Label (empty clears):",
                theme.title,
            )));
            lines.push(Line::from(format!("  › {input}")));
        }
        Some(TreePrompt::Summarize { options, selected }) => {
            lines.push(Line::from(Span::styled("  Summarize branch?", theme.title)));
            lines.extend(rows(options, *selected, theme));
        }
        Some(TreePrompt::Custom { input }) => {
            lines.push(Line::from(Span::styled(
                "  Custom summarization instructions:",
                theme.title,
            )));
            lines.push(Line::from(format!("  › {input}")));
        }
        None => {}
    }
    let height = usize::from(area.height.saturating_sub(2));
    let lines: Vec<Line<'static>> = lines
        .into_iter()
        .rev()
        .take(height)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border)
        .title(Span::styled(" cây hội thoại ", theme.title));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Draw a model question: the prompt, then its options as a menu with the
/// highlighted one marked, scrolled so the highlight is always in view.
pub fn render_question(
    frame: &mut Frame,
    area: Rect,
    prompt: &str,
    options: &[String],
    selected: usize,
    theme: &Theme,
) {
    let compact = area.height <= 3;
    let height = usize::from(if compact {
        area.height
    } else {
        area.height - 2
    });
    let width = area.width.saturating_sub(if compact { 0 } else { 2 });
    let mut lines: Vec<Line<'static>> = prompt
        .lines()
        .flat_map(|line| {
            super::super::markdown::wrap_spans(
                vec![Span::styled(line.to_owned(), theme.title)],
                width,
            )
        })
        .collect();
    let first_option = lines.len();
    let numbered = options
        .iter()
        .enumerate()
        .map(|(index, option)| format!("{}. {option}", index + 1))
        .collect::<Vec<_>>();
    if !options.is_empty() {
        lines.extend(rows(&numbered, selected, theme));
    }
    lines.push(Line::from(Span::styled(
        "hoặc nhập câu trả lời rồi nhấn Enter".to_owned(),
        theme.dim,
    )));
    // Keep the highlighted option in view; the prompt scrolls away first.
    let target = if options.is_empty() {
        0
    } else {
        first_option + selected.min(options.len() - 1)
    };
    let offset = target.saturating_add(1).saturating_sub(height);
    let visible: Vec<_> = lines.into_iter().skip(offset).take(height).collect();
    if compact {
        frame.render_widget(Paragraph::new(visible), area);
        return;
    }
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border)
        .title(Span::styled(" question ", theme.title));
    if !options.is_empty() {
        block = block.title_top(
            Line::from(Span::styled(
                format!(
                    " {}/{} ",
                    selected.min(options.len() - 1) + 1,
                    options.len()
                ),
                theme.dim,
            ))
            .right_aligned(),
        );
    }
    frame.render_widget(Paragraph::new(visible).block(block), area);
}

fn render_named(
    frame: &mut Frame,
    area: Rect,
    items: &[String],
    selected: usize,
    theme: &Theme,
    title: &'static str,
) {
    let compact = area.height <= 3;
    let height = usize::from(if compact {
        area.height
    } else {
        area.height - 2
    });
    let selected = selected.min(items.len().saturating_sub(1));
    let offset = selected.saturating_add(1).saturating_sub(height);
    let lines: Vec<_> = rows(items, selected, theme)
        .into_iter()
        .skip(offset)
        .take(height)
        .collect();
    if compact {
        frame.render_widget(Paragraph::new(lines), area);
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border)
        .title(Span::styled(title, theme.title));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The picker rows, with an arrow on the highlighted one.
#[must_use]
pub fn rows(items: &[String], selected: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let marker = if index == selected { "❯ " } else { "  " };
        let style = if index == selected {
            theme.selection
        } else {
            theme.dim
        };
        lines.push(Line::from(vec![
            Span::styled(marker.to_owned(), style),
            Span::styled(item.clone(), style),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "no persisted sessions in this project yet".to_owned(),
            theme.dim,
        )));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::rows;
    use crate::interactive::tui::markdown::plain_text;
    use crate::interactive::tui::theme::Theme;

    #[test]
    fn t06_the_arrow_marks_the_highlighted_row() {
        let items = vec![
            "session_a  1 input".to_owned(),
            "session_b  2 inputs".to_owned(),
        ];
        let text = plain_text(&rows(&items, 1, &Theme::plain()));
        let mut lines = text.lines();
        assert!(
            !lines.next().expect("first row").starts_with('❯'),
            "the first row is not highlighted"
        );
        assert!(
            lines.next().expect("second row").starts_with("❯ "),
            "the selected row carries the arrow: {text}"
        );
    }

    #[test]
    fn t06_an_empty_listing_says_so_instead_of_showing_nothing() {
        let text = plain_text(&rows(&[], 0, &Theme::plain()));
        assert!(text.contains("no persisted sessions"), "{text}");
    }
}
