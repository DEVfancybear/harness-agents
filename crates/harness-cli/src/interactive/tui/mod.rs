//! TUI renderer for the interactive app (`HA_TUI`).
//!
//! The renderer draws an **inline viewport** at the bottom of the console and
//! pushes finished conversation rows above it with `Terminal::insert_before`.
//! Everything above the viewport is the terminal's own scrollback: the app never
//! reimplements scrolling, and quitting leaves the conversation on screen.
//!
//! T01 measured the three decisions this module depends on: the inline viewport
//! height is fixed when the terminal is created (so a resize only relayouts, it
//! never rebuilds the terminal); `insert_before` erases the viewport, so an insert
//! is always followed by a draw or the user sees a blank panel; and leaving the
//! viewport needs a blank frame, otherwise rows an earlier frame painted stay on
//! screen above the shell prompt.

pub mod card;
pub mod fullscreen;
pub mod highlight;
pub mod history;
pub mod icons;
pub mod layout;
pub mod markdown;
#[cfg(test)]
mod preview;
pub mod theme;
pub mod widgets;

use std::io;
use std::time::{Duration, Instant};

use harness_types::{ErrorCode, HarnessError};
use ratatui::backend::{Backend, ClearType};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Widget;
use ratatui::{Terminal, TerminalOptions, Viewport};

use self::theme::Theme;
use super::controller::Effect;
use super::events::{HistoryItem, Key, UiState};
use super::frontend::Frontend;
use super::terminal::TerminalBackend;

/// How long one render-loop iteration waits for a key before draining events.
///
/// The same interval as the plain renderer, so streaming feels identical in both.
pub const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How often the viewport is repainted while a run is active.
///
/// The spinner and the elapsed clock need a repaint; an idle app needs none, and
/// that is what acceptance U06 measures.
const TICK_INTERVAL: Duration = Duration::from_millis(100);

/// How long after the last resize the screen is repainted once more.
const RESIZE_SETTLE: Duration = Duration::from_millis(250);

/// The status bar always takes exactly one row.
pub const STATUS_ROWS: u16 = 1;

/// The viewport never grows past this, and never below [`MIN_VIEWPORT_ROWS`].
const MAX_VIEWPORT_ROWS: u16 = 14;
const MIN_VIEWPORT_ROWS: u16 = 5;

/// The inline viewport height for a console of `rows` rows.
///
/// T01 measured that this cannot change while the app runs, so it is decided once
/// and the composer scrolls internally instead.
#[must_use]
pub fn viewport_rows(rows: u16) -> u16 {
    (rows / 2).clamp(MIN_VIEWPORT_ROWS, MAX_VIEWPORT_ROWS)
}

/// What the loop must do after one step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Step {
    Continue,
    Exit(u8),
}

/// How the loop ended, with the renderer back so a caller can read what it drew.
///
/// The binary only reads the exit code; the tests read the painted rows, which is
/// why the renderer is handed back instead of being dropped inside the loop.
pub struct Outcome<R> {
    pub code: u8,
    #[allow(dead_code, reason = "the unit tests read the painted rows from it")]
    pub renderer: R,
}

/// Everything the render loop needs from the terminal, so the same loop drives a
/// real console and the scripted backend in tests.
pub trait TuiRenderer {
    #[allow(dead_code, reason = "the tests measure the painted width through it")]
    fn columns(&self) -> u16;
    #[allow(dead_code, reason = "T07 uses the height to clamp the layout")]
    fn rows(&self) -> u16;
    /// Wait up to the timeout for a key, and take it.
    fn poll_key(&mut self, timeout: Duration) -> io::Result<Option<Key>>;
    /// Paint one frame of the viewport.
    fn draw_state(&mut self, state: &UiState) -> io::Result<()>;
    /// Push one finished history entry into the scrollback above the viewport.
    fn insert_history(&mut self, item: &HistoryItem) -> io::Result<()>;
    /// Push several entries at once. Every insert erases the viewport, so the
    /// entries one loop step produced go in as one insert, not one each.
    fn insert_history_batch(&mut self, items: &[HistoryItem]) -> io::Result<()> {
        for item in items {
            self.insert_history(item)?;
        }
        Ok(())
    }
    /// Start one frame's worth of output: the terminal shows nothing of it until
    /// [`Self::end_update`], so an insert that erases the viewport and the draw
    /// that repaints it reach the screen together instead of as a flash.
    fn begin_update(&mut self) -> io::Result<()> {
        Ok(())
    }
    /// Show what was written since [`Self::begin_update`].
    fn end_update(&mut self) -> io::Result<()> {
        Ok(())
    }
    /// Clear only the inline viewport; terminal scrollback remains intact.
    fn clear_viewport(&mut self) -> io::Result<()> {
        Ok(())
    }
    /// Write plain assistant text to the OS clipboard.
    fn copy_text(&mut self, _text: &str) -> io::Result<()> {
        Ok(())
    }
    /// Emit the configured terminal bell.
    fn bell(&mut self) -> io::Result<()> {
        Ok(())
    }
    /// Redraw what the screen shows in this detail mode: the newest rows that
    /// fit above the viewport. prime-agent does this for ctrl+o
    /// (`requestRenderPreservingViewport`) and when the terminal is resized; the
    /// scrollback keeps what it already had.
    fn reprint(&mut self, _detail: super::events::Detail) -> io::Result<()> {
        Ok(())
    }
    /// Take over a conversation another process drew (`ha attach`).
    fn restore(&mut self, items: &[HistoryItem]) -> io::Result<()> {
        self.insert_history_batch(items)
    }
    /// The opening banner has new lines (the model or the thinking level changed):
    /// replace what it says and draw the screen again.
    fn retitle(&mut self, _lines: &[String]) -> io::Result<()> {
        Ok(())
    }
    /// Whether prime-agent's fullscreen rendering is up.
    fn fullscreen(&self) -> bool {
        false
    }
    /// Enter or leave prime-agent's fullscreen rendering (`/fullscreen`).
    fn set_fullscreen(&mut self, _on: bool) -> io::Result<()> {
        Ok(())
    }
    /// One mouse report, while fullscreen tracks the mouse.
    fn mouse(
        &mut self,
        _input: super::events::MouseInput,
        _panel_open: bool,
    ) -> fullscreen::MouseOutcome {
        fullscreen::MouseOutcome::default()
    }
    /// prime-agent's fullscreen viewport keys; false when the key is not one.
    fn viewport_key(&mut self, _key: &Key) -> bool {
        false
    }
    /// Let a drag held at an edge scroll on; true when the frame changed.
    fn tick_viewport(&mut self) -> bool {
        false
    }
    /// Erase the viewport footprint and leave the cursor on a fresh line.
    fn finish(&mut self) -> io::Result<()>;
}

/// How many history entries a renderer keeps for ctrl+o to draw again.
const REPRINT_ENTRIES: usize = 4000;

/// The terminal-side half of a renderer: a ratatui `Terminal` over some backend.
pub struct RealRenderer<B: Backend> {
    terminal: Terminal<B>,
    /// The inline viewport's height, fixed when the terminal was opened (T01).
    viewport_height: u16,
    theme: Theme,
    /// The detail mode of the last frame, which history rows are drawn in.
    detail: super::events::Detail,
    /// What was pushed into the scrollback, newest last, for a reprint.
    shown: std::collections::VecDeque<HistoryItem>,
    assistant_continuing: bool,
    /// The console size the last paint was made for. A paint or an insert at any
    /// other size first repaints the whole screen: a terminal that is being dragged
    /// bigger or smaller re-wraps what it shows, and drawing only what changed on
    /// top of that leaves the old frame behind and a second one a row away.
    known: (u16, u16),
}

