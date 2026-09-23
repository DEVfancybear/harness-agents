//! The provider API key saved by the app itself.
//!
//! `HA_LAUNCH` kept credentials out of the strict configuration file, and that
//! stays true: this is a separate file, written only when the user pastes a key
//! into the app, and readable only by that user.
//!
//! Two rules decide everything here:
//!
//! 1. **A sourced environment variable always wins over the file.** The file is
//!    what `/key` writes; the environment is what scripts, CI and shells set, and
//!    it must be able to override a stored key without editing a file by hand.
//! 2. **The key is only ever written into a file nobody else can open.** On Unix
//!    the stage file is created with mode 0600 and the directory with 0700, so the
//!    guarantee is the same one `ssh` and `git` make for their own secrets. On
//!    Windows the private directory receives an explicit ACL for the process
//!    identity and SYSTEM before the key is staged.
//!
//! An unreadable or malformed file is never quietly treated as "no credential":
//! the message names the path and a next step, and it deliberately does not echo
//! the parser detail, because a TOML type error can quote the value it rejected.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use harness_types::{ErrorCode, HarnessError};

use super::paths::LaunchEnvironment;

/// File name under the resolved data root; `.env`-style, not the strict config.
pub const CREDENTIAL_FILE_NAME: &str = "credentials.env";

/// Variable used inside the credential file.
pub const CREDENTIAL_FILE_VARIABLE: &str = "DEEPSEEK_API_KEY";

/// Environment variables probed for a key, in precedence order.
///
/// Only presence and the name are ever reported: the value is never logged,
/// stored in a journal, or placed in a command history.
pub const CREDENTIAL_VARIABLES: [&str; 2] = ["DEEPSEEK_API_KEY", "HA_API_KEY"];

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
    /// How the protection reads in `/status` and in the saved notice.
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
/// The value is deliberately absent: this type is rendered in `/status`, in
/// notices and in errors, so it can only ever carry the *name* of the source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CredentialSource {
    /// An environment variable carried the key at process start.
    Environment { variable: String },
    /// The app saved the key in this file.
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
    /// An environment variable is read from the same process table the credential
    /// resolver reads, so replacing it in the app would be a lie unless the whole
    /// process environment changed. The file is re-read at call time, so it
    /// changes the moment the app writes it — which is the whole reason `/key`
    /// works without a restart. Asserted by the unit test, not called in
    /// production.
    #[allow(dead_code, reason = "documents the /key contract; asserted by tests")]
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

/// Where the credential for this launch comes from, if anywhere.
///
/// An environment variable always wins over the saved file: scripts, CI and
/// shells export the key, and an override has to be able to beat a file the app
/// wrote earlier without editing that file by hand.
///
/// A **malformed** file is not an error here. This runs during boot and on every
/// render, so it stays infallible and cheap: it answers from the process
/// environment and one `stat`. A broken file is reported with its path by
/// [`load`], at call time, where the message can be acted on.
#[must_use]
pub fn source(environment: &LaunchEnvironment, data_dir: &Path) -> Option<CredentialSource> {
    if let Some(variable) = CREDENTIAL_VARIABLES.iter().find(|name| {
        environment
            .value(name)
            .is_some_and(|value| !value.is_empty())
    }) {
        return Some(CredentialSource::Environment {
            variable: (*variable).to_owned(),
        });
    }
    source_for(environment, data_dir, CREDENTIAL_VARIABLES[0])
}

/// Resolve a configured credential variable before the app-owned file fallback.
#[must_use]
pub fn source_for(
    environment: &LaunchEnvironment,
    data_dir: &Path,
    variable: &str,
) -> Option<CredentialSource> {
    if environment
        .value(variable)
        .is_some_and(|value| !value.is_empty())
    {
        return Some(CredentialSource::Environment {
            variable: variable.to_owned(),
        });
    }
    let path = resolve_file(environment, data_dir);
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => Some(CredentialSource::File {
            path,
            // Measured when the key was saved, not on every launch: re-reading an
            // ACL means shelling out on Windows, and `source` runs on render paths.
            // `NotReverified` is the honest label for that; it never claims more
            // protection than was actually applied.
            protection: Protection::NotReverified,
        }),
        _ => None,
    }
}

/// The stored key, if the file exists and holds one.
///
/// Returns `Ok(None)` when the file is absent, and an actionable error when it
/// exists but cannot be used. An empty or blank value is treated as absent: it
/// grants nothing, and reporting it as a credential would only produce a
/// confusing failure at call time.
pub fn load(path: &Path) -> Result<Option<String>, HarnessError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
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
    let value = parse(&contents).map_err(|detail| {
        HarnessError::new(
            ErrorCode::ConfigParseError,
            format!(
                "credential file {} is invalid: {detail}; run ha again and use /key to save the key, or delete the file",
                path.display()
            ),
        )
    })?;
    match value {
        Some(value) if !value.trim().is_empty() => Ok(Some(value)),
        _ => Ok(None),
    }
}

