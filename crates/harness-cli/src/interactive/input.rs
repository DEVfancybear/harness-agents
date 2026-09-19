//! Minimal prompt editor: a pure state machine over normalized keys.
//!
//! Cursor movement is counted in characters, never bytes, so Vietnamese and other
//! multi-byte input cannot be split in the middle of a character. The editor owns
//! no terminal state: the host decides how to render or redraw.

use super::events::Key;

/// What the host should do after one key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputOutcome {
    /// The prompt must be redrawn.
    Redraw,
    /// The user submitted a non-empty request.
    Submit(String),
    /// Ctrl-C was pressed.
    Interrupt,
    /// Ctrl-D was pressed on an empty prompt.
    Exit,
    /// The key changed nothing.
    Unchanged,
}

/// Single-line prompt editor with history.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LineEditor {
    buffer: String,
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    draft: String,
}

impl LineEditor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Cursor position counted in characters from the start of the buffer.
    ///
    /// The renderer needs it to place the terminal cursor inside a multi-line
    /// prompt; it is never a byte offset.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    #[cfg(test)]
    #[must_use]
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Drop the current input without touching the history.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.history_index = None;
        self.draft.clear();
    }

    /// Apply one key.
    #[must_use]
    pub fn handle(&mut self, key: Key) -> InputOutcome {
        match key {
            Key::Char(character) if !character.is_control() => {
                self.insert(&character.to_string());
                InputOutcome::Redraw
            }
            Key::Newline => {
                // A line break on an empty buffer is not a request, and a second
                // one right after the first adds nothing: keep the prompt usable.
                if self.buffer.is_empty() || self.buffer.ends_with('\n') {
                    return InputOutcome::Unchanged;
                }
                self.insert("\n");
                InputOutcome::Redraw
            }
            Key::Paste(text) => {
                self.insert(&normalize_paste(&text));
                InputOutcome::Redraw
            }
            Key::Backspace => {
                if self.cursor == 0 {
                    return InputOutcome::Unchanged;
                }
                self.remove_before();
                InputOutcome::Redraw
            }
            Key::Delete => {
                if self.cursor >= self.char_len() {
                    return InputOutcome::Unchanged;
                }
                self.remove_at();
                InputOutcome::Redraw
            }
            Key::Left => {
                if self.cursor == 0 {
                    return InputOutcome::Unchanged;
                }
                self.cursor -= 1;
                InputOutcome::Redraw
            }
            Key::Right => {
                if self.cursor >= self.char_len() {
                    return InputOutcome::Unchanged;
                }
                self.cursor += 1;
                InputOutcome::Redraw
            }
            Key::Home => {
                self.cursor = 0;
                InputOutcome::Redraw
            }
            Key::End => {
                self.cursor = self.char_len();
                InputOutcome::Redraw
            }
            Key::Up => self.recall(true),
            Key::Down => self.recall(false),
            Key::Enter => self.submit(),
            Key::Interrupt => InputOutcome::Interrupt,
            Key::EndOfInput => {
                if self.buffer.is_empty() {
                    InputOutcome::Exit
                } else {
                    InputOutcome::Unchanged
                }
            }
            // Control characters are not typed text; Resize only needs a redraw of
            // the prompt that the host already performs on its own.
            Key::Char(_) | Key::Resize { .. } | Key::Unknown => InputOutcome::Unchanged,
        }
    }

    fn submit(&mut self) -> InputOutcome {
        if self.buffer.trim().is_empty() {
            return InputOutcome::Unchanged;
        }
        let submitted = std::mem::take(&mut self.buffer);
        if self.history.last() != Some(&submitted) {
            self.history.push(submitted.clone());
        }
        self.cursor = 0;
        self.history_index = None;
        self.draft.clear();
        InputOutcome::Submit(submitted)
    }

    fn recall(&mut self, older: bool) -> InputOutcome {
        if self.history.is_empty() {
            return InputOutcome::Unchanged;
        }
        let last_index = self.history.len() - 1;
        let next = match (self.history_index, older) {
            (None, true) => {
                self.draft.clone_from(&self.buffer);
                Some(last_index)
            }
            (None, false) => return InputOutcome::Unchanged,
            (Some(0), true) => Some(0),
            (Some(index), true) => Some(index - 1),
            (Some(index), false) if index >= last_index => {
                self.history_index = None;
                self.buffer.clone_from(&self.draft);
                self.cursor = self.char_len();
                return InputOutcome::Redraw;
            }
            (Some(index), false) => Some(index + 1),
        };
        self.history_index = next;
        if let Some(index) = next {
            self.buffer.clone_from(&self.history[index]);
            self.cursor = self.char_len();
        }
        InputOutcome::Redraw
    }

    fn char_len(&self) -> usize {
        self.buffer.chars().count()
    }

    fn byte_offset(&self, cursor: usize) -> usize {
        self.buffer
            .char_indices()
            .nth(cursor)
            .map_or(self.buffer.len(), |(index, _)| index)
    }

    fn insert(&mut self, text: &str) {
        let offset = self.byte_offset(self.cursor);
        self.buffer.insert_str(offset, text);
        self.cursor += text.chars().count();
    }

    fn remove_before(&mut self) {
        let start = self.byte_offset(self.cursor - 1);
        let end = self.byte_offset(self.cursor);
        self.buffer.replace_range(start..end, "");
        self.cursor -= 1;
    }

    fn remove_at(&mut self) {
        let start = self.byte_offset(self.cursor);
        let end = self.byte_offset(self.cursor + 1);
        self.buffer.replace_range(start..end, "");
    }
}

