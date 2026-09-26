//! Markdown for assistant text, as prime-agent renders it.
//!
//! Deliberately minimal and dependency-free: fenced code blocks with a language
//! label, inline code, `#` headings, `-`/`*` bullets, and `**emphasis**`. Anything
//! the parser does not recognise is emitted **verbatim** - the one thing this module
//! must never do is swallow a character, because the transcript is the user's record
//! of what the model said.
//!
//! Rows are wrapped here rather than left to the terminal. A terminal clips a row
//! that is too wide, so text the model wrote would simply disappear; wrapping in the
//! renderer is what keeps it readable at any console width.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::Theme;

/// How one line of model text is classified.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Block {
    /// A `#` heading, with its level.
    Heading(usize),
    /// A `-`, `*` or `+` bullet.
    Bullet,
    /// A `>` quote.
    Quote,
    /// A `---`, `***` or `___` rule.
    Rule,
    /// Plain prose.
    Text,
}

/// Render model text into styled lines of at most `width` cells, the way
/// prime-agent's `Markdown` component does (`tui/src/components/markdown.ts`):
/// headings in `mdHeading` (H1 bold and underlined, H2-H3 bold, H4 bold italic,
/// deeper italic), code blocks indented two cells in `mdCodeBlock` with no fence
/// rows, quotes behind a `│ ` bar in italics, rules as `─` up to 80 cells, bullets
/// as `- `, and prose in `mdBody`.
#[must_use]
pub fn render(text: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_fence = false;
    for raw in text.split('\n') {
        let trimmed = raw.trim_end_matches('\r');
        if trimmed.trim_start().starts_with("```") {
            // The fence is a border, not content: prime-agent draws no fence rows.
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            let spans = vec![
                Span::raw("  ".to_owned()),
                Span::styled(trimmed.to_owned(), theme.md_code_block),
            ];
            lines.extend(wrap_spans(spans, width));
            continue;
        }
        match classify(trimmed) {
            Block::Heading(level) => {
                let text = trimmed.trim_start().trim_start_matches('#').trim_start();
                let style = match level {
                    1 => theme
                        .md_heading
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                    2 | 3 => theme.md_heading.add_modifier(Modifier::BOLD),
                    4 => theme
                        .md_heading
                        .add_modifier(Modifier::BOLD | Modifier::ITALIC),
                    _ => theme.md_heading.add_modifier(Modifier::ITALIC),
                };
                lines.extend(wrap_spans(
                    vec![Span::styled(text.to_owned(), style)],
                    width,
                ));
            }
            Block::Bullet => {
                let indent = trimmed.len() - trimmed.trim_start().len();
                let text = trimmed.trim_start()[1..].trim_start();
                let mut spans = vec![
                    Span::raw(" ".repeat(indent)),
                    Span::styled("- ".to_owned(), theme.md_quote),
                ];
                spans.extend(inline_spans(text, theme));
                lines.extend(wrap_spans(spans, width));
            }
            Block::Quote => {
                let text = trimmed.trim_start()[1..].trim_start();
                let mut spans = vec![Span::styled("│ ".to_owned(), theme.border)];
                spans.extend(inline_spans(text, theme).into_iter().map(|span| {
                    let style = span
                        .style
                        .patch(theme.md_quote)
                        .add_modifier(Modifier::ITALIC);
                    span.style(style)
                }));
                lines.extend(wrap_spans(spans, width));
            }
            Block::Rule => {
                let cells = usize::from(width).clamp(1, 80);
                lines.push(Line::from(Span::styled("─".repeat(cells), theme.border)));
            }
            Block::Text => lines.extend(wrap_spans(inline_spans(trimmed, theme), width)),
        }
    }
    lines
}

/// Classify one line of model text.
#[must_use]
pub fn classify(line: &str) -> Block {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        let level = trimmed
            .chars()
            .take_while(|character| *character == '#')
            .count();
        if trimmed[level..].starts_with(' ') || trimmed.len() == level {
            return Block::Heading(level);
        }
    }
    let compact = trimmed.replace(' ', "");
    if compact.len() >= 3
        && (compact.chars().all(|character| character == '-')
            || compact.chars().all(|character| character == '*')
            || compact.chars().all(|character| character == '_'))
    {
        return Block::Rule;
    }
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return Block::Bullet;
    }
    if trimmed.starts_with('>') {
        return Block::Quote;
    }
    Block::Text
}

