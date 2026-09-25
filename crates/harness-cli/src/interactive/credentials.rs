//! Provider credentials saved by the app itself, one entry per provider, after
//! prime-agent's `auth.json` (`packages/coding-agent/src/core/auth-storage.ts`).
//!
//! `HA_LAUNCH` kept credentials out of the strict configuration file, and that
//! stays true: this is a separate file, written only by `/login`, and readable
//! only by that user. It maps a provider id to either an API key or the tokens a
//! browser sign-in returned:
//!
//! ```json
//! {"opencode": {"type": "api_key", "key": "..."},
//!  "openai-codex": {"type": "oauth", "access": "...", "refresh": "...", "expires": 0}}
//! ```
//!
//! Two rules decide everything here:
//!
//! 1. **A saved credential wins over the environment**, as in prime-agent: what the
//!    user chose with `/login` is what runs. The provider's environment variables
//!    are the fallback, and `/logout` removes the saved entry so they apply again.
//! 2. **The file is only ever written where nobody else can open it.** On Unix the
//!    stage file is created with mode 0600 and the directory with 0700; on Windows
//!    the private directory receives an explicit ACL for the process identity and
//!    SYSTEM before the file is staged.
//!
//! An unreadable or malformed file is never quietly treated as "no credential":
//! the message names the path and a next step, and it deliberately does not echo
//! the parser detail, because a parse error can quote the value it rejected.
//!
//! The single-key `credentials.env` of earlier versions is read as the `deepseek`
//! entry until the first save writes `auth.json`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use harness_types::{ErrorCode, HarnessError};
use serde::{Deserialize, Serialize};

use super::paths::LaunchEnvironment;

/// File name under the private directory.
pub const CREDENTIAL_FILE_NAME: &str = "auth.json";

/// The single-key file earlier versions wrote; read as the `deepseek` entry.
pub const LEGACY_FILE_NAME: &str = "credentials.env";

/// Variable used inside the legacy file.
const LEGACY_FILE_VARIABLE: &str = "DEEPSEEK_API_KEY";

/// Every environment variable that can carry a provider key, for the boot check
/// and for redaction. Only presence and the name are ever reported.
pub const CREDENTIAL_VARIABLES: [&str; 5] = [
    "DEEPSEEK_API_KEY",
    "HA_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "OPENCODE_API_KEY",
];

/// Environment variable that relocates the credential file, for tests and for a
/// caller that keeps its secrets somewhere else. It holds a directory, like
/// `HA_HOME`, not the file itself.
pub const CREDENTIAL_DIRECTORY_VARIABLE: &str = "HA_CREDENTIALS_DIR";

/// Where the credential file lives: a directory the app tightens on its own.
///
/// It is deliberately **not** the data root. The data root also holds project
/// stores that other tooling may need to reach, and restricting it would change
/// the ACL of directories this module does not own. A dedicated subdirectory can
/// be locked down without touching anything else.
pub const CREDENTIAL_DIRECTORY_NAME: &str = "private";

/// One saved credential.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Credential {
    ApiKey {
        key: String,
    },
    /// Tokens from a browser sign-in; `expires` is milliseconds since the epoch.
    Oauth {
        access: String,
        refresh: String,
        expires: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_id: Option<String>,
    },
}

impl Credential {
    #[must_use]
    pub fn api_key(key: impl Into<String>) -> Self {
        Self::ApiKey { key: key.into() }
    }

    /// What goes in the request: the key, or the access token.
    #[must_use]
    pub fn secret(&self) -> &str {
        match self {
            Self::ApiKey { key } => key,
            Self::Oauth { access, .. } => access,
        }
    }

    /// `API key` or `sign-in`, for listings; never the value.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::ApiKey { .. } => "API key",
            Self::Oauth { .. } => "sign-in",
        }
    }
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Credential({})", self.kind())
    }
}

