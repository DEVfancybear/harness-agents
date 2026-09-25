//! Palette and glyphs for the TUI: prime-agent's `prime` theme.
//!
//! Ported from prime-agent's `modes/interactive/theme/prime.json` and `theme.ts`: a
//! dark palette in true colour, quantised to the 256-colour cube when the terminal
//! does not say it renders true colour (`COLORTERM=truecolor|24bit`, Windows
//! Terminal's `WT_SESSION`, and the terminals that set `TERM_PROGRAM`). `NO_COLOR` or
//! `TERM=dumb` selects [`Theme::plain`], which keeps every glyph and the layout but
//! drops every SGR colour attribute - that is what the T07 `NO_COLOR` case asserts on.

use ratatui::style::{Color, Modifier, Style};

/// prime-agent's base colours (`prime.json` "vars").
mod palette {
    pub const FG: u32 = 0xf4_f4_f5;
    pub const MUTED: u32 = 0xa1_a1_aa;
    pub const DIM: u32 = 0x71_71_7a;
    pub const GRID: u32 = 0x52_52_5b;
    pub const SURFACE: u32 = 0x0d_0d_10;
    pub const SELECTED_BG: u32 = 0x22_22_26;
    pub const USER_MSG_BG: u32 = 0x1a_1a_1f;
    pub const PRIMARY: u32 = 0x7c_6f_af;
    pub const PRIMARY_SOFT: u32 = 0x8d_7f_c0;
    pub const SUCCESS: u32 = 0x7d_a8_76;
    pub const WARNING: u32 = 0xf5_9e_0b;
    pub const ERROR: u32 = 0xd0_6f_82;
    pub const INFO: u32 = 0x38_bd_f8;
    pub const STRING_MINT: u32 = 0x8b_a8_88;
    pub const MD_BODY: u32 = 0xd8_d8_dc;
    pub const MD_CODE: u32 = 0xc8_c8_cd;
    pub const DIFF_ADDED: u32 = 0x3f_b9_50;
    pub const DIFF_REMOVED: u32 = 0xf8_51_49;
    pub const REFINEMENT: u32 = 0x95_75_cd;
}

/// How many colours the terminal renders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Depth {
    TrueColor,
    Ansi256,
}

/// `theme.ts`'s colour conversion: the hex value itself, or its nearest entry in
/// the 256-colour cube and grey ramp.
#[must_use]
pub fn color(hex: u32, depth: Depth) -> Color {
    let [_, red, green, blue] = hex.to_be_bytes();
    match depth {
        Depth::TrueColor => Color::Rgb(red, green, blue),
        Depth::Ansi256 => Color::Indexed(nearest_256(red, green, blue)),
    }
}

fn nearest_256(red: u8, green: u8, blue: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |value: u8| {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, level)| value.abs_diff(**level))
            .map_or(0, |(index, _)| index)
    };
    let (r, g, b) = (level(red), level(green), level(blue));
    let cube_distance = |ri: usize, gi: usize, bi: usize| {
        let d = |value: u8, index: usize| u32::from(value.abs_diff(LEVELS[index])).pow(2);
        d(red, ri) + d(green, gi) + d(blue, bi)
    };
    let cube = cube_distance(r, g, b);
    // The 24-step grey ramp, 8..=238.
    let average = (u32::from(red) + u32::from(green) + u32::from(blue)) / 3;
    let grey_step = average.saturating_sub(3) / 10;
    let grey_step = grey_step.min(23);
    let grey = 8 + grey_step * 10;
    let grey_distance = [red, green, blue]
        .iter()
        .map(|value| u32::from(*value).abs_diff(grey).pow(2))
        .sum::<u32>();
    #[allow(
        clippy::cast_possible_truncation,
        reason = "indices are below 256 by construction"
    )]
    if grey_distance < cube {
        (232 + grey_step) as u8
    } else {
        (16 + 36 * r + 6 * g + b) as u8
    }
}

/// One palette for the whole viewport, keyed by what each style is used for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Theme {
    /// Whether colour attributes may be emitted at all.
    pub color: bool,
    /// `accent`/`border`: the spinner, selected items, the prompt prefix.
    pub accent: Style,
    /// `userMessageText`.
    pub user: Style,
    /// `mdBody`: assistant prose.
    pub assistant: Style,
    /// `success`.
    pub tool_ok: Style,
    /// `error` for a failed tool.
    pub tool_failed: Style,
    pub warning: Style,
    pub dim: Style,
    pub error: Style,
    /// `borderMuted`: rules around the editor and menus.
    pub border: Style,
    /// `mdHeading` in bold: titles.
    pub title: Style,
    /// The footer and the working line text (`muted`).
    pub status: Style,
    pub composer_border: Style,
    /// A selected row.
    pub selection: Style,
    /// `muted`: tool labels, tool output (`toolOutput`).
    pub muted: Style,
    /// `toolPanelBg`: the background of tool panels and popups.
    pub panel: Style,
    /// `userMessageBg`: the background of a user message box.
    pub user_box: Style,
    /// `bashMode`: running markers and `$ cmd`.
    pub bash: Style,
    pub md_heading: Style,
    pub md_link: Style,
    pub md_code: Style,
    pub md_code_block: Style,
    pub md_quote: Style,
    pub diff_added: Style,
    pub diff_removed: Style,
    /// `refinementHeader`.
    pub refinement: Style,
    /// `info`.
    pub info: Style,
}