/// Pasted text keeps its content but loses line breaks: a paste must never turn
/// into several submitted commands, and the single-line prompt must not break.
fn normalize_paste(text: &str) -> String {
    text.replace("\r\n", " ")
        .replace(['\r', '\n'], " ")
        .chars()
        .filter(|character| !character.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{InputOutcome, LineEditor};
    use crate::interactive::events::Key;

    fn type_text(editor: &mut LineEditor, text: &str) {
        for character in text.chars() {
            let outcome = editor.handle(Key::Char(character));
            assert_eq!(outcome, InputOutcome::Redraw);
        }
    }

    #[test]
    fn h03_editor_edits_vietnamese_text_by_character() {
        let mut editor = LineEditor::new();
        type_text(&mut editor, "sửa lỗi parser");
        assert_eq!(editor.buffer(), "sửa lỗi parser");
        assert_eq!(editor.cursor(), 14);

        assert_eq!(editor.handle(Key::Backspace), InputOutcome::Redraw);
        assert_eq!(editor.buffer(), "sửa lỗi parse");
        assert_eq!(editor.cursor(), 13);

        assert_eq!(editor.handle(Key::Home), InputOutcome::Redraw);
        assert_eq!(editor.cursor(), 0);
        assert_eq!(editor.handle(Key::Delete), InputOutcome::Redraw);
        assert_eq!(editor.buffer(), "ửa lỗi parse");

        assert_eq!(editor.handle(Key::End), InputOutcome::Redraw);
        let _ = editor.handle(Key::Char('!'));
        assert_eq!(editor.buffer(), "ửa lỗi parse!");
    }

    #[test]
    fn h03_editor_keeps_boundaries_and_ignores_empty_submissions() {
        let mut editor = LineEditor::new();
        assert_eq!(editor.handle(Key::Backspace), InputOutcome::Unchanged);
        assert_eq!(editor.handle(Key::Left), InputOutcome::Unchanged);
        assert_eq!(editor.handle(Key::Delete), InputOutcome::Unchanged);
        assert_eq!(editor.handle(Key::Enter), InputOutcome::Unchanged);
        assert!(editor.history().is_empty());

        type_text(&mut editor, "   ");
        assert_eq!(editor.handle(Key::Enter), InputOutcome::Unchanged);
        assert!(editor.history().is_empty(), "blank input is not a request");
    }

    #[test]
    fn h03_editor_accepts_multiline_input_on_the_newline_key() {
        let mut editor = LineEditor::new();
        // A line break before any text would be a blank request.
        assert_eq!(editor.handle(Key::Newline), InputOutcome::Unchanged);
        assert_eq!(editor.buffer(), "");

        type_text(&mut editor, "first line");
        assert_eq!(editor.handle(Key::Newline), InputOutcome::Redraw);
        assert_eq!(editor.buffer(), "first line\n");
        // Two breaks in a row add nothing.
        assert_eq!(editor.handle(Key::Newline), InputOutcome::Unchanged);

        type_text(&mut editor, "second line");
        assert_eq!(editor.buffer(), "first line\nsecond line");
        assert_eq!(editor.cursor(), "first line\nsecond line".chars().count());

        // Enter still submits the whole message, and it is one history entry.
        assert_eq!(
            editor.handle(Key::Enter),
            InputOutcome::Submit("first line\nsecond line".to_owned())
        );
        assert_eq!(editor.history(), ["first line\nsecond line".to_owned()]);

        // Cursor movement stays character-based across a line break: two steps
        // left from the end of "ab\ncd" sits after 'c', so the break is ahead and
        // Backspace joins the two rows back into one.
        type_text(&mut editor, "ab");
        assert_eq!(editor.handle(Key::Newline), InputOutcome::Redraw);
        type_text(&mut editor, "cd");
        assert_eq!(editor.buffer(), "ab\ncd");
        assert_eq!(editor.cursor(), 5);
        assert_eq!(editor.handle(Key::Left), InputOutcome::Redraw);
        assert_eq!(editor.handle(Key::Left), InputOutcome::Redraw);
        assert_eq!(editor.cursor(), 3);
        assert_eq!(editor.handle(Key::Backspace), InputOutcome::Redraw);
        assert_eq!(editor.buffer(), "abcd");
    }

    #[test]
    fn h03_editor_submits_once_and_remembers_history() {
        let mut editor = LineEditor::new();
        type_text(&mut editor, "first request");
        assert_eq!(
            editor.handle(Key::Enter),
            InputOutcome::Submit("first request".to_owned())
        );
        assert_eq!(editor.buffer(), "");
        assert_eq!(editor.history(), ["first request".to_owned()]);

        type_text(&mut editor, "second");
        assert_eq!(editor.handle(Key::Up), InputOutcome::Redraw);
        assert_eq!(editor.buffer(), "first request");
        assert_eq!(editor.handle(Key::Up), InputOutcome::Redraw);
        assert_eq!(editor.buffer(), "first request");
        assert_eq!(editor.handle(Key::Down), InputOutcome::Redraw);
        assert_eq!(editor.buffer(), "second", "the draft comes back");

        assert_eq!(
            editor.handle(Key::Enter),
            InputOutcome::Submit("second".to_owned())
        );
        assert_eq!(
            editor.history(),
            ["first request".to_owned(), "second".to_owned()]
        );
    }

    #[test]
    fn h03_editor_paste_never_submits_multiple_commands() {
        let mut editor = LineEditor::new();
        let outcome = editor.handle(Key::Paste("fix the parser\nrm -rf /\r\n:q".to_owned()));
        assert_eq!(outcome, InputOutcome::Redraw, "paste must not submit");
        assert_eq!(editor.buffer(), "fix the parser rm -rf / :q");
        assert!(editor.history().is_empty());

        let control = editor.handle(Key::Paste("keep\ttext\u{7}".to_owned()));
        assert_eq!(control, InputOutcome::Redraw);
        assert!(editor.buffer().contains("keeptext"), "{}", editor.buffer());
    }

    #[test]
    fn h03_editor_ctrl_c_and_ctrl_d_follow_the_plan() {
        let mut editor = LineEditor::new();
        assert_eq!(editor.handle(Key::EndOfInput), InputOutcome::Exit);

        type_text(&mut editor, "work in progress");
        assert_eq!(
            editor.handle(Key::EndOfInput),
            InputOutcome::Unchanged,
            "Ctrl-D with text present must not exit"
        );
        assert_eq!(editor.handle(Key::Interrupt), InputOutcome::Interrupt);
        assert_eq!(editor.buffer(), "work in progress");

        editor.clear();
        assert_eq!(editor.buffer(), "");
        assert_eq!(editor.handle(Key::EndOfInput), InputOutcome::Exit);
    }
}
