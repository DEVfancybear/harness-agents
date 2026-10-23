//! `find_files`: a file found by its name, the way fsearch's (and fff-mcp's)
//! `find_files` finds one for a coding agent.
//!
//! `glob` matched a bare word as a substring of the name and listed the newest
//! files first, so `mian.rs` found nothing and `config loader` found nothing
//! either. Here:
//!
//! - the files matching every word somewhere in their path, else those
//!   matching the most words (`harness_types::search`); a word forgives a
//!   typo (one from five letters, two from eight; a short word two swapped
//!   letters);
//! - results come best first: the name itself, then a name that starts with the
//!   word, contains it, a folder that contains it, then a match through a typo;
//!   at equal scores what git says is changed, then the newest;
//! - filters go inline: `*.rs`, `src/`, `src/**/*.rs`, `!test/`, `git:modified`;
//! - each hit carries its git status where it is not clean;
//! - with no words, what git says is changed comes first, then the newest;
//! - no name matching a word, the files whose text holds the query or the
//!   most of its words, and the result says so.

use std::{
    collections::HashMap,
    path::Path,
    process::{Command, Stdio},
};

use globset::{Glob, GlobMatcher};
use harness_types::{ErrorCode, HarnessError};

use harness_types::search::{Coverage, match_words};

use crate::{
    contracts::FoundFile,
    walk::{WalkFile, relative_text},
    workspace::tool_walk,
};

/// How many files one call lists unless it asks for another number.
pub(crate) const FIND_DEFAULT_LIMIT: u32 = 30;
/// The most files one call lists.
pub(crate) const FIND_MAX_LIMIT: u32 = 200;
/// The most files a fallback to the files' text lists.
const CONTENT_FALLBACK_FILES: usize = 5;

pub(crate) struct FindOutput {
    pub files: Vec<FoundFile>,
    pub truncated: bool,
    pub note: Option<String>,
}

/// What git says about a path that is not clean.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "git's own flags for one path, read as they are"
)]
struct GitState {
    staged: bool,
    unstaged: bool,
    untracked: bool,
    conflict: bool,
}

impl GitState {
    const fn label(self) -> &'static str {
        if self.conflict {
            "conflict"
        } else if self.untracked {
            "untracked"
        } else if self.unstaged {
            "modified"
        } else {
            "staged"
        }
    }
}

/// `git:` filters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GitFilter {
    /// Any tracked change, staged or not.
    Modified,
    Staged,
    Untracked,
    /// Anything git does not call clean.
    Changed,
    Conflict,
}

impl GitFilter {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "modified" => Self::Modified,
            "staged" => Self::Staged,
            "untracked" => Self::Untracked,
            "changed" | "dirty" => Self::Changed,
            "conflict" => Self::Conflict,
            _ => return None,
        })
    }

    fn admits(self, state: Option<GitState>) -> bool {
        let Some(state) = state else {
            return false;
        };
        match self {
            Self::Modified => !state.untracked && (state.staged || state.unstaged),
            Self::Staged => state.staged,
            Self::Untracked => state.untracked,
            Self::Changed => true,
            Self::Conflict => state.conflict,
        }
    }
}

/// A filter on the path: a glob, a folder, or text the path contains.
enum Pattern {
    /// Matched against the whole root-relative path when it holds a `/`,
    /// else against the file name (`*.rs`).
    Glob { matcher: GlobMatcher, whole: bool },
    /// `src/`: the path runs through a folder of that name (or those names).
    Folder(String),
    /// `!test`: the path contains it.
    Text(String),
}

impl Pattern {
    fn parse(text: &str) -> Result<Self, HarnessError> {
        if text.contains(['*', '?', '[', '{']) {
            let matcher = Glob::new(text)
                .map_err(|error| {
                    HarnessError::new(
                        ErrorCode::InvalidPayload,
                        format!("find_files filter {text:?} is not a glob: {error}"),
                    )
                })?
                .compile_matcher();
            return Ok(Self::Glob {
                matcher,
                whole: text.contains('/'),
            });
        }
        if let Some(folder) = text.strip_suffix('/') {
            return Ok(Self::Folder(
                folder.trim_matches('/').to_lowercase().replace('\\', "/"),
            ));
        }
        Ok(Self::Text(text.to_lowercase()))
    }

