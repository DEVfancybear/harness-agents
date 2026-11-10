//! The code an entry cites, checked when it is read, and the files an entry
//! is kept for (`docs/architecture/MEMORY_STORE.en.md` §14).
//!
//! The fields this adds to an entry's document are all named here:
//!
//! - `citations`: the code a fact rests on, `[{path, quote, lines}]` - a
//!   workspace-relative file, a short span of its own words, and the lines
//!   the host found that span at when the entry was written. The host checks
//!   each one at write (the file is in the workspace and holds the quote,
//!   whitespace aside) and again whenever the entry is read: the digest, a
//!   search, a read. A citation holds where its quote still is: at its lines,
//!   or moved, and then its lines follow the code. It does not hold when the
//!   file or the quote is gone, and the entry is then shown as resting on
//!   code that changed since.
//! - `paths`: the workspace files an entry is kept for, as gitignore-style
//!   globs (`src/billing/**`). Such an entry is out of the digest every turn
//!   carries: it enters a conversation when the conversation touches a
//!   matching file (`memory_scoped`).
//!
//! A check never writes the entry: its last result is kept in the store's
//! meta under [`CHECK_META`], by memory id, so a read makes no version.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::memory_store;

/// The field of an entry naming the code it rests on.
pub const CITATIONS: &str = "citations";
/// The field of an entry naming the files it is kept for.
pub const PATHS: &str = "paths";
/// The store meta key the last check of each entry's citations is kept under.
pub const CHECK_META: &str = "citations";
/// The most citations one entry carries.
pub const MAX_CITATIONS: usize = 5;
/// The longest quote: a citation is a span, not a copy of the file.
pub const MAX_QUOTE_CHARS: usize = 400;
/// The fewest characters other than whitespace a quote has: shorter spans
/// are found in almost any file.
pub const MIN_QUOTE_CHARS: usize = 8;
/// The most globs one entry is kept for, and the longest one.
pub const MAX_PATHS: usize = 16;
const MAX_GLOB_CHARS: usize = 200;
/// A file past this is not read for a check: its citations are unchecked.
const MAX_FILE_BYTES: u64 = 1 << 20;
/// What one digest, search or read may read of files not cached: past it,
/// the rest of its citations are unchecked this time.
pub const READ_BUDGET: usize = 4 << 20;
/// How many files the cache keeps, by path, modification time and size.
const CACHED_FILES: usize = 256;

/// Which of the two features the memory switch leaves on: `citations` and
/// `scoped` (`memory_switch`). Off, citations are neither checked nor shown,
/// and scoped entries neither loaded nor written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Features {
    pub citations: bool,
    pub scoped: bool,
}

impl Default for Features {
    fn default() -> Self {
        Self {
            citations: true,
            scoped: true,
        }
    }
}

impl Features {
    /// What `switch` leaves on.
    #[must_use]
    pub fn of(switch: &super::memory_switch::Switch) -> Self {
        Self {
            citations: switch.on(super::memory_switch::Feature::Citations),
            scoped: switch.on(super::memory_switch::Feature::Scoped),
        }
    }
}

/// One citation as an entry keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Citation {
    pub path: String,
    pub quote: String,
    /// The 1-based lines `[start, end]` the quote was found at.
    pub lines: Option<(usize, usize)>,
    /// The key of the project it was written in: another project's files
    /// are not the ones it names.
    pub project: Option<String>,
}

/// What ranking, the digest and the scoped loader read of these fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cited {
    pub citations: Vec<Citation>,
    /// Its globs, as written.
    pub paths: Vec<String>,
    /// The last check of its citations, from the store's meta.
    pub last: Option<Value>,
}

impl Cited {
    /// The citations and globs an entry's document records.
    #[must_use]
    pub fn of(doc: &Value) -> Self {
        let citations = doc[CITATIONS]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| {
                Some(Citation {
                    path: item["path"].as_str()?.to_owned(),
                    quote: item["quote"].as_str()?.to_owned(),
                    lines: lines_of(&item["lines"]),
                    project: item["project"].as_str().map(str::to_owned),
                })
            })
            .collect();
        let paths = doc[PATHS]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        Self {
            citations,
            paths,
            last: None,
        }
    }

    /// Kept for some files only: out of the digest every turn carries.
    #[must_use]
    pub fn scoped(&self) -> bool {
        !self.paths.is_empty()
    }

    /// Whether the entry is kept for the workspace file `path`.
    #[must_use]
    pub fn applies_to(&self, path: &str) -> bool {
        self.scoped() && matcher(&self.paths).is_some_and(|set| set.is_match(path))
    }

    /// Whether `named` - a path a search names - is one this entry is kept
    /// for or cites.
    #[must_use]
    pub fn names(&self, named: &str) -> bool {
        let named = relative_text(named);
        self.applies_to(&named)
            || self.citations.iter().any(|citation| {
                citation.path == named || citation.path.ends_with(&format!("/{named}"))
            })
    }

    /// The last check's verdict, as the meta keeps it: `holds`, `broken` or
    /// `unchecked`.
    #[must_use]
    pub fn last_verdict(&self) -> Option<&str> {
        self.last.as_ref()?["state"].as_str()
    }
}

