//! `git_search` and `git_show`: the repository's past, read-only.
//!
//! `git_log` lists the latest commits and `git_diff` shows what is not yet
//! committed, so an agent could not find the commit that once changed what it
//! is working on, nor read what a commit changed. `RepoMem` (arXiv 2510.01003)
//! found that searching past commit messages and reading the found commit's
//! diff helps an agent locate the files an issue is about (SWE-bench Verified
//! Acc@5 71.6 -> 74.3 from this part alone).
//!
//! - `git_search` ranks the messages (and changed file names) of the latest
//!   [`SEARCH_SCAN_COMMITS`] commits against a query with BM25, over the words
//!   [`harness_types::readout::words`] reads everywhere in ha, and answers the
//!   best few with their changed files.
//! - `git_show` prints one commit: its message, the files it changed with their
//!   line counts, and its diff, one page at a time (`offset` and `length`, as
//!   `history_read` pages).
//!
//! Every git call here takes its arguments as a list: nothing a model wrote is
//! ever part of a command line.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use harness_providers::CancellationToken;
use harness_types::{
    ErrorCode, HarnessError,
    readout::words,
    search::{bm25_idf, bm25_term},
};

use crate::{
    GIT_SEARCH_DEFAULT_LIMIT, ToolOutput,
    capture::{ProcessSpoolConfig, SpoolLimits},
    process,
    service::git_output,
    workspace::redact_text,
};

/// Latest commits whose messages one `git_search` reads. A repository with more
/// is searched over its newest part, and the answer says so.
pub(crate) const SEARCH_SCAN_COMMITS: usize = 5_000;

/// Changed files one hit lists before "+N more".
const SEARCH_FILES_SHOWN: usize = 8;

/// Changed files of one commit that count as words when ranking: a commit
/// that touched thousands of files would otherwise match everything.
const SEARCH_FILES_RANKED: usize = 200;

/// Characters of a commit's body that count as words when ranking.
const SEARCH_BODY_CHARS: usize = 2_000;

/// Characters of a body a hit shows, on one line.
const SEARCH_BODY_SHOWN: usize = 140;

/// A word of the subject counts this many times a word of the body or a path:
/// the subject says what the commit was about (`RepoMem` ranks on messages).
const SUBJECT_WEIGHT: f64 = 3.0;

/// Hex digits of a commit id a hit shows.
const SHORT_HASH: usize = 10;

/// Most output bytes of one history read that is held in memory: the log of
/// the latest commits, or one commit's diff.
const SCAN_BYTES: usize = 8 * 1024 * 1024;

/// Changed files `git_show` lists before "+N more".
const SHOW_FILES_SHOWN: usize = 200;

/// Characters of a message `git_show` prints.
const SHOW_MESSAGE_CHARS: usize = 8_000;

/// How long one git call may run.
const GIT_TIMEOUT_MS: u64 = 30_000;

/// What a changed file outside the workspace is marked with.
const OUTSIDE: &str = "(outside the workspace) ";

/// Whether `revision` can be handed to git as one revision and nothing else:
/// a hash or a ref name (`main`, `v1.2`, `HEAD~3`, `origin/main`, `HEAD@{1}`),
/// with no space, no leading `-` (git would read it as an option) and no
/// character that makes a range, a path or a pathspec of it.
pub(crate) fn check_revision(revision: &str) -> Result<(), HarnessError> {
    let allowed =
        |character: char| character.is_ascii_alphanumeric() || "/._-~^@{}".contains(character);
    if revision.is_empty()
        || revision.len() > 256
        || revision.starts_with('-')
        || revision.contains("..")
        || !revision.chars().all(allowed)
    {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "git_show commit must be a commit hash or a ref name (letters, digits and / . _ - ~ ^ @ { } only, no spaces, not starting with '-', no range)",
        ));
    }
    Ok(())
}

/// One commit, as `git_search` reads it.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Commit {
    pub hash: String,
    pub date: String,
    pub author: String,
    pub subject: String,
    pub body: String,
    pub files: Vec<String>,
}

/// The `git log` argument list that reads the latest commits' messages and
/// changed files: a record starts at `0x1e`, its fields are cut by `0x1f` (so a
/// message may hold tabs and newlines), and the file names follow the format.
pub(crate) fn search_arguments(path: Option<&str>) -> Vec<String> {
    let mut args = [
        "--literal-pathspecs",
        "-c",
        "core.quotepath=off",
        "log",
        "--no-color",
        "--no-show-signature",
        "--name-only",
        "--format=%x1e%H%x1f%aI%x1f%an%x1f%s%x1f%b%x1f",
    ]
    .map(str::to_owned)
    .to_vec();
    args.push(format!("-n{SEARCH_SCAN_COMMITS}"));
    if let Some(path) = path {
        args.push("--".to_owned());
        args.push(path.to_owned());
    }
    args
}

/// The commits of [`search_arguments`]' output. A record cut short (a log that
/// outgrew [`SCAN_BYTES`]) has no file section and is dropped.
pub(crate) fn parse_log(text: &str) -> Vec<Commit> {
    text.split('\u{1e}')
        .filter_map(|record| {
            let mut fields = record.splitn(6, '\u{1f}');
            let hash = fields.next()?.trim();
            let date = fields.next()?;
            let author = fields.next()?;
            let subject = fields.next()?;
            let body = fields.next()?;
            let files = fields.next()?;
            if hash.is_empty() || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            Some(Commit {
                hash: hash.to_owned(),
                date: date.trim().to_owned(),
                author: author.trim().to_owned(),
                subject: subject.trim().to_owned(),
                body: body.trim().to_owned(),
                files: files
                    .lines()
                    .map(str::trim)
                    .filter(|file| !file.is_empty())
                    .map(str::to_owned)
                    .collect(),
            })
        })
        .collect()
}

/// What a ranking found.
#[derive(Debug)]
pub(crate) struct Ranking<'a> {
    /// The best `limit` commits, best first; a tie goes to the newer commit.
    pub hits: Vec<&'a Commit>,
    /// How many commits hold at least one query word.
    pub matching: usize,
    /// The query words no commit holds.
    pub unmatched: Vec<String>,
}