    fn matches(&self, path: &str, lower: &str, name: &str) -> bool {
        match self {
            Self::Glob { matcher, whole } => {
                if *whole {
                    matcher.is_match(path)
                } else {
                    matcher.is_match(name)
                }
            }
            Self::Folder(folder) => format!("/{lower}").contains(&format!("/{folder}/")),
            Self::Text(text) => lower.contains(text.as_str()),
        }
    }
}

/// A `find_files` query taken apart.
struct Query {
    words: Vec<String>,
    include: Vec<Pattern>,
    exclude: Vec<Pattern>,
    git: Option<GitFilter>,
}

impl Query {
    fn parse(text: &str) -> Result<Self, HarnessError> {
        let mut query = Self {
            words: Vec::new(),
            include: Vec::new(),
            exclude: Vec::new(),
            git: None,
        };
        for token in text.split_whitespace() {
            if let Some(filter) = token.strip_prefix("git:") {
                query.git = Some(GitFilter::parse(&filter.to_lowercase()).ok_or_else(|| {
                    HarnessError::new(
                        ErrorCode::InvalidPayload,
                        "find_files git: takes modified, staged, untracked, changed or conflict",
                    )
                })?);
            } else if let Some(excluded) = token.strip_prefix('!').filter(|rest| !rest.is_empty()) {
                query.exclude.push(Pattern::parse(excluded)?);
            } else if token.contains(['*', '?', '[', '{']) || token.ends_with('/') {
                query.include.push(Pattern::parse(token)?);
            } else {
                query.words.push(token.to_lowercase());
            }
        }
        Ok(query)
    }
}

/// One file the walk found, as the scoring reads it.
struct Candidate<'a> {
    file: &'a WalkFile,
    path: String,
    lower: String,
    name: String,
    stem: String,
    tokens: Vec<String>,
    git: Option<GitState>,
}

impl<'a> Candidate<'a> {
    fn new(file: &'a WalkFile, path: String, git: Option<GitState>) -> Self {
        let lower = path.to_lowercase();
        let name = lower.rsplit('/').next().unwrap_or(&lower).to_owned();
        let stem = name
            .rsplit_once('.')
            .map_or(name.as_str(), |(stem, _)| stem)
            .to_owned();
        let tokens = path_tokens(&path);
        Self {
            file,
            path,
            lower,
            name,
            stem,
            tokens,
            git,
        }
    }

    fn word_score(&self, word: &str) -> Option<u32> {
        if self.name == word || self.stem == word {
            return Some(100);
        }
        if self.name.starts_with(word) {
            return Some(80);
        }
        if self.name.contains(word) {
            return Some(60);
        }
        if self.lower.contains(word) {
            return Some(40);
        }
        let budget = typo_budget(word);
        if budget == 0 {
            return None;
        }
        let name_tokens = path_tokens(&self.name);
        self.tokens
            .iter()
            .chain([&self.name, &self.stem])
            .filter_map(|token| {
                let (distance, whole) = typo_distance(word, token, budget)?;
                let in_name = name_tokens.contains(token) || *token == self.name;
                // A typo of the whole name beats one of a name's start.
                let bonus = match (whole, *token == self.stem, in_name) {
                    (true, true, _) => 10,
                    (true, false, true) => 5,
                    (false, _, true) => 2,
                    _ => 0,
                };
                Some(30 - 8 * distance + bonus)
            })
            .max()
    }
}

/// The words of a path: its folders and name split at `/ . _ -`, spaces and
/// `camelCase` humps, lowercased.
fn path_tokens(path: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for part in path.split(['/', '\\', '.', '_', '-', ' ']) {
        let mut current = String::new();
        let mut previous_lower = false;
        for character in part.chars() {
            if character.is_uppercase() && previous_lower && !current.is_empty() {
                tokens.push(std::mem::take(&mut current).to_lowercase());
            }
            previous_lower = character.is_lowercase() || character.is_ascii_digit();
            current.push(character);
        }
        if !current.is_empty() {
            tokens.push(current.to_lowercase());
        }
        // The whole part too, so `fileloader` matches `FileLoader.rs`.
        let whole = part.to_lowercase();
        if !whole.is_empty() && !tokens.contains(&whole) {
            tokens.push(whole);
        }
    }
    tokens
}

