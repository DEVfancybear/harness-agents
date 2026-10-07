//! `search_text`'s engine: every candidate file scanned whole, on all cores,
//! with the result the old line-by-line loop produced.
//!
//! The old loop read each file, split it into a vector of lines and ran the
//! regex on every line in turn, one file after another. Here:
//!
//! - a file is searched as one buffer with a multi-line regex, so the regex
//!   engine's literal prefilters (`memchr`, Teddy) skip the text that cannot
//!   match, and a file without a match costs one pass;
//! - a buffer match only proposes a line: that line is matched again with the
//!   per-line regex, which is what decides the result, its columns and its
//!   order - so a pattern means exactly what it meant per line;
//! - files are scanned in parallel in blocks, and the blocks are merged in path
//!   order, so the cut at 512 matches or 50 KB lands where it always did;
//! - in a large workspace the n-gram index ([`crate::index`]) names the files
//!   that can hold a match, and only those are read.

use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

use globset::Glob;
use harness_types::{ErrorCode, HarnessError};
use regex::RegexBuilder;

use crate::{
    contracts::SearchMatch,
    truncate::{DEFAULT_MAX_BYTES, GREP_MAX_LINE_LENGTH, truncate_line},
    walk::{Walk, WalkFile, relative_text},
    workspace::{MAX_SEARCH_MATCHES, MAX_TEXT_FILE_BYTES, redact_text},
};

pub(crate) struct SearchQuery<'a> {
    pub query: &'a str,
    pub use_regex: bool,
    pub case_insensitive: bool,
    pub glob: Option<&'a str>,
    pub context_lines: u32,
    /// Hashline anchors on each match (`LINE#HASH`).
    pub anchors: bool,
}

pub(crate) struct Found {
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
}

/// The regexes of one search: the per-line one that decides, and the
/// whole-buffer one that finds the lines worth deciding.
pub(crate) struct Matchers {
    pub line: regex::Regex,
    /// `None` when the pattern means something else across a whole buffer
    /// (`\A`, `\z`, inline flags): such a search runs line by line.
    buffer: Option<regex::bytes::Regex>,
    pub pattern: String,
}

impl Matchers {
    pub(crate) fn new(
        query: &str,
        use_regex: bool,
        case_insensitive: bool,
    ) -> Result<Self, HarnessError> {
        let pattern = if use_regex {
            query.to_owned()
        } else {
            regex::escape(query)
        };
        let line = RegexBuilder::new(&pattern)
            .case_insensitive(case_insensitive)
            .build()
            .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error.to_string()))?;
        let line_bound = ["\\A", "\\z", "\\Z", "(?"]
            .iter()
            .any(|marker| pattern.contains(marker));
        let buffer = if line_bound {
            None
        } else {
            regex::bytes::RegexBuilder::new(&pattern)
                .case_insensitive(case_insensitive)
                .multi_line(true)
                .crlf(true)
                .build()
                .ok()
        };
        Ok(Self {
            line,
            buffer,
            pattern,
        })
    }
}

/// One search result line, redacted and cut by characters with prime-agent's
/// visible marker, so the model knows to read the file for the rest of it.
fn search_line(line: &str) -> String {
    truncate_line(&redact_text(line), GREP_MAX_LINE_LENGTH).0
}

/// The text of a candidate file, or `None` for one the search skips as it
/// always has: over the 1 MiB text bound, binary (a NUL), or not UTF-8.
fn searchable_text(file: &WalkFile) -> Option<String> {
    if file.len > MAX_TEXT_FILE_BYTES as u64 {
        return None;
    }
    let bytes = std::fs::read(&file.absolute).ok()?;
    if bytes.len() > MAX_TEXT_FILE_BYTES || memchr::memchr(0, &bytes).is_some() {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// The lines of a text as `str::lines` yields them: `\n` or `\r\n` ends one,
/// and a final terminator starts no empty line.
struct Lines<'t> {
    text: &'t str,
    /// Start offset of every line.
    starts: Vec<usize>,
}

impl<'t> Lines<'t> {
    fn new(text: &'t str) -> Self {
        let mut starts = vec![0];
        starts.extend(memchr::memchr_iter(b'\n', text.as_bytes()).map(|at| at + 1));
        if starts.last() == Some(&text.len()) {
            starts.pop();
        }
        if text.is_empty() {
            starts.clear();
        }
        Self { text, starts }
    }

    fn len(&self) -> usize {
        self.starts.len()
    }

    /// The line holding byte `offset`.
    fn index_of(&self, offset: usize) -> usize {
        self.starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1)
    }

    fn line(&self, index: usize) -> &'t str {
        let start = self.starts[index];
        let terminated = self.starts.get(index + 1).is_some() || self.text.ends_with('\n');
        let end = match self.starts.get(index + 1) {
            Some(next) => next - 1,
            None if terminated => self.text.len() - 1,
            None => self.text.len(),
        };
        let line = &self.text[start..end];
        // A `\r` ends a line only before a `\n`, as `str::lines` reads it.
        if terminated {
            line.strip_suffix('\r').unwrap_or(line)
        } else {
            line
        }
    }
}

