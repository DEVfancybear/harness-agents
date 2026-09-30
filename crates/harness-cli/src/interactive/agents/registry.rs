//! Where background workers say how to reach them.
//!
//! One worker serves one project store: its agents share the store, so two of
//! them never fight over the store's writer lock. The worker writes a descriptor
//! (its port, its token, its process) beside a lock file it holds while it runs;
//! a client reads the descriptor to connect. The token is the only thing that
//! lets a connection in, so the descriptor is written for its owner only and
//! never printed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: u16 = 1;

/// The directory the worker descriptors live in.
#[must_use]
pub fn directory(data_dir: &Path) -> PathBuf {
    data_dir.join("workers")
}

/// A short, stable name for the worker of one project store.
#[must_use]
pub fn project_key(store_dir: &Path) -> String {
    let digest = Sha256::digest(store_dir.to_string_lossy().as_bytes());
    digest.iter().take(8).fold(String::new(), |mut key, byte| {
        use std::fmt::Write as _;
        let _ = write!(key, "{byte:02x}");
        key
    })
}

/// What a running worker wrote about itself.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub schema_version: u16,
    pub worker_id: String,
    pub pid: u32,
    /// The loopback port it listens on.
    pub port: u16,
    /// What a connection presents first. Owner-only, never logged.
    pub token: String,
    pub store_dir: PathBuf,
    pub project_root: PathBuf,
    pub started_at_unix_ms: i64,
    /// Which build of `ha` runs it ([`build_identity`]): a client of another
    /// build does not speak to it.
    pub build: String,
}

/// This executable's identity: its version and the size and time of its file.
/// An install replaces the file, so a worker started before it tells itself
/// apart from a client started after.
#[must_use]
pub fn build_identity() -> String {
    let file = std::env::current_exe()
        .and_then(std::fs::metadata)
        .map(|metadata| {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_nanos());
            format!("{}-{modified}", metadata.len())
        })
        .unwrap_or_default();
    format!("{}+{file}", env!("CARGO_PKG_VERSION"))
}

#[must_use]
pub fn descriptor_path(directory: &Path, key: &str) -> PathBuf {
    directory.join(format!("{key}.json"))
}

#[must_use]
pub fn lock_path(directory: &Path, key: &str) -> PathBuf {
    directory.join(format!("{key}.lock"))
}

#[must_use]
pub fn log_path(directory: &Path, key: &str) -> PathBuf {
    directory.join(format!("{key}.log"))
}

/// Write the descriptor through a staged copy, for its owner only.
///
/// # Errors
/// The file cannot be written.
pub fn write(directory: &Path, key: &str, descriptor: &Descriptor) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
    let text = serde_json::to_string_pretty(descriptor).map_err(std::io::Error::other)?;
    let path = descriptor_path(directory, key);
    let staged = path.with_extension("json.staged");
    std::fs::write(&staged, text)?;
    restrict_to_owner(&staged)?;
    std::fs::rename(&staged, &path)
}

/// The descriptor at `path`, when there is a readable one.
#[must_use]
pub fn read(path: &Path) -> Option<Descriptor> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Descriptor>(&text)
        .ok()
        .filter(|descriptor| descriptor.schema_version == SCHEMA_VERSION)
}

/// Every descriptor in the directory, with its project key.
#[must_use]
pub fn all(directory: &Path) -> Vec<(String, Descriptor)> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut found = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                return None;
            }
            let key = path.file_stem()?.to_str()?.to_owned();
            read(&path).map(|descriptor| (key, descriptor))
        })
        .collect::<Vec<_>>();
    found.sort_by_key(|(_, descriptor)| descriptor.started_at_unix_ms);
    found
}

/// Remove the descriptor, but only while it is still this process's.
pub fn remove_if_owned(directory: &Path, key: &str, pid: u32) {
    let path = descriptor_path(directory, key);
    if read(&path).is_some_and(|descriptor| descriptor.pid == pid) {
        let _ = std::fs::remove_file(path);
    }
}

