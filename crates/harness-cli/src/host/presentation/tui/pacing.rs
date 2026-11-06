//! Frame pacing: how often the loop looks for work while a run streams, how the
//! answer still streaming is revealed, and the clock the working line's
//! animation runs on.
//!
//! Measured on 0.9.2 (`HA_FRAME_LOG`, a 600-word answer on `gpt-6-luna`): a frame
//! cost 2-4 ms, yet frames came 62 ms apart at the median and 125 ms at p90, and
//! the answer arrived in about 40 jumps - the loop waited up to 50 ms for a key
//! before it took the worker's frames, and showed each burst the provider sent
//! as it came. The loop now looks every [`FAST_POLL`] while a run is active
//! (an idle app still draws nothing), and [`Reveal`] lets the text flow out at
//! a pace that follows how far behind it is.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::interactive::events::UiState;

/// How long one loop iteration waits for a key while a run is active or the
/// answer is still being revealed: a frame about every 8-16 ms.
pub const FAST_POLL: Duration = Duration::from_millis(8);

/// How often the working line's animation (spinner, shimmer) is redrawn while a
/// run is active and nothing else asked for a frame: about 30 a second.
pub const ANIMATION_EVERY: Duration = Duration::from_millis(33);

/// The slowest the answer is revealed, in characters a second: above what a
/// model writes (a hundred tokens a second is about 400 characters), so a
/// model that trickles is shown as it comes and the reveal never lags it.
const MIN_RATE: f64 = 500.0;

/// How long the text waiting to be shown should take to come out: the rate
/// rises with the backlog, so a burst of a hundred characters flows out over
/// this time instead of appearing at once, and the reveal never falls far
/// behind the model.
const CATCH_UP: f64 = 0.18;

/// A backlog this long is shown at once: a block pasted whole (a restored
/// answer, a long table row) is not typed out.
const JUMP_AT: usize = 4_000;