/// The Windows account of the process token, rather than the interactive account
/// inherited through `USERNAME`/`USERDOMAIN`.
///
/// Sandboxes and service hosts commonly preserve the latter variables while running
/// under a restricted identity. Granting that stale name after removing inheritance
/// locks the running app out of the directory it just created.
#[cfg(windows)]
fn current_windows_account() -> Option<String> {
    let output = std::process::Command::new("whoami").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let account = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!account.is_empty()).then_some(account)
}

#[cfg(not(windows))]
fn current_windows_account() -> Option<String> {
    None
}

/// How well the file is protected against other accounts on this machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protection {
    /// Owner-only permissions were applied by this app (Unix mode 0600/0700).
    ///
    /// Constructed only where mode bits exist; the Windows branch reports
    /// [`Self::OwnerOnlyAcl`] instead.
    #[cfg_attr(windows, allow(dead_code, reason = "the Unix branch constructs this"))]
    OwnerOnly,
    /// An access control list restricting the file was applied by this app.
    ///
    /// The mirror of [`Self::OwnerOnly`]: constructed only by the Windows branch,
    /// so it is dead code where mode bits are what protect the file.
    #[cfg_attr(unix, allow(dead_code, reason = "the Windows branch constructs this"))]
    OwnerOnlyAcl,
    /// Only the platform default applies: the file sits in the user's own profile
    /// directory and nothing else was changed.
    ///
    /// Reached from the Windows branch when the account cannot be determined; on
    /// Unix the mode-bit path reports [`Self::OwnerOnly`] instead.
    #[cfg_attr(unix, allow(dead_code, reason = "the Windows branch returns this"))]
    ProfileDefault,
    /// A file that was found on disk. This app set it up when it saved the key, but
    /// the current launch did not measure it again, so it is not claimed.
    NotReverified,
}

impl Protection {
    /// How the protection reads in `/session` and in the saved notice.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::OwnerOnly => "owner-only permissions (0600)",
            Self::OwnerOnlyAcl => "an access control list with this account and SYSTEM only",
            Self::ProfileDefault => {
                "the profile default only: no owner-only permission could be applied, so another \
                 account on this machine may be able to read the file"
            }
            Self::NotReverified => {
                "the permissions this app applied when it saved the key (not re-measured now)"
            }
        }
    }
}

/// Where the credential for this launch came from, and its name.
///
/// The value is deliberately absent: this type is rendered in `/session`, in
/// notices and in errors, so it can only ever carry the *name* of the source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CredentialSource {
    /// An environment variable carried the key at process start.
    Environment { variable: String },
    /// The app saved the credential in this file.
    File {
        path: PathBuf,
        protection: Protection,
    },
}

impl CredentialSource {
    /// Short description for a rendered line; never the value.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Environment { variable } => format!("environment variable {variable}"),
            Self::File { path, .. } => format!("saved file {}", path.display()),
        }
    }

    /// How well a saved file is protected; `None` for an environment variable,
    /// which is not a file at all.
    #[must_use]
    pub const fn protection(&self) -> Option<Protection> {
        match self {
            Self::File { protection, .. } => Some(*protection),
            Self::Environment { .. } => None,
        }
    }

    /// Whether the key can still change while the process runs.
    ///
    /// The file is re-read at call time, so it changes the moment `/login` writes
    /// it; an environment variable is fixed for the process. Asserted by the unit
    /// test, not called in production.
    #[allow(dead_code, reason = "documents the /login contract; asserted by tests")]
    #[must_use]
    pub const fn is_live(&self) -> bool {
        match self {
            Self::Environment { .. } => false,
            Self::File { .. } => true,
        }
    }
}

/// Resolve the credential file for one launch.
///
/// [`CREDENTIAL_DIRECTORY_VARIABLE`] wins over the data root so a test can point
/// the file at a temporary directory instead of the user's real one.
#[must_use]
pub fn resolve_file(environment: &LaunchEnvironment, data_dir: &Path) -> PathBuf {
    resolve_path(environment, data_dir).join(CREDENTIAL_FILE_NAME)
}

