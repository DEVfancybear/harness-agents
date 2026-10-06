//! prime-agent's fullscreen rendering (`packages/tui/src/fullscreen.ts`).
//!
//! The alternate screen holds a scrollable window over the conversation with
//! the dock - the live block, a panel or menu, the composer, the hints and the
//! status line - pinned to the bottom rows, and prime-agent's top bar (the
//! chat's name and its spend) pinned to the top row. The scroll position is
//! application state, not terminal scrollback: the mouse wheel scrolls three
//! rows, PageUp/PageDown a page, Shift+Alt+Up goes to the top and
//! Ctrl+Shift+Down (or Ctrl+End, which Windows Terminal lets through) back to
//! the end, following the output again. A drag selects
//! text - anchored to the conversation's rows, so streaming and scrolling never
//! shift it, and scrolling on its own when it reaches an edge - and the release
//! copies it. Leaving fullscreen prints what it showed into the primary
//! screen's scrollback, as prime-agent's inline renderer resumes.

use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use ratatui::backend::Backend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Terminal, TerminalOptions, Viewport as RatatuiViewport};
use unicode_width::UnicodeWidthStr;

use super::super::events::{Detail, HistoryItem, MouseInput, MouseKind, UiState};
use super::super::paths::LaunchEnvironment;
use super::theme::Theme;
use super::{REPRINT_ENTRIES, history, layout, to_io, widgets};

/// prime-agent's `FULLSCREEN_MIN_TRANSCRIPT_ROWS`: the dock never takes these.
pub const MIN_TRANSCRIPT_ROWS: u16 = 3;

/// prime-agent's `WHEEL_SCROLL_LINES`.
pub const WHEEL_SCROLL_LINES: isize = 3;

/// How often a drag held at an edge scrolls the selection one row further.
const AUTO_SCROLL_EVERY: Duration = Duration::from_millis(50);

/// The follow key prime-agent's hint names (`tui.viewport.follow`).
pub const FOLLOW_KEY: &str = "ctrl+end";

/// Whether this terminal renders fullscreen, and with the mouse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Prefs {
    pub enabled: bool,
    pub mouse: bool,
}

/// prime-agent's `getFullscreen` and `getFullscreenMouse`: `HA_FULLSCREEN`
/// (prime-agent's `PI_FULLSCREEN`: on only for `1`) wins, else
/// `terminal.fullscreen` in `settings.json`, on by default; the mouse is
/// `terminal.fullscreenMouse`, on by default.
#[must_use]
pub fn prefs(environment: &LaunchEnvironment, config_file: &Path) -> Prefs {
    let terminal = super::super::config::load_setting(config_file, "terminal");
    let setting = |key: &str| {
        terminal
            .as_ref()
            .and_then(|terminal| terminal.get(key))
            .and_then(serde_json::Value::as_bool)
    };
    let enabled = match environment
        .value("HA_FULLSCREEN")
        .and_then(|value| value.to_str())
    {
        Some(value) => value.trim() == "1",
        None => setting("fullscreen").unwrap_or(true),
    };
    Prefs {
        enabled,
        mouse: setting("fullscreenMouse").unwrap_or(true),
    }
}

/// prime-agent's `setFullscreen`: keep `terminal.fullscreen` in `settings.json`.
///
/// # Errors
/// The settings file cannot be written.
pub fn save(config_file: &Path, enabled: bool) -> Result<(), String> {
    let mut terminal = super::super::config::load_setting(config_file, "terminal")
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(object) = terminal.as_object_mut() {
        object.insert("fullscreen".to_owned(), serde_json::json!(enabled));
    }
    super::super::config::save_setting(config_file, "terminal", Some(terminal))
}

/// A selection endpoint: a conversation row (or a frame row) and a cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Point {
    line: usize,
    col: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    /// Rows of the conversation, anchored so scrolling never shifts them.
    Transcript,
    /// Cells of the frame as it was when the drag began: the dock or a panel.
    Frame,
}

