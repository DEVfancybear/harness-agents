//! Minimal prompt editor: a pure state machine over normalized keys.
//!
//! Cursor movement is counted in characters, never bytes, so Vietnamese and other
//! multi-byte input cannot be split in the middle of a character. The editor owns
//! no terminal state: the host decides how to render or redraw.
//!
//! T03 extends the H03 editor instead of replacing it: paste keeps its line
//! breaks, the control keys a terminal actually reports are understood, and the
//! editor also owns the two small modal states (session picker, reference
//! overlay) so the controller keeps one source of focus.

use super::events::Key;

/// What the host should do after one key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputOutcome {
    /// The prompt must be redrawn.
    Redraw,
    /// The user submitted a non-empty request.
    Submit(String),
    /// The user submitted a secret; it must never be echoed, logged or stored in
    /// the history, so it travels as its own outcome instead of a `Submit`.
    Secret(String),
    /// Ctrl-C was pressed.
    Interrupt,
    /// Ctrl-D was pressed on an empty prompt.
    Exit,
    /// Tab completed the buffer; the host only has to repaint.
    CompleteSuggestion,
    /// The key changed nothing.
    Unchanged,
}

/// The session picker, owned by the editor so focus has one owner.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Picker {
    items: Vec<String>,
    selected: usize,
}

impl Picker {
    #[must_use]
    pub fn items(&self) -> &[String] {
        &self.items
    }

    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }
}

/// A reference overlay (`/help`, `/status`, `/config`, `/model`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Overlay {
    pub title: String,
    pub lines: Vec<String>,
}

/// Prompt editor with history, completion and the two modal panels.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LineEditor {
    buffer: String,
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    draft: String,
    /// Slash commands matching the buffer right now; Tab accepts the only one.
    suggestion: Vec<&'static str>,
    picker: Option<Picker>,
    overlay: Option<Overlay>,
    /// The buffer is a secret: it is masked in every rendered form and never
    /// becomes history. Set only by an explicit in-app request.
    secret: bool,
}

/// The character a secret buffer is rendered with.
pub const SECRET_MASK: char = '•';

impl LineEditor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The raw buffer, with no masking applied.
    ///
    /// Production code renders through [`Self::display_buffer`]; this accessor
    /// exists for tests that assert what was actually collected.
    #[cfg(test)]
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

    #[must_use]
    pub fn picker(&self) -> Option<&Picker> {
        self.picker.as_ref()
    }

    /// The reference overlay, when one is open.
    #[allow(dead_code, reason = "T06 draws the overlay from this accessor")]
    #[must_use]
    pub fn overlay(&self) -> Option<&Overlay> {
        self.overlay.as_ref()
    }

    /// The completion candidates matching the current buffer.
    #[allow(dead_code, reason = "T03 shows the candidates in the composer hint")]
    #[must_use]
    pub fn suggestions(&self) -> &[&'static str] {
        &self.suggestion
    }

    /// Open the session picker with one label per candidate.
    pub fn open_picker(&mut self, items: Vec<String>) {
        self.picker = Some(Picker { items, selected: 0 });
    }

    pub fn close_picker(&mut self) {
        self.picker = None;
    }

    /// Move the picker highlight, saturating at both ends.
    pub fn move_picker(&mut self, delta: i32) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let last = picker.items.len().saturating_sub(1);
        let next = if delta.is_negative() {
            picker
                .selected
                .saturating_sub(delta.unsigned_abs() as usize)
        } else {
            picker
                .selected
                .saturating_add(delta.unsigned_abs() as usize)
        };
        picker.selected = next.min(last);
    }

    pub fn open_overlay(&mut self, title: &str, lines: Vec<String>) {
        self.overlay = Some(Overlay {
            title: title.to_owned(),
            lines,
        });
    }

    /// Close the reference overlay.
    #[allow(dead_code, reason = "T06 closes the overlay from the controller")]
    pub fn close_overlay(&mut self) {
        self.overlay = None;
    }

    /// Drop the current input without touching the history.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.history_index = None;
        self.draft.clear();
        self.suggestion.clear();
    }

    /// Remove a just-submitted command when it carried a secret inline.
    ///
    /// Normal submissions enter recall history in [`Self::submit`] before the
    /// controller knows which command they name. `/key <value>` is the exception:
    /// the command remains supported, but Up must never reveal it again.
    pub fn forget_submission(&mut self, submitted: &str) {
        if self.history.last().is_some_and(|entry| entry == submitted) {
            self.history.pop();
        }
        self.history_index = None;
        self.draft.clear();
    }

    /// Start collecting a secret: one masked line, no completion, no picker.
    ///
    /// Called only from an explicit user request. Nothing here touches the
    /// history, so the value cannot be recalled with the arrow keys afterwards.
    pub fn begin_secret_entry(&mut self) {
        self.secret = true;
        self.picker = None;
        self.overlay = None;
        self.clear();
    }

    /// Whether the buffer is currently a secret.
    ///
    /// The controller renders through [`Self::display_buffer`], so this is the
    /// state query rather than the value; tests assert the masking contract with it.
    #[allow(dead_code, reason = "the controller renders the masked buffer instead")]
    #[must_use]
    pub const fn secret_entry(&self) -> bool {
        self.secret
    }

    /// The buffer as it may be rendered: masked while a secret is being typed.
    ///
    /// Every render path goes through this, so the value has no way to reach the
    /// screen or the scrollback while it is being entered. The cursor still moves
    /// by one cell per character, because the mask is one character per character.
    #[must_use]
    pub fn display_buffer(&self) -> String {
        mask_secret(self.secret, &self.buffer)
    }

    /// Finish secret entry, returning the collected value.
    ///
    /// The value is returned to the caller and dropped from the editor: it is
    /// never pushed to history.
    pub fn take_secret(&mut self) -> String {
        let value = std::mem::take(&mut self.buffer);
        self.secret = false;
        self.cursor = 0;
        self.suggestion.clear();
        value
    }

    /// Leave secret entry without using the value.
    pub fn cancel_secret(&mut self) {
        self.secret = false;
        self.clear();
    }
}

