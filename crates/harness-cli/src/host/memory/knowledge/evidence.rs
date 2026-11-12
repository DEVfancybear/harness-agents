//! Where a page's evidence is read, and what a reference to it means.
//!
//! One resolver for every writer and reader of a source: the wiki pass reads
//! a page's sources to check its claims against them, the store checks a
//! footnote's references when a page is written, and `knowledge evidence`
//! opens the source behind a statement. "Can LLM Agents Organize Their Own
//! Memory?" (`SoMem`, arXiv 2611.11929) validates every evidence reference
//! before an edit is accepted and found that dropping the Source records,
//! the evidence the references resolve to, costs the most accuracy of any
//! part (`docs/architecture/KNOWLEDGE_WIKI.en.md`). A second resolver would
//! disagree with the first about what a source is.
//!
//! A reference is one of:
//!
//! - a workspace file: `path`, `path:line` or `path:start-end`;
//! - a source of this project's journal, as `history_search` returns it
//!   (`event_<uuid>`) and `history_read` takes it;
//! - a conversation of this project (`task_<uuid>`);
//! - an `http(s)://` URL, which is checked for its syntax only: it cannot be
//!   resolved offline, and a reference that may be wrong is not invented.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The most bytes of a file read to count its lines or show them.
const SOURCE_READ: u64 = 1 << 20;

/// The largest file whose lines are counted, to check a cited range: past it
/// the range cannot be checked, so it is refused rather than guessed.
const COUNT_LINES_UP_TO: u64 = 8 << 20;

/// The characters of a source `knowledge evidence` shows for one reference.
pub(crate) const OPEN_CHARS: usize = 4_000;

/// The lines of a file shown around a cited range.
const CONTEXT_LINES: usize = 2;

/// The lines shown of a file cited without a line.
const WHOLE_FILE_LINES: usize = 40;

/// The longest reference.
const MAX_REFERENCE: usize = 2_048;

/// What a footnote's reference names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Reference {
    /// A workspace file, whole or a range of its lines (1-based, inclusive).
    File {
        path: String,
        lines: Option<(usize, usize)>,
    },
    /// A source of the journal: `history_search`'s `source_id`.
    Source(String),
    /// A conversation (task) of the project.
    Conversation(String),
    /// A web address, not fetched.
    Url(String),
}

impl Reference {
    /// Read `token` as a reference: its syntax only, nothing is looked up.
    ///
    /// # Errors
    /// The token is empty, too long, a URL with no host, or a file with a
    /// line range that cannot be (line 0, a range that runs backwards).
    pub(crate) fn parse(token: &str) -> Result<Self, String> {
        let token = token.trim();
        if token.is_empty() {
            return Err("a reference is empty".to_owned());
        }
        if token.len() > MAX_REFERENCE {
            return Err(format!("a reference is at most {MAX_REFERENCE} characters"));
        }
        let lower = token.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            let rest = &token[token.find("://").map_or(0, |at| at + 3)..];
            if rest.is_empty()
                || rest.starts_with(['/', '?', '#'])
                || token.chars().any(|c| c.is_whitespace() || c.is_control())
            {
                return Err(format!(
                    "{token:?} is not a URL: it needs a host and no spaces"
                ));
            }
            return Ok(Self::Url(token.to_owned()));
        }
        if harness_types::EventId::parse(token).is_ok() {
            return Ok(Self::Source(token.to_owned()));
        }
        if harness_types::TaskId::parse(token).is_ok() {
            return Ok(Self::Conversation(token.to_owned()));
        }
        if token.contains("://") {
            return Err(format!(
                "{token:?} is not a reference: a web address starts with http:// or https://"
            ));
        }
        let (path, suffix) = split_lines(token);
        let lines = suffix.map(parse_lines).transpose()?;
        Ok(Self::File {
            path: path.to_owned(),
            lines,
        })
    }

    /// The reference as it is written.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::File { path, lines: None } => path.clone(),
            Self::File {
                path,
                lines: Some((start, end)),
            } if start == end => format!("{path}:{start}"),
            Self::File {
                path,
                lines: Some((start, end)),
            } => format!("{path}:{start}-{end}"),
            Self::Source(id) | Self::Conversation(id) | Self::Url(id) => id.clone(),
        }
    }
}

/// The path and the `:line` or `:start-end` that ends it, when it ends in
/// one: digits and dashes after the last colon.
pub(crate) fn split_lines(source: &str) -> (&str, Option<&str>) {
    match source.rsplit_once(':') {
        Some((path, lines))
            if !path.is_empty()
                && !lines.is_empty()
                && lines.chars().all(|c| c.is_ascii_digit() || c == '-') =>
        {
            (path, Some(lines))
        }
        _ => (source, None),
    }
}

