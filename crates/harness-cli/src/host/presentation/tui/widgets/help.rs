//! The reference overlay used by `/help`, `/status`, `/config` and `/model`.
//!
//! An overlay is temporary: closing it with Escape must leave the history
//! untouched, which is what acceptance U09 asserts. The controller therefore
//! never pushes overlay content as a history item in TUI mode.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use super::super::icons::ToolKind;
use super::super::theme::Theme;
use super::composer::display_width;
use crate::interactive::events::{BadgeKind, RefLine};

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
    rich: Option<&[RefLine]>,
    scroll: usize,
    theme: &Theme,
) {
    let compact = area.height <= 3;
    let width = area.width.saturating_sub(if compact { 0 } else { 2 });
    let base = panel_rows(lines, rich, width, theme);
    let rows: Vec<_> = base
        .into_iter()
        .flat_map(|line| super::super::markdown::wrap_spans(line.spans, width))
        .collect();
    let height = usize::from(if compact {
        area.height
    } else {
        area.height - 2
    })
    .max(1);
    let max_scroll = rows.len().saturating_sub(height);
    let offset = scroll.min(max_scroll);
    let visible: Vec<Line<'static>> = rows.into_iter().skip(offset).take(height).collect();
    if compact {
        frame.render_widget(Paragraph::new(visible), area);
        return;
    }
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
        .border_type(BorderType::Rounded)
        .border_style(theme.border)
        .title(Span::styled(format!(" {title} "), theme.title))
        .title_bottom(Span::styled(hint, theme.dim));
    frame.render_widget(Paragraph::new(visible).block(block), area);
}

/// The badge style for what a badge means.
fn badge_style(kind: BadgeKind, theme: &Theme) -> ratatui::style::Style {
    match kind {
        BadgeKind::Ok => theme.badge_ok,
        BadgeKind::Accent => theme.badge_accent,
        BadgeKind::Warn => theme.badge_warn,
        BadgeKind::Neutral => theme.chip,
    }
}

