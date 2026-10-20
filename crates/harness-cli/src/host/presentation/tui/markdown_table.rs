//! GFM pipe tables in assistant text, after prime-agent's `markdown_table.rs`
//! (its port of `markdown.ts renderTable`): a header row, a delimiter row and
//! data rows, drawn in a box sized to the width with every cell wrapped and
//! padded to its column. A table too narrow to draw keeps its source lines.

use ratatui::text::{Line, Span};

use super::theme::Theme;

/// A parsed pipe table and the source lines it came from.
pub struct Table {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub raw: Vec<String>,
}

/// Split a row into cells on unescaped pipes (marked's `splitCells`): an empty
/// edge cell from a leading or trailing pipe is dropped, cells are trimmed,
/// `\|` is a literal pipe, and a data row is fitted to the header's width.
fn split_cells(row: &str, expected: Option<usize>) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut escaped = false;
    for character in row.chars() {
        match character {
            '\\' if !escaped => {
                escaped = true;
                cell.push(character);
            }
            '|' if !escaped => cells.push(std::mem::take(&mut cell)),
            _ => {
                escaped = false;
                cell.push(character);
            }
        }
    }
    cells.push(cell);
    if cells.len() > 1 && cells[0].trim().is_empty() {
        cells.remove(0);
    }
    if cells.len() > 1 && cells.last().is_some_and(|last| last.trim().is_empty()) {
        cells.pop();
    }
    let mut cells = cells
        .into_iter()
        .map(|cell| cell.trim().replace("\\|", "|"))
        .collect::<Vec<_>>();
    if let Some(expected) = expected {
        cells.resize(expected, String::new());
    }
    cells
}

/// The column count of a delimiter row (`| --- | :-: |`), or `None`.
fn delimiter_columns(line: &str) -> Option<usize> {
    let trimmed = line.trim();
    if trimmed.is_empty() || !(trimmed.contains('|') || trimmed.contains(':')) {
        return None;
    }
    let body = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let body = body.strip_suffix('|').unwrap_or(body);
    let cells = body.split('|').collect::<Vec<_>>();
    cells
        .iter()
        .all(|cell| {
            let dashes = cell.trim().trim_matches(':');
            !dashes.is_empty() && dashes.chars().all(|character| character == '-')
        })
        .then_some(cells.len())
}

/// Whether `header` followed by `next` opens a table: a row with pipes whose
/// cell count is the delimiter row's.
#[must_use]
pub fn is_table_start(header: &str, next: Option<&str>) -> bool {
    let Some(columns) = next.and_then(delimiter_columns) else {
        return false;
    };
    !header.trim().is_empty() && header.contains('|') && split_cells(header, None).len() == columns
}

/// Read a table from `lines[*index]` on; `index` ends after its last row.
#[must_use]
pub fn parse(lines: &[&str], index: &mut usize) -> Table {
    let header = split_cells(lines[*index], None);
    let columns = header.len();
    let mut raw = vec![lines[*index].to_owned(), lines[*index + 1].to_owned()];
    *index += 2;
    let mut rows = Vec::new();
    while *index < lines.len() && lines[*index].contains('|') && !lines[*index].trim().is_empty() {
        rows.push(split_cells(lines[*index], Some(columns)));
        raw.push(lines[*index].to_owned());
        *index += 1;
    }
    Table { header, rows, raw }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .flat_map(|span| span.content.chars())
        .map(super::widgets::composer::char_width)
        .sum()
}

/// The longest word of a cell, at most 30 cells (prime-agent's minimum column).
fn longest_word(text: &str) -> usize {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .map(super::widgets::composer::char_width)
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0)
        .min(30)
}