/// prime-agent's `FullscreenViewport`: the scroll position and the selection.
#[derive(Debug)]
pub struct Viewport {
    scroll_top: usize,
    following: bool,
    max_scroll: usize,
    window_top: u16,
    window_height: u16,
    anchor: Option<Point>,
    head: Option<Point>,
    mode: Option<Mode>,
    /// The last frame's cells, row by row, for a frame selection.
    frame: Vec<Vec<String>>,
    /// The frame a frame selection copies from: the one it began on.
    frame_snapshot: Option<Vec<Vec<String>>>,
    /// The first frame row a frame selection may start on (the dock's top),
    /// or every row while a panel is open.
    selectable_from: u16,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            scroll_top: 0,
            following: true,
            max_scroll: 0,
            window_top: 0,
            window_height: 0,
            anchor: None,
            head: None,
            mode: None,
            frame: Vec::new(),
            frame_snapshot: None,
            selectable_from: 0,
        }
    }
}

impl Viewport {
    /// Place the window over `total` conversation rows: following pins it to
    /// the end, otherwise it stays where it was while rows append.
    pub fn compose(&mut self, total: usize, window_top: u16, window_height: u16) -> usize {
        let max_scroll = total.saturating_sub(usize::from(window_height));
        self.scroll_top = if self.following {
            max_scroll
        } else {
            self.scroll_top.min(max_scroll)
        };
        self.max_scroll = max_scroll;
        self.window_top = window_top;
        self.window_height = window_height;
        self.scroll_top
    }

    /// Scrolling up pauses following; reaching the bottom resumes it.
    pub fn scroll_by(&mut self, delta: isize) {
        let base = if self.following {
            self.max_scroll
        } else {
            self.scroll_top
        };
        self.scroll_top = base.saturating_add_signed(delta).min(self.max_scroll);
        self.following = self.scroll_top >= self.max_scroll;
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_top = 0;
        self.following = self.max_scroll == 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_top = self.max_scroll;
        self.following = true;
    }

    #[must_use]
    pub fn page_size(&self) -> isize {
        isize::try_from(self.window_height.saturating_sub(1).max(1)).unwrap_or(1)
    }

    #[must_use]
    pub const fn is_following(&self) -> bool {
        self.following
    }

    #[cfg(test)]
    #[must_use]
    pub const fn scroll_top(&self) -> usize {
        self.scroll_top
    }

    /// The conversation row under a screen row, or `None` outside the window
    /// (unless `clamp`, which answers the nearest window row).
    fn transcript_line(&self, row: u16, clamp: bool) -> Option<usize> {
        if self.window_height == 0 {
            return None;
        }
        let last = self.window_top + self.window_height - 1;
        let row = if clamp {
            row.clamp(self.window_top, last)
        } else if (self.window_top..=last).contains(&row) {
            row
        } else {
            return None;
        };
        Some(self.scroll_top + usize::from(row - self.window_top))
    }

    /// Begin a selection in the conversation; false outside its window.
    pub fn begin_selection(&mut self, row: u16, col: u16) -> bool {
        let Some(line) = self.transcript_line(row, false) else {
            self.clear_selection();
            return false;
        };
        let point = Point {
            line,
            col: usize::from(col),
        };
        self.anchor = Some(point);
        self.head = Some(point);
        self.mode = Some(Mode::Transcript);
        true
    }

    /// Begin a selection over the frame (the dock, or an open panel).
    pub fn begin_frame_selection(&mut self, row: u16, col: u16) -> bool {
        if row < self.selectable_from || usize::from(row) >= self.frame.len() {
            self.clear_selection();
            return false;
        }
        let point = Point {
            line: usize::from(row),
            col: usize::from(col),
        };
        self.frame_snapshot = Some(self.frame.clone());
        self.anchor = Some(point);
        self.head = Some(point);
        self.mode = Some(Mode::Frame);
        true
    }

    /// Move the selection's free end.
    pub fn extend(&mut self, row: u16, col: u16) {
        if self.anchor.is_none() {
            return;
        }
        let line = match self.mode {
            Some(Mode::Transcript) => self.transcript_line(row, true),
            Some(Mode::Frame) => {
                let rows = self
                    .frame_snapshot
                    .as_ref()
                    .map_or(self.frame.len(), Vec::len);
                (rows > 0).then(|| usize::from(row).min(rows - 1))
            }
            None => None,
        };
        if let Some(line) = line {
            self.head = Some(Point {
                line,
                col: usize::from(col),
            });
        }
    }

