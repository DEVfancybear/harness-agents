use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{Condvar, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use globset::Glob;
use harness_store_sqlite::ProjectRegistrationRecord;
use harness_types::{ContentHash, ErrorCode, HarnessError, ProjectId, WorkspaceObservation};
use regex::RegexBuilder;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    contracts::EditSpec,
    contracts::{SearchMatch, observation},
    edit_diff::{PlannedEdit, plan_edits},
    truncate::{TruncatedBy, TruncationLimits, format_size, truncate_head},
    walk::{TOOL_WALK_DEADLINE, Walk, WalkFile, cached_walk, note_change, relative_text},
};

pub(crate) const MAX_TEXT_FILE_BYTES: usize = 1024 * 1024;
#[cfg(test)]
const MAX_OUTPUT_BYTES: usize = 128 * 1024;
/// Most entries one `list_files` or `search_text` result carries.
pub(crate) const MAX_SEARCH_MATCHES: usize = 512;

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceDescriptor {
    pub project_id: ProjectId,
    pub root: PathBuf,
    pub root_text: String,
    pub git_common_dir: Option<String>,
    pub git_head: String,
    pub identity_hash: ContentHash,
    pub fingerprint: ContentHash,
}

impl WorkspaceDescriptor {
    #[must_use]
    pub(crate) fn registration(&self) -> ProjectRegistrationRecord {
        ProjectRegistrationRecord {
            project_id: self.project_id.clone(),
            canonical_root: self.root_text.clone(),
            git_common_dir: self.git_common_dir.clone(),
            identity_hash: self.identity_hash.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TextOutput {
    pub text: String,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct SearchOutput {
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceMutation {
    /// Hash of the actual prior content, or of the canonical absent-file marker.
    pub before_hash: ContentHash,
    pub after_hash: ContentHash,
    pub replacements: u64,
}

/// Produce the P2 workspace observation used when a P3 CLI flow starts a
/// runtime. Execution always recomputes this snapshot in the durable gate.
pub fn observe_workspace(
    project_id: ProjectId,
    root: impl AsRef<Path>,
) -> Result<WorkspaceObservation, HarnessError> {
    let descriptor = inspect_workspace(root.as_ref(), project_id.clone())?;
    let worktree_id = format!(
        "p3-{}",
        descriptor
            .identity_hash
            .as_str()
            .strip_prefix("sha256:")
            .unwrap_or(descriptor.identity_hash.as_str())
            .chars()
            .take(16)
            .collect::<String>()
    );
    Ok(observation(
        project_id,
        worktree_id,
        descriptor.git_head,
        descriptor.fingerprint,
    ))
}

/// The registration record of one workspace root.
///
/// A project id is generated, never derived, so a caller that wants the same
/// identity on the next run must register this record and read the id back. The
/// canonical root, Git common directory and identity hash come from the same
/// inspection the execution gate performs, so the record cannot disagree with the
/// workspace the tools later act on — and registration never hashes workspace
/// files, which keeps it usable while a build or an editor holds them.
pub fn workspace_registration(
    project_id: ProjectId,
    root: impl AsRef<Path>,
) -> Result<ProjectRegistrationRecord, HarnessError> {
    let identity = inspect_identity(root.as_ref())?;
    Ok(ProjectRegistrationRecord {
        project_id,
        canonical_root: identity.root_text,
        git_common_dir: identity.git_common_dir,
        identity_hash: identity.identity_hash,
    })
}

/// Observe the current text content hash through the same rooted path and
/// sensitive-path guards used by P3. This is a read-only CLI setup helper for
/// deterministic fixtures; it is not an execution authority.
pub fn observed_file_hash(
    root: impl AsRef<Path>,
    relative_path: &str,
) -> Result<ContentHash, HarnessError> {
    let descriptor = inspect_workspace(root.as_ref(), ProjectId::generate())?;
    let target = resolve_relative(&descriptor.root, relative_path, false)?;
    let text = read_text(&target)?;
    Ok(ContentHash::from_bytes(text.as_bytes()))
}

/// The identity of one workspace root: where it is, and which project it is.
///
/// Identity is deliberately independent of the current revision, so it needs no
/// file walk. Callers that only need to recognise a root again — registering and
/// resolving a project — use this instead of hashing every workspace file.
#[derive(Clone, Debug)]
pub(crate) struct WorkspaceIdentity {
    pub root: PathBuf,
    pub root_text: String,
    pub git_common_dir: Option<String>,
    pub identity_hash: ContentHash,
}

pub(crate) fn inspect_identity(root: &Path) -> Result<WorkspaceIdentity, HarnessError> {
    let canonical = fs::canonicalize(root).map_err(|error| {
        HarnessError::new(
            ErrorCode::WorkspaceEscape,
            format!("cannot canonicalize workspace root: {error}"),
        )
    })?;
    let metadata = fs::metadata(&canonical).map_err(|error| {
        HarnessError::new(
            ErrorCode::WorkspaceEscape,
            format!("cannot inspect workspace root: {error}"),
        )
    })?;
    if !metadata.is_dir() || is_link_or_reparse(&canonical)? {
        return Err(HarnessError::new(
            ErrorCode::WorkspaceEscape,
            "workspace root must be a real directory, not a link or reparse point",
        ));
    }
    let root_text = canonical_path_text(&canonical)?;
    let git_common_dir = cached_git_common_dir(&canonical);
    // Project identity is deliberately independent of the current revision.
    // `git_head` and all tracked/untracked file content remain in the mutable
    // workspace fingerprint, so an external commit invalidates an approval
    // rather than incorrectly turning an established project into a different
    // project registration.
    let identity_value = json!({
        "canonical_root": root_text,
        "git_common_dir": git_common_dir,
    });
    let identity_hash = ContentHash::from_canonical_json(&identity_value)?;
    Ok(WorkspaceIdentity {
        root: canonical,
        root_text,
        git_common_dir,
        identity_hash,
    })
}

/// The Git directory of a root, asked of `git` once per process and root.
///
/// Every tool call inspects the workspace, and on Windows each `git` spawn costs
/// tens of milliseconds. The answer only changes when a repository is created or
/// removed, so the cache is keyed by whether the root has a `.git` entry.
fn cached_git_common_dir(root: &Path) -> Option<String> {
    cached_git_path(root, "--git-common-dir")
}

/// The Git directory of the root's own worktree, where its `HEAD` and index
/// live; the common directory for the main worktree.
fn cached_git_dir(root: &Path) -> Option<String> {
    cached_git_path(root, "--git-dir")
}

fn cached_git_path(root: &Path, flag: &'static str) -> Option<String> {
    type GitDirs = HashMap<(PathBuf, bool, &'static str), Option<String>>;
    static CACHE: OnceLock<Mutex<GitDirs>> = OnceLock::new();
    let key = (root.to_owned(), root.join(".git").exists(), flag);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(value) = cache.lock().ok().and_then(|map| map.get(&key).cloned()) {
        return value;
    }
    let value =
        git_output(root, ["rev-parse", flag]).and_then(|value| canonicalize_git_path(root, &value));
    // No answer for a root that has a `.git` entry is a `git` that failed this
    // once (a timeout, a lock), not a root outside a repository: it is asked
    // again next time instead of being remembered for the whole session, where it
    // would change the project's identity until the app restarts.
    if (value.is_some() || !key.1)
        && let Ok(mut map) = cache.lock()
    {
        map.insert(key, value.clone());
    }
    value
}

/// What the repository files say `HEAD` is.
enum HeadRead {
    Commit(String),
    /// A repository with no commit yet, or no repository.
    Unborn,
    /// Something these files do not settle (a reftable, a symbolic ref chain):
    /// `git` is asked.
    Unknown,
}

fn is_commit_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The commit `HEAD` names, read from the repository's own files: `HEAD`, the
/// loose ref it points at, or `packed-refs`. A `git rev-parse HEAD` process on
/// every write cost tens of milliseconds on Windows.
fn head_from_files(root: &Path) -> HeadRead {
    let (Some(git_dir), Some(common)) = (cached_git_dir(root), cached_git_common_dir(root)) else {
        return if cached_git_common_dir(root).is_none() && !root.join(".git").exists() {
            HeadRead::Unborn
        } else {
            HeadRead::Unknown
        };
    };
    let (git_dir, common) = (PathBuf::from(git_dir), PathBuf::from(common));
    if common.join("reftable").exists() {
        return HeadRead::Unknown;
    }
    let Ok(head) = fs::read_to_string(git_dir.join("HEAD")) else {
        return HeadRead::Unknown;
    };
    let head = head.trim();
    let Some(reference) = head.strip_prefix("ref:").map(str::trim) else {
        return if is_commit_id(head) {
            HeadRead::Commit(head.to_owned())
        } else {
            HeadRead::Unknown
        };
    };
    if !reference.starts_with("refs/") || reference.split('/').any(|part| part == "..") {
        return HeadRead::Unknown;
    }
    for base in [&git_dir, &common] {
        if let Ok(text) = fs::read_to_string(base.join(reference)) {
            let commit = text.trim();
            return if is_commit_id(commit) {
                HeadRead::Commit(commit.to_owned())
            } else {
                HeadRead::Unknown
            };
        }
    }
    if let Ok(packed) = fs::read_to_string(common.join("packed-refs")) {
        for line in packed.lines() {
            if let Some((commit, name)) = line.split_once(' ')
                && name == reference
                && !line.starts_with(['#', '^'])
            {
                return if is_commit_id(commit) {
                    HeadRead::Commit(commit.to_owned())
                } else {
                    HeadRead::Unknown
                };
            }
        }
    }
    HeadRead::Unborn
}

/// The commit at `HEAD`, or `None` outside a repository and before its first
/// commit, as `git rev-parse HEAD` answers.
pub(crate) fn git_head(root: &Path) -> Option<String> {
    match head_from_files(root) {
        HeadRead::Commit(commit) => Some(commit),
        HeadRead::Unborn => None,
        HeadRead::Unknown => git_output(root, ["rev-parse", "HEAD"]),
    }
}

/// The branch `HEAD` is on, as `git rev-parse --abbrev-ref HEAD` names it: the
/// short branch name, `HEAD` when detached, `None` outside a repository and
/// before the first commit. Read from the repository's files like
/// [`git_head`], so the prompt of every turn no longer waits on a `git`
/// process; `git` answers only when the files cannot.
#[must_use]
pub fn git_branch(root: &Path) -> Option<String> {
    let asked = || git_output(root, ["rev-parse", "--abbrev-ref", "HEAD"]);
    match head_from_files(root) {
        HeadRead::Unborn => None,
        HeadRead::Unknown => asked(),
        HeadRead::Commit(_) => {
            let head = fs::read_to_string(PathBuf::from(cached_git_dir(root)?).join("HEAD"));
            match head.as_deref().map(str::trim) {
                Ok(head) => match head.strip_prefix("ref:").map(str::trim) {
                    None => Some("HEAD".to_owned()),
                    Some(reference) => reference
                        .strip_prefix("refs/heads/")
                        .map(str::to_owned)
                        .or_else(asked),
                },
                Err(_) => asked(),
            }
        }
    }
}

/// The hash of the worktree's Git index, through the same size-and-time hash
/// cache as the workspace files.
fn git_index_hash(root: &Path) -> Option<ContentHash> {
    let index = PathBuf::from(cached_git_dir(root)?).join("index");
    let metadata = fs::metadata(&index).ok()?;
    let file = WalkFile {
        absolute: index,
        relative: String::new(),
        len: metadata.len(),
        modified: metadata.modified().ok(),
    };
    hash_files(std::slice::from_ref(&file))
        .ok()?
        .into_iter()
        .next()
        .flatten()
}

pub(crate) fn inspect_workspace(
    root: &Path,
    project_id: ProjectId,
) -> Result<WorkspaceDescriptor, HarnessError> {
    let identity = inspect_identity(root)?;
    let git_head = git_head(&identity.root).unwrap_or_else(|| "not_git".to_owned());
    let fingerprint = workspace_fingerprint(&identity.root, &git_head)?;
    Ok(WorkspaceDescriptor {
        project_id,
        root: identity.root,
        root_text: identity.root_text,
        git_common_dir: identity.git_common_dir,
        git_head,
        identity_hash: identity.identity_hash,
        fingerprint,
    })
}

/// The workspace as a read-only action needs it: where it is, without hashing
/// its files.
///
/// The fingerprint binds an approval to the workspace it was given for, so that a
/// change made while a write waited for its approval invalidates it. A read
/// changes nothing and its result is taken when it runs, so it binds only to the
/// root's identity; the files are not walked three times for one `read_file`.
pub(crate) fn inspect_workspace_for_read(
    root: &Path,
    project_id: ProjectId,
) -> Result<WorkspaceDescriptor, HarnessError> {
    let identity = inspect_identity(root)?;
    let fingerprint = ContentHash::from_canonical_json(&json!({
        "identity": identity.identity_hash,
        "files": "not observed for a read-only action",
    }))?;
    Ok(WorkspaceDescriptor {
        project_id,
        root: identity.root,
        root_text: identity.root_text,
        git_common_dir: identity.git_common_dir,
        git_head: "not_observed".to_owned(),
        identity_hash: identity.identity_hash,
        fingerprint,
    })
}

pub(crate) fn workspace_fingerprint(
    root: &Path,
    git_head: &str,
) -> Result<ContentHash, HarnessError> {
    // The walk is the one the tools share: taken again only when something
    // changed since the last one.
    let walked = if too_large(root) {
        None
    } else {
        let walk = cached_walk(root, root, Some(Instant::now() + FINGERPRINT_WALK_DEADLINE))?;
        if walk.complete {
            Some(walk)
        } else {
            remember_too_large(root);
            None
        }
    };
    let Some(walk) = walked else {
        // Measured: started in the user's home folder, the walk took 32 seconds
        // to reach the bound and then failed every turn before it reached the
        // model, and Ctrl+C waited for it. A workspace this large is fingerprinted
        // by its Git state alone, and remembered as such for the process.
        let status = git_output(root, ["status", "--porcelain=v1", "--untracked-files=all"])
            .unwrap_or_else(|| "not_git".to_owned());
        return ContentHash::from_canonical_json(&json!({
            "git_head": git_head,
            "git_status": status,
            "files": "unobserved: the workspace is too large to walk",
        }));
    };
    let hashes = hash_files(&walk.files)?;
    let mut entries = Vec::with_capacity(walk.files.len());
    for (item, hash) in walk.files.iter().zip(hashes) {
        match hash {
            Some(hash) => entries.push(json!({"path": item.relative, "content_hash": hash})),
            // Present but locked by another process. The path stays in the
            // fingerprint; its content does not. Once it becomes readable the
            // content hash appears and the fingerprint changes, so an approval
            // bound to the unreadable state is invalidated rather than silently
            // comparing equal.
            None => entries.push(json!({
                "path": item.relative,
                "content_hash": null,
                "unreadable": "locked",
            })),
        }
    }
    // What `git status` added beyond the files themselves is the index: what
    // is staged. Its bytes say the same, without a `git` process per call
    // (tens of milliseconds each on Windows, twice per write).
    ContentHash::from_canonical_json(&json!({
        "git_head": git_head,
        "git_index": git_index_hash(root),
        "files": entries,
    }))
}

/// The part of an absolute `path` below `root`, with `/` separators; `None` when
/// `path` is not absolute or not inside `root`. Compared case-insensitively on
/// Windows, and without the verbatim prefix a canonical path carries there.
fn absolute_within(root: &Path, path: &str) -> Option<String> {
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        return None;
    }
    let normalize = |text: &str| {
        let text = text
            .strip_prefix(r"\\?\")
            .unwrap_or(text)
            .replace('\\', "/");
        let text = text.trim_end_matches('/').to_owned();
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    };
    let root_text = root.to_str()?;
    let root_norm = normalize(root_text);
    let original = path
        .strip_prefix(r"\\?\")
        .unwrap_or(path)
        .replace('\\', "/");
    let path_norm = normalize(path);
    if path_norm == root_norm {
        return Some(String::new());
    }
    let prefix = format!("{root_norm}/");
    path_norm
        .starts_with(&prefix)
        .then(|| original[prefix.len()..].trim_end_matches('/').to_owned())
}

pub(crate) fn resolve_relative(
    root: &Path,
    relative: &str,
    allow_root: bool,
) -> Result<PathBuf, HarnessError> {
    if relative.trim().is_empty() {
        if allow_root {
            return Ok(root.to_owned());
        }
        return Err(HarnessError::new(
            ErrorCode::WorkspaceEscape,
            "workspace path must not be empty",
        ));
    }
    // An absolute path inside the workspace is that relative path, as prime-agent's
    // tools resolve it; models name files by the path they were shown. One that
    // leaves the workspace is still refused below.
    let within = absolute_within(root, relative);
    let relative = match &within {
        Some(inside) if inside.is_empty() => {
            if allow_root {
                return Ok(root.to_owned());
            }
            return Err(HarnessError::new(
                ErrorCode::WorkspaceEscape,
                "workspace path must name a file inside the workspace",
            ));
        }
        Some(inside) => inside.as_str(),
        None => relative,
    };
    let input = Path::new(relative);
    if input.is_absolute()
        || input.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(HarnessError::new(
            ErrorCode::WorkspaceEscape,
            "absolute paths and parent traversal are not permitted",
        ));
    }
    let mut candidate = root.to_owned();
    for component in input.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        candidate.push(component);
        if candidate.exists() && is_link_or_reparse(&candidate)? {
            return Err(HarnessError::new(
                ErrorCode::WorkspaceEscape,
                "workspace path traverses a symlink or reparse point",
            ));
        }
    }
    if candidate == root && allow_root {
        return Ok(candidate);
    }
    let parent = candidate.parent().ok_or_else(|| {
        HarnessError::new(ErrorCode::WorkspaceEscape, "workspace path has no parent")
    })?;
    if parent.exists() {
        let canonical_parent = fs::canonicalize(parent).map_err(|error| {
            HarnessError::new(
                ErrorCode::WorkspaceEscape,
                format!("cannot canonicalize workspace path parent: {error}"),
            )
        })?;
        if !canonical_parent.starts_with(root) {
            return Err(HarnessError::new(
                ErrorCode::WorkspaceEscape,
                "workspace path resolves outside the registered root",
            ));
        }
    }
    if is_sensitive_relative(input) {
        return Err(HarnessError::new(
            ErrorCode::SensitivePathDenied,
            "protected or credential-like workspace path is denied",
        ));
    }
    Ok(candidate)
}

pub(crate) fn read_text(path: &Path) -> Result<String, HarnessError> {
    let metadata = fs::metadata(path).map_err(|error| {
        // A path that is not there is the common case, and the OS text ("The
        // system cannot find the path specified. (os error 3)") does not say
        // which path; prime-agent says "File not found: <path>".
        if error.kind() == ErrorKind::NotFound {
            let shown = path.to_string_lossy();
            return HarnessError::new(
                ErrorCode::InvalidPayload,
                format!(
                    "File not found: {}",
                    shown.strip_prefix(r"\\?\").unwrap_or(&shown)
                ),
            );
        }
        HarnessError::new(
            ErrorCode::InvalidPayload,
            format!("cannot inspect workspace file: {error}"),
        )
    })?;
    if !metadata.is_file() {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "workspace path is not a regular file",
        ));
    }
    if metadata.len() > u64::try_from(MAX_TEXT_FILE_BYTES).unwrap_or(u64::MAX) {
        return Err(HarnessError::new(
            ErrorCode::OutputLimitExceeded,
            "text file exceeds the P3 bounded edit limit",
        ));
    }
    let bytes = fs::read(path).map_err(|error| {
        HarnessError::new(
            ErrorCode::InvalidPayload,
            format!("cannot read workspace file: {error}"),
        )
    })?;
    decode_utf8(&bytes)
}

#[cfg(test)]
pub(crate) fn read_text_output(path: &Path) -> Result<TextOutput, HarnessError> {
    let mut file = File::open(path).map_err(|error| {
        HarnessError::new(
            ErrorCode::InvalidPayload,
            format!("cannot open workspace file: {error}"),
        )
    })?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(u64::try_from(MAX_OUTPUT_BYTES + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|error| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                format!("cannot read workspace file: {error}"),
            )
        })?;
    let truncated = bytes.len() > MAX_OUTPUT_BYTES;
    if truncated {
        bytes.truncate(MAX_OUTPUT_BYTES);
        if let Err(error) = std::str::from_utf8(&bytes) {
            // Only an incomplete trailing character is attributable to the
            // byte cap. Invalid bytes inside the prefix must still be denied.
            if error.error_len().is_none() {
                bytes.truncate(error.valid_up_to());
            }
        }
    }
    let text = decode_utf8(&bytes)?;
    Ok(TextOutput {
        text: redact_text(&text),
        truncated,
    })
}

/// The walk a tool lists, globs or searches: `requested` inside `root`, from
/// the shared cache.
pub(crate) fn tool_walk(
    root: &Path,
    requested: Option<&str>,
    tool: &str,
) -> Result<std::sync::Arc<Walk>, HarnessError> {
    let base = match requested {
        Some(path) => resolve_relative(root, path, true)?,
        None => root.to_owned(),
    };
    if !base.is_dir() {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            format!("{tool} path must be a directory"),
        ));
    }
    cached_walk(root, &base, Some(Instant::now() + TOOL_WALK_DEADLINE))
}

fn root_relative(root: &Path, file: &WalkFile) -> Result<String, HarnessError> {
    let relative = file.absolute.strip_prefix(root).map_err(|_| {
        HarnessError::new(
            ErrorCode::WorkspaceEscape,
            "listed path escaped workspace root",
        )
    })?;
    Ok(relative_text(relative))
}

pub(crate) fn list_files(
    root: &Path,
    requested: Option<&str>,
) -> Result<(Vec<String>, bool), HarnessError> {
    let walk = tool_walk(root, requested, "list_files")?;
    let mut paths = Vec::new();
    for file in walk.files.iter().take(MAX_SEARCH_MATCHES) {
        paths.push(root_relative(root, file)?);
    }
    Ok((
        paths,
        walk.files.len() > MAX_SEARCH_MATCHES || !walk.complete,
    ))
}

/// Most paths one `glob` result carries.
pub(crate) const MAX_GLOB_MATCHES: usize = 2_000;

pub(crate) fn glob_files(
    root: &Path,
    requested: Option<&str>,
    pattern: &str,
) -> Result<(Vec<String>, bool), HarnessError> {
    let matcher = Glob::new(pattern)
        .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error.to_string()))?
        .compile_matcher();
    let walk = tool_walk(root, requested, "glob")?;
    let mut paths = Vec::new();
    let mut truncated = !walk.complete;
    for file in &walk.files {
        if matcher.is_match(Path::new(&file.relative)) {
            if paths.len() == MAX_GLOB_MATCHES {
                truncated = true;
                break;
            }
            paths.push(root_relative(root, file)?);
        }
    }
    Ok((paths, truncated))
}

