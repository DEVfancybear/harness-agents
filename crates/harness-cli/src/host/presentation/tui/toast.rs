//! prime-agent's ephemeral action toasts (`pa-tui/src/toast`): short-lived
//! confirmations - clipboard copies - shown as right-aligned pills over the
//! top rows of the frame for three seconds, instead of rows the transcript
//! keeps. A repeat of a toast still on screen coalesces into it ("(x3)");
//! at most three stack, the oldest dropping first.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::theme::Theme;

/// How long a toast stays on screen.
pub const TOAST_TTL: Duration = Duration::from_secs(3);

/// How many distinct toasts stack at once.
pub const TOAST_STACK_LIMIT: usize = 3;

#[derive(Debug)]
struct Toast {
    text: String,
    repeats: usize,
    expires_at: Instant,
}

impl Toast {
    fn label(&self) -> String {
        if self.repeats > 1 {
            format!("{} (x{})", self.text, self.repeats)
        } else {
            self.text.clone()
        }
    }
}

/// The toast stack, oldest first.
#[derive(Debug, Default)]
pub struct Toasts {
    entries: Vec<Toast>,
}

impl Toasts {
    /// Show a toast; a repeat of one still on screen coalesces into it.
    pub fn push(&mut self, text: impl Into<String>) {
        let text = text.into();
        let now = Instant::now();
        if let Some(index) = self
            .entries
            .iter()
            .rposition(|toast| toast.text == text && toast.expires_at > now)
        {
            let mut toast = self.entries.remove(index);
            toast.expires_at = now + TOAST_TTL;
            toast.repeats += 1;
            self.entries.push(toast);
        } else {
            self.entries.push(Toast {
                text,
                repeats: 1,
                expires_at: now + TOAST_TTL,
            });
        }
        while self.entries.len() > TOAST_STACK_LIMIT {
            self.entries.remove(0);
        }
    }

    /// Drop the toasts whose time passed; `true` when any went.
    pub fn prune_expired(&mut self, now: Instant) -> bool {
        let before = self.entries.len();
        self.entries.retain(|toast| toast.expires_at > now);
        before != self.entries.len()
    }

    /// The labels on screen, oldest first.
    #[must_use]
    pub fn active(&self, now: Instant) -> Vec<String> {
        self.entries
            .iter()
            .filter(|toast| toast.expires_at > now)
            .map(Toast::label)
            .collect()
    }
}

static TOASTS: Mutex<Toasts> = Mutex::new(Toasts {
    entries: Vec::new(),
});

/// Show a toast in this terminal.
pub fn push(text: impl Into<String>) {
    if let Ok(mut toasts) = TOASTS.lock() {
        toasts.push(text);
    }
}

/// Drop expired toasts; `true` when the frame must be drawn again.
pub fn prune_expired() -> bool {
    TOASTS
        .lock()
        .is_ok_and(|mut toasts| toasts.prune_expired(Instant::now()))
}

/// Paint the toasts as pills at the right edge of `area`'s top rows; when the
/// area is shorter than the stack the newest stay.
pub fn render(frame: &mut Frame, area: Rect, theme: &Theme) {
    let labels = TOASTS
        .lock()
        .map(|toasts| toasts.active(Instant::now()))
        .unwrap_or_default();
    let capacity = usize::from(area.height);
    let skip = labels.len().saturating_sub(capacity);
    for (offset, label) in labels.iter().skip(skip).enumerate() {
        let pill = format!(" {label} ");
        let width = u16::try_from(super::widgets::composer::display_width(&pill))
            .unwrap_or(u16::MAX)
            .min(area.width);
        let row = Rect::new(
            area.x + area.width - width,
            area.y + u16::try_from(offset).unwrap_or(0),
            width,
            1,
        );
        frame.render_widget(Paragraph::new(Span::styled(pill, theme.selection)), row);
    }
}

#[cfg(test)]
mod tests {
    use super::Toasts;
    use std::time::{Duration, Instant};

    #[test]
    fn repeats_coalesce_and_the_stack_is_bounded() {
        let mut toasts = Toasts::default();
        toasts.push("Copied selection to clipboard");
        toasts.push("Copied selection to clipboard");
        toasts.push("Copied selection to clipboard");
        assert_eq!(
            toasts.active(Instant::now()),
            ["Copied selection to clipboard (x3)"]
        );
        for text in ["a", "b", "c"] {
            toasts.push(text);
        }
        assert_eq!(toasts.active(Instant::now()), ["a", "b", "c"]);
        assert!(toasts.prune_expired(Instant::now() + Duration::from_secs(4)));
        assert!(toasts.active(Instant::now()).is_empty());
    }
}