impl<B: Backend> RealRenderer<B>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Open the inline viewport on this backend.
    ///
    /// # Errors
    /// Fails when the backend cannot report its size or cannot be initialised.
    pub fn open(backend: B) -> io::Result<Self> {
        Self::with_theme(backend, &Theme::detect())
    }

    /// Open the viewport with an explicit palette.
    ///
    /// The console decides the palette ([`Theme::detect`]); the preview dump passes
    /// one, so a frame it writes does not change with the terminal it was rendered in.
    ///
    /// # Errors
    /// Fails when the backend cannot report its size or cannot be initialised.
    #[cfg(test)]
    pub fn open_with(backend: B, theme: &Theme) -> io::Result<Self> {
        Self::with_theme(backend, theme)
    }

    fn with_theme(backend: B, theme: &Theme) -> io::Result<Self> {
        let size = backend.size().map_err(to_io)?;
        let viewport_height = viewport_rows(size.height);
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(viewport_height),
            },
        )
        .map_err(to_io)?;
        Ok(Self {
            terminal,
            viewport_height,
            theme: *theme,
            detail: super::events::Detail::default(),
            shown: std::collections::VecDeque::new(),
            assistant_continuing: false,
            known: (size.width, size.height),
        })
    }

    /// Redraw the visible screen in `detail`: erase it, put the viewport back at
    /// its top, and push the newest rows that fit above the viewport.
    ///
    /// prime-agent repaints only the visible screen for ctrl+o
    /// (`requestRenderPreservingViewport`) and for a resize (its full render starts
    /// at `newLines.length - height`); the scrollback keeps what it had. Drawing
    /// every remembered entry instead - thousands of rows pushed through the
    /// scrollback - is what made the view jump for a moment before it settled.
    /// A resize needs the same: an inline viewport the terminal re-wrapped at the
    /// new width stays on screen as a broken second input box otherwise.
    fn repaint_screen(&mut self, detail: super::events::Detail) -> io::Result<()> {
        self.detail = detail;
        let size = self.terminal.size().map_err(to_io)?;
        self.known = (size.width, size.height);
        // The whole screen, erased from its top-left cell down: the cursor is put
        // there first, since after a resize its own position is the least
        // trustworthy thing the terminal reports. Not `ED 2`: Windows Terminal
        // moves what `ED 2` erases into the scrollback, so every repaint of a
        // drag left one more copy of the screen above it.
        self.terminal.set_cursor_position((0, 0)).map_err(to_io)?;
        self.terminal
            .backend_mut()
            .clear_region(ClearType::AfterCursor)
            .map_err(to_io)?;
        // The cursor is at the top of an erased screen, so the viewport is
        // placed there; the rows pushed below then move it down to the bottom.
        self.terminal
            .resize(Rect::new(0, 0, size.width, size.height))
            .map_err(to_io)?;
        let budget = usize::from(size.height.saturating_sub(self.viewport_height));
        let theme = self.theme;
        // Newest first until the screen is full, then back into reading order.
        let mut blocks = Vec::new();
        let mut total = 0_usize;
        for (index, item) in self.shown.iter().enumerate().rev() {
            if total == budget {
                break;
            }
            let continuing =
                index > 0 && matches!(self.shown[index - 1], HistoryItem::Assistant { .. });
            let mut rows = history::render_fragment(item, size.width, &theme, detail, continuing);
            let room = budget - total;
            if rows.len() > room {
                rows.drain(..rows.len() - room);
            }
            total += rows.len();
            blocks.push(rows);
        }
        let rows = blocks.into_iter().rev().flatten().collect::<Vec<_>>();
        self.insert_rows(&rows)
    }

    /// Replace the opening banner's lines in what the screen remembers and draw the
    /// screen again, so a banner still in view names the model now in use. A banner
    /// that has scrolled away is left to the status row, which is always current.
    fn retitle(&mut self, lines: &[String]) -> io::Result<()> {
        let mut replaced = false;
        if let Some(HistoryItem::Banner { lines: banner }) = self
            .shown
            .iter_mut()
            .find(|item| matches!(item, HistoryItem::Banner { .. }))
        {
            banner.clone_from(&lines.to_vec());
            replaced = true;
        }
        if replaced {
            self.repaint_screen(self.detail)?;
        }
        Ok(())
    }

    /// Repaint the screen first when the console changed size since the last paint.
    fn resynced(&mut self) -> io::Result<()> {
        let size = self.terminal.size().map_err(to_io)?;
        if (size.width, size.height) != self.known {
            self.repaint_screen(self.detail)?;
        }
        Ok(())
    }

    /// Take over a conversation another process drew (`ha attach`): keep its
    /// entries for ctrl+o and draw only the newest that fit, as a reprint does,
    /// rather than pushing the whole conversation through the scrollback.
    fn restore(&mut self, items: &[HistoryItem]) -> io::Result<()> {
        self.shown = items
            .iter()
            .skip(items.len().saturating_sub(REPRINT_ENTRIES))
            .cloned()
            .collect();
        self.assistant_continuing =
            matches!(self.shown.back(), Some(HistoryItem::Assistant { .. }));
        self.repaint_screen(self.detail)
    }

    /// Push rows above the viewport in screen-sized batches: one insert per batch
    /// rather than one per history entry, since every insert repaints the viewport.
    fn insert_rows(&mut self, rows: &[Line<'static>]) -> io::Result<()> {
        let batch = usize::from(self.rows().max(1));
        for chunk in rows.chunks(batch) {
            let height = u16::try_from(chunk.len()).unwrap_or(u16::MAX);
            self.terminal
                .insert_before(height, |buffer| {
                    let area = buffer.area;
                    for (index, line) in chunk.iter().enumerate() {
                        let offset = u16::try_from(index).unwrap_or(0);
                        if offset >= area.height {
                            break;
                        }
                        line.clone().render(
                            Rect {
                                x: area.x,
                                y: area.y + offset,
                                width: area.width,
                                height: 1,
                            },
                            buffer,
                        );
                    }
                })
                .map_err(to_io)?;
        }
        Ok(())
    }

    fn columns(&self) -> u16 {
        self.terminal.size().map_or(0, |size| size.width)
    }

    fn rows(&self) -> u16 {
        self.terminal.size().map_or(0, |size| size.height)
    }

    fn draw_state(&mut self, state: &UiState) -> io::Result<()> {
        let theme = self.theme;
        self.detail = state.detail;
        self.resynced()?;
        self.terminal
            .draw(|frame| {
                let plan = layout::plan(frame.area(), state, &theme);
                widgets::render(frame, &plan, state, &theme);
                if let Some((x, y)) = plan.cursor {
                    frame.set_cursor_position((x, y));
                }
            })
            .map(|_| ())
            .map_err(to_io)
    }

    #[cfg(test)]
    fn insert_history(&mut self, item: &HistoryItem) -> io::Result<()> {
        self.insert_history_batch(std::slice::from_ref(item))
    }

    /// Render every entry, then insert all their rows in one go.
    fn insert_history_batch(&mut self, items: &[HistoryItem]) -> io::Result<()> {
        self.resynced()?;
        let theme = self.theme;
        let width = self.columns();
        let mut rows = Vec::new();
        for item in items {
            if self.shown.len() == REPRINT_ENTRIES {
                self.shown.pop_front();
            }
            self.shown.push_back(item.clone());
            rows.extend(history::render_fragment(
                item,
                width,
                &theme,
                self.detail,
                self.assistant_continuing,
            ));
            self.assistant_continuing = matches!(item, HistoryItem::Assistant { .. });
        }
        if rows.is_empty() {
            return Ok(());
        }
        self.insert_rows(&rows)
    }

    fn clear_viewport(&mut self) -> io::Result<()> {
        self.terminal.clear().map_err(to_io)
    }

    fn finish(&mut self) -> io::Result<()> {
        // Measured in Windows Terminal (T01): the viewport moves as rows are
        // inserted, so the rows it left behind are only erased by painting them
        // blank. Clearing the region alone leaves ghost rows above the prompt.
        self.terminal
            .draw(|frame| {
                frame.render_widget(ratatui::widgets::Clear, frame.area());
            })
            .map_err(to_io)?;
        let size = self.terminal.size().map_err(to_io)?;
        self.terminal
            .set_cursor_position((0, size.height.saturating_sub(1)))
            .map_err(to_io)?;
        self.terminal.show_cursor().map_err(to_io)?;
        Ok(())
    }
}

/// What the real console shows: prime-agent's inline viewport, or its
/// fullscreen screen.
enum Screen {
    Inline(Box<RealRenderer<ratatui::backend::CrosstermBackend<io::Stdout>>>),
    Fullscreen(Box<fullscreen::FullscreenScreen<ratatui::backend::CrosstermBackend<io::Stdout>>>),
}

/// The real renderer: keys through the host backend, frames through ratatui.
///
/// It owns the host backend, so the loop reads keys through the same object that
/// draws: there is only ever one reader of the console.
pub struct RuntimeRenderer<T: TerminalBackend> {
    backend: T,
    screen: Screen,
    /// prime-agent's `fullscreenMouse`: fullscreen tracks the mouse.
    mouse: bool,
}

impl<T: TerminalBackend> RuntimeRenderer<T> {
    /// Open the viewport on the real console.
    ///
    /// # Errors
    /// Fails when the console cannot be reached or measured.
    #[allow(dead_code, reason = "T07 opens the viewport from the fallback probe")]
    pub fn open(backend: T) -> io::Result<Self> {
        Self::open_with(
            backend,
            fullscreen::Prefs {
                enabled: false,
                mouse: false,
            },
        )
    }

    /// Open the console in prime-agent's fullscreen rendering when `prefs`
    /// asks for it, and in the inline viewport otherwise - or when the console
    /// refuses the alternate screen.
    ///
    /// # Errors
    /// Fails when the console cannot be reached or measured.
    pub fn open_with(backend: T, prefs: fullscreen::Prefs) -> io::Result<Self> {
        let mut renderer = Self {
            backend,
            screen: Screen::Inline(Box::new(RealRenderer::open(
                ratatui::backend::CrosstermBackend::new(io::stdout()),
            )?)),
            mouse: prefs.mouse,
        };
        if prefs.enabled {
            renderer.set_fullscreen(true)?;
        }
        Ok(renderer)
    }
}

impl<T: TerminalBackend> TuiRenderer for RuntimeRenderer<T> {
    fn columns(&self) -> u16 {
        match &self.screen {
            Screen::Inline(inline) => inline.columns(),
            Screen::Fullscreen(screen) => screen.columns(),
        }
    }

    fn rows(&self) -> u16 {
        match &self.screen {
            Screen::Inline(inline) => inline.rows(),
            Screen::Fullscreen(screen) => screen.rows_count(),
        }
    }