pub(crate) fn validate_glob(pattern: &str) -> Result<(), HarnessError> {
    Glob::new(pattern)
        .map(|_| ())
        .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error.to_string()))
}

pub(crate) fn validate_search(
    query: &str,
    use_regex: bool,
    case_insensitive: bool,
) -> Result<(), HarnessError> {
    if query.is_empty() {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "search_text query must not be empty",
        ));
    }
    let pattern = if use_regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(case_insensitive)
        .build()
        .map(|_| ())
        .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error.to_string()))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the search tool's arguments, as the model sends them"
)]
pub(crate) fn search_text(
    root: &Path,
    query: &str,
    requested: Option<&str>,
    use_regex: bool,
    case_insensitive: bool,
    glob: Option<&str>,
    context_lines: u32,
    anchors: bool,
) -> Result<SearchOutput, HarnessError> {
    validate_search(query, use_regex, case_insensitive)?;
    // A search the stream prefetched, or the same search made again, while
    // nothing changed: the result it would compute now.
    let key = crate::prefetch::search_key(
        root,
        &[
            &query,
            &requested,
            &use_regex,
            &case_insensitive,
            &glob,
            &context_lines,
            &anchors,
        ],
    );
    if let Some(kept) = crate::prefetch::kept_search(&key) {
        return Ok(kept);
    }
    let stamp = crate::walk::generation();
    let watched = crate::walk::ensure_watched(root);
    let walk = tool_walk(root, requested, "search_text")?;
    let found = crate::search::search(
        root,
        &walk,
        &crate::search::SearchQuery {
            query,
            use_regex,
            case_insensitive,
            glob,
            context_lines,
            anchors,
        },
    )?;
    let output = SearchOutput {
        matches: found.matches,
        truncated: found.truncated || !walk.complete,
    };
    crate::prefetch::keep_search(key, &output, stamp, watched);
    Ok(output)
}