/// The process's animation clock.
fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Milliseconds on the animation clock.
#[must_use]
pub fn clock_ms() -> u64 {
    u64::try_from(epoch().elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The spinner's frame on the animation clock: one every 80 ms.
#[must_use]
pub fn spinner_frame() -> u64 {
    clock_ms() / 80
}

/// The answer text the live block shows while it streams.
#[derive(Debug)]
pub struct Reveal {
    /// Characters of the live text already shown.
    shown: usize,
    /// When the reveal last moved.
    last: Instant,
}

impl Default for Reveal {
    fn default() -> Self {
        Self {
            shown: 0,
            last: Instant::now(),
        }
    }
}

impl Reveal {
    /// The first `chars` characters of the live text went to the scrollback:
    /// what is still shown starts that much earlier.
    pub fn committed(&mut self, chars: usize) {
        self.shown = self.shown.saturating_sub(chars);
    }

    /// Whether the live text holds more than is shown.
    #[cfg(test)]
    #[must_use]
    pub fn behind(&self, state: &UiState) -> bool {
        self.shown < state.live_text.chars().count()
    }

    /// Move the reveal on for a frame drawn now, and the state that frame
    /// shows: `None` when it shows `state` as it is (all of the live text is
    /// out), else a copy whose live text stops where the reveal is.
    pub fn frame(&mut self, state: &UiState) -> Option<UiState> {
        self.frame_at(state, Instant::now())
    }

    fn frame_at(&mut self, state: &UiState, now: Instant) -> Option<UiState> {
        let target = state.live_text.chars().count();
        let elapsed = now.saturating_duration_since(self.last);
        self.last = now;
        if target == 0 || self.shown >= target {
            self.shown = target;
            return None;
        }
        let backlog = target - self.shown;
        if backlog >= JUMP_AT {
            self.shown = target;
            return None;
        }
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a backlog under JUMP_AT characters and a frame's share of it"
        )]
        let step = {
            let rate = MIN_RATE.max(backlog as f64 / CATCH_UP);
            // A frame after a long pause moves at most what one at the fast
            // pace would, so the first frame of a burst does not jump.
            let seconds = elapsed.as_secs_f64().min(0.05);
            ((rate * seconds).ceil() as usize).max(1)
        };
        self.shown = (self.shown + step).min(target);
        if self.shown >= target {
            return None;
        }
        let mut shown = state.clone();
        let end = state
            .live_text
            .char_indices()
            .nth(self.shown)
            .map_or(state.live_text.len(), |(index, _)| index);
        shown.live_text.truncate(end);
        Some(shown)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::Reveal;
    use crate::interactive::events::UiState;

    fn live(text: &str) -> UiState {
        UiState {
            phase: crate::interactive::events::AppPhase::Running,
            setup_required: false,
            setup_hint: None,
            header: Vec::new(),
            buffer: String::new(),
            cursor: 0,
            live_text: text.to_owned(),
            open_tools: Vec::new(),
            live_tool_output: Vec::new(),
            child_activity: Vec::new(),
            activity: None,
            modal: None,
            granted_for_run: false,
            queued_input: false,
            queued_count: 0,
            queued_previews: Vec::new(),
            writing_code: false,
            provider_wait: None,
            last_request: None,
            run_started_at: None,
            last_run_elapsed: Duration::ZERO,
            steps: 0,
            max_steps: 8,
            tool_calls: 0,
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

    /// A burst the provider sent whole flows out over several frames.
    #[test]
    fn a_burst_is_revealed_over_several_frames_and_then_whole() {
        let mut reveal = Reveal::default();
        let start = Instant::now();
        let burst = "word ".repeat(30);
        let mut at = start;
        let mut frames = Vec::new();
        for _ in 0..40 {
            at += Duration::from_millis(16);
            let shown = reveal.frame_at(&live(&burst), at);
            frames.push(shown.map_or(burst.len(), |state| state.live_text.len()));
        }
        assert!(
            frames[0] < burst.len() / 4,
            "the first frame shows a little: {frames:?}"
        );
        assert!(
            frames.windows(2).all(|pair| pair[0] <= pair[1]),
            "{frames:?}"
        );
        let whole = frames.iter().position(|shown| *shown == burst.len());
        assert!(
            whole.is_some_and(|frame| frame <= 20),
            "the burst is out within about a third of a second: {frames:?}"
        );
    }

    /// Text that went to the scrollback leaves the live block's start, and the
    /// reveal keeps its place in what is left.
    #[test]
    fn a_commit_moves_the_reveal_back_by_what_left() {
        let mut reveal = Reveal::default();
        let start = Instant::now();
        let _ = reveal.frame_at(&live("first line\n"), start + Duration::from_secs(1));
        let _ = reveal.frame_at(&live("first line\n"), start + Duration::from_secs(2));
        assert!(!reveal.behind(&live("first line\n")));
        reveal.committed("first line\n".chars().count());
        assert!(reveal.behind(&live("second")));
        assert_eq!(reveal.shown, 0);
    }

    #[test]
    fn nothing_live_is_drawn_as_it_is() {
        let mut reveal = Reveal::default();
        assert!(reveal.frame(&live("")).is_none());
        assert!(!reveal.behind(&live("")));
    }

    /// A backlog too long to type out is shown at once.
    #[test]
    fn a_huge_backlog_is_shown_whole() {
        let mut reveal = Reveal::default();
        assert!(reveal.frame(&live(&"x".repeat(5_000))).is_none());
    }

    /// Never cuts inside a character.
    #[test]
    fn the_cut_falls_between_characters() {
        let mut reveal = Reveal::default();
        let start = Instant::now();
        let text = "Tiếng Việt có dấu ".repeat(10);
        let shown = reveal.frame_at(&live(&text), start + Duration::from_millis(16));
        let shown = shown.expect("behind");
        assert!(text.starts_with(&shown.live_text));
    }
}
