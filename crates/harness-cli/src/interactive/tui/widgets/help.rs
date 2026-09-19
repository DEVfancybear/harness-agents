//! The reference overlay used by `/help`, `/status`, `/config` and `/model`.
//!
//! An overlay is temporary: closing it with Escape must leave the history
//! untouched, which is what acceptance U09 asserts. The controller therefore
//! never pushes overlay content as a history item in TUI mode.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::super::theme::Theme;

/// Draw the overlay with its title in the border.
pub fn render(frame: &mut Frame, area: Rect, title: &str, lines: &[String], theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(format!(" {title} "), theme.title))
        .title_bottom(Span::styled(" Esc đóng ", theme.dim));
    frame.render_widget(
        Paragraph::new(rows(lines, theme))
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The overlay rows.
#[must_use]
pub fn rows(lines: &[String], theme: &Theme) -> Vec<Line<'static>> {
    if lines.is_empty() {
        return vec![Line::from(Span::styled(
            "(không có nội dung)".to_owned(),
            theme.dim,
        ))];
    }
    lines
        .iter()
        .map(|line| {
            // The help table aligns its second column; keep the padding and only
            // style the leading command.
            if let Some((command, rest)) = line.split_once("  ")
                && command.starts_with('/')
            {
                return Line::from(vec![
                    Span::styled(command.to_owned(), theme.accent),
                    Span::raw("  "),
                    Span::styled(rest.to_owned(), theme.dim),
                ]);
            }
            Line::from(Span::raw(line.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::rows;
    use crate::interactive::tui::markdown::plain_text;
    use crate::interactive::tui::theme::Theme;

    #[test]
    fn t06_overlay_rows_keep_the_help_text_verbatim() {
        let lines = vec![
            "/help            list these commands".to_owned(),
            "/resume <id>     resume a persisted session".to_owned(),
        ];
        let text = plain_text(&rows(&lines, &Theme::plain()));
        for line in &lines {
            assert!(text.contains(line.as_str()), "missing {line:?} in:\n{text}");
        }
    }

    #[test]
    fn t06_an_empty_overlay_says_so() {
        let text = plain_text(&rows(&[], &Theme::plain()));
        assert!(text.contains("không có nội dung"), "{text}");
    }
}