/// Read a numbered line range, cut to the shared output limits.
///
/// A range that stops before the end of the file says so in the text, with the
/// `offset` that continues it (prime-agent's read notice, with ha's zero-based
/// offset), so the model pages on instead of re-reading the same slice.
#[cfg(test)]
pub(crate) fn read_file_range(
    path: &Path,
    offset: u64,
    limit: u32,
) -> Result<TextOutput, HarnessError> {
    Ok(numbered_range(&read_text(path)?, offset, limit, false))
}

/// Read a file once and return its numbered range together with the hash of
/// its whole content. The hash is what `write_file` and `apply_patch` take as
/// `expected_hash`, so the model gets it from the read instead of computing it.
pub(crate) fn read_file_range_with_hash(
    path: &Path,
    offset: u64,
    limit: u32,
    anchors: bool,
) -> Result<(TextOutput, ContentHash), HarnessError> {
    let text = read_text(path)?;
    let hash = ContentHash::from_bytes(text.as_bytes());
    Ok((numbered_range(&text, offset, limit, anchors), hash))
}

fn numbered_range(text: &str, offset: u64, limit: u32, anchors: bool) -> TextOutput {
    let lines = text.lines().collect::<Vec<_>>();
    let total = lines.len();
    let start = usize::try_from(offset).unwrap_or(usize::MAX).min(total);
    let count = usize::try_from(limit).unwrap_or(usize::MAX);
    let end = start.saturating_add(count).min(total);
    // With hashline editing on, each line carries the anchor an edit names it
    // by (`12#a3: text`).
    let numbered = lines[start..end]
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let number = start + index + 1;
            if anchors {
                format!(
                    "{}: {}",
                    crate::edit_diff::line_anchor(number, line),
                    redact_text(line)
                )
            } else {
                format!("{number}: {}", redact_text(line))
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let limits = TruncationLimits::default();
    let cut = truncate_head(&numbered, limits);
    let first = start + 1;
    if cut.first_line_exceeds_limit {
        // One line alone is over the limit: show its beginning rather than
        // nothing, and say where the next line starts.
        let line = &numbered[..numbered.find('\n').unwrap_or(numbered.len())];
        let mut kept = limits.max_bytes;
        while !line.is_char_boundary(kept) {
            kept -= 1;
        }
        let limit_size = format_size(limits.max_bytes as u64);
        return TextOutput {
            text: format!(
                "{}\n\n[Line {first} is {}, exceeds {limit_size} limit; showing its first {limit_size}. Use offset={first} to continue after it.]",
                &line[..kept],
                format_size(line.len() as u64),
            ),
            truncated: true,
        };
    }
    let shown_end = start + cut.output_lines;
    if !cut.truncated && end == total {
        return TextOutput {
            text: cut.content,
            truncated: false,
        };
    }
    let limit_note = if cut.truncated_by == Some(TruncatedBy::Bytes) {
        format!(" ({} limit)", format_size(limits.max_bytes as u64))
    } else {
        String::new()
    };
    TextOutput {
        text: format!(
            "{}\n\n[Showing lines {first}-{shown_end} of {total}{limit_note}. Use offset={shown_end} to continue.]",
            cut.content
        ),
        truncated: true,
    }
}

pub(crate) fn apply_text_patch(
    path: &Path,
    expected_hash: &ContentHash,
    replacement: &str,
) -> Result<WorkspaceMutation, HarnessError> {
    write_text_checked(path, Some(expected_hash), replacement)
}

/// Serializes mutations of one file within this process (prime-agent's
/// `file-mutation-queue.ts`).
///
/// Every write re-checks the file's hash just before it replaces it, but two
/// writers inside this process — parallel delegated workers, say — can still
/// both pass that check before either renames. Holding this lock across the
/// read, the check and the replacement closes that window for writers here;
/// the hash check still catches every writer outside the process. Different
/// files never wait for each other.
pub(crate) struct FileMutationGuard {
    key: PathBuf,
}

fn busy_files() -> &'static (Mutex<HashSet<PathBuf>>, Condvar) {
    static BUSY: OnceLock<(Mutex<HashSet<PathBuf>>, Condvar)> = OnceLock::new();
    BUSY.get_or_init(|| (Mutex::new(HashSet::new()), Condvar::new()))
}