/// Rank `commits` (newest first) against the query's `terms` with BM25: each
/// commit is a document of its subject (weighted [`SUBJECT_WEIGHT`]), its body
/// and the words of the paths it changed, scored with the shared
/// `bm25_idf`/`bm25_term`.
pub(crate) fn rank<'a>(commits: &'a [Commit], terms: &[String], limit: usize) -> Ranking<'a> {
    let wanted = terms.iter().map(String::as_str).collect::<BTreeSet<_>>();
    // Per commit: the weighted frequency of each query word it holds, and its
    // length in words.
    let documents = commits
        .iter()
        .map(|commit| {
            let mut frequencies = BTreeMap::<&str, f64>::new();
            let mut length = 0.0_f64;
            let body = commit
                .body
                .chars()
                .take(SEARCH_BODY_CHARS)
                .collect::<String>();
            let paths = commit
                .files
                .iter()
                .take(SEARCH_FILES_RANKED)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" ");
            for (text, weight) in [
                (commit.subject.as_str(), SUBJECT_WEIGHT),
                (body.as_str(), 1.0),
                (paths.as_str(), 1.0),
            ] {
                for word in terms_of(text) {
                    length += 1.0;
                    if let Some(term) = wanted.get(word.as_str()) {
                        *frequencies.entry(term).or_default() += weight;
                    }
                }
            }
            (frequencies, length)
        })
        .collect::<Vec<_>>();
    let documents_count = commits.len() as u64;
    let average = documents.iter().map(|(_, length)| length).sum::<f64>()
        / f64::from(u32::try_from(commits.len().max(1)).unwrap_or(u32::MAX));
    let idf = wanted
        .iter()
        .map(|term| {
            let holding = documents
                .iter()
                .filter(|(frequencies, _)| frequencies.contains_key(term))
                .count() as u64;
            (*term, (holding, bm25_idf(documents_count, holding)))
        })
        .collect::<BTreeMap<_, _>>();
    let mut scored = documents
        .iter()
        .enumerate()
        .filter(|(_, (frequencies, _))| !frequencies.is_empty())
        .map(|(index, (frequencies, length))| {
            let score = frequencies
                .iter()
                .map(|(term, frequency)| bm25_term(idf[term].1, *frequency, *length, average))
                .sum::<f64>();
            (index, score)
        })
        .collect::<Vec<_>>();
    let matching = scored.len();
    // `sort_by` is stable: equal scores keep the log's order, newest first.
    scored.sort_by(|left, right| right.1.total_cmp(&left.1));
    Ranking {
        hits: scored
            .into_iter()
            .take(limit)
            .map(|(index, _)| &commits[index])
            .collect(),
        matching,
        unmatched: terms
            .iter()
            .filter(|term| idf[term.as_str()].0 == 0)
            .cloned()
            .collect(),
    }
}

/// `text` on one line, cut to `limit` characters.
fn one_line(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let mut cut = flat.chars().take(limit).collect::<String>();
    cut.push_str("...");
    cut
}

/// The pieces of one run of letters and digits where its spelling shows a
/// seam: `parseHeader` is `parse` and `Header`, `HTTPServer` is `HTTP` and
/// `Server`, `utf8` is `utf` and `8`. A run with no seam is one piece.
fn spelling_parts(run: &str) -> Vec<String> {
    let characters = run.chars().collect::<Vec<_>>();
    let mut parts = Vec::new();
    let mut current = String::new();
    for (index, character) in characters.iter().enumerate() {
        if let Some(previous) = current.chars().last() {
            let next = characters.get(index + 1);
            let seam = (previous.is_lowercase() && character.is_uppercase())
                || (previous.is_alphabetic() != character.is_alphabetic())
                || (previous.is_uppercase()
                    && character.is_uppercase()
                    && next.is_some_and(|next| next.is_lowercase()));
            if seam {
                parts.push(std::mem::take(&mut current));
            }
        }
        current.push(*character);
    }
    parts.push(current);
    parts
}

/// The terms of a text, for the query and for every commit alike: the words
/// `harness_types::readout::words` reads (`parseheader` for `parseHeader`),
/// and the pieces of any word spelled with a seam (`parse` and `header`), so
/// `parseHeader` and `parse_header` find each other; and the joined form of a
/// name written with `_` or `-` (`parseheader` for `parse_header`), so the
/// joined word finds it too. `words` lower-cases the text before it splits,
/// which loses the seams; this reads the original case.
pub(crate) fn terms_of(text: &str) -> Vec<String> {
    let mut terms = words(text);
    for name in text.split(|character: char| {
        !(character.is_alphanumeric() || character == '_' || character == '-')
    }) {
        let pieces = name
            .split(['_', '-'])
            .filter(|piece| !piece.is_empty())
            .collect::<Vec<_>>();
        if pieces.len() > 1 {
            terms.push(pieces.concat().to_lowercase());
        }
    }
    for run in text
        .split(|character: char| !character.is_alphanumeric())
        .filter(|run| !run.is_empty())
    {
        let parts = spelling_parts(run);
        if parts.len() > 1 {
            terms.extend(
                parts
                    .iter()
                    .map(|part| part.to_lowercase())
                    .filter(|part| part.chars().count() >= 2),
            );
        }
    }
    terms
}

/// A path git printed from the repository's root, as the workspace's other
/// tools name it. `prefix` is where the workspace lies in the repository
/// (`sub/dir/`, empty at the root). A file outside the workspace is not
/// dropped: it is shown as `../x` and marked. A rename (`a => b`) is left as
/// git printed it.
pub(crate) fn workspace_path(prefix: &str, file: &str) -> String {
    if prefix.is_empty() || file.contains(" => ") {
        return file.to_owned();
    }
    if let Some(inside) = file.strip_prefix(prefix) {
        return inside.to_owned();
    }
    let place = prefix.trim_end_matches('/').split('/').collect::<Vec<_>>();
    let parts = file.split('/').collect::<Vec<_>>();
    let common = place
        .iter()
        .zip(&parts[..parts.len() - 1])
        .take_while(|(left, right)| left == right)
        .count();
    format!(
        "{OUTSIDE}{}{}",
        "../".repeat(place.len() - common),
        parts[common..].join("/")
    )
}

