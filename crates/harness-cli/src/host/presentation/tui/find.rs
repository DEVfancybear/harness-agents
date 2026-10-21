//! Ctrl+F: find in the conversation, as a browser finds in a page. A small
//! bar over the window's top row takes what is typed; every match in the
//! rows the fullscreen window holds is marked, the current one apart, and the
//! bar counts them (`3/17`). Enter or Down goes to the next match, Shift+Enter
//! or Up to the one before, and the window scrolls to keep the current match
//! in view. Esc closes the bar and leaves the window where it is.
//!
//! ```text
//!                                      ╭ ⌕ parser█        3/17  ↑↓ esc ╮
//! ```
//!
//! The matching is case-insensitive and over the text the rows show, so what
//! is found is what can be seen: a match is a row and the cells it covers.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::Theme;
use super::widgets::composer::{char_width, display_width};

/// One match: the conversation row it is on, its first cell and how many
/// cells it covers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Match {
    pub row: usize,
    pub column: usize,
    pub width: usize,
}

/// The find bar: what is searched for, the matches and the current one.
#[derive(Clone, Debug, Default)]
pub struct Find {
    pub query: String,
    pub matches: Vec<Match>,
    /// The index of the current match in `matches`.
    pub current: usize,
    /// What `matches` were found in: the rows' count and the first row's
    /// place in the conversation (rows are dropped from the front), so a
    /// frame finds again only when the rows changed.
    searched: Option<(usize, usize, String)>,
}