/// Column widths for `width` cells, or `None` when there is no room.
fn layout(header: &[String], rows: &[Vec<String>], width: usize) -> Option<Vec<usize>> {
    let columns = header.len();
    let overhead = 3 * columns + 1;
    let available = width.checked_sub(overhead)?;
    if columns == 0 || available < columns {
        return None;
    }
    let measure = |text: &str| {
        text.chars()
            .map(super::widgets::composer::char_width)
            .sum::<usize>()
    };
    let mut natural = header.iter().map(|cell| measure(cell)).collect::<Vec<_>>();
    let mut minimum = header
        .iter()
        .map(|cell| longest_word(cell))
        .collect::<Vec<_>>();
    for row in rows {
        for (column, cell) in row.iter().enumerate() {
            natural[column] = natural[column].max(measure(cell));
            minimum[column] = minimum[column].max(longest_word(cell));
        }
    }
    for (column, min) in minimum.iter_mut().enumerate() {
        *min = (*min).max(1).min(natural[column].max(1));
    }
    if natural.iter().sum::<usize>() <= available {
        return Some(natural.iter().map(|natural| (*natural).max(1)).collect());
    }
    if minimum.iter().sum::<usize>() > available {
        // Even the words do not fit: share the room by their size.
        let total: usize = minimum.iter().sum();
        let mut widths = minimum
            .iter()
            .map(|min| (min * available / total.max(1)).max(1))
            .collect::<Vec<_>>();
        while widths.iter().sum::<usize>() > available {
            if let Some(widest) = widths.iter_mut().max() {
                *widest -= 1;
            }
        }
        return Some(widths);
    }
    // Every column gets its longest word, and the rest of the room goes to the
    // columns by how much more they would take.
    let spare = available - minimum.iter().sum::<usize>();
    let growth = natural
        .iter()
        .zip(&minimum)
        .map(|(natural, min)| natural.saturating_sub(*min))
        .collect::<Vec<_>>();
    let total_growth = growth.iter().sum::<usize>().max(1);
    let mut widths = minimum
        .iter()
        .zip(&growth)
        .map(|(min, grow)| min + grow * spare / total_growth)
        .collect::<Vec<_>>();
    let mut leftover = available.saturating_sub(widths.iter().sum());
    for (column, width) in widths.iter_mut().enumerate() {
        if leftover == 0 {
            break;
        }
        if *width < natural[column] {
            *width += 1;
            leftover -= 1;
        }
    }
    Some(widths)
}

/// Draw `table` in at most `width` cells.
pub fn render(
    table: &Table,
    width: u16,
    theme: &Theme,
    inline: &dyn Fn(&str) -> Vec<Span<'static>>,
) -> Vec<Line<'static>> {
    let Some(widths) = layout(&table.header, &table.rows, usize::from(width)) else {
        return table
            .raw
            .iter()
            .flat_map(|line| super::markdown::wrap_spans(vec![Span::raw(line.clone())], width))
            .collect();
    };
    let border = theme.border;
    let rule = |left: char, middle: char, right: char| {
        let inner = widths
            .iter()
            .map(|width| "─".repeat(*width))
            .collect::<Vec<_>>()
            .join(&format!("─{middle}─"));
        Line::from(Span::styled(format!("{left}─{inner}─{right}"), border))
    };
    let mut out = vec![rule('┌', '┬', '┐')];
    let row_lines = |cells: &[String]| -> Vec<Line<'static>> {
        let wrapped = cells
            .iter()
            .zip(&widths)
            .map(|(cell, width)| {
                let lines = super::markdown::wrap_spans(
                    inline(cell),
                    u16::try_from(*width).unwrap_or(u16::MAX),
                );
                if lines.is_empty() {
                    vec![Line::default()]
                } else {
                    lines
                }
            })
            .collect::<Vec<_>>();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        (0..height)
            .map(|index| {
                let mut spans = vec![Span::styled("│ ".to_owned(), border)];
                for (column, cell) in wrapped.iter().enumerate() {
                    if column > 0 {
                        spans.push(Span::styled(" │ ".to_owned(), border));
                    }
                    let text = cell.get(index).cloned().unwrap_or_default();
                    let pad = widths[column].saturating_sub(spans_width(&text.spans));
                    spans.extend(text.spans);
                    if pad > 0 {
                        spans.push(Span::raw(" ".repeat(pad)));
                    }
                }
                spans.push(Span::styled(" │".to_owned(), border));
                Line::from(spans)
            })
            .collect()
    };
    out.extend(row_lines(&table.header));
    out.push(rule('├', '┼', '┤'));
    for (index, row) in table.rows.iter().enumerate() {
        out.extend(row_lines(row));
        if index + 1 < table.rows.len() {
            out.push(rule('├', '┼', '┤'));
        }
    }
    out.push(rule('└', '┴', '┘'));
    out
}
