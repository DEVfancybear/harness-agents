use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use harness_store_sqlite::ProjectRegistrationRecord;
use harness_types::{ContentHash, ErrorCode, HarnessError, ProjectId, WorkspaceObservation};
use ignore::WalkBuilder;
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

pub(crate) fn inspect_workspace(
    root: &Path,
    project_id: ProjectId,
) -> Result<WorkspaceDescriptor, HarnessError> {
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
    let git_head =
        git_output(&canonical, ["rev-parse", "HEAD"]).unwrap_or_else(|| "not_git".to_owned());
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
    let fingerprint = workspace_fingerprint(&canonical, &git_head)?;
    Ok(WorkspaceDescriptor {
        project_id,
        root: canonical,
        root_text,
        git_common_dir,
        git_head,
        identity_hash,
        fingerprint,
    })
}

pub(crate) fn workspace_fingerprint(
    root: &Path,
    git_head: &str,
) -> Result<ContentHash, HarnessError> {
    let mut entries = Vec::new();
    for item in walk_files(root)? {
        let hash = hash_file(&item.absolute)?;
        entries.push(json!({"path": item.relative, "content_hash": hash}));
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

pub(crate) fn search_text(
    root: &Path,
    query: &str,
    requested: Option<&str>,
) -> Result<SearchOutput, HarnessError> {
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
    let mut matches = Vec::new();
    let mut truncated = false;
    for file in walk_files(&base)? {
        let Ok(text) = read_text(&file.absolute) else {
            continue;
        };
        for (line_index, line) in text.lines().enumerate() {
            for (column, _) in line.match_indices(query) {
                if matches.len() == MAX_SEARCH_MATCHES {
                    truncated = true;
                    break;
                }
                let relative = file.absolute.strip_prefix(root).map_err(|_| {
                    HarnessError::new(
                        ErrorCode::WorkspaceEscape,
                        "searched path escaped workspace root",
                    )
                })?;
                matches.push(SearchMatch {
                    path: relative_text(relative),
                    line: u64::try_from(line_index.saturating_add(1)).unwrap_or(u64::MAX),
                    column: u64::try_from(column.saturating_add(1)).unwrap_or(u64::MAX),
                    preview: truncate_text(&redact_text(line), 240),
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
    Ok(SearchOutput { matches, truncated })
}

pub(crate) fn apply_text_patch(
    path: &Path,
    expected_hash: &ContentHash,
    replacement: &str,
) -> Result<(ContentHash, ContentHash), HarnessError> {
    let current = read_text(path)?;
    let before_hash = ContentHash::from_bytes(current.as_bytes());
    if &before_hash != expected_hash {
        return Err(HarnessError::new(
            ErrorCode::StaleWorkspace,
            "patch expected hash does not match current file content",
        ));
    }
    write_text_atomically(path, replacement)?;
    let after = read_text(path)?;
    let after_hash = ContentHash::from_bytes(after.as_bytes());
    if after != replacement {
        return Err(HarnessError::new(
            ErrorCode::ProcessOutcomeUnknown,
            "atomic patch replacement cannot be verified",
        ));
    }
    Ok((before_hash, after_hash))
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
        let item = item.map_err(|error| {
            HarnessError::new(
                ErrorCode::WorkspaceEscape,
                format!("workspace walk failed: {error}"),
            )
        })?;
        let path = item.path();
        if path == root {
            continue;
        }
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            HarnessError::new(
                ErrorCode::WorkspaceEscape,
                format!("cannot inspect workspace walk entry: {error}"),
            )
        })?;
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

fn write_text_atomically(path: &Path, replacement: &str) -> Result<(), HarnessError> {
    let parent = path.parent().ok_or_else(|| {
        HarnessError::new(
            ErrorCode::WorkspaceEscape,
            "patch path has no parent directory",
        )
    })?;
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

fn hash_file(path: &Path) -> Result<ContentHash, HarnessError> {
    let mut file = File::open(path).map_err(|error| {
        HarnessError::new(
            ErrorCode::WorkspaceEscape,
            format!("cannot hash workspace file: {error}"),
        )
    })?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 32 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            HarnessError::new(
                ErrorCode::WorkspaceEscape,
                format!("cannot hash workspace file: {error}"),
            )
        })?;
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
    ContentHash::parse(text)
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
}