/// Parse one `NAME=value` line.
///
/// Only the single known variable is accepted: a file that happens to hold
/// something else is reported rather than half-understood. The returned detail
/// never contains the value.
fn parse(contents: &str) -> Result<Option<String>, &'static str> {
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
        let name = name.trim();
        if name != CREDENTIAL_FILE_VARIABLE {
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

/// Save a key so the next launch is already configured.
///
/// The write is staged and then renamed, so a reader never observes a half-written
/// file. The order that matters is the one inside [`write_staged`]: on Unix the
/// file is **created** with mode 0600, so the key is never in a file that anyone
/// else can open, not even for the instant between creating and tightening it. The
/// containing directory is restricted first as well, which is what bounds exposure
/// on Windows, where creating a file with an explicit owner-only ACL is not
/// something this crate does.
pub fn save(path: &Path, key: &str) -> Result<Protection, HarnessError> {
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
    let contents = format!("{CREDENTIAL_FILE_VARIABLE}=\"{}\"\n", escape(key));
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

/// Escape the characters that would break the quoted value.
///
/// The backslash is escaped first, then the quote, so the two never collide: a
/// key that contains a literal `\"` survives the round trip through [`unescape`].
fn escape(key: &str) -> String {
    key.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Reverse [`escape`].
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
            .unwrap_or_else(|| std::ffi::OsStr::new("credentials.env")),
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
        CREDENTIAL_DIRECTORY_VARIABLE, CREDENTIAL_FILE_NAME, CREDENTIAL_FILE_VARIABLE,
        CredentialSource, Protection, load, resolve_file, save,
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

    #[test]
    fn k01_only_the_known_variable_is_accepted() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join(CREDENTIAL_FILE_NAME);
        std::fs::write(&path, "SOMETHING_ELSE=\"sk-x\"\n").expect("fixture");
        let error = load(&path).expect_err("an unknown variable is refused");
        let message = error.to_string();
        assert!(message.contains("does not use"), "{message}");
        assert!(
            !message.contains("sk-x"),
            "the message must not echo the file: {message}"
        );
    }

    #[test]
    fn k01_the_parser_detail_never_quotes_the_value() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join(CREDENTIAL_FILE_NAME);
        std::fs::write(&path, "DEEPSEEK_API_KEY=sk-secret-without-quotes\n").expect("fixture");
        let error = load(&path).expect_err("an unquoted value is refused");
        let message = error.to_string();
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains("/key"), "{message}");
        assert!(
            !message.contains("sk-secret-without-quotes"),
            "a parse error must not echo the key: {message}"
        );
    }

    #[test]
    fn k01_missing_and_blank_files_are_absent_not_errors() {
        let temp = tempfile::tempdir().expect("temp dir");
        assert!(
            load(&temp.path().join("nothing-here.env"))
                .expect("a missing file is not an error")
                .is_none()
        );
        let blank = temp.path().join(CREDENTIAL_FILE_NAME);
        std::fs::write(&blank, format!("{CREDENTIAL_FILE_VARIABLE}=\"   \"\n")).expect("fixture");
        assert!(
            load(&blank)
                .expect("a blank value is not an error")
                .is_none(),
            "a blank value grants nothing"
        );
    }

    #[test]
    fn k01_round_trip_keeps_the_key_and_leaves_no_staging_file() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("nested").join(CREDENTIAL_FILE_NAME);
        save(&path, "sk-round-trip").expect("the key is saved");
        assert_eq!(
            load(&path).expect("the key loads").as_deref(),
            Some("sk-round-trip")
        );
        let leftovers: Vec<PathBuf> = std::fs::read_dir(path.parent().expect("parent"))
            .expect("readable")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|entry| entry.to_string_lossy().contains("staged"))
            .collect();
        assert!(leftovers.is_empty(), "staging files remain: {leftovers:?}");
    }

    #[test]
    fn k01_a_quote_in_the_key_survives_a_round_trip() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join(CREDENTIAL_FILE_NAME);
        save(&path, "sk-with\"quote").expect("the key is saved");
        assert_eq!(
            load(&path).expect("the key loads").as_deref(),
            Some("sk-with\"quote")
        );
    }

    #[cfg(unix)]
    #[test]
    fn k01_the_file_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join(CREDENTIAL_FILE_NAME);
        save(&path, "sk-permissions").expect("the key is saved");
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
        let staging = temp.path().join(".credentials.env.staged");
        super::write_staged(&staging, b"DEEPSEEK_API_KEY=\"sk-x\"\n").expect("staged write");
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
        assert_eq!(
            std::fs::read_to_string(&staging).expect("readable"),
            "DEEPSEEK_API_KEY=\"sk-x\"\n"
        );
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
            path: PathBuf::from("C:/fixture/credentials.env"),
            protection: Protection::OwnerOnlyAcl,
        };
        assert!(from_file.describe().contains("credentials.env"));
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
    /// The profile directory on this machine inherits a read grant to a group that
    /// is not the user, so this is the difference between claiming protection and
    /// applying it.
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
        let protection = save(&path, "sk-acl-measured").expect("the key is saved");
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
        assert_eq!(
            load(&path).expect("the key loads").as_deref(),
            Some("sk-acl-measured")
        );
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