    fn poll_key(&mut self, timeout: Duration) -> io::Result<Option<Key>> {
        // The injected backend fault (I08) is a property of the terminal, not of
        // one renderer: it must fail the TUI exactly like the plain renderer.
        super::terminal::injected_fault()?;
        if self.backend.poll_key(timeout)? {
            Ok(Some(self.backend.read_key()?))
        } else {
            Ok(None)
        }
    }

    fn draw_state(&mut self, state: &UiState) -> io::Result<()> {
        super::terminal::injected_fault()?;
        match &mut self.screen {
            Screen::Inline(inline) => inline.draw_state(state),
            Screen::Fullscreen(screen) => screen.draw(state),
        }
    }

    fn insert_history(&mut self, item: &HistoryItem) -> io::Result<()> {
        self.insert_history_batch(std::slice::from_ref(item))
    }

    fn insert_history_batch(&mut self, items: &[HistoryItem]) -> io::Result<()> {
        match &mut self.screen {
            Screen::Inline(inline) => inline.insert_history_batch(items),
            Screen::Fullscreen(screen) => {
                screen.insert_history_batch(items);
                Ok(())
            }
        }
    }

    // DEC mode 2026, synchronized output: a terminal that knows it holds the
    // frame until the end mark; one that does not ignores both marks.
    fn begin_update(&mut self) -> io::Result<()> {
        self.backend.write("\x1b[?2026h")?;
        self.backend.flush()
    }

    fn end_update(&mut self) -> io::Result<()> {
        self.backend.write("\x1b[?2026l")?;
        self.backend.flush()
    }

    fn clear_viewport(&mut self) -> io::Result<()> {
        match &mut self.screen {
            Screen::Inline(inline) => inline.clear_viewport(),
            Screen::Fullscreen(screen) => screen.clear(),
        }
    }

    fn reprint(&mut self, detail: super::events::Detail) -> io::Result<()> {
        match &mut self.screen {
            Screen::Inline(inline) => inline.repaint_screen(detail),
            Screen::Fullscreen(screen) => {
                screen.reprint(detail);
                Ok(())
            }
        }
    }

    fn restore(&mut self, items: &[HistoryItem]) -> io::Result<()> {
        match &mut self.screen {
            Screen::Inline(inline) => inline.restore(items),
            Screen::Fullscreen(screen) => {
                screen.restore(items);
                Ok(())
            }
        }
    }

    fn retitle(&mut self, lines: &[String]) -> io::Result<()> {
        match &mut self.screen {
            Screen::Inline(inline) => inline.retitle(lines),
            Screen::Fullscreen(screen) => {
                screen.retitle(lines);
                Ok(())
            }
        }
    }

    fn fullscreen(&self) -> bool {
        matches!(self.screen, Screen::Fullscreen(_))
    }

    /// prime-agent's `enterFullscreen` / `exitFullscreen`: the alternate screen
    /// takes the conversation over, and leaving it prints what arrived while it
    /// was up into the primary screen's scrollback.
    fn set_fullscreen(&mut self, on: bool) -> io::Result<()> {
        if on == self.fullscreen() {
            return Ok(());
        }
        let backend = || ratatui::backend::CrosstermBackend::new(io::stdout());
        if on {
            let Screen::Inline(inline) = &mut self.screen else {
                return Ok(());
            };
            let shown = std::mem::take(&mut inline.shown);
            let (theme, detail) = (inline.theme, inline.detail);
            // The inline viewport is erased, as leaving the app erases it.
            inline.finish()?;
            if let Err(error) = super::terminal::enter_fullscreen(self.mouse) {
                inline.shown = shown;
                return Err(error);
            }
            let screen = fullscreen::FullscreenScreen::open(backend(), &theme, detail, shown)?;
            self.screen = Screen::Fullscreen(Box::new(screen));
        } else {
            let placeholder = Screen::Inline(Box::new(RealRenderer::open(backend())?));
            let Screen::Fullscreen(screen) = std::mem::replace(&mut self.screen, placeholder)
            else {
                return Ok(());
            };
            let (mut shown, arrived) = screen.into_shown();
            super::terminal::leave_fullscreen(self.mouse)?;
            let mut printed = shown.split_off(shown.len() - arrived.min(shown.len()));
            let mut inline = RealRenderer::open(backend())?;
            inline.shown = shown;
            inline.insert_history_batch(printed.make_contiguous())?;
            self.screen = Screen::Inline(Box::new(inline));
        }
        Ok(())
    }

    fn mouse(
        &mut self,
        input: super::events::MouseInput,
        panel_open: bool,
    ) -> fullscreen::MouseOutcome {
        match &mut self.screen {
            Screen::Fullscreen(screen) => screen.mouse(input, panel_open),
            Screen::Inline(_) => fullscreen::MouseOutcome::default(),
        }
    }

    fn viewport_key(&mut self, key: &Key) -> bool {
        let Screen::Fullscreen(screen) = &mut self.screen else {
            return false;
        };
        match key {
            Key::PageUp => screen.page(false),
            Key::PageDown => screen.page(true),
            Key::ViewportTop => screen.scroll_to_top(),
            Key::ViewportFollow => screen.follow(),
            _ => return false,
        }
        true
    }

    fn tick_viewport(&mut self) -> bool {
        match &mut self.screen {
            Screen::Fullscreen(screen) => screen.tick(),
            Screen::Inline(_) => false,
        }
    }

    fn copy_text(&mut self, text: &str) -> io::Result<()> {
        let mut clipboard =
            arboard::Clipboard::new().map_err(|error| io::Error::other(error.to_string()))?;
        clipboard
            .set_text(text.to_owned())
            .map_err(|error| io::Error::other(error.to_string()))
    }

    fn bell(&mut self) -> io::Result<()> {
        self.backend.write("\x07")?;
        self.backend.flush()
    }

    fn finish(&mut self) -> io::Result<()> {
        // Fullscreen is left first, so what it showed reaches the scrollback.
        self.set_fullscreen(false)?;
        if let Screen::Inline(inline) = &mut self.screen {
            inline.finish()?;
        }
        // Leave the cursor at column zero of a fresh line, so the shell prompt
        // that follows the app is not parked inside the viewport's last row.
        self.backend.write("\r\n")?;
        self.backend.flush()
    }
}

/// Renderer that paints through a `TestBackend` and mirrors what it drew into the
/// host's terminal backend, so a test can drive the real loop and assert on the
/// viewport rows without a console.
pub struct ScriptedRenderer<T: TerminalBackend> {
    backend: T,
    inner: RealRenderer<ratatui::backend::TestBackend>,
    painted: Vec<String>,
    copied: Vec<String>,
    bells: usize,
}

impl<T: TerminalBackend> ScriptedRenderer<T> {
    /// Open the scripted renderer for a console of this size.
    ///
    /// # Errors
    /// Fails when the test backend cannot be initialised.
    #[allow(dead_code, reason = "T03-T07 drive the scripted renderer from tests")]
    pub fn open(backend: T, columns: u16, rows: u16) -> io::Result<Self> {
        let test = ratatui::backend::TestBackend::new(columns, rows);
        Ok(Self {
            backend,
            inner: RealRenderer::open(test)?,
            painted: Vec::new(),
            copied: Vec::new(),
            bells: 0,
        })
    }

    /// The rows the last frame painted, top to bottom.
    #[cfg(test)]
    #[must_use]
    pub fn painted(&self) -> &[String] {
        &self.painted
    }

    #[cfg(test)]
    #[must_use]
    pub fn copied(&self) -> &[String] {
        &self.copied
    }

    #[cfg(test)]
    #[must_use]
    pub const fn bells(&self) -> usize {
        self.bells
    }

    /// The host backend, so a test can read what the loop wrote.
    #[cfg(test)]
    #[must_use]
    pub const fn backend(&self) -> &T {
        &self.backend
    }

    /// Copy the rows the frame painted into the host's backend.
    fn mirror(&mut self) -> io::Result<()> {
        let buffer = self.inner.terminal.backend().buffer().clone();
        let mut painted = Vec::new();
        for y in 0..buffer.area.height {
            let mut row = String::new();
            for x in 0..buffer.area.width {
                if let Some(cell) = buffer.cell((x, y)) {
                    row.push_str(cell.symbol());
                }
            }
            painted.push(row.trim_end().to_owned());
        }
        while painted.last().is_some_and(String::is_empty) {
            painted.pop();
        }
        for row in &painted {
            self.backend.write(row)?;
            self.backend.write("\r\n")?;
        }
        self.backend.flush()?;
        self.painted = painted;
        Ok(())
    }
}

impl<T: TerminalBackend> TuiRenderer for ScriptedRenderer<T> {
    fn columns(&self) -> u16 {
        self.inner.columns()
    }

    fn rows(&self) -> u16 {
        self.inner.rows()
    }

    fn poll_key(&mut self, timeout: Duration) -> io::Result<Option<Key>> {
        if self.backend.poll_key(timeout)? {
            Ok(Some(self.backend.read_key()?))
        } else {
            Ok(None)
        }
    }

    fn draw_state(&mut self, state: &UiState) -> io::Result<()> {
        self.inner.draw_state(state)?;
        self.mirror()
    }

    fn reprint(&mut self, detail: super::events::Detail) -> io::Result<()> {
        self.inner.repaint_screen(detail)
    }

    fn retitle(&mut self, lines: &[String]) -> io::Result<()> {
        self.inner.retitle(lines)
    }

