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
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Terminal, TerminalOptions, Viewport as RatatuiViewport};
use unicode_width::UnicodeWidthStr;

use super::super::events::{Detail, HistoryItem, Key, MouseInput, MouseKind, UiState};
use super::super::paths::LaunchEnvironment;
use super::theme::Theme;
use super::{REPRINT_ENTRIES, history, layout, to_io, widgets};

/// prime-agent's `FULLSCREEN_MIN_TRANSCRIPT_ROWS`: the dock never takes these.
pub const MIN_TRANSCRIPT_ROWS: u16 = 3;

/// prime-agent's `WHEEL_SCROLL_LINES`.
pub const WHEEL_SCROLL_LINES: isize = 3;

/// How often a drag held at an edge scrolls the selection one row further.
const AUTO_SCROLL_EVERY: Duration = Duration::from_millis(50);

/// How many screens of the newest rows are drawn at once after the width or
/// the detail mode changed; the older entries follow between frames.
const FIRST_WINDOWS: usize = 4;

/// How long one turn of the loop may spend drawing older entries: well under
/// a frame, so keys and the wheel are not held up behind it.
const OLDER_BUDGET: Duration = Duration::from_millis(6);

/// The follow key prime-agent's hint names (`tui.viewport.follow`).
pub const FOLLOW_KEY: &str = "ctrl+end";

/// Whether this terminal renders fullscreen, and with the mouse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Prefs {
    pub enabled: bool,
    pub mouse: bool,
    /// herdr's sidebar: `terminal.sidebar`, on by default.
    pub sidebar: bool,
}

