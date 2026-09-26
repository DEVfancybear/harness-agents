//! Shared truncation of tool output the model reads (prime-agent's `truncate.ts`).
//!
//! Two independent limits bound an output, and whichever is hit first wins: a
//! line limit and a byte limit. A cut never returns a partial line, except for
//! the one case a tail cut cannot avoid: the last line alone is larger than the
//! byte limit, and showing its end is better than showing nothing.
//!
//! Head truncation keeps the beginning (a file, a result list); tail truncation
//! keeps the end (a command log, where the error and the final status are).

/// Lines one tool output may show.
pub(crate) const DEFAULT_MAX_LINES: usize = 2000;

/// Bytes one tool output may show.
pub(crate) const DEFAULT_MAX_BYTES: usize = 50 * 1024;

/// Characters one search match line may show.
pub(crate) const GREP_MAX_LINE_LENGTH: usize = 500;

/// Which limit a cut hit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TruncatedBy {
    Lines,
    Bytes,
}

/// The two limits one cut applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TruncationLimits {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for TruncationLimits {
    fn default() -> Self {
        Self {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// A cut output and what it left out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    /// `None` exactly when nothing was cut.
    pub truncated_by: Option<TruncatedBy>,
    /// Lines in the original content (`split('\n')` count, so a trailing
    /// newline counts an empty last line, as prime counts it).
    pub total_lines: usize,
    pub total_bytes: usize,
    /// Whole lines kept.
    pub output_lines: usize,
    pub output_bytes: usize,
    /// Whether the kept text starts inside a line (tail cut of an oversized
    /// last line only).
    pub last_line_partial: bool,
    /// Whether the first line alone is over the byte limit (head cut only);
    /// the content is then empty.
    pub first_line_exceeds_limit: bool,
    pub limits: TruncationLimits,
}

impl TruncationResult {
    fn whole(content: &str, total_lines: usize, limits: TruncationLimits) -> Self {
        Self {
            content: content.to_owned(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes: content.len(),
            output_lines: total_lines,
            output_bytes: content.len(),
            last_line_partial: false,
            first_line_exceeds_limit: false,
            limits,
        }
    }
}

/// A byte count as a short human size: `512B`, `1.5KB`, `2.0MB`.
pub(crate) fn format_size(bytes: u64) -> String {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a display size with one decimal; the loss is far below that"
    )]
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", value / 1024.0)
    } else {
        format!("{:.1}MB", value / (1024.0 * 1024.0))
    }
}

/// Keep the first lines that fit both limits.
///
/// Never returns a partial line: when the first line alone is over the byte
/// limit the content is empty and `first_line_exceeds_limit` says why, so the
/// caller can tell the model how to read that line instead.
pub(crate) fn truncate_head(content: &str, limits: TruncationLimits) -> TruncationResult {
    let lines = content.split('\n').collect::<Vec<_>>();
    let total_lines = lines.len();
    if total_lines <= limits.max_lines && content.len() <= limits.max_bytes {
        return TruncationResult::whole(content, total_lines, limits);
    }
    if lines[0].len() > limits.max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes: content.len(),
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            limits,
        };
    }
    let mut kept = Vec::new();
    let mut bytes = 0_usize;
    let mut truncated_by = TruncatedBy::Lines;
    for (index, line) in lines.iter().take(limits.max_lines).enumerate() {
        // Every line after the first also costs its separating newline.
        let cost = line.len() + usize::from(index > 0);
        if bytes + cost > limits.max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        kept.push(*line);
        bytes += cost;
    }
    if kept.len() >= limits.max_lines && bytes <= limits.max_bytes {
        truncated_by = TruncatedBy::Lines;
    }
    let output = kept.join("\n");
    TruncationResult {
        output_bytes: output.len(),
        output_lines: kept.len(),
        content: output,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes: content.len(),
        last_line_partial: false,
        first_line_exceeds_limit: false,
        limits,
    }
}