/// Mask a buffer when it is a secret; one mask character per input character.
#[must_use]
pub fn mask_secret(secret: bool, buffer: &str) -> String {
    if !secret {
        return buffer.to_owned();
    }
    buffer.chars().map(|_| SECRET_MASK).collect()
}

impl LineEditor {
    /// Apply one key.
    #[allow(clippy::too_many_lines, reason = "one arm per key, in key order")]
    #[must_use]
    pub fn handle(&mut self, key: Key) -> InputOutcome {
        match key {
            Key::Char(character) if !character.is_control() => {
                self.insert(&character.to_string());
                self.refresh_suggestion();
                InputOutcome::Redraw
            }
            Key::Newline => {
                // A line break on an empty buffer is not a request, and a second
                // one right after the first adds nothing: keep the prompt usable.
                if self.buffer.is_empty() || self.buffer.ends_with('\n') {
                    return InputOutcome::Unchanged;
                }
                self.insert("\n");
                self.suggestion.clear();
                InputOutcome::Redraw
            }
            Key::Paste(text) => {
                self.insert(&normalize_paste(&text));
                self.refresh_suggestion();
                InputOutcome::Redraw
            }
            Key::Backspace => {
                if self.cursor == 0 {
                    return InputOutcome::Unchanged;
                }
                self.remove_before();
                self.refresh_suggestion();
                InputOutcome::Redraw
            }
            Key::Delete => {
                if self.cursor >= self.char_len() {
                    return InputOutcome::Unchanged;
                }
                self.remove_at();
                self.refresh_suggestion();
                InputOutcome::Redraw
            }
            Key::Left => {
                if self.cursor == 0 {
                    return InputOutcome::Unchanged;
                }
                self.cursor -= 1;
                // A line break belongs to the row above it, so stepping left from
                // the first character of a row lands at the end of that row
                // instead of on the break itself.
                if self.buffer.chars().nth(self.cursor) == Some('\n') {
                    self.cursor -= 1;
                }
                InputOutcome::Redraw
            }
            Key::Right => {
                if self.cursor >= self.char_len() {
                    return InputOutcome::Unchanged;
                }
                self.cursor += 1;
                InputOutcome::Redraw
            }
            Key::Home | Key::LineStart => {
                if self.buffer.contains('\n') && key == Key::LineStart {
                    self.cursor = self.line_start();
                } else {
                    self.cursor = 0;
                }
                InputOutcome::Redraw
            }
            Key::End | Key::LineEnd => {
                if self.buffer.contains('\n') && key == Key::LineEnd {
                    self.cursor = self.line_end();
                } else {
                    self.cursor = self.char_len();
                }
                InputOutcome::Redraw
            }
            Key::EraseToLineStart => {
                let start = self.line_start();
                if start == self.cursor {
                    return InputOutcome::Unchanged;
                }
                let (from, to) = (self.byte_offset(start), self.byte_offset(self.cursor));
                self.buffer.replace_range(from..to, "");
                self.cursor = start;
                self.refresh_suggestion();
                InputOutcome::Redraw
            }
            Key::EraseWord => {
                if self.cursor == 0 {
                    return InputOutcome::Unchanged;
                }
                let target = self.word_start();
                if target == self.cursor {
                    return InputOutcome::Unchanged;
                }
                let (from, to) = (self.byte_offset(target), self.byte_offset(self.cursor));
                self.buffer.replace_range(from..to, "");
                self.cursor = target;
                self.refresh_suggestion();
                InputOutcome::Redraw
            }
            Key::Tab => {
                if let Some(only) = self.unique_suggestion() {
                    self.buffer = only.to_owned();
                    self.cursor = self.char_len();
                    self.suggestion.clear();
                    return InputOutcome::CompleteSuggestion;
                }
                InputOutcome::Unchanged
            }
            Key::Esc => {
                // Escape never cancels a run; it dismisses what the editor showed
                // on its own behalf. Secret entry is one of those things: the app
                // tells the user Esc cancels it, so Esc has to.
                if self.secret {
                    self.cancel_secret();
                    return InputOutcome::Redraw;
                }
                if self.overlay.is_some() {
                    self.overlay = None;
                    return InputOutcome::Redraw;
                }
                if !self.suggestion.is_empty() {
                    self.suggestion.clear();
                    return InputOutcome::Redraw;
                }
                InputOutcome::Unchanged
            }
            Key::Up => {
                if self.buffer.contains('\n')
                    && let Some(previous) = self.row_start_before_cursor()
                {
                    self.cursor = previous;
                    return InputOutcome::Redraw;
                }
                self.recall(true)
            }
            Key::Down => {
                if self.buffer.contains('\n')
                    && let Some(next) = self.row_start_after_cursor()
                {
                    self.cursor = next;
                    return InputOutcome::Redraw;
                }
                self.recall(false)
            }
            Key::Enter => self.submit(),
            Key::Interrupt => InputOutcome::Interrupt,
            Key::EndOfInput => {
                if self.buffer.is_empty() {
                    InputOutcome::Exit
                } else {
                    InputOutcome::Unchanged
                }
            }
            // Control characters are not typed text; Resize and the repaint key
            // only need the redraw the host already performs on its own.
            Key::Char(_)
            | Key::PageUp
            | Key::PageDown
            | Key::Redraw
            | Key::Resize { .. }
            | Key::Unknown => InputOutcome::Unchanged,
        }
    }

