//! Text matching and diffs for `edit_file` (prime-agent's `edit-diff.ts`).
//!
//! A model reproduces the text it wants to replace from memory, and the misses
//! are predictable: the file uses CRLF and the model sends LF, a line carries
//! trailing whitespace the model dropped, or the file has typographic quotes,
//! dashes or no-break spaces the model wrote as ASCII. Matching therefore tries
//! the exact text first and then a normalized form of both sides, and still
//! refuses a match that is not unique.
//!
//! When only the normalized form matches, the replacement is made in normalized
//! space, so the written file also carries the normalized whitespace, quotes and
//! dashes. That is prime's behaviour and a deliberate trade: the edit lands
//! instead of failing on characters the model cannot see.

use harness_types::{ErrorCode, HarnessError};
use similar::{ChangeTag, TextDiff};
use unicode_normalization::UnicodeNormalization;

/// Lines of unchanged context shown around each change in an edit diff.
pub(crate) const DIFF_CONTEXT_LINES: usize = 4;

const BOM: char = '\u{feff}';

/// The line ending a file uses, by its first line break.
pub(crate) fn detect_line_ending(content: &str) -> &'static str {
    match (content.find("\r\n"), content.find('\n')) {
        (Some(crlf), Some(lf)) if crlf < lf => "\r\n",
        _ => "\n",
    }
}

pub(crate) fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub(crate) fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_owned()
    }
}

/// Split a leading UTF-8 byte-order mark from the text. The model never puts
/// an invisible BOM in `old_string`, so it is matched without and restored.
pub(crate) fn strip_bom(content: &str) -> (&str, &str) {
    match content.strip_prefix(BOM) {
        Some(text) => (&content[..BOM.len_utf8()], text),
        None => ("", content),
    }
}

/// The form both sides are compared in when the exact text is not found:
/// NFKC, no trailing whitespace on any line, and ASCII for typographic
/// quotes, dashes and special spaces.
pub(crate) fn normalize_for_fuzzy_match(text: &str) -> String {
    let composed = text.nfkc().collect::<String>();
    composed
        .split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .map(|character| match character {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '\u{00A0}' | '\u{2002}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

/// Where `old_text` was found, and in which text the offsets apply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FuzzyMatch {
    pub index: usize,
    pub match_length: usize,
    /// Whether only the normalized form matched (prime's `usedFuzzyMatch`).
    pub normalized: bool,
    /// The original content for an exact match, the normalized content for a
    /// fuzzy one: the offsets above index this text.
    pub content_for_replacement: String,
}

/// Find `old_text` exactly, else in normalized form; `None` when neither
/// matches.
pub(crate) fn fuzzy_find_text(content: &str, old_text: &str) -> Option<FuzzyMatch> {
    if let Some(index) = content.find(old_text) {
        return Some(FuzzyMatch {
            index,
            match_length: old_text.len(),
            normalized: false,
            content_for_replacement: content.to_owned(),
        });
    }
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    let index = fuzzy_content.find(&fuzzy_old)?;
    Some(FuzzyMatch {
        index,
        match_length: fuzzy_old.len(),
        normalized: true,
        content_for_replacement: fuzzy_content,
    })
}

/// Occurrences of `old_text`, counted in normalized form so that two regions
/// differing only in what normalization erases still count as ambiguous.
fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    if fuzzy_old.is_empty() {
        return 0;
    }
    normalize_for_fuzzy_match(content)
        .matches(fuzzy_old.as_str())
        .count()
}

/// One planned edit: the LF content it was matched in, the result, and how
/// many regions it replaced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AppliedEdit {
    /// The LF, BOM-free content the edit was applied to (normalized when the
    /// match was fuzzy). A diff against `new_content` shows exactly the edit.
    pub base_content: String,
    pub new_content: String,
    pub replacements: u64,
}