/// Whether a process id is still alive.
///
/// Used only to decide whether a worker descriptor is stale. It is deliberately
/// conservative: an unknown answer is treated as "alive", because removing the
/// descriptor of a running worker would leave its agents unreachable.
#[must_use]
pub fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // This process is necessarily alive, even when the host policy prevents
    // querying the process table (as it does in some Windows sandboxes).
    if pid == std::process::id() {
        return true;
    }
    #[cfg(windows)]
    {
        // `tasklist` is the portable check on Windows. A missing tool, access
        // denial, failed command, or unrecognized output is unknown and must
        // answer "alive": a descriptor removed on an unknown result could be
        // a live worker's.
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output();
        output
            .ok()
            .and_then(|output| {
                tasklist_pid_status(
                    pid,
                    output.status.success(),
                    &String::from_utf8_lossy(&output.stdout),
                    &String::from_utf8_lossy(&output.stderr),
                )
            })
            .unwrap_or(true)
    }
    #[cfg(unix)]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
        true
    }
}

#[cfg(windows)]
fn tasklist_pid_status(pid: u32, success: bool, stdout: &str, stderr: &str) -> Option<bool> {
    if !success || !stderr.trim().is_empty() {
        return None;
    }
    for line in stdout.lines() {
        // CSV rows start with the image name and then the PID. Compare the
        // complete second field, not a substring that could confuse PID 12
        // with PID 123.
        if let Some((_, rest)) = line
            .trim()
            .strip_prefix('"')
            .and_then(|line| line.split_once("\",\""))
            && let Some((reported_pid, _)) = rest.split_once("\",\"")
            && reported_pid.parse::<u32>().ok() == Some(pid)
        {
            return Some(true);
        }
    }
    let lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    (!lines.is_empty()
        && lines
            .iter()
            .all(|line| line.to_ascii_uppercase().starts_with("INFO:")))
    .then_some(false)
}

#[cfg(all(test, windows))]
mod process_probe_tests {
    use super::tasklist_pid_status;

    #[test]
    fn exact_csv_pid_is_alive_and_nonmatching_substrings_are_not_matches() {
        assert_eq!(
            tasklist_pid_status(
                123,
                true,
                "\"ha.exe\",\"123\",\"Console\",\"1\",\"10,000 K\"",
                ""
            ),
            Some(true)
        );
        assert_eq!(
            tasklist_pid_status(
                12,
                true,
                "\"ha.exe\",\"123\",\"Console\",\"1\",\"10,000 K\"",
                ""
            ),
            None
        );
    }

    #[test]
    fn only_an_explicit_no_match_is_dead_and_unknown_results_stay_unknown() {
        assert_eq!(
            tasklist_pid_status(
                123,
                true,
                "INFO: No tasks are running which match the specified criteria.",
                ""
            ),
            Some(false)
        );
        assert_eq!(
            tasklist_pid_status(123, true, "ERROR: Access denied", ""),
            None
        );
        assert_eq!(tasklist_pid_status(123, true, "", "access denied"), None);
        assert_eq!(tasklist_pid_status(123, true, "", ""), None);
        assert_eq!(tasklist_pid_status(123, false, "", ""), None);
    }
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// The data directory is under the user's profile, which Windows already keeps
/// from other users.
#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps, reason = "the Unix version can fail")]
const fn restrict_to_owner(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Descriptor, SCHEMA_VERSION, all, project_key, read, remove_if_owned, write};

    fn descriptor(pid: u32) -> Descriptor {
        Descriptor {
            schema_version: SCHEMA_VERSION,
            worker_id: "w1".to_owned(),
            pid,
            port: 4242,
            token: "t".to_owned(),
            store_dir: "store".into(),
            project_root: "root".into(),
            started_at_unix_ms: 1,
            build: super::build_identity(),
        }
    }

    #[test]
    fn a_descriptor_is_read_back_and_removed_only_by_its_owner() {
        let directory = tempfile::tempdir().expect("temp dir");
        let key = project_key(std::path::Path::new("some/store"));
        assert_eq!(key.len(), 16);
        write(directory.path(), &key, &descriptor(7)).expect("written");
        let path = super::descriptor_path(directory.path(), &key);
        assert_eq!(read(&path), Some(descriptor(7)));
        assert_eq!(all(directory.path()).len(), 1);
        remove_if_owned(directory.path(), &key, 8);
        assert!(path.is_file(), "another process's descriptor stays");
        remove_if_owned(directory.path(), &key, 7);
        assert!(!path.exists());
    }
}