fn parse_lines(suffix: &str) -> Result<(usize, usize), String> {
    let number = |text: &str| {
        text.parse::<usize>()
            .ok()
            .filter(|number| *number > 0)
            .ok_or_else(|| format!("line {text:?} is not a line number (lines start at 1)"))
    };
    match suffix.split_once('-') {
        None => number(suffix).map(|line| (line, line)),
        Some((start, end)) => {
            let (start, end) = (number(start)?, number(end)?);
            if end < start {
                return Err(format!("lines {start}-{end} run backwards"));
            }
            Ok((start, end))
        }
    }
}

/// Where a page's sources are read: the workspace's files and the
/// conversations of its project's journal.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    workspace: Option<PathBuf>,
    /// The main checkout of the workspace when it is a linked worktree: the
    /// project library lives there, and a file a worktree does not have may
    /// be there.
    main: Option<PathBuf>,
    journal: Option<PathBuf>,
    /// Whether a path relative to the workspace names a source. The global
    /// library is shared by every project: its pages' relative paths are not
    /// this workspace's files, so only an absolute path into it, or a
    /// conversation, is read for them.
    relative: bool,
}

impl Sources {
    #[must_use]
    pub fn new(workspace: &Path, data_dir: &Path) -> Self {
        let main = crate::interactive::learned::main_checkout(workspace);
        Self {
            workspace: Some(workspace.to_path_buf()),
            main: (main != workspace).then_some(main),
            journal: Some(crate::interactive::memory_provenance::store_dir(
                data_dir,
                &crate::interactive::memory::ProjectTag::of(workspace).key,
            )),
            relative: true,
        }
    }

    /// Sources of one workspace and no journal, for tests.
    #[cfg(test)]
    pub(crate) fn in_workspace(workspace: PathBuf) -> Self {
        Self {
            workspace: Some(workspace),
            relative: true,
            ..Self::default()
        }
    }

    /// The same with a journal directory, for tests.
    #[cfg(test)]
    pub(crate) fn with_journal(mut self, journal: PathBuf) -> Self {
        self.journal = Some(journal);
        self
    }

    /// The sources as a library of `scope` reads them: the global library
    /// reads no path relative to this workspace.
    #[must_use]
    pub fn for_scope(&self, scope: &str) -> Self {
        Self {
            relative: self.relative && scope != "global",
            ..self.clone()
        }
    }

    /// The roots a relative path is looked for in, the workspace first.
    fn roots(&self) -> Vec<&Path> {
        self.workspace
            .iter()
            .chain(&self.main)
            .map(PathBuf::as_path)
            .collect()
    }

    /// The workspace file a bare `path` names: a file in the workspace (or
    /// its main checkout) that is neither a protected path (`.env`, `.git`,
    /// `.harness`, a key, a name with `secret` or `credential` in it:
    /// `is_sensitive_workspace_path`) nor one Git ignores. Its text goes to
    /// the model. Says why when it is not one.
    fn locate(&self, path: &str) -> Result<PathBuf, String> {
        if self.workspace.is_none() {
            return Err(format!("{path}: no workspace to read a file from"));
        }
        let path = path.trim();
        let given = Path::new(path);
        let candidates = if given.is_absolute() {
            vec![(given.to_path_buf(), None)]
        } else if self.relative {
            self.roots()
                .into_iter()
                .map(|root| (root.join(given), Some(root)))
                .collect()
        } else {
            return Err(format!(
                "{path}: the global library is shared by every project, so a path relative to one workspace means nothing in it; cite an absolute path, a history source or a URL"
            ));
        };
        let mut why = format!("{path}: no such file in the workspace");
        for (full, root) in candidates {
            let Ok(full) = full.canonicalize() else {
                continue;
            };
            let inside_root = root.map_or_else(
                || {
                    self.roots()
                        .into_iter()
                        .filter_map(|root| root.canonicalize().ok())
                        .find(|root| full.starts_with(root))
                },
                |root| root.canonicalize().ok(),
            );
            let Some(inside_root) = inside_root else {
                why = format!("{path}: outside the workspace");
                continue;
            };
            let Ok(inside) = full.strip_prefix(&inside_root) else {
                why = format!("{path}: outside the workspace");
                continue;
            };
            if !full.is_file() {
                why = format!("{path}: not a file");
                continue;
            }
            if harness_tools::is_sensitive_workspace_path(inside)
                || (!given.is_absolute() && harness_tools::is_sensitive_workspace_path(given))
            {
                return Err(format!(
                    "{path}: a protected path (.env, .git, .harness, a key, a secret) is never a source"
                ));
            }
            if ignored(&inside_root, &full) {
                return Err(format!(
                    "{path}: a file Git ignores (a build output or a local file) is not a source"
                ));
            }
            return Ok(full);
        }
        Err(why)
    }

