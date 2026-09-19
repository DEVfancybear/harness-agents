//! Palette and glyphs for the TUI.
//!
//! Only the sixteen standard colours are used, so the TUI looks right in a
//! default console and never asks for true colour. `NO_COLOR` or `TERM=dumb`
//! selects [`Theme::plain`], which keeps the layout boxes but drops every SGR
//! colour attribute - that is what the T07 `NO_COLOR` case asserts on.

use ratatui::style::{Color, Modifier, Style};

/// One palette for the whole viewport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Theme {
    /// Whether colour attributes may be emitted at all.
    pub color: bool,
    pub accent: Style,
    pub user: Style,
    pub assistant: Style,
    pub tool_ok: Style,
    pub tool_failed: Style,
    pub dim: Style,
    pub error: Style,
    pub border: Style,
    pub title: Style,
}

impl Theme {
    /// The coloured theme.
    #[must_use]
    pub const fn colored() -> Self {
        Self {
            color: true,
            accent: Style::new().fg(Color::Cyan),
            user: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            assistant: Style::new(),
            tool_ok: Style::new().fg(Color::Green),
            tool_failed: Style::new().fg(Color::Red),
            dim: Style::new().fg(Color::DarkGray),
            error: Style::new().fg(Color::Red),
            border: Style::new().fg(Color::DarkGray),
            title: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        }
    }

    /// The colourless theme: identical layout, no colour attributes.
    #[must_use]
    pub const fn plain() -> Self {
        Self {
            color: false,
            accent: Style::new(),
            user: Style::new().add_modifier(Modifier::BOLD),
            assistant: Style::new(),
            tool_ok: Style::new(),
            tool_failed: Style::new().add_modifier(Modifier::BOLD),
            dim: Style::new(),
            error: Style::new().add_modifier(Modifier::BOLD),
            border: Style::new(),
            title: Style::new().add_modifier(Modifier::BOLD),
        }
    }

    /// Choose the theme from the environment.
    ///
    /// `NO_COLOR` follows the convention that any non-empty value disables
    /// colour; `TERM=dumb` means the terminal cannot be trusted with attributes.
    #[must_use]
    pub fn detect() -> Self {
        Self::from_pairs(
            std::env::var("NO_COLOR").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
    }

    /// The detection rule, injectable so it can be unit tested.
    #[must_use]
    pub fn from_pairs(no_color: Option<&str>, term: Option<&str>) -> Self {
        let no_color = no_color.is_some_and(|value| !value.is_empty());
        let dumb = term == Some("dumb");
        if no_color || dumb {
            Self::plain()
        } else {
            Self::colored()
        }
    }

    /// The spinner frame for a tick counter.
    #[must_use]
    pub const fn spinner(tick: u64) -> &'static str {
        const FRAMES: [&str; 4] = ["⠋", "⠙", "⠹", "⠸"];
        // The modulo bounds the value to the frame count, so the cast is exact.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded by the frame count"
        )]
        let index = (tick % FRAMES.len() as u64) as usize;
        FRAMES[index]
    }

    /// The style for one banner row.
    ///
    /// The app name is the only row that is emphasised; the rest of the header is
    /// information the user reads once.
    #[must_use]
    pub fn banner(&self, line: &str) -> Style {
        if line.starts_with("Harness Agents") {
            self.title
        } else {
            self.dim
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::detect()
    }
}

#[cfg(test)]
mod tests {
    use super::Theme;

    #[test]
    fn t05_no_color_and_dumb_terminals_drop_the_palette() {
        assert!(Theme::from_pairs(None, Some("xterm-256color")).color);
        assert!(!Theme::from_pairs(Some("1"), None).color);
        assert!(!Theme::from_pairs(Some("anything"), None).color);
        assert!(
            !Theme::from_pairs(None, Some("dumb")).color,
            "TERM=dumb disables colour too"
        );
        assert!(
            Theme::from_pairs(Some(""), None).color,
            "an empty NO_COLOR does not disable colour"
        );
    }

    #[test]
    fn t05_the_spinner_cycles_through_its_frames() {
        let frames: Vec<&str> = (0..4).map(Theme::spinner).collect();
        assert_eq!(frames, vec!["⠋", "⠙", "⠹", "⠸"]);
        assert_eq!(Theme::spinner(4), Theme::spinner(0), "it wraps");
    }
}