/// Apply one replacement to LF-normalized, BOM-free content.
///
/// `path` only names the file in the error a model reads. Without
/// `replace_all` the match must be unique; with it, every occurrence in the
/// matched space is replaced.
pub(crate) fn apply_edit_to_normalized_content(
    normalized_content: &str,
    old_text: &str,
    new_text: &str,
    replace_all: bool,
    path: &str,
) -> Result<AppliedEdit, HarnessError> {
    let old_text = normalize_to_lf(old_text);
    let new_text = normalize_to_lf(new_text);
    if old_text.is_empty() {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            format!("old_string must not be empty in {path}."),
        ));
    }
    let base_content = match fuzzy_find_text(normalized_content, &old_text) {
        Some(found) if found.normalized => normalize_for_fuzzy_match(normalized_content),
        _ => normalized_content.to_owned(),
    };
    let Some(found) = fuzzy_find_text(&base_content, &old_text) else {
        return Err(HarnessError::new(
            ErrorCode::EditNotFound,
            format!(
                "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
            ),
        ));
    };
    let occurrences = count_occurrences(&base_content, &old_text);
    if occurrences > 1 && !replace_all {
        // ha's edit_file has `replace_all`, which prime's does not; the model is
        // told about it here because it is the other way out of an ambiguity.
        return Err(HarnessError::new(
            ErrorCode::EditAmbiguous,
            format!(
                "Found {occurrences} occurrences of the text in {path}. The text must be unique. Please provide more context to make it unique, or set replace_all to change every occurrence."
            ),
        ));
    }
    let (new_content, replacements) = if replace_all {
        // Replace in the space the match was made in: the exact text, or the
        // normalized text inside normalized content.
        let (haystack, needle) = if found.normalized {
            (
                found.content_for_replacement,
                normalize_for_fuzzy_match(&old_text),
            )
        } else {
            (base_content.clone(), old_text)
        };
        let count = haystack.matches(needle.as_str()).count();
        (
            haystack.replace(needle.as_str(), &new_text),
            u64::try_from(count).unwrap_or(u64::MAX),
        )
    } else {
        let source = &found.content_for_replacement;
        let end = found.index + found.match_length;
        (
            format!("{}{new_text}{}", &source[..found.index], &source[end..]),
            1,
        )
    };
    if base_content == new_content {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            format!(
                "No changes made to {path}. The replacement produced identical content. This might indicate an issue with special characters or the text not existing as expected."
            ),
        ));
    }
    Ok(AppliedEdit {
        base_content,
        new_content,
        replacements,
    })
}

/// A planned edit of a whole file: the bytes to write and the diff to show.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlannedEdit {
    /// The full replacement file content, with the file's BOM and line
    /// endings restored.
    pub content: String,
    pub replacements: u64,
    /// Numbered diff of the edit (see [`generate_diff_string`]).
    pub diff: String,
}

/// Plan one edit of a file's current text, as prime's edit tool does: strip the
/// BOM, match in LF space, then restore the BOM and the file's line ending.
pub(crate) fn plan_edit(
    current: &str,
    old_text: &str,
    new_text: &str,
    replace_all: bool,
    path: &str,
) -> Result<PlannedEdit, HarnessError> {
    let (bom, text) = strip_bom(current);
    let ending = detect_line_ending(text);
    let normalized = normalize_to_lf(text);
    let applied =
        apply_edit_to_normalized_content(&normalized, old_text, new_text, replace_all, path)?;
    let content = format!(
        "{bom}{}",
        restore_line_endings(&applied.new_content, ending)
    );
    let (diff, _) = generate_diff_string(
        &applied.base_content,
        &applied.new_content,
        DIFF_CONTEXT_LINES,
        1,
    );
    Ok(PlannedEdit {
        content,
        replacements: applied.replacements,
        diff,
    })
}

/// One run of lines that are all unchanged, all removed, or all added.
struct Part {
    tag: ChangeTag,
    lines: Vec<String>,
}

