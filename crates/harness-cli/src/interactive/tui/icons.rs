//! What a tool is, at a glance: a kind, its glyph and its colour.
//!
//! The transcript is read by scrolling, so a call has to be recognisable before
//! its words are: a reader who sees `▤` in blue knows a file was read, `✎` in
//! amber that one was written, `❯` in magenta that a command ran. The glyphs are
//! from blocks every console font falls back to (geometric shapes, arrows,
//! dingbats), not from a private-use icon font, so nothing needs installing.

use ratatui::style::Style;

use super::theme::Theme;

/// The families of tool the transcript tells apart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolKind {
    /// Reading a file or a file's history.
    Read,
    /// Listing or matching paths.
    List,
    /// Searching text.
    Search,
    /// Changing an existing file.
    Edit,
    /// Creating or replacing a file.
    Write,
    /// A process or shell command.
    Shell,
    /// Git.
    Git,
    /// The web.
    Web,
    /// Delegating to another agent.
    Agent,
    /// The Python kernel.
    Python,
    /// An MCP server's tool.
    Mcp,
    /// A skill.
    Skill,
    /// Asking the operator.
    Ask,
    /// Anything else.
    Other,
}

impl ToolKind {
    /// How many kinds there are, for a style table indexed by kind.
    pub const COUNT: usize = 14;

    /// The kind of a tool by its name. The names are the tools' own contract
    /// names, so this is a table, not a guess about wording.
    #[must_use]
    pub fn of(name: &str) -> Self {
        match name {
            "read_file" | "history_read" => Self::Read,
            "list_files" | "glob" => Self::List,
            "search_text" | "history_search" => Self::Search,
            "edit_file" | "apply_patch" => Self::Edit,
            "write_file" => Self::Write,
            "bash" | "run_shell" | "run_process" | "read_process_output" | "stop_process" => {
                Self::Shell
            }
            "git_status" | "git_diff" | "git_log" => Self::Git,
            "web_search" | "web_fetch" => Self::Web,
            "delegate" | "list_subagents" | "agent_message" | "stop_subagent" => Self::Agent,
            "ipython" => Self::Python,
            "activate_skill" | "list_skills" | "read_skill_file" => Self::Skill,
            "ask_user" => Self::Ask,
            other if other.starts_with("mcp") || other.contains("__") => Self::Mcp,
            _ => Self::Other,
        }
    }

    /// The position in a per-kind table.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The glyph in front of a call of this kind.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Read => "▤",
            Self::List => "▥",
            Self::Search => "⌕",
            Self::Edit => "✎",
            Self::Write => "✚",
            Self::Shell => "❯",
            Self::Git => "⎇",
            Self::Web => "◍",
            Self::Agent => "◈",
            Self::Python => "λ",
            Self::Mcp => "⬢",
            Self::Skill => "✦",
            Self::Ask => "?",
            Self::Other => "•",
        }
    }

    /// The style a call of this kind is drawn in.
    #[must_use]
    pub fn style(self, theme: &Theme) -> Style {
        theme.kinds[self.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::ToolKind;

    #[test]
    fn every_tool_name_has_a_kind_and_a_glyph_of_its_own_family() {
        assert_eq!(ToolKind::of("read_file"), ToolKind::Read);
        assert_eq!(ToolKind::of("glob"), ToolKind::List);
        assert_eq!(ToolKind::of("search_text"), ToolKind::Search);
        assert_eq!(ToolKind::of("apply_patch"), ToolKind::Edit);
        assert_eq!(ToolKind::of("write_file"), ToolKind::Write);
        assert_eq!(ToolKind::of("git_status"), ToolKind::Git);
        assert_eq!(ToolKind::of("web_fetch"), ToolKind::Web);
        assert_eq!(ToolKind::of("delegate"), ToolKind::Agent);
        assert_eq!(ToolKind::of("ipython"), ToolKind::Python);
        assert_eq!(ToolKind::of("something_new"), ToolKind::Other);
    }

    #[test]
    fn kinds_have_distinct_glyphs() {
        let kinds = [
            ToolKind::Read,
            ToolKind::List,
            ToolKind::Search,
            ToolKind::Edit,
            ToolKind::Write,
            ToolKind::Shell,
            ToolKind::Git,
            ToolKind::Web,
            ToolKind::Agent,
            ToolKind::Python,
            ToolKind::Mcp,
            ToolKind::Skill,
            ToolKind::Ask,
            ToolKind::Other,
        ];
        assert_eq!(kinds.len(), ToolKind::COUNT);
        let mut glyphs: Vec<_> = kinds.iter().map(|kind| kind.glyph()).collect();
        glyphs.sort_unstable();
        glyphs.dedup();
        assert_eq!(glyphs.len(), ToolKind::COUNT, "two kinds share a glyph");
    }
}