impl Find {
    /// The rows changed or the query did: find again. The current match
    /// stays the one it was when it is still found, else the first one at or
    /// after `from` (the window's top), else the first.
    pub fn refresh(&mut self, rows: &[Line<'_>], dropped: usize, from: usize) -> bool {
        let key = (rows.len(), dropped, self.query.clone());
        if self.searched.as_ref() == Some(&key) {
            return false;
        }
        let previous = self.matches.get(self.current).map(|found| {
            (
                found.row + self.searched.as_ref().map_or(0, |(_, d, _)| *d),
                found.column,
            )
        });
        self.matches = matches(rows, &self.query);
        self.current = previous
            .and_then(|(place, column)| {
                self.matches
                    .iter()
                    .position(|found| found.row + dropped == place && found.column == column)
            })
            .or_else(|| self.matches.iter().position(|found| found.row >= from))
            .unwrap_or(0);
        self.searched = Some(key);
        true
    }

    /// Search afresh for `query`, from the window's top.
    pub fn set_query(&mut self, query: String, rows: &[Line<'_>], dropped: usize, from: usize) {
        self.query = query;
        // A new query starts from the window, not from the old current match.
        self.matches.clear();
        self.searched = None;
        self.refresh(rows, dropped, from);
    }

    /// The next match (`forward`) or the one before, wrapping around.
    pub fn step(&mut self, forward: bool) {
        let count = self.matches.len();
        if count == 0 {
            return;
        }
        self.current = if forward {
            (self.current + 1) % count
        } else {
            (self.current + count - 1) % count
        };
    }

    #[must_use]
    pub fn current_match(&self) -> Option<Match> {
        self.matches.get(self.current).copied()
    }

    /// `3/17`, `0/0` with nothing found.
    #[must_use]
    pub fn count(&self) -> String {
        if self.matches.is_empty() {
            "0/0".to_owned()
        } else {
            format!("{}/{}", self.current + 1, self.matches.len())
        }
    }
}

/// Every match of `query` in `rows`, case-insensitive, in reading order. Two
/// matches never overlap.
#[must_use]
pub fn matches(rows: &[Line<'_>], query: &str) -> Vec<Match> {
    let needle = fold(query);
    if needle.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for (row, line) in rows.iter().enumerate() {
        let text = line
            .spans
            .iter()
            .flat_map(|span| span.content.chars())
            .collect::<Vec<_>>();
        let folded = text.iter().map(|c| fold_char(*c)).collect::<Vec<_>>();
        let mut at = 0;
        while at + needle.len() <= folded.len() {
            if folded[at..at + needle.len()] == needle[..] {
                let column = text[..at].iter().map(|c| char_width(*c)).sum();
                let width = text[at..at + needle.len()]
                    .iter()
                    .map(|c| char_width(*c))
                    .sum();
                found.push(Match { row, column, width });
                at += needle.len();
            } else {
                at += 1;
            }
        }
    }
    found
}

fn fold(text: &str) -> Vec<char> {
    text.chars().map(fold_char).collect()
}

/// A character as a case-insensitive comparison sees it: one character for
/// one, so a match's cells are the row's own.
fn fold_char(character: char) -> char {
    let mut lower = character.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(single), None) => single,
        _ => character,
    }
}

/// How a match is marked: the current one reversed, the others underlined in
/// the warning colour.
#[must_use]
pub fn style(theme: &Theme, current: bool) -> Style {
    if current {
        Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        theme
            .warning
            .add_modifier(Modifier::UNDERLINED | Modifier::BOLD)
    }
}

/// The rects the matches in rows `top..top + window.height` cover on
/// screen, with whether each is the current match.
#[must_use]
pub fn marks(find: &Find, top: usize, window: Rect) -> Vec<(Rect, bool)> {
    let bottom = top + usize::from(window.height);
    find.matches
        .iter()
        .enumerate()
        .filter(|(_, found)| found.row >= top && found.row < bottom)
        .filter_map(|(index, found)| {
            let y = window.y + u16::try_from(found.row - top).ok()?;
            let x = window.x + u16::try_from(found.column).ok()?;
            if x >= window.x + window.width {
                return None;
            }
            let width = u16::try_from(found.width)
                .ok()?
                .min(window.x + window.width - x);
            Some((Rect::new(x, y, width, 1), index == find.current))
        })
        .collect()
}

/// The bar: ` ⌕ query  3/17  ↑↓ esc `, at most `width` cells.
#[must_use]
pub fn bar(find: &Find, width: usize, theme: &Theme) -> Line<'static> {
    let count = find.count();
    let keys = "  ↑↓ esc ";
    let room = width.saturating_sub(display_width(&count) + display_width(keys) + 6);
    let mut query: String = find.query.chars().rev().collect();
    // The end of a long query, where the typing is.
    while display_width(&query) > room {
        query.pop();
    }
    let query: String = query.chars().rev().collect();
    let found = if find.matches.is_empty() && !find.query.is_empty() {
        theme.error
    } else {
        theme.strong
    };
    Line::from(vec![
        Span::styled(" ⌕ ", theme.accent),
        Span::styled(query, theme.strong),
        Span::styled("█", theme.accent),
        Span::raw("  "),
        Span::styled(count, found),
        Span::styled(keys, theme.dim),
    ])
    .style(theme.selection)
}

#[cfg(test)]
mod tests {
    use super::{Find, Match, bar, marks, matches};
    use crate::interactive::tui::markdown::plain_text;
    use crate::interactive::tui::theme::Theme;
    use ratatui::layout::Rect;
    use ratatui::text::{Line, Span};

