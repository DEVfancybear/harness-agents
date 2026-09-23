use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use globset::Glob;
use harness_store_sqlite::ProjectRegistrationRecord;
use harness_types::{ContentHash, ErrorCode, HarnessError, ProjectId, WorkspaceObservation};
use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::contracts::{SearchMatch, observation};

pub(crate) const MAX_TEXT_FILE_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_WALK_ENTRIES: usize = 4096;
const MAX_SEARCH_MATCHES: usize = 512;

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
    let git_common_dir = git_output(&canonical, ["rev-parse", "--git-common-dir"])
        .and_then(|value| canonicalize_git_path(&canonical, &value));
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

pub(crate) fn inspect_workspace(
    root: &Path,
    project_id: ProjectId,
) -> Result<WorkspaceDescriptor, HarnessError> {
    let identity = inspect_identity(root)?;
    let git_head =
        git_output(&identity.root, ["rev-parse", "HEAD"]).unwrap_or_else(|| "not_git".to_owned());
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

pub(crate) fn workspace_fingerprint(
    root: &Path,
    git_head: &str,
) -> Result<ContentHash, HarnessError> {
    let mut entries = Vec::new();
    for item in walk_files(root)? {
        match hash_file(&item.absolute)? {
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
    let status = git_output(root, ["status", "--porcelain=v1", "--untracked-files=all"])
        .unwrap_or_else(|| "not_git".to_owned());
    ContentHash::from_canonical_json(&json!({
        "git_head": git_head,
        "git_status": status,
        "files": entries,
    }))
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

pub(crate) fn list_files(
    root: &Path,
    requested: Option<&str>,
) -> Result<(Vec<String>, bool), HarnessError> {
    let base = match requested {
        Some(path) => resolve_relative(root, path, true)?,
        None => root.to_owned(),
    };
    if !base.is_dir() {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "list_files path must be a directory",
        ));
    }
    let files = walk_files(&base)?;
    let mut paths = Vec::new();
    let mut truncated = false;
    for file in files {
        let relative = file.absolute.strip_prefix(root).map_err(|_| {
            HarnessError::new(
                ErrorCode::WorkspaceEscape,
                "listed path escaped workspace root",
            )
        })?;
        if paths.len() == MAX_SEARCH_MATCHES {
            truncated = true;
            break;
        }
        paths.push(relative_text(relative));
    }
    Ok((paths, truncated))
}

pub(crate) fn glob_files(
    root: &Path,
    requested: Option<&str>,
    pattern: &str,
) -> Result<(Vec<String>, bool), HarnessError> {
    let matcher = Glob::new(pattern)
        .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error.to_string()))?
        .compile_matcher();
    let base = match requested {
        Some(path) => resolve_relative(root, path, true)?,
        None => root.to_owned(),
    };
    if !base.is_dir() {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "glob path must be a directory",
        ));
    }
    let files = walk_files(&base)?;
    let mut paths = Vec::new();
    for file in files {
        if matcher.is_match(Path::new(&file.relative)) {
            let relative = file.absolute.strip_prefix(root).map_err(|_| {
                HarnessError::new(ErrorCode::WorkspaceEscape, "glob path escaped workspace")
            })?;
            paths.push(relative_text(relative));
        }
    }
    Ok((paths, false))
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

