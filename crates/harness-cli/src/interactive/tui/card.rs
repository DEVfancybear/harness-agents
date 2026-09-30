//! The tool card: one call, drawn so its kind, its target and its outcome can be
//! read in a glance while scrolling.
//!
//! ```text
//!   ▤ Read file  docs/REVIEW.md  limit=260 offset=130               ✓ 206ms
//!   │ 131: |---|---|
//!   │ 132: | P0 | SEO |
//!   ╰─ … 99 more lines
//! ```
//!
//! The glyph and the name take the colour of the call's family
//! ([`ToolKind`]); the target - the file, the query, the command - is the one thing
//! drawn bright; what is left of the arguments is dim; and the outcome sits at the
//! right edge, green or red. The body hangs from a rail in the family's colour, so
//! neighbouring cards are separated by a blank row and their own colours, not by
//! dark blocks that touch.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::icons::ToolKind;
use super::theme::Theme;
use super::widgets::composer::display_width;

/// The rail down a card's body.
pub const RAIL: &str = "  │ ";
/// The rail's last row, under the last line of the body.
pub const RAIL_END: &str = "  ╰─ ";
/// Cells both rails occupy.
pub const RAIL_CELLS: usize = 5;

/// The argument that says what a call is about, most specific first.
const TARGET_KEYS: [&str; 9] = [
    "command", "code", "query", "pattern", "url", "path", "name", "task", "role",
];

/// A call's arguments as the card shows them.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Arguments {
    /// The value of the argument that names the call's target.
    pub target: Option<String>,
    /// The other `key=value` pairs, in the order they came.
    pub rest: Vec<String>,
    /// Notes after the arguments, such as how the call was allowed.
    pub notes: Vec<String>,
}

/// Split the summary the turn driver writes (`key=value key=value · note`).
///
/// The summary is our own format: pairs separated by spaces, a value running up
/// to the next ` key=`, and notes after ` · `. Anything that is not in that shape
/// is kept whole as the rest, so an unusual summary is shown, never lost.
#[must_use]
pub fn split_summary(summary: &str) -> Arguments {
    let mut parts = summary.split(" · ");
    let head = parts.next().unwrap_or_default().trim();
    let notes = parts
        .map(str::trim)
        .filter(|note| !note.is_empty())
        .map(str::to_owned)
        .collect();
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut loose = String::new();
    for token in head.split(' ') {
        match token.split_once('=') {
            Some((key, value))
                if !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c == '_') =>
            {
                pairs.push((key.to_owned(), value.to_owned()));
            }
            _ => {
                if let Some((_, value)) = pairs.last_mut() {
                    value.push(' ');
                    value.push_str(token);
                } else {
                    if !loose.is_empty() {
                        loose.push(' ');
                    }
                    loose.push_str(token);
                }
            }
        }
    }
    let mut arguments = Arguments {
        notes,
        ..Arguments::default()
    };
    if !loose.is_empty() {
        arguments.rest.push(loose);
    }
    let target = TARGET_KEYS
        .iter()
        .find_map(|key| pairs.iter().position(|(name, _)| name == key));
    if let Some(index) = target {
        arguments.target = Some(pairs.remove(index).1);
    }
    arguments.rest.extend(
        pairs
            .into_iter()
            .map(|(key, value)| format!("{key}={value}")),
    );
    arguments
}

/// `read_file` as `Read file`.
#[must_use]
pub fn label(name: &str) -> String {
    let mut text = name.replace(['_', '-'], " ");
    if let Some(first) = text.get(..1) {
        let upper = first.to_uppercase();
        text.replace_range(..1, &upper);
    }
    text
}

/// How a call ended, at the right edge of its header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// Still running; the string is the spinner frame.
    Running(&'static str),
    /// Done, with its duration.
    Done(String),
    /// Failed, with its duration.
    Failed(String),
}