/// Text cut into rows of at most `cells` cells at word boundaries.
fn wrap_plain(text: &str, cells: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in text.split_whitespace() {
        let joined = display_width(&row) + usize::from(!row.is_empty()) + display_width(word);
        if !row.is_empty() && joined > cells {
            rows.push(std::mem::take(&mut row));
        }
        if !row.is_empty() {
            row.push(' ');
        }
        row.push_str(word);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// Text cut into rows of at most `cells` cells, at spaces where it can be and
/// inside a word too long for a row (a path) where it cannot.
fn wrap_hard(text: &str, cells: usize) -> Vec<String> {
    let cells = cells.max(1);
    let mut rows = Vec::new();
    for row in wrap_plain(text, cells) {
        if display_width(&row) <= cells {
            rows.push(row);
            continue;
        }
        let mut piece = String::new();
        for character in row.chars() {
            let mut buffer = [0; 4];
            if !piece.is_empty()
                && display_width(&piece) + display_width(character.encode_utf8(&mut buffer)) > cells
            {
                rows.push(std::mem::take(&mut piece));
            }
            piece.push(character);
        }
        if !piece.is_empty() {
            rows.push(piece);
        }
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// The style of a field's value for what it means.
fn tone_style(kind: BadgeKind, theme: &Theme) -> ratatui::style::Style {
    match kind {
        BadgeKind::Ok => theme.tool_ok,
        BadgeKind::Accent => theme.accent,
        BadgeKind::Warn => theme.warning,
        BadgeKind::Neutral => ratatui::style::Style::default(),
    }
}

/// A value's spans: a closing note in brackets (`[default]`, `(value hidden)`)
/// is dimmed, so the value itself stands out.
fn value_spans(row: &str, style: ratatui::style::Style, theme: &Theme) -> Vec<Span<'static>> {
    let note = row
        .rfind(" (")
        .filter(|_| row.ends_with(')'))
        .or_else(|| row.rfind(" [").filter(|_| row.ends_with(']')));
    match note {
        Some(at) => vec![
            Span::styled(row[..at].to_owned(), style),
            Span::styled(row[at..].to_owned(), theme.dim),
        ],
        None => vec![Span::styled(row.to_owned(), style)],
    }
}

/// The widest label a value column makes room for; a longer one pushes its own
/// value along instead of the whole column.
const LABEL_CELLS: usize = 14;

/// A plain panel line as a structured row: `Label: value` as a field, the rest
/// as text. A label is a few words at the start of the line: `Config:` is
/// one, and so is `AGENTS.md:`, but `C:\work` and a sentence with a colon
/// in it are not.
#[must_use]
pub fn line_row(line: &str) -> RefLine {
    let field = line.split_once(':').filter(|(label, value)| {
        let label = label.trim_end();
        !label.is_empty()
            && label.len() <= 24
            && label.chars().next().is_some_and(char::is_alphabetic)
            && label.split_whitespace().count() <= 3
            && !label.contains(['/', '\\', '(', '`'])
            && value.starts_with(char::is_whitespace)
            && !value.trim().is_empty()
    });
    match field {
        Some((label, value)) => RefLine::field(label.trim_end(), value.trim()),
        None if line.trim().is_empty() => RefLine::Blank,
        None => RefLine::Text(line.to_owned()),
    }
}

/// The rows of a panel: its structured rows when it has them, else its plain
/// lines read as fields and text, so every reference panel is drawn alike.
#[must_use]
pub fn panel_rows(
    lines: &[String],
    rich: Option<&[RefLine]>,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    match rich {
        Some(rich) => rich_rows(rich, width, theme),
        None if lines.is_empty() => rows(lines, theme),
        None => {
            let converted: Vec<RefLine> = lines.iter().map(|line| line_row(line)).collect();
            rich_rows(&converted, width, theme)
        }
    }
}

/// The rows of a structured reference panel: headings as chips, each item on a
/// row of its own with its description wrapped and hung under the name, and
/// fields with their values in one column.
#[must_use]
pub fn rich_rows(lines: &[RefLine], width: u16, theme: &Theme) -> Vec<Line<'static>> {
    /// Cells the description is indented: past the margin, the glyph and its gap.
    const HANG: usize = 5;
    let column = lines
        .iter()
        .filter_map(|line| match line {
            RefLine::Field { label, .. } => Some(display_width(label)),
            _ => None,
        })
        .filter(|cells| *cells <= LABEL_CELLS)
        .max()
        .unwrap_or(0);
    let mut rows = Vec::new();
    for line in lines {
        match line {
            RefLine::Field { label, value, tone } => {
                let pad = column.max(display_width(label));
                let hang = 2 + pad + 2;
                let room = usize::from(width).saturating_sub(hang + 1).max(12);
                let style = tone_style(*tone, theme);
                for (index, row) in wrap_hard(value, room).into_iter().enumerate() {
                    let mut spans = if index == 0 {
                        vec![
                            Span::raw("  "),
                            Span::styled(label.clone(), theme.muted),
                            Span::raw(" ".repeat(pad - display_width(label) + 2)),
                        ]
                    } else {
                        vec![Span::raw(" ".repeat(hang))]
                    };
                    spans.extend(value_spans(&row, style, theme));
                    rows.push(Line::from(spans));
                }
            }
            RefLine::Text(text) if text.starts_with('/') && text.contains("  ") => {
                rows.extend(self::rows(std::slice::from_ref(text), theme));
            }
            RefLine::Heading { title, note } => {
                let mut spans = vec![
                    Span::raw(" "),
                    Span::styled(format!(" {title} "), theme.badge_accent),
                ];
                if !note.is_empty() {
                    spans.push(Span::styled(format!("  {note}"), theme.dim));
                }
                rows.push(Line::from(spans));
            }
            RefLine::Item {
                glyph,
                name,
                meta,
                badges,
                detail,
            } => {
                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("{glyph} "),
                        ToolKind::Skill
                            .style(theme)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                    Span::styled(name.clone(), theme.strong),
                ];
                if !meta.is_empty() {
                    spans.push(Span::styled(format!("  v{meta}"), theme.dim));
                }
                for (badge, kind) in badges {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        format!(" {} ", badge.to_uppercase()),
                        badge_style(*kind, theme),
                    ));
                }
                rows.push(Line::from(spans));
                let room = usize::from(width).saturating_sub(HANG + 1).max(8);
                for row in wrap_plain(detail, room) {
                    rows.push(Line::from(vec![
                        Span::raw(" ".repeat(HANG)),
                        Span::styled(row, theme.muted),
                    ]));
                }
            }
            RefLine::Text(text) => rows.push(Line::from(Span::raw(format!(" {text}")))),
            RefLine::Hint(text) => rows.push(Line::from(vec![
                Span::styled(" • ", theme.info),
                Span::styled(text.clone(), theme.dim),
            ])),
            RefLine::Blank => rows.push(Line::default()),
        }
    }
    if rows.is_empty() {
        rows.push(Line::from(Span::styled(
            "(không có nội dung)".to_owned(),
            theme.dim,
        )));
    }
    rows
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
    fn end_reaches_the_last_row_after_long_paths_wrap() {
        let backend = ratatui::backend::TestBackend::new(30, 7);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let lines = vec![
            "a long configuration path ".repeat(12),
            "Store: final entry".into(),
        ];
        terminal
            .draw(|frame| {
                super::render(
                    frame,
                    frame.area(),
                    "/config",
                    &lines,
                    None,
                    usize::MAX,
                    &Theme::plain(),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text: String = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        // `Store:` is drawn as a field: the label, then its value in the column.
        assert!(text.contains("Store  final entry"), "{text}");
    }

    /// Every panel's `Label: value` lines share one value column, and a path
    /// too long for a row wraps under the value instead of under the label.
    #[test]
    fn fields_line_up_and_a_long_path_hangs_under_its_value() {
        let lines = vec![
            "Git:     clean".to_owned(),
            r"Store:   C:\Users\me\AppData\Local\HarnessAgents\data\projects\0123456789abcdef"
                .to_owned(),
            "/help  list commands".to_owned(),
        ];
        let text = plain_text(&super::panel_rows(&lines, None, 40, &Theme::plain()));
        let rows: Vec<&str> = text.lines().collect();
        assert!(rows[0].starts_with("  Git    clean"), "{text}");
        assert!(rows[1].starts_with(r"  Store  C:\Users"), "{text}");
        assert!(
            rows[2].starts_with("         "),
            "a path hangs under its value:\n{text}"
        );
        assert!(rows.iter().all(|row| row.chars().count() <= 40), "{text}");
        assert!(text.contains("/help  list commands"), "{text}");
    }

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
    fn a_skills_panel_has_headings_items_badges_and_wrapped_descriptions() {
        use crate::interactive::events::{BadgeKind, RefLine};
        let lines = vec![
            RefLine::Heading {
                title: "✦ Bundled with ha".to_owned(),
                note: "2".to_owned(),
            },
            RefLine::Item {
                glyph: "✦".to_owned(),
                name: "brainstorming".to_owned(),
                meta: "6.4.1".to_owned(),
                badges: vec![("active".to_owned(), BadgeKind::Ok)],
                detail: "Use before any creative work to explore intent and requirements"
                    .to_owned(),
            },
            RefLine::Blank,
            RefLine::Hint("/skill:<name> runs one".to_owned()),
        ];
        let text = plain_text(&super::rich_rows(&lines, 40, &Theme::plain()));
        assert!(text.contains("✦ Bundled with ha"), "{text}");
        assert!(text.contains("✦ brainstorming  v6.4.1"), "{text}");
        assert!(text.contains(" ACTIVE "), "{text}");
        assert!(
            text.lines().all(|line| line.chars().count() <= 40),
            "a description wraps inside the panel:\n{text}"
        );
        assert!(text.contains("• /skill:<name> runs one"), "{text}");
    }

    #[test]
    fn t06_an_empty_overlay_says_so() {
        let text = plain_text(&rows(&[], &Theme::plain()));
        assert!(text.contains("không có nội dung"), "{text}");
    }
}