    fn restore(&mut self, items: &[HistoryItem]) -> io::Result<()> {
        self.inner.restore(items)?;
        self.mirror()
    }

    fn insert_history(&mut self, item: &HistoryItem) -> io::Result<()> {
        let width = self.inner.columns();
        let rows = history::render_fragment(
            item,
            width,
            &self.inner.theme,
            self.inner.detail,
            self.inner.assistant_continuing,
        );
        self.inner.assistant_continuing = matches!(item, HistoryItem::Assistant { .. });
        for line in &rows {
            self.backend.write(&line.to_string())?;
            self.backend.write("\r\n")?;
        }
        self.backend.flush()?;
        Ok(())
    }

    fn clear_viewport(&mut self) -> io::Result<()> {
        self.inner.clear_viewport()?;
        self.mirror()
    }

    fn copy_text(&mut self, text: &str) -> io::Result<()> {
        self.copied.push(text.to_owned());
        Ok(())
    }

    fn bell(&mut self) -> io::Result<()> {
        self.bells += 1;
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        self.inner.finish()?;
        self.backend.write("\r\n")?;
        self.backend.flush()
    }
}

fn to_io<E: std::error::Error + Send + Sync + 'static>(error: E) -> io::Error {
    io::Error::other(error)
}

/// Run the TUI render loop until the controller exits.
///
/// # Errors
/// A terminal failure propagates as a typed error, exactly like the plain
/// renderer: the I08 guarantee does not depend on which renderer is active.
pub fn run(
    backend: impl TerminalBackend,
    controller: &mut impl Frontend,
    notice: Option<&str>,
    prefs: fullscreen::Prefs,
) -> Result<u8, HarnessError> {
    let renderer =
        RuntimeRenderer::open_with(backend, prefs).map_err(|error| terminal_error(&error))?;
    if Theme::detect().color {
        highlight::warm_up();
    }
    run_loop(renderer, controller, notice).map(|outcome| outcome.code)
}

/// The loop, generic over the renderer so the tests drive the same code.
///
/// # Errors
/// A terminal failure propagates as a typed error.
pub fn run_loop<R: TuiRenderer>(
    mut renderer: R,
    controller: &mut impl Frontend,
    notice: Option<&str>,
) -> Result<Outcome<R>, HarnessError> {
    let result = run_loop_inner(&mut renderer, controller, notice);
    if result.is_err() {
        // Best-effort cleanup must not replace the original I/O error. Raw mode
        // is restored by its guard; this clears the inline viewport as well.
        let _ = renderer.finish();
    }
    result.map(|code| Outcome { code, renderer })
}

fn run_loop_inner(
    renderer: &mut impl TuiRenderer,
    controller: &mut impl Frontend,
    notice: Option<&str>,
) -> Result<u8, HarnessError> {
    if let Some(source) = notice {
        controller
            .resume_source(source)
            .map_err(|message| HarnessError::new(ErrorCode::InvalidPayload, message))?;
    }
    controller.set_columns(renderer.columns());
    let boot = controller.boot_lines();
    renderer
        .insert_history(&HistoryItem::Banner { lines: boot })
        .map_err(|error| terminal_error(&error))?;
    if let Some(source) = notice {
        renderer
            .insert_history(&HistoryItem::Message {
                text: format!(
                    "Selected session {source}; the next request verifies and recovers its context."
                ),
            })
            .map_err(|error| terminal_error(&error))?;
    }
    renderer
        .draw_state(&controller.ui_state())
        .map_err(|error| terminal_error(&error))?;

    let mut last_tick = Instant::now();
    // When the console was last resized. A terminal keeps re-wrapping for a moment
    // after the size event, so once no further resize has come the screen is
    // repainted one more time, at the size it settled on.
    let mut resized_at: Option<Instant> = None;
    loop {
        let mut redraw = false;
        // Everything one step produced is gathered first and painted as one
        // frame: rows pushed into the scrollback erase the viewport, and the draw
        // that repaints it must reach the screen with them (no flash).
        let mut effects = Vec::new();
        if let Some(key) = renderer
            .poll_key(POLL_INTERVAL)
            .map_err(|error| terminal_error(&error))?
        {
            if let Some(handled) = fullscreen_input(renderer, controller, &key, &mut effects) {
                redraw = handled;
            } else if let Key::Resize { columns, .. } = key {
                // The viewport height cannot change (T01). The terminal re-wraps
                // what it shows at the new size, the last viewport included, so
                // the screen is drawn again as prime-agent does on a resize; the
                // draft is untouched.
                controller.set_columns(columns);
                effects.push(Effect::Reprint(controller.ui_state().detail));
                redraw = true;
                resized_at = Some(Instant::now());
            } else {
                let from_key = controller.handle_key(key);
                redraw = !from_key.is_empty();
                effects.extend(from_key);
            }
        }
        if renderer.tick_viewport() {
            redraw = true;
        }
        if resized_at.is_some_and(|at| at.elapsed() >= RESIZE_SETTLE) {
            resized_at = None;
            effects.push(Effect::Reprint(controller.ui_state().detail));
            redraw = true;
        }
        // A key that exits must not wait for the session's events.
        let exiting = effects
            .iter()
            .any(|effect| matches!(effect, Effect::Exit(_)));
        if !exiting {
            let pumped = controller.pump_events();
            redraw = redraw || !pumped.is_empty();
            effects.extend(pumped);
            // The spinner and the clock only need repainting while something runs:
            // an idle app returns no effect, so it never draws (acceptance U06).
            if last_tick.elapsed() >= TICK_INTERVAL {
                last_tick = Instant::now();
                let ticked = controller.tick();
                redraw = redraw || !ticked.is_empty();
                effects.extend(ticked);
            }
        }
        if !redraw && effects.is_empty() {
            continue;
        }
        renderer
            .begin_update()
            .map_err(|error| terminal_error(&error))?;
        let step = apply(renderer, effects);
        let drawn = match step {
            Ok(Step::Continue) if redraw => renderer
                .draw_state(&controller.ui_state())
                .map_err(|error| terminal_error(&error)),
            _ => Ok(()),
        };
        renderer
            .end_update()
            .map_err(|error| terminal_error(&error))?;
        drawn?;
        if let Step::Exit(code) = step? {
            return Ok(code);
        }
    }
}

/// prime-agent's `handleFullscreenInput`: a mouse report is consumed here and
/// never typed, and the viewport keys scroll the transcript unless a panel or a
/// menu keeps its own PageUp/PageDown. `None` when the key is not the viewport's;
/// else whether the frame changed.
fn fullscreen_input(
    renderer: &mut impl TuiRenderer,
    controller: &mut impl Frontend,
    key: &Key,
    effects: &mut Vec<Effect>,
) -> Option<bool> {
    if let Key::Mouse(input) = key {
        let panel_open = controller.ui_state().modal.is_some();
        let outcome = renderer.mouse(*input, panel_open);
        if let Some(text) = outcome.copied {
            effects.push(match renderer.copy_text(&text) {
                Ok(()) => Effect::History(HistoryItem::Notice {
                    message: "Copied selection to clipboard".to_owned(),
                }),
                Err(error) => Effect::History(HistoryItem::Error {
                    message: format!("Failed to copy selection: {error}"),
                }),
            });
        }
        return Some(outcome.redraw);
    }
    if !renderer.fullscreen()
        || !matches!(
            key,
            Key::PageUp | Key::PageDown | Key::ViewportTop | Key::ViewportFollow
        )
    {
        return None;
    }
    let state = controller.ui_state();
    if state.modal.is_some() || !state.suggestions.is_empty() {
        return None;
    }
    Some(renderer.viewport_key(key))
}

/// Apply one batch of effects. Consecutive history rows go into the scrollback
/// as one insert.
fn apply(renderer: &mut impl TuiRenderer, effects: Vec<Effect>) -> Result<Step, HarnessError> {
    let mut pending: Vec<HistoryItem> = Vec::new();
    for effect in effects {
        match effect {
            Effect::History(item) => {
                pending.push(item);
                continue;
            }
            // Streamed text is committed to the scrollback as it is flushed: the
            // live block already showed it, and the history renderer styles it.
            Effect::Stream(text) => {
                pending.push(HistoryItem::Assistant { text });
                continue;
            }
            Effect::Thinking(text) => {
                pending.push(HistoryItem::Thinking { text });
                continue;
            }
            _ => {}
        }
        flush_history(renderer, &mut pending)?;
        match effect {
            Effect::Copy(text) => {
                renderer
                    .copy_text(&text)
                    .map_err(|error| terminal_error(&error))?;
                renderer
                    .insert_history(&HistoryItem::Notice {
                        message: "copied to clipboard".to_owned(),
                    })
                    .map_err(|error| terminal_error(&error))?;
            }
            Effect::Bell => renderer.bell().map_err(|error| terminal_error(&error))?,
            Effect::Reprint(detail) => renderer
                .reprint(detail)
                .map_err(|error| terminal_error(&error))?,
            Effect::ClearViewport => renderer
                .clear_viewport()
                .map_err(|error| terminal_error(&error))?,
            Effect::Fullscreen(on) => renderer
                .set_fullscreen(on)
                .map_err(|error| terminal_error(&error))?,
            Effect::Restore(items) => renderer
                .restore(&items)
                .map_err(|error| terminal_error(&error))?,
            Effect::Banner(lines) => renderer
                .retitle(&lines)
                .map_err(|error| terminal_error(&error))?,
            // History rows were gathered above; a redraw is the frame's own draw.
            Effect::History(_) | Effect::Stream(_) | Effect::Thinking(_) | Effect::Redraw => {}
            Effect::Exit(code) => {
                renderer.finish().map_err(|error| terminal_error(&error))?;
                return Ok(Step::Exit(code));
            }
        }
    }
    flush_history(renderer, &mut pending)?;
    Ok(Step::Continue)
}