/// Resolve the directory that holds the credential file.
///
/// The data root gets a dedicated `private` subdirectory, so tightening this
/// directory cannot change the ACL of the project stores beside it.
#[must_use]
pub fn resolve_path(environment: &LaunchEnvironment, data_dir: &Path) -> PathBuf {
    environment
        .value(CREDENTIAL_DIRECTORY_VARIABLE)
        .filter(|value| !value.is_empty())
        .map_or_else(|| data_dir.join(CREDENTIAL_DIRECTORY_NAME), PathBuf::from)
}

/// Whether any credential exists for this launch: a saved one, or a provider
/// variable in the environment. The boot check asks this; which provider the
/// credential is for is settled when the provider is resolved.
#[must_use]
pub fn source(environment: &LaunchEnvironment, data_dir: &Path) -> Option<CredentialSource> {
    let path = resolve_file(environment, data_dir);
    if read_all(&path).is_ok_and(|entries| !entries.is_empty()) {
        return Some(CredentialSource::File {
            path,
            protection: Protection::NotReverified,
        });
    }
    CREDENTIAL_VARIABLES
        .iter()
        .find(|name| {
            environment
                .value(name)
                .is_some_and(|value| !value.is_empty())
        })
        .map(|variable| CredentialSource::Environment {
            variable: (*variable).to_owned(),
        })
}

/// Where one provider's credential comes from: the saved entry first, then the
/// configured variable, then the provider's own variables.
///
/// This stays infallible and cheap because it runs on render paths: a broken file
/// is reported with its path by [`load`], at call time, where it can be acted on.
#[must_use]
pub fn source_for(
    environment: &LaunchEnvironment,
    data_dir: &Path,
    provider: &str,
    variable: &str,
) -> Option<CredentialSource> {
    let path = resolve_file(environment, data_dir);
    if read_all(&path).is_ok_and(|entries| entries.contains_key(provider)) {
        return Some(CredentialSource::File {
            path,
            // Measured when the key was saved, not on every launch: re-reading an
            // ACL means shelling out on Windows, and this runs on render paths.
            protection: Protection::NotReverified,
        });
    }
    std::iter::once(variable)
        .chain(super::providers::env_variables(provider).iter().copied())
        .filter(|name| !name.is_empty())
        .find(|name| {
            environment
                .value(name)
                .is_some_and(|value| !value.is_empty())
        })
        .map(|variable| CredentialSource::Environment {
            variable: variable.to_owned(),
        })
}

/// The saved credential for one provider, if there is one.
///
/// Returns `Ok(None)` when the file or the entry is absent, and an actionable
/// error when the file exists but cannot be used. A blank key is treated as
/// absent: it grants nothing.
pub fn load(path: &Path, provider: &str) -> Result<Option<Credential>, HarnessError> {
    Ok(read_all(path)?
        .remove(provider)
        .filter(|credential| !credential.secret().trim().is_empty()))
}

/// The providers with a saved credential, and what kind each is.
pub fn stored(path: &Path) -> Result<Vec<(String, &'static str)>, HarnessError> {
    Ok(read_all(path)?
        .into_iter()
        .map(|(provider, credential)| (provider, credential.kind()))
        .collect())
}

/// Every saved secret - keys, access and refresh tokens - so an export can
/// redact them. Unreadable files contribute nothing.
#[must_use]
pub fn secrets(path: &Path) -> Vec<String> {
    read_all(path)
        .unwrap_or_default()
        .into_values()
        .flat_map(|credential| match credential {
            Credential::ApiKey { key } => vec![key],
            Credential::Oauth {
                access, refresh, ..
            } => vec![access, refresh],
        })
        .filter(|secret| !secret.trim().is_empty())
        .collect()
}