/// How many typos a word forgives: one from five letters, two from eight; a
/// three- or four-letter word only two swapped letters (`mian`).
fn typo_budget(word: &str) -> u32 {
    match word.chars().count() {
        0..=2 => 0,
        3..=7 => 1,
        _ => 2,
    }
}

/// The edits (insert, delete, change, swap two neighbours) from `word` to
/// `token` - or to `token`'s start, so `confg` finds `configuration` - when
/// they are within `budget`, and whether the whole token matched. A short
/// word forgives only a swap.
fn typo_distance(word: &str, token: &str, budget: u32) -> Option<(u32, bool)> {
    let word = word.chars().collect::<Vec<_>>();
    let token = token.chars().collect::<Vec<_>>();
    if token.len() < 3 {
        return None;
    }
    let short = word.len() < 5;
    let mut best = None;
    let candidates = [
        token.len(),
        (word.len() + 1).min(token.len()),
        word.len().min(token.len()),
    ];
    for end in candidates {
        let target = &token[..end];
        if short {
            if target.len() == word.len() && is_one_swap(&word, target) {
                return Some((1, end == token.len()));
            }
            continue;
        }
        let distance = damerau(&word, target);
        let whole = end == token.len();
        if distance <= budget
            && best.is_none_or(|(best, best_whole): (u32, bool)| {
                distance < best || (distance == best && whole && !best_whole)
            })
        {
            best = Some((distance, whole));
        }
    }
    best
}

fn is_one_swap(word: &[char], target: &[char]) -> bool {
    let differ = word
        .iter()
        .zip(target)
        .enumerate()
        .filter(|(_, (left, right))| left != right)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    matches!(differ.as_slice(), [first, second] if *second == first + 1
        && word[*first] == target[*second]
        && word[*second] == target[*first])
}

/// The optimal-string-alignment distance: insertions, deletions,
/// substitutions and swaps of two neighbours.
fn damerau(left: &[char], right: &[char]) -> u32 {
    let (rows, columns) = (left.len() + 1, right.len() + 1);
    let mut table = vec![vec![0_u32; columns]; rows];
    for (row, cells) in table.iter_mut().enumerate() {
        cells[0] = u32::try_from(row).unwrap_or(u32::MAX);
    }
    for (column, cell) in table[0].iter_mut().enumerate() {
        *cell = u32::try_from(column).unwrap_or(u32::MAX);
    }
    for row in 1..rows {
        for column in 1..columns {
            let cost = u32::from(left[row - 1] != right[column - 1]);
            let mut value = (table[row - 1][column] + 1)
                .min(table[row][column - 1] + 1)
                .min(table[row - 1][column - 1] + cost);
            if row > 1
                && column > 1
                && left[row - 1] == right[column - 2]
                && left[row - 2] == right[column - 1]
            {
                value = value.min(table[row - 2][column - 2] + 1);
            }
            table[row][column] = value;
        }
    }
    table[rows - 1][columns - 1]
}

/// What git says about the paths under `root` that are not clean, by
/// root-relative path; empty outside a repository.
fn git_states(root: &Path) -> HashMap<String, GitState> {
    let run = |arguments: &[&str]| {
        Command::new("git")
            .args(arguments)
            .current_dir(root)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let Some(prefix) = run(&["rev-parse", "--show-prefix"]) else {
        return HashMap::new();
    };
    let prefix = prefix.trim().replace('\\', "/");
    let Some(porcelain) = run(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]) else {
        return HashMap::new();
    };
    let mut states = HashMap::new();
    let mut entries = porcelain.split('\0');
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let (code, path) = entry.split_at(3);
        let mut code = code.chars();
        let (index, tree) = (code.next().unwrap_or(' '), code.next().unwrap_or(' '));
        // A rename or copy names its source next.
        if matches!(index, 'R' | 'C') {
            entries.next();
        }
        let Some(relative) = path.strip_prefix(prefix.as_str()) else {
            continue;
        };
        let untracked = index == '?';
        let conflict = index == 'U'
            || tree == 'U'
            || (index == 'A' && tree == 'A')
            || (index == 'D' && tree == 'D');
        states.insert(
            relative.to_owned(),
            GitState {
                staged: !untracked && index != ' ',
                unstaged: !untracked && tree != ' ',
                untracked,
                conflict,
            },
        );
    }
    states
}