/// The key two spellings of one file share: its real path, or its parent's
/// real path and its name when it does not exist yet.
fn mutation_key(path: &Path) -> PathBuf {
    if let Ok(real) = fs::canonicalize(path) {
        return real;
    }
    match (path.parent().map(fs::canonicalize), path.file_name()) {
        (Some(Ok(parent)), Some(name)) => parent.join(name),
        _ => path.to_owned(),
    }
}

/// Wait until no other mutation of `path` runs in this process, then hold it.
pub(crate) fn lock_file_mutation(path: &Path) -> FileMutationGuard {
    let key = mutation_key(path);
    let (busy, released) = busy_files();
    // A poisoned set only means another writer panicked; the set itself is
    // still a set of paths, so keep using it.
    let mut held = busy
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while held.contains(&key) {
        held = released
            .wait(held)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    held.insert(key.clone());
    FileMutationGuard { key }
}

impl Drop for FileMutationGuard {
    fn drop(&mut self) {
        let (busy, released) = busy_files();
        busy.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
        released.notify_all();
    }
}

pub(crate) fn write_text_checked(
    path: &Path,
    expected_hash: Option<&ContentHash>,
    replacement: &str,
) -> Result<WorkspaceMutation, HarnessError> {
    let _guard = lock_file_mutation(path);
    write_text_checked_locked(path, expected_hash, replacement)
}

/// [`write_text_checked`] for a caller that already holds the file's
/// mutation lock.
fn write_text_checked_locked(
    path: &Path,
    expected_hash: Option<&ContentHash>,
    replacement: &str,
) -> Result<WorkspaceMutation, HarnessError> {
    if replacement.len() > MAX_TEXT_FILE_BYTES {
        return Err(HarnessError::new(
            ErrorCode::OutputLimitExceeded,
            "workspace write exceeds the 1 MiB text limit",
        ));
    }
    let existed = path.exists();
    let before_hash = if existed {
        // Read once, under the file's mutation lock, immediately before the
        // atomic replacement: a changed or unreadable preimage is never
        // overwritten.
        let current = read_text(path)?;
        let hash = ContentHash::from_bytes(current.as_bytes());
        let Some(expected_hash) = expected_hash else {
            return Err(HarnessError::new(
                ErrorCode::StaleWorkspace,
                "expected_hash is required to overwrite an existing file",
            ));
        };
        if &hash != expected_hash {
            return Err(HarnessError::new(
                ErrorCode::StaleWorkspace,
                "expected_hash does not match current file content",
            ));
        }
        hash
    } else {
        if expected_hash.is_some() {
            return Err(HarnessError::new(
                ErrorCode::StaleWorkspace,
                "expected_hash was supplied but the target file does not exist",
            ));
        }
        ContentHash::from_canonical_json(&serde_json::json!({"exists": false}))?
    };
    if existed {
        let written = write_text_atomically(path, replacement);
        note_change();
        written?;
    } else {
        let created = create_text_atomically(path, replacement);
        note_change();
        created?;
    }
    let after = read_text(path)?;
    if after != replacement {
        return Err(HarnessError::new(
            ErrorCode::ProcessOutcomeUnknown,
            "workspace replacement cannot be verified",
        ));
    }
    Ok(WorkspaceMutation {
        before_hash,
        after_hash: ContentHash::from_bytes(after.as_bytes()),
        replacements: 1,
    })
}

/// Apply one `edit_file` replacement and return the mutation with the diff of
/// the edit.
///
/// The file is read, matched and replaced under its mutation lock, so a
/// second edit of the same file in this process applies to the result of the
/// first instead of failing its hash check.
pub(crate) fn edit_text(
    path: &Path,
    display_path: &str,
    edits: &[EditSpec],
) -> Result<(WorkspaceMutation, String), HarnessError> {
    let _guard = lock_file_mutation(path);
    let current = read_text(path)?;
    let planned = plan_edit_text(&current, edits, display_path)?;
    let expected = ContentHash::from_bytes(current.as_bytes());
    let mut mutation = write_text_checked_locked(path, Some(&expected), &planned.content)?;
    mutation.replacements = planned.replacements;
    Ok((mutation, planned.diff))
}

/// Plan the replacements of an `edit_file` call on `current` without writing
/// them: exact match first, then prime-agent's normalized match, then by
/// whole lines with indentation set aside, or by hashline anchors (see
/// [`crate::edit_diff`]). `display_path` only names the file in an error.
pub(crate) fn plan_edit_text(
    current: &str,
    edits: &[EditSpec],
    display_path: &str,
) -> Result<PlannedEdit, HarnessError> {
    plan_edits(current, edits, display_path)
}

pub(crate) fn redact_text(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|segment| {
            let lower = segment.to_ascii_lowercase();
            let sensitive = [
                "secret",
                "token",
                "password",
                "api_key",
                "credential",
                "authorization",
            ]
            .iter()
            .any(|needle| lower.contains(needle));
            if !sensitive {
                return segment.to_owned();
            }
            let ending = if segment.ends_with('\n') { "\n" } else { "" };
            let body = segment.strip_suffix('\n').unwrap_or(segment);
            if let Some(index) = body.find(['=', ':']) {
                format!("{}=[REDACTED]{ending}", &body[..index])
            } else {
                format!("[REDACTED]{ending}")
            }
        })
        .collect()
}