    /// Which way a drag held at an edge of the window scrolls, if it does.
    #[must_use]
    pub fn auto_scroll_direction(&self, row: u16) -> Option<isize> {
        let (Some(anchor), Some(head), Some(Mode::Transcript)) =
            (self.anchor, self.head, self.mode)
        else {
            return None;
        };
        if self.window_height == 0 {
            return None;
        }
        let last = self.window_top + self.window_height - 1;
        if head.line < anchor.line && row <= self.window_top && self.scroll_top > 0 {
            return Some(-1);
        }
        if head.line > anchor.line && row >= last && self.scroll_top < self.max_scroll {
            return Some(1);
        }
        None
    }

    /// Scroll one row with a held drag, and move its end along.
    pub fn scroll_selection(&mut self, direction: isize, col: u16) -> bool {
        if self.mode != Some(Mode::Transcript) {
            return false;
        }
        let before = self.scroll_top;
        self.scroll_by(direction);
        if self.scroll_top == before || self.window_height == 0 {
            return false;
        }
        let edge = if direction < 0 {
            self.window_top
        } else {
            self.window_top + self.window_height - 1
        };
        self.extend(edge, col);
        true
    }

    fn ordered(&self) -> Option<(Point, Point)> {
        let (anchor, head) = (self.anchor?, self.head?);
        if anchor == head {
            return None;
        }
        let flipped =
            anchor.line > head.line || (anchor.line == head.line && anchor.col > head.col);
        Some(if flipped {
            (head, anchor)
        } else {
            (anchor, head)
        })
    }

    /// The selected cells of one row, `[from, to)`.
    fn span(line: usize, (start, end): (Point, Point)) -> Option<(usize, usize)> {
        if line < start.line || line > end.line {
            return None;
        }
        let from = if line == start.line { start.col } else { 0 };
        let to = if line == end.line {
            end.col
        } else {
            usize::MAX
        };
        (to > from).then_some((from, to))
    }

    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.ordered().is_some()
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
        self.head = None;
        self.mode = None;
        self.frame_snapshot = None;
    }

    /// Finish the selection and return its text (`None` when empty).
    pub fn end_selection(&mut self, transcript: &[Line<'static>]) -> Option<String> {
        let selection = self.ordered();
        let mode = self.mode;
        let frame = self
            .frame_snapshot
            .take()
            .unwrap_or_else(|| self.frame.clone());
        self.clear_selection();
        let selection = selection?;
        let mut lines = Vec::new();
        for line in selection.0.line..=selection.1.line {
            let Some((from, to)) = Self::span(line, selection) else {
                continue;
            };
            let text = match mode {
                Some(Mode::Transcript) => transcript
                    .get(line)
                    .map(|row| slice_columns(&line_text(row), from, to))
                    .unwrap_or_default(),
                Some(Mode::Frame) => frame
                    .get(line)
                    .map(|cells| slice_cells(cells, from, to))
                    .unwrap_or_default(),
                None => continue,
            };
            lines.push(text.trim_end().to_owned());
        }
        let text = lines.join("\n");
        (!text.trim().is_empty()).then_some(text)
    }

    /// The selected cells of the frame about to be drawn, as screen rects.
    fn highlighted(&self, width: u16) -> Vec<Rect> {
        let Some(selection) = self.ordered() else {
            return Vec::new();
        };
        let mut rects = Vec::new();
        let mut push = |row: u16, (from, to): (usize, usize)| {
            let from = u16::try_from(from).unwrap_or(u16::MAX).min(width);
            let to = u16::try_from(to).unwrap_or(u16::MAX).min(width);
            if to > from {
                rects.push(Rect::new(from, row, to - from, 1));
            }
        };
        match self.mode {
            Some(Mode::Transcript) => {
                for offset in 0..self.window_height {
                    let line = self.scroll_top + usize::from(offset);
                    if let Some(span) = Self::span(line, selection) {
                        push(self.window_top + offset, span);
                    }
                }
            }
            Some(Mode::Frame) => {
                for line in selection.0.line..=selection.1.line {
                    if let (Some(span), Ok(row)) =
                        (Self::span(line, selection), u16::try_from(line))
                    {
                        push(row, span);
                    }
                }
            }
            None => {}
        }
        rects
    }
}