/// Split one line into styled runs.
///
/// `**bold**` markers are removed because printing them looks like a defect in a
/// terminal; `` `code` `` markers are kept because the backticks are part of the
/// text. An unclosed marker is emitted verbatim, exactly once, where it was written.
///
/// A run is buffered as styled characters rather than as a string, so a code span and
/// the emphasis around it can be open at once without either one flattening the other.
fn inline_spans(line: &str, theme: &Theme) -> Vec<Span<'static>> {
    let mut pending: Vec<Glyph> = Vec::new();
    let mut at_code = false;
    let mut strong = false;
    let mut rest = line;
    while let Some(character) = rest.chars().next() {
        rest = &rest[character.len_utf8()..];
        if character == '`' {
            // The backticks stay in the text - they are what the model wrote - but
            // each is emitted exactly once.
            at_code = !at_code;
            pending.push((character, inline_style(at_code, strong, theme)));
            continue;
        }
        if character == '*' && !at_code && rest.starts_with('*') {
            // Both marker characters are consumed before looking for the close, so the
            // second `*` is never rescanned as an opener.
            let after = &rest[1..];
            // `****` has nothing between its markers, so it is text, not emphasis:
            // requiring a non-empty body is what keeps it from styling nothing.
            if let Some(at) = after.find("**").filter(|at| *at > 0) {
                // The body sits between the markers, so it is emitted here, styled by
                // the emphasis this marker opened.
                strong = !strong;
                for body in after[..at].chars() {
                    pending.push((body, inline_style(at_code, strong, theme)));
                }
                // Skip the body and the closing marker.
                rest = &after[at + 2..];
            } else {
                // An unclosed marker is text, emitted where it was written.
                rest = after;
                pending.push(('*', inline_style(false, strong, theme)));
                pending.push(('*', inline_style(false, strong, theme)));
            }
            continue;
        }
        pending.push((character, inline_style(at_code, strong, theme)));
    }
    spans_of(pending)
}

/// The style one character of a line carries.
fn inline_style(at_code: bool, strong: bool, theme: &Theme) -> Style {
    let base = if at_code {
        theme.md_code
    } else {
        theme.assistant
    };
    if strong {
        base.add_modifier(Modifier::BOLD)
    } else {
        base
    }
}

/// One character of a line, with the styling it carries.
type Glyph = (char, Style);

/// Break spans into rows of at most `width` cells, never dropping a character.
///
/// A row is broken at the last space it contains so that words stay whole; only a
/// run with no space at all - a long path or URL, say - is split mid-word, because
/// the alternative is the terminal clipping it away.
///
/// The whitespace a break lands on belongs to neither row: leaving it would pad the
/// finished row or indent the next one, so it is consumed. Whitespace is the only
/// thing wrapping ever removes.
fn wrap_spans(spans: Vec<Span<'static>>, width: u16) -> Vec<Line<'static>> {
    let limit = usize::from(width.max(1));

    // Flatten to characters first: a row must be able to break *inside* a span,
    // which is where a styled run such as `"word another"` sits.
    let mut glyphs: Vec<Glyph> = Vec::new();
    for span in spans {
        let style = span.style;
        glyphs.extend(span.content.chars().map(|character| (character, style)));
    }

    let mut rows: Vec<Vec<Glyph>> = Vec::new();
    let mut row: Vec<Glyph> = Vec::new();
    let mut used = 0_usize;

    for (character, glyph_style) in glyphs {
        let cells = super::widgets::composer::char_width(character);
        if used + cells > limit && used > 0 {
            // `used + cells > limit` implies the row is non-empty, so there is
            // always something before this newest character to break on.
            let at = break_point(&row).unwrap_or(row.len());
            // `at` is the space itself, so it goes on the finished row and
            // `trim_trailing_spaces` takes it off. Breaking *after* it instead would
            // leave the space heading the next row, which reads as an indent.
            let rest = row.split_off(at);
            trim_trailing_spaces(&mut row);
            used = 0;
            rows.push(std::mem::take(&mut row));
            row = rest;
            for &(each, _) in &row {
                used += super::widgets::composer::char_width(each);
            }
        }
        row.push((character, glyph_style));
        used += cells;
    }
    // The last row is trimmed too: text that ends in a space must not be printed
    // wider than the text it came from.
    trim_trailing_spaces(&mut row);
    rows.push(row);

    rows.into_iter()
        .map(|row| Line::from(spans_of(row)))
        .collect()
}

/// The index the next row should start at, or `None` when the row has no break.
///
/// The index is one past the space, so the space stays on the finished row where
/// `trim_trailing_spaces` takes it off; the next row then starts on a real character
/// rather than on an indent.
fn break_point(row: &[Glyph]) -> Option<usize> {
    let space = row
        .iter()
        .rposition(|&(character, _)| character.is_whitespace())?;
    Some(space + 1)
}

/// Drop the spaces a break landed on, so no row is ever printed padded.
fn trim_trailing_spaces(row: &mut Vec<Glyph>) {
    while row.last().is_some_and(|&(character, _)| character == ' ') {
        row.pop();
    }
}

/// Collapse characters back into spans, one per run of equal styling.
fn spans_of(row: Vec<Glyph>) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut text = String::new();
    let mut style: Option<Style> = None;
    for (character, glyph_style) in row {
        if style != Some(glyph_style) && !text.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut text),
                style.unwrap_or_default(),
            ));
        }
        style = Some(glyph_style);
        text.push(character);
    }
    if !text.is_empty() {
        spans.push(Span::styled(text, style.unwrap_or_default()));
    }
    spans
}