/// Classify a failure to open or inspect a directory during the walk.
///
/// A path this process is not allowed to read is a **read failure** and must say
/// so, naming the path: reporting `workspace_escape` for it tells an operator the
/// walk tried to leave the workspace when it only could not look inside. The
/// escape code stays reserved for a walk that really left the root, and for any
/// other walker failure whose nature is not established here.
pub(crate) fn deny_read_error(
    path: &Path,
    error: &(dyn std::fmt::Display + 'static),
) -> HarnessError {
    HarnessError::new(
        ErrorCode::StorageOpenFailed,
        format!(
            "cannot read workspace directory {}: {error}",
            path.display()
        ),
    )
}

pub(crate) fn entry_failure(path: &Path, error: &std::io::Error) -> HarnessError {
    if error.kind() == ErrorKind::PermissionDenied {
        return deny_read_error(path, error);
    }
    HarnessError::new(
        ErrorCode::WorkspaceEscape,
        format!("cannot inspect workspace walk entry: {error}"),
    )
}

/// How long the fingerprint walk may take before the workspace counts as too
/// large to observe file by file.
const FINGERPRINT_WALK_DEADLINE: Duration = Duration::from_secs(3);

/// Roots whose walk ran past the bound or the deadline in this process.
fn too_large_roots() -> &'static Mutex<HashSet<PathBuf>> {
    static ROOTS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    ROOTS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn too_large(root: &Path) -> bool {
    too_large_roots()
        .lock()
        .is_ok_and(|roots| roots.contains(root))
}

fn remember_too_large(root: &Path) {
    if let Ok(mut roots) = too_large_roots().lock() {
        roots.insert(root.to_owned());
    }
}

/// Content hashes of workspace files, by path, size and modification time.
///
/// Every tool call fingerprints the workspace, and reading every file every time
/// made a `read_file` cost most of a second on a real repository. A file whose
/// size and modification time are unchanged is not read again - the same test
/// `git` uses for its index. A file modified in the last two seconds is always
/// read, because a second write inside one timestamp tick keeps the same time
/// ("racily clean" in git's terms).
type HashCache = HashMap<PathBuf, (u64, SystemTime, ContentHash)>;

fn hash_cache() -> &'static Mutex<HashCache> {
    static CACHE: OnceLock<Mutex<HashCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

const RACY_WINDOW: Duration = Duration::from_secs(2);

/// Entries the hash cache may hold beyond the files of the walk at hand.
const HASH_CACHE_SLACK: usize = 64 * 1024;

/// Keep the hash cache from growing for as long as the host runs.
///
/// Files are deleted and renamed, and a long session over several worktrees
/// visits many roots; nothing ever removed their entries. Once the cache holds
/// far more than the walk being hashed, it keeps only that walk's files: a hash
/// dropped this way is a cache miss later, never a wrong answer.
fn bound_hash_cache(cache: &mut HashCache, files: &[WalkFile]) {
    if cache.len()
        <= files
            .len()
            .saturating_mul(2)
            .saturating_add(HASH_CACHE_SLACK)
    {
        return;
    }
    let current = files
        .iter()
        .map(|file| file.absolute.as_path())
        .collect::<HashSet<_>>();
    cache.retain(|path, _| current.contains(path.as_path()));
}

/// Hash every file, reusing cached hashes and reading the rest on all cores.
fn hash_files(files: &[WalkFile]) -> Result<Vec<Option<ContentHash>>, HarnessError> {
    let now = SystemTime::now();
    let mut hashes: Vec<Option<Option<ContentHash>>> = vec![None; files.len()];
    let mut misses = Vec::new();
    if let Ok(cache) = hash_cache().lock() {
        for (index, file) in files.iter().enumerate() {
            let settled = file.modified.filter(|modified| {
                now.duration_since(*modified)
                    .is_ok_and(|age| age >= RACY_WINDOW)
            });
            match (settled, cache.get(&file.absolute)) {
                (Some(modified), Some((len, cached_modified, hash)))
                    if *len == file.len && *cached_modified == modified =>
                {
                    hashes[index] = Some(Some(hash.clone()));
                }
                _ => misses.push(index),
            }
        }
    } else {
        misses.extend(0..files.len());
    }
    let workers = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .clamp(1, 16)
        .min(misses.len().max(1));
    let chunk = misses.len().div_ceil(workers).max(1);
    let computed = std::thread::scope(|scope| {
        let handles = misses
            .chunks(chunk)
            .map(|indexes| {
                scope.spawn(move || {
                    indexes
                        .iter()
                        .map(|index| (*index, hash_file(&files[*index].absolute)))
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect::<Vec<_>>()
    });
    let mut cache = hash_cache().lock().ok();
    if let Some(cache) = cache.as_mut() {
        bound_hash_cache(cache, files);
    }
    for (index, hash) in computed {
        let hash = hash?;
        let file = &files[index];
        if let (Some(cache), Some(hash), Some(modified)) = (cache.as_mut(), &hash, file.modified) {
            let settled = now
                .duration_since(modified)
                .is_ok_and(|age| age >= RACY_WINDOW);
            if settled {
                cache.insert(file.absolute.clone(), (file.len, modified, hash.clone()));
            }
        }
        hashes[index] = Some(hash);
    }
    Ok(hashes.into_iter().map(Option::flatten).collect())
}

/// Flush a workspace file's temporary copy before it is renamed over (or
/// linked as) the real file.
///
/// Without the flush, a power loss soon after a write can leave the user's
/// file empty or torn: the rename may reach the disk before the bytes do. It
/// costs about 70 ms per write on a Windows disk with antivirus scanning, so
/// `HA_WRITE_SYNC=off` skips it for a user who would rather have the speed and
/// accepts that risk; a crash of ha itself loses nothing either way, since the
/// operating system still holds the bytes. Only the user's files are
/// affected: the journal and artifacts keep their own flushes.
fn flush_workspace_file(file: &File) -> std::io::Result<()> {
    if std::env::var_os("HA_WRITE_SYNC").is_some_and(|value| value == "off") {
        return Ok(());
    }
    file.sync_all()
}

fn create_text_atomically(path: &Path, content: &str) -> Result<(), HarnessError> {
    let parent = path.parent().ok_or_else(|| {
        HarnessError::new(
            ErrorCode::WorkspaceEscape,
            "write path has no parent directory",
        )
    })?;
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| HarnessError::new(ErrorCode::StorageWriteFailed, "system clock is invalid"))?
        .as_nanos();
    let mut temporary = None;
    for attempt in 0_u8..32 {
        let candidate = parent.join(format!(
            ".harness-write-{started}-{}-{attempt}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                if let Err(error) = file
                    .write_all(content.as_bytes())
                    .and_then(|()| flush_workspace_file(&file))
                {
                    drop(file);
                    let _ = fs::remove_file(&candidate);
                    return Err(HarnessError::new(
                        ErrorCode::StorageWriteFailed,
                        format!("cannot write new workspace file: {error}"),
                    ));
                }
                temporary = Some(candidate);
                break;
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(HarnessError::new(
                    ErrorCode::StorageWriteFailed,
                    format!("cannot create workspace temporary file: {error}"),
                ));
            }
        }
    }
    let temporary = temporary.ok_or_else(|| {
        HarnessError::new(
            ErrorCode::StorageWriteFailed,
            "cannot allocate a unique workspace temporary file",
        )
    })?;
    match fs::hard_link(&temporary, path) {
        Ok(()) => {
            fs::remove_file(&temporary).map_err(|error| {
                HarnessError::new(
                    ErrorCode::ProcessOutcomeUnknown,
                    format!("new file was created but its temporary link remains: {error}"),
                )
            })?;
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&temporary);
            Err(HarnessError::new(
                ErrorCode::StaleWorkspace,
                "target file appeared before the create operation",
            ))
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(HarnessError::new(
                ErrorCode::StorageWriteFailed,
                format!("cannot publish new workspace file: {error}"),
            ))
        }
    }
}

fn write_text_atomically(path: &Path, replacement: &str) -> Result<(), HarnessError> {
    let parent = path.parent().ok_or_else(|| {
        HarnessError::new(
            ErrorCode::WorkspaceEscape,
            "patch path has no parent directory",
        )
    })?;
    // A rename replaces the target with the temporary file, so the temporary
    // file must carry the target's own permissions: without this a private
    // (0600) file would become group/world readable after a patch on Unix.
    #[cfg(unix)]
    let target_permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| HarnessError::new(ErrorCode::StorageWriteFailed, "system clock is invalid"))?
        .as_nanos();
    let mut temporary = None;
    for attempt in 0_u8..32 {
        let candidate = parent.join(format!(
            ".harness-p3-{started}-{}-{attempt}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                file.write_all(replacement.as_bytes()).map_err(|error| {
                    HarnessError::new(
                        ErrorCode::StorageWriteFailed,
                        format!("cannot write patch temporary file: {error}"),
                    )
                })?;
                flush_workspace_file(&file).map_err(|error| {
                    HarnessError::new(
                        ErrorCode::StorageWriteFailed,
                        format!("cannot flush patch temporary file: {error}"),
                    )
                })?;
                #[cfg(unix)]
                if let Some(permissions) = target_permissions {
                    fs::set_permissions(&candidate, permissions).map_err(|error| {
                        HarnessError::new(
                            ErrorCode::StorageWriteFailed,
                            format!("cannot preserve patch target permissions: {error}"),
                        )
                    })?;
                }
                temporary = Some(candidate);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(HarnessError::new(
                    ErrorCode::StorageWriteFailed,
                    format!("cannot create patch temporary file: {error}"),
                ));
            }
        }
    }
    let temporary = temporary.ok_or_else(|| {
        HarnessError::new(
            ErrorCode::StorageWriteFailed,
            "cannot allocate a unique patch temporary file",
        )
    })?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(HarnessError::new(
                ErrorCode::StorageWriteFailed,
                format!("cannot replace workspace file atomically: {error}"),
            ))
        }
    }
}