    fn rows_of(texts: &[&str]) -> Vec<Line<'static>> {
        texts
            .iter()
            .map(|text| {
                // Spans split mid-word, as styled rows are.
                let (head, tail) = text.split_at(text.len() / 2);
                Line::from(vec![Span::raw(head.to_owned()), Span::raw(tail.to_owned())])
            })
            .collect()
    }

    #[test]
    fn matching_is_case_insensitive_across_spans_and_counts_cells() {
        let rows = rows_of(&[
            "  ▎ fix the Parser",
            "nothing here",
            "parser, PARSER and parsers",
            "  │ 解析 parser",
        ]);
        let found = matches(&rows, "parser");
        assert_eq!(
            found,
            vec![
                Match {
                    row: 0,
                    column: 12,
                    width: 6
                },
                Match {
                    row: 2,
                    column: 0,
                    width: 6
                },
                Match {
                    row: 2,
                    column: 8,
                    width: 6
                },
                Match {
                    row: 2,
                    column: 19,
                    width: 6
                },
                Match {
                    row: 3,
                    column: 9,
                    width: 6
                },
            ],
            "a wide character before a match moves it two cells"
        );
        assert!(matches(&rows, "").is_empty());
        assert!(matches(&rows, "absent").is_empty());
        assert_eq!(
            matches(&rows_of(&["aaaa"]), "aa").len(),
            2,
            "matches never overlap"
        );
    }

    #[test]
    fn next_and_previous_wrap_and_the_count_follows() {
        let rows = rows_of(&["one x", "two x", "three x"]);
        let mut find = Find::default();
        find.set_query("x".to_owned(), &rows, 0, 1);
        assert_eq!(
            find.count(),
            "2/3",
            "the first match at or after the window's top"
        );
        find.step(true);
        assert_eq!(find.count(), "3/3");
        find.step(true);
        assert_eq!(find.count(), "1/3", "past the last it wraps to the first");
        find.step(false);
        assert_eq!(find.count(), "3/3", "before the first it wraps to the last");
        find.set_query("nothing".to_owned(), &rows, 0, 0);
        assert_eq!(find.count(), "0/0");
        find.step(true);
        assert_eq!(find.current_match(), None);
    }

    #[test]
    fn the_current_match_holds_while_rows_arrive_or_drop() {
        let mut texts = vec!["a x", "b x", "c x"];
        let mut find = Find::default();
        find.set_query("x".to_owned(), &rows_of(&texts), 0, 0);
        find.step(true);
        assert_eq!(find.current_match().map(|found| found.row), Some(1));
        texts.push("d x");
        assert!(
            find.refresh(&rows_of(&texts), 0, 0),
            "new rows are searched"
        );
        assert_eq!(find.current_match().map(|found| found.row), Some(1));
        assert!(
            !find.refresh(&rows_of(&texts), 0, 0),
            "the same rows are not"
        );
        // The oldest row is dropped: the same match is now row 0.
        texts.remove(0);
        find.refresh(&rows_of(&texts), 1, 0);
        assert_eq!(find.current_match().map(|found| found.row), Some(0));
        assert_eq!(find.count(), "1/3");
    }

    #[test]
    fn marks_cover_the_visible_matches_and_single_out_the_current_one() {
        let rows = rows_of(&["x .. x", "....", "x"]);
        let mut find = Find::default();
        find.set_query("x".to_owned(), &rows, 0, 0);
        find.step(true);
        let window = Rect::new(29, 1, 40, 2);
        assert_eq!(
            marks(&find, 0, window),
            vec![
                (Rect::new(29, 1, 1, 1), false),
                (Rect::new(34, 1, 1, 1), true),
            ],
            "the third match is below the window"
        );
        assert_eq!(
            marks(&find, 1, window),
            vec![(Rect::new(29, 2, 1, 1), false)]
        );
    }

    #[test]
    fn the_bar_says_the_query_and_the_count() {
        let rows = rows_of(&["parser", "parser"]);
        let mut find = Find::default();
        find.set_query("parser".to_owned(), &rows, 0, 0);
        let text = plain_text(&[bar(&find, 40, &Theme::plain())]);
        assert!(text.contains("⌕ parser█"), "{text}");
        assert!(text.contains("1/2"), "{text}");
        assert!(text.contains("esc"), "{text}");
        find.set_query("x".repeat(80), &rows, 0, 0);
        let text = plain_text(&[bar(&find, 40, &Theme::plain())]);
        assert!(text.contains("0/0"), "{text}");
        assert!(text.chars().count() <= 41, "{text}");
    }
}