/// The visible text of a set of rendered lines, for tests, for measuring and for
/// `/more`, which shows rendered rows as plain panel lines.
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
        let rendered = plain_text(&render(text, 80, &Theme::plain()));
        for needle in [
            "Title",
            "prose with",
            "`code`",
            "日本語",
            "fn main() {}",
            "- bullet",
            "- other",
        ] {
            assert!(
                rendered.contains(needle),
                "missing {needle:?} in:\n{rendered}"
            );
        }
    }

    #[test]
    /// prime-agent draws a code block indented two cells, with no fence rows.
    fn t04_code_blocks_are_indented_without_fences() {
        let rendered = render("```sh\ncargo test\n```", 80, &Theme::plain());
        assert_eq!(plain_text(&rendered), "  cargo test");
    }

    #[test]
    fn quotes_rules_and_heading_levels_follow_prime_agent() {
        let rendered = plain_text(&render("> quoted\n---\n## Two", 20, &Theme::plain()));
        assert_eq!(rendered, format!("│ quoted\n{}\nTwo", "─".repeat(20)));
        assert_eq!(classify("### three"), Block::Heading(3));
        assert_eq!(classify("#hashtag"), Block::Text);
    }

    #[test]
    fn t04_classification_is_conservative() {
        assert_eq!(classify("# h"), Block::Heading(1));
        assert_eq!(classify("- item"), Block::Bullet);
        assert_eq!(classify("* item"), Block::Bullet);
        assert_eq!(classify("text - not a bullet"), Block::Text);
        assert_eq!(classify(""), Block::Text);
    }

    /// Found in a real session: `**Tool call:**` was printed with its asterisks.
    #[test]
    fn t04_emphasis_markers_become_styling_instead_of_asterisks() {
        let rendered = render(
            "**Tool call:** `list_files` — **failed**",
            80,
            &Theme::plain(),
        );
        let visible = plain_text(&rendered);
        assert_eq!(visible, "Tool call: `list_files` — failed");
        assert!(!visible.contains('*'), "no marker may survive: {visible:?}");
    }

    /// `****` has nothing between its markers, so it is text, not emphasis.
    #[test]
    fn t04_markers_with_nothing_between_them_are_text() {
        let rendered = plain_text(&render("a **** b", 80, &Theme::plain()));
        assert_eq!(rendered, "a **** b");
    }

    #[test]
    fn t04_an_unclosed_marker_is_text() {
        let rendered = plain_text(&render("a ** b ` c", 80, &Theme::plain()));
        assert_eq!(rendered, "a ** b ` c");
    }

    /// Found in the same session: a long row was clipped by the terminal.
    #[test]
    fn t04_long_rows_wrap_instead_of_being_clipped() {
        let paragraph = "word ".repeat(40);
        let width = 40;
        let rendered = render(&paragraph, width, &Theme::plain());
        assert!(rendered.len() > 1, "a long row must wrap");
        for line in &rendered {
            let cells = crate::interactive::tui::widgets::composer::display_width(&plain_text(
                std::slice::from_ref(line),
            ));
            assert!(
                cells <= usize::from(width),
                "row too wide: {cells} > {width}"
            );
        }
        let joined = plain_text(&rendered).replace('\n', " ");
        assert!(
            joined.contains("word word word"),
            "wrapping must keep the words: {joined}"
        );
    }

    /// A row is broken at a space, so no row begins or ends with a stray gap.
    #[test]
    fn t04_wrapping_breaks_at_spaces() {
        let rendered = render("alpha beta gamma delta", 12, &Theme::plain());
        assert_eq!(
            plain_text(&rendered),
            "alpha beta\ngamma delta",
            "a break must land on a space, not mid-word"
        );
    }

    /// A run with no space in it still wraps: a clipped path is lost text.
    #[test]
    fn t04_a_word_wider_than_the_row_still_wraps() {
        let long = "a".repeat(25);
        let rendered = render(&long, 10, &Theme::plain());
        assert_eq!(rendered.len(), 3, "25 cells over a 10-cell row is 3 rows");
        assert_eq!(plain_text(&rendered).replace('\n', ""), long);
    }

    /// Wrapping moves the space it breaks on, but never invents or drops a word.
    #[test]
    fn t04_wrapping_keeps_every_word_of_a_long_paragraph() {
        let paragraph = "alpha beta gamma delta epsilon zeta ".repeat(4);
        let rows: Vec<String> = render(&paragraph, 20, &Theme::plain())
            .iter()
            .map(|line| plain_text(std::slice::from_ref(line)))
            .collect();
        for row in &rows {
            assert!(
                row.len() <= 20 && !row.starts_with(' ') && !row.ends_with(' '),
                "row is padded or indented: {row:?}"
            );
        }
        let words: Vec<&str> = rows.iter().flat_map(|row| row.split_whitespace()).collect();
        let expected: Vec<&str> = paragraph.split_whitespace().collect();
        assert_eq!(words, expected, "no word may be lost or reordered");
    }

    /// The exact text a live paid turn returned on 20/09/2026, when asked to answer
    /// with `**bold**` followed by a long line of prose. Both defects in the report
    /// came from *this* shape of answer: the markers were printed and the prose row
    /// was clipped by the terminal. Pinning the real bytes is what keeps the fix
    /// honest - a synthetic one-liner would not have caught either.
    #[test]
    fn t04_a_real_answer_loses_no_marker_and_no_word() {
        let answer = "**bold**\n\nThe morning light filtered through the tall windows as the team gathered around the worn wooden table, laptops open and coffee cups steaming. They had spent weeks preparing for this moment, refining every detail, testing each assumption, and questioning the parts that felt too easy. Now the room filled with a quiet energy, the kind that precedes something meaningful. Someone sketched diagrams on the whiteboard while another typed notes in rapid bursts. Outside, traffic hummed and birds crossed the pale sky, indifferent to the work happening indoors. The conversation moved between problem and possibility, and slowly a shape emerged from the noise, clear enough to follow. They agreed to begin.";
        // A narrow console, which is where the report came from.
        let width = 72;
        let rendered = render(answer, width, &Theme::plain());
        let visible = plain_text(&rendered);

        assert!(
            !visible.contains("**"),
            "the markers must not reach the screen: {visible}"
        );
        assert!(
            visible.starts_with("bold"),
            "the emphasised word stays: {visible}"
        );
        for line in &rendered {
            let cells = crate::interactive::tui::widgets::composer::display_width(&plain_text(
                std::slice::from_ref(line),
            ));
            assert!(
                cells <= usize::from(width),
                "a prose row was left for the terminal to clip: {cells} > {width}"
            );
        }
        // Every word of the answer is still readable, in order.
        let words: Vec<&str> = visible.split_whitespace().collect();
        let expected: Vec<&str> = answer
            .split_whitespace()
            .map(|word| word.trim_matches('*'))
            .collect();
        assert_eq!(words, expected, "the answer must survive wrapping intact");
    }
}