    /// The workspace file `source` names: a path in the workspace (a
    /// `:line` or `:start-end` after it is left off) that [`Self::locate`]
    /// accepts.
    pub(super) fn file(&self, source: &str) -> Option<PathBuf> {
        let source = source.trim();
        if source.contains("://") || source.is_empty() {
            return None;
        }
        self.locate(split_lines(source).0).ok()
    }

    /// The conversation `source` names: a word of it that is a conversation
    /// id of the project's journal.
    fn conversation(&self, source: &str) -> Option<String> {
        self.journal.as_ref()?;
        source
            .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | ';' | '<' | '>'))
            .map(|word| word.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
            .find(|word| harness_types::TaskId::parse(*word).is_ok())
            .map(str::to_owned)
    }

    /// The text of `source`: a workspace file, or a conversation's turns.
    pub(super) fn read(&self, source: &str) -> Option<String> {
        if let Some(path) = self.file(source) {
            use std::io::Read as _;
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .ok()?
                .take(SOURCE_READ)
                .read_to_end(&mut bytes)
                .ok()?;
            return Some(String::from_utf8_lossy(&bytes).into_owned());
        }
        let task = self.conversation(source)?;
        self.conversation_text(&task)
    }

    /// The turns of a conversation of the project, joined.
    fn conversation_text(&self, task: &str) -> Option<String> {
        let said = crate::interactive::memory_provenance::of_task(self.journal.as_ref()?, task);
        (!said.is_empty()).then(|| {
            said.iter()
                .map(|part| part.text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    /// What changes when the source changes, cheaply: a file's length and
    /// time, a conversation's id.
    pub(super) fn stamp(&self, source: &str) -> Option<String> {
        if let Some(path) = self.file(source) {
            let metadata = std::fs::metadata(path).ok()?;
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |time| time.as_secs());
            return Some(format!("{}:{modified}", metadata.len()));
        }
        self.conversation(source)
    }

    /// A source of the journal: its kind and the first `chars` characters of
    /// its text, and whether the rest was cut. Reads the project's journal
    /// file directly and read-only, as the conversation reads do.
    fn journal_source(&self, id: &str, chars: usize) -> Result<(String, String, bool), String> {
        let unknown = || {
            format!(
                "{id} is not a source of this project's journal: copy a source_id whole from a history_search hit"
            )
        };
        let file = self
            .journal
            .as_ref()
            .ok_or_else(unknown)?
            .join("harness.sqlite3");
        if !file.is_file() {
            return Err(unknown());
        }
        let connection = rusqlite::Connection::open_with_flags(
            &file,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| unknown())?;
        connection
            .busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| unknown())?;
        let limit = i64::try_from(chars.saturating_add(1)).unwrap_or(i64::MAX);
        let (kind, availability, text) = connection
            .query_row(
                "SELECT kind, availability, substr(content, 1, ?1) FROM history_sources WHERE source_id = ?2",
                rusqlite::params![limit, id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|_| unknown())?;
        if availability != "available" {
            return Err(format!(
                "{id} is a source of the journal that is no longer available"
            ));
        }
        let cut = text.chars().count() > chars;
        Ok((kind, text.chars().take(chars).collect(), cut))
    }

    /// Check that `reference` resolves: a file that exists (and has the
    /// lines cited), a source or conversation of the journal, a URL that is
    /// well formed. The reason names what would resolve.
    ///
    /// # Errors
    /// The reference does not resolve.
    pub(crate) fn check(&self, reference: &Reference) -> Result<(), String> {
        match reference {
            Reference::Url(_) => Ok(()),
            Reference::Source(id) => self.journal_source(id, 1).map(|_| ()),
            Reference::Conversation(task) => {
                self.conversation_text(task).map(|_| ()).ok_or_else(|| {
                    format!(
                        "{task} is not a conversation of this project's journal; cite a source_id from history_search, or a file"
                    )
                })
            }
            Reference::File { path, lines } => {
                let full = self.locate(path)?;
                if let Some((start, end)) = lines {
                    let count = line_count(&full)?;
                    if *end > count {
                        return Err(format!(
                            "{path} has {count} line(s): lines {start}-{end} are out of range; cite lines within 1-{count}"
                        ));
                    }
                }
                Ok(())
            }
        }
    }

    /// The source behind `reference`, as `knowledge evidence` shows it: a
    /// file's cited lines with a little around them, a journal source's
    /// text (cut at [`OPEN_CHARS`]), a conversation's turns, a URL as it is.
    ///
    /// # Errors
    /// The reference no longer resolves.
    pub(crate) fn open(&self, reference: &Reference) -> Result<Value, String> {
        let shown = reference.describe();
        match reference {
            Reference::Url(url) => Ok(json!({
                "ref": shown, "kind": "url", "url": url,
                "note": "a web address is not fetched; open it with a web tool if it is needed",
            })),
            Reference::Source(id) => {
                let (kind, text, cut) = self.journal_source(id, OPEN_CHARS)?;
                Ok(json!({
                    "ref": shown, "kind": "history", "source_kind": kind,
                    "text": text, "truncated": cut,
                    "note": "history_read reads the rest, by this source_id",
                }))
            }
            Reference::Conversation(task) => {
                let text = self.conversation_text(task).ok_or_else(|| {
                    format!("{task} is not a conversation of this project's journal")
                })?;
                let cut = text.chars().count() > OPEN_CHARS;
                Ok(json!({
                    "ref": shown, "kind": "conversation",
                    "text": text.chars().take(OPEN_CHARS).collect::<String>(), "truncated": cut,
                }))
            }
            Reference::File { path, lines } => self.open_file(&shown, path, *lines),
        }
    }

    fn open_file(
        &self,
        shown: &str,
        path: &str,
        lines: Option<(usize, usize)>,
    ) -> Result<Value, String> {
        use std::fmt::Write as _;
        use std::io::Read as _;
        let full = self.locate(path)?;
        let mut bytes = Vec::new();
        std::fs::File::open(&full)
            .and_then(|file| file.take(SOURCE_READ).read_to_end(&mut bytes))
            .map_err(|error| format!("{path}: {error}"))?;
        let text = String::from_utf8_lossy(&bytes);
        let all = text.lines().collect::<Vec<_>>();
        if lines.is_some_and(|(start, _)| start > all.len()) {
            return Err(format!(
                "{path} has {} line(s): {shown} is out of range",
                all.len()
            ));
        }
        let (from, to) = lines.map_or((1, WHOLE_FILE_LINES), |(start, end)| {
            (
                start.saturating_sub(CONTEXT_LINES).max(1),
                end + CONTEXT_LINES,
            )
        });
        let mut shown_text = String::new();
        for (offset, line) in all.iter().enumerate().skip(from - 1).take(to - from + 1) {
            let number = offset + 1;
            let cited = lines.is_none_or(|(start, end)| (start..=end).contains(&number));
            let _ = writeln!(
                shown_text,
                "{}{number}| {line}",
                if cited { ">" } else { " " }
            );
        }
        let cut = shown_text.chars().count() > OPEN_CHARS;
        Ok(json!({
            "ref": shown, "kind": "file", "path": path,
            "text": shown_text.chars().take(OPEN_CHARS).collect::<String>(),
            "truncated": cut,
            "note": "lines marked > are the cited ones; the others are context",
        }))
    }
}

/// How many lines a file has, counting a last line with no newline.
fn line_count(file: &Path) -> Result<usize, String> {
    use std::io::Read as _;
    let length = std::fs::metadata(file)
        .map_err(|error| error.to_string())?
        .len();
    if length > COUNT_LINES_UP_TO {
        return Err(format!(
            "{} is over {} MiB: its lines cannot be checked, cite the file without a line",
            file.display(),
            COUNT_LINES_UP_TO >> 20
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(file)
        .and_then(|file| file.take(COUNT_LINES_UP_TO).read_to_end(&mut bytes))
        .map_err(|error| error.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).lines().count())
}

/// Whether Git ignores `file`, a file under `root`: the `.gitignore` files of
/// `root` and of each folder down to the file, and `.git/info/exclude`. An
/// ignored file is a build output or a local file the project keeps out of
/// its history, not a source a page is checked against.
fn ignored(root: &Path, file: &Path) -> bool {
    let Ok(inside) = file.strip_prefix(root) else {
        return true;
    };
    let mut folders = vec![root.to_path_buf()];
    let mut folder = root.to_path_buf();
    if let Some(parent) = inside.parent() {
        for part in parent.components() {
            folder.push(part);
            folders.push(folder.clone());
        }
    }
    let mut is_ignored = false;
    for (index, folder) in folders.iter().enumerate() {
        let mut builder = ignore::gitignore::GitignoreBuilder::new(folder);
        let mut any = false;
        for list in [
            folder.join(".gitignore"),
            folder.join(".git").join("info").join("exclude"),
        ] {
            if (index == 0 || list.ends_with(".gitignore")) && list.is_file() {
                any |= builder.add(&list).is_none();
            }
        }
        if !any {
            continue;
        }
        let Ok(matcher) = builder.build() else {
            continue;
        };
        match matcher.matched_path_or_any_parents(file, false) {
            ignore::Match::Ignore(_) => is_ignored = true,
            ignore::Match::Whitelist(_) => is_ignored = false,
            ignore::Match::None => {}
        }
    }
    is_ignored
}