/// What a review is told beside an entry its last check found broken: it
/// rests on code that changed since, so it is not evidence as it stands.
#[must_use]
pub fn review_note(cited: &Cited) -> String {
    if cited.last_verdict() != Some("broken") {
        return String::new();
    }
    let broken = cited
        .last
        .as_ref()
        .and_then(|last| last["broken"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("; ");
    format!(" [its cited code changed since it was kept: {broken}]")
}

/// `[start, end]`, a line number, or `"start-end"`.
fn lines_of(value: &Value) -> Option<(usize, usize)> {
    let number = |value: &Value| {
        value
            .as_u64()
            .or_else(|| value.as_str()?.trim().parse().ok())
            .and_then(|line| usize::try_from(line).ok())
            .filter(|line| *line >= 1)
    };
    let (start, end) = match value {
        Value::Array(items) if items.len() == 2 => (number(&items[0])?, number(&items[1])?),
        Value::Array(items) if items.len() == 1 => (number(&items[0])?, number(&items[0])?),
        Value::String(text) => match text.split_once('-') {
            Some((start, end)) => (number(&json!(start.trim()))?, number(&json!(end.trim()))?),
            None => (number(value)?, number(value)?),
        },
        Value::Number(_) => (number(value)?, number(value)?),
        _ => return None,
    };
    (start <= end).then_some((start, end))
}

/// A path as the workspace names it: `/`-separated, without `./`.
#[must_use]
pub fn relative_text(path: &str) -> String {
    let path = path.trim().replace('\\', "/");
    let mut path = path.as_str();
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    path.to_owned()
}

/// The workspace-relative form of a path a tool's result names: as given
/// when relative, made relative when it is inside the workspace; `None`
/// outside it.
#[must_use]
pub fn in_workspace(workspace: &Path, path: &str) -> Option<String> {
    let text = relative_text(path);
    let as_path = Path::new(&text);
    if as_path.is_absolute() {
        let inside = as_path
            .strip_prefix(workspace)
            .map(Path::to_path_buf)
            .ok()
            .or_else(|| {
                let canonical = std::fs::canonicalize(workspace).ok()?;
                as_path.strip_prefix(canonical).ok().map(Path::to_path_buf)
            })?
            .to_string_lossy()
            .replace('\\', "/");
        let plain = Path::new(&inside)
            .components()
            .all(|part| matches!(part, Component::Normal(_)));
        return (plain && !inside.is_empty()).then_some(inside);
    }
    let plain = as_path
        .components()
        .all(|part| matches!(part, Component::Normal(_)));
    (plain && !text.is_empty()).then_some(text)
}

/// The globs of `paths` as one set, gitignore-style: a glob with no `/`
/// matches at any depth, and a directory matches what is below it.
#[must_use]
pub fn matcher(paths: &[String]) -> Option<globset::GlobSet> {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in paths {
        for glob in expanded(pattern) {
            builder.add(
                globset::GlobBuilder::new(&glob)
                    .literal_separator(true)
                    .build()
                    .ok()?,
            );
        }
    }
    builder.build().ok()
}

/// The globs one gitignore-style pattern stands for; a leading `/` anchors it
/// at the root.
fn expanded(pattern: &str) -> Vec<String> {
    let pattern = relative_text(pattern);
    let anchored = pattern.starts_with('/');
    let pattern = pattern.trim_start_matches('/');
    let pattern = if pattern.ends_with('/') {
        format!("{pattern}**")
    } else {
        pattern.to_owned()
    };
    let mut globs = vec![pattern.clone()];
    if !pattern.ends_with("**") {
        globs.push(format!("{pattern}/**"));
    }
    if !anchored && !pattern.contains('/') {
        globs.extend(globs.clone().into_iter().map(|glob| format!("**/{glob}")));
    }
    globs
}

/// The globs a write gives in `paths`, checked: workspace-relative patterns
/// that parse, at most [`MAX_PATHS`]. An empty list or `null` clears them.
///
/// # Errors
/// A pattern that is not a string, leaves the workspace or does not parse.
pub fn verify_paths(given: &Value) -> Result<Vec<String>, String> {
    if given.is_null() {
        return Ok(Vec::new());
    }
    let items = given
        .as_array()
        .ok_or("paths is a list of workspace-relative globs, such as [\"src/billing/**\"]")?;
    if items.len() > MAX_PATHS {
        return Err(format!("an entry is kept for at most {MAX_PATHS} globs"));
    }
    let mut paths = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let pattern = item
            .as_str()
            .map(relative_text)
            .filter(|pattern| !pattern.is_empty())
            .ok_or_else(|| format!("paths[{index}] is a non-empty glob"))?;
        glob_error(&pattern).map_or(Ok(()), |error| Err(format!("paths[{index}] {error}")))?;
        if !paths.contains(&pattern) {
            paths.push(pattern);
        }
    }
    Ok(paths)
}

/// Why one glob of `paths` is refused, when it is.
#[must_use]
pub fn glob_error(pattern: &str) -> Option<String> {
    if pattern.chars().count() > MAX_GLOB_CHARS {
        return Some(format!("is longer than {MAX_GLOB_CHARS} characters"));
    }
    // A leading `/` anchors the glob at the root: it is no absolute path.
    let leaves = Path::new(pattern.trim_start_matches('/')).is_absolute()
        || pattern.contains(':')
        || pattern.split('/').any(|label| label == "..");
    if leaves {
        return Some(format!(
            "{pattern:?} is not inside the workspace: give a path relative to its root"
        ));
    }
    expanded(pattern).iter().find_map(|glob| {
        globset::GlobBuilder::new(glob)
            .literal_separator(true)
            .build()
            .err()
            .map(|error| format!("{pattern:?} is not a glob: {error}"))
    })
}

// --- finding a quote ---

/// A file's text with its whitespace folded to single spaces, and where each
/// line starts in it.
#[derive(Debug)]
struct Flat {
    text: String,
    /// `(offset in text, line)` at each line's first character kept.
    starts: Vec<(usize, usize)>,
}

impl Flat {
    fn of(text: &str) -> Self {
        let mut flat = String::with_capacity(text.len());
        let mut starts = Vec::new();
        let mut line = 1;
        let mut recorded = 0;
        let mut space = false;
        for character in text.chars() {
            if character == '\n' {
                line += 1;
            }
            if character.is_whitespace() {
                space = !flat.is_empty();
                continue;
            }
            if space {
                flat.push(' ');
                space = false;
            }
            if line != recorded {
                starts.push((flat.len(), line));
                recorded = line;
            }
            flat.push(character);
        }
        Self { text: flat, starts }
    }

    /// The line the character at `offset` is on.
    fn line(&self, offset: usize) -> usize {
        let index = self.starts.partition_point(|(start, _)| *start <= offset);
        self.starts
            .get(index.saturating_sub(1))
            .map_or(1, |(_, line)| *line)
    }

    /// The lines of each place `quote` (folded) is found at.
    fn found(&self, quote: &str) -> Vec<(usize, usize)> {
        if quote.is_empty() {
            return Vec::new();
        }
        self.text
            .match_indices(quote)
            .map(|(offset, _)| (self.line(offset), self.line(offset + quote.len() - 1)))
            .collect()
    }
}

/// A quote with its whitespace folded, as [`Flat`] folds a file.
fn folded(quote: &str) -> String {
    quote.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a citation holds now, and where.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Holds {
    /// The quote is at the lines it was found at.
    Same,
    /// The quote is elsewhere in the file: the lines follow it.
    Moved,
    /// The file is gone, or no longer holds the quote.
    Gone,
    /// Not checked: the file is too large or unreadable, or the read budget
    /// was spent.
    Unchecked,
}

/// One citation's check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub path: String,
    pub holds: Holds,
    pub lines: Option<(usize, usize)>,
    pub reason: Option<String>,
}

impl State {
    fn value(&self) -> Value {
        let mut value = json!({
            "path": self.path,
            "state": match self.holds {
                Holds::Same => "holds",
                Holds::Moved => "moved",
                Holds::Gone => "broken",
                Holds::Unchecked => "unchecked",
            },
        });
        if let Some((start, end)) = self.lines {
            value["lines"] = json!([start, end]);
        }
        if let Some(reason) = &self.reason {
            value["reason"] = json!(reason);
        }
        value
    }

    /// `path:lines`, or the path alone.
    fn place(&self) -> String {
        match self.lines {
            Some((start, end)) if start == end => format!("{}:{start}", self.path),
            Some((start, end)) => format!("{}:{start}-{end}", self.path),
            None => self.path.clone(),
        }
    }
}

/// What an entry's citations say together: they hold when each one does, are
/// broken when one does not, and are unchecked otherwise.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verdict {
    Holds,
    Broken,
    Unchecked,
}