/// prime-agent's `getFullscreen` and `getFullscreenMouse`: `HA_FULLSCREEN`
/// (prime-agent's `PI_FULLSCREEN`: on only for `1`) wins, else
/// `terminal.fullscreen` in `settings.json`, on by default; the mouse is
/// `terminal.fullscreenMouse`, on by default; the sidebar is `terminal.sidebar`,
/// on by default.
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
        sidebar: setting("sidebar").unwrap_or(true),
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
    /// The last frame, for a frame selection. Kept as the buffer and turned
    /// into text only when a selection begins: a string per cell on every
    /// frame was a large part of a frame's cost while agents streamed.
    frame: Buffer,
    /// The frame a frame selection copies from: the one it began on.
    frame_snapshot: Option<Vec<Vec<String>>>,
    /// The first frame row a frame selection may start on (the dock's top),
    /// or every row while a panel is open.
    selectable_from: u16,
    /// The first column of the conversation: the sidebar's width beside it.
    left: u16,
    /// The column past the conversation: a delegate's pane starts there.
    right: u16,
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
            frame: Buffer::empty(Rect::default()),
            frame_snapshot: None,
            selectable_from: 0,
            left: 0,
            right: 0,
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

    /// The first conversation row the window shows.
    #[must_use]
    pub const fn top(&self) -> usize {
        if self.following {
            self.max_scroll
        } else {
            self.scroll_top
        }
    }

    /// Scroll so conversation row `row` is in the window, a third of the way
    /// down it; a row already in view does not move the window.
    pub fn reveal(&mut self, row: usize) {
        let height = usize::from(self.window_height.max(1));
        let top = self.top();
        if row >= top && row < top + height {
            return;
        }
        self.scroll_top = row.saturating_sub(height / 3).min(self.max_scroll);
        self.following = self.scroll_top >= self.max_scroll;
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
        if col < self.left {
            self.clear_selection();
            return false;
        }
        let point = Point {
            line,
            col: usize::from(col - self.left),
        };
        self.anchor = Some(point);
        self.head = Some(point);
        self.mode = Some(Mode::Transcript);
        true
    }

    /// Begin a selection over the frame (the dock, or an open panel).
    pub fn begin_frame_selection(&mut self, row: u16, col: u16) -> bool {
        if row < self.selectable_from || row >= self.frame.area.height {
            self.clear_selection();
            return false;
        }
        let point = Point {
            line: usize::from(row),
            col: usize::from(col),
        };
        self.frame_snapshot = Some(frame_cells(&self.frame));
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
        let col = if self.mode == Some(Mode::Transcript) {
            col.saturating_sub(self.left)
        } else {
            col
        };
        let line = match self.mode {
            Some(Mode::Transcript) => self.transcript_line(row, true),
            Some(Mode::Frame) => {
                let rows = self
                    .frame_snapshot
                    .as_ref()
                    .map_or(usize::from(self.frame.area.height), Vec::len);
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

    /// The first `count` conversation rows were dropped: what is on screen keeps
    /// its place instead of sliding up under a reader who scrolled back.
    /// Put the window's first row at `top`, not following the end.
    fn place(&mut self, top: usize) {
        self.scroll_top = top;
        self.following = false;
    }

    /// Rows were drawn above the ones there were (older entries, drawn after
    /// the newest): the window and a selection stay on the rows they were on.
    fn rows_added(&mut self, count: usize) {
        if !self.following {
            self.scroll_top += count;
        }
        self.max_scroll += count;
        if self.mode == Some(Mode::Transcript) {
            for point in [&mut self.anchor, &mut self.head].into_iter().flatten() {
                point.line += count;
            }
        }
    }

    fn rows_removed(&mut self, count: usize) {
        if !self.following {
            self.scroll_top = self.scroll_top.saturating_sub(count);
        }
        self.max_scroll = self.max_scroll.saturating_sub(count);
        if self.mode == Some(Mode::Transcript) {
            match (self.anchor, self.head) {
                (Some(anchor), Some(head)) if anchor.line >= count && head.line >= count => {
                    self.anchor = Some(Point {
                        line: anchor.line - count,
                        ..anchor
                    });
                    self.head = Some(Point {
                        line: head.line - count,
                        ..head
                    });
                }
                _ => self.clear_selection(),
            }
        }
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
            .unwrap_or_else(|| frame_cells(&self.frame));
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
        // A transcript selection counts columns from the conversation's edge,
        // and stops at a delegate's pane.
        let (left, limit) = if self.mode == Some(Mode::Transcript) {
            let limit = if self.right > 0 {
                self.right.min(width)
            } else {
                width
            };
            (self.left.min(limit), limit)
        } else {
            (0, width)
        };
        let mut push = |row: u16, (from, to): (usize, usize)| {
            let from = u16::try_from(from).unwrap_or(u16::MAX).min(limit - left) + left;
            let to = u16::try_from(to).unwrap_or(u16::MAX).min(limit - left) + left;
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
    /// A sidebar row asked for another agent: switch the terminal to it.
    pub switch_to: Option<String>,
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
    /// How many of `rows` each entry of `shown` drew, while `rows_key` holds:
    /// the oldest entry is dropped by removing its rows, not by drawing every
    /// entry again.
    row_counts: VecDeque<usize>,
    rows_key: Option<(u16, Detail)>,
    /// The first entry of `shown` that `rows` holds: after a change of width
    /// or detail mode the newest entries are drawn first, the older ones a
    /// few at a time between frames.
    built_from: usize,
    /// The live block's tallest height in the current run (never more than
    /// its cap). It grows and shrinks with every tool card and output line;
    /// holding its room keeps the conversation from jumping each frame.
    dock_floor: u16,
    assistant_continuing: bool,
    viewport: Viewport,
    /// A drag held at an edge: its direction, cell and last scroll.
    auto_scroll: Option<(isize, u16, u16, Instant)>,
    /// How many rows the oldest entries took that were dropped: a row's
    /// place counted from the conversation's start, across drops.
    dropped_rows: usize,
    /// The screen, the window and the window's first row (counted with
    /// `dropped_rows`) of the last frame, while the screen still shows it.
    last_window: Option<(Rect, Rect, usize)>,
    /// How many frames moved the window's rows with the terminal.
    scrolled_frames: u64,
    /// herdr's sidebar, shown when the console is wide enough for it.
    sidebar: bool,
    /// The project's other agents, read on a thread of their own.
    roster: Option<super::roster::Poller>,
    /// The other agents' spinner clock, and the frame last drawn from it.
    spin_clock: std::time::Instant,
    spun: u64,
    /// The delegate whose pane is open beside the conversation.
    focus: Option<String>,
    /// The sidebar's agent rows as last drawn, for a click.
    hits: Vec<(u16, super::sidebar::Target)>,
    /// Ctrl+F's find bar, while it is open.
    find: Option<super::find::Find>,
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
            row_counts: VecDeque::new(),
            rows_key: None,
            built_from: 0,
            dock_floor: 0,
            assistant_continuing,
            viewport: Viewport::default(),
            auto_scroll: None,
            dropped_rows: 0,
            last_window: None,
            scrolled_frames: 0,
            sidebar: false,
            roster: None,
            spin_clock: std::time::Instant::now(),
            spun: 0,
            focus: None,
            hits: Vec::new(),
            find: None,
        })
    }

    /// Show herdr's sidebar beside the conversation on a wide enough console.
    #[must_use]
    pub fn with_sidebar(mut self, on: bool) -> Self {
        self.sidebar = on;
        self
    }

    /// List the project's other agents in the sidebar (a terminal attached to
    /// a background agent knows which project and which agent it is).
    #[must_use]
    pub fn with_roster(mut self, scope: Option<super::roster::Scope>) -> Self {
        self.roster = scope.map(super::roster::Poller::start);
        self
    }

    /// List these agents as the project's others: the preview frames.
    #[cfg(test)]
    #[must_use]
    pub fn with_roster_entries(mut self, entries: Vec<super::roster::Entry>) -> Self {
        self.roster = Some(super::roster::Poller::fixed(entries));
        self
    }

    /// The sidebar's columns on a console `columns` wide.
    fn sidebar_columns(&self, columns: u16) -> u16 {
        super::sidebar::width(columns, self.sidebar)
    }

    /// A delegate's pane's columns beside a conversation `content` wide.
    fn pane_columns(&self, content: u16) -> u16 {
        if self.focus.is_some() {
            super::pane::width(content)
        } else {
            0
        }
    }

    /// The columns the conversation's rows are drawn at: the conversation's,
    /// less a delegate's pane.
    fn rows_width(&self) -> u16 {
        let content = self.columns();
        content - self.pane_columns(content)
    }

    /// What a sidebar row opens: the main conversation, a delegate's pane
    /// (again: closed), or another agent. Returns whether the frame changed
    /// and the agent to switch to.
    fn open_target(&mut self, target: super::sidebar::Target) -> (bool, Option<String>) {
        match target {
            super::sidebar::Target::Main => (self.focus.take().is_some(), None),
            super::sidebar::Target::Child(name) => {
                if self.focus.as_deref() == Some(name.as_str()) {
                    self.focus = None;
                } else {
                    self.focus = Some(name);
                }
                (true, None)
            }
            super::sidebar::Target::Agent(id) => (false, Some(id)),
        }
    }

    /// Alt+`number`: the sidebar's row of that number.
    pub fn agent_key(&mut self, number: u8, state: &UiState) -> (bool, Option<String>) {
        let others = self
            .roster
            .as_ref()
            .map_or(&[][..], super::roster::Poller::entries);
        let target = super::sidebar::targets(state, others)
            .into_iter()
            .nth(usize::from(number).saturating_sub(1));
        match target {
            Some(target) if number > 0 => self.open_target(target),
            _ => (false, None),
        }
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

    /// The conversation's columns: the console's, less the sidebar's.
    pub fn columns(&self) -> u16 {
        self.terminal
            .size()
            .map_or(0, |size| size.width - self.sidebar_columns(size.width))
    }

    pub fn rows_count(&self) -> u16 {
        self.terminal.size().map_or(0, |size| size.height)
    }

    /// Draw the entries again at this width and detail mode, when either
    /// changed: the newest first, as many as fill a few windows, and the
    /// older ones a few at a time between frames ([`Self::draw_older`]).
    /// Drawing all of a long run's entries (4000, with their markdown and
    /// highlighted code) in the frame that opened a subagent's pane was the
    /// lag of picking one, and of Ctrl+O and of attaching. A window scrolled
    /// back keeps the entry it showed at its top: the entries from there
    /// down are drawn now. The find bar counts rows from the start, so with
    /// it open every entry is.
    fn ensure_rows(&mut self, width: u16) {
        if self.rows_key == Some((width, self.detail)) {
            return;
        }
        // The old rows still say where the window is, while they match the
        // entries (a reprint keeps them; a restore does not).
        let anchor = if self.viewport.is_following()
            || self.row_counts.len() != self.shown.len().saturating_sub(self.built_from)
        {
            None
        } else {
            self.top_entry()
        };
        self.rows = Vec::new();
        self.row_counts = VecDeque::new();
        self.built_from = self.shown.len();
        self.rows_key = Some((width, self.detail));
        // Rows were re-wrapped: a selection anchored to the old ones is gone.
        self.viewport.clear_selection();
        let height = self
            .terminal
            .size()
            .map_or(50, |size| usize::from(size.height));
        if self.find.is_some() {
            self.draw_older(usize::MAX, None, 0);
        } else if let Some((entry, offset)) = anchor {
            self.draw_older(usize::MAX, None, entry);
            let first = self.row_counts.front().copied().unwrap_or(0);
            self.viewport.place(offset.min(first.saturating_sub(1)));
            self.draw_older(height * FIRST_WINDOWS, None, 0);
        } else {
            self.draw_older(height * FIRST_WINDOWS, None, 0);
        }
    }

    /// The entry the window's first row belongs to, and that row's place in
    /// the entry's rows.
    fn top_entry(&self) -> Option<(usize, usize)> {
        let top = self.viewport.top();
        let mut start = 0;
        for (index, count) in self.row_counts.iter().enumerate() {
            if top < start + count {
                return Some((self.built_from + index, top - start));
            }
            start += count;
        }
        None
    }

    /// Draw entries older than the ones `rows` holds, newest first, until
    /// `rows_wanted` more rows are drawn, `budget` is spent, or the entry
    /// `floor` is drawn; they go above the rows there were. How many rows
    /// were added.
    fn draw_older(&mut self, rows_wanted: usize, budget: Option<Duration>, floor: usize) -> usize {
        let Some((width, detail)) = self.rows_key else {
            return 0;
        };
        let begun = Instant::now();
        let mut fragments = Vec::new();
        let mut added = 0;
        while self.built_from > floor
            && added < rows_wanted
            && budget.is_none_or(|budget| begun.elapsed() < budget)
        {
            let index = self.built_from - 1;
            let continuing =
                index > 0 && matches!(self.shown[index - 1], HistoryItem::Assistant { .. });
            let drawn = history::render_fragment(
                &self.shown[index],
                width,
                &self.theme,
                detail,
                continuing,
            );
            added += drawn.len();
            fragments.push(drawn);
            self.built_from = index;
        }
        if fragments.is_empty() {
            return 0;
        }
        for fragment in &fragments {
            self.row_counts.push_front(fragment.len());
        }
        let older: Vec<Line<'static>> = fragments.into_iter().rev().flatten().collect();
        self.rows.splice(0..0, older);
        self.viewport.rows_added(added);
        // The last frame's top, counted from the start, moved with its rows:
        // the window did not.
        if let Some((_, _, top)) = &mut self.last_window {
            *top += added;
        }
        added
    }

    /// Every entry drawn: for a search over the whole conversation, or the
    /// window's first row.
    fn draw_all(&mut self) {
        if self.rows_key.is_some() {
            self.draw_older(usize::MAX, None, 0);
        }
    }

    pub fn insert_history_batch(&mut self, items: &[HistoryItem]) {
        let width = self.rows_width();
        let drawn = self.rows_key == Some((width, self.detail));
        for item in items {
            if self.shown.len() == REPRINT_ENTRIES {
                self.drop_oldest(drawn);
            }
            self.shown.push_back(item.clone());
            self.since_entered = (self.since_entered + 1).min(self.shown.len());
            if drawn {
                let fragment = history::render_fragment(
                    item,
                    width,
                    &self.theme,
                    self.detail,
                    self.assistant_continuing,
                );
                self.row_counts.push_back(fragment.len());
                self.rows.extend(fragment);
            }
            self.assistant_continuing = matches!(item, HistoryItem::Assistant { .. });
        }
    }

    /// Drop the oldest entry. A long run with several agents reaches the cap,
    /// and then every new row dropped one: drawing all 4000 entries again for
    /// each was the lag, and the shifted rows moved the view under the reader.
    /// Its rows are removed instead, the new first entry - drawn as a
    /// continuation of the one dropped - is drawn again on its own, and the
    /// window and any selection move with the rows.
    fn drop_oldest(&mut self, drawn: bool) {
        self.shown.pop_front();
        if drawn && self.built_from > 0 {
            // Not drawn yet, so there are no rows to remove.
            self.built_from -= 1;
            return;
        }
        if !drawn || self.row_counts.len() != self.shown.len() + 1 {
            self.rows_key = None;
            self.row_counts.clear();
            return;
        }
        let dropped = self
            .row_counts
            .pop_front()
            .unwrap_or(0)
            .min(self.rows.len());
        self.rows.drain(..dropped);
        // Rows before the second kept entry: `dropped + old` then, `new` now.
        let mut shift = dropped;
        if let (Some(first), Some(count)) = (self.shown.front(), self.row_counts.front_mut()) {
            let width = self.rows_key.map_or(0, |(width, _)| width);
            let fragment = history::render_fragment(first, width, &self.theme, self.detail, false);
            let old = (*count).min(self.rows.len());
            let new = fragment.len();
            self.rows.splice(..old, fragment);
            *count = new;
            shift = (dropped + old).saturating_sub(new);
        }
        self.dropped_rows += shift;
        self.viewport.rows_removed(shift);
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
        self.row_counts.clear();
        self.viewport.scroll_to_bottom();
    }

    /// Force the next frame to paint every cell.
    ///
    /// # Errors
    /// The terminal cannot be cleared.
    pub fn clear(&mut self) -> io::Result<()> {
        self.last_window = None;
        self.terminal.clear().map_err(to_io)?;
        // Written now: the backend gathers a frame until it is flushed, and
        // what follows a clear may go to the console another way.
        self.terminal.backend_mut().flush().map_err(to_io)
    }

    /// Draw one frame: the top bar, the window over the conversation, the dock.
    ///
    /// # Errors
    /// The terminal cannot be drawn to.
    #[allow(
        clippy::too_many_lines,
        reason = "one frame, top to bottom: the bar, the window, the dock, the hints"
    )]
    pub fn draw(&mut self, state: &UiState) -> io::Result<()> {
        self.detail = state.detail;
        let size = self.terminal.size().map_err(to_io)?;
        // A pane whose delegate is gone (a new turn began) closes.
        if let Some(name) = &self.focus
            && !state.child_activity.iter().any(|child| &child.name == name)
        {
            self.focus = None;
        }
        let left = self.sidebar_columns(size.width);
        let pane = self.pane_columns(size.width - left);
        self.ensure_rows(size.width - left - pane);
        // Scrolled back to near the first row drawn: draw the entries above
        // it now, so the wheel never stops at a top that is not the start.
        let height = usize::from(size.height);
        if self.built_from > 0 && !self.viewport.is_following() && self.viewport.top() < height * 2
        {
            self.draw_older(height * FIRST_WINDOWS, None, 0);
        }
        self.viewport.left = left;
        self.viewport.right = size.width - pane;
        let others = self
            .roster
            .as_ref()
            .map_or(&[][..], super::roster::Poller::entries);
        let focus = self.focus.as_deref();
        let spin = self.spin_frame();
        let mut hits = Vec::new();
        let theme = self.theme;
        let dropped = self.dropped_rows;
        let rows = &self.rows;
        let viewport = &mut self.viewport;
        let find = &mut self.find;
        let running = state.phase.has_active_run();
        if !running {
            self.dock_floor = 0;
        }
        let dock_floor = &mut self.dock_floor;
        let dock_top;
        let (drawn_window, drawn_top);
        let mut cursor = None;
        // Drawn by hand rather than with `Terminal::draw`, so that rows the
        // window only moved are moved by the terminal (`scroll_window`)
        // before the frame is written.
        self.terminal.autoresize().map_err(to_io)?;
        let area = {
            let mut frame = self.terminal.get_frame();
            let frame = &mut frame;
            {
                let screen = frame.area();
                // herdr's sidebar takes the left columns; everything else is
                // laid out in the rest, as it would be without it.
                let area = Rect::new(
                    screen.x + left,
                    screen.y,
                    screen.width - left,
                    screen.height,
                );
                if left > 0 {
                    hits = super::sidebar::render(
                        frame,
                        Rect::new(screen.x, screen.y, left, screen.height),
                        state,
                        super::sidebar::Extra {
                            others,
                            focus,
                            spin,
                        },
                        &theme,
                    );
                }
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
                let mut plan = layout::plan(dock_area, state, &theme);
                dock_top = [plan.live, plan.modal, plan.queue, plan.suggest]
                    .into_iter()
                    .flatten()
                    .chain([plan.composer])
                    .filter(|rect| rect.height > 0)
                    .map(|rect| rect.y)
                    .min()
                    .unwrap_or(dock_area.y)
                    .max(area.y + header);
                // While a run lasts the room above the composer is held at the
                // tallest the live block has been, so cards and output lines
                // coming and going there do not move the conversation up and
                // down. Only the live block is held, at most its own cap: a
                // panel (approval, picker) takes its room while it is open and
                // gives it back, instead of shrinking the window for the rest
                // of the run.
                let rest_top = [plan.modal, plan.queue, plan.suggest]
                    .into_iter()
                    .flatten()
                    .chain([plan.composer])
                    .filter(|rect| rect.height > 0)
                    .map(|rect| rect.y)
                    .min()
                    .unwrap_or(dock_area.y);
                if running && plan.modal.is_none() {
                    // The live block and the spacing that comes with it.
                    *dock_floor = (*dock_floor).max(rest_top.saturating_sub(dock_top));
                }
                let held = if plan.modal.is_some() {
                    dock_top
                } else {
                    rest_top.saturating_sub(*dock_floor)
                };
                let window_bottom = dock_top.min(held).max(area.y + header);
                // One blank row between the conversation and the dock, so the
                // newest row never sits on the composer's border.
                let window_rows = window_bottom - (area.y + header);
                let window = Rect::new(
                    area.x,
                    area.y + header,
                    area.width - pane,
                    if window_rows > 1 {
                        window_rows - 1
                    } else {
                        window_rows
                    },
                );
                // herdr's split: the delegate's pane beside the conversation,
                // as tall as the window and the row below it.
                if pane > 0
                    && let Some(name) = focus
                    && let Some((index, (child, status))) = super::sidebar::ordered_children(state)
                        .into_iter()
                        .enumerate()
                        .find(|(_, (child, _))| child.name == name)
                {
                    super::pane::render(
                        frame,
                        Rect::new(window.x + window.width, window.y, pane, window_rows),
                        super::pane::View {
                            activity: child,
                            status,
                            number: index + 2,
                            detail: state.detail,
                            tick: state.tick,
                        },
                        &theme,
                    );
                }
                let top = viewport.compose(rows.len(), window.y, window.height);
                (drawn_window, drawn_top) = (window, top);
                let visible = rows
                    .iter()
                    .skip(top)
                    .take(usize::from(window.height))
                    .cloned()
                    .collect::<Vec<_>>();
                frame.render_widget(Paragraph::new(visible), window);
                // Ctrl+F: every match in the window marked, the current one
                // apart, and the bar over the window's top right.
                if let Some(find) = find.as_mut() {
                    find.refresh(rows, dropped, top);
                    let buffer = frame.buffer_mut();
                    for (rect, current) in super::find::marks(find, top, window) {
                        buffer.set_style(rect, super::find::style(&theme, current));
                    }
                    let width = window.width.min(56);
                    if width >= 20 && window.height > 0 {
                        frame.render_widget(
                            Paragraph::new(super::find::bar(find, usize::from(width), &theme)),
                            Rect::new(window.x + window.width - width, window.y, width, 1),
                        );
                    }
                }
                // The held room is the live block's tallest: a shorter block sat
                // at its foot, by the composer, and left blank rows between it
                // and the conversation. It starts below the spacer row instead.
                if plan.modal.is_none()
                    && let Some(live) = plan.live.as_mut()
                {
                    live.y = live.y.min(window_bottom);
                }
                if header > 0 {
                    frame.render_widget(
                        Paragraph::new(top_bar(state, area.width, &theme)),
                        Rect::new(area.x, area.y, area.width, 1),
                    );
                }
                widgets::render(frame, &plan, state, &theme);
                // prime-agent's toasts, over the window's top rows.
                super::toast::render(frame, window, &theme);
                if let Some((x, y)) = plan.cursor {
                    frame.set_cursor_position((x, y));
                    cursor = Some(Position::new(x, y));
                }
                // prime-agent's follow hint, on the spacer row below the
                // window: drawn over the window's last row, it hid that row.
                if !viewport.is_following() && window.height > 0 {
                    let hint_y = if window_rows > 1 {
                        window.y + window.height
                    } else {
                        window.y + window.height - 1
                    };
                    let label = format!(" {FOLLOW_KEY} to follow ");
                    let width = u16::try_from(label.width()).unwrap_or(u16::MAX);
                    if width <= area.width {
                        let hint = Rect::new(area.x + (area.width - width) / 2, hint_y, width, 1);
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
                for rect in viewport.highlighted(screen.width) {
                    buffer.set_style(rect, Style::new().add_modifier(Modifier::REVERSED));
                }
            }
            frame.area()
        };
        // The cursor is hidden while the frame is written and shown at its
        // place after: left on, it ran across every row the frame wrote.
        self.terminal.hide_cursor().map_err(to_io)?;
        self.scroll_window(area, drawn_window, drawn_top)?;
        let completed = self
            .terminal
            .apply_buffer_with_cursor(cursor)
            .map_err(to_io)?;
        self.viewport.frame = completed.buffer.clone();
        self.hits = hits;
        self.viewport.selectable_from = if state.modal.is_some() { 0 } else { dock_top };
        Ok(())
    }

    /// Move the rows the window only moved with the terminal. A wheel notch
    /// changed every row of the window, and writing them all through the
    /// Windows console was what lagged behind the wheel on a long
    /// conversation. The terminal scrolls the window's region, ratatui is
    /// told the screen now shows the last frame shifted, and only the rows
    /// that came into view - and whatever else changed - are written.
    fn scroll_window(&mut self, area: Rect, window: Rect, top: usize) -> io::Result<()> {
        let top = top + self.dropped_rows;
        let Some((last_area, last_window, last_top)) =
            self.last_window.replace((area, window, top))
        else {
            return Ok(());
        };
        let up = top > last_top;
        let amount = top.abs_diff(last_top);
        if last_area != area
            || last_window != window
            || self.viewport.frame.area != area
            || amount == 0
            || amount >= usize::from(window.height)
        {
            return Ok(());
        }
        let shown = shifted(&self.viewport.frame, window, up, amount);
        let drawn = std::mem::take(self.terminal.current_buffer_mut());
        *self.terminal.current_buffer_mut() = shown;
        self.terminal.swap_buffers();
        *self.terminal.current_buffer_mut() = drawn;
        let region = window.y..window.y + window.height;
        let amount = u16::try_from(amount).unwrap_or(u16::MAX);
        let backend = self.terminal.backend_mut();
        if up {
            backend.scroll_region_up(region, amount)
        } else {
            backend.scroll_region_down(region, amount)
        }
        .map_err(to_io)?;
        self.scrolled_frames += 1;
        Ok(())
    }

    /// prime-agent's `handleFullscreenInput` for a mouse report: the wheel
    /// scrolls the conversation, a drag selects and the release copies. While a
    /// panel has the focus the wheel is left alone and a drag selects the frame.
    pub fn mouse(&mut self, input: MouseInput, panel_open: bool) -> MouseOutcome {
        let (row, col) = (input.row, input.column);
        let mut outcome = MouseOutcome::default();
        // A click on the sidebar opens the agent row under it.
        if col < self.viewport.left {
            if input.kind == MouseKind::Press
                && let Some((_, target)) = self.hits.iter().find(|(at, _)| *at == row)
            {
                (outcome.redraw, outcome.switch_to) = self.open_target(target.clone());
            }
            return outcome;
        }
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

    /// A drag held at an edge scrolls on its own, and the roster may have
    /// changed; true when the frame should be drawn again.
    pub fn tick(&mut self) -> bool {
        let roster = self
            .roster
            .as_mut()
            .is_some_and(super::roster::Poller::poll);
        let scrolled = self.scroll_tick();
        // The older entries, a few at a time. Only a window scrolled back
        // sees them arrive (its rows stay put, its place in the whole moves).
        let older = self.built_from > 0
            && self.draw_older(usize::MAX, Some(OLDER_BUDGET), 0) > 0
            && !self.viewport.is_following();
        // An agent elsewhere that works spins on the screen's own clock.
        let spinning = self.roster.as_ref().is_some_and(|roster| {
            roster
                .entries()
                .iter()
                .any(|entry| entry.status == super::sidebar::Status::Working)
        }) && {
            let frame = self.spin_frame();
            std::mem::replace(&mut self.spun, frame) != frame
        };
        roster || scrolled || older || spinning
    }

    /// The other agents' spinner frame: one every 100 ms, the session's own
    /// spinner's pace.
    fn spin_frame(&self) -> u64 {
        u64::try_from(self.spin_clock.elapsed().as_millis() / 100).unwrap_or(u64::MAX)
    }

    fn scroll_tick(&mut self) -> bool {
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
        self.draw_all();
        self.viewport.scroll_to_top();
    }

    pub fn follow(&mut self) {
        self.viewport.scroll_to_bottom();
    }

    /// Ctrl+F: open the find bar (it stays open, with its query, when it is).
    pub fn open_find(&mut self) {
        self.draw_all();
        if self.find.is_none() {
            self.find = Some(super::find::Find::default());
        }
    }

    /// A key while the find bar is open: typing searches, Enter or Down goes
    /// to the next match and Shift+Enter or Up to the one before, Esc closes
    /// the bar where the window is. `None` when the bar is closed or the key
    /// is not the bar's (the wheel, the pages, the sidebar's keys); else
    /// whether the frame changed.
    pub fn find_key(&mut self, key: &Key) -> Option<bool> {
        let from = self.viewport.top();
        let find = self.find.as_mut()?;
        match key {
            Key::Char(character) => {
                let mut query = find.query.clone();
                query.push(*character);
                find.set_query(query, &self.rows, self.dropped_rows, from);
            }
            Key::Backspace => {
                let mut query = find.query.clone();
                if query.pop().is_none() {
                    return Some(false);
                }
                find.set_query(query, &self.rows, self.dropped_rows, from);
            }
            Key::Enter | Key::Down => find.step(true),
            Key::Newline | Key::Up => find.step(false),
            Key::Esc => {
                self.find = None;
                return Some(true);
            }
            Key::Mouse(_)
            | Key::Agent(_)
            | Key::Resize { .. }
            | Key::PageUp
            | Key::PageDown
            | Key::ViewportTop
            | Key::ViewportFollow
            | Key::Interrupt => return None,
            _ => return Some(false),
        }
        if let Some(found) = self
            .find
            .as_ref()
            .and_then(super::find::Find::current_match)
        {
            self.viewport.reveal(found.row);
        }
        Some(true)
    }
}

/// `frame` as the screen shows it once the terminal scrolled `window` by
/// `amount` rows (`up`: its rows moved up): the rows that left are gone and
/// the rows that came in are blank.
fn shifted(frame: &Buffer, window: Rect, up: bool, amount: usize) -> Buffer {
    let mut shown = frame.clone();
    let width = usize::from(frame.area.width);
    let start = usize::from(window.y.saturating_sub(frame.area.y)) * width;
    let end = (start + usize::from(window.height) * width).min(shown.content.len());
    let region = &mut shown.content[start.min(end)..end];
    let cells = (amount * width).min(region.len());
    if up {
        region.rotate_left(cells);
        let kept = region.len() - cells;
        region[kept..].fill_with(Cell::default);
    } else {
        region.rotate_right(cells);
        region[..cells].fill_with(Cell::default);
    }
    shown
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

    /// herdr's split: Alt+2 opens the first delegate's pane beside the
    /// conversation, which is drawn narrower; Alt+1 closes it, a click on the
    /// delegate's sidebar row opens it again, and a turn without that
    /// delegate closes it.
    #[test]
    fn a_delegates_pane_opens_from_its_sidebar_row() {
        use crate::interactive::events::{ChildActivity, ChildLine, ChildLineKind};
        let mut screen = screen(140, 30).with_sidebar(true);
        let mut state = crate::interactive::tui::tests::idle_state();
        state.child_activity = vec![ChildActivity {
            name: "explorer".to_owned(),
            tool_calls: 1,
            latest: "read_file: path=a.rs".to_owned(),
            recent: vec![ChildLine {
                kind: ChildLineKind::Call,
                text: "read_file: path=pane-marker.rs".to_owned(),
                call: "c1".to_owned(),
            }],
            ..ChildActivity::default()
        }];
        screen.insert_history_batch(&[HistoryItem::User {
            text: "x".repeat(200),
        }]);
        screen.draw(&state).expect("drawn");
        let wide = screen.rows_width();
        assert!(!painted(&screen).join("\n").contains("pane-marker"));

        assert_eq!(screen.agent_key(2, &state), (true, None));
        screen.draw(&state).expect("drawn");
        let text = painted(&screen).join("\n");
        assert!(text.contains("pane-marker"), "{text}");
        assert!(screen.rows_width() < wide, "the conversation makes room");

        assert_eq!(screen.agent_key(1, &state), (true, None));
        screen.draw(&state).expect("drawn");
        assert!(!painted(&screen).join("\n").contains("pane-marker"));
        assert_eq!(screen.agent_key(9, &state), (false, None), "no such row");

        // A click on the delegate's row in the sidebar.
        let row = painted(&screen)
            .iter()
            .position(|row| row.contains("explorer"))
            .expect("its row");
        let outcome = screen.mouse(
            MouseInput {
                kind: MouseKind::Press,
                column: 3,
                row: u16::try_from(row).expect("row"),
                modified: false,
            },
            false,
        );
        assert!(outcome.redraw);
        assert_eq!(outcome.switch_to, None);
        screen.draw(&state).expect("drawn");
        assert!(painted(&screen).join("\n").contains("pane-marker"));

        state.child_activity.clear();
        screen.draw(&state).expect("drawn");
        assert_eq!(
            screen.rows_width(),
            wide,
            "the pane closed with its delegate"
        );
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

    /// Ctrl+F: typing marks the matches and counts them, the current one
    /// reversed and scrolled into view; Enter steps to the next (wrapping)
    /// and Shift+Enter back; Esc closes the bar and leaves the window there.
    #[test]
    fn find_marks_counts_and_scrolls_to_the_current_match() {
        use crate::interactive::events::Key;
        let mut screen = screen(80, 20);
        let state = crate::interactive::tui::tests::idle_state();
        let items = (0..60)
            .map(|index| HistoryItem::Notice {
                message: if index % 20 == 5 {
                    format!("row {index} needle here")
                } else {
                    format!("row {index}")
                },
            })
            .collect::<Vec<_>>();
        screen.restore(&items);
        screen.draw(&state).expect("drawn");
        assert_eq!(
            screen.find_key(&Key::Char('n')),
            None,
            "closed, the bar reads no key"
        );
        screen.open_find();
        for character in "NEEDLE".chars() {
            assert_eq!(screen.find_key(&Key::Char(character)), Some(true));
        }
        // The current match's row, by its index: the matches are rows 5, 25, 45.
        let shown = |screen: &FullscreenScreen<ratatui::backend::TestBackend>| {
            let current = screen.find.as_ref().expect("open").current;
            (
                format!("row {} needle", 5 + 20 * current),
                format!("{}/3", current + 1),
            )
        };
        let reversed_needle = |screen: &FullscreenScreen<ratatui::backend::TestBackend>| {
            let buffer = screen.backend().buffer();
            (0..buffer.area.height).any(|y| {
                (0..buffer.area.width)
                    .filter_map(|x| buffer.cell((x, y)))
                    .filter(|cell| cell.modifier.contains(ratatui::style::Modifier::REVERSED))
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
                    .contains("needle")
            })
        };
        screen.draw(&state).expect("drawn");
        let text = painted(&screen).join("\n");
        let (row, count) = shown(&screen);
        assert!(text.contains("⌕ NEEDLE"), "{text}");
        assert!(text.contains(&count), "{count}: {text}");
        assert!(text.contains(&row), "the current match is in view: {text}");
        assert!(
            reversed_needle(&screen),
            "the current match is marked: {text}"
        );

        for key in [Key::Enter, Key::Enter, Key::Newline] {
            assert_eq!(screen.find_key(&key), Some(true));
            screen.draw(&state).expect("drawn");
            let text = painted(&screen).join("\n");
            let (row, count) = shown(&screen);
            assert!(
                text.contains(&row) && text.contains(&count),
                "{row} {count}: {text}"
            );
        }

        let (row, _) = shown(&screen);
        assert_eq!(screen.find_key(&Key::Esc), Some(true));
        screen.draw(&state).expect("drawn");
        let text = painted(&screen).join("\n");
        assert!(!text.contains('⌕'), "the bar closed: {text}");
        assert!(
            text.contains(&row),
            "the window stayed where it was: {text}"
        );
        assert!(!reversed_needle(&screen), "no match is marked: {text}");
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

    /// A wheel notch moves the window's rows with the terminal and writes only
    /// the rows that came in; whatever happens between frames - the wheel
    /// either way, a page, rows arriving while following or scrolled back,
    /// the oldest entries dropped at the cap - the screen shows exactly the
    /// frame that was drawn.
    #[test]
    fn rows_the_window_only_moved_are_moved_by_the_terminal() {
        let mut screen = screen(70, 24);
        let mut state = crate::interactive::tui::tests::idle_state();
        let items = (0..super::REPRINT_ENTRIES)
            .map(|index| {
                if index % 7 == 0 {
                    HistoryItem::Assistant {
                        text: format!("**Step {index}**\n\n- one\n- two `code`\n"),
                    }
                } else {
                    HistoryItem::Notice {
                        message: format!("row {index} {}", "x".repeat(index % 50)),
                    }
                }
            })
            .collect::<Vec<_>>();
        screen.restore(&items);
        screen.draw(&state).expect("drawn");
        let wheel = |kind| MouseInput {
            kind,
            column: 5,
            row: 5,
            modified: false,
        };
        let check = |screen: &FullscreenScreen<ratatui::backend::TestBackend>, step: &str| {
            assert!(
                screen.backend().buffer() == &screen.viewport.frame,
                "after {step} the screen is not the frame drawn:\n{}",
                painted(screen).join("\n")
            );
        };
        for step in 0..12 {
            let _ = screen.mouse(wheel(MouseKind::WheelUp), false);
            screen.draw(&state).expect("drawn");
            check(&screen, &format!("wheel up {step}"));
        }
        assert!(
            screen.scrolled_frames >= 12,
            "each notch moved the rows: {}",
            screen.scrolled_frames
        );
        let rows = painted(&screen);
        assert!(
            rows.iter().any(|row| row.contains("ctrl+end to follow")),
            "{rows:?}"
        );
        // Rows arriving at the cap while the reader is scrolled back: the
        // oldest are dropped and the view holds still.
        for index in 0..5 {
            screen.insert_history_batch(&[HistoryItem::Notice {
                message: format!("late {index}"),
            }]);
            screen.draw(&state).expect("drawn");
            check(&screen, &format!("late row {index}"));
        }
        for step in 0..5 {
            let _ = screen.mouse(wheel(MouseKind::WheelDown), false);
            screen.draw(&state).expect("drawn");
            check(&screen, &format!("wheel down {step}"));
        }
        screen.page(false);
        screen.draw(&state).expect("drawn");
        check(&screen, "a page up");
        screen.follow();
        screen.draw(&state).expect("drawn");
        check(&screen, "following again");
        // Following, new rows move the window by as many rows.
        for index in 0..4 {
            screen.insert_history_batch(&[HistoryItem::Notice {
                message: format!("new {index}"),
            }]);
            state.tick += 1;
            screen.draw(&state).expect("drawn");
            check(&screen, &format!("new row {index}"));
        }
        assert!(painted(&screen).iter().any(|row| row.contains("new 3")));
        // A changed screen size is drawn whole.
        screen.terminal.backend_mut().resize(80, 26);
        screen.draw(&state).expect("drawn");
        check(&screen, "a resize");
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
        // Seen: the hint was drawn over the window's last row and hid it.
        let hint = rows
            .iter()
            .position(|row| row.contains("ctrl+end to follow"))
            .expect("hint row");
        assert!(!rows[hint].contains("line-"), "{rows:?}");
        assert!(rows[hint - 1].contains("line-"), "{rows:?}");
        screen.follow();
        screen.draw(&state).expect("drawn");
        assert!(painted(&screen).iter().any(|row| row.contains("line-39")));
        assert_eq!(state.phase, AppPhase::Ready);
    }

    /// A long conversation at the entry cap, as a multi-session task leaves it:
    /// how long the first draw and each wheel notch take.
    /// `cargo test -p harness-cli --release --bin ha -- --ignored scroll_cost --nocapture`
    #[test]
    #[ignore = "a measurement, not a check"]
    fn scroll_cost_of_a_long_conversation() {
        use crate::interactive::events::ToolState;
        use std::time::{Duration, Instant};
        let mut screen = screen(160, 45);
        let mut state = crate::interactive::tui::tests::idle_state();
        state.phase = AppPhase::Running;
        let mut items = Vec::new();
        for turn in 0..super::REPRINT_ENTRIES / 4 {
            items.push(HistoryItem::Assistant {
                text: format!(
                    "## Step {turn}\n\nSome **bold** prose with `code` and a list:\n\n- one\n- two\n\n```rust\nfn main() {{\n    let x = {turn};\n    println!(\"{{x}}\");\n}}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n"
                ),
            });
            items.push(HistoryItem::Tool {
                name: "read_file".to_owned(),
                summary: format!("paths=[\"src/file{turn}.rs\"]"),
                input: String::new(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(12),
                },
            });
            items.push(HistoryItem::ToolOutput {
                name: "read_file".to_owned(),
                text: (0..200)
                    .map(|line| format!("{line}: let value_{line} = compute({line}, \"text\");"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                path: Some(format!("src/file{turn}.rs")),
            });
            items.push(HistoryItem::Notice {
                message: "context: 1 older tool result(s) in this turn were shortened".to_owned(),
            });
        }
        let started = Instant::now();
        screen.restore(&items);
        screen.draw(&state).expect("drawn");
        eprintln!(
            "first draw of {} entries: {:?}",
            items.len(),
            started.elapsed()
        );
        let wheel = MouseInput {
            kind: MouseKind::WheelUp,
            column: 5,
            row: 5,
            modified: false,
        };
        let started = Instant::now();
        for _ in 0..100 {
            let _ = screen.mouse(wheel, false);
            screen.draw(&state).expect("drawn");
        }
        eprintln!(
            "one wheel notch and its frame: {:?}",
            started.elapsed() / 100
        );
        let started = Instant::now();
        for turn in 0..50 {
            screen.insert_history_batch(&[HistoryItem::Notice {
                message: format!("row {turn}"),
            }]);
            screen.draw(&state).expect("drawn");
        }
        eprintln!(
            "one new row at the cap and its frame: {:?}",
            started.elapsed() / 50
        );
    }

    /// Seen while an agent wrote: a reader who had scrolled back saw the view
    /// jump up and down. A run is replayed frame by frame - streamed text
    /// growing, tool cards opening and closing, rows arriving, the banner
    /// redrawn, the run pausing between autonomous turns - and the rows the
    /// reader is looking at must stay where they are.
    #[test]
    fn a_reader_who_scrolled_back_keeps_their_rows_while_the_agent_works() {
        let mut screen = screen(80, 30);
        let mut state = crate::interactive::tui::tests::idle_state();
        state.phase = AppPhase::Running;
        state.header.push("Chat: ninja".to_owned());
        let notice = |index: usize| HistoryItem::Notice {
            message: format!("row-{index}"),
        };
        // Near the cap: the run goes on dropping the oldest entries.
        let start = super::REPRINT_ENTRIES - 40;
        screen.insert_history_batch(&(0..start).map(notice).collect::<Vec<_>>());
        screen.draw(&state).expect("drawn");
        // A few rows back, as one wheel notch takes the reader: close enough
        // to the end that the window's height decides where it can stand.
        let _ = screen.mouse(
            MouseInput {
                kind: MouseKind::WheelUp,
                column: 5,
                row: 5,
                modified: false,
            },
            false,
        );
        screen.draw(&state).expect("drawn");
        let anchor = |rows: &[String]| {
            rows.iter()
                .enumerate()
                .find_map(|(y, row)| {
                    // The row's id only: a toast another test pushed (the
                    // queue is the process's) may be drawn beside it.
                    row.trim()
                        .strip_prefix("• row-")
                        .and_then(|rest| rest.split_whitespace().next())
                        .map(|id| (y, id.to_owned()))
                })
                .expect("a conversation row in view")
        };
        let first = anchor(&painted(&screen));
        let mut next = start;
        for frame in 0..120 {
            match frame % 8 {
                0 => {
                    state.live_text.push_str("streaming words of the answer\n");
                }
                1 => state
                    .open_tools
                    .push(("ipython".to_owned(), format!("call {frame}"))),
                2 => {
                    state.live_tool_output = (0..frame % 5).map(|n| format!("out {n}")).collect();
                }
                3 => {
                    state.open_tools.clear();
                    state.live_tool_output.clear();
                    screen.insert_history_batch(&[notice(next)]);
                    next += 1;
                }
                4 => {
                    // The text committed as it is flushed.
                    state.live_text.clear();
                    screen.insert_history_batch(&[HistoryItem::Assistant {
                        text: format!("answer part {frame}\nsecond line"),
                    }]);
                }
                // The banner redrawn with a row arriving in the same batch.
                5 => {
                    screen.retitle(&["ha".to_owned(), format!("model {frame}")]);
                    screen.insert_history_batch(&[notice(next)]);
                    next += 1;
                }
                6 => state.phase = AppPhase::Ready,
                _ => state.phase = AppPhase::Running,
            }
            screen.draw(&state).expect("drawn");
            let rows = painted(&screen);
            assert_eq!(
                anchor(&rows),
                first,
                "frame {frame}: the rows the reader looks at moved:\n{}",
                rows.join("\n")
            );
        }
    }

    /// A long run with several agents fills the conversation to its cap, and
    /// from then on every row drops the oldest one. That drew all entries again
    /// per row (the lag) and slid the rows under a reader who had scrolled back.
    #[test]
    fn dropping_the_oldest_entry_keeps_the_view_and_draws_only_what_changed() {
        let mut screen = screen(60, 20);
        let state = crate::interactive::tui::tests::idle_state();
        let notices = |range: std::ops::Range<usize>| {
            range
                .map(|index| HistoryItem::Notice {
                    message: format!("line-{index}"),
                })
                .collect::<Vec<_>>()
        };
        screen.insert_history_batch(&notices(0..super::REPRINT_ENTRIES));
        screen.draw(&state).expect("drawn");
        let wheel = MouseInput {
            kind: MouseKind::WheelUp,
            column: 5,
            row: 3,
            modified: false,
        };
        for _ in 0..10 {
            screen.mouse(wheel, false);
        }
        screen.draw(&state).expect("drawn");
        let before = painted(&screen);
        screen.insert_history_batch(&notices(
            super::REPRINT_ENTRIES..super::REPRINT_ENTRIES + 50,
        ));
        assert!(screen.rows_key.is_some(), "the rows were not drawn again");
        screen.draw(&state).expect("drawn");
        assert_eq!(painted(&screen), before, "the reader's view did not move");
        assert_eq!(screen.shown.len(), super::REPRINT_ENTRIES);
        assert_eq!(screen.rows.len(), screen.row_counts.iter().sum::<usize>());
        // The older entries, drawn between frames, complete the same rows.
        screen.draw_all();
        assert_eq!(screen.built_from, 0);
        assert_eq!(screen.row_counts.len(), screen.shown.len());
        let kept = screen.rows.clone();
        screen.rows_key = None;
        screen.ensure_rows(60);
        screen.draw_all();
        assert_eq!(screen.rows, kept, "the same rows a full redraw gives");
    }

    /// Opening a subagent's pane narrows the conversation: only the newest
    /// entries are drawn again at once, the older ones between frames, and a
    /// reader scrolled back keeps the entry at the window's top.
    #[test]
    fn a_new_width_draws_the_newest_entries_first_and_keeps_the_readers_place() {
        let mut screen = screen(100, 20);
        let state = crate::interactive::tui::tests::idle_state();
        let notices = (0..2000)
            .map(|index| HistoryItem::Notice {
                message: format!("entry-{index}"),
            })
            .collect::<Vec<_>>();
        screen.insert_history_batch(&notices);
        screen.draw(&state).expect("drawn");
        assert!(
            screen.built_from > 1500,
            "only the newest are drawn: {}",
            screen.built_from
        );
        assert!(
            painted(&screen)
                .join(
                    "
"
                )
                .contains("entry-1999")
        );
        // Between frames the rest arrive, a little at a time.
        while screen.built_from > 0 {
            assert!(screen.draw_older(usize::MAX, Some(super::OLDER_BUDGET), 0) > 0);
        }
        assert_eq!(screen.row_counts.len(), 2000);

        // Scrolled back to entry 1200, then the width changes.
        let at = screen.rows.iter().position(|row| {
            row.spans
                .iter()
                .any(|span| span.content.contains("entry-1200"))
        });
        screen.viewport.place(at.expect("a row of entry 1200"));
        screen.draw(&state).expect("drawn");
        let top = |screen: &FullscreenScreen<ratatui::backend::TestBackend>| {
            screen.rows[screen.viewport.top()]
                .spans
                .iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        };
        assert!(top(&screen).contains("entry-1200"), "{}", top(&screen));
        // Ctrl+O: the next frame draws in the new mode.
        let mut details = state.clone();
        details.detail = Detail::Details;
        screen.reprint(Detail::Details);
        screen.draw(&details).expect("drawn");
        assert!(top(&screen).contains("entry-1200"), "{}", top(&screen));
        assert!(
            screen.built_from > 0,
            "the older entries are left for later"
        );
        while screen.built_from > 0 {
            screen.tick();
        }
        assert!(top(&screen).contains("entry-1200"), "{}", top(&screen));
    }

    /// Tool cards and output lines come and go in the live block while a run
    /// lasts; the conversation above it must not move up and down with them.
    #[test]
    fn the_conversation_holds_still_while_the_live_block_changes_in_a_run() {
        let mut screen = screen(60, 24);
        let mut state = crate::interactive::tui::tests::idle_state();
        state.phase = AppPhase::Running;
        screen.insert_history_batch(
            &(0..60)
                .map(|index| HistoryItem::Notice {
                    message: format!("line-{index}"),
                })
                .collect::<Vec<_>>(),
        );
        let newest_row = |screen: &FullscreenScreen<_>| {
            painted(screen)
                .iter()
                .position(|row| row.contains("line-59"))
                .expect("the newest row is shown")
        };
        state.open_tools = vec![
            ("read_file".to_owned(), "path=a".to_owned()),
            ("read_file".to_owned(), "path=b".to_owned()),
            ("read_file".to_owned(), "path=c".to_owned()),
        ];
        state.live_tool_output = vec!["one".to_owned(), "two".to_owned()];
        screen.draw(&state).expect("drawn");
        let tall = newest_row(&screen);
        state.open_tools.clear();
        state.live_tool_output.clear();
        screen.draw(&state).expect("drawn");
        assert_eq!(newest_row(&screen), tall, "the window kept its height");
        // Once the run ends the window takes the room back.
        state.phase = AppPhase::Ready;
        screen.draw(&state).expect("drawn");
        assert!(newest_row(&screen) > tall);
    }

    /// Seen in 0.3.4: a tall panel opened during a run set the held height, and
    /// for the rest of the run the conversation was two rows over a blank
    /// screen. Only the live block's room is held; a panel gives its back.
    #[test]
    fn a_panel_opened_in_a_run_gives_its_room_back_when_it_closes() {
        let mut screen = screen(60, 30);
        let mut state = crate::interactive::tui::tests::idle_state();
        state.phase = AppPhase::Running;
        screen.insert_history_batch(
            &(0..80)
                .map(|index| HistoryItem::Notice {
                    message: format!("line-{index}"),
                })
                .collect::<Vec<_>>(),
        );
        let shown = |screen: &FullscreenScreen<_>| {
            painted(screen)
                .iter()
                .filter(|row| row.contains("line-"))
                .count()
        };
        screen.draw(&state).expect("drawn");
        let before = shown(&screen);
        state.modal = Some(crate::interactive::events::Modal::Question {
            prompt: "pick one".to_owned(),
            options: (0..12).map(|index| format!("option {index}")).collect(),
            selected: 0,
        });
        screen.draw(&state).expect("drawn");
        assert!(shown(&screen) < before, "the panel takes its room");
        state.modal = None;
        screen.draw(&state).expect("drawn");
        assert_eq!(shown(&screen), before, "and gives it back");
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

    /// How long a frame takes on a heavy screen: a long run's conversation,
    /// six delegates, one of them 193 calls in. `cargo test --release ... --
    /// --ignored frame_cost --nocapture` prints the costs; nothing is asserted.
    #[test]
    #[ignore = "a measurement, run by hand"]
    #[allow(
        clippy::too_many_lines,
        reason = "one heavy screen, timed step by step"
    )]
    fn frame_cost() {
        use crate::interactive::events::{
            AppPhase, ChildActivity, ChildLine, ChildLineKind, ChildRunState, ToolState,
        };
        use std::time::{Duration, Instant};
        let mut screen = screen(200, 50).with_sidebar(true);
        let mut items = Vec::new();
        for turn in 0..400 {
            items.push(HistoryItem::User {
                text: format!("turn {turn}: làm game full chức năng"),
            });
            for call in 0..4 {
                items.push(HistoryItem::Tool {
                    name: "read_file".to_owned(),
                    summary: format!("read_file: path=src/modules/arena/service{call}.ts"),
                    input: String::new(),
                    state: ToolState::Ok {
                        elapsed: Duration::from_millis(20),
                    },
                });
                items.push(HistoryItem::ToolOutput {
                    name: "read_file".to_owned(),
                    text: (0..30)
                        .map(|line| {
                            format!("{line}: const value = this.rejectNotInWorld(session, rows);")
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    path: Some("src/a.ts".to_owned()),
                });
            }
            items.push(HistoryItem::Assistant {
                text: "## Kết quả\n\n- **arena**: `service.ts` sửa xong, chạy `npm test` qua.\n- bảng:\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```ts\nconst x = 1;\n```\n".repeat(2),
            });
        }
        screen.insert_history_batch(&items);
        let mut state = crate::interactive::tui::tests::idle_state();
        state.phase = AppPhase::Running;
        state.child_activity = (0..6)
            .map(|child| ChildActivity {
                name: format!("coder-cd19609{child}"),
                tool_calls: if child == 0 { 193 } else { 12 },
                latest: "read_file: path=src/a.ts".to_owned(),
                recent: (0..60)
                    .map(|line| ChildLine {
                        kind: if line % 3 == 0 {
                            ChildLineKind::Call
                        } else {
                            ChildLineKind::Output
                        },
                        text: format!("read_file: path=src/modules/arena/file{line}.ts"),
                        call: format!("c{}", line / 3),
                    })
                    .collect(),
                state: if child < 2 {
                    ChildRunState::Running
                } else {
                    ChildRunState::Done
                },
                ..ChildActivity::default()
            })
            .collect();
        let time =
            |screen: &mut FullscreenScreen<ratatui::backend::TestBackend>,
             state: &crate::interactive::events::UiState,
             what: &str,
             change: &dyn Fn(&mut FullscreenScreen<ratatui::backend::TestBackend>)| {
                let mut worst = Duration::ZERO;
                let mut total = Duration::ZERO;
                for _ in 0..20 {
                    change(screen);
                    let begun = Instant::now();
                    screen.draw(state).expect("drawn");
                    let took = begun.elapsed();
                    worst = worst.max(took);
                    total += took;
                }
                println!("{what:<28} mean {:>8.2?} worst {:>8.2?}", total / 20, worst);
            };
        screen.draw(&state).expect("drawn");
        time(&mut screen, &state, "idle frame (tick)", &|_| {});
        time(&mut screen, &state, "wheel notch up", &|screen| {
            let _ = screen.mouse(
                MouseInput {
                    kind: MouseKind::WheelUp,
                    column: 120,
                    row: 20,
                    modified: false,
                },
                false,
            );
        });
        screen.follow();
        let pick = |screen: &mut FullscreenScreen<ratatui::backend::TestBackend>,
                    state: &crate::interactive::events::UiState,
                    what: &str,
                    number: u8| {
            let _ = screen.agent_key(number, state);
            let begun = Instant::now();
            screen.draw(state).expect("drawn");
            println!("{what:<28} {:>8.2?}", begun.elapsed());
        };
        pick(&mut screen, &state, "open a child's pane", 2);
        pick(&mut screen, &state, "close it", 1);
        for _ in 0..30 {
            let _ = screen.mouse(
                MouseInput {
                    kind: MouseKind::WheelUp,
                    column: 120,
                    row: 20,
                    modified: false,
                },
                false,
            );
        }
        screen.draw(&state).expect("drawn");
        pick(&mut screen, &state, "scrolled back, open a pane", 2);
        pick(&mut screen, &state, "scrolled back, close it", 1);
        screen.follow();
        screen.draw(&state).expect("drawn");
        let started = Instant::now();
        let mut ticks = 0;
        while screen.built_from > 0 {
            screen.tick();
            ticks += 1;
        }
        println!(
            "{:<28} {:>8.2?} in {ticks} ticks",
            "older entries drawn",
            started.elapsed()
        );
        let _ = screen.agent_key(2, &state);
        time(&mut screen, &state, "child pane open (tick)", &|_| {});
        time(&mut screen, &state, "child pane, wheel", &|screen| {
            let _ = screen.mouse(
                MouseInput {
                    kind: MouseKind::WheelDown,
                    column: 120,
                    row: 20,
                    modified: false,
                },
                false,
            );
        });
        let _ = screen.agent_key(1, &state);
        time(&mut screen, &state, "a notice comes in", &|screen| {
            screen.insert_history_batch(&[HistoryItem::Notice {
                message: "agent message · main → coder-cd196090 · tiếp tục".to_owned(),
            }]);
        });
        time(&mut screen, &state, "a tool card comes in", &|screen| {
            screen.insert_history_batch(&[HistoryItem::Tool {
                name: "read_file".to_owned(),
                summary: "read_file: path=src/a.ts".to_owned(),
                input: String::new(),
                state: ToolState::Ok {
                    elapsed: Duration::from_millis(20),
                },
            }]);
        });
        time(&mut screen, &state, "scrolled back, notice", &|screen| {
            let _ = screen.mouse(
                MouseInput {
                    kind: MouseKind::WheelUp,
                    column: 120,
                    row: 20,
                    modified: false,
                },
                false,
            );
            screen.insert_history_batch(&[HistoryItem::Notice {
                message: "context: compacted".to_owned(),
            }]);
        });
    }
}