fn flush_history(
    renderer: &mut impl TuiRenderer,
    pending: &mut Vec<HistoryItem>,
) -> Result<(), HarnessError> {
    if !pending.is_empty() {
        renderer
            .insert_history_batch(pending)
            .map_err(|error| terminal_error(&error))?;
        pending.clear();
    }
    Ok(())
}

fn terminal_error(error: &io::Error) -> HarnessError {
    HarnessError::new(
        // Terminal I/O failures keep the interactive CLI's generic-failure
        // exit code (1), unlike durable storage write failures (4).
        ErrorCode::MissingRequiredService,
        format!("terminal input/output failed: {error}"),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{
        MAX_VIEWPORT_ROWS, MIN_VIEWPORT_ROWS, ScriptedRenderer, TuiRenderer, viewport_rows,
    };
    use crate::interactive::controller::Effect;
    use crate::interactive::events::{AppPhase, HistoryItem, Key, UiState};
    use crate::interactive::terminal::ScriptedBackend;
    use std::io;
    use std::time::Duration;

    /// T01 measured that the inline viewport height is fixed for the life of the
    /// terminal, so the height decision has to be made here, once.
    #[test]
    fn t02_viewport_height_is_bounded_for_any_console() {
        assert_eq!(viewport_rows(30), 14, "a tall console is capped at 14 rows");
        assert_eq!(viewport_rows(120), MAX_VIEWPORT_ROWS);
        assert_eq!(viewport_rows(10), MIN_VIEWPORT_ROWS);
        assert_eq!(viewport_rows(4), MIN_VIEWPORT_ROWS);
        assert_eq!(viewport_rows(20), 10, "half of the console");
    }

    /// An idle frame with an empty composer.
    pub(crate) fn idle_state() -> UiState {
        let mut idle = state(AppPhase::Ready);
        idle.buffer.clear();
        idle.cursor = 0;
        idle.live_text.clear();
        idle
    }

    fn state(phase: AppPhase) -> UiState {
        UiState {
            phase,
            setup_required: false,
            setup_hint: None,
            header: Vec::new(),
            buffer: "sửa lỗi".to_owned(),
            cursor: 6,
            live_text: "đang trả lời".to_owned(),
            open_tools: Vec::new(),
            modal: None,
            granted_for_run: false,
            queued_input: false,
            queued_count: 0,
            provider_wait: None,
            last_request: None,
            run_started_at: None,
            last_run_elapsed: Duration::ZERO,
            steps: 2,
            max_steps: 8,
            tool_calls: 1,
            max_tool_calls: 16,
            suggestions: Vec::new(),
            suggestion_selected: 0,
            fallback_reason: None,
            tick: 0,
            detail: crate::interactive::events::Detail::default(),
            thinking: None,
            service_tier: None,
            goal: None,
        }
    }

    #[test]
    fn redesign_composer_frame_has_rounded_box_hints_and_status_in_reading_order() {
        let mut draft = state(AppPhase::Ready);
        draft.live_text.clear();
        draft.header = vec!["Service: deepseek-v4-flash".to_owned()];
        let theme = super::theme::Theme::plain();
        let area = ratatui::layout::Rect::new(0, 0, 80, 12);
        let plan = super::layout::plan(area, &draft, &theme);
        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                super::widgets::render(frame, &plan, &draft, &theme);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = (0..12)
            .map(|y| {
                (0..80)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect();
        // The frame is read from the plan, so the assertion is about the reading
        // order - box, then hints, then status - and not about where the anchor
        // happens to put the block.
        let top = usize::from(plan.composer.y);
        assert!(rows[top].starts_with("╭─ Yêu cầu "), "{rows:#?}");
        assert!(rows[top + 1].starts_with("│ > sửa lỗi"), "{rows:#?}");
        assert!(
            rows[top + 2].starts_with('│'),
            "the empty input row gives the draft room"
        );
        assert!(rows[top + 3].starts_with('╰'));
        let hints = &rows[usize::from(plan.hints.y)];
        assert!(
            hints.contains("Enter gửi") && hints.contains("@ file"),
            "{rows:#?}"
        );
        let status = &rows[usize::from(plan.status.y)];
        assert!(status.contains("deepseek-v4-flash"), "{rows:#?}");
    }

    #[test]
    fn redesign_layout_keeps_cursor_inside_the_box_at_every_size() {
        let mut draft = state(AppPhase::Ready);
        draft.live_text.clear();
        draft.buffer = "sửa lỗi\n日本語\nمرحبا Rust\ne\u{301} 😀".to_owned();
        draft.cursor = draft.buffer.chars().count();
        for width in 0..85 {
            for height in 0..25 {
                let area = ratatui::layout::Rect::new(3, 2, width, height);
                let plan = super::layout::plan(area, &draft, &super::theme::Theme::plain());
                for rect in [plan.composer, plan.status] {
                    assert!(rect.x >= area.x && rect.y >= area.y);
                    assert!(
                        rect.right() <= area.right() && rect.bottom() <= area.bottom(),
                        "{area:?}: {plan:?}"
                    );
                }
                if let Some((x, y)) = plan.cursor {
                    assert!(x > plan.composer.x && x < plan.composer.right() - 1);
                    assert!(y > plan.composer.y && y < plan.composer.bottom() - 1);
                }
            }
        }
    }

    #[test]
    fn redesign_stream_fragments_have_no_speaker_label() {
        let backend = ScriptedBackend::new(Vec::new());
        let mut renderer = ScriptedRenderer::open(backend, 80, 24).unwrap();
        for i in 0..20 {
            renderer
                .insert_history(&HistoryItem::Assistant {
                    text: format!("line {i}\n"),
                })
                .unwrap();
        }
        let output = renderer.backend().output();
        assert_eq!(output.matches("▎ HA").count(), 0, "{output}");
        assert!(output.contains("line 0") && output.contains("line 19"));
    }

    #[test]
    fn redesign_picker_keeps_the_selected_last_item_visible() {
        let backend = ScriptedBackend::new(Vec::new());
        let mut renderer = ScriptedRenderer::open(backend, 80, 24).unwrap();
        let mut picking = state(AppPhase::Ready);
        picking.modal = Some(crate::interactive::events::Modal::Picker {
            items: (0..12).map(|i| format!("session_{i}")).collect(),
            selected: 11,
        });
        renderer.draw_state(&picking).unwrap();
        assert!(renderer.painted().join("\n").contains("❯ session_11"));
    }

    #[test]
    fn redesign_short_question_panel_keeps_its_prompt_visible() {
        let backend = ScriptedBackend::new(Vec::new());
        let mut renderer = ScriptedRenderer::open(backend, 80, 14).unwrap();
        let mut asking = state(AppPhase::WaitingInput);
        asking.modal = Some(crate::interactive::events::Modal::Question {
            prompt: "Which file should I update?".into(),
            options: Vec::new(),
            selected: 0,
        });
        renderer.draw_state(&asking).unwrap();
        assert!(
            renderer
                .painted()
                .join("\n")
                .contains("Which file should I update?")
        );
    }

    #[test]
    fn g06_file_picker_renders_workspace_paths_through_test_backend() {
        let backend = ScriptedBackend::new(Vec::new());
        let mut renderer = ScriptedRenderer::open(backend, 100, 30).expect("renderer opens");
        let mut picking = state(AppPhase::Ready);
        picking.modal = Some(crate::interactive::events::Modal::FilePicker {
            items: vec!["src/lib.rs".to_owned(), "docs/guide.md".to_owned()],
            selected: 0,
        });
        renderer.draw_state(&picking).expect("file picker draws");
        let painted = renderer.painted().join("\n");
        assert!(
            painted.contains("chọn file"),
            "file picker title: {painted}"
        );
        assert!(painted.contains("src/lib.rs"), "workspace entry: {painted}");
        assert!(
            painted.contains("gõ lọc") && painted.contains("Esc"),
            "keys: {painted}"
        );
    }

    #[test]
    fn g06_ask_user_panel_renders_question_and_numbered_options() {
        let backend = ScriptedBackend::new(Vec::new());
        let mut renderer = ScriptedRenderer::open(backend, 100, 30).expect("renderer opens");
        let mut asking = state(AppPhase::WaitingInput);
        asking.live_text.clear();
        asking.modal = Some(crate::interactive::events::Modal::Question {
            prompt: "Which color should I use?".to_owned(),
            options: vec!["blue".to_owned(), "green".to_owned()],
            selected: 0,
        });
        renderer.draw_state(&asking).expect("question panel draws");
        let painted = renderer.painted().join("\n");
        assert!(
            painted.contains("Which color should I use?"),
            "prompt: {painted}"
        );
        assert!(
            painted.contains("1. blue") && painted.contains("2. green"),
            "options: {painted}"
        );
        assert!(painted.contains("nhấn Enter"), "free-text hint: {painted}");
    }

    /// A long option list scrolls with the highlight: the chosen option is
    /// always drawn, even past the rows the panel can hold. The composer keeps
    /// the cursor, since the question can be answered by typing.
    #[test]
    fn a_long_question_keeps_the_highlight_in_view_and_the_cursor_in_the_composer() {
        let options = (1..=20)
            .map(|n| format!("option-{n:02}"))
            .collect::<Vec<_>>();
        let mut asking = state(AppPhase::WaitingInput);
        asking.live_text.clear();
        asking.modal = Some(crate::interactive::events::Modal::Question {
            prompt: "Which one?".to_owned(),
            options,
            selected: 17,
        });
        let painted = {
            let backend = ScriptedBackend::new(Vec::new());
            let mut renderer = ScriptedRenderer::open(backend, 100, 20).expect("renderer opens");
            renderer.draw_state(&asking).expect("question panel draws");
            renderer.painted().join("\n")
        };
        assert!(painted.contains("❯ 18. option-18"), "highlight: {painted}");
        assert!(painted.contains("18/20"), "position: {painted}");
        let plan = super::layout::plan(
            ratatui::layout::Rect::new(0, 0, 100, 20),
            &asking,
            &super::theme::Theme::plain(),
        );
        assert!(plan.cursor.is_some(), "the composer keeps the focus");
    }

    /// K01: a masked buffer reaches the screen as a mask, never as the key.
    ///
    /// The controller masks at the single point it builds `UiState`, and the
    /// composer draws whatever is in `state.buffer`. This asserts the painted frame
    /// itself, so a renderer that reached around the state for the raw editor value
    /// could not pass.
    #[test]
    fn k01_a_secret_buffer_is_painted_as_a_mask() {
        let backend = ScriptedBackend::new(Vec::new());
        let mut renderer = ScriptedRenderer::open(backend, 80, 24).expect("renderer opens");
        let mut masked = state(AppPhase::Ready);
        masked.buffer = crate::interactive::input::mask_secret(true, "sk-live-secret");
        masked.cursor = masked.buffer.chars().count();
        renderer.draw_state(&masked).expect("frame draws");
        let painted = renderer.painted().join("\n");
        assert!(
            !painted.contains("sk-live-secret"),
            "the key reached the screen: {painted}"
        );
        assert!(
            painted.contains('\u{2022}'),
            "the mask is what the user sees: {painted}"
        );
    }

    /// The frame carries the D5 text landmarks the PTY assertions depend on.
    #[test]
    fn t04_a_frame_paints_the_composer_marker_the_live_text_and_the_status_row() {
        let backend = ScriptedBackend::new(Vec::new());
        let mut renderer = ScriptedRenderer::open(backend, 80, 24).expect("renderer opens");
        renderer
            .draw_state(&state(AppPhase::Running))
            .expect("frame draws");
        let painted = renderer.painted().join("\n");
        assert!(
            painted.contains(".. sửa lỗi"),
            "the composer carries the running marker and the draft: {painted}"
        );
        assert!(painted.contains("đang trả lời"), "live text: {painted}");
        assert!(
            painted.contains("Writing") && painted.contains("step 2/8"),
            "status row: {painted}"
        );
    }

    /// Seen on a real screen: with the approval panel open, the viewport showed the
    /// proposal twice - the scrollback copy and the panel - and the composer's hint
    /// pointed at a panel it did not name. This asserts the painted frame itself.
    #[test]
    fn t06_a_pending_approval_frame_holds_one_copy_of_the_proposal() {
        let backend = ScriptedBackend::new(Vec::new());
        let mut renderer = ScriptedRenderer::open(backend, 100, 30).expect("renderer opens");
        let mut asking = state(AppPhase::WaitingApproval);
        asking.live_text = String::new();
        asking.modal = Some(crate::interactive::events::Modal::Approval {
            request_id: "req-1".to_owned(),
            action: "apply_patch".to_owned(),
            summary: "path=src/parser.rs".to_owned(),
            workspace: "C:/work/project".to_owned(),
            scope: "once".to_owned(),
            expires_at: std::time::Instant::now() + Duration::from_mins(5),
            read_only: false,
            scroll: 0,
        });
        renderer.draw_state(&asking).expect("frame draws");
        let painted = renderer.painted().join("\n");
        assert_eq!(
            painted.matches("path=src/parser.rs").count(),
            1,
            "the proposal belongs in the panel, once: {painted}"
        );
        assert!(
            painted.contains("workspace: C:/work/project"),
            "the panel carries the proposal detail: {painted}"
        );
        assert!(
            painted.contains("y chạy") && painted.contains("n từ chối"),
            "the panel names its keys: {painted}"
        );
        assert!(
            !painted.contains("đang trả lời"),
            "the live block yields the upper region to the panel: {painted}"
        );
    }

    #[test]
    fn g08_copy_uses_the_scripted_clipboard_boundary() {
        let backend = ScriptedBackend::new(Vec::new());
        let mut renderer = ScriptedRenderer::open(backend, 80, 24).expect("renderer opens");

        super::apply(&mut renderer, vec![Effect::Copy("answer text".to_owned())])
            .expect("copy effect applies");

        assert_eq!(renderer.copied(), ["answer text"]);
    }

    #[test]
    fn g09_bell_effect_reaches_the_testbackend_renderer() {
        let backend = ScriptedBackend::new(Vec::new());
        let mut renderer = ScriptedRenderer::open(backend, 80, 24).expect("renderer opens");

        super::apply(&mut renderer, vec![Effect::Bell]).expect("bell effect applies");

        assert_eq!(renderer.bells(), 1);
    }

    #[test]
    fn g04_approval_panel_shows_diff() {
        let backend = ScriptedBackend::new(Vec::new());
        let mut renderer = ScriptedRenderer::open(backend, 100, 40).expect("renderer opens");
        let mut asking = state(AppPhase::WaitingApproval);
        asking.modal = Some(crate::interactive::events::Modal::Approval {
            request_id: "req-g04".to_owned(),
            action: "edit_file".to_owned(),
            summary: format!(
                "path=src/parser.rs\n[diff]\n{}",
                (0..18)
                    .map(|index| format!(" line {index}"))
                    .chain(["-old".to_owned(), "+new-tail".to_owned()])
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            workspace: "C:/work/project".to_owned(),
            scope: "once".to_owned(),
            expires_at: std::time::Instant::now() + Duration::from_mins(5),
            read_only: false,
            scroll: 0,
        });
        renderer.draw_state(&asking).expect("frame draws");
        let painted = renderer.painted().join("\n");
        assert!(
            painted.contains("[diff]"),
            "diff heading is visible: {painted}"
        );
        assert!(
            painted.lines().any(|line| line.contains(" line 0")),
            "diff context is laid out as real rows: {painted:?}"
        );
        if let Some(crate::interactive::events::Modal::Approval { scroll, .. }) = &mut asking.modal
        {
            *scroll = usize::MAX;
        }
        renderer.draw_state(&asking).expect("scrolled frame draws");
        let painted = renderer.painted().join("\n");
        assert!(
            painted.contains("-old"),
            "removed line is visible: {painted}"
        );
        assert!(
            painted.contains("+new-tail"),
            "added line is visible: {painted}"
        );
        assert!(
            painted.lines().any(|line| line.contains("+new-tail")),
            "the diff line remains a real panel row after scrolling: {painted:?}"
        );
    }

    /// The measured complaint, seen on the screen the user was looking at: a turn of
    /// shell commands asked about each one, and the panel named no key that ended the
    /// questions. This asserts the painted frame: the panel offers `a` and says what
    /// it covers, and once the gate is open the status row says so for the rest of
    /// the turn - a widened gate is never silent.
    #[test]
    fn the_approval_frame_offers_the_turn_grant_and_the_status_row_shows_it_open() {
        let backend = ScriptedBackend::new(Vec::new());
        let mut renderer = ScriptedRenderer::open(backend, 100, 30).expect("renderer opens");
        let mut asking = state(AppPhase::Running);
        asking.live_text = String::new();
        asking.modal = Some(crate::interactive::events::Modal::Approval {
            request_id: "approval-2-93e218c98fe4".to_owned(),
            action: "RunProcess".to_owned(),
            summary: "run git log -1 --stat --format=fuller".to_owned(),
            workspace: "C:/Users/duong/orca/projects/harness-agents".to_owned(),
            scope: "one action, this turn only".to_owned(),
            expires_at: std::time::Instant::now() + Duration::from_mins(5),
            read_only: false,
            scroll: 0,
        });
        renderer.draw_state(&asking).expect("frame draws");
        let painted = renderer.painted().join("\n");
        assert!(
            painted.contains("run git log -1 --stat --format=fuller"),
            "the panel names the command: {painted}"
        );
        assert!(
            painted.contains("a cho phép mọi thao tác trong lượt này"),
            "the panel offers the key that ends the questions: {painted}"
        );
        assert!(
            painted.contains("a cả lượt"),
            "and the editor's rule names the same key: {painted}"
        );

        // The answer is given: the panel closes, and the status row carries the state
        // for the rest of the turn.
        let mut granted = state(AppPhase::Running);
        granted.live_text = String::new();
        granted.granted_for_run = true;
        renderer.draw_state(&granted).expect("frame draws");
        let painted = renderer.painted().join("\n");
        assert!(
            painted.contains("tự động cả lượt"),
            "an open gate is state the operator can see: {painted}"
        );
        assert!(
            !painted.contains("panel duyệt đang chờ"),
            "and no panel is up while it is open: {painted}"
        );
    }

    /// The whole loop runs against the scripted backend, so the TUI path itself is
    /// covered by a unit test instead of only by a console run.
    #[test]
    fn t02_the_loop_boots_paints_and_exits_through_the_tui_path() {
        use crate::interactive::bootstrap::{self, LaunchRequest};
        use crate::interactive::controller::InteractiveController;
        use crate::interactive::events::Key;
        use crate::interactive::paths::{HostPlatform, LaunchEnvironment};
        use crate::interactive::service::{FixtureService, SessionChannel};

        let temp = tempfile::tempdir().expect("temp root");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(home.join("config.toml"), "schema_version = 1\n").expect("config");
        let context = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project,
            platform: HostPlatform::current(),
            environment: LaunchEnvironment::from_pairs([
                ("HA_HOME", home.to_string_lossy().into_owned()),
                ("DEEPSEEK_API_KEY", "fixture-secret".to_owned()),
            ]),
            explicit_data_dir: None,
        })
        .expect("context");
        let channel = SessionChannel::new();
        let events = channel.sender();
        let mut controller = InteractiveController::new(
            &context,
            Box::new(FixtureService::new(events)),
            channel,
            false,
        );
        // A scripted key stream: type a request, submit it, then leave.
        let mut keys: Vec<Key> = "hi".chars().map(Key::Char).collect();
        keys.push(Key::Enter);
        keys.push(Key::EndOfInput);
        let backend = ScriptedBackend::new(keys);
        let renderer = ScriptedRenderer::open(backend, 100, 30).expect("renderer");
        let outcome = super::run_loop(renderer, &mut controller, None).expect("loop runs");
        assert_eq!(outcome.code, 0);
        let renderer = outcome.renderer;

        let output = renderer.backend().output().to_owned();
        assert!(output.contains("Harness Agents"), "{output}");
        assert!(
            output.contains("fixture answer for: hi"),
            "the streamed answer reached the scrollback: {output}"
        );
        assert!(output.contains("✓ done"), "{output}");
        assert!(
            renderer.painted().join("\n").contains('>'),
            "the last frame keeps the composer marker"
        );
    }

    /// K04: a long answer keeps its END in the viewport and its start in the
    /// scrollback.
    ///
    /// The report was a long reply that visibly stopped mid-sentence while the run
    /// finished normally. The text is not lost - it arrives whole and the scrollback
    /// keeps every line - but a viewport that grows with the answer would push its
    /// own last lines off the screen, and that reads as truncation. This asserts the
    /// split: the live block holds the tail and stays bounded, the scrollback holds
    /// what scrolled out, and a burst of lines in one delta does not outrun it.
    #[test]
    fn k04_a_long_streamed_answer_keeps_its_end_visible_and_its_start_in_scrollback() {
        use crate::interactive::bootstrap::{self, LaunchRequest};
        use crate::interactive::controller::InteractiveController;
        use crate::interactive::events::SessionEvent;
        use crate::interactive::paths::{HostPlatform, LaunchEnvironment};
        use crate::interactive::service::{SessionChannel, SessionPort, SubmitRequest};
        use std::fmt::Write as _;

        /// A port that streams a paragraph in small deltas, like a real model.
        struct LongAnswerPort {
            sender: tokio::sync::mpsc::UnboundedSender<SessionEvent>,
        }

        impl SessionPort for LongAnswerPort {
            fn label(&self) -> String {
                "long-answer fixture".to_owned()
            }

            fn cancel(&mut self) {}

            fn submit(&mut self, request: SubmitRequest) {
                let _ = self.sender.send(SessionEvent::Accepted {
                    input_id: request.input_id,
                });
                let _ = self.sender.send(SessionEvent::StepStarted { step: 1 });
                // Text is driven by the test, one delta per pump, because that is how
                // a real stream arrives: a port that sends every delta inside `submit`
                // delivers them as one batch, which collapses the boundary this test
                // exists to check.
            }
        }

        let temp = tempfile::tempdir().expect("temp root");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(home.join("config.toml"), "schema_version = 1\n").expect("config");
        let context = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project,
            platform: HostPlatform::current(),
            environment: LaunchEnvironment::from_pairs([
                ("HA_HOME", home.to_string_lossy().into_owned()),
                ("DEEPSEEK_API_KEY", "fixture-secret".to_owned()),
            ]),
            explicit_data_dir: None,
        })
        .expect("context");
        let channel = SessionChannel::new();
        let events = channel.sender();
        let port = LongAnswerPort {
            sender: events.clone(),
        };
        let mut controller = InteractiveController::new(&context, Box::new(port), channel, false);
        // Drive the controller directly: submitting and pumping is the same path the
        // loop takes, and it leaves the viewport observable after the answer instead
        // of after the app has closed itself.
        controller.boot_lines();
        let _ = controller.handle_key(Key::Char('h'));
        let _ = controller.handle_key(Key::Char('i'));
        let _ = controller.handle_key(Key::Enter);
        let _ = controller.pump_events();

        // One delta per pump, the way a streamed answer actually arrives.
        for index in 0..12 {
            let _ = events.send(SessionEvent::TextDelta {
                text: format!("line {index} of the answer\n"),
            });
            let _ = controller.pump_events();
        }
        let _ = events.send(SessionEvent::TextDelta {
            text: "THE-LAST-LINE-OF-THE-ANSWER".to_owned(),
        });
        let _ = controller.pump_events();

        let state = controller.ui_state();
        assert!(
            state.live_text.contains("THE-LAST-LINE-OF-THE-ANSWER"),
            "the viewport lost the end of the answer: {:?}",
            state.live_text
        );
        assert!(
            !state.live_text.contains("line 0 of the answer"),
            "the live block must stay bounded, not grow with the answer: {:?}",
            state.live_text
        );
        let transcript = controller.transcript().join("\n");
        assert!(
            transcript.contains("line 0 of the answer"),
            "the earliest line was dropped instead of moving to the scrollback: {transcript}"
        );

        // A tool result or a paste arrives as several lines in ONE delta, which is
        // the shape that can outrun a per-poll overflow decision.
        let mut burst = String::new();
        for index in 0..12 {
            let _ = writeln!(burst, "burst {index} of the answer");
        }
        burst.push_str("BURST-LAST-LINE");
        let _ = events.send(SessionEvent::TextDelta { text: burst });
        let _ = controller.pump_events();
        let state = controller.ui_state();
        assert!(
            state.live_text.contains("BURST-LAST-LINE"),
            "a burst of lines pushed the end of the answer out of the viewport: {:?}",
            state.live_text
        );
    }

    /// The measured complaint: typing `/` offered nothing, so the only way to learn
    /// a command was to already know it. This asserts the **painted frame**: the
    /// list is above the composer, the draft stays where it was, the border names
    /// the menu's keys, and accepting a row does not run anything by itself.
    #[test]
    fn slash_typing_a_slash_paints_the_menu_above_the_composer() {
        use crate::interactive::bootstrap::{self, LaunchRequest};
        use crate::interactive::controller::InteractiveController;
        use crate::interactive::events::Key;
        use crate::interactive::paths::{HostPlatform, LaunchEnvironment};
        use crate::interactive::service::{FixtureService, SessionChannel};

        let temp = tempfile::tempdir().expect("temp root");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(home.join("config.toml"), "schema_version = 1\n").expect("config");
        let context = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project,
            platform: HostPlatform::current(),
            environment: LaunchEnvironment::from_pairs([
                ("HA_HOME", home.to_string_lossy().into_owned()),
                ("DEEPSEEK_API_KEY", "fixture-secret".to_owned()),
            ]),
            explicit_data_dir: None,
        })
        .expect("context");
        let channel = SessionChannel::new();
        let events = channel.sender();
        let mut controller = InteractiveController::new(
            &context,
            Box::new(FixtureService::new(events)),
            channel,
            false,
        );
        let _ = controller.boot_lines();
        let backend = ScriptedBackend::new(Vec::new());
        let mut renderer = ScriptedRenderer::open(backend, 100, 30).expect("renderer");

        let _ = controller.handle_key(Key::Char('/'));
        renderer
            .draw_state(&controller.ui_state())
            .expect("frame draws");
        let painted = renderer.painted();
        let text = painted.join("\n");
        assert!(
            text.contains("❯ /model"),
            "the first row is highlighted: {text}"
        );
        assert!(
            text.contains("> /"),
            "the composer keeps the draft while the menu is up: {text}"
        );
        assert!(
            text.contains("Tab/Enter"),
            "the border names the menu's keys, not the composer's: {text}"
        );
        let menu_row = painted
            .iter()
            .position(|row| row.contains("❯ /model"))
            .expect("a menu row");
        let composer_row = painted
            .iter()
            .position(|row| row.starts_with("│ > /"))
            .expect("the composer row");
        assert!(
            menu_row < composer_row,
            "the list belongs above the draft: {text}"
        );

        // The window follows the highlight, and a row shows the argument its
        // command takes, so the line reads as the thing to type.
        let _ = controller.handle_key(Key::Char('a'));
        let _ = controller.handle_key(Key::Char('t'));
        renderer
            .draw_state(&controller.ui_state())
            .expect("frame draws");
        assert!(
            renderer.painted().join("\n").contains("❯ /attach <path>"),
            "{:?}",
            renderer.painted()
        );

        // Narrowing the word narrows the list, and Tab accepts the row it points
        // at - without running the command, which still needs its own Enter.
        let _ = controller.handle_key(Key::EraseToLineStart);
        let _ = controller.handle_key(Key::Char('/'));
        let _ = controller.handle_key(Key::Char('r'));
        let _ = controller.handle_key(Key::Char('e'));
        let _ = controller.handle_key(Key::Char('s'));
        renderer
            .draw_state(&controller.ui_state())
            .expect("frame draws");
        assert!(
            renderer.painted().join("\n").contains("❯ /resume"),
            "{:?}",
            renderer.painted()
        );

        let _ = controller.handle_key(Key::Tab);
        renderer
            .draw_state(&controller.ui_state())
            .expect("frame draws");
        let text = renderer.painted().join("\n");
        assert!(
            text.contains("> /resume"),
            "the accepted command is in the composer: {text}"
        );
        assert!(
            !text.contains("❯ /resume"),
            "a whole command has nothing left to suggest: {text}"
        );
    }

    /// Poll timeouts are deliberately longer than the animation interval. If an
    /// idle tick ever starts returning a redraw effect, this catches the resulting
    /// CPU/output churn instead of relying on a visual inspection.
    #[test]
    fn t05_idle_poll_does_not_redraw() {
        use crate::interactive::bootstrap::{self, LaunchRequest};
        use crate::interactive::controller::InteractiveController;
        use crate::interactive::paths::{HostPlatform, LaunchEnvironment};
        use crate::interactive::service::{FixtureService, SessionChannel};

        #[derive(Default)]
        struct IdleRenderer {
            polls: usize,
            draws: usize,
            finished: bool,
        }

        impl TuiRenderer for IdleRenderer {
            fn columns(&self) -> u16 {
                100
            }

            fn rows(&self) -> u16 {
                30
            }

            fn poll_key(&mut self, _timeout: Duration) -> io::Result<Option<Key>> {
                self.polls += 1;
                std::thread::sleep(Duration::from_millis(60));
                Ok((self.polls >= 3).then_some(Key::EndOfInput))
            }

            fn draw_state(&mut self, _state: &UiState) -> io::Result<()> {
                self.draws += 1;
                Ok(())
            }

            fn insert_history(&mut self, _item: &HistoryItem) -> io::Result<()> {
                Ok(())
            }

            fn finish(&mut self) -> io::Result<()> {
                self.finished = true;
                Ok(())
            }
        }

        let temp = tempfile::tempdir().expect("temp root");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(home.join("config.toml"), "schema_version = 1\n").expect("config");
        let context = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project,
            platform: HostPlatform::current(),
            environment: LaunchEnvironment::from_pairs([
                ("HA_HOME", home.to_string_lossy().into_owned()),
                ("DEEPSEEK_API_KEY", "fixture-secret".to_owned()),
            ]),
            explicit_data_dir: None,
        })
        .expect("context");
        let channel = SessionChannel::new();
        let events = channel.sender();
        let mut controller = InteractiveController::new(
            &context,
            Box::new(FixtureService::new(events)),
            channel,
            false,
        );
        let outcome = super::run_loop(IdleRenderer::default(), &mut controller, None)
            .expect("idle loop exits");

        assert_eq!(outcome.code, 0);
        assert_eq!(outcome.renderer.polls, 3);
        assert_eq!(
            outcome.renderer.draws, 1,
            "only the initial frame is painted while ready"
        );
        assert!(outcome.renderer.finished);
    }

    fn screen(renderer: &super::RealRenderer<ratatui::backend::TestBackend>) -> Vec<String> {
        let buffer = renderer.terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .filter_map(|x| buffer.cell((x, y)).map(ratatui::buffer::Cell::symbol))
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    /// Ctrl+O and a resize redraw the visible screen only, as prime-agent does:
    /// the newest rows fill the screen above the viewport and one input box sits
    /// at the bottom. The user saw the view jump while thousands of rows were
    /// pushed again, and, after the terminal was resized, a broken second input
    /// box left on screen above the real one.
    #[test]
    fn a_repaint_draws_only_the_screen_with_one_input_box_at_the_bottom() {
        let theme = super::theme::Theme::plain();
        let mut renderer =
            super::RealRenderer::open_with(ratatui::backend::TestBackend::new(60, 20), &theme)
                .expect("renderer opens");
        let items = (0..200)
            .map(|index| HistoryItem::Notice {
                message: format!("row {index}"),
            })
            .collect::<Vec<_>>();
        renderer
            .insert_history_batch(&items)
            .expect("history goes in");
        let mut ready = state(AppPhase::Ready);
        ready.live_text.clear();
        renderer.draw_state(&ready).expect("frame draws");

        renderer.terminal.backend_mut().resize(90, 20);
        renderer
            .repaint_screen(crate::interactive::events::Detail::default())
            .expect("screen repaints");
        renderer.draw_state(&ready).expect("frame draws");

        let rows = screen(&renderer);
        let joined = rows.join(
            "
",
        );
        assert_eq!(
            joined.matches("sửa lỗi").count(),
            1,
            "one input box on screen:
{joined}"
        );
        let above = usize::from(20 - super::viewport_rows(20));
        assert!(
            rows[above - 1].contains("row 199"),
            "the newest row sits right above the viewport:
{joined}"
        );
        assert!(
            rows[..above].iter().all(|row| row.contains("row ")),
            "history fills the screen above the viewport:
{joined}"
        );
        assert!(
            !joined.contains("row 150"),
            "only what fits on screen is drawn again:
{joined}"
        );
    }

    /// A banner still on screen names the model now in use: retitle replaces what the
    /// renderer remembers of it and draws the screen again, once, with no copy of
    /// the old one left behind.
    #[test]
    fn a_retitled_banner_replaces_the_old_one_on_screen() {
        let theme = super::theme::Theme::plain();
        let mut renderer =
            super::RealRenderer::open_with(ratatui::backend::TestBackend::new(80, 20), &theme)
                .expect("renderer opens");
        let lines = |model: &str, level: &str| {
            vec![
                String::new(),
                "Harness Agents 0.1.6".to_owned(),
                "Project: C:/work/demo".to_owned(),
                format!("Service: {model} via https://example.test"),
                "Permissions: ask".to_owned(),
                format!("Thinking: {level}"),
            ]
        };
        renderer
            .insert_history(&HistoryItem::Banner {
                lines: lines("first-model", "low"),
            })
            .expect("banner goes in");
        let mut ready = state(AppPhase::Ready);
        ready.live_text.clear();
        renderer.draw_state(&ready).expect("frame draws");
        let before = screen(&renderer).join("\n");
        assert!(
            before.contains("first-model") && before.contains("✧ low"),
            "{before}"
        );

        renderer
            .retitle(&lines("second-model", "high"))
            .expect("banner is retitled");
        renderer.draw_state(&ready).expect("frame draws");
        let after = screen(&renderer).join("\n");
        assert!(
            after.contains("second-model") && after.contains("✧ high"),
            "{after}"
        );
        assert!(
            !after.contains("first-model") && !after.contains("✧ low"),
            "no copy of the old banner is left: {after}"
        );
        assert_eq!(after.matches("second-model").count(), 1, "{after}");
    }

    /// A terminal dragged to another size re-wraps what it shows. Whatever the
    /// order its events arrive in, the next paint must notice the new size and
    /// repaint the screen, not draw a frame over the re-wrapped old one: the user
    /// saw two input boxes a row apart, at two widths, after dragging a window.
    #[test]
    fn a_paint_at_a_new_size_repaints_the_screen_by_itself() {
        let theme = super::theme::Theme::plain();
        let mut renderer =
            super::RealRenderer::open_with(ratatui::backend::TestBackend::new(60, 20), &theme)
                .expect("renderer opens");
        let items = (0..200)
            .map(|index| HistoryItem::Notice {
                message: format!("row {index}"),
            })
            .collect::<Vec<_>>();
        renderer
            .insert_history_batch(&items)
            .expect("history goes in");
        let mut ready = state(AppPhase::Ready);
        ready.live_text.clear();
        renderer.draw_state(&ready).expect("frame draws");

        // Taller and wider, with no resize event and no explicit repaint.
        renderer.terminal.backend_mut().resize(100, 34);
        renderer.draw_state(&ready).expect("frame draws");
        let rows = screen(&renderer);
        let joined = rows.join("\n");
        assert_eq!(
            joined.matches("sửa lỗi").count(),
            1,
            "one input box on screen:\n{joined}"
        );

        // Then narrower and shorter, and a history row arrives before the frame.
        renderer.terminal.backend_mut().resize(50, 16);
        renderer
            .insert_history(&HistoryItem::Notice {
                message: "row 200".to_owned(),
            })
            .expect("history goes in");
        renderer.draw_state(&ready).expect("frame draws");
        let rows = screen(&renderer);
        let joined = rows.join("\n");
        assert_eq!(
            joined.matches("sửa lỗi").count(),
            1,
            "still one input box:\n{joined}"
        );
        assert!(
            joined.contains("row 200"),
            "the new row is shown:\n{joined}"
        );
    }
}