pub(crate) fn search_text(
    root: &Path,
    query: &str,
    requested: Option<&str>,
    use_regex: bool,
    case_insensitive: bool,
    glob: Option<&str>,
    context_lines: u32,
) -> Result<SearchOutput, HarnessError> {
    validate_search(query, use_regex, case_insensitive)?;
    let pattern = if use_regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    let matcher = RegexBuilder::new(&pattern)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error.to_string()))?;
    let file_matcher = glob
        .map(|pattern| {
            Glob::new(pattern)
                .map(|glob| glob.compile_matcher())
                .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error.to_string()))
        })
        .transpose()?;
    let base = match requested {
        Some(path) => resolve_relative(root, path, true)?,
        None => root.to_owned(),
    };
    if !base.is_dir() {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "search_text path must be a directory",
        ));
    }
    let mut found_matches = Vec::new();
    let mut truncated = false;
    let mut output_bytes = 0_usize;
    for file in walk_files(&base)? {
        if file_matcher
            .as_ref()
            .is_some_and(|matcher| !matcher.is_match(Path::new(&file.relative)))
        {
            continue;
        }
        let Ok(text) = read_text(&file.absolute) else {
            continue;
        };
        let lines = text.lines().collect::<Vec<_>>();
        for (line_index, line) in lines.iter().enumerate() {
            for found in matcher.find_iter(line) {
                if found_matches.len() == MAX_SEARCH_MATCHES {
                    truncated = true;
                    break;
                }
                let start = line_index.saturating_sub(usize::try_from(context_lines).unwrap_or(0));
                let end = line_index
                    .saturating_add(usize::try_from(context_lines).unwrap_or(0))
                    .saturating_add(1)
                    .min(lines.len());
                let context = lines[start..end]
                    .iter()
                    .enumerate()
                    .filter(|(offset, _)| start + *offset != line_index)
                    .map(|(_, context_line)| truncate_text(&redact_text(context_line), 160))
                    .collect::<Vec<_>>();
                let preview = truncate_text(&redact_text(line), 240);
                let relative = file.absolute.strip_prefix(root).map_err(|_| {
                    HarnessError::new(
                        ErrorCode::WorkspaceEscape,
                        "searched path escaped workspace root",
                    )
                })?;
                let path = relative_text(relative);
                let context_bytes = context.iter().map(String::len).sum::<usize>();
                if output_bytes
                    .saturating_add(preview.len())
                    .saturating_add(context_bytes)
                    .saturating_add(path.len())
                    > MAX_OUTPUT_BYTES
                {
                    truncated = true;
                    break;
                }
                output_bytes = output_bytes
                    .saturating_add(preview.len())
                    .saturating_add(context_bytes)
                    .saturating_add(path.len());
                found_matches.push(SearchMatch {
                    path,
                    line: u64::try_from(line_index.saturating_add(1)).unwrap_or(u64::MAX),
                    column: u64::try_from(found.start().saturating_add(1)).unwrap_or(u64::MAX),
                    preview,
                    context,
                });
            }
            if truncated {
                break;
            }
        }
        if truncated {
            break;
        }
    }
    Ok(SearchOutput {
        matches: found_matches,
        truncated,
    })
}

pub(crate) fn read_file_range(
    path: &Path,
    offset: u64,
    limit: u32,
) -> Result<TextOutput, HarnessError> {
    use std::fmt::Write as _;

    let text = read_text(path)?;
    let lines = text.lines().collect::<Vec<_>>();
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(lines.len());
    let count = usize::try_from(limit).unwrap_or(usize::MAX);
    let end = start.saturating_add(count).min(lines.len());
    let mut numbered = String::new();
    for (index, line) in lines[start..end].iter().enumerate() {
        if numbered.len() >= MAX_OUTPUT_BYTES {
            return Ok(TextOutput {
                text: numbered,
                truncated: true,
            });
        }
        let number = start.saturating_add(index).saturating_add(1);
        let _ = write!(numbered, "{number}: {}", redact_text(line));
        if index + start + 1 < end {
            numbered.push('\n');
        }
    }
    Ok(TextOutput {
        text: truncate_text(&numbered, MAX_OUTPUT_BYTES),
        truncated: end < lines.len() || numbered.len() > MAX_OUTPUT_BYTES,
    })
}

pub(crate) fn apply_text_patch(
    path: &Path,
    expected_hash: &ContentHash,
    replacement: &str,
) -> Result<WorkspaceMutation, HarnessError> {
    write_text_checked(path, Some(expected_hash), replacement)
}

