//! Markdown-lite for assistant text.
//!
//! Deliberately minimal and dependency-free: fenced code blocks with a language
//! label, inline code, `#` headings, and `-`/`*` bullets. Anything the parser does
//! not recognise is emitted **verbatim** - the one thing this module must never do
//! is swallow a character, because the transcript is the user's record of what the
//! model said.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::Theme;

/// How one line of model text is classified.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Block {
    /// A `#` heading.
    Heading,
    /// A `-` or `*` bullet.
    Bullet,
    /// Plain prose.
    Text,
}

/// Render model text into styled lines.
///
/// The function is total: every input character appears in the output, in order.
#[must_use]
pub fn render(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_fence = false;
    for raw in text.split('\n') {
        let trimmed = raw.trim_end_matches('\r');
        if let Some(rest) = trimmed.trim_start().strip_prefix("```") {
            if in_fence {
                in_fence = false;
                // The closing fence is a border, not content: it is replaced by a
                // rule that carries no information away.
                lines.push(Line::from(vec![Span::styled("└".to_owned(), theme.dim)]));
            } else {
                in_fence = true;
                let label = rest.trim();
                let title = if label.is_empty() {
                    "┌ code".to_owned()
                } else {
                    format!("┌ {label}")
                };
                lines.push(Line::from(vec![Span::styled(title, theme.dim)]));
            }
            continue;
        }
        if in_fence {
            let mut spans = vec![Span::styled("│ ".to_owned(), theme.dim)];
            spans.extend(inline_spans(trimmed, theme));
            lines.push(Line::from(spans));
            continue;
        }
        let block = classify(trimmed);
        match block {
            Block::Heading => {
                let text = trimmed.trim_start_matches('#').trim_start();
                lines.push(Line::from(vec![Span::styled(
                    text.to_owned(),
                    theme.title.add_modifier(Modifier::BOLD),
                )]));
            }
            Block::Bullet => {
                let text = trimmed
                    .trim_start()
                    .trim_start_matches(['-', '*'])
                    .trim_start();
                let mut spans = vec![Span::styled("• ".to_owned(), theme.accent)];
                spans.extend(inline_spans(text, theme));
                lines.push(Line::from(spans));
            }
            Block::Text => lines.push(Line::from(inline_spans(trimmed, theme))),
        }
    }
    lines
}

/// Classify one line of model text.
#[must_use]
pub fn classify(line: &str) -> Block {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return Block::Heading;
    }
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
        return Block::Bullet;
    }
    Block::Text
}

/// Split one line into spans, styling `` `code` `` runs.
///
/// Backticks are kept: dropping them would change what the model wrote, and the
/// style already marks the run.
fn inline_spans(line: &str, theme: &Theme) -> Vec<Span<'static>> {
    if !line.contains('`') {
        return vec![Span::raw(line.to_owned())];
    }
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut in_code = false;
    for character in line.chars() {
        if character == '`' {
            if !current.is_empty() {
                let style = if in_code { theme.accent } else { Style::new() };
                spans.push(Span::styled(std::mem::take(&mut current), style));
            }
            current.push('`');
            in_code = !in_code;
            continue;
        }
        current.push(character);
    }
    if !current.is_empty() {
        let style = if in_code { theme.accent } else { Style::new() };
        spans.push(Span::styled(current, style));
    }
    spans
}

/// The visible text of a set of rendered lines, for tests and for measuring.
#[cfg(test)]
#[must_use]
pub fn plain_text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{Block, classify, plain_text, render};
    use crate::interactive::tui::theme::Theme;

    /// The one invariant that matters: rendering never loses a character.
    #[test]
    fn t04_markdown_rendering_keeps_every_visible_character() {
        let text = "# Title\n\nprose with `code` and 日本語\n\n```rust\nfn main() {}\n```\n- bullet\n* other";
        let rendered = plain_text(&render(text, &Theme::plain()));
        for needle in [
            "Title",
            "prose with",
            "`code`",
            "日本語",
            "rust",
            "fn main() {}",
            "• bullet",
            "• other",
        ] {
            assert!(
                rendered.contains(needle),
                "missing {needle:?} in:\n{rendered}"
            );
        }
    }

    #[test]
    fn t04_fences_report_their_language_and_keep_their_body() {
        let rendered = render("```sh\ncargo test\n```", &Theme::plain());
        assert_eq!(rendered.len(), 3);
        assert!(plain_text(&rendered[0..1]).contains("sh"));
        assert!(plain_text(&rendered[1..2]).contains("cargo test"));
    }

    #[test]
    fn t04_classification_is_conservative() {
        assert_eq!(classify("# h"), Block::Heading);
        assert_eq!(classify("- item"), Block::Bullet);
        assert_eq!(classify("* item"), Block::Bullet);
        assert_eq!(classify("text - not a bullet"), Block::Text);
        assert_eq!(classify(""), Block::Text);
    }
}