/// Every saved entry. A missing file is empty; before `auth.json` exists, the
/// legacy `credentials.env` beside it is read as the `deepseek` entry.
fn read_all(path: &Path) -> Result<BTreeMap<String, Credential>, HarnessError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return read_legacy(&path.with_file_name(LEGACY_FILE_NAME));
        }
        Err(error) => {
            return Err(HarnessError::new(
                ErrorCode::ConfigReadError,
                format!(
                    "credential file {} could not be read: {error}",
                    path.display()
                ),
            ));
        }
    };
    if contents.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_str(&contents).map_err(|_| {
        HarnessError::new(
            ErrorCode::ConfigParseError,
            format!(
                "credential file {} is invalid; run /login again, or delete the file",
                path.display()
            ),
        )
    })
}

fn read_legacy(path: &Path) -> Result<BTreeMap<String, Credential>, HarnessError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(error) => {
            return Err(HarnessError::new(
                ErrorCode::ConfigReadError,
                format!(
                    "credential file {} could not be read: {error}",
                    path.display()
                ),
            ));
        }
    };
    let value = parse_legacy(&contents).map_err(|detail| {
        HarnessError::new(
            ErrorCode::ConfigParseError,
            format!(
                "credential file {} is invalid: {detail}; use /login to save the key again, or delete the file",
                path.display()
            ),
        )
    })?;
    Ok(value
        .filter(|key| !key.trim().is_empty())
        .map(|key| BTreeMap::from([("deepseek".to_owned(), Credential::api_key(key))]))
        .unwrap_or_default())
}

/// Parse the legacy `DEEPSEEK_API_KEY="..."` line. The detail never has the value.
fn parse_legacy(contents: &str) -> Result<Option<String>, &'static str> {
    let mut stored: Option<String> = None;
    for (index, raw) in contents.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            return Err(if index == 0 {
                "the first line is not NAME=value"
            } else {
                "a line is not NAME=value"
            });
        };
        if name.trim() != LEGACY_FILE_VARIABLE {
            return Err("it names a variable this app does not use");
        }
        if stored.is_some() {
            return Err("the variable is set twice");
        }
        let value = value.trim();
        let Some(stored_value) = value
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .or_else(|| {
                value
                    .strip_prefix('\'')
                    .and_then(|rest| rest.strip_suffix('\''))
            })
        else {
            return Err("the value is not quoted; write DEEPSEEK_API_KEY=\"...\"");
        };
        stored = Some(unescape(stored_value));
    }
    Ok(stored)
}

/// Save one provider's credential, keeping the others.
///
/// The write is staged and then renamed, so a reader never observes a half-written
/// file. On Unix the stage file is **created** with mode 0600, so the key is never
/// in a file anyone else can open; the directory is restricted first as well,
/// which is what bounds exposure on Windows.
pub fn save(
    path: &Path,
    provider: &str,
    credential: &Credential,
) -> Result<Protection, HarnessError> {
    let mut entries = read_all(path)?;
    entries.insert(provider.to_owned(), credential.clone());
    write_all(path, &entries)
}

/// Remove one provider's saved credential. Returns whether there was one.
pub fn remove(path: &Path, provider: &str) -> Result<bool, HarnessError> {
    let mut entries = read_all(path)?;
    if entries.remove(provider).is_none() {
        return Ok(false);
    }
    write_all(path, &entries)?;
    // The legacy file would otherwise bring the key back as `deepseek`.
    if provider == "deepseek" {
        let _ = std::fs::remove_file(path.with_file_name(LEGACY_FILE_NAME));
    }
    Ok(true)
}