/// The check of one entry's citations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub verdict: Verdict,
    pub states: Vec<State>,
}

impl Checked {
    fn of(states: Vec<State>) -> Self {
        let verdict = if states.iter().any(|state| state.holds == Holds::Gone) {
            Verdict::Broken
        } else if states.iter().any(|state| state.holds == Holds::Unchecked) {
            Verdict::Unchecked
        } else {
            Verdict::Holds
        };
        Self { verdict, states }
    }

    /// The verdict's name, as the meta and the results say it.
    #[must_use]
    pub const fn verdict_name(&self) -> &'static str {
        match self.verdict {
            Verdict::Holds => "holds",
            Verdict::Broken => "broken",
            Verdict::Unchecked => "unchecked",
        }
    }

    /// Each citation's state, as a search or a read returns it.
    #[must_use]
    pub fn values(&self) -> Vec<Value> {
        self.states.iter().map(State::value).collect()
    }

    /// What the digest says beside the entry: where its code is while it
    /// holds; that it changed since, and to check it, when it does not.
    #[must_use]
    pub fn note(&self) -> Option<String> {
        match self.verdict {
            Verdict::Holds => Some(format!(
                "[cites {}]",
                self.states
                    .iter()
                    .map(State::place)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Verdict::Broken => Some(format!(
                "[its cited code changed since: {}; check the code before relying on it, then update or supersede the entry]",
                self.states
                    .iter()
                    .filter(|state| state.holds == Holds::Gone)
                    .map(|state| format!(
                        "{} {}",
                        state.path,
                        state.reason.as_deref().unwrap_or("does not hold")
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            )),
            Verdict::Unchecked => None,
        }
    }

    /// The record the store's meta keeps: the verdict, and each citation's
    /// lines where it was found, which the next check looks at first.
    fn record(&self) -> Value {
        json!({
            "state": self.verdict_name(),
            "lines": self.states.iter().map(|state| state.lines.map_or(Value::Null, |(start, end)| json!([start, end]))).collect::<Vec<_>>(),
            "broken": self.states.iter().filter(|state| state.holds == Holds::Gone).map(|state| format!("{} {}", state.path, state.reason.as_deref().unwrap_or_default())).collect::<Vec<_>>(),
        })
    }
}

/// A file read for a check, kept while its modification time and size stay.
struct Cached {
    modified: Option<std::time::SystemTime>,
    size: u64,
    flat: Arc<Flat>,
    used: u64,
}

/// The files read for checks, by path; the least recently used goes first.
fn cache() -> &'static Mutex<(HashMap<PathBuf, Cached>, u64)> {
    static CACHE: std::sync::OnceLock<Mutex<(HashMap<PathBuf, Cached>, u64)>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new((HashMap::new(), 0)))
}

/// Checks an entry's citations against the files of one workspace, within a
/// read budget shared by every check it makes.
#[derive(Clone, Debug)]
pub struct Checker {
    workspace: PathBuf,
    /// The workspace's project key: a citation written in another project
    /// is not checked here.
    project: String,
    budget: Arc<AtomicUsize>,
}

/// Why a file is not read for a check.
enum Unread {
    Gone(String),
    Unchecked(String),
}

impl Checker {
    #[must_use]
    pub fn new(workspace: &Path) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
            project: super::bootstrap::project_key_of(workspace),
            budget: Arc::new(AtomicUsize::new(READ_BUDGET)),
        }
    }

    /// The file `relative` names, folded, from the cache while it is
    /// unchanged; a read counts against the budget.
    fn flat(&self, relative: &str) -> Result<Arc<Flat>, Unread> {
        let inside = in_workspace(&self.workspace, relative)
            .ok_or_else(|| Unread::Gone("is outside the workspace".to_owned()))?;
        let path = self.workspace.join(&inside);
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => return Err(Unread::Gone("is no longer a file".to_owned())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Unread::Gone("is gone".to_owned()));
            }
            Err(error) => return Err(Unread::Unchecked(format!("could not be read: {error}"))),
        };
        if metadata.len() > MAX_FILE_BYTES {
            return Err(Unread::Unchecked("is too large to check".to_owned()));
        }
        let modified = metadata.modified().ok();
        if let Ok(mut guard) = cache().lock() {
            let (files, clock) = &mut *guard;
            *clock += 1;
            let now = *clock;
            if let Some(cached) = files.get_mut(&path)
                && cached.modified == modified
                && cached.size == metadata.len()
            {
                cached.used = now;
                return Ok(Arc::clone(&cached.flat));
            }
        }
        let size = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
        let spent = self
            .budget
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(size)
            });
        if spent.is_err() {
            return Err(Unread::Unchecked(
                "was not read: this read's budget for checking citations is spent".to_owned(),
            ));
        }
        let bytes = std::fs::read(&path)
            .map_err(|error| Unread::Unchecked(format!("could not be read: {error}")))?;
        let flat = Arc::new(Flat::of(&String::from_utf8_lossy(&bytes)));
        // Read outside the lock; kept once it is read.
        let Ok(mut guard) = cache().lock() else {
            return Ok(flat);
        };
        let (files, clock) = &mut *guard;
        *clock += 1;
        let now = *clock;
        if files.len() >= CACHED_FILES
            && let Some(oldest) = files
                .iter()
                .min_by_key(|(_, cached)| cached.used)
                .map(|(path, _)| path.clone())
        {
            files.remove(&oldest);
        }
        files.insert(
            path,
            Cached {
                modified,
                size: metadata.len(),
                flat: Arc::clone(&flat),
                used: now,
            },
        );
        Ok(flat)
    }

    /// Check one citation, looking first at `near` (where the last check
    /// found it), else at its own lines.
    fn state(&self, citation: &Citation, near: Option<(usize, usize)>) -> State {
        let mut state = State {
            path: citation.path.clone(),
            holds: Holds::Unchecked,
            lines: near.or(citation.lines),
            reason: None,
        };
        if citation
            .project
            .as_ref()
            .is_some_and(|project| *project != self.project)
        {
            state.reason = Some("is in another project's workspace".to_owned());
            return state;
        }
        let flat = match self.flat(&citation.path) {
            Ok(flat) => flat,
            Err(Unread::Gone(reason)) => {
                state.holds = Holds::Gone;
                state.reason = Some(reason);
                return state;
            }
            Err(Unread::Unchecked(reason)) => {
                state.reason = Some(reason);
                return state;
            }
        };
        let found = flat.found(&folded(&citation.quote));
        let Some(first) = found.first().copied() else {
            state.holds = Holds::Gone;
            state.reason = Some("no longer holds the quoted code".to_owned());
            return state;
        };
        let Some(wanted) = near.or(citation.lines) else {
            state.holds = Holds::Same;
            state.lines = Some(first);
            return state;
        };
        // Where it was last found, else the place nearest to it: the code
        // moved, and its lines follow.
        let at = if found.contains(&wanted) {
            wanted
        } else {
            found
                .iter()
                .min_by_key(|(start, _)| start.abs_diff(wanted.0))
                .copied()
                .unwrap_or(first)
        };
        state.holds = if citation.lines.is_none_or(|original| original == at) {
            Holds::Same
        } else {
            Holds::Moved
        };
        state.lines = Some(at);
        state
    }

    /// Check an entry's citations; `None` when it cites nothing.
    #[must_use]
    pub fn check(&self, cited: &Cited) -> Option<Checked> {
        if cited.citations.is_empty() {
            return None;
        }
        let last = cited
            .last
            .as_ref()
            .and_then(|last| last["lines"].as_array());
        Some(Checked::of(
            cited
                .citations
                .iter()
                .enumerate()
                .map(|(index, citation)| {
                    let near = last.and_then(|lines| lines.get(index)).and_then(lines_of);
                    self.state(citation, near)
                })
                .collect(),
        ))
    }

    /// The citations a write gives, checked: each names a file in the
    /// workspace and a quote that file holds (whitespace aside), within the
    /// lines given or anywhere, and is kept with the lines it was found at.
    /// An empty list or `null` clears them.
    ///
    /// # Errors
    /// A citation that is malformed, outside the workspace, or does not hold.
    pub fn verify(&self, given: &Value) -> Result<Vec<Value>, String> {
        if given.is_null() {
            return Ok(Vec::new());
        }
        let items = given.as_array().ok_or(
            "citations is a list of {path, quote, lines?}: the file the fact rests on and a short span of its code, copied word for word",
        )?;
        if items.len() > MAX_CITATIONS {
            return Err(format!("an entry cites at most {MAX_CITATIONS} places"));
        }
        let mut kept = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let number = index + 1;
            let path = item["path"]
                .as_str()
                .map(relative_text)
                .filter(|path| !path.is_empty())
                .ok_or_else(|| {
                    format!(
                        "citation {number} needs path: the file, relative to the workspace root"
                    )
                })?;
            let quote = item["quote"]
                .as_str()
                .map(str::trim)
                .filter(|quote| !quote.is_empty())
                .ok_or_else(|| {
                    format!("citation {number} needs quote: the code's own words the fact rests on")
                })?;
            let shown = folded(quote);
            if shown.chars().filter(|character| *character != ' ').count() < MIN_QUOTE_CHARS {
                return Err(format!(
                    "citation {number}: a quote of fewer than {MIN_QUOTE_CHARS} characters is found in almost any file: quote the line the fact rests on"
                ));
            }
            if quote.chars().count() > MAX_QUOTE_CHARS {
                return Err(format!(
                    "citation {number}: a quote is at most {MAX_QUOTE_CHARS} characters: quote the span the fact rests on, not the file"
                ));
            }
            let wanted = match item.get("lines") {
                None | Some(Value::Null) => None,
                Some(lines) => Some(lines_of(lines).ok_or_else(|| {
                    format!("citation {number}: lines is [start, end], 1-based, start <= end")
                })?),
            };
            let inside = in_workspace(&self.workspace, &path).ok_or_else(|| {
                format!("citation {number}: {path} is not inside the workspace: give a path relative to its root")
            })?;
            let flat = match self.flat(&inside) {
                Ok(flat) => flat,
                Err(Unread::Gone(reason) | Unread::Unchecked(reason)) => {
                    return Err(format!("citation {number}: {inside} {reason}"));
                }
            };
            let found = flat.found(&shown);
            let Some(first) = found.first().copied() else {
                return Err(format!(
                    "citation {number}: {inside} does not hold that quote (compared with whitespace aside): copy a short span of the file's own words"
                ));
            };
            let at = match wanted {
                None => first,
                Some((start, end)) => found
                    .iter()
                    .find(|(from, to)| *from >= start && *to <= end)
                    .copied()
                    .ok_or_else(|| {
                        format!(
                            "citation {number}: {inside} holds that quote at lines {}-{}, not within {start}-{end}: give those lines, or leave lines out",
                            first.0, first.1
                        )
                    })?,
            };
            kept.push(json!({"path": inside, "quote": quote, "lines": [at.0, at.1], "project": self.project}));
        }
        Ok(kept)
    }
}