pub(crate) fn write_text_checked(
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
        // Close the ordinary stale-edit case immediately before the atomic
        // replacement. A changed or unreadable preimage is never overwritten.
        let current = read_text(path)?;
        let current_hash = ContentHash::from_bytes(current.as_bytes());
        if Some(&current_hash) != expected_hash {
            return Err(HarnessError::new(
                ErrorCode::StaleWorkspace,
                "expected_hash changed immediately before file replacement",
            ));
        }
        write_text_atomically(path, replacement)?;
    } else {
        create_text_atomically(path, replacement)?;
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

pub(crate) fn edit_text(
    path: &Path,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<WorkspaceMutation, HarnessError> {
    let current = read_text(path)?;
    let (replacement, replacements) =
        plan_edit_text(&current, old_string, new_string, replace_all)?;
    let expected = ContentHash::from_bytes(current.as_bytes());
    let mut mutation = write_text_checked(path, Some(&expected), &replacement)?;
    mutation.replacements = replacements;
    Ok(mutation)
}

pub(crate) fn plan_edit_text(
    current: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<(String, u64), HarnessError> {
    if old_string.is_empty() {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "edit_file old_string must not be empty",
        ));
    }
    let count = current.matches(old_string).count();
    if count == 0 {
        return Err(HarnessError::new(
            ErrorCode::EditNotFound,
            "edit_file old_string was not found",
        ));
    }
    if !replace_all && count != 1 {
        return Err(HarnessError::new(
            ErrorCode::EditAmbiguous,
            format!("edit_file old_string matched {count} times"),
        ));
    }
    let replacement = if replace_all {
        current.replace(old_string, new_string)
    } else {
        current.replacen(old_string, new_string, 1)
    };
    Ok((replacement, u64::try_from(count).unwrap_or(u64::MAX)))
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
fn deny_read_error(path: &Path, error: &(dyn std::fmt::Display + 'static)) -> HarnessError {
    HarnessError::new(
        ErrorCode::StorageOpenFailed,
        format!(
            "cannot read workspace directory {}: {error}",
            path.display()
        ),
    )
}

fn walk_failure(path: &Path, error: &ignore::Error) -> HarnessError {
    if error
        .io_error()
        .is_some_and(|io_error| io_error.kind() == ErrorKind::PermissionDenied)
    {
        return deny_read_error(path, error);
    }
    HarnessError::new(
        ErrorCode::WorkspaceEscape,
        format!("workspace walk failed: {error}"),
    )
}

fn entry_failure(path: &Path, error: &std::io::Error) -> HarnessError {
    if error.kind() == ErrorKind::PermissionDenied {
        return deny_read_error(path, error);
    }
    HarnessError::new(
        ErrorCode::WorkspaceEscape,
        format!("cannot inspect workspace walk entry: {error}"),
    )
}

fn walk_files(root: &Path) -> Result<Vec<WalkFile>, HarnessError> {
    let mut files = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .follow_links(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .build();
    for item in walker {
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                // The walker wraps the failing path in its own error types, so
                // recover the location instead of leaving the message anonymous.
                let path = match &error {
                    ignore::Error::WithPath { path, .. } => path.clone(),
                    _ => root.to_owned(),
                };
                return Err(walk_failure(&path, &error));
            }
        };
        let path = item.path();
        if path == root {
            continue;
        }
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => return Err(entry_failure(path, &error)),
        };
        if is_link_or_reparse(path)? || !metadata.is_file() {
            continue;
        }
        let relative = path.strip_prefix(root).map_err(|_| {
            HarnessError::new(ErrorCode::WorkspaceEscape, "workspace walk escaped root")
        })?;
        if is_sensitive_relative(relative) {
            continue;
        }
        if files.len() == MAX_WALK_ENTRIES {
            return Err(HarnessError::new(
                ErrorCode::OutputLimitExceeded,
                "workspace walk exceeded the P3 entry bound",
            ));
        }
        files.push(WalkFile {
            absolute: path.to_owned(),
            relative: relative_text(relative),
        });
    }
    files.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(files)
}

#[derive(Clone, Debug)]
struct WalkFile {
    absolute: PathBuf,
    relative: String,
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
                    .and_then(|()| file.sync_all())
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
                file.sync_all().map_err(|error| {
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
        let value = value.to_string_lossy().to_ascii_lowercase();
        if matches!(value.as_str(), ".git" | ".harness" | ".env")
            || value.starts_with(".env.")
            || value.contains("credential")
            || value.contains("secret")
            || value.contains("password")
            || value.contains("private_key")
        {
            return true;
        }
        last = Some(value);
    }
    last.is_some_and(|name| {
        matches!(
            name.rsplit_once('.').map(|(_, extension)| extension),
            Some("pem" | "key" | "p12" | "pfx" | "clixml")
        )
    })
}

fn relative_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn truncate_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let result = walk_files(&root);
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