fn write_all(
    path: &Path,
    entries: &BTreeMap<String, Credential>,
) -> Result<Protection, HarnessError> {
    let Some(directory) = path.parent() else {
        return Err(HarnessError::new(
            ErrorCode::StorageOpenFailed,
            format!("credential path {} has no directory", path.display()),
        ));
    };
    std::fs::create_dir_all(directory).map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageOpenFailed,
            format!(
                "credential directory {} could not be created: {error}",
                directory.display()
            ),
        )
    })?;
    restrict_directory(directory);
    // Windows needs the process token's account. Environment variables can name
    // the desktop user even when a sandbox executes this process as another user.
    let account = current_windows_account();
    let protection = restrict_acl(directory, account.as_deref());
    let staging = staging_path(path);
    let mut contents = serde_json::to_string_pretty(entries).map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageOpenFailed,
            format!("credentials could not be encoded: {error}"),
        )
    })?;
    contents.push('\n');
    write_staged(&staging, contents.as_bytes()).map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageOpenFailed,
            format!(
                "credential file {} could not be written: {error}",
                staging.display()
            ),
        )
    })?;
    std::fs::rename(&staging, path).map_err(|error| {
        let _ = std::fs::remove_file(&staging);
        HarnessError::new(
            ErrorCode::StorageOpenFailed,
            format!(
                "credential file {} could not be replaced: {error}",
                path.display()
            ),
        )
    })?;
    // The rename carried the staged file's mode into place on Unix. On Windows an
    // existing file keeps whatever ACL it already had, so the directory is the
    // boundary; this call closes the gap for a file that predates this version.
    restrict_file(path);
    Ok(protection)
}

/// Restrict the directory with an access control list, on Windows only.
///
/// Windows has no mode bits to set, and a file created under the user profile
/// inherits the profile's ACL — which on a managed machine can include a group
/// that is not the user. This narrows the directory to the current account and
/// SYSTEM, and the credential file inherits that.
///
/// It shells out to `icacls` because the alternative is calling Win32 security
/// APIs, and this crate does not contain `unsafe` code. Every failure is reported
/// as [`Protection::ProfileDefault`] rather than claimed as protection: a file
/// that could not be restricted must not be described as restricted.
///
/// On Unix this is a no-op: mode bits already did the job.
#[cfg_attr(
    unix,
    allow(unused_variables, reason = "only the Windows branch reads the path")
)]
fn restrict_acl(directory: &Path, account: Option<&str>) -> Protection {
    #[cfg(unix)]
    {
        let _ = account;
        Protection::OwnerOnly
    }
    #[cfg(not(unix))]
    {
        let Some(account) = account else {
            return Protection::ProfileDefault;
        };
        let inheritable = format!("{account}:(OI)(CI)F");
        let removed = std::process::Command::new("icacls")
            .arg(directory)
            .arg("/inheritance:r")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if !matches!(removed, Ok(status) if status.success()) {
            return Protection::ProfileDefault;
        }
        let granted = std::process::Command::new("icacls")
            .arg(directory)
            .args(["/grant:r", &inheritable])
            .args(["/grant:r", "SYSTEM:(OI)(CI)F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if matches!(granted, Ok(status) if status.success()) {
            Protection::OwnerOnlyAcl
        } else {
            Protection::ProfileDefault
        }
    }
}

/// Create the staged file owner-only and write the key into it.
///
/// `OpenOptions::mode` is applied by the kernel at creation time, which is the
/// only way to write a secret into a file that was never readable by anyone else.
/// `set_permissions` after the fact cannot make that promise: it needs the file to
/// exist first, and that gap is exactly what this avoids.
fn write_staged(staging: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    // A staging file left by an interrupted write must not be reused: it may
    // carry wider permissions than this write grants, or be a symlink planted
    // at the path. Removing the link and creating anew is the only shape that
    // keeps the owner-only promise.
    match std::fs::remove_file(staging) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(staging)?;
    file.write_all(contents)?;
    file.sync_all()
}

/// Undo the legacy file's quoting.
///
/// A trailing lone backslash is kept as written rather than dropped: the file is
/// user-editable, and silently losing a character from a key would produce a
/// confusing authentication failure instead of a visible one.
fn unescape(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            result.push(character);
            continue;
        }
        match characters.next() {
            // An escaped backslash and a trailing lone backslash both stay as one
            // backslash: the file is user-editable, and silently dropping the
            // character would turn a typo into a confusing authentication failure.
            Some('\\') | None => result.push('\\'),
            Some('"') => result.push('"'),
            Some(other) => {
                result.push('\\');
                result.push(other);
            }
        }
    }
    result
}