/// The plain text of a rendered row.
fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// The characters of `text` between display columns `from` and `to`.
fn slice_columns(text: &str, from: usize, to: usize) -> String {
    let mut column = 0;
    let mut out = String::new();
    for character in text.chars() {
        let width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if column >= to {
            break;
        }
        if column >= from {
            out.push(character);
        }
        column += width;
    }
    out
}

/// The text of frame cells `[from, to)`; the cell a wide character covers
/// after its first adds nothing.
fn slice_cells(cells: &[String], from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut skip = 0;
    for (index, symbol) in cells.iter().enumerate() {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        let width = symbol.width();
        if index >= to {
            break;
        }
        if index >= from {
            out.push_str(symbol);
        }
        skip = width.saturating_sub(1);
    }
    out
}

/// The cells of every row of a drawn buffer.
fn frame_cells(buffer: &Buffer) -> Vec<Vec<String>> {
    let area = buffer.area;
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| {
                    buffer
                        .cell((area.x + x, area.y + y))
                        .map_or_else(String::new, |cell| cell.symbol().to_owned())
                })
                .collect()
        })
        .collect()
}

/// What one mouse report did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MouseOutcome {
    /// The frame changed: draw it again.
    pub redraw: bool,
    /// A selection ended: copy this text.
    pub copied: Option<String>,
}

/// The fullscreen screen: the conversation rows, the viewport over them, and
/// the dock drawn by the same widgets as the inline viewport.
pub struct FullscreenScreen<B: Backend> {
    terminal: Terminal<B>,
    theme: Theme,
    detail: Detail,
    /// What the conversation shows, newest last (as the inline renderer keeps it).
    shown: VecDeque<HistoryItem>,
    /// How many of `shown` arrived since fullscreen was entered: they are
    /// printed into the primary screen's scrollback when it is left.
    since_entered: usize,
    /// `shown` drawn at `rows_key`'s width and detail mode.
    rows: Vec<Line<'static>>,
    rows_key: Option<(u16, Detail)>,
    assistant_continuing: bool,
    viewport: Viewport,
    /// A drag held at an edge: its direction, cell and last scroll.
    auto_scroll: Option<(isize, u16, u16, Instant)>,
}