/// The header row of a card, `width` cells wide.
#[must_use]
pub fn header(
    name: &str,
    summary: &str,
    outcome: &Outcome,
    width: u16,
    theme: &Theme,
) -> Line<'static> {
    let kind = ToolKind::of(name);
    let base = kind.style(theme);
    let (tail_text, tail_style) = match outcome {
        Outcome::Running(frame) => (format!("{frame} running"), base),
        Outcome::Done(duration) => (format!("✓ {duration}"), theme.tool_ok),
        Outcome::Failed(duration) => (format!("✗ {duration}"), theme.tool_failed),
    };
    let arguments = split_summary(summary);
    let mut head = vec![
        Span::raw("  "),
        Span::styled(
            format!("{} ", kind.glyph()),
            base.add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if name == "ipython" {
                "Python".to_owned()
            } else {
                label(name)
            },
            base.add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(target) = &arguments.target {
        head.push(Span::raw("  "));
        head.push(Span::styled(
            target.replace(['\n', '\r'], " "),
            theme.strong.remove_modifier(Modifier::BOLD),
        ));
    }
    if !arguments.rest.is_empty() {
        head.push(Span::raw("  "));
        head.push(Span::styled(
            arguments.rest.join(" ").replace(['\n', '\r'], " "),
            theme.dim,
        ));
    }
    for note in &arguments.notes {
        head.push(Span::styled(" · ", theme.dim));
        head.push(Span::styled(note.clone(), theme.dim));
    }
    let tail = Span::styled(tail_text, tail_style);
    let room = usize::from(width).saturating_sub(display_width(&tail.content) + 3);
    let mut head = clip(head, room);
    let used: usize = head.iter().map(|span| display_width(&span.content)).sum();
    let fill = usize::from(width).saturating_sub(used + display_width(&tail.content) + 1);
    head.push(Span::raw(" ".repeat(fill.max(1))));
    head.push(tail);
    Line::from(head)
}

/// Keep the first `cells` cells of a row, ending a cut with an ellipsis.
fn clip(spans: Vec<Span<'static>>, cells: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(|span| display_width(&span.content)).sum();
    if total <= cells {
        return spans;
    }
    let budget = cells.saturating_sub(1);
    let mut used = 0;
    let mut kept: Vec<Span<'static>> = Vec::new();
    for span in spans {
        let width = display_width(&span.content);
        if used + width <= budget {
            used += width;
            kept.push(span);
            continue;
        }
        let mut text = String::new();
        for character in span.content.chars() {
            let cell = display_width(character.encode_utf8(&mut [0; 4]));
            if used + cell > budget {
                break;
            }
            used += cell;
            text.push(character);
        }
        kept.push(Span::styled(text, span.style));
        break;
    }
    kept.push(Span::styled("…", Style::new()));
    kept
}

#[cfg(test)]
mod tests {
    use super::{Outcome, header, label, split_summary};
    use crate::interactive::tui::markdown::plain_text;
    use crate::interactive::tui::theme::Theme;

    #[test]
    fn a_summary_names_its_target_and_keeps_the_rest_and_the_notes() {
        let parsed = split_summary(
            "limit=260 offset=130 path=docs/REVIEW-2026-09-28.md · allowed by mode full-auto",
        );
        assert_eq!(parsed.target.as_deref(), Some("docs/REVIEW-2026-09-28.md"));
        assert_eq!(parsed.rest, vec!["limit=260", "offset=130"]);
        assert_eq!(parsed.notes, vec!["allowed by mode full-auto"]);
    }

    #[test]
    fn a_value_with_spaces_stays_whole() {
        let parsed = split_summary("command=cargo test --workspace timeout=60");
        assert_eq!(parsed.target.as_deref(), Some("cargo test --workspace"));
        assert_eq!(parsed.rest, vec!["timeout=60"]);
    }

    #[test]
    fn a_summary_without_pairs_is_shown_not_lost() {
        let parsed = split_summary("just some words");
        assert_eq!(parsed.target, None);
        assert_eq!(parsed.rest, vec!["just some words"]);
    }

    #[test]
    fn a_tool_name_reads_as_a_label() {
        assert_eq!(label("read_file"), "Read file");
        assert_eq!(label("glob"), "Glob");
    }

    #[test]
    fn the_header_puts_the_outcome_at_the_right_edge() {
        let row = header(
            "read_file",
            "path=src/main.rs",
            &Outcome::Done("206ms".to_owned()),
            60,
            &Theme::plain(),
        );
        let text = plain_text(&[row]);
        assert!(text.contains("▤ Read file  src/main.rs"), "{text}");
        assert!(text.trim_end().ends_with("✓ 206ms"), "{text}");
        assert_eq!(text.trim_end_matches('\n').chars().count(), 59, "{text}");
    }

    #[test]
    fn a_long_header_is_cut_before_its_outcome() {
        let row = header(
            "search_text",
            &format!("query={}", "x".repeat(200)),
            &Outcome::Failed("1s".to_owned()),
            40,
            &Theme::plain(),
        );
        let text = plain_text(&[row]);
        assert!(text.contains('…'), "{text}");
        assert!(text.trim_end().ends_with("✗ 1s"), "{text}");
    }
}
