//! What a resource resolution read from disk, so its result can be reused
//! until one of those reads would answer differently.
//!
//! The session resolves its resources on every skills discovery and menu
//! refresh, and a resolution walks every skill, prompt and package tree. Its
//! result is a function of what it read: the settings and manifest files, the
//! listings of the directories it walked and the paths it probed for. Each of
//! those is recorded with its stamp (length and modification time), and the
//! result stays valid while every recorded path still has the same stamp. A
//! directory's modification time changes when an entry is added, removed or
//! renamed in it, but a clock too coarse for two changes close together (or a
//! file system that updates it late) can leave it unchanged, so a directory's
//! stamp also holds a digest of its entry names; a file found by a listing
//! needs no stamp of its own, because only its presence counts.
//!
//! Recording is per thread and only while [`record`] runs, so the package
//! commands that use the same code outside a session pay nothing.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

thread_local! {
    static RECORDING: RefCell<Option<Vec<(PathBuf, Seen)>>> = const { RefCell::new(None) };
}

/// What a path looked like: absent, or its kind, length and modification time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    is_dir: bool,
    len: u64,
    modified: Option<SystemTime>,
    /// For a directory, a digest of its entry names (0 for a file).
    names: u64,
}

/// What a resolution saw of one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seen {
    /// Whether it existed: all a probe like `.git` depends on, whose stamp
    /// changes with every git command.
    Presence(bool),
    /// Its stamp, or `None` when it was absent.
    Stamp(Option<Stamp>),
}

impl Seen {
    fn now(path: &Path, presence_only: bool) -> Self {
        if presence_only {
            Self::Presence(path.exists())
        } else {
            Self::Stamp(stamp(path))
        }
    }
}

fn stamp(path: &Path) -> Option<Stamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(Stamp {
        is_dir: metadata.is_dir(),
        len: metadata.len(),
        modified: metadata.modified().ok(),
        names: if metadata.is_dir() {
            names_digest(path)
        } else {
            0
        },
    })
}

/// A digest of the entry names in a directory that does not depend on the
/// order the file system lists them in.
fn names_digest(dir: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            entry.file_name().hash(&mut hasher);
            hasher.finish()
        })
        .fold(0_u64, u64::wrapping_add)
}

/// Note that the running resolution's result depends on `path`: its listing
/// when it is a directory that is read, its content when it is a file that is
/// read, its existence otherwise. A no-op outside [`record`].
pub(crate) fn note(path: &Path) {
    push(path, false);
}

/// Note that the running resolution only asked whether `path` exists.
pub(crate) fn note_presence(path: &Path) {
    push(path, true);
}

fn push(path: &Path, presence_only: bool) {
    RECORDING.with(|recording| {
        if let Some(paths) = recording.borrow_mut().as_mut() {
            // Stamped now, not at the end: a change made while the resolution
            // runs then shows up as a mismatch on the next check.
            paths.push((path.to_path_buf(), Seen::now(path, presence_only)));
        }
    });
}

/// The paths a resolution depended on, with their stamps at the time.
#[derive(Clone, Debug, Default)]
pub(crate) struct Inputs(Vec<(PathBuf, Seen)>);

impl Inputs {
    /// True while every recorded path still has the stamp it was read with.
    pub(crate) fn unchanged(&self) -> bool {
        self.0
            .iter()
            .all(|(path, before)| Seen::now(path, matches!(before, Seen::Presence(_))) == *before)
    }
}

/// Run `resolve`, recording the paths it notes.
pub(crate) fn record<T>(resolve: impl FnOnce() -> T) -> (T, Inputs) {
    let outer = RECORDING.with(|recording| recording.borrow_mut().replace(Vec::new()));
    let value = resolve();
    let mut paths = RECORDING
        .with(|recording| std::mem::replace(&mut *recording.borrow_mut(), outer))
        .unwrap_or_default();
    // A walk notes some paths more than once (a directory and its ignore
    // files are probed by every scanner that visits it).
    paths.sort_by(|a, b| a.0.cmp(&b.0));
    paths.dedup_by(|a, b| a.0 == b.0);
    (value, Inputs(paths))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_change_with_a_listing_a_content_or_an_existence() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("settings.json");
        let missing = dir.path().join("absent");
        std::fs::write(&file, "{}").expect("write");
        let ((), inputs) = record(|| {
            note(dir.path());
            note(&file);
            note(&missing);
        });
        assert!(inputs.unchanged());

        std::fs::write(&file, "{\"a\":1}").expect("rewrite");
        assert!(!inputs.unchanged(), "a longer file is a change");

        let ((), inputs) = record(|| note(&missing));
        std::fs::create_dir(&missing).expect("create");
        assert!(!inputs.unchanged(), "a path that appears is a change");

        let ((), inputs) = record(|| note(dir.path()));
        std::fs::write(dir.path().join("new.md"), "").expect("new entry");
        assert!(!inputs.unchanged(), "a new entry changes the listing");

        let ((), inputs) = record(|| note_presence(dir.path()));
        std::fs::write(dir.path().join("other.md"), "").expect("other entry");
        assert!(inputs.unchanged(), "a presence probe ignores the listing");
    }

    #[test]
    fn nothing_is_noted_outside_a_recording() {
        note(Path::new("."));
        let ((), inputs) = record(|| {});
        assert!(inputs.0.is_empty());
    }
}