/// Where the workspace lies in its repository (`git rev-parse --show-prefix`):
/// `sub/dir/`, or empty at the root or when git cannot say.
async fn workspace_prefix(
    root: &std::path::Path,
    cancellation: CancellationToken,
    spool: &ProcessSpoolConfig,
) -> String {
    let args = ["rev-parse", "--show-prefix"].map(str::to_owned);
    match process::run_internal_git(root, &args, GIT_TIMEOUT_MS, cancellation, spool).await {
        Ok(output) if output.exit_code == Some(0) => output.stdout.trim().replace('\\', "/"),
        _ => String::new(),
    }
}

fn short(hash: &str) -> &str {
    hash.get(..SHORT_HASH).unwrap_or(hash)
}

/// The answer of a `git_search`.
pub(crate) fn render_search(
    query: &str,
    ranking: &Ranking<'_>,
    scanned: usize,
    cut: Option<&str>,
    prefix: &str,
) -> String {
    let history = cut.map_or_else(
        || format!("{scanned} commits searched"),
        |reason| format!("{scanned} commits searched ({reason})"),
    );
    if ranking.hits.is_empty() {
        return format!(
            "no commit message matches {query:?} ({history}; changed file names were searched too). Try other words the change would have used, or git_log for the latest commits."
        );
    }
    let mut text = format!(
        "{} of {} matching commits for {query:?} ({history}), best first; read one with git_show:\n",
        ranking.hits.len(),
        ranking.matching
    );
    if !ranking.unmatched.is_empty() {
        let _ = writeln!(text, "no commit holds: {}", ranking.unmatched.join(", "));
    }
    for (index, commit) in ranking.hits.iter().enumerate() {
        let date = commit.date.get(..10).unwrap_or(&commit.date);
        let _ = writeln!(
            text,
            "{}. {} {date} {} - {}",
            index + 1,
            short(&commit.hash),
            commit.author,
            commit.subject
        );
        let body = one_line(&commit.body, SEARCH_BODY_SHOWN);
        if !body.is_empty() {
            let _ = writeln!(text, "   {body}");
        }
        if !commit.files.is_empty() {
            let shown = commit
                .files
                .iter()
                .take(SEARCH_FILES_SHOWN)
                .map(|file| workspace_path(prefix, file))
                .collect::<Vec<_>>()
                .join(", ");
            let more = commit.files.len().saturating_sub(SEARCH_FILES_SHOWN);
            if more > 0 {
                let _ = writeln!(text, "   files: {shown}, +{more} more");
            } else {
                let _ = writeln!(text, "   files: {shown}");
            }
        }
    }
    text
}

/// A git call may need more output than the host keeps by default; this
/// spool keeps up to [`SCAN_BYTES`] of it in memory.
fn wide(spool: &ProcessSpoolConfig) -> ProcessSpoolConfig {
    ProcessSpoolConfig::new(
        spool.root(),
        SpoolLimits {
            max_capture_bytes: SCAN_BYTES as u64,
            head_preview_bytes: SCAN_BYTES,
            ..spool.limits()
        },
    )
}

fn answer(operation: &str, text: &str, truncated: bool) -> ToolOutput {
    ToolOutput::Git {
        operation: operation.to_owned(),
        output: redact_text(text),
        truncated,
        exit_code: Some(0),
    }
}

/// Run `git_search`.
pub(crate) async fn search(
    root: &std::path::Path,
    query: &str,
    path: Option<&str>,
    limit: Option<u32>,
    cancellation: CancellationToken,
    spool: &ProcessSpoolConfig,
) -> Result<ToolOutput, HarnessError> {
    let terms = terms_of(query)
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Ok(answer(
            "search",
            &format!(
                "git_search needs words to look for: {query:?} holds none (a word is two or more letters or digits; parse_header is the words parse and header)."
            ),
            false,
        ));
    }
    let output = process::run_internal_git(
        root,
        &search_arguments(path),
        GIT_TIMEOUT_MS,
        cancellation.clone(),
        &wide(spool),
    )
    .await?;
    if output.timed_out {
        return Ok(answer(
            "search",
            "reading the commit history took longer than 30 seconds and was stopped; narrow the search with a path",
            true,
        ));
    }
    // Not a repository, or a repository with no commit yet: git says which.
    if output.exit_code != Some(0) {
        return Ok(git_output("search", output));
    }
    let mut commits = parse_log(&output.stdout);
    let cut = if output.stdout_truncated {
        // The last record may be half there; `parse_log` kept it only if it
        // was whole, and the count is what was read.
        Some(format!(
            "the log text was larger than {} MiB, so only the newest {} commits were read",
            SCAN_BYTES / (1024 * 1024),
            commits.len()
        ))
    } else if commits.len() >= SEARCH_SCAN_COMMITS {
        commits.truncate(SEARCH_SCAN_COMMITS);
        Some(format!(
            "the history was cut at the newest {SEARCH_SCAN_COMMITS}; older commits were not searched"
        ))
    } else {
        None
    };
    let limit = limit.unwrap_or(GIT_SEARCH_DEFAULT_LIMIT) as usize;
    let ranking = rank(&commits, &terms, limit);
    let prefix = workspace_prefix(root, cancellation, spool).await;
    let text = render_search(query, &ranking, commits.len(), cut.as_deref(), &prefix);
    Ok(answer("search", &text, cut.is_some()))
}