impl<B: Backend> FullscreenScreen<B>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Open the screen over an alternate screen the caller entered.
    ///
    /// # Errors
    /// The backend cannot be measured.
    pub fn open(
        backend: B,
        theme: &Theme,
        detail: Detail,
        shown: VecDeque<HistoryItem>,
    ) -> io::Result<Self> {
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: RatatuiViewport::Fullscreen,
            },
        )
        .map_err(to_io)?;
        terminal.clear().map_err(to_io)?;
        let assistant_continuing = matches!(shown.back(), Some(HistoryItem::Assistant { .. }));
        Ok(Self {
            terminal,
            theme: *theme,
            detail,
            shown,
            since_entered: 0,
            rows: Vec::new(),
            rows_key: None,
            assistant_continuing,
            viewport: Viewport::default(),
            auto_scroll: None,
        })
    }

    /// The conversation it shows, and how many of those entries arrived while
    /// fullscreen: what leaving it prints.
    #[must_use]
    pub fn into_shown(self) -> (VecDeque<HistoryItem>, usize) {
        (self.shown, self.since_entered)
    }

    #[cfg(test)]
    pub fn backend(&self) -> &B {
        self.terminal.backend()
    }

    pub fn columns(&self) -> u16 {
        self.terminal.size().map_or(0, |size| size.width)
    }

    pub fn rows_count(&self) -> u16 {
        self.terminal.size().map_or(0, |size| size.height)
    }

    /// Draw every entry again at this width and detail mode, when either changed.
    fn ensure_rows(&mut self, width: u16) {
        if self.rows_key == Some((width, self.detail)) {
            return;
        }
        let mut rows = Vec::new();
        for (index, item) in self.shown.iter().enumerate() {
            let continuing =
                index > 0 && matches!(self.shown[index - 1], HistoryItem::Assistant { .. });
            rows.extend(history::render_fragment(
                item,
                width,
                &self.theme,
                self.detail,
                continuing,
            ));
        }
        self.rows = rows;
        self.rows_key = Some((width, self.detail));
        // Rows were re-wrapped: a selection anchored to the old ones is gone.
        self.viewport.clear_selection();
    }

    pub fn insert_history_batch(&mut self, items: &[HistoryItem]) {
        let width = self.columns();
        let mut dropped = false;
        for item in items {
            if self.shown.len() == REPRINT_ENTRIES {
                self.shown.pop_front();
                dropped = true;
            }
            self.shown.push_back(item.clone());
            self.since_entered = (self.since_entered + 1).min(self.shown.len());
            if !dropped && self.rows_key == Some((width, self.detail)) {
                self.rows.extend(history::render_fragment(
                    item,
                    width,
                    &self.theme,
                    self.detail,
                    self.assistant_continuing,
                ));
            }
            self.assistant_continuing = matches!(item, HistoryItem::Assistant { .. });
        }
        if dropped {
            self.rows_key = None;
        }
    }

    /// Draw the conversation again in this detail mode (ctrl+o, a resize).
    pub fn reprint(&mut self, detail: Detail) {
        self.detail = detail;
        self.rows_key = None;
    }

    /// Replace the opening banner's lines: the model or the level changed.
    pub fn retitle(&mut self, lines: &[String]) {
        if let Some(HistoryItem::Banner { lines: banner }) = self
            .shown
            .iter_mut()
            .find(|item| matches!(item, HistoryItem::Banner { .. }))
        {
            banner.clone_from(&lines.to_vec());
            self.rows_key = None;
        }
    }

    /// Take over a conversation another process drew (`ha attach`).
    pub fn restore(&mut self, items: &[HistoryItem]) {
        self.shown = items
            .iter()
            .skip(items.len().saturating_sub(REPRINT_ENTRIES))
            .cloned()
            .collect();
        self.since_entered = self.shown.len();
        self.assistant_continuing =
            matches!(self.shown.back(), Some(HistoryItem::Assistant { .. }));
        self.rows_key = None;
        self.viewport.scroll_to_bottom();
    }

    /// Force the next frame to paint every cell.
    ///
    /// # Errors
    /// The terminal cannot be cleared.
    pub fn clear(&mut self) -> io::Result<()> {
        self.terminal.clear().map_err(to_io)
    }

    /// Draw one frame: the top bar, the window over the conversation, the dock.
    ///
    /// # Errors
    /// The terminal cannot be drawn to.
    pub fn draw(&mut self, state: &UiState) -> io::Result<()> {
        self.detail = state.detail;
        let size = self.terminal.size().map_err(to_io)?;
        self.ensure_rows(size.width);
        let theme = self.theme;
        let rows = &self.rows;
        let viewport = &mut self.viewport;
        let mut dock_top = 0;
        let completed = self
            .terminal
            .draw(|frame| {
                let area = frame.area();
                // prime-agent's top bar is taken from the window, never from the
                // dock, and goes first when the screen is short.
                let header = u16::from(area.height > MIN_TRANSCRIPT_ROWS + 8);
                let floor = header + MIN_TRANSCRIPT_ROWS;
                let dock_area = Rect::new(
                    area.x,
                    area.y + floor.min(area.height),
                    area.width,
                    area.height.saturating_sub(floor),
                );
                let plan = layout::plan(dock_area, state, &theme);
                dock_top = [plan.live, plan.modal, plan.queue, plan.suggest]
                    .into_iter()
                    .flatten()
                    .chain([plan.composer])
                    .filter(|rect| rect.height > 0)
                    .map(|rect| rect.y)
                    .min()
                    .unwrap_or(dock_area.y)
                    .max(area.y + header);
                // One blank row between the conversation and the dock, so the
                // newest row never sits on the composer's border.
                let window_rows = dock_top - (area.y + header);
                let window = Rect::new(
                    area.x,
                    area.y + header,
                    area.width,
                    if window_rows > 1 {
                        window_rows - 1
                    } else {
                        window_rows
                    },
                );
                let top = viewport.compose(rows.len(), window.y, window.height);
                let visible = rows
                    .iter()
                    .skip(top)
                    .take(usize::from(window.height))
                    .cloned()
                    .collect::<Vec<_>>();
                frame.render_widget(Paragraph::new(visible), window);
                if header > 0 {
                    frame.render_widget(
                        Paragraph::new(top_bar(state, area.width, &theme)),
                        Rect::new(area.x, area.y, area.width, 1),
                    );
                }
                widgets::render(frame, &plan, state, &theme);
                if let Some((x, y)) = plan.cursor {
                    frame.set_cursor_position((x, y));
                }
                // prime-agent's follow hint, over the window's last row.
                if !viewport.is_following() && window.height > 0 {
                    let label = format!(" {FOLLOW_KEY} to follow ");
                    let width = u16::try_from(label.width()).unwrap_or(u16::MAX);
                    if width <= area.width {
                        let hint = Rect::new(
                            area.x + (area.width - width) / 2,
                            window.y + window.height - 1,
                            width,
                            1,
                        );
                        frame.render_widget(
                            Paragraph::new(Line::from(Span::styled(
                                label,
                                Style::new().add_modifier(Modifier::REVERSED),
                            ))),
                            hint,
                        );
                    }
                }
                let buffer = frame.buffer_mut();
                for rect in viewport.highlighted(area.width) {
                    buffer.set_style(rect, Style::new().add_modifier(Modifier::REVERSED));
                }
            })
            .map_err(to_io)?;
        let cells = frame_cells(completed.buffer);
        self.viewport.frame = cells;
        self.viewport.selectable_from = if state.modal.is_some() { 0 } else { dock_top };
        Ok(())
    }

    /// prime-agent's `handleFullscreenInput` for a mouse report: the wheel
    /// scrolls the conversation, a drag selects and the release copies. While a
    /// panel has the focus the wheel is left alone and a drag selects the frame.
    pub fn mouse(&mut self, input: MouseInput, panel_open: bool) -> MouseOutcome {
        let (row, col) = (input.row, input.column);
        let mut outcome = MouseOutcome::default();
        if !panel_open {
            match input.kind {
                MouseKind::WheelUp | MouseKind::WheelDown => {
                    self.auto_scroll = None;
                    self.viewport
                        .scroll_by(if input.kind == MouseKind::WheelUp {
                            -WHEEL_SCROLL_LINES
                        } else {
                            WHEEL_SCROLL_LINES
                        });
                    outcome.redraw = true;
                    return outcome;
                }
                MouseKind::Press => {
                    self.auto_scroll = None;
                    if !self.viewport.begin_selection(row, col) {
                        self.viewport.begin_frame_selection(row, col);
                    }
                    outcome.redraw = true;
                    return outcome;
                }
                _ => {}
            }
        } else if input.kind == MouseKind::Press {
            self.auto_scroll = None;
            if !self.viewport.begin_frame_selection(row, col) {
                self.viewport.begin_selection(row, col);
            }
            outcome.redraw = true;
            return outcome;
        }
        match input.kind {
            MouseKind::Drag => {
                self.viewport.extend(row, col);
                self.auto_scroll = (!panel_open)
                    .then(|| self.viewport.auto_scroll_direction(row))
                    .flatten()
                    .map(|direction| (direction, row, col, Instant::now()));
                outcome.redraw = true;
            }
            MouseKind::Release => {
                self.auto_scroll = None;
                if self.viewport.has_selection() {
                    outcome.copied = self.viewport.end_selection(&self.rows);
                } else {
                    self.viewport.clear_selection();
                }
                outcome.redraw = true;
            }
            _ => {}
        }
        outcome
    }

    /// A drag held at an edge scrolls on its own; true when it did.
    pub fn tick(&mut self) -> bool {
        let Some((direction, row, col, at)) = self.auto_scroll else {
            return false;
        };
        if at.elapsed() < AUTO_SCROLL_EVERY {
            return false;
        }
        if self.viewport.auto_scroll_direction(row) != Some(direction)
            || !self.viewport.scroll_selection(direction, col)
        {
            self.auto_scroll = None;
            return false;
        }
        self.auto_scroll = Some((direction, row, col, Instant::now()));
        true
    }

    /// prime-agent's viewport keys: a page up or down, the top, the end.
    pub fn page(&mut self, down: bool) {
        let page = self.viewport.page_size();
        self.viewport.scroll_by(if down { page } else { -page });
    }

    pub fn scroll_to_top(&mut self) {
        self.viewport.scroll_to_top();
    }

    pub fn follow(&mut self) {
        self.viewport.scroll_to_bottom();
    }
}