/// Keep the last lines that fit both limits.
///
/// When the last line alone is over the byte limit its end is kept instead
/// (`last_line_partial`): for a log, the end of the line is where the error is.
pub(crate) fn truncate_tail(content: &str, limits: TruncationLimits) -> TruncationResult {
    let lines = content.split('\n').collect::<Vec<_>>();
    let total_lines = lines.len();
    if total_lines <= limits.max_lines && content.len() <= limits.max_bytes {
        return TruncationResult::whole(content, total_lines, limits);
    }
    // Collected newest first; reversed once at the end.
    let mut kept: Vec<String> = Vec::new();
    let mut bytes = 0_usize;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;
    for line in lines.iter().rev() {
        if kept.len() >= limits.max_lines {
            break;
        }
        let cost = line.len() + usize::from(!kept.is_empty());
        if bytes + cost > limits.max_bytes {
            truncated_by = TruncatedBy::Bytes;
            // Trailing blank lines must not defeat the oversized-line rescue;
            // keep as many of them as the budget allows.
            if kept.iter().all(String::is_empty) {
                let blanks = kept.len().min(limits.max_bytes.saturating_sub(1));
                kept.truncate(blanks);
                let partial = truncate_str_to_bytes_from_end(line, limits.max_bytes - blanks);
                bytes = partial.len() + blanks;
                kept.push(partial.to_owned());
                last_line_partial = true;
            }
            break;
        }
        kept.push((*line).to_owned());
        bytes += cost;
    }
    if kept.len() >= limits.max_lines && bytes <= limits.max_bytes {
        truncated_by = TruncatedBy::Lines;
    }
    kept.reverse();
    let output = kept.join("\n");
    TruncationResult {
        output_bytes: output.len(),
        output_lines: kept.len(),
        content: output,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes: content.len(),
        last_line_partial,
        first_line_exceeds_limit: false,
        limits,
    }
}

/// The longest suffix of `text` within `max_bytes`, starting on a character
/// boundary.
fn truncate_str_to_bytes_from_end(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Cut one line to `max_chars` characters with a visible marker, for search
/// match lines. Returns the line and whether it was cut.
pub(crate) fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    match line.char_indices().nth(max_chars) {
        None => (line.to_owned(), false),
        Some((end, _)) => (format!("{}... [truncated]", &line[..end]), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(max_lines: usize, max_bytes: usize) -> TruncationLimits {
        TruncationLimits {
            max_lines,
            max_bytes,
        }
    }

    #[test]
    fn sizes_read_like_prime_formats_them() {
        assert_eq!(format_size(512), "512B");
        assert_eq!(format_size(1536), "1.5KB");
        assert_eq!(format_size(50 * 1024), "50.0KB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.0MB");
    }

    #[test]
    fn content_within_both_limits_is_returned_whole() {
        let result = truncate_head("a\nb", limits(2, 10));
        assert!(!result.truncated);
        assert_eq!(result.truncated_by, None);
        assert_eq!(result.content, "a\nb");
        assert_eq!((result.total_lines, result.output_lines), (2, 2));
    }

    #[test]
    fn a_head_cut_keeps_whole_lines_and_names_the_limit_it_hit() {
        let by_lines = truncate_head("1\n2\n3\n4", limits(2, 100));
        assert_eq!(by_lines.content, "1\n2");
        assert_eq!(by_lines.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!((by_lines.output_lines, by_lines.total_lines), (2, 4));

        // "aaa\nbbb" is 7 bytes; the third line would need 4 more.
        let by_bytes = truncate_head("aaa\nbbb\nccc", limits(10, 9));
        assert_eq!(by_bytes.content, "aaa\nbbb");
        assert_eq!(by_bytes.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(by_bytes.output_bytes, 7);
    }

    #[test]
    fn a_head_cut_of_an_oversized_first_line_returns_nothing_and_says_so() {
        let result = truncate_head(&format!("{}\nshort", "x".repeat(20)), limits(10, 10));
        assert!(result.first_line_exceeds_limit);
        assert!(result.content.is_empty());
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
    }

    #[test]
    fn a_tail_cut_keeps_the_last_lines() {
        let by_lines = truncate_tail("1\n2\n3\n4", limits(2, 100));
        assert_eq!(by_lines.content, "3\n4");
        assert_eq!(by_lines.truncated_by, Some(TruncatedBy::Lines));

        let by_bytes = truncate_tail("aaa\nbbb\nccc", limits(10, 9));
        assert_eq!(by_bytes.content, "bbb\nccc");
        assert_eq!(by_bytes.truncated_by, Some(TruncatedBy::Bytes));
        assert!(!by_bytes.last_line_partial);
    }

    #[test]
    fn a_tail_cut_of_an_oversized_last_line_keeps_its_end_on_a_char_boundary() {
        // Each "é" is two bytes; a 5-byte budget cannot start mid-character.
        let result = truncate_tail("head\néééééé", limits(10, 5));
        assert!(result.last_line_partial);
        assert_eq!(result.content, "éé");
        assert_eq!(result.output_lines, 1);
    }

    #[test]
    fn trailing_blank_lines_do_not_defeat_the_oversized_line_rescue() {
        let result = truncate_tail(&format!("{}\n\n", "y".repeat(20)), limits(10, 6));
        assert!(result.last_line_partial);
        // Two blank lines kept, four bytes of the long line.
        assert_eq!(result.content, "yyyy\n\n");
    }

    #[test]
    fn a_long_match_line_is_cut_by_characters_with_a_marker() {
        assert_eq!(truncate_line("short", 10), ("short".to_owned(), false));
        let (text, cut) = truncate_line(&"é".repeat(12), 10);
        assert!(cut);
        assert_eq!(text, format!("{}... [truncated]", "é".repeat(10)));
    }
}