/// The common start of a `git show` argument list.
fn show_base() -> Vec<String> {
    [
        "--literal-pathspecs",
        "-c",
        "core.quotepath=off",
        "show",
        "--no-color",
        "--no-show-signature",
        "--no-ext-diff",
        "--no-textconv",
        // A merge shows what it changed against its first parent.
        "-m",
        "--first-parent",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// `git show` of the hash, author, date and message alone. It takes no path:
/// git prints nothing, not even the message, for a commit that did not touch
/// the path.
pub(crate) fn show_header_arguments(hash: &str) -> Vec<String> {
    let mut args = show_base();
    args.push("--no-patch".to_owned());
    args.push("--format=%H%x1f%an%x1f%aI%x1f%B%x1f".to_owned());
    args.push(hash.to_owned());
    args
}

/// `git show` of the per-file added and removed line counts.
pub(crate) fn show_numstat_arguments(hash: &str, path: Option<&str>) -> Vec<String> {
    let mut args = show_base();
    args.push("--numstat".to_owned());
    args.push("--format=".to_owned());
    args.push(hash.to_owned());
    if let Some(path) = path {
        args.push("--".to_owned());
        args.push(path.to_owned());
    }
    args
}

/// `git show` of the diff alone.
pub(crate) fn show_patch_arguments(hash: &str, path: Option<&str>) -> Vec<String> {
    let mut args = show_base();
    args.push("--patch".to_owned());
    // Paths in the diff as the workspace names them, as the file list says.
    args.push("--relative".to_owned());
    args.push("--format=".to_owned());
    args.push(hash.to_owned());
    if let Some(path) = path {
        args.push("--".to_owned());
        args.push(path.to_owned());
    }
    args
}

/// The page of `text` that starts at byte `offset`, at most `length` bytes,
/// ending at a line end when one is in reach, and both ends on character
/// boundaries. Returns the page's start and end.
pub(crate) fn page_bounds(text: &str, offset: u64, length: u32) -> (usize, usize) {
    let total = text.len();
    let mut start = usize::try_from(offset).unwrap_or(usize::MAX).min(total);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let mut end = start.saturating_add(length as usize).min(total);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end < total {
        // Stop after the last whole line of the page, unless the page is one line.
        if let Some(newline) = text[start..end].rfind('\n') {
            end = start + newline + 1;
        } else if end == start {
            // One character wider than `length` is better than no progress.
            end = start + text[start..].chars().next().map_or(0, char::len_utf8);
        }
    }
    (start, end)
}

/// What `git show` printed before the diff, split into its parts.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Header {
    pub hash: String,
    pub author: String,
    pub date: String,
    pub message: String,
    /// Per file: lines added and removed (`None` for a binary file), and its path.
    pub files: Vec<(Option<(u64, u64)>, String)>,
}

/// The files of a `--numstat` listing: lines added and removed (`None` for a
/// binary file) and the path.
pub(crate) fn parse_numstat(text: &str) -> Vec<(Option<(u64, u64)>, String)> {
    text.lines()
        .filter_map(|line| {
            let mut columns = line.splitn(3, '\t');
            let added = columns.next()?;
            let removed = columns.next()?;
            let path = columns.next()?.to_owned();
            Some((added.parse().ok().zip(removed.parse().ok()), path))
        })
        .collect()
}

pub(crate) fn parse_header(text: &str) -> Option<Header> {
    let mut fields = text.trim_start().splitn(5, '\u{1f}');
    let hash = fields.next()?.trim().to_owned();
    let author = fields.next()?.trim().to_owned();
    let date = fields.next()?.trim().to_owned();
    let message = fields.next()?.trim().to_owned();
    let files = parse_numstat(fields.next()?);
    Some(Header {
        hash,
        author,
        date,
        message,
        files,
    })
}

/// The top of a first `git_show` page: who, when, the whole message, and the
/// files the commit changed with their line counts.
fn show_head(header: &Header, path: Option<&str>) -> String {
    let mut text = String::new();
    let _ = write!(
        text,
        "commit {}\nAuthor: {}\nDate:   {}\n\n",
        header.hash, header.author, header.date
    );
    let message = header
        .message
        .chars()
        .take(SHOW_MESSAGE_CHARS)
        .collect::<String>();
    for line in message.lines() {
        let _ = writeln!(text, "    {line}");
    }
    if header.message.chars().count() > SHOW_MESSAGE_CHARS {
        text.push_str("    [message cut]\n");
    }
    let scope = path.map_or_else(String::new, |path| format!(" under {path}"));
    if header.files.is_empty() {
        let _ = writeln!(text, "\nno file changed{scope} in this commit");
        text.push('\n');
        return text;
    }
    let counted = header.files.iter().filter_map(|(counts, _)| *counts);
    let added = counted.clone().map(|counts| counts.0).sum::<u64>();
    let removed = counted.map(|counts| counts.1).sum::<u64>();
    let count = header.files.len();
    let noun = if count == 1 { "file" } else { "files" };
    let _ = writeln!(
        text,
        "\n{count} {noun} changed{scope} (+{added} -{removed}):"
    );
    for (counts, file) in header.files.iter().take(SHOW_FILES_SHOWN) {
        match counts {
            Some((added, removed)) => {
                let _ = writeln!(text, "  +{added} -{removed}  {file}");
            }
            None => {
                let _ = writeln!(text, "  binary  {file}");
            }
        }
    }
    if count > SHOW_FILES_SHOWN {
        let _ = writeln!(text, "  +{} more files", count - SHOW_FILES_SHOWN);
    }
    if header
        .files
        .iter()
        .any(|(_, file)| file.starts_with(OUTSIDE))
    {
        text.push_str("  (the diff below shows only the files inside the workspace)\n");
    }
    text.push('\n');
    text
}

/// The answer of a `git_show`: the page of `diff` at `offset`, and whether
/// more of it follows.
pub(crate) fn render_show(
    header: &Header,
    path: Option<&str>,
    diff: &str,
    scan_cut: bool,
    offset: u64,
    length: u32,
) -> (String, bool) {
    let (start, end) = page_bounds(diff, offset, length);
    let total = diff.len();
    let mut text = if offset == 0 {
        show_head(header, path)
    } else {
        format!("commit {}\n", header.hash)
    };
    let more = end < total;
    if total == 0 {
        text.push_str("no diff to show (an empty commit, or nothing under that path)\n");
    } else if offset > 0 && usize::try_from(offset).map_or(true, |offset| offset >= total) {
        let _ = writeln!(
            text,
            "offset {offset} is past the end of the diff ({total} bytes)"
        );
    } else {
        let _ = writeln!(text, "diff, bytes {start}-{end} of {total}:");
        text.push_str(&diff[start..end]);
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    if more {
        let scope = path.map_or_else(String::new, |path| format!(", path {path:?}"));
        let _ = writeln!(
            text,
            "[the diff goes on: call git_show with commit \"{}\", offset {end}{scope} to read the rest]",
            header.hash
        );
    }
    if scan_cut {
        let mebibytes = SCAN_BYTES / (1024 * 1024);
        let _ = writeln!(
            text,
            "[this diff is larger than {mebibytes} MiB: only its first {mebibytes} MiB can be read; narrow it with path]"
        );
    }
    (text, more)
}

/// Run `git_show`.
pub(crate) async fn show(
    root: &std::path::Path,
    commit: &str,
    path: Option<&str>,
    offset: u64,
    length: u32,
    cancellation: CancellationToken,
    spool: &ProcessSpoolConfig,
) -> Result<ToolOutput, HarnessError> {
    check_revision(commit)?;
    let spool = wide(spool);
    // Resolved first, so that nothing but a full commit id is ever read as an
    // argument of the calls below.
    let resolved = process::run_internal_git(
        root,
        &[
            "rev-parse".to_owned(),
            "--verify".to_owned(),
            "--quiet".to_owned(),
            format!("{commit}^{{commit}}"),
        ],
        GIT_TIMEOUT_MS,
        cancellation.clone(),
        &spool,
    )
    .await?;
    if resolved
        .stderr
        .to_ascii_lowercase()
        .contains("not a git repository")
    {
        return Ok(git_output("show", resolved));
    }
    let hash = resolved.stdout.trim().to_owned();
    if resolved.exit_code != Some(0)
        || hash.is_empty()
        || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            format!(
                "git_show: no commit named {commit:?} in this repository; find one with git_search or git_log"
            ),
        ));
    }
    let header = process::run_internal_git(
        root,
        &show_header_arguments(&hash),
        GIT_TIMEOUT_MS,
        cancellation.clone(),
        &spool,
    )
    .await?;
    if header.exit_code != Some(0) {
        return Ok(git_output("show", header));
    }
    let Some(mut header) = parse_header(&header.stdout) else {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "git_show: git's answer could not be read",
        ));
    };
    let numstat = process::run_internal_git(
        root,
        &show_numstat_arguments(&hash, path),
        GIT_TIMEOUT_MS,
        cancellation.clone(),
        &spool,
    )
    .await?;
    if numstat.exit_code != Some(0) {
        return Ok(git_output("show", numstat));
    }
    let prefix = workspace_prefix(root, cancellation.clone(), &spool).await;
    header.files = parse_numstat(&numstat.stdout)
        .into_iter()
        .map(|(counts, file)| (counts, workspace_path(&prefix, &file)))
        .collect();
    let diff_run = process::run_internal_git(
        root,
        &show_patch_arguments(&hash, path),
        GIT_TIMEOUT_MS,
        cancellation,
        &spool,
    )
    .await?;
    if diff_run.exit_code != Some(0) {
        return Ok(git_output("show", diff_run));
    }
    let diff = diff_run.stdout.trim_start_matches('\n');
    let (text, more) = render_show(
        &header,
        path,
        diff,
        diff_run.stdout_truncated,
        offset,
        length,
    );
    Ok(answer("show", &text, more))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(hash: &str, subject: &str, body: &str, files: &[&str]) -> Commit {
        Commit {
            hash: hash.to_owned(),
            date: "2026-11-03T10:00:00+00:00".to_owned(),
            author: "Ann".to_owned(),
            subject: subject.to_owned(),
            body: body.to_owned(),
            files: files.iter().map(|file| (*file).to_owned()).collect(),
        }
    }

    fn terms(query: &str) -> Vec<String> {
        terms_of(query)
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    #[test]
    fn a_revision_is_one_token_git_cannot_read_as_an_option() {
        for good in [
            "abc1234",
            "0123456789abcdef0123456789abcdef01234567",
            "main",
            "v1.2.3",
            "origin/main",
            "HEAD~3",
            "HEAD^",
            "HEAD@{1}",
            "feature/x_y-z",
        ] {
            assert!(check_revision(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "-n",
            "--output=x",
            "abc def",
            "a\tb",
            "a\nb",
            "main..dev",
            "a;b",
            "$(x)",
            "HEAD:file",
            "a*b",
            ":/msg",
            "-",
        ] {
            assert!(check_revision(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_log_record_with_tabs_and_newlines_in_its_message_parses() {
        let text = "\u{1e}aaaa1111\u{1f}2026-01-02T03:04:05+00:00\u{1f}Ann\u{1f}fix\ttabs\u{1f}line one\n\nline\ttwo\u{1f}\n\nsrc/a.rs\nsrc/b.rs\n\u{1e}bbbb2222\u{1f}2026-01-01T00:00:00+00:00\u{1f}Bo\u{1f}second\u{1f}\u{1f}\n\nREADME.md\n";
        let commits = parse_log(text);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hash, "aaaa1111");
        assert_eq!(commits[0].subject, "fix\ttabs");
        assert_eq!(commits[0].body, "line one\n\nline\ttwo");
        assert_eq!(commits[0].files, ["src/a.rs", "src/b.rs"]);
        assert_eq!(commits[1].author, "Bo");
        assert!(commits[1].body.is_empty());
        assert_eq!(commits[1].files, ["README.md"]);
    }

    #[test]
    fn a_record_cut_short_is_dropped() {
        let text = "\u{1e}aaaa1111\u{1f}d\u{1f}Ann\u{1f}whole\u{1f}\u{1f}\n\nx\n\u{1e}bbbb2222\u{1f}d\u{1f}Bo\u{1f}half a mess";
        let commits = parse_log(text);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "whole");
    }

    #[test]
    fn the_commit_that_names_the_identifier_ranks_first() {
        let commits = vec![
            commit("c3", "update docs", "", &["README.md"]),
            commit("c2", "add header parsing tests", "", &["tests/header.rs"]),
            commit(
                "c1",
                "fix parse_header off-by-one",
                "the length was one short",
                &["src/parse.rs"],
            ),
        ];
        let ranking = rank(&commits, &terms("parse_header"), 10);
        assert_eq!(ranking.hits[0].hash, "c1");
        assert_eq!(ranking.matching, 2);
        assert!(ranking.unmatched.is_empty());
    }

    #[test]
    fn a_subject_word_outweighs_a_body_word_and_paths_find_a_file() {
        let commits = vec![
            commit("b", "tidy", "touches the invoice code", &["a.rs"]),
            commit("a", "invoice totals", "", &["a.rs"]),
            commit("p", "tidy again", "", &["billing/invoice.py"]),
        ];
        let ranking = rank(&commits, &terms("invoice"), 10);
        let order = ranking
            .hits
            .iter()
            .map(|commit| commit.hash.as_str())
            .collect::<Vec<_>>();
        assert_eq!(order[0], "a", "{order:?}");
        assert_eq!(ranking.matching, 3, "the path words count");
    }

    #[test]
    fn equal_scores_keep_the_newest_first_and_the_limit_bounds() {
        let commits = (0..6)
            .map(|index| commit(&format!("h{index}"), "same words", "", &[]))
            .collect::<Vec<_>>();
        let ranking = rank(&commits, &terms("same"), 3);
        let order = ranking
            .hits
            .iter()
            .map(|commit| commit.hash.as_str())
            .collect::<Vec<_>>();
        assert_eq!(order, ["h0", "h1", "h2"]);
        assert_eq!(ranking.matching, 6);
    }

    #[test]
    fn a_query_nothing_holds_says_so_and_names_the_missing_words() {
        let commits = vec![commit("a", "fix parser", "", &[])];
        let ranking = rank(&commits, &terms("zebra parser"), 10);
        assert_eq!(ranking.unmatched, ["zebra"]);
        let none = rank(&commits, &terms("zebra"), 10);
        assert!(none.hits.is_empty());
        let text = render_search("zebra", &none, 1, None, "");
        assert!(text.starts_with("no commit message matches"), "{text}");
    }

    #[test]
    fn a_hit_lists_a_few_files_and_counts_the_rest() {
        let files = (0..11)
            .map(|index| format!("f{index}.rs"))
            .collect::<Vec<_>>();
        let refs = files.iter().map(String::as_str).collect::<Vec<_>>();
        let commits = vec![commit(
            "0123456789abcdef",
            "big change",
            "a\nbody   that\nspans lines",
            &refs,
        )];
        let ranking = rank(&commits, &terms("big"), 10);
        let text = render_search("big", &ranking, 1, Some("cut here"), "");
        assert!(
            text.contains("0123456789 2026-11-03 Ann - big change"),
            "{text}"
        );
        assert!(text.contains("a body that spans lines"), "{text}");
        assert!(text.contains("f7.rs, +3 more"), "{text}");
        assert!(!text.contains("f8.rs"), "{text}");
        assert!(text.contains("(1 commits searched (cut here))"), "{text}");
    }

    #[test]
    fn pages_end_on_line_ends_and_character_boundaries() {
        let text = "alpha\nbeta\ngamma\n";
        assert_eq!(page_bounds(text, 0, 8), (0, 6));
        assert_eq!(page_bounds(text, 6, 100), (6, text.len()));
        // One line longer than the page still makes progress.
        assert_eq!(page_bounds("abcdefghij", 0, 4), (0, 4));
        // Never inside a multi-byte character.
        let accents = "ééééé";
        let (start, end) = page_bounds(accents, 1, 3);
        assert!(accents.is_char_boundary(start) && accents.is_char_boundary(end));
        assert!(end > start);
        // An offset past the end is an empty page at the end.
        assert_eq!(page_bounds(text, 999, 10), (text.len(), text.len()));
    }

    #[test]
    fn a_header_reads_message_and_counts() {
        let text = "ffff0000\u{1f}Ann\u{1f}2026-01-02T03:04:05+00:00\u{1f}subject\n\nbody line\u{1f}\n\n3\t1\tsrc/a.rs\n-\t-\timg.png\n";
        let header = parse_header(text).expect("header");
        assert_eq!(header.hash, "ffff0000");
        assert_eq!(header.message, "subject\n\nbody line");
        assert_eq!(
            header.files,
            [
                (Some((3, 1)), "src/a.rs".to_owned()),
                (None, "img.png".to_owned())
            ]
        );
        let (shown, more) = render_show(&header, None, "diff --git a b\n+x\n", false, 0, 1000);
        assert!(shown.contains("2 files changed (+3 -1):"), "{shown}");
        assert!(shown.contains("binary  img.png"), "{shown}");
        assert!(shown.contains("diff --git a b"), "{shown}");
        assert!(!more);
    }

    #[test]
    fn a_long_diff_names_the_offset_that_reads_on() {
        let header = Header {
            hash: "ffff0000".to_owned(),
            author: "Ann".to_owned(),
            date: "d".to_owned(),
            message: "m".to_owned(),
            files: Vec::new(),
        };
        let diff = "one\ntwo\nthree\nfour\n";
        let (first, more) = render_show(&header, None, diff, false, 0, 10);
        assert!(more);
        assert!(first.contains("offset 8"), "{first}");
        let (rest, more) = render_show(&header, None, diff, false, 8, 100);
        assert!(!more);
        assert!(rest.contains("three\nfour"), "{rest}");
        assert!(
            !rest.contains("Author:"),
            "a later page repeats no header: {rest}"
        );
    }

    // The tests below run the real git over a temporary repository.

    fn git(dir: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=Tess"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git starts");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Commit `file` holding `content`, under `message`.
    fn commit_file(dir: &std::path::Path, file: &str, content: &str, message: &str) {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, content).expect("write");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", message]);
    }

    /// Three commits: the second names `parse_header`.
    fn history() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("dir");
        git(dir.path(), &["init", "-q"]);
        commit_file(dir.path(), "README.md", "hello\n", "start the project");
        commit_file(
            dir.path(),
            "src/parse.rs",
            "fn parse_header() { let n = 1; }\n",
            "fix parse_header off-by-one\n\nthe header length was one short",
        );
        commit_file(dir.path(), "docs/guide.md", "guide\n", "write the guide");
        dir
    }

    async fn search_in(
        dir: &std::path::Path,
        query: &str,
        path: Option<&str>,
        limit: Option<u32>,
    ) -> String {
        let output = search(
            dir,
            query,
            path,
            limit,
            CancellationToken::new(),
            &ProcessSpoolConfig::default(),
        )
        .await
        .expect("search runs");
        let ToolOutput::Git { output, .. } = output else {
            panic!("a git output");
        };
        output
    }

    async fn show_in(
        dir: &std::path::Path,
        commit: &str,
        path: Option<&str>,
        offset: u64,
        length: u32,
    ) -> Result<String, HarnessError> {
        let output = show(
            dir,
            commit,
            path,
            offset,
            length,
            CancellationToken::new(),
            &ProcessSpoolConfig::default(),
        )
        .await?;
        let ToolOutput::Git { output, .. } = output else {
            panic!("a git output");
        };
        Ok(output)
    }

    #[tokio::test]
    async fn git_search_finds_the_commit_a_message_names_first() {
        let dir = history();
        let found = search_in(dir.path(), "parse_header", None, None).await;
        let first = found
            .lines()
            .find(|line| line.starts_with("1. "))
            .expect("a first hit");
        assert!(first.contains("fix parse_header off-by-one"), "{found}");
        assert!(found.contains("the header length was one short"), "{found}");
        assert!(found.contains("files: src/parse.rs"), "{found}");
        assert!(!found.contains("write the guide"), "{found}");
    }

    #[tokio::test]
    async fn git_search_path_and_limit_bound_the_answer() {
        let dir = history();
        // Only commits that touched the path are read.
        let docs = search_in(dir.path(), "the", Some("docs"), None).await;
        assert!(docs.contains("write the guide"), "{docs}");
        assert!(!docs.contains("start the project"), "{docs}");
        assert!(docs.contains("1 commits searched"), "{docs}");
        // Three commits hold `the`; the limit keeps one.
        let one = search_in(dir.path(), "the", None, Some(1)).await;
        assert!(one.starts_with("1 of 3 matching commits"), "{one}");
        assert_eq!(
            one.lines().filter(|line| line.contains(" Tess - ")).count(),
            1,
            "{one}"
        );
    }

    #[tokio::test]
    async fn git_search_says_when_no_commit_matches() {
        let dir = history();
        let none = search_in(dir.path(), "zebra", None, None).await;
        assert!(none.starts_with("no commit message matches"), "{none}");
        let blank = search_in(dir.path(), "- -", None, None).await;
        assert!(blank.contains("needs words"), "{blank}");
    }

    #[tokio::test]
    async fn git_search_finds_a_file_by_its_name() {
        let dir = history();
        let found = search_in(dir.path(), "src/parse.rs", None, None).await;
        assert!(found.contains("fix parse_header off-by-one"), "{found}");
    }

    #[tokio::test]
    async fn both_tools_say_plainly_when_there_is_no_repository() {
        let dir = tempfile::tempdir().expect("dir");
        let searched = search_in(dir.path(), "anything", None, None).await;
        assert!(searched.contains("not a Git repository"), "{searched}");
        assert!(searched.len() < 300, "{searched}");
        let shown = show_in(dir.path(), "HEAD", None, 0, 1000)
            .await
            .expect("an answer, not an error");
        assert!(shown.contains("not a Git repository"), "{shown}");
    }

    #[tokio::test]
    async fn git_show_prints_message_counts_and_diff_of_the_right_commit() {
        let dir = history();
        let shown = show_in(dir.path(), "HEAD~1", None, 0, 16 * 1024)
            .await
            .expect("shows");
        assert!(shown.starts_with("commit "), "{shown}");
        assert!(shown.contains("Author: Tess"), "{shown}");
        assert!(shown.contains("fix parse_header off-by-one"), "{shown}");
        assert!(shown.contains("the header length was one short"), "{shown}");
        assert!(shown.contains("1 file changed (+1 -0):"), "{shown}");
        assert!(shown.contains("+1 -0  src/parse.rs"), "{shown}");
        assert!(
            shown.contains("+fn parse_header() { let n = 1; }"),
            "{shown}"
        );
        assert!(!shown.contains("guide"), "{shown}");
        // A path that the commit did not touch leaves the diff empty.
        let none = show_in(dir.path(), "HEAD~1", Some("docs"), 0, 1000)
            .await
            .expect("shows");
        assert!(none.contains("no file changed under docs"), "{none}");
        assert!(none.contains("no diff to show"), "{none}");
    }

    #[tokio::test]
    async fn git_show_accepts_a_short_hash_and_the_root_commit() {
        let dir = history();
        let log = std::process::Command::new("git")
            .args(["log", "--format=%H", "-n3"])
            .current_dir(dir.path())
            .output()
            .expect("log");
        let hashes = String::from_utf8_lossy(&log.stdout)
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let by_prefix = show_in(dir.path(), &hashes[1][..7], None, 0, 4000)
            .await
            .expect("a short hash resolves");
        assert!(
            by_prefix.contains(&format!("commit {}", hashes[1])),
            "{by_prefix}"
        );
        let root = show_in(dir.path(), &hashes[2], None, 0, 4000)
            .await
            .expect("the root commit shows");
        assert!(root.contains("+hello"), "{root}");
    }

    #[tokio::test]
    async fn git_show_refuses_an_option_or_a_space_and_names_an_unknown_commit() {
        let dir = history();
        for bad in ["--output=evil", "-p", "HEAD --stat", "a b", "HEAD..HEAD~1"] {
            let error = show_in(dir.path(), bad, None, 0, 1000)
                .await
                .expect_err(bad);
            assert_eq!(error.code(), ErrorCode::InvalidPayload, "{bad}");
        }
        assert!(!dir.path().join("evil").exists());
        let error = show_in(dir.path(), "deadbeefdeadbeef", None, 0, 1000)
            .await
            .expect_err("no such commit");
        assert!(error.to_string().contains("no commit named"), "{error}");
        let error = show_in(dir.path(), "no-such-branch", None, 0, 1000)
            .await
            .expect_err("no such ref");
        assert!(error.to_string().contains("no commit named"), "{error}");
    }

    /// The offset a page names to read on, when it names one.
    fn next_offset(page: &str) -> Option<u64> {
        let (_, after) = page.split_once("the diff goes on")?;
        let (_, after) = after.split_once("offset ")?;
        after
            .split(|character: char| !character.is_ascii_digit())
            .next()?
            .parse()
            .ok()
    }

    #[tokio::test]
    async fn git_show_pages_a_long_diff_to_its_end() {
        let dir = tempfile::tempdir().expect("dir");
        git(dir.path(), &["init", "-q"]);
        let body = (0..400).fold(String::new(), |mut body, index| {
            let _ = writeln!(body, "line number {index}");
            body
        });
        commit_file(dir.path(), "big.txt", &body, "add a long file");
        let first = show_in(dir.path(), "HEAD", None, 0, 1000)
            .await
            .expect("first page");
        assert!(first.contains("add a long file"), "{first}");
        assert!(first.contains("the diff goes on: call git_show"), "{first}");
        let mut collected = String::new();
        let mut offset = 0_u64;
        let mut pages = 0;
        loop {
            let page = show_in(dir.path(), "HEAD", None, offset, 1000)
                .await
                .expect("a page");
            pages += 1;
            for line in page.lines() {
                if let Some(text) = line.strip_prefix('+').filter(|_| !line.starts_with("+++")) {
                    collected.push_str(text);
                    collected.push('\n');
                }
            }
            match next_offset(&page) {
                Some(next) => offset = next,
                None => break,
            }
        }
        assert!(pages > 3, "{pages} pages");
        assert_eq!(collected, body, "every line comes back exactly once");
        // Past the end says so.
        let past = show_in(dir.path(), "HEAD", None, 10_000_000, 1000)
            .await
            .expect("past the end");
        assert!(past.contains("past the end of the diff"), "{past}");
    }

    #[test]
    fn a_joined_word_finds_a_name_written_with_underscores_or_dashes() {
        let terms = super::terms_of("fix parse_header and read-only-mode");
        assert!(terms.iter().any(|term| term == "parseheader"), "{terms:?}");
        assert!(terms.iter().any(|term| term == "readonlymode"), "{terms:?}");
        assert!(terms.iter().any(|term| term == "parse"), "{terms:?}");
        let plain = super::terms_of("fix the header");
        assert!(!plain.iter().any(|term| term == "fixthe"), "{plain:?}");
    }

    #[test]
    fn camel_case_and_snake_case_spell_the_same_terms() {
        let camel = terms_of("parseHeader");
        assert!(camel.contains(&"parseheader".to_owned()), "{camel:?}");
        assert!(camel.contains(&"parse".to_owned()), "{camel:?}");
        assert!(camel.contains(&"header".to_owned()), "{camel:?}");
        let snake = terms_of("parse_header");
        assert_eq!(snake, ["parse", "header", "parseheader"]);
        // An acronym, and a digit seam.
        let acronym = terms_of("HTTPServer");
        assert!(acronym.contains(&"http".to_owned()), "{acronym:?}");
        assert!(acronym.contains(&"server".to_owned()), "{acronym:?}");
        let digits = terms_of("utf8Decode");
        assert!(digits.contains(&"utf".to_owned()), "{digits:?}");
        assert!(digits.contains(&"decode".to_owned()), "{digits:?}");
        // A plain word is just itself.
        assert_eq!(terms_of("header"), ["header"]);
    }

    #[test]
    fn parse_header_and_parse_header_in_camel_case_find_each_other() {
        let commits = vec![
            commit("c3", "update docs", "", &["README.md"]),
            commit("c2", "fix parse_header", "", &["src/a.rs"]),
            commit("c1", "fix parseHeader off-by-one", "", &["src/b.rs"]),
        ];
        for (query, found) in [
            ("parse_header", ["c2", "c1"]),
            ("parseheader", ["c1", "c1"]),
            ("parseHeader", ["c2", "c1"]),
        ] {
            let ranking = rank(&commits, &terms(query), 10);
            let hashes = ranking
                .hits
                .iter()
                .map(|commit| commit.hash.as_str())
                .collect::<Vec<_>>();
            for hash in found {
                assert!(hashes.contains(&hash), "{query}: {hashes:?}");
            }
            assert!(!hashes.contains(&"c3"), "{query}: {hashes:?}");
        }
    }

    #[test]
    fn a_path_is_named_from_the_workspace_and_an_outside_one_is_marked() {
        assert_eq!(workspace_path("", "a/b.rs"), "a/b.rs");
        assert_eq!(workspace_path("app/", "app/src/a.rs"), "src/a.rs");
        assert_eq!(
            workspace_path("app/", "other/x.rs"),
            "(outside the workspace) ../other/x.rs"
        );
        assert_eq!(
            workspace_path("app/", "README.md"),
            "(outside the workspace) ../README.md"
        );
        assert_eq!(
            workspace_path("app/lib/", "app/src/y.rs"),
            "(outside the workspace) ../src/y.rs"
        );
        assert_eq!(
            workspace_path("app/lib/", "app/lib2/y.rs"),
            "(outside the workspace) ../lib2/y.rs"
        );
        // A rename is left as git printed it.
        assert_eq!(workspace_path("app/", "a => b"), "a => b");
    }

    /// A repository whose workspace is its `app` folder: one commit touches a
    /// file there and one outside it, another only the one outside.
    fn nested() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("dir");
        git(dir.path(), &["init", "-q"]);
        commit_file(dir.path(), "README.md", "hello\n", "start");
        commit_file(
            dir.path(),
            "app/src/render.rs",
            "fn render() {}\n",
            "add the renderer",
        );
        std::fs::write(dir.path().join("other.txt"), "x\n").expect("write");
        std::fs::write(
            dir.path().join("app/src/render.rs"),
            "fn render() { draw(); }\n",
        )
        .expect("write");
        git(dir.path(), &["add", "-A"]);
        git(
            dir.path(),
            &["commit", "-q", "-m", "renderer draws and notes"],
        );
        dir
    }

    #[tokio::test]
    async fn a_workspace_below_the_repository_root_sees_its_own_paths() {
        let dir = nested();
        let workspace = dir.path().join("app");
        let found = search_in(&workspace, "renderer", None, None).await;
        assert!(found.contains("files: src/render.rs"), "{found}");
        assert!(!found.contains("app/src/render.rs"), "{found}");
        assert!(
            found.contains("(outside the workspace) ../other.txt"),
            "a file outside is marked, not dropped: {found}"
        );
        let shown = show_in(&workspace, "HEAD", None, 0, 16 * 1024)
            .await
            .expect("shows");
        assert!(shown.contains("+1 -1  src/render.rs"), "{shown}");
        assert!(
            shown.contains("(outside the workspace) ../other.txt"),
            "{shown}"
        );
        assert!(
            shown.contains("shows only the files inside the workspace"),
            "{shown}"
        );
        assert!(
            shown.contains("diff --git a/src/render.rs b/src/render.rs"),
            "{shown}"
        );
        assert!(!shown.contains("other.txt b/other.txt"), "{shown}");
        // A path is read from the workspace too.
        let scoped = search_in(&workspace, "renderer", Some("src"), None).await;
        assert!(scoped.contains("files: src/render.rs"), "{scoped}");
    }
}