    fn submit(&mut self) -> InputOutcome {
        if self.buffer.trim().is_empty() {
            return InputOutcome::Unchanged;
        }
        if self.secret {
            return InputOutcome::Secret(self.take_secret());
        }
        let submitted = std::mem::take(&mut self.buffer);
        if self.history.last() != Some(&submitted) {
            self.history.push(submitted.clone());
        }
        self.cursor = 0;
        self.history_index = None;
        self.draft.clear();
        self.suggestion.clear();
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

    /// The commands the buffer could still become.
    fn refresh_suggestion(&mut self) {
        if !self.buffer.starts_with('/') || self.buffer.contains(char::is_whitespace) {
            self.suggestion.clear();
            return;
        }
        self.suggestion = completions(&self.buffer);
    }

    /// The one command Tab may accept: exactly one candidate, and it is longer
    /// than what is typed.
    fn unique_suggestion(&self) -> Option<&'static str> {
        match self.suggestion.as_slice() {
            [only] if *only != self.buffer => Some(only),
            _ => None,
        }
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

    /// Character index of the start of the row the cursor is on.
    fn line_start(&self) -> usize {
        self.buffer
            .chars()
            .take(self.cursor)
            .enumerate()
            .filter(|(_, character)| *character == '\n')
            .map(|(index, _)| index + 1)
            .last()
            .unwrap_or(0)
    }

    /// Character index of the end of the row the cursor is on.
    fn line_end(&self) -> usize {
        self.buffer
            .chars()
            .enumerate()
            .skip(self.cursor)
            .find(|(_, character)| *character == '\n')
            .map_or_else(|| self.char_len(), |(index, _)| index)
    }

    /// Character index where the word before the cursor starts.
    ///
    /// Whitespace immediately before the cursor is skipped first, then the word
    /// itself: that is what Ctrl-W deletes in a shell, and a line break counts as
    /// whitespace.
    fn word_start(&self) -> usize {
        let offset = self.byte_offset(self.cursor);
        let mut characters = self.buffer[..offset].chars().rev().peekable();
        let mut index = self.cursor;
        while characters
            .next_if(|character| character.is_whitespace())
            .is_some()
        {
            index -= 1;
        }
        while characters
            .next_if(|character| !character.is_whitespace())
            .is_some()
        {
            index -= 1;
        }
        index
    }

    /// Cursor cell at the start of the previous row, when there is one.
    fn row_start_before_cursor(&self) -> Option<usize> {
        let previous_break = self
            .buffer
            .chars()
            .take(self.cursor)
            .enumerate()
            .filter_map(|(index, character)| (character == '\n').then_some(index))
            .last()?;
        let column = self.cursor - previous_break - 1;
        let row_start = self
            .buffer
            .chars()
            .take(previous_break)
            .enumerate()
            .filter_map(|(index, character)| (character == '\n').then_some(index))
            .last()
            .map_or(0, |index| index + 1);
        let row_len = previous_break - row_start;
        Some(row_start + column.min(row_len))
    }

    /// Cursor cell at the start of the next row, when there is one.
    fn row_start_after_cursor(&self) -> Option<usize> {
        let break_ahead = self
            .buffer
            .chars()
            .enumerate()
            .skip(self.cursor)
            .find(|(_, character)| *character == '\n')
            .map(|(index, _)| index)?;
        let row_start = self.line_start();
        let column = self.cursor - row_start;
        let next_start = break_ahead + 1;
        let next_len = self
            .buffer
            .chars()
            .enumerate()
            .skip(next_start)
            .find(|(_, character)| *character == '\n')
            .map_or(self.char_len() - next_start, |(index, _)| {
                index - next_start
            });
        Some(next_start + column.min(next_len))
    }
}

/// Slash commands this revision understands, in help order.
pub const SLASH_COMMANDS: [&str; 8] = [
    "/help", "/status", "/key", "/new", "/model", "/config", "/resume", "/exit",
];

/// Commands whose name starts with `prefix`.
#[must_use]
pub fn completions(prefix: &str) -> Vec<&'static str> {
    SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|command| command.starts_with(prefix))
        .collect()
}