fn decode_utf8(bytes: &[u8]) -> Result<String, HarnessError> {
    if bytes.contains(&0) {
        return Err(HarnessError::new(
            ErrorCode::BinaryContentDenied,
            "workspace file contains NUL and is treated as binary",
        ));
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        HarnessError::new(
            ErrorCode::UnsupportedTextEncoding,
            "workspace file is not UTF-8",
        )
    })
}

/// Hash one workspace file.
///
/// A lock violation means another process holds a byte range of the file — the
/// app's own store does exactly that for `-shm`/`-wal` when it lives inside the
/// workspace. It is reported as `Ok(None)` so the fingerprint can record the
/// path as present-but-unreadable instead of failing the whole turn. Every
/// other failure is a read failure with its own typed code: `workspace_escape`
/// would send an operator looking for a path bug that does not exist.
fn hash_file(path: &Path) -> Result<Option<ContentHash>, HarnessError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if is_lock_violation(&error) => return Ok(None),
        Err(error) => return Err(hash_failure(path, &error)),
    };
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 32 * 1024];
    loop {
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if is_lock_violation(&error) => return Ok(None),
            Err(error) => return Err(hash_failure(path, &error)),
        };
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    let digest = hash.finalize();
    let mut text = String::with_capacity(digest.len().saturating_mul(2).saturating_add(7));
    text.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    ContentHash::parse(text).map(Some)
}

fn hash_failure(path: &Path, error: &std::io::Error) -> HarnessError {
    HarnessError::new(
        ErrorCode::StorageOpenFailed,
        format!("cannot hash workspace file {}: {error}", path.display()),
    )
}

/// Whether an I/O failure is another process's byte-range lock rather than an
/// access or path problem. Windows reports `ERROR_SHARING_VIOLATION` (32) and
/// `ERROR_LOCK_VIOLATION` (33); POSIX advisory locks never block a plain read.
fn is_lock_violation(error: &std::io::Error) -> bool {
    #[cfg(windows)]
    {
        matches!(error.raw_os_error(), Some(32 | 33))
    }
    #[cfg(not(windows))]
    {
        let _ = error;
        false
    }
}

