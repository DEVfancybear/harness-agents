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
///
/// `scroll` is a row offset from the top; it is clamped here because this is the
/// only place that knows how many rows fit. The bottom border says whether there is
/// more content and how to reach it, so a clipped panel never looks like the whole
/// answer.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: &[String],
    scroll: usize,
    theme: &Theme,
) {
    let rows = rows(lines, theme);
    let height = usize::from(area.height.saturating_sub(2)).max(1);
    let max_scroll = rows.len().saturating_sub(height);
    let offset = scroll.min(max_scroll);
    let visible: Vec<Line<'static>> = rows.into_iter().skip(offset).take(height).collect();
    // The keys named here are exactly the ones the controller handles for an open
    // panel: PageUp/PageDown by a page, Home to the first row, End to the last. The
    // arrows are deliberately absent - they belong to the editor, so a panel opened
    // over a draft must not take them, and naming them here would advertise a key
    // that edits the draft behind the panel instead of scrolling it.
    let hint = if max_scroll == 0 {
        " Esc đóng ".to_owned()
    } else if offset == 0 {
        format!(" PgUp/PgDn · Home/End cuộn · còn {max_scroll} dòng · Esc đóng ")
    } else if offset >= max_scroll {
        " PgUp/PgDn · Home/End cuộn · cuối · Esc đóng ".to_owned()
    } else {
        format!(
            " PgUp/PgDn · Home/End cuộn · dòng {}/{} · Esc đóng ",
            offset + 1,
            max_scroll + 1
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border)
        .title(Span::styled(format!(" {title} "), theme.title))
        .title_bottom(Span::styled(hint, theme.dim));
    frame.render_widget(
        Paragraph::new(visible)
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