/// Pasted text keeps its content **and** its line breaks.
///
/// A paste is one message: the newlines are preserved so a pasted code block
/// stays one request, and carriage returns are normalized so a Windows paste does
/// not introduce `\r` into the buffer. Other control characters are dropped.
fn normalize_paste(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                let _ = characters.next_if_eq(&'\n');
                normalized.push('\n');
            }
            '\n' => normalized.push('\n'),
            other if !other.is_control() => normalized.push(other),
            _ => {}
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::{InputOutcome, LineEditor, SECRET_MASK, completions};
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
    fn k01_secret_entry_masks_the_buffer_and_never_reaches_history() {
        let mut editor = LineEditor::new();
        editor.begin_secret_entry();
        assert!(editor.secret_entry());
        type_text(&mut editor, "sk-live-secret");
        assert_eq!(
            editor.display_buffer(),
            SECRET_MASK
                .to_string()
                .repeat("sk-live-secret".chars().count()),
            "a secret is rendered as one mask per character"
        );
        assert!(
            !editor.display_buffer().contains("sk-live"),
            "the value has no way to reach a renderer"
        );
        assert_eq!(
            editor.handle(Key::Enter),
            InputOutcome::Secret("sk-live-secret".to_owned())
        );
        assert!(!editor.secret_entry(), "submitting ends secret entry");
        assert_eq!(editor.buffer(), "");
        assert!(
            editor.history().is_empty(),
            "a secret is never added to the recall history"
        );

        // Escaping without submitting leaves nothing behind either. This presses the
        // real key, because the app promises "Esc cancels" and a test that calls
        // `cancel_secret` directly would not notice if that promise broke.
        editor.begin_secret_entry();
        type_text(&mut editor, "sk-abandoned");
        assert_eq!(editor.handle(Key::Esc), InputOutcome::Redraw);
        assert!(!editor.secret_entry(), "Esc ends secret entry");
        assert_eq!(editor.buffer(), "");
        assert!(editor.history().is_empty());
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

    /// T03 contract change: a paste keeps its line breaks.
    ///
    /// The H03 contract flattened them because the prompt was one line. The T03
    /// composer is multi-row, so a pasted code block stays one message **with**
    /// its newlines - still exactly one submit, which is the part that must never
    /// change. Recorded in `docs/specs/HA_TUI.vi.md`.
    #[test]
    fn t03_paste_keeps_newlines_and_submits_once() {
        let mut editor = LineEditor::new();
        let outcome = editor.handle(Key::Paste("fix the parser\nrm -rf /\r\n:q".to_owned()));
        assert_eq!(outcome, InputOutcome::Redraw, "paste must not submit");
        assert_eq!(
            editor.buffer(),
            "fix the parser\nrm -rf /\n:q",
            "newlines are preserved and \\r\\n is normalized"
        );
        assert!(editor.history().is_empty());

        let control = editor.handle(Key::Paste("keep\ttext\u{7}".to_owned()));
        assert_eq!(control, InputOutcome::Redraw);
        assert!(editor.buffer().contains("keeptext"), "{}", editor.buffer());

        // One paste of several lines is still exactly one submission.
        assert_eq!(
            editor.handle(Key::Enter),
            InputOutcome::Submit("fix the parser\nrm -rf /\n:qkeeptext".to_owned())
        );
        assert_eq!(editor.history().len(), 1, "one message, one history entry");
    }

    #[test]
    fn t03_ctrl_u_w_a_e_and_home_end_move_within_the_row() {
        let mut editor = LineEditor::new();
        type_text(&mut editor, "one two");
        assert_eq!(editor.handle(Key::LineStart), InputOutcome::Redraw);
        assert_eq!(editor.cursor(), 0);
        assert_eq!(editor.handle(Key::LineEnd), InputOutcome::Redraw);
        assert_eq!(editor.cursor(), 7);

        // Ctrl-W deletes the word before the cursor, then the space before it.
        assert_eq!(editor.handle(Key::EraseWord), InputOutcome::Redraw);
        assert_eq!(editor.buffer(), "one ");
        assert_eq!(editor.handle(Key::EraseWord), InputOutcome::Redraw);
        assert_eq!(editor.buffer(), "");
        assert_eq!(editor.handle(Key::EraseWord), InputOutcome::Unchanged);

        type_text(&mut editor, "abcdef");
        assert_eq!(editor.cursor(), 6);
        let _ = editor.handle(Key::Left);
        let _ = editor.handle(Key::Left);
        assert_eq!(editor.cursor(), 4, "two steps left from the end");
        assert_eq!(editor.handle(Key::EraseToLineStart), InputOutcome::Redraw);
        assert_eq!(editor.buffer(), "ef", "Ctrl-U erases to the row start");
        assert_eq!(editor.cursor(), 0);

        // With two rows, Ctrl-U and Ctrl-A act on the row the cursor is on.
        let mut multiline = LineEditor::new();
        type_text(&mut multiline, "row one");
        let _ = multiline.handle(Key::Newline);
        type_text(&mut multiline, "row two");
        assert_eq!(multiline.cursor(), 15);
        assert_eq!(multiline.handle(Key::LineStart), InputOutcome::Redraw);
        assert_eq!(multiline.cursor(), 8, "the start of the second row");
        assert_eq!(
            multiline.handle(Key::EraseToLineStart),
            InputOutcome::Unchanged,
            "there is nothing before the cursor on its own row"
        );

        // One step left lands at the end of the row above, not on the break, so
        // Ctrl-U still acts on the row the cursor started on.
        let _ = multiline.handle(Key::Left);
        assert_eq!(
            multiline.cursor(),
            6,
            "the last character of the row above, never the break itself"
        );
        assert_eq!(
            multiline.handle(Key::EraseToLineStart),
            InputOutcome::Redraw
        );
        assert_eq!(
            multiline.buffer(),
            "e\nrow two",
            "everything before the cursor is erased, and the second row is untouched"
        );
    }

    #[test]
    fn t03_up_and_down_move_rows_before_history() {
        let mut editor = LineEditor::new();
        type_text(&mut editor, "first");
        let _ = editor.handle(Key::Enter);
        type_text(&mut editor, "ab");
        let _ = editor.handle(Key::Newline);
        type_text(&mut editor, "cdef");

        // Multi-row buffer: Up moves to the previous row instead of the history.
        assert_eq!(editor.handle(Key::Up), InputOutcome::Redraw);
        assert_eq!(editor.cursor(), 2, "same column on the previous row");
        assert_eq!(
            editor.buffer(),
            "ab\ncdef",
            "the buffer is unchanged by row movement"
        );
        assert_eq!(editor.handle(Key::Down), InputOutcome::Redraw);
        assert_eq!(editor.cursor(), 5, "down returns to the same column");

        // Single-row buffer: Up recalls history.
        let mut single = LineEditor::new();
        type_text(&mut single, "remembered");
        let _ = single.handle(Key::Enter);
        type_text(&mut single, "draft");
        assert_eq!(single.handle(Key::Up), InputOutcome::Redraw);
        assert_eq!(single.buffer(), "remembered");
        assert_eq!(single.handle(Key::Down), InputOutcome::Redraw);
        assert_eq!(single.buffer(), "draft");
    }

    #[test]
    fn t03_tab_completes_only_a_unique_slash_command() {
        let mut editor = LineEditor::new();
        type_text(&mut editor, "/re");
        assert_eq!(editor.suggestions(), ["/resume"]);
        assert_eq!(
            editor.handle(Key::Tab),
            InputOutcome::CompleteSuggestion,
            "Tab accepts the only candidate"
        );
        assert_eq!(editor.buffer(), "/resume");

        // With two candidates Tab does nothing and the hint stays visible.
        let mut ambiguous = LineEditor::new();
        type_text(&mut ambiguous, "/");
        assert_eq!(ambiguous.suggestions().len(), super::SLASH_COMMANDS.len());
        assert_eq!(ambiguous.handle(Key::Tab), InputOutcome::Unchanged);
        assert_eq!(ambiguous.buffer(), "/");

        // Escape clears the suggestion instead of completing it.
        let mut escaping = LineEditor::new();
        type_text(&mut escaping, "/re");
        assert_eq!(escaping.handle(Key::Esc), InputOutcome::Redraw);
        assert!(escaping.suggestions().is_empty());
        assert_eq!(escaping.buffer(), "/re");
    }

    #[test]
    fn t03_escape_closes_an_overlay_before_clearing_a_suggestion() {
        let mut editor = LineEditor::new();
        type_text(&mut editor, "/he");
        editor.open_overlay("/help", vec!["/help  list".to_owned()]);
        assert!(editor.overlay().is_some());
        assert_eq!(editor.handle(Key::Esc), InputOutcome::Redraw);
        assert!(editor.overlay().is_none(), "the overlay closes first");
        assert_eq!(editor.buffer(), "/he", "and the draft survives");
    }

    #[test]
    fn t03_the_picker_clamps_at_both_ends() {
        let mut editor = LineEditor::new();
        editor.open_picker(vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(editor.picker().expect("picker").selected(), 0);
        editor.move_picker(1);
        assert_eq!(editor.picker().expect("picker").selected(), 1);
        editor.move_picker(5);
        assert_eq!(editor.picker().expect("picker").selected(), 1, "clamped");
        editor.move_picker(-9);
        assert_eq!(editor.picker().expect("picker").selected(), 0, "clamped");
        editor.close_picker();
        assert!(editor.picker().is_none());
    }

    #[test]
    fn t03_completions_are_prefix_matches_in_help_order() {
        assert_eq!(completions("/"), super::SLASH_COMMANDS.to_vec());
        assert_eq!(completions("/re"), ["/resume"]);
        assert_eq!(completions("/c"), ["/config"]);
        assert!(completions("/zzz").is_empty());
        assert!(completions("hello").is_empty());
    }
}
