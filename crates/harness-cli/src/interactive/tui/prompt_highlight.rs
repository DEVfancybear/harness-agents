//! prime-agent's prompt highlighting (`prompt_highlight.rs`, TS
//! `prompt-highlight.ts`): in the prompt and the queued strip, a leading
//! recognized slash command in the accent colour, `@path` tokens in the
//! success colour and `--flag` tokens in the link colour. A token must start
//! at the beginning of the text or after whitespace.

use ratatui::style::Style;
use ratatui::text::Span;

use super::theme::Theme;

/// What one highlighted range is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Token {
    Command,
    Path,
    Flag,
}

/// The highlighted char ranges of `text`, in order. `command` asks for the
/// leading `/name` when it is a known command; `cursor` (a char index) inside
/// it leaves it plain, as prime-agent does while the name is being typed.
fn tokens(text: &str, command: bool, cursor: Option<usize>) -> Vec<(usize, usize, Token)> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    let mut index = 0;
    if command && chars.first() == Some(&'/') {
        let end = chars
            .iter()
            .position(|character| character.is_whitespace())
            .unwrap_or(chars.len());
        let name: String = chars[..end].iter().collect();
        let typing = cursor.is_some_and(|cursor| cursor <= end);
        if !typing && crate::interactive::commands::builtin(&name).is_some() {
            found.push((0, end, Token::Command));
        }
        index = end;
    }
    while index < chars.len() {
        let boundary = index == 0 || chars[index - 1].is_whitespace();
        if !boundary {
            index += 1;
            continue;
        }
        if let Some(end) = path_token(&chars, index) {
            found.push((index, end, Token::Path));
            index = end;
        } else if let Some(end) = flag_token(&chars, index) {
            found.push((index, end, Token::Flag));
            index = end;
        } else {
            index += 1;
        }
    }
    found
}

/// `@"quoted path"`, or `@` and what follows up to whitespace or `|` (a
/// backslash escapes the next character).
fn path_token(chars: &[char], start: usize) -> Option<usize> {
    if chars.get(start) != Some(&'@') {
        return None;
    }
    if chars.get(start + 1) == Some(&'"') {
        let close = chars[start + 2..]
            .iter()
            .position(|character| matches!(character, '"' | '\n'))?;
        return (chars[start + 2 + close] == '"').then_some(start + 3 + close);
    }
    let mut end = start + 1;
    while end < chars.len() {
        match chars[end] {
            '\\' if chars.get(end + 1).is_some_and(|next| !next.is_whitespace()) => end += 2,
            character if character.is_whitespace() || character == '|' => break,
            _ => end += 1,
        }
    }
    (end > start + 1).then_some(end)
}

/// `--` and a letter or digit, then letters, digits and dashes.
fn flag_token(chars: &[char], start: usize) -> Option<usize> {
    if chars.get(start) != Some(&'-') || chars.get(start + 1) != Some(&'-') {
        return None;
    }
    if !chars.get(start + 2)?.is_ascii_alphanumeric() {
        return None;
    }
    let mut end = start + 3;
    while chars
        .get(end)
        .is_some_and(|character| character.is_ascii_alphanumeric() || *character == '-')
    {
        end += 1;
    }
    Some(end)
}

/// `text` as spans in `base`, with its tokens in their colours.
#[must_use]
pub fn spans(
    text: &str,
    base: Style,
    theme: &Theme,
    command: bool,
    cursor: Option<usize>,
) -> Vec<Span<'static>> {
    let found = tokens(text, command, cursor);
    if found.is_empty() {
        return vec![Span::styled(text.to_owned(), base)];
    }
    let chars: Vec<char> = text.chars().collect();
    let piece = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
    let mut spans = Vec::new();
    let mut at = 0;
    for (start, end, token) in found {
        if start > at {
            spans.push(Span::styled(piece(at, start), base));
        }
        let style = match token {
            Token::Command => theme.accent,
            Token::Path => theme.tool_ok,
            Token::Flag => theme.md_link,
        };
        spans.push(Span::styled(piece(start, end), style));
        at = end;
    }
    if at < chars.len() {
        spans.push(Span::styled(piece(at, chars.len()), base));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::{Token, tokens};

    #[test]
    fn commands_paths_and_flags_are_found_where_prime_finds_them() {
        assert_eq!(
            tokens("/model --json @src/main.rs", true, None),
            vec![
                (0, 6, Token::Command),
                (7, 13, Token::Flag),
                (14, 26, Token::Path)
            ]
        );
        // An unknown command, a token inside a word and a bare `--` stay plain.
        assert!(tokens("/nosuch a@b x--y --", true, None).is_empty());
        // The name being typed is not coloured yet.
        assert!(tokens("/model", true, Some(3)).is_empty());
        assert_eq!(
            tokens("see @\"my file.txt\" now", false, None),
            vec![(4, 18, Token::Path)]
        );
    }
}