fn git_output<const N: usize>(root: &Path, arguments: [&str; N]) -> Option<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn canonicalize_git_path(root: &Path, value: &str) -> Option<String> {
    let candidate = Path::new(value);
    let candidate = if candidate.is_absolute() {
        candidate.to_owned()
    } else {
        root.join(candidate)
    };
    fs::canonicalize(candidate)
        .ok()
        .and_then(|path| canonical_path_text(&path).ok())
}

fn canonical_path_text(path: &Path) -> Result<String, HarnessError> {
    path.to_str().map(ToOwned::to_owned).ok_or_else(|| {
        HarnessError::new(
            ErrorCode::WorkspaceEscape,
            "workspace path is not valid Unicode",
        )
    })
}

fn is_link_or_reparse(path: &Path) -> Result<bool, HarnessError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        HarnessError::new(
            ErrorCode::WorkspaceEscape,
            format!("cannot inspect workspace link metadata: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        Ok(metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
    }
    #[cfg(not(windows))]
    {
        Ok(false)
    }
}

pub fn is_sensitive_workspace_path(path: &Path) -> bool {
    is_sensitive_relative(path)
}

fn is_sensitive_relative(path: &Path) -> bool {
    let mut last = None;
    for component in path.components() {
        let Component::Normal(value) = component else {
            return true;
        };
        let value = value.to_string_lossy();
        if crate::walk::sensitive_component(&value) {
            return true;
        }
        last = Some(value.to_ascii_lowercase());
    }
    last.is_some_and(|name| {
        matches!(
            name.rsplit_once('.').map(|(_, extension)| extension),
            Some("pem" | "key" | "p12" | "pfx" | "clixml")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The branch read from the repository files is what `git rev-parse
    /// --abbrev-ref HEAD` says: nothing before the first commit, the branch
    /// name on a branch and `HEAD` when detached.
    #[test]
    fn the_branch_read_from_files_matches_git() {
        let root = std::env::temp_dir().join(format!("br-{}", harness_types::InputId::generate()));
        std::fs::create_dir_all(&root).expect("root");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&root)
                .output()
                .expect("git");
            assert!(status.status.success(), "git {args:?}");
        };
        let asked = || git_output(&root, ["rev-parse", "--abbrev-ref", "HEAD"]);
        git(&["init", "-q", "-b", "feature/x"]);
        assert_eq!(git_branch(&root), None, "no commit yet");
        std::fs::write(root.join("a.txt"), "a").expect("file");
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "one"]);
        assert_eq!(git_branch(&root).as_deref(), Some("feature/x"));
        assert_eq!(git_branch(&root), asked());
        git(&["checkout", "-q", "--detach"]);
        assert_eq!(git_branch(&root).as_deref(), Some("HEAD"));
        assert_eq!(git_branch(&root), asked());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A folder that is not a Git repository still keeps out what its
    /// `.gitignore` names; a walk past its deadline counts as too large; and a
    /// root known to be too large is fingerprinted without being walked.
    #[test]
    fn a_large_workspace_is_fingerprinted_without_a_walk() {
        let root = std::env::temp_dir().join(format!("ws-{}", harness_types::InputId::generate()));
        std::fs::create_dir_all(root.join("node_modules").join("pkg")).expect("root");
        std::fs::write(root.join(".gitignore"), "node_modules/\n").expect("ignore");
        std::fs::write(root.join("node_modules").join("pkg").join("a.js"), "x").expect("dep");
        std::fs::write(root.join("main.py"), "print(1)").expect("file");
        let walked = crate::walk::walk_files_within(&root, 100, None).expect("walk");
        let names = walked
            .files
            .iter()
            .map(|file| file.relative.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"main.py"), "{names:?}");
        assert!(
            !names.iter().any(|name| name.starts_with("node_modules")),
            "outside Git too, .gitignore keeps node_modules out: {names:?}"
        );

        let late = crate::walk::walk_files_within(
            &root,
            100,
            Instant::now().checked_sub(Duration::from_secs(1)),
        )
        .expect("a walk past its deadline returns what it has");
        assert!(!late.complete, "past the deadline the walk is incomplete");

        let walked_print = workspace_fingerprint(&root, "not_git").expect("walked");
        remember_too_large(&root);
        let started = Instant::now();
        let unwalked = workspace_fingerprint(&root, "not_git").expect("not walked");
        assert!(started.elapsed() < FINGERPRINT_WALK_DEADLINE);
        assert_ne!(
            walked_print, unwalked,
            "the degraded fingerprint is its own"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A file that is not there says so and names it, instead of the OS text
    /// "The system cannot find the path specified. (os error 3)".
    #[test]
    fn a_missing_file_is_reported_as_not_found_by_name() {
        let root = std::env::temp_dir().join(format!("ws-{}", harness_types::InputId::generate()));
        std::fs::create_dir_all(&root).expect("root");
        let missing = root.join(".claude-plugin").join("plugin.json");
        let error = read_text(&missing).expect_err("missing");
        assert_eq!(error.code(), ErrorCode::InvalidPayload);
        assert_eq!(
            error.message(),
            format!("File not found: {}", missing.display())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Models name files by the absolute path they were shown; inside the
    /// workspace that is the relative path, outside it stays refused.
    #[test]
    fn an_absolute_path_inside_the_workspace_resolves_and_outside_is_refused() {
        let root = std::env::temp_dir().join(format!("ws-{}", harness_types::InputId::generate()));
        std::fs::create_dir_all(root.join("docs")).expect("root");
        std::fs::write(root.join("docs").join("a.md"), "x").expect("file");
        let root = std::fs::canonicalize(&root).expect("canonical");
        let plain = root
            .to_str()
            .expect("utf-8")
            .trim_start_matches(r"\\?\")
            .to_owned();
        let inside = format!("{plain}\\docs\\a.md");
        assert_eq!(
            resolve_relative(&root, &inside, false).expect("inside resolves"),
            root.join("docs").join("a.md")
        );
        assert_eq!(
            resolve_relative(&root, &plain, true).expect("the root itself"),
            root
        );
        let outside = std::env::temp_dir().join("elsewhere.md");
        assert!(resolve_relative(&root, outside.to_str().expect("utf-8"), false).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn scratch_file(content: &str) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("scratch directory");
        let path = directory.path().join("f.txt");
        fs::write(&path, content).expect("scratch file");
        (directory, path)
    }

    #[test]
    fn a_read_that_stops_before_the_end_says_how_to_continue() {
        let (_directory, path) = scratch_file("a\nb\nc\nd\n");
        let partial = read_file_range(&path, 1, 2).expect("range");
        assert!(partial.truncated);
        assert_eq!(
            partial.text,
            "2: b\n3: c\n\n[Showing lines 2-3 of 4. Use offset=3 to continue.]"
        );

        let whole = read_file_range(&path, 0, 10).expect("whole");
        assert!(!whole.truncated);
        assert_eq!(whole.text, "1: a\n2: b\n3: c\n4: d");
    }

    #[test]
    fn a_read_over_the_byte_limit_names_the_limit_and_the_next_offset() {
        let line = "x".repeat(1000);
        let (_directory, path) = scratch_file(&format!("{line}\n").repeat(100));
        let output = read_file_range(&path, 0, 100).expect("range");
        assert!(output.truncated);
        // 50 numbered lines of 1003-1005 bytes fit in 50 KiB; the 51st does not.
        assert!(
            output.text.ends_with(
                "[Showing lines 1-50 of 100 (50.0KB limit). Use offset=50 to continue.]"
            ),
            "{}",
            &output.text[output.text.len() - 120..]
        );
    }

    #[test]
    fn a_single_line_over_the_byte_limit_shows_its_beginning() {
        let (_directory, path) = scratch_file(&format!("{}\nnext\n", "y".repeat(60 * 1024)));
        let output = read_file_range(&path, 0, 2).expect("range");
        assert!(output.truncated);
        assert!(output.text.starts_with("1: yyy"));
        assert!(
            output.text.ends_with(
                "[Line 1 is 60.0KB, exceeds 50.0KB limit; showing its first 50.0KB. Use offset=1 to continue after it.]"
            ),
            "{}",
            &output.text[output.text.len() - 150..]
        );
    }

    #[test]
    fn a_long_search_match_is_cut_with_a_visible_marker() {
        let directory = tempfile::tempdir().expect("scratch directory");
        fs::write(
            directory.path().join("long.txt"),
            format!("needle {}\n", "z".repeat(900)),
        )
        .expect("file");
        let root = fs::canonicalize(directory.path()).expect("root");
        let output =
            search_text(&root, "needle", None, false, false, None, 0, false).expect("search");
        assert_eq!(output.matches.len(), 1);
        let preview = &output.matches[0].preview;
        assert!(preview.ends_with("... [truncated]"), "{preview}");
        assert_eq!(
            preview.chars().count(),
            crate::truncate::GREP_MAX_LINE_LENGTH + 15
        );
    }

    fn spec(old: &str, new: &str) -> EditSpec {
        EditSpec {
            old_string: old.to_owned(),
            new_string: new.to_owned(),
            replace_all: false,
            start: None,
            end: None,
        }
    }

    #[test]
    fn edits_of_a_crlf_file_keep_its_line_endings_and_return_the_diff() {
        let (_directory, path) = scratch_file("one\r\ntwo\r\n");
        let (mutation, diff) = edit_text(&path, "f.txt", &[spec("two\n", "2\n")]).expect("edit");
        assert_eq!(mutation.replacements, 1);
        assert_eq!(fs::read_to_string(&path).expect("read"), "one\r\n2\r\n");
        assert_eq!(diff, [" 1 one", "-2 two", "+2 2"].join("\n"));
    }

    /// Two writers in this process editing one file both land: the second
    /// waits for the first and applies to its result instead of failing the
    /// hash check it would otherwise race.
    #[test]
    fn concurrent_edits_of_one_file_are_serialized() {
        let numbered = |prefix: &str| {
            (0..16).fold(String::new(), |mut text, n| {
                use std::fmt::Write as _;
                let _ = writeln!(text, "{prefix} {n}");
                text
            })
        };
        let (_directory, path) = scratch_file(&numbered("line"));
        std::thread::scope(|scope| {
            for n in 0..16 {
                let path = &path;
                scope.spawn(move || {
                    edit_text(
                        path,
                        "f.txt",
                        &[spec(&format!("line {n}\n"), &format!("edited {n}\n"))],
                    )
                    .expect("every edit lands");
                });
            }
        });
        assert_eq!(fs::read_to_string(&path).expect("read"), numbered("edited"));
    }

    #[test]
    fn review_bounded_reader_rejects_invalid_utf8_inside_prefix() {
        let path = std::env::temp_dir().join(format!("{}.txt", harness_types::InputId::generate()));
        let mut bytes = vec![b'a'; MAX_OUTPUT_BYTES + 10];
        bytes[1] = 0xff;
        fs::write(&path, bytes).unwrap();
        let result = read_text_output(&path);
        fs::remove_file(path).unwrap();
        assert!(
            matches!(result, Err(ref error) if error.code() == ErrorCode::UnsupportedTextEncoding)
        );
    }

    #[test]
    fn review_bounded_reader_preserves_valid_unicode_prefix() {
        let path = std::env::temp_dir().join(format!("{}.txt", harness_types::InputId::generate()));
        let prefix = "a".repeat(MAX_OUTPUT_BYTES - 1);
        fs::write(&path, format!("{prefix}€more")).unwrap();
        let result = read_text_output(&path);
        fs::remove_file(path).unwrap();
        let output = result.unwrap();
        assert!(output.truncated);
        assert_eq!(output.text, prefix);
    }

    #[test]
    fn review_permission_denial_maps_to_a_read_failure_with_the_path() {
        let path = Path::new("locked");
        let io_error = std::io::Error::from(ErrorKind::PermissionDenied);
        let error = entry_failure(path, &io_error);

        assert_eq!(error.code(), ErrorCode::StorageOpenFailed);
        assert!(error.to_string().contains("locked"));
    }

    /// Restores the ACL of a denied directory when the test ends, so a failing
    /// assertion cannot leave an unreadable temporary tree behind.
    struct DeniedRead {
        directory: PathBuf,
        identity: String,
    }

    impl DeniedRead {
        fn apply(directory: &Path) -> Self {
            let identity = match std::env::var("USERDOMAIN") {
                Ok(domain) if !domain.is_empty() => format!(
                    "{domain}\\{}",
                    std::env::var("USERNAME").expect("USERNAME is set")
                ),
                _ => std::env::var("USERNAME").expect("USERNAME is set"),
            };
            let output = std::process::Command::new("icacls")
                .arg(directory)
                .arg("/deny")
                .arg(format!("{identity}:(OI)(CI)(R)"))
                .output()
                .expect("icacls runs");
            assert!(
                output.status.success(),
                "icacls could not deny read on {}: {}",
                directory.display(),
                String::from_utf8_lossy(&output.stderr)
            );
            Self {
                directory: directory.to_owned(),
                identity,
            }
        }
    }

    impl Drop for DeniedRead {
        fn drop(&mut self) {
            let _ = std::process::Command::new("icacls")
                .arg(&self.directory)
                .arg("/remove:d")
                .arg(&self.identity)
                .output();
        }
    }

    /// A directory the process may not read is a read failure, not an escape
    /// attempt: an operator reading the error must not be told the walk left
    /// the workspace when it merely could not open a directory inside it.
    #[test]
    fn review_unreadable_directory_is_reported_as_a_read_failure_with_the_path() {
        // The denial is applied with `icacls` and the account name comes from
        // `USERNAME`, so this case is Windows-only by construction. It is skipped
        // rather than failed elsewhere: a skipped test that says why is more honest
        // than one that panics on a platform the fixture cannot work on.
        if !cfg!(windows) {
            eprintln!(
                "read-denial fixture needs icacls and USERNAME; skipping on {}",
                std::env::consts::OS
            );
            return;
        }
        let root =
            std::env::temp_dir().join(format!("walk-{}", harness_types::InputId::generate()));
        let locked = root.join("locked");
        fs::create_dir_all(&locked).unwrap();
        fs::write(root.join("visible.txt"), "readable").unwrap();
        fs::write(locked.join("hidden.txt"), "inside a locked directory").unwrap();

        let denial = DeniedRead::apply(&locked);
        let denied = match fs::read_dir(&locked)
            .and_then(|mut entries| entries.next().transpose().map(|_| ()))
        {
            Err(error) if error.kind() == ErrorKind::PermissionDenied => true,
            Ok(()) => false,
            Err(error) => panic!(
                "ACL probe failed with an unexpected error for {}: {error}",
                locked.display()
            ),
        };
        if !denied {
            drop(denial);
            fs::remove_dir_all(&root).unwrap();
            eprintln!(
                "read-denial fixture skipped: the current Windows token can still list {} after icacls",
                locked.display()
            );
            return;
        }
        let result = crate::walk::walk_files_within(&root, 1000, None);
        let error = result.expect_err("an unreadable directory must not be skipped silently");
        drop(denial);

        assert_eq!(
            error.code(),
            ErrorCode::StorageOpenFailed,
            "a permission failure is not a workspace escape: {error}"
        );
        assert!(
            error.to_string().contains("locked"),
            "the failure must name the directory that could not be read: {error}"
        );

        fs::remove_dir_all(&root).unwrap();
    }
}
