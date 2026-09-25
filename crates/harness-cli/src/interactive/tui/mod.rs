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

pub mod history;
pub mod layout;
pub mod markdown;
pub mod theme;
pub mod widgets;

use std::io;
use std::time::{Duration, Instant};

use harness_types::{ErrorCode, HarnessError};
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use ratatui::{Terminal, TerminalOptions, Viewport};

use self::theme::Theme;
use super::controller::{Effect, InteractiveController};
use super::events::{HistoryItem, Key, UiState};
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
    /// Clear the screen and draw every row again in this detail mode, as
    /// prime-agent re-renders its chat when ctrl+o changes what rows show.
    fn reprint(&mut self, _detail: super::events::Detail) -> io::Result<()> {
        Ok(())
    }
    /// Erase the viewport footprint and leave the cursor on a fresh line.
    fn finish(&mut self) -> io::Result<()>;
}

/// How many history entries a renderer keeps for ctrl+o to draw again.
const REPRINT_ENTRIES: usize = 4000;

/// The terminal-side half of a renderer: a ratatui `Terminal` over some backend.
pub struct RealRenderer<B: Backend> {
    terminal: Terminal<B>,
    theme: Theme,
    /// The detail mode of the last frame, which history rows are drawn in.
    detail: super::events::Detail,
    /// What was pushed into the scrollback, newest last, for a reprint.
    shown: std::collections::VecDeque<HistoryItem>,
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
        let size = backend.size().map_err(to_io)?;
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(viewport_rows(size.height)),
            },
        )
        .map_err(to_io)?;
        Ok(Self {
            terminal,
            theme: Theme::detect(),
            detail: super::events::Detail::default(),
            shown: std::collections::VecDeque::new(),
        })
    }

    /// Draw every remembered entry again, in `detail`.
    fn replay(&mut self, detail: super::events::Detail) -> io::Result<()> {
        self.detail = detail;
        self.terminal.clear().map_err(to_io)?;
        let items = self.shown.iter().cloned().collect::<Vec<_>>();
        for item in &items {
            self.draw_history(item)?;
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

    fn insert_history(&mut self, item: &HistoryItem) -> io::Result<()> {
        if self.shown.len() == REPRINT_ENTRIES {
            self.shown.pop_front();
        }
        self.shown.push_back(item.clone());
        self.draw_history(item)
    }

    fn draw_history(&mut self, item: &HistoryItem) -> io::Result<()> {
        let theme = self.theme;
        let width = self.columns();
        let rows = history::render(item, width, &theme, self.detail);
        if rows.is_empty() {
            return Ok(());
        }
        let height = u16::try_from(rows.len()).unwrap_or(u16::MAX);
        self.terminal
            .insert_before(height, |buffer| {
                let area = buffer.area;
                for (index, line) in rows.iter().enumerate() {
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
            .map_err(to_io)
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

/// The real renderer: keys through the host backend, frames through ratatui.
///
/// It owns the host backend, so the loop reads keys through the same object that
/// draws: there is only ever one reader of the console.
pub struct RuntimeRenderer<T: TerminalBackend> {
    backend: T,
    inner: RealRenderer<ratatui::backend::CrosstermBackend<io::Stdout>>,
}

impl<T: TerminalBackend> RuntimeRenderer<T> {
    /// Open the viewport on the real console.
    ///
    /// # Errors
    /// Fails when the console cannot be reached or measured.
    #[allow(dead_code, reason = "T07 opens the viewport from the fallback probe")]
    pub fn open(backend: T) -> io::Result<Self> {
        Ok(Self {
            backend,
            inner: RealRenderer::open(ratatui::backend::CrosstermBackend::new(io::stdout()))?,
        })
    }
}

impl<T: TerminalBackend> TuiRenderer for RuntimeRenderer<T> {
    fn columns(&self) -> u16 {
        self.inner.columns()
    }

    fn rows(&self) -> u16 {
        self.inner.rows()
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
        self.inner.draw_state(state)
    }

    fn insert_history(&mut self, item: &HistoryItem) -> io::Result<()> {
        self.inner.insert_history(item)
    }

    fn clear_viewport(&mut self) -> io::Result<()> {
        self.inner.clear_viewport()
    }

    fn reprint(&mut self, detail: super::events::Detail) -> io::Result<()> {
        // The rows already in the scrollback were drawn in the old mode; erase the
        // screen and the scrollback, then draw them all again.
        self.backend.write("\x1b[2J\x1b[3J\x1b[H")?;
        self.backend.flush()?;
        self.inner.replay(detail)
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
        self.inner.finish()?;
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
        self.inner.replay(detail)
    }

    fn insert_history(&mut self, item: &HistoryItem) -> io::Result<()> {
        let width = self.inner.columns();
        let rows = history::render(item, width, &self.inner.theme, self.inner.detail);
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
    controller: &mut InteractiveController,
    notice: Option<&str>,
) -> Result<u8, HarnessError> {
    let renderer = RuntimeRenderer::open(backend).map_err(|error| terminal_error(&error))?;
    run_loop(renderer, controller, notice).map(|outcome| outcome.code)
}

/// The loop, generic over the renderer so the tests drive the same code.
///
/// # Errors
/// A terminal failure propagates as a typed error.
pub fn run_loop<R: TuiRenderer>(
    mut renderer: R,
    controller: &mut InteractiveController,
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
    controller: &mut InteractiveController,
    notice: Option<&str>,
) -> Result<u8, HarnessError> {
    if let Some(source) = notice {
        controller
            .resume_source(source)
            .map_err(|message| HarnessError::new(ErrorCode::InvalidPayload, message))?;
    }
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
    loop {
        let mut redraw = false;
        if let Some(key) = renderer
            .poll_key(POLL_INTERVAL)
            .map_err(|error| terminal_error(&error))?
        {
            if matches!(key, Key::Resize { .. }) {
                // The viewport height cannot change (T01), so a resize only
                // repaints the layout at the new size; the draft is untouched.
                redraw = true;
            } else {
                let effects = controller.handle_key(key);
                redraw = !effects.is_empty();
                if let Step::Exit(code) = apply(renderer, effects)? {
                    return Ok(code);
                }
            }
        }
        let effects = controller.pump_events();
        redraw = redraw || !effects.is_empty();
        if let Step::Exit(code) = apply(renderer, effects)? {
            return Ok(code);
        }

        // The spinner and the clock only need repainting while something runs: an
        // idle app returns no effect, so it never draws (acceptance U06).
        if last_tick.elapsed() >= TICK_INTERVAL {
            last_tick = Instant::now();
            let effects = controller.tick();
            redraw = redraw || !effects.is_empty();
            if let Step::Exit(code) = apply(renderer, effects)? {
                return Ok(code);
            }
        }
        if redraw {
            renderer
                .draw_state(&controller.ui_state())
                .map_err(|error| terminal_error(&error))?;
        }
    }
}

/// Apply one batch of effects.
fn apply(renderer: &mut impl TuiRenderer, effects: Vec<Effect>) -> Result<Step, HarnessError> {
    for effect in effects {
        match effect {
            Effect::History(item) => renderer
                .insert_history(&item)
                .map_err(|error| terminal_error(&error))?,
            // Streamed text is committed to the scrollback as it is flushed: the
            // live block already showed it, and the history renderer styles it.
            Effect::Stream(text) => renderer
                .insert_history(&HistoryItem::Assistant { text })
                .map_err(|error| terminal_error(&error))?,
            Effect::Thinking(text) => renderer
                .insert_history(&HistoryItem::Thinking { text })
                .map_err(|error| terminal_error(&error))?,
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
            Effect::Redraw => {}
            Effect::Exit(code) => {
                renderer.finish().map_err(|error| terminal_error(&error))?;
                return Ok(Step::Exit(code));
            }
        }
    }
    Ok(Step::Continue)
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
mod tests {
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
        }
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
        assert!(output.contains("done · "), "{output}");
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
            .position(|row| row.starts_with("> /"))
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
}