fn diff_parts(old_content: &str, new_content: &str) -> Vec<Part> {
    let diff = TextDiff::from_lines(old_content, new_content);
    let mut parts: Vec<Part> = Vec::new();
    for change in diff.iter_all_changes() {
        let line = change.value();
        let line = line.strip_suffix('\n').unwrap_or(line).to_owned();
        match parts.last_mut() {
            Some(part) if part.tag == change.tag() => part.lines.push(line),
            _ => parts.push(Part {
                tag: change.tag(),
                lines: vec![line],
            }),
        }
    }
    parts
}

/// A diff with line numbers and `context_lines` of context around each
/// change, and the first changed line in the new file.
///
/// Lines read `+NN text`, `-NN text` or ` NN text`; a run of unchanged lines
/// longer than the context is elided as `...`.
pub(crate) fn generate_diff_string(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
    start_line: usize,
) -> (String, Option<usize>) {
    let parts = diff_parts(old_content, new_content);
    let old_count = old_content.split('\n').count();
    let new_count = new_content.split('\n').count();
    let mut diff = NumberedDiff {
        output: Vec::new(),
        old_line: start_line,
        new_line: start_line,
        width: (start_line - 1 + old_count.max(new_count))
            .to_string()
            .len(),
    };
    let mut last_was_change = false;
    let mut first_changed_line = None;
    for (index, part) in parts.iter().enumerate() {
        if part.tag != ChangeTag::Equal {
            first_changed_line.get_or_insert(diff.new_line);
            diff.changed(part);
            last_was_change = true;
            continue;
        }
        let next_is_change = parts
            .get(index + 1)
            .is_some_and(|next| next.tag != ChangeTag::Equal);
        let raw = &part.lines;
        if last_was_change && next_is_change {
            if raw.len() <= context_lines * 2 {
                diff.context(raw);
            } else {
                diff.context(&raw[..context_lines]);
                diff.elide(raw.len() - context_lines * 2);
                diff.context(&raw[raw.len() - context_lines..]);
            }
        } else if last_was_change {
            let shown = raw.len().min(context_lines);
            diff.context(&raw[..shown]);
            diff.elide(raw.len() - shown);
        } else if next_is_change {
            let skipped = raw.len().saturating_sub(context_lines);
            diff.elide(skipped);
            diff.context(&raw[skipped..]);
        } else {
            diff.skip(raw.len());
        }
        last_was_change = false;
    }
    (diff.output.join("\n"), first_changed_line)
}

/// The output of [`generate_diff_string`] and where it is in both files.
struct NumberedDiff {
    output: Vec<String>,
    old_line: usize,
    new_line: usize,
    width: usize,
}

impl NumberedDiff {
    fn changed(&mut self, part: &Part) {
        let width = self.width;
        for line in &part.lines {
            if part.tag == ChangeTag::Insert {
                self.output
                    .push(format!("+{:>width$} {line}", self.new_line));
                self.new_line += 1;
            } else {
                self.output
                    .push(format!("-{:>width$} {line}", self.old_line));
                self.old_line += 1;
            }
        }
    }

    /// Unchanged lines, numbered by the old file.
    fn context(&mut self, lines: &[String]) {
        let width = self.width;
        for line in lines {
            self.output
                .push(format!(" {:>width$} {line}", self.old_line));
            self.skip(1);
        }
    }

    /// Unchanged lines left out, shown as one `...` row.
    fn elide(&mut self, skipped: usize) {
        if skipped == 0 {
            return;
        }
        self.output.push(format!(" {} ...", " ".repeat(self.width)));
        self.skip(skipped);
    }

