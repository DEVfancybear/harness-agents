//! The host's own file plumbing, shared by every writer that keeps a file of
//! its own: a lock file held across processes, and a write that replaces a
//! file whole or not at all.
//!
//! - [`hold`] opens a lock file (its directory created, the file never
//!   truncated) and waits for it until a deadline; [`open_lock`] and
//!   [`try_take`] are its two halves, for a caller that waits its own way (an
//!   async one, which sleeps on the runtime).
//! - [`write_atomic`] writes a uniquely named file beside the target, syncs it
//!   and renames it into place: a reader sees the old file or the new one,
//!   never half of either, and a failed write leaves the old file whole.

use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How often a waiting [`hold`] tries the lock again.
const POLL: Duration = Duration::from_millis(5);

/// Distinguishes the temporary files of one process's concurrent writes.
static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// Open the lock file at `path` for [`try_take`], creating it and its
/// directory when they are missing. The file's content is never touched.
///
/// # Errors
/// The directory or the file cannot be created or opened; the message names
/// `what`.
pub fn open_lock(path: &Path, what: &str) -> Result<File, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{what}: cannot create {}: {error}", parent.display()))?;
    }
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| format!("{what}: cannot open the lock {}: {error}", path.display()))
}

/// Try once to take the lock `file` stands for: `true` when this handle holds
/// it now, `false` when another holds it.
///
/// # Errors
/// The lock cannot be taken for another reason than its being held.
pub fn try_take(file: &File, what: &str) -> Result<bool, String> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(error)) => Err(format!("{what}: lock: {error}")),
    }
}

/// Hold the lock file at `path`, waiting up to `wait` while another process
/// (or another handle in this one) holds it. The lock is released when the
/// returned file is dropped.
///
/// # Errors
/// The lock cannot be opened or taken, or another holder kept it past `wait`;
/// the message names `what`.
pub fn hold(path: &Path, wait: Duration, what: &str) -> Result<File, String> {
    let file = open_lock(path, what)?;
    let deadline = Instant::now() + wait;
    loop {
        if try_take(&file, what)? {
            return Ok(file);
        }
        if Instant::now() >= deadline {
            return Err(format!("waiting for another {what} timed out"));
        }
        std::thread::sleep(POLL);
    }
}

/// Replace `path` with `text`, whole or not at all: see [`write_atomic_checked`].
///
/// # Errors
/// The directory, the temporary file or the rename fails.
pub fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    write_atomic_checked(path, text, || Ok(()))
}

/// Replace `path` with `text`, whole or not at all: a uniquely named file in
/// the same directory is created (never an existing one), written, synced,
/// then renamed over `path`, and the directory is synced where the platform
/// allows it. `check` runs just before the rename, when the new text is safe
/// on disk: an error from it leaves `path` as it was. The temporary file is
/// removed on any error.
///
/// # Errors
/// The directory, the temporary file, `check` or the rename fails.
pub fn write_atomic_checked(
    path: &Path,
    text: &str,
    check: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let temporary = parent.join(format!(
        ".{name}.{}-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("cannot create {}: {error}", temporary.display()))?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
        drop(file);
        check()?;
        std::fs::rename(&temporary, path)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Sync a directory, so a rename into it survives a crash. Only Unix can open
/// a directory for that; elsewhere it is nothing.
#[cfg_attr(
    not(unix),
    allow(
        clippy::unnecessary_wraps,
        reason = "Directory fsync is fallible on Unix"
    )
)]
fn sync_directory(directory: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|error| format!("cannot sync {}: {error}", directory.display()))?;
    }
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_lock_waits_and_times_out_naming_what_waits() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("deep/x.lock");
        let first = hold(&path, Duration::from_secs(1), "test write").expect("first");
        let error = hold(&path, Duration::from_millis(30), "test write").expect_err("held");
        assert_eq!(error, "waiting for another test write timed out");
        drop(first);
        hold(&path, Duration::from_millis(30), "test write").expect("free again");
    }

    #[test]
    fn a_lock_file_is_never_truncated() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("x.lock");
        std::fs::write(&path, "kept").expect("seed");
        drop(hold(&path, Duration::from_secs(1), "test").expect("hold"));
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "kept");
    }

    #[test]
    fn a_write_replaces_the_file_and_leaves_no_temporary() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("sub/note.md");
        write_atomic(&path, "one").expect("one");
        write_atomic(&path, "two").expect("two");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "two");
        let names = std::fs::read_dir(dir.path().join("sub"))
            .expect("list")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["note.md"]);
    }

    #[test]
    fn a_failed_write_leaves_the_old_file_whole() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("note.md");
        std::fs::write(&path, "the old text\n").expect("seed");
        let error = write_atomic_checked(&path, "the new", || Err("refused".to_owned()))
            .expect_err("refused");
        assert_eq!(error, "refused");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "the old text\n"
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("list").count(),
            1,
            "the temporary file is gone"
        );
    }
}