impl Theme {
    /// prime-agent's `prime` theme at the given depth.
    #[must_use]
    pub fn prime(depth: Depth) -> Self {
        let fg = |hex| Style::new().fg(color(hex, depth));
        let bg = |hex| Style::new().bg(color(hex, depth));
        Self {
            color: true,
            accent: fg(palette::PRIMARY),
            user: fg(palette::FG),
            assistant: fg(palette::MD_BODY),
            tool_ok: fg(palette::SUCCESS),
            tool_failed: fg(palette::ERROR),
            warning: fg(palette::WARNING),
            dim: fg(palette::DIM),
            error: fg(palette::ERROR),
            border: fg(palette::GRID),
            title: fg(palette::PRIMARY_SOFT).add_modifier(Modifier::BOLD),
            status: fg(palette::MUTED),
            composer_border: fg(palette::GRID),
            selection: fg(palette::FG)
                .bg(color(palette::SELECTED_BG, depth))
                .add_modifier(Modifier::BOLD),
            muted: fg(palette::MUTED),
            panel: bg(palette::SURFACE),
            user_box: fg(palette::FG).bg(color(palette::USER_MSG_BG, depth)),
            bash: fg(palette::SUCCESS),
            md_heading: fg(palette::PRIMARY_SOFT),
            md_link: fg(palette::INFO),
            md_code: fg(palette::MD_CODE),
            md_code_block: fg(palette::STRING_MINT),
            md_quote: fg(palette::MUTED),
            diff_added: fg(palette::DIFF_ADDED),
            diff_removed: fg(palette::DIFF_REMOVED),
            refinement: fg(palette::REFINEMENT),
            info: fg(palette::INFO),
        }
    }

    /// The coloured theme for this terminal.
    #[must_use]
    pub fn colored() -> Self {
        Self::prime(Self::detect_depth(
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var_os("WT_SESSION").is_some(),
            std::env::var("TERM_PROGRAM").ok().as_deref(),
        ))
    }

    /// Whether this terminal says it renders true colour.
    #[must_use]
    pub fn detect_depth(
        colorterm: Option<&str>,
        windows_terminal: bool,
        program: Option<&str>,
    ) -> Depth {
        let truecolor = colorterm.is_some_and(|value| {
            matches!(value.to_ascii_lowercase().as_str(), "truecolor" | "24bit")
        }) || windows_terminal
            || program.is_some_and(|value| {
                matches!(
                    value,
                    "vscode" | "iTerm.app" | "WezTerm" | "ghostty" | "Apple_Terminal"
                )
            });
        if truecolor {
            Depth::TrueColor
        } else {
            Depth::Ansi256
        }
    }

    /// The colourless theme: identical glyphs and layout, no colour attributes.
    #[must_use]
    pub const fn plain() -> Self {
        let none = Style::new();
        Self {
            color: false,
            accent: none,
            user: none,
            assistant: none,
            tool_ok: none,
            tool_failed: Style::new().add_modifier(Modifier::BOLD),
            warning: Style::new().add_modifier(Modifier::BOLD),
            dim: none,
            error: Style::new().add_modifier(Modifier::BOLD),
            border: none,
            title: Style::new().add_modifier(Modifier::BOLD),
            status: none,
            composer_border: none,
            selection: Style::new().add_modifier(Modifier::BOLD),
            muted: none,
            panel: none,
            user_box: none,
            bash: none,
            md_heading: Style::new().add_modifier(Modifier::BOLD),
            md_link: none,
            md_code: none,
            md_code_block: none,
            md_quote: none,
            diff_added: none,
            diff_removed: none,
            refinement: none,
            info: none,
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

    /// The spinner frame for a tick counter: prime-agent's `Loader` braille frames.
    #[must_use]
    pub const fn spinner(tick: u64) -> &'static str {
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        // The modulo bounds the value to the frame count, so the cast is exact.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded by the frame count"
        )]
        let index = (tick % FRAMES.len() as u64) as usize;
        FRAMES[index]
    }

    /// prime-agent's running marker (`working-icon.ts`): a diamond that pulses.
    #[must_use]
    pub const fn working(tick: u64) -> &'static str {
        const FRAMES: [&str; 4] = ["◇", "◈", "◆", "◈"];
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded by the frame count"
        )]
        let index = (tick % FRAMES.len() as u64) as usize;
        FRAMES[index]
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::detect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Depth, Theme, color};
    use ratatui::style::Color;

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
        let frames: Vec<&str> = (0..10).map(Theme::spinner).collect();
        assert_eq!(
            frames,
            vec!["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
        );
        assert_eq!(Theme::spinner(10), Theme::spinner(0), "it wraps");
    }

    /// prime-agent's palette in true colour, quantised where true colour is not
    /// known to render.
    #[test]
    fn the_prime_palette_is_true_colour_or_its_nearest_256_entry() {
        assert_eq!(
            color(0x7c_6f_af, Depth::TrueColor),
            Color::Rgb(0x7c, 0x6f, 0xaf)
        );
        assert!(matches!(
            color(0x7c_6f_af, Depth::Ansi256),
            Color::Indexed(_)
        ));
        // Near-greys land on the grey ramp.
        assert_eq!(color(0x1a_1a_1f, Depth::Ansi256), Color::Indexed(234));
        assert_eq!(color(0xff_ff_ff, Depth::Ansi256), Color::Indexed(231));
        assert_eq!(
            Theme::detect_depth(Some("truecolor"), false, None),
            Depth::TrueColor
        );
        assert_eq!(Theme::detect_depth(None, true, None), Depth::TrueColor);
        assert_eq!(Theme::detect_depth(None, false, None), Depth::Ansi256);
    }
}