fn found(candidate: &Candidate<'_>) -> FoundFile {
    FoundFile {
        path: candidate.path.clone(),
        git: candidate.git.map(|state| state.label().to_owned()),
        line: None,
    }
}

/// Sort scored candidates best first: by score, then what git says is changed,
/// then the newest, then the shorter path.
fn best_first<S: Ord>(scored: &mut [(S, &Candidate<'_>)]) {
    scored.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| right.git.is_some().cmp(&left.git.is_some()))
            .then_with(|| right.file.modified.cmp(&left.file.modified))
            .then_with(|| left.path.len().cmp(&right.path.len()))
            .then_with(|| left.path.cmp(&right.path))
    });
}

/// The candidates for `words`, best first, under the shared rule of
/// `harness_types::search`: those matching every word, else those matching
/// the most; and the note an incomplete answer carries. No words: every
/// candidate, what git says is changed first, then the newest.
fn ranked<'a, 'b>(
    candidates: &'b [Candidate<'a>],
    words: &[String],
) -> (Vec<&'b Candidate<'a>>, Option<String>) {
    let (mut scored, note) = if words.is_empty() {
        (
            candidates
                .iter()
                .map(|candidate| (Coverage::default(), candidate))
                .collect::<Vec<_>>(),
            None,
        )
    } else {
        let found = match_words(candidates, words, |candidate, word| {
            candidate.word_score(word).map(u64::from)
        });
        let note = found.note("files", words);
        (found.hits, note)
    };
    best_first(&mut scored);
    (
        scored.into_iter().map(|(_, candidate)| candidate).collect(),
        note,
    )
}