/// Stage beside the target so the rename stays on one filesystem.
fn staging_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(
        path.file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new(CREDENTIAL_FILE_NAME)),
    );
    name.push(".staged");
    path.with_file_name(name)
}

/// Owner-only permissions on Unix; best effort elsewhere.
fn restrict_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Owner-only directory permissions on Unix; best effort elsewhere.
fn restrict_directory(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CREDENTIAL_DIRECTORY_VARIABLE, CREDENTIAL_FILE_NAME, Credential, CredentialSource,
        LEGACY_FILE_NAME, Protection, load, remove, resolve_file, save, source_for, stored,
    };
    use crate::interactive::paths::LaunchEnvironment;
    use std::path::{Path, PathBuf};

    fn environment(pairs: &[(&str, &str)]) -> LaunchEnvironment {
        LaunchEnvironment::from_pairs(
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
        )
    }

    fn key(path: &Path, provider: &str) -> Option<String> {
        load(path, provider)
            .expect("the file loads")
            .map(|credential| credential.secret().to_owned())
    }

    #[test]
    fn k01_each_provider_keeps_its_own_entry() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("nested").join(CREDENTIAL_FILE_NAME);
        save(&path, "opencode", &Credential::api_key("sk-open")).expect("saved");
        save(&path, "deepseek", &Credential::api_key("sk-deep")).expect("saved");
        assert_eq!(key(&path, "opencode").as_deref(), Some("sk-open"));
        assert_eq!(key(&path, "deepseek").as_deref(), Some("sk-deep"));
        assert_eq!(key(&path, "openai"), None);
        let leftovers: Vec<PathBuf> = std::fs::read_dir(path.parent().expect("parent"))
            .expect("readable")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|entry| entry.to_string_lossy().contains("staged"))
            .collect();
        assert!(leftovers.is_empty(), "staging files remain: {leftovers:?}");

        assert!(remove(&path, "opencode").expect("removed"));
        assert!(!remove(&path, "opencode").expect("already gone"));
        assert_eq!(key(&path, "opencode"), None);
        assert_eq!(
            stored(&path).expect("listed"),
            vec![("deepseek".to_owned(), "API key")]
        );
    }

    #[test]
    fn k01_a_broken_file_is_reported_without_its_contents() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join(CREDENTIAL_FILE_NAME);
        std::fs::write(&path, "{\"deepseek\": sk-not-json").expect("fixture");
        let message = load(&path, "deepseek")
            .expect_err("a broken file is refused")
            .to_string();
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains("/login"), "{message}");
        assert!(!message.contains("sk-not-json"), "{message}");
    }

    #[test]
    fn k01_the_legacy_single_key_file_reads_as_deepseek() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join(CREDENTIAL_FILE_NAME);
        std::fs::write(
            temp.path().join(LEGACY_FILE_NAME),
            "DEEPSEEK_API_KEY=\"sk-with\\\"quote\"\n",
        )
        .expect("fixture");
        assert_eq!(key(&path, "deepseek").as_deref(), Some("sk-with\"quote"));
        // The first save carries it over; removing it removes the legacy file too.
        save(&path, "opencode", &Credential::api_key("sk-open")).expect("saved");
        assert_eq!(key(&path, "deepseek").as_deref(), Some("sk-with\"quote"));
        assert!(remove(&path, "deepseek").expect("removed"));
        assert!(!temp.path().join(LEGACY_FILE_NAME).exists());
        assert_eq!(key(&path, "deepseek"), None);
    }

    #[test]
    fn k01_a_blank_key_grants_nothing_and_debug_never_shows_a_value() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join(CREDENTIAL_FILE_NAME);
        save(&path, "deepseek", &Credential::api_key("   ")).expect("saved");
        assert_eq!(key(&path, "deepseek"), None);
        let shown = format!("{:?}", Credential::api_key("sk-secret"));
        assert!(!shown.contains("sk-secret"), "{shown}");
    }

    /// prime-agent's order: what `/login` saved wins over the environment.
    #[test]
    fn k02_a_saved_credential_wins_over_the_environment() {
        let temp = tempfile::tempdir().expect("temp dir");
        let data_dir = temp.path();
        let env = environment(&[("OPENCODE_API_KEY", "sk-env")]);
        assert_eq!(
            source_for(&env, data_dir, "opencode", ""),
            Some(CredentialSource::Environment {
                variable: "OPENCODE_API_KEY".to_owned()
            })
        );
        let path = resolve_file(&env, data_dir);
        save(&path, "opencode", &Credential::api_key("sk-saved")).expect("saved");
        assert!(matches!(
            source_for(&env, data_dir, "opencode", ""),
            Some(CredentialSource::File { .. })
        ));
        // Another provider's saved key is not this provider's credential.
        assert_eq!(source_for(&environment(&[]), data_dir, "openai", ""), None);
    }

    #[cfg(unix)]
    #[test]
    fn k01_the_file_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join(CREDENTIAL_FILE_NAME);
        save(&path, "deepseek", &Credential::api_key("sk-permissions")).expect("saved");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "credential file mode was {mode:o}");
        let directory_mode = std::fs::metadata(path.parent().expect("parent"))
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            directory_mode, 0o700,
            "credential directory mode was {directory_mode:o}"
        );
    }

    /// The key must never exist in a file that others can open, not even briefly.
    ///
    /// This asserts the creation flags rather than a timing window: the file is
    /// created by `write_staged`, and on Unix that call passes `mode(0o600)` to the
    /// kernel. A later `set_permissions` could not make the same promise, because it
    /// needs the file to exist first.
    #[test]
    fn k01_the_stage_file_is_created_with_restrictive_flags() {
        let temp = tempfile::tempdir().expect("temp dir");
        let staging = temp.path().join(".auth.json.staged");
        super::write_staged(&staging, b"{}\n").expect("staged write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&staging)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "stage file mode was {mode:o}");
        }
        assert_eq!(std::fs::read_to_string(&staging).expect("readable"), "{}\n");
    }

    #[test]
    fn k02_the_credential_file_follows_the_explicit_directory() {
        let data_dir = PathBuf::from("C:/fixture/data");
        assert_eq!(
            resolve_file(&environment(&[]), &data_dir),
            data_dir
                .join(super::CREDENTIAL_DIRECTORY_NAME)
                .join(CREDENTIAL_FILE_NAME)
        );
        assert_eq!(
            resolve_file(
                &environment(&[(CREDENTIAL_DIRECTORY_VARIABLE, "C:/fixture/secrets")]),
                &data_dir
            ),
            Path::new("C:/fixture/secrets").join(CREDENTIAL_FILE_NAME)
        );
        assert_eq!(
            resolve_file(
                &environment(&[(CREDENTIAL_DIRECTORY_VARIABLE, "")]),
                &data_dir
            ),
            data_dir
                .join(super::CREDENTIAL_DIRECTORY_NAME)
                .join(CREDENTIAL_FILE_NAME),
            "an empty override is not an override"
        );
    }

    #[test]
    fn k02_a_source_is_described_by_name_never_by_value() {
        let from_environment = CredentialSource::Environment {
            variable: "DEEPSEEK_API_KEY".to_owned(),
        };
        assert!(from_environment.describe().contains("DEEPSEEK_API_KEY"));
        assert!(!from_environment.is_live());

        let from_file = CredentialSource::File {
            path: PathBuf::from("C:/fixture/auth.json"),
            protection: Protection::OwnerOnlyAcl,
        };
        assert!(from_file.describe().contains("auth.json"));
        assert!(from_file.is_live(), "a saved file is re-read at call time");
        assert_eq!(from_file.protection(), Some(Protection::OwnerOnlyAcl));
        assert!(
            from_file
                .protection()
                .expect("a file reports protection")
                .describe()
                .contains("SYSTEM"),
            "the acl wording names what it grants"
        );
        assert_eq!(
            from_environment.protection(),
            None,
            "an environment variable is not a file and has no file protection"
        );
    }

    /// K03: the credential directory is the documented `private` subdirectory, so
    /// locking it down never disturbs the project stores beside it.
    #[test]
    fn k03_the_credential_directory_is_a_dedicated_subdirectory() {
        let data_dir = PathBuf::from("C:/fixture/data");
        assert_eq!(
            super::resolve_path(&environment(&[]), &data_dir),
            data_dir.join(super::CREDENTIAL_DIRECTORY_NAME)
        );
        assert_eq!(
            super::resolve_path(
                &environment(&[(CREDENTIAL_DIRECTORY_VARIABLE, "C:/fixture/secrets")]),
                &data_dir
            ),
            Path::new("C:/fixture/secrets"),
            "an explicit directory is used as given, not nested again"
        );
    }

    /// K03: on Windows the saved directory carries a real access control list.
    ///
    /// This measures the actual ACL instead of trusting the code path: it saves a
    /// key, reads the ACL back with `icacls`, and asserts that the account running
    /// the test and SYSTEM are present while an unrelated sandbox group is gone.
    ///
    /// The assertion is on the English spelling of SYSTEM, because `icacls` prints
    /// resolved account names rather than SIDs. That is the measured behavior here;
    /// it is not claimed to hold on a non-English Windows.
    #[cfg(windows)]
    #[test]
    fn k03_a_saved_key_is_restricted_to_this_account_by_an_acl() {
        let temp = tempfile::tempdir().expect("temp dir");
        let directory = temp.path().join("private");
        let path = directory.join(CREDENTIAL_FILE_NAME);
        let protection =
            save(&path, "deepseek", &Credential::api_key("sk-acl-measured")).expect("saved");
        assert_eq!(
            protection,
            Protection::OwnerOnlyAcl,
            "the ACL step must report what it did"
        );

        let account = super::current_windows_account().expect("whoami resolves this process");
        let acl = std::process::Command::new("icacls")
            .arg(&directory)
            .output()
            .expect("icacls runs");
        let text = String::from_utf8_lossy(&acl.stdout).into_owned();
        assert!(
            text.to_lowercase().contains(&account.to_lowercase()),
            "the ACL must name this account: {text}"
        );
        assert!(text.contains("SYSTEM"), "the ACL must keep SYSTEM: {text}");
        assert!(
            !text.contains("CodexSandboxUsers") && !text.contains("S-1-15-3-"),
            "the inherited group grants must be gone: {text}"
        );
        // The app must still be able to use what it just protected.
        assert_eq!(key(&path, "deepseek").as_deref(), Some("sk-acl-measured"));
    }

    /// The mode bits and the ACL are one contract: owner-only either way.
    #[test]
    fn k03_the_protection_labels_say_what_they_grant() {
        assert!(Protection::OwnerOnly.describe().contains("0600"));
        assert!(Protection::OwnerOnlyAcl.describe().contains("SYSTEM"));
        assert!(
            Protection::ProfileDefault
                .describe()
                .contains("may be able to read"),
            "a file that could not be restricted must say so"
        );
        assert!(
            Protection::NotReverified
                .describe()
                .contains("not re-measured"),
            "a launch must not claim it measured anything"
        );
    }
}