/// prime-agent's `TopBar`: the chat's name centered in plain text, its spend
/// beside it.
fn top_bar(state: &UiState, width: u16, theme: &Theme) -> Line<'static> {
    let name = state
        .header
        .iter()
        .find_map(|line| line.strip_prefix("Chat: "))
        .unwrap_or_default()
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if name.is_empty() {
        return Line::default();
    }
    let cost = state
        .header
        .iter()
        .find_map(|line| line.strip_prefix("Cost: "))
        .filter(|cost| cost.starts_with('$'))
        .map(str::to_owned);
    let shown = 2 + name.width();
    let start = usize::from(width).saturating_sub(shown) / 2;
    let mut spans = vec![
        Span::raw(" ".repeat(start)),
        Span::styled("✦ ", theme.accent),
        Span::styled(name, theme.strong),
    ];
    if let Some(cost) = cost {
        spans.push(Span::styled(format!("  ◎ {cost}"), theme.dim));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::{FullscreenScreen, MouseInput, MouseKind, Viewport, prefs, save, slice_cells};
    use crate::interactive::events::{AppPhase, Detail, HistoryItem};
    use crate::interactive::paths::LaunchEnvironment;
    use crate::interactive::tui::theme::Theme;

    #[test]
    fn fullscreen_is_prime_agents_setting() {
        let home = tempfile::tempdir().expect("temp");
        let config = home.path().join("config.toml");
        let none = LaunchEnvironment::from_pairs::<_, &str, &str>([]);
        assert!(prefs(&none, &config).enabled, "on by default");
        assert!(prefs(&none, &config).mouse);
        save(&config, false).expect("saved");
        assert!(!prefs(&none, &config).enabled);
        assert!(
            prefs(
                &LaunchEnvironment::from_pairs([("HA_FULLSCREEN", "1")]),
                &config
            )
            .enabled,
            "the environment wins"
        );
        save(&config, true).expect("saved");
        assert!(
            !prefs(
                &LaunchEnvironment::from_pairs([("HA_FULLSCREEN", "0")]),
                &config
            )
            .enabled,
            "only 1 turns it on"
        );
        std::fs::write(
            home.path().join("settings.json"),
            r#"{"terminal": {"fullscreen": true, "fullscreenMouse": false}}"#,
        )
        .expect("settings");
        assert!(!prefs(&none, &config).mouse);
    }

    #[test]
    fn scrolling_up_pauses_following_and_the_bottom_resumes_it() {
        let mut viewport = Viewport::default();
        assert_eq!(
            viewport.compose(100, 1, 10),
            90,
            "following starts at the end"
        );
        viewport.scroll_by(-3);
        assert!(!viewport.is_following());
        assert_eq!(
            viewport.compose(120, 1, 10),
            87,
            "new rows do not move a paused window"
        );
        viewport.scroll_by(1000);
        assert!(viewport.is_following());
        assert_eq!(viewport.compose(130, 1, 10), 120);
        viewport.scroll_to_top();
        assert_eq!(viewport.compose(130, 1, 10), 0);
        assert_eq!(viewport.page_size(), 9);
    }

    #[test]
    fn a_selection_is_anchored_to_the_conversation_rows() {
        let rows = (0..40)
            .map(|index| ratatui::text::Line::from(format!("row {index} text")))
            .collect::<Vec<_>>();
        let mut viewport = Viewport::default();
        viewport.compose(rows.len(), 0, 10);
        viewport.scroll_by(-5);
        viewport.compose(rows.len(), 0, 10);
        assert!(viewport.begin_selection(2, 4), "row 2 of the window");
        viewport.extend(3, 6);
        // Scrolling does not shift what was selected.
        viewport.scroll_by(-2);
        viewport.compose(rows.len(), 0, 10);
        assert_eq!(
            viewport.end_selection(&rows).as_deref(),
            Some("27 text\nrow 28")
        );
        assert!(!viewport.has_selection());
        assert!(!viewport.begin_selection(20, 0), "below the window");
    }

    #[test]
    fn a_drag_at_the_edge_scrolls_the_selection() {
        let mut viewport = Viewport::default();
        viewport.compose(50, 0, 10);
        viewport.scroll_by(-20);
        viewport.compose(50, 0, 10);
        assert!(viewport.begin_selection(5, 0));
        viewport.extend(9, 3);
        assert_eq!(viewport.auto_scroll_direction(9), Some(1));
        let top = viewport.scroll_top();
        assert!(viewport.scroll_selection(1, 3));
        assert_eq!(viewport.scroll_top(), top + 1);
    }

    #[test]
    fn a_wide_character_is_copied_once() {
        let cells = ["日", " ", "a", "b"].map(str::to_owned);
        assert_eq!(slice_cells(&cells, 0, 4), "日ab");
    }

    fn screen(columns: u16, rows: u16) -> FullscreenScreen<ratatui::backend::TestBackend> {
        FullscreenScreen::open(
            ratatui::backend::TestBackend::new(columns, rows),
            &Theme::plain(),
            Detail::default(),
            std::collections::VecDeque::new(),
        )
        .expect("screen")
    }

    fn painted(screen: &FullscreenScreen<ratatui::backend::TestBackend>) -> Vec<String> {
        let buffer = screen.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn the_dock_is_pinned_and_the_wheel_scrolls_the_conversation() {
        let mut screen = screen(60, 20);
        let mut state = crate::interactive::tui::tests::idle_state();
        state.header.push("Chat: demo-shop".to_owned());
        let items = (0..40)
            .map(|index| HistoryItem::Notice {
                message: format!("line-{index}"),
            })
            .collect::<Vec<_>>();
        screen.insert_history_batch(&items);
        screen.draw(&state).expect("drawn");
        let rows = painted(&screen);
        assert!(rows[0].contains("demo-shop"), "the top bar: {rows:?}");
        assert!(
            rows.iter().any(|row| row.contains("line-39")),
            "following shows the newest: {rows:?}"
        );
        assert!(
            rows.last().is_some_and(|row| !row.is_empty()),
            "the status line is the last row: {rows:?}"
        );
        let wheel = |kind| MouseInput {
            kind,
            column: 5,
            row: 3,
            modified: false,
        };
        for _ in 0..4 {
            assert!(screen.mouse(wheel(MouseKind::WheelUp), false).redraw);
        }
        screen.draw(&state).expect("drawn");
        let rows = painted(&screen);
        assert!(!rows.iter().any(|row| row.contains("line-39")), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.contains("ctrl+end to follow")),
            "the follow hint: {rows:?}"
        );
        screen.follow();
        screen.draw(&state).expect("drawn");
        assert!(painted(&screen).iter().any(|row| row.contains("line-39")));
        assert_eq!(state.phase, AppPhase::Ready);
    }

    #[test]
    fn a_drag_copies_what_it_covered() {
        let mut screen = screen(60, 20);
        let state = crate::interactive::tui::tests::idle_state();
        screen.insert_history_batch(&[HistoryItem::Notice {
            message: "copy-me-please".to_owned(),
        }]);
        screen.draw(&state).expect("drawn");
        let rows = painted(&screen);
        let row = rows
            .iter()
            .position(|row| row.contains("copy-me-please"))
            .expect("drawn");
        let byte = rows[row].find("copy-me-please").expect("column");
        let column = unicode_width::UnicodeWidthStr::width(&rows[row][..byte]);
        let at = |kind, column: usize| MouseInput {
            kind,
            column: u16::try_from(column).expect("column"),
            row: u16::try_from(row).expect("row"),
            modified: false,
        };
        screen.mouse(at(MouseKind::Press, column), false);
        screen.mouse(at(MouseKind::Drag, column + 7), false);
        screen.draw(&state).expect("drawn");
        let copied = screen.mouse(at(MouseKind::Release, column + 7), false);
        assert_eq!(copied.copied.as_deref(), Some("copy-me"));
    }
}