    fn skip(&mut self, lines: usize) {
        self.old_line += lines;
        self.new_line += lines;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(current: &str, old: &str, new: &str) -> Result<PlannedEdit, HarnessError> {
        plan_edit(current, old, new, false, "f.txt")
    }

    #[test]
    fn an_exact_match_is_replaced_and_nothing_else_changes() {
        let planned = plan("a  \nb\nc\n", "b", "B").expect("exact");
        // The trailing spaces on the first line survive an exact match.
        assert_eq!(planned.content, "a  \nB\nc\n");
        assert_eq!(planned.replacements, 1);
    }

    #[test]
    fn a_crlf_file_matches_lf_text_and_keeps_its_line_endings_and_bom() {
        let planned = plan("\u{feff}one\r\ntwo\r\nthree\r\n", "one\ntwo", "uno\ndos").expect("lf");
        assert_eq!(planned.content, "\u{feff}uno\r\ndos\r\nthree\r\n");
    }

    #[test]
    fn trailing_whitespace_quotes_dashes_and_spaces_match_in_normalized_form() {
        let current =
            "let s = \u{201C}hi\u{201D};   \nlet d = a \u{2013} b;\nlet n = 1\u{00A0}+ 2;\n";
        let planned = plan(
            current,
            "let s = \"hi\";\nlet d = a - b;\nlet n = 1 + 2;",
            "changed",
        )
        .expect("fuzzy");
        // The replacement is made in normalized space.
        assert_eq!(planned.content, "changed\n");
    }

    #[test]
    fn a_fuzzy_match_normalizes_the_rest_of_the_file_as_prime_does() {
        let planned = plan(
            "keep \u{2018}x\u{2019}  \nold\u{00A0}line\n",
            "old line",
            "new",
        )
        .expect("fuzzy");
        assert_eq!(planned.content, "keep 'x'\nnew\n");
    }

    #[test]
    fn missing_ambiguous_empty_and_no_op_edits_are_refused_with_prime_messages() {
        let missing = plan("abc", "zzz", "y").expect_err("missing");
        assert_eq!(missing.code(), ErrorCode::EditNotFound);
        assert!(
            missing
                .to_string()
                .contains("Could not find the exact text in f.txt."),
            "{missing}"
        );

        let ambiguous = plan("x\nx\n", "x", "y").expect_err("ambiguous");
        assert_eq!(ambiguous.code(), ErrorCode::EditAmbiguous);
        assert!(
            ambiguous
                .to_string()
                .contains("Found 2 occurrences of the text in f.txt."),
            "{ambiguous}"
        );

        // Regions that differ only in what normalization erases are ambiguous too.
        let near = plan("a;  \na;\n", "a;\n", "b\n").expect_err("normalized duplicate");
        assert_eq!(near.code(), ErrorCode::EditAmbiguous);

        let empty = plan("abc", "", "y").expect_err("empty");
        assert_eq!(empty.code(), ErrorCode::InvalidPayload);
        assert!(
            empty
                .to_string()
                .contains("old_string must not be empty in f.txt.")
        );

        let same = plan("abc", "b", "b").expect_err("no change");
        assert!(
            same.to_string().contains("No changes made to f.txt."),
            "{same}"
        );
    }

    #[test]
    fn replace_all_replaces_every_occurrence_and_counts_them() {
        let planned = plan_edit("x\nx\ny\n", "x", "z", true, "f.txt").expect("all");
        assert_eq!(planned.content, "z\nz\ny\n");
        assert_eq!(planned.replacements, 2);

        let fuzzy = plan_edit("a\u{2013}b\r\na\u{2013}b\r\n", "a-b", "c", true, "f.txt")
            .expect("fuzzy all");
        assert_eq!(fuzzy.content, "c\r\nc\r\n");
        assert_eq!(fuzzy.replacements, 2);
    }

    #[test]
    fn the_diff_numbers_lines_and_elides_distant_context() {
        let old = (1..=20)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let new = old.replace("\n10\n", "\nten\n");
        let (diff, first) = generate_diff_string(&old, &new, 2, 1);
        assert_eq!(first, Some(10));
        assert_eq!(
            diff,
            [
                "    ...", "  8 8", "  9 9", "-10 10", "+10 ten", " 11 11", " 12 12", "    ...",
            ]
            .join("\n")
        );
    }

    #[test]
    fn a_planned_edit_carries_the_diff_of_the_edit() {
        let planned = plan("a\nb\nc\n", "b", "B").expect("edit");
        assert_eq!(planned.diff, [" 1 a", "-2 b", "+2 B", " 3 c"].join("\n"));
    }
}