/// Keep `checked` as the last check of the memory `memory` in the store of
/// `dir`'s meta, when it differs from what is kept: never in the entry, so a
/// check is no version and no event. A failure is a debug record, never the
/// read's failure.
pub fn record(dir: &Path, checks: &[(String, Checked)]) {
    if checks.is_empty() || !memory_store::db_path(dir).is_file() {
        return;
    }
    let mut kept = memory_store::read_meta(dir, CHECK_META)
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let mut changed = false;
    for (memory, checked) in checks {
        let record = checked.record();
        let same = kept.get(memory).is_some_and(|old| {
            old["state"] == record["state"]
                && old["lines"] == record["lines"]
                && old["broken"] == record["broken"]
        });
        if !same {
            let mut record = record;
            record["at"] = json!(memory_store::now());
            kept.insert(memory.clone(), record);
            changed = true;
        }
    }
    if changed && let Err(error) = memory_store::write_meta(dir, CHECK_META, &Value::Object(kept)) {
        harness_types::diag::debug("memory", "citations_not_recorded")
            .field("dir", dir.display().to_string())
            .field("error", error)
            .emit();
    }
}

/// The last checks the store of `dir` keeps, by memory id.
#[must_use]
pub fn recorded(dir: &Path) -> BTreeMap<String, Value> {
    memory_store::read_meta(dir, CHECK_META)
        .and_then(|value| value.as_object().cloned())
        .map(|kept| kept.into_iter().collect())
        .unwrap_or_default()
}