/// A match before the output budget is applied.
struct RawMatch {
    line: usize,
    column: usize,
    preview: String,
    context: Vec<String>,
    context_start: usize,
    anchor: Option<String>,
}

/// Every match in one file, in line and column order, at most `cap` of them.
fn file_matches(
    text: &str,
    matchers: &Matchers,
    context_lines: usize,
    anchors: bool,
    cap: usize,
) -> Vec<RawMatch> {
    let lines = Lines::new(text);
    let mut found = Vec::new();
    let on_line = |index: usize, found: &mut Vec<RawMatch>| {
        let line = lines.line(index);
        for hit in matchers.line.find_iter(line) {
            if found.len() == cap {
                return;
            }
            let start = index.saturating_sub(context_lines);
            let end = index
                .saturating_add(context_lines)
                .saturating_add(1)
                .min(lines.len());
            let context = (start..end)
                .filter(|other| *other != index)
                .map(|other| search_line(lines.line(other)))
                .collect();
            found.push(RawMatch {
                line: index,
                column: hit.start(),
                preview: search_line(line),
                context,
                context_start: start + 1,
                anchor: anchors.then(|| crate::edit_diff::line_anchor(index + 1, line)),
            });
        }
    };
    match &matchers.buffer {
        Some(buffer) => {
            let bytes = text.as_bytes();
            let mut position = 0;
            while position <= bytes.len() && found.len() < cap {
                let Some(hit) = buffer.find_at(bytes, position) else {
                    break;
                };
                let index = lines.index_of(hit.start().min(text.len().saturating_sub(1)));
                if index >= lines.len() {
                    break;
                }
                on_line(index, &mut found);
                // Search on from the next line, not from the end of the hit: a
                // match that ran across a line end must not hide the next line.
                match lines.starts.get(index + 1) {
                    Some(next) => position = *next,
                    None => break,
                }
            }
        }
        None => {
            for index in 0..lines.len() {
                if found.len() == cap {
                    break;
                }
                on_line(index, &mut found);
            }
        }
    }
    found
}

/// Search `walk` (the files of the searched folder) for `query`.
pub(crate) fn search(
    root: &Path,
    walk: &Walk,
    query: &SearchQuery<'_>,
) -> Result<Found, HarnessError> {
    let matchers = Matchers::new(query.query, query.use_regex, query.case_insensitive)?;
    let file_matcher = query
        .glob
        .map(|pattern| {
            Glob::new(pattern)
                .map(|glob| glob.compile_matcher())
                .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error.to_string()))
        })
        .transpose()?;
    let globbed: Vec<&WalkFile> = walk
        .files
        .iter()
        .filter(|file| {
            file_matcher
                .as_ref()
                .is_none_or(|matcher| matcher.is_match(Path::new(&file.relative)))
        })
        .collect();
    let candidates = crate::index::candidates(root, &globbed, &matchers, query.case_insensitive)
        .unwrap_or_else(|| (0..globbed.len()).collect());
    let context_lines = usize::try_from(query.context_lines).unwrap_or(0);
    let threads = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .clamp(1, 16);
    let block = threads * 16;
    let mut matches = Vec::new();
    let mut output_bytes = 0_usize;
    for chunk in candidates.chunks(block) {
        let scanned = scan_block(
            &globbed,
            chunk,
            &matchers,
            context_lines,
            query.anchors,
            threads,
        );
        for (file_index, raw) in chunk.iter().zip(scanned) {
            if raw.is_empty() {
                continue;
            }
            let file = globbed[*file_index];
            let relative = file.absolute.strip_prefix(root).map_err(|_| {
                HarnessError::new(
                    ErrorCode::WorkspaceEscape,
                    "searched path escaped workspace root",
                )
            })?;
            let path = relative_text(relative);
            for hit in raw {
                if matches.len() == MAX_SEARCH_MATCHES {
                    return Ok(Found {
                        matches,
                        truncated: true,
                    });
                }
                let context_bytes = hit.context.iter().map(String::len).sum::<usize>();
                let cost = hit
                    .preview
                    .len()
                    .saturating_add(context_bytes)
                    .saturating_add(path.len());
                if output_bytes.saturating_add(cost) > DEFAULT_MAX_BYTES {
                    return Ok(Found {
                        matches,
                        truncated: true,
                    });
                }
                output_bytes = output_bytes.saturating_add(cost);
                matches.push(SearchMatch {
                    path: path.clone(),
                    line: u64::try_from(hit.line.saturating_add(1)).unwrap_or(u64::MAX),
                    column: u64::try_from(hit.column.saturating_add(1)).unwrap_or(u64::MAX),
                    context_start: if hit.context.is_empty() {
                        0
                    } else {
                        u64::try_from(hit.context_start).unwrap_or(u64::MAX)
                    },
                    preview: hit.preview,
                    context: hit.context,
                    anchor: hit.anchor,
                });
            }
        }
    }
    Ok(Found {
        matches,
        truncated: false,
    })
}