/// The first few of `candidates` whose text holds `text`, each with the line.
fn files_by_text(
    root: &Path,
    requested: Option<&str>,
    text: &str,
    candidates: &[Candidate<'_>],
) -> Vec<FoundFile> {
    let allowed = candidates
        .iter()
        .map(|candidate| (candidate.path.as_str(), candidate))
        .collect::<HashMap<_, _>>();
    let Ok((found_text, _)) = crate::lookup::lookup(
        root,
        &crate::lookup::Lookup {
            query: text,
            any_of: &[],
            path: requested,
            regex: false,
            case_insensitive: true,
            glob: None,
            context_lines: 0,
            anchors: false,
        },
    ) else {
        return Vec::new();
    };
    let mut files: Vec<FoundFile> = Vec::new();
    for hit in found_text.matches {
        if files.len() >= CONTENT_FALLBACK_FILES {
            break;
        }
        let Some(candidate) = allowed.get(hit.path.as_str()) else {
            continue;
        };
        if files.iter().any(|file| file.path == hit.path) {
            continue;
        }
        files.push(FoundFile {
            line: Some(format!("{}: {}", hit.line, hit.preview.trim())),
            ..found(candidate)
        });
    }
    files
}

/// Find files under `requested` by `query`, best first, at most `limit`.
pub(crate) fn find_files(
    root: &Path,
    requested: Option<&str>,
    query: &str,
    limit: u32,
) -> Result<FindOutput, HarnessError> {
    let parsed = Query::parse(query)?;
    let walk = tool_walk(root, requested, "find_files")?;
    let states = git_states(root);
    let limit = usize::try_from(limit.clamp(1, FIND_MAX_LIMIT)).unwrap_or(30);
    let candidates =
        walk.files
            .iter()
            .filter_map(|file| {
                let path = relative_text(file.absolute.strip_prefix(root).ok()?);
                let candidate = Candidate::new(file, path, None);
                let git = states.get(&candidate.path).copied();
                Some(Candidate { git, ..candidate })
            })
            .filter(|candidate| {
                parsed.include.iter().all(|pattern| {
                    pattern.matches(&candidate.path, &candidate.lower, &candidate.name)
                }) && !parsed.exclude.iter().any(|pattern| {
                    pattern.matches(&candidate.path, &candidate.lower, &candidate.name)
                }) && parsed.git.is_none_or(|filter| filter.admits(candidate.git))
            })
            .collect::<Vec<_>>();
    let listing = |hits: &[&Candidate<'_>], note: Option<String>| FindOutput {
        files: hits
            .iter()
            .take(limit)
            .map(|candidate| found(candidate))
            .collect(),
        truncated: hits.len() > limit || !walk.complete,
        note,
    };
    let (hits, note) = ranked(&candidates, &parsed.words);
    if parsed.words.is_empty() {
        let note = (query.trim().is_empty() && !hits.is_empty())
            .then(|| "no query: changed in git first, then the newest".to_owned());
        return Ok(listing(&hits, note));
    }
    if !hits.is_empty() {
        return Ok(listing(&hits, note));
    }
    // No name holds a word: the files' text, where a name is often a file's
    // subject; several words find the files holding the most of them.
    let text = parsed.words.join(" ");
    let files = files_by_text(root, requested, &text, &candidates);
    if !files.is_empty() {
        return Ok(FindOutput {
            files,
            truncated: false,
            note: Some(format!(
                "0 file names match '{text}'; files containing {}:",
                if parsed.words.len() > 1 {
                    "its words"
                } else {
                    "it"
                }
            )),
        });
    }
    Ok(FindOutput {
        files: Vec::new(),
        truncated: !walk.complete,
        note: Some(format!(
            "0 files match '{}'{}",
            query.trim(),
            requested.map_or_else(String::new, |path| format!(" in {path}"))
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::{Pattern, Query, damerau, find_files, path_tokens, typo_distance};
    use std::{fs, path::Path, process::Command};

    fn git(root: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(arguments)
            .current_dir(root)
            .output()
            .expect("git runs")
            .status;
        assert!(status.success(), "git {arguments:?}");
    }

    fn paths(found: &super::FindOutput) -> Vec<&str> {
        found.files.iter().map(|file| file.path.as_str()).collect()
    }

    /// A project as a coding agent meets it: a typo, two words, filters, git.
    #[test]
    fn files_are_found_by_name_with_typos_filters_and_git() {
        let directory = tempfile::tempdir().expect("scratch directory");
        let root = fs::canonicalize(directory.path()).expect("root");
        for (path, text) in [
            ("src/main.rs", "fn main() {}"),
            ("src/config/loader.rs", "pub fn load() {}"),
            ("src/config/mod.rs", "mod loader;"),
            ("tests/main_test.rs", "#[test] fn t() {}"),
            ("docs/guide.md", "the zebrafinch protocol"),
        ] {
            fs::create_dir_all(root.join(path).parent().expect("parent")).expect("dirs");
            fs::write(root.join(path), text).expect("file");
        }
        git(&root, &["init", "-q"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "init"]);
        fs::write(root.join("src/config/mod.rs"), "mod loader; // changed").expect("edit");
        fs::write(root.join("src/new_thing.rs"), "").expect("new");

        let found = find_files(&root, None, "mian", 30).expect("find");
        assert_eq!(
            paths(&found)[0],
            "src/main.rs",
            "a swapped pair: {:?}",
            paths(&found)
        );
        let found = find_files(&root, None, "confg lodaer", 30).expect("find");
        assert_eq!(
            paths(&found),
            ["src/config/loader.rs"],
            "two typos, two words"
        );
        let found = find_files(&root, None, "main *.rs !tests/", 30).expect("find");
        assert_eq!(paths(&found), ["src/main.rs"]);
        let found = find_files(&root, None, "git:modified", 30).expect("find");
        assert_eq!(paths(&found), ["src/config/mod.rs"]);
        assert_eq!(found.files[0].git.as_deref(), Some("modified"));
        let found = find_files(&root, None, "git:untracked", 30).expect("find");
        assert_eq!(paths(&found), ["src/new_thing.rs"]);
        let found = find_files(&root, None, "", 2).expect("find");
        assert!(found.truncated);
        assert_eq!(found.files.len(), 2);
        assert!(
            found.files.iter().all(|file| file.git.is_some()),
            "changed first: {:?}",
            paths(&found)
        );
        let found = find_files(&root, None, "loader frobnicate", 30).expect("find");
        assert_eq!(paths(&found), ["src/config/loader.rs"]);
        assert!(
            found
                .note
                .as_deref()
                .is_some_and(|note| note.contains("no files match 'frobnicate'")),
            "{:?}",
            found.note
        );
        // Several files named in one query: each is found, though no path
        // holds every word.
        let found = find_files(&root, None, "guide loader nginx", 30).expect("find");
        let mut listed = paths(&found);
        listed.sort_unstable();
        assert_eq!(listed, ["docs/guide.md", "src/config/loader.rs"]);
        assert!(
            found.note.as_deref().is_some_and(|note| note
                .contains("files matching the most of its words; no files match 'nginx'")),
            "{:?}",
            found.note
        );
        let found = find_files(&root, None, "zebrafinch", 30).expect("find");
        assert_eq!(paths(&found), ["docs/guide.md"]);
        assert!(
            found
                .note
                .as_deref()
                .is_some_and(|note| note.contains("files containing it")),
            "{:?}",
            found.note
        );
        assert!(
            found.files[0]
                .line
                .as_deref()
                .is_some_and(|line| line.starts_with("1: "))
        );
        let found = find_files(&root, Some("src"), "loader", 30).expect("find");
        assert_eq!(
            paths(&found),
            ["src/config/loader.rs"],
            "paths stay root-relative"
        );
        // No name holds any word: the files whose text holds one of them.
        let found = find_files(&root, None, "qqqqqqqq zebrafinch", 30).expect("find");
        assert_eq!(paths(&found), ["docs/guide.md"]);
        let none = find_files(&root, None, "qqqqqqqq", 30).expect("find");
        assert!(none.files.is_empty());
        assert!(
            none.note
                .as_deref()
                .is_some_and(|note| note.starts_with("0 files match"))
        );
    }

    #[test]
    fn a_path_splits_into_its_words() {
        assert_eq!(
            path_tokens("src/ConfigLoader.rs"),
            ["src", "config", "loader", "configloader", "rs"]
        );
        assert!(path_tokens("crates/harness-cli").contains(&"harness".to_owned()));
    }

    #[test]
    fn a_word_forgives_its_typos() {
        assert_eq!(damerau(&['a', 'b'], &['b', 'a']), 1);
        assert_eq!(typo_distance("mian", "main", 1), Some((1, true)), "a swap");
        assert_eq!(
            typo_distance("mian", "main_test", 1),
            Some((1, false)),
            "its start"
        );
        assert_eq!(typo_distance("mxin", "main", 1), None, "only a swap");
        assert_eq!(typo_distance("confg", "config", 1), Some((1, true)));
        assert_eq!(typo_distance("confg", "configuration", 1), Some((1, false)));
        assert_eq!(typo_distance("delegaton", "delegation", 2), Some((1, true)));
        assert_eq!(typo_distance("lodaer", "loader", 1), Some((1, true)));
        assert_eq!(typo_distance("parser", "printer", 1), None);
    }

    #[test]
    fn a_query_keeps_its_filters_apart_from_its_words() {
        let query = Query::parse("Config *.rs src/ !test/ git:modified").expect("parses");
        assert_eq!(query.words, ["config"]);
        assert_eq!(query.include.len(), 2);
        assert_eq!(query.exclude.len(), 1);
        assert!(query.git.is_some());
        assert!(Query::parse("git:clean").is_err());
        let folder = Pattern::parse("src/").expect("folder");
        assert!(folder.matches("src/a.rs", "src/a.rs", "a.rs"));
        assert!(folder.matches("crates/x/src/a.rs", "crates/x/src/a.rs", "a.rs"));
        assert!(!folder.matches("srcs/a.rs", "srcs/a.rs", "a.rs"));
        let glob = Pattern::parse("*.rs").expect("glob");
        assert!(glob.matches("deep/a.rs", "deep/a.rs", "a.rs"));
    }
}