/// A turn's digest state as the memory switch leaves it: the code its shown
/// entries cite checked in `workspace` while `citations` is on, and the
/// entries kept for some files not there at all while `scoped` is off.
pub fn for_digest(
    state: &mut super::harness::State,
    workspace: &Path,
    switch: &super::memory_switch::Switch,
) {
    let features = Features::of(switch);
    state.checker = features.citations.then(|| Checker::new(workspace));
    if !features.scoped {
        for entries in state.entries.values_mut() {
            entries.retain(|entry| !entry.cited.scoped());
        }
    }
}

/// How a project's entries stand, for `ha doctor`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Count {
    /// Live entries that cite code.
    pub citing: usize,
    /// Of those, the ones a citation of which no longer holds.
    pub broken: usize,
    /// Live entries kept for some files only.
    pub scoped: usize,
}

/// [`Count`] of the global store's entries the project at `root` sees (its
/// own and every project's), their citations checked now in `root`.
#[must_use]
pub fn count(data_dir: &Path, root: &Path) -> Count {
    let global = super::harness::global_dir(data_dir);
    if !memory_store::db_path(&global).is_file() {
        return Count::default();
    }
    let state = super::harness::for_project(
        super::harness::load(&global, "global"),
        Some(super::bootstrap::project_key_of(root).as_str()),
    );
    let checker = Checker::new(root);
    let mut count = Count::default();
    for entry in state
        .entries
        .values()
        .flatten()
        .filter(|entry| !entry.ended && !entry.standing.closed())
    {
        if entry.cited.scoped() {
            count.scoped += 1;
        }
        if let Some(checked) = checker.check(&entry.cited) {
            count.citing += 1;
            if checked.verdict == Verdict::Broken {
                count.broken += 1;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("dir");
        std::fs::create_dir_all(directory.path().join("src/billing")).expect("dirs");
        std::fs::write(
            directory.path().join("src/billing/tax.rs"),
            "// Tax.\n\npub fn rate() -> f64 {\n    0.0825 // the Austin rate\n}\n",
        )
        .expect("file");
        directory
    }

    #[test]
    fn a_quote_is_found_with_whitespace_aside_and_kept_with_its_lines() {
        let directory = workspace();
        let verifier = Checker::new(directory.path());
        let kept = verifier
            .verify(&json!([{"path": "./src/billing/tax.rs", "quote": "pub fn rate()   -> f64 {\n 0.0825"}]))
            .expect("holds");
        assert_eq!(
            kept,
            [
                json!({"path": "src/billing/tax.rs", "quote": "pub fn rate()   -> f64 {\n 0.0825", "lines": [3, 4],
                    "project": super::super::bootstrap::project_key_of(directory.path())})
            ]
        );
        // Read in another project, it is not checked: those are other files.
        let mut elsewhere = kept[0].clone();
        elsewhere["project"] = json!("another-project");
        let checked = verifier
            .check(&Cited::of(&json!({"citations": [elsewhere]})))
            .expect("cites");
        assert_eq!(checked.verdict, Verdict::Unchecked);
        let within = verifier
            .verify(&json!([{"path": "src/billing/tax.rs", "quote": "0.0825 // the Austin rate", "lines": [4, 4]}]))
            .expect("within");
        assert_eq!(within[0]["lines"], json!([4, 4]));
    }

    #[test]
    fn a_citation_that_does_not_hold_is_refused_with_the_reason() {
        let directory = workspace();
        let verifier = Checker::new(directory.path());
        let refused = |given: Value| verifier.verify(&given).expect_err("refused");
        assert!(
            refused(json!([{"path": "src/billing/tax.rs", "quote": "0.0900 // a rate"}]))
                .contains("does not hold that quote")
        );
        assert!(
            refused(json!([{"path": "src/billing/gone.rs", "quote": "pub fn rate()"}]))
                .contains("is gone")
        );
        assert!(
            refused(json!([{"path": "../outside.rs", "quote": "pub fn rate()"}]))
                .contains("not inside the workspace")
        );
        assert!(
            refused(json!([{"path": "src/billing/tax.rs", "quote": "rate"}]))
                .contains("fewer than")
        );
        assert!(
            refused(json!([{"path": "src/billing/tax.rs", "quote": "0.0825 // the Austin rate", "lines": [1, 2]}]))
                .contains("at lines 4-4, not within 1-2")
        );
        assert_eq!(
            verifier.verify(&Value::Null).expect("cleared"),
            Vec::<Value>::new()
        );
    }

    #[test]
    fn a_citation_holds_moves_with_its_code_and_breaks_when_the_code_goes() {
        let directory = workspace();
        let verifier = Checker::new(directory.path());
        let kept = verifier
            .verify(&json!([{"path": "src/billing/tax.rs", "quote": "0.0825 // the Austin rate"}]))
            .expect("holds");
        let mut cited = Cited::of(&json!({"citations": kept}));
        let checked = Checker::new(directory.path()).check(&cited).expect("cites");
        assert_eq!(checked.verdict, Verdict::Holds);
        assert_eq!(checked.states[0].holds, Holds::Same);
        assert_eq!(
            checked.note().as_deref(),
            Some("[cites src/billing/tax.rs:4]")
        );
        // Two lines above it: the code moved, the citation still holds there.
        let file = directory.path().join("src/billing/tax.rs");
        std::fs::write(
            &file,
            "// Tax.\n// Rates by city.\n\n\npub fn rate() -> f64 {\n    0.0825 // the Austin rate\n}\n",
        )
        .expect("moved");
        let checked = Checker::new(directory.path()).check(&cited).expect("cites");
        assert_eq!(checked.verdict, Verdict::Holds);
        assert_eq!(checked.states[0].holds, Holds::Moved);
        assert_eq!(checked.states[0].lines, Some((6, 6)));
        // The next check starts where the last one found it.
        cited.last = Some(checked.record());
        let again = Checker::new(directory.path()).check(&cited).expect("cites");
        assert_eq!(again.states[0].lines, Some((6, 6)));
        // The quoted code changed: the citation no longer holds.
        std::fs::write(
            &file,
            "pub fn rate() -> f64 {\n    0.0900 // the Austin rate\n}\n",
        )
        .expect("changed");
        let broken = Checker::new(directory.path()).check(&cited).expect("cites");
        assert_eq!(broken.verdict, Verdict::Broken);
        assert!(
            broken
                .note()
                .expect("note")
                .contains("its cited code changed since")
        );
        std::fs::remove_file(&file).expect("gone");
        let gone = Checker::new(directory.path()).check(&cited).expect("cites");
        assert_eq!(gone.states[0].reason.as_deref(), Some("is gone"));
    }

    #[test]
    fn the_read_budget_leaves_the_rest_unchecked() {
        let directory = workspace();
        let verifier = Checker::new(directory.path());
        verifier.budget.store(0, Ordering::SeqCst);
        let cited = Cited::of(
            &json!({"citations": [{"path": "src/billing/other.rs", "quote": "never read"}]}),
        );
        std::fs::write(
            directory.path().join("src/billing/other.rs"),
            "never read here\n",
        )
        .expect("file");
        let checked = verifier.check(&cited).expect("cites");
        assert_eq!(checked.verdict, Verdict::Unchecked);
        assert!(checked.note().is_none());
    }

    #[test]
    fn globs_match_gitignore_style_and_bad_ones_are_refused() {
        let cited = Cited::of(&json!({"paths": ["src/billing/**", "*.sql", "docs/api"]}));
        assert!(cited.applies_to("src/billing/tax.rs"));
        assert!(cited.applies_to("src/billing/deep/inner.rs"));
        assert!(!cited.applies_to("src/billingx/tax.rs"));
        assert!(cited.applies_to("db/migrations/001.sql"));
        assert!(cited.applies_to("docs/api/index.md"));
        assert!(!cited.applies_to("src/main.rs"));
        assert!(cited.names("./src/billing/tax.rs"));
        assert_eq!(
            verify_paths(&json!(["./src/billing/**", "src/billing/**"])).expect("paths"),
            ["src/billing/**"]
        );
        assert!(
            verify_paths(&json!(["src/[billing"]))
                .expect_err("bad")
                .contains("is not a glob")
        );
        assert!(
            verify_paths(&json!(["../x/**"]))
                .expect_err("outside")
                .contains("not inside")
        );
        assert!(verify_paths(&json!("src/**")).is_err());
    }

    /// A leading `/` anchors a glob at the workspace root, as in gitignore:
    /// it is no absolute path, and a name under it is not matched at any
    /// depth.
    #[test]
    fn a_leading_slash_anchors_a_glob_at_the_root() {
        assert_eq!(
            verify_paths(&json!(["/Makefile", "/docs/"])).expect("anchored"),
            ["/Makefile", "/docs/"]
        );
        let cited = Cited::of(&json!({"paths": ["/Makefile", "/docs/"]}));
        assert!(cited.applies_to("Makefile"));
        assert!(!cited.applies_to("vendor/lib/Makefile"));
        assert!(cited.applies_to("docs/api/index.md"));
        assert!(!cited.applies_to("src/docs/index.md"));
        assert!(
            verify_paths(&json!(["C:/work/src/**"]))
                .expect_err("a drive")
                .contains("not inside")
        );
    }
}