/// The matches of each file of one block, scanned side by side, returned in
/// the block's order.
fn scan_block(
    files: &[&WalkFile],
    chunk: &[usize],
    matchers: &Matchers,
    context_lines: usize,
    anchors: bool,
    threads: usize,
) -> Vec<Vec<RawMatch>> {
    let next = AtomicUsize::new(0);
    let mut results: Vec<Option<Vec<RawMatch>>> = (0..chunk.len()).map(|_| None).collect();
    let workers = threads.min(chunk.len()).max(1);
    let collected = std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut local = Vec::new();
                    loop {
                        let slot = next.fetch_add(1, Ordering::Relaxed);
                        let Some(file_index) = chunk.get(slot) else {
                            break;
                        };
                        let found =
                            searchable_text(files[*file_index]).map_or_else(Vec::new, |text| {
                                file_matches(
                                    &text,
                                    matchers,
                                    context_lines,
                                    anchors,
                                    MAX_SEARCH_MATCHES,
                                )
                            });
                        local.push((slot, found));
                    }
                    local
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect::<Vec<_>>()
    });
    for (slot, found) in collected {
        results[slot] = Some(found);
    }
    results.into_iter().map(Option::unwrap_or_default).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The old engine, kept as the reference the new one must agree with.
    fn per_line(
        text: &str,
        matchers: &Matchers,
        context_lines: usize,
    ) -> Vec<(usize, usize, Vec<String>)> {
        let lines = text.lines().collect::<Vec<_>>();
        let mut found = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            for hit in matchers.line.find_iter(line) {
                let start = index.saturating_sub(context_lines);
                let end = (index + context_lines + 1).min(lines.len());
                let context = (start..end)
                    .filter(|other| *other != index)
                    .map(|other| search_line(lines[other]))
                    .collect();
                found.push((index, hit.start(), context));
            }
        }
        found
    }

    fn agree(text: &str, query: &str, regex: bool, insensitive: bool) {
        let matchers = Matchers::new(query, regex, insensitive).expect("pattern");
        for context in [0, 2] {
            let new = file_matches(text, &matchers, context, false, usize::MAX)
                .into_iter()
                .map(|hit| (hit.line, hit.column, hit.context))
                .collect::<Vec<_>>();
            assert_eq!(
                new,
                per_line(text, &matchers, context),
                "{query:?} regex={regex} insensitive={insensitive} context={context} on {text:?}"
            );
        }
    }

    #[test]
    fn the_buffer_search_finds_what_the_line_search_found() {
        let texts = [
            "fn main() {\n    let x = 1;\n}\n",
            "alpha beta\r\nbeta gamma\r\nend\r\n",
            "no final newline\nlast line with foo",
            "",
            "\n\n\nfoo\n\n",
            "foo  \nbar  \r\n  baz\n",
            "a\nfoofoo foo\nb",
            "x = \"token\"\nTOKEN here\n",
        ];
        let queries: [(&str, bool, bool); 12] = [
            ("foo", false, false),
            ("beta", false, false),
            ("^beta", true, false),
            ("gamma$", true, false),
            (r"\s+$", true, false),
            (r"^\s*$", true, false),
            (r"o+", true, false),
            ("TOKEN", false, true),
            (r"\bfoo\b", true, false),
            (r"\Afoo", true, false),
            ("let x", false, false),
            (r"[a-z]+\s[a-z]+", true, false),
        ];
        for text in texts {
            for (query, regex, insensitive) in queries {
                agree(text, query, regex, insensitive);
            }
        }
    }

    #[test]
    fn lines_split_like_str_lines() {
        for text in [
            "", "a", "a\n", "a\r\nb", "a\n\nb\n", "\n", "\r\n\r\n", "a\r", "a\r\nb\r",
        ] {
            let lines = Lines::new(text);
            let ours = (0..lines.len())
                .map(|index| lines.line(index))
                .collect::<Vec<_>>();
            assert_eq!(ours, text.lines().collect::<Vec<_>>(), "{text:?}");
        }
    }
}
