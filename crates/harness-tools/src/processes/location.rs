//! Where the last shell call of a conversation left off.
//!
//! Every `run_shell` and `run_process` used to start at the workspace root with
//! the host's `PATH`, so a `cd sub` (or `Set-Location sub`) in one call was gone
//! in the next and the model ran the next command in the wrong place. The Python
//! REPL kept `os.chdir`; the shell did not.
//!
//! The host puts a recording in front of each `run_shell` command, never after
//! it: text appended to a command is read as part of it (an unterminated
//! heredoc or a trailing backslash swallowed it, and a command that exits itself
//! never reached it). The recording runs when the shell exits, however it does
//! (the end of the command, `exit N`, an error that stops it), writes the shell's
//! final directory and `PATH` to a side file the host chose (never to stdout), and
//! leaves the exit status as the command made it. A command that is killed, times
//! out or is canceled never runs it, so the location stays as it was.
//!
//! What is kept, per conversation (task):
//! - the directory, only when it is inside the workspace root once resolved
//!   (a link out of it does not count);
//! - `PATH`, only when it differs from the host's.
//!
//! No other variable is kept: an environment carries secrets. `HA_SHELL_KEEP_LOCATION=off`
//! restores the old behavior: every call starts at the root.
//!
//! The calls of one conversation take turns: a call reads its start place and
//! stores its end place under the conversation's lock ([`Places::serialize`]), so
//! two calls that run together never start from a place the other is about to
//! replace.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// The switch: `off` (also `0` or `false`) starts every call at the root again.
pub(crate) const SWITCH: &str = "HA_SHELL_KEEP_LOCATION";

/// Whether the switch's value leaves the location kept (the default).
pub(crate) fn enabled_by(value: Option<&str>) -> bool {
    value.is_none_or(|value| {
        !["off", "0", "false"]
            .iter()
            .any(|off| value.trim().eq_ignore_ascii_case(off))
    })
}

/// `HA_SHELL_KEEP_LOCATION`.
pub(crate) fn enabled() -> bool {
    enabled_by(std::env::var(SWITCH).ok().as_deref())
}

/// How many conversations' places are kept at most; past it the oldest-first
/// order is not tracked, so the map starts over rather than growing.
const KEPT_LIMIT: usize = 256;

/// The shells the host starts for `run_shell`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Shell {
    /// `sh -c`.
    Sh,
    /// `pwsh` or `powershell.exe`, with `-Command`.
    PowerShell,
}

impl Shell {
    /// The shell `run_shell` starts on this platform.
    pub(crate) const fn native() -> Self {
        if cfg!(windows) {
            Self::PowerShell
        } else {
            Self::Sh
        }
    }
}

/// `command` with the recording in front of it.
///
/// - `sh`: an `EXIT` trap, set before the command, saves `$?`, writes
///   `pwd` and `$PATH` to `side_file` and exits with the saved status. A command
///   that sets its own `EXIT` trap replaces ours: then no location is kept (the
///   old behavior), and its status is its own. A syntax error leaves where the
///   shell started, with the shell's status 2; a heredoc with no end or a
///   trailing backslash are read as before, and the trap still runs.
/// - `pwsh` and `powershell.exe`: a `PowerShell.Exiting` engine event, registered
///   before the command, writes the same. It runs on `exit N` too (checked on
///   PowerShell 7 and 5.1: a `try`/`finally` around the command would also run
///   there, but needs text after it). It reads the location from
///   `$ExecutionContext` and the `PATH` from the process environment, because
///   a cmdlet cannot be loaded once the runspace is closing.
///
/// PowerShell's exit status needs the last statement's `$?`, so a short status
/// line does follow the command, after a blank line (a trailing backtick ends at
/// it). `pwsh -Command` exits 1 for any failure, a native program's own code
/// included; this exits with that code when the command failed, `$LASTEXITCODE` is a
/// non-zero integer, and otherwise 1, and 0 on success. It is never worse than
/// without it: a success is 0, a failure is not, and `exit N` is `N`. It is better
/// when the last thing the command ran was a native program that failed (its own
/// code, not 1). A failing cmdlet after an earlier failing native program also
/// gives that program's code, for it is still the last non-zero one the command
/// saw. When the status line is swallowed (a block comment with no end, which is a
/// parse error anyway) the status is the plain one.
pub(crate) fn wrap(shell: Shell, command: &str, side_file: &Path) -> String {
    let file = side_file.to_string_lossy();
    match shell {
        Shell::Sh => {
            let file = file.replace('\'', "'\''");
            format!(
                "__ha_exit() {{\n__ha_rc=$?\nprintf '%s\n%s\n' \"$(pwd)\" \"$PATH\" > '{file}' 2>/dev/null\nexit $__ha_rc\n}}\ntrap __ha_exit EXIT\n{command}"
            )
        }
        Shell::PowerShell => {
            let file = quoted(&file);
            format!(
                "Register-EngineEvent -SourceIdentifier PowerShell.Exiting -Action {{ try {{ [System.IO.File]::WriteAllText({file}, $ExecutionContext.SessionState.Path.CurrentFileSystemLocation.ProviderPath + \"`n\" + [System.Environment]::GetEnvironmentVariable('PATH')) }} catch {{ }} }} | Out-Null\n{command}\n\n$__ha_ok = $?; $__ha_rc = $LASTEXITCODE\nif ($__ha_ok) {{ exit 0 }} elseif ($__ha_rc -is [int] -and $__ha_rc -ne 0) {{ exit $__ha_rc }} else {{ exit 1 }}\n"
            )
        }
    }
}

/// `text` as a PowerShell single-quoted string. PowerShell also reads the four
/// typographic single quotes as quotes, so each is doubled too.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for character in text.chars() {
        out.push(character);
        if matches!(
            character,
            '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}'
        ) {
            out.push(character);
        }
    }
    out.push('\'');
    out
}

/// What a shell left in the side file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct End {
    pub dir: String,
    pub path: String,
}

/// Read the side file text: the directory on the first line, `PATH` after it.
pub(crate) fn parse_end(text: &str) -> Option<End> {
    let (dir, path) = text.split_once('\n')?;
    let dir = dir.trim_end_matches('\r');
    if dir.is_empty() {
        return None;
    }
    Some(End {
        dir: dir.to_owned(),
        path: path.trim_end_matches(['\r', '\n']).to_owned(),
    })
}

/// The file `name` names in `path` (a `PATH` value: the host's own), as an
/// absolute path, so a shell can be started by it whatever `PATH` the command
/// that runs in it has. Remembered per name and `PATH`, as the choice of shell
/// is: a lookup is a stat per directory. `None` when no directory holds it.
pub(crate) fn absolute_program(name: &str, path: Option<&str>) -> Option<PathBuf> {
    type Found = Mutex<HashMap<(String, String), Option<PathBuf>>>;
    static FOUND: OnceLock<Found> = OnceLock::new();
    let path = path?;
    let cache = FOUND.get_or_init(Mutex::default);
    let key = (name.to_owned(), path.to_owned());
    if let Some(found) = cache.lock().ok().and_then(|cache| cache.get(&key).cloned()) {
        return found;
    }
    let found = std::env::split_paths(path)
        .filter(|directory| directory.is_absolute())
        .flat_map(|directory| {
            let plain = directory.join(name);
            // `CreateProcess` adds `.exe` to a bare name.
            let exe = (cfg!(windows) && Path::new(name).extension().is_none())
                .then(|| directory.join(format!("{name}.exe")));
            [Some(plain), exe].into_iter().flatten()
        })
        .find(|candidate| candidate.is_file());
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= 64 {
            cache.clear();
        }
        cache.insert(key, found.clone());
    }
    found
}

/// Whether a program the model started by a bare name is in `path`, the `PATH`
/// a shell call kept (relative entries are read from `cwd`, where the program
/// starts). A name with a directory in it, or any name when no shell call kept a
/// `PATH`, is not asked about. Without this a program was found through the
/// host's `PATH` where the kept one did not hold it: Windows looks a program
/// up in the parent's `PATH` too, so `git` ran after a call that had set `PATH`
/// to one unrelated folder, where a shell with that `PATH` says it is not found.
pub(crate) fn program_on_path(name: &str, path: &str, cwd: &Path, pathext: Option<&str>) -> bool {
    if name.contains(['/', '\\', ':']) {
        return true;
    }
    let default_pathext = ".COM;.EXE;.BAT;.CMD";
    let suffixes: Vec<String> = if !cfg!(windows) || Path::new(name).extension().is_some() {
        vec![String::new()]
    } else {
        pathext
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(default_pathext)
            .split(';')
            .filter(|suffix| !suffix.is_empty())
            .map(str::to_owned)
            .collect()
    };
    std::env::split_paths(path).any(|directory| {
        let directory = if directory.is_absolute() {
            directory
        } else {
            cwd.join(directory)
        };
        suffixes.iter().any(|suffix| {
            let candidate = directory.join(format!("{name}{suffix}"));
            candidate.is_file() && is_executable(&candidate)
        })
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

/// A new path for a side file in `directory`, unique within this process.
pub(crate) fn side_file_in(directory: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    directory.join(format!(
        "shell-end-{}-{}.txt",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// `reported` resolved, when it is a directory inside `root` (itself resolved):
/// the form every guard and the next call use. A relative path, a missing one,
/// or one that leaves the workspace (a link included) is `None`.
pub(crate) fn confine(root: &Path, reported: &str) -> Option<PathBuf> {
    let reported = Path::new(reported);
    if !reported.is_absolute() {
        return None;
    }
    let resolved = std::fs::canonicalize(reported).ok()?;
    let root = std::fs::canonicalize(root).ok()?;
    (resolved.is_dir() && resolved.starts_with(&root)).then_some(resolved)
}

/// Where a conversation's next call starts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Place {
    /// The directory, when it is not the root.
    pub dir: Option<PathBuf>,
    /// The `PATH`, when it is not the host's.
    pub path: Option<String>,
}

impl Place {
    fn is_default(&self) -> bool {
        self.dir.is_none() && self.path.is_none()
    }
}

/// The places of the conversations of one tool service, and the one-line notes
/// their calls leave for the result.
#[derive(Debug)]
pub(crate) struct Places {
    enabled: bool,
    kept: Mutex<HashMap<String, (PathBuf, Place)>>,
    notes: Mutex<HashMap<String, Vec<String>>>,
    /// One lock per conversation, held from reading a start place to storing
    /// the end place ([`Places::serialize`]).
    locks: Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    /// The directory each call was judged from by the command guard.
    judged: Mutex<HashMap<String, String>>,
}

impl Default for Places {
    /// Kept unless `HA_SHELL_KEEP_LOCATION` says otherwise.
    fn default() -> Self {
        Self::new(enabled())
    }
}

impl Places {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            kept: Mutex::default(),
            notes: Mutex::default(),
            locks: Mutex::default(),
            judged: Mutex::default(),
        }
    }

    /// Whether calls keep and use a location.
    pub(crate) const fn enabled(&self) -> bool {
        self.enabled
    }

    /// The turn of a call of `task`: it is held from reading the start place
    /// ([`Places::start`]) to storing the end place ([`Places::finish`]), so the
    /// calls of one conversation never overlap there. A call used to read its
    /// start place before it had the process permit, and the call that ran first
    /// could store its end place before the second, started from the old one,
    /// stored its own over it. `None` while the switch is off.
    pub(crate) async fn serialize(&self, task: &str) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        if !self.enabled {
            return None;
        }
        let lock = {
            let mut locks = self.locks.lock().ok()?;
            if locks.len() >= KEPT_LIMIT && !locks.contains_key(task) {
                // Only the locks nobody holds or waits for can go.
                locks.retain(|_, lock| std::sync::Arc::strong_count(lock) > 1);
            }
            std::sync::Arc::clone(locks.entry(task.to_owned()).or_default())
        };
        Some(lock.lock_owned().await)
    }

    /// Where the next call of `task` starts in `root`; nothing while the switch
    /// is off. A directory that is gone is dropped.
    pub(crate) fn start(&self, task: &str, root: &Path) -> Place {
        if !self.enabled {
            return Place::default();
        }
        let Ok(mut kept) = self.kept.lock() else {
            return Place::default();
        };
        let Some((kept_root, place)) = kept.get(task) else {
            return Place::default();
        };
        if kept_root != root {
            return Place::default();
        }
        let mut place = place.clone();
        if place.dir.as_deref().is_some_and(|dir| !dir.is_dir()) {
            place.dir = None;
            if place.is_default() {
                kept.remove(task);
            } else {
                kept.insert(task.to_owned(), (root.to_owned(), place.clone()));
            }
        }
        place
    }

    /// Record what a call left: `end` is what its epilogue wrote (`None` when
    /// it never ran), `host_path` the host's own `PATH`. The notes for the
    /// call's result are kept under `call`.
    pub(crate) fn finish(
        &self,
        task: &str,
        call: &str,
        root: &Path,
        started: &Place,
        end: Option<&End>,
        host_path: Option<&str>,
    ) {
        let mut notes = Vec::new();
        if let Some(dir) = &started.dir {
            notes.push(format!(
                "{}{}",
                harness_runtime::SHELL_DIR_PREFIX,
                shown(root, dir)
            ));
        }
        if let (true, Some(end)) = (self.enabled, end) {
            let mut next = started.clone();
            // Outside the workspace (or gone): not kept.
            let (dir, left) = match confine(root, &end.dir) {
                Some(dir) => {
                    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
                    (Some(dir).filter(|dir| *dir != root), None)
                }
                None => (None, Some(end.dir.clone())),
            };
            let moved = dir != started.dir;
            next.dir = dir;
            let path_changed = host_path != Some(end.path.as_str());
            let path = path_changed.then(|| end.path.clone());
            let path_moved = path != started.path;
            next.path = path;
            if let Some(outside) = left {
                notes.push(format!(
                    "the shell ended in {outside}, outside the workspace: the next call starts at the workspace root"
                ));
            } else if moved {
                notes.push(match &next.dir {
                    Some(dir) => format!("the next call starts in {}", shown(root, dir)),
                    None => "the next call starts at the workspace root".to_owned(),
                });
            }
            if path_moved {
                notes.push(if next.path.is_some() {
                    "PATH was changed: the next calls keep it".to_owned()
                } else {
                    "PATH is the host's again".to_owned()
                });
            }
            if let Ok(mut kept) = self.kept.lock() {
                if next.is_default() {
                    kept.remove(task);
                } else {
                    if kept.len() >= KEPT_LIMIT && !kept.contains_key(task) {
                        kept.clear();
                    }
                    kept.insert(task.to_owned(), (root.to_owned(), next));
                }
            }
        }
        if !notes.is_empty()
            && let Ok(mut all) = self.notes.lock()
        {
            if all.len() >= KEPT_LIMIT {
                all.clear();
            }
            all.insert(call.to_owned(), notes);
        }
    }

    /// The directory the host's command guard judged `call` from, before the
    /// call was approved and run. The place can change in between (a call of the
    /// conversation that ran meanwhile), so the dispatch compares it with the
    /// place it reads under the lock ([`Places::take_judged`]).
    pub(crate) fn judged(&self, call: &str, dir: &str) {
        if let Ok(mut judged) = self.judged.lock() {
            if judged.len() >= KEPT_LIMIT {
                judged.clear();
            }
            judged.insert(call.to_owned(), dir.to_owned());
        }
    }

    /// The directory the guard judged `call` from, once; `None` when it was not
    /// judged (no guard, or the call did not pass the guard's gate).
    pub(crate) fn take_judged(&self, call: &str) -> Option<String> {
        self.judged
            .lock()
            .ok()
            .and_then(|mut judged| judged.remove(call))
    }

    /// The notes `call` left, once.
    pub(crate) fn take_notes(&self, call: &str) -> Vec<String> {
        self.notes
            .lock()
            .ok()
            .and_then(|mut all| all.remove(call))
            .unwrap_or_default()
    }
}

/// `dir` as the model reads it: relative to the root, `/` separated.
fn shown(root: &Path, dir: &Path) -> String {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
    dir.strip_prefix(&root)
        .map_or_else(
            |_| dir.to_string_lossy(),
            |relative| relative.to_string_lossy(),
        )
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp root")
    }

    #[test]
    fn the_switch_is_on_unless_off() {
        assert!(enabled_by(None));
        assert!(enabled_by(Some("on")));
        for off in ["off", "OFF", "0", "false", " off "] {
            assert!(!enabled_by(Some(off)), "{off}");
        }
    }

    #[test]
    fn the_side_file_text_is_a_directory_and_a_path() {
        assert_eq!(
            parse_end("/w/sub\n/bin:/usr/bin\n"),
            Some(End {
                dir: "/w/sub".to_owned(),
                path: "/bin:/usr/bin".to_owned()
            })
        );
        assert_eq!(
            parse_end("C:\\w\\sub\r\nC:\\bin\r\n").map(|end| end.dir),
            Some("C:\\w\\sub".to_owned())
        );
        assert_eq!(parse_end(""), None);
        assert_eq!(parse_end("\n/bin"), None);
        assert_eq!(parse_end("no newline"), None);
    }

    #[test]
    fn only_a_directory_inside_the_root_is_kept() {
        let root = temp();
        let inside = root.path().join("sub");
        std::fs::create_dir(&inside).expect("sub");
        let outside = temp();
        let text = |path: &Path| path.to_string_lossy().into_owned();
        assert_eq!(
            confine(root.path(), &text(&inside)),
            std::fs::canonicalize(&inside).ok()
        );
        assert!(confine(root.path(), &text(outside.path())).is_none());
        assert!(confine(root.path(), "sub").is_none(), "relative");
        assert!(confine(root.path(), &text(&root.path().join("missing"))).is_none());
        // Up and out of the root by `..` is outside once resolved.
        assert!(confine(root.path(), &text(&inside.join("..").join(".."))).is_none());
    }

    fn end(dir: &Path, path: &str) -> End {
        End {
            dir: dir.to_string_lossy().into_owned(),
            path: path.to_owned(),
        }
    }

    #[test]
    fn a_directory_inside_is_kept_and_the_next_call_is_told() {
        let root = temp();
        let sub = root.path().join("sub");
        std::fs::create_dir(&sub).expect("sub");
        let places = Places::default();
        let started = places.start("t", root.path());
        assert_eq!(started, Place::default());
        places.finish(
            "t",
            "c1",
            root.path(),
            &started,
            Some(&end(&sub, "/h")),
            Some("/h"),
        );
        assert_eq!(
            places.take_notes("c1"),
            ["the next call starts in sub".to_owned()]
        );
        assert!(places.take_notes("c1").is_empty(), "taken once");
        let next = places.start("t", root.path());
        assert_eq!(next.dir, std::fs::canonicalize(&sub).ok());
        assert_eq!(next.path, None);
        // The conversation's own: another one starts at the root.
        assert_eq!(places.start("other", root.path()), Place::default());
        // Back at the root, the place is forgotten.
        places.finish(
            "t",
            "c2",
            root.path(),
            &next,
            Some(&end(root.path(), "/h")),
            Some("/h"),
        );
        assert_eq!(
            places.take_notes("c2"),
            [
                "ran in sub".to_owned(),
                "the next call starts at the workspace root".to_owned()
            ]
        );
        assert_eq!(places.start("t", root.path()), Place::default());
    }

    #[test]
    fn a_directory_outside_is_not_kept_and_the_result_says_so() {
        let root = temp();
        let outside = temp();
        let places = Places::default();
        places.finish(
            "t",
            "c1",
            root.path(),
            &Place::default(),
            Some(&end(outside.path(), "/h")),
            Some("/h"),
        );
        let notes = places.take_notes("c1");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("outside the workspace") && notes[0].contains("workspace root"),
            "{notes:?}"
        );
        assert_eq!(places.start("t", root.path()), Place::default());
    }

    #[test]
    fn a_path_is_kept_only_when_it_differs_from_the_hosts() {
        let root = temp();
        let places = Places::default();
        places.finish(
            "t",
            "c1",
            root.path(),
            &Place::default(),
            Some(&end(root.path(), "/extra:/h")),
            Some("/h"),
        );
        assert_eq!(
            places.take_notes("c1"),
            ["PATH was changed: the next calls keep it".to_owned()]
        );
        let started = places.start("t", root.path());
        assert_eq!(started.path.as_deref(), Some("/extra:/h"));
        assert_eq!(started.dir, None);
        places.finish(
            "t",
            "c2",
            root.path(),
            &started,
            Some(&end(root.path(), "/h")),
            Some("/h"),
        );
        assert_eq!(
            places.take_notes("c2"),
            ["PATH is the host's again".to_owned()]
        );
        assert_eq!(places.start("t", root.path()), Place::default());
    }

    #[test]
    fn a_call_that_never_reached_its_epilogue_leaves_the_place_as_it_was() {
        let root = temp();
        let sub = root.path().join("sub");
        std::fs::create_dir(&sub).expect("sub");
        let places = Places::default();
        places.finish(
            "t",
            "c1",
            root.path(),
            &Place::default(),
            Some(&end(&sub, "/h")),
            Some("/h"),
        );
        let started = places.start("t", root.path());
        places.finish("t", "c2", root.path(), &started, None, Some("/h"));
        assert_eq!(places.take_notes("c2"), ["ran in sub".to_owned()]);
        assert_eq!(places.start("t", root.path()), started);
    }

    #[test]
    fn a_kept_directory_that_is_gone_is_dropped() {
        let root = temp();
        let sub = root.path().join("sub");
        std::fs::create_dir(&sub).expect("sub");
        let places = Places::default();
        places.finish(
            "t",
            "c1",
            root.path(),
            &Place::default(),
            Some(&end(&sub, "/h")),
            Some("/h"),
        );
        std::fs::remove_dir(&sub).expect("removed");
        assert_eq!(places.start("t", root.path()), Place::default());
    }

    // The recording is run by each shell this machine has: a shell that is not
    // installed is skipped, as the process tests skip theirs.

    fn available(program: &str, probe: &[&str]) -> bool {
        std::process::Command::new(program)
            .args(probe)
            .output()
            .is_ok_and(|output| output.status.success())
    }

    /// The shells this machine has, with how each is started on a command.
    fn shells() -> Vec<(&'static str, Shell, Vec<&'static str>)> {
        let mut found = Vec::new();
        if available("sh", &["-c", "exit 0"]) {
            found.push(("sh", Shell::Sh, vec!["-c"]));
        }
        for name in ["pwsh", "powershell.exe"] {
            if available(
                name,
                &["-NoProfile", "-NonInteractive", "-Command", "exit 0"],
            ) {
                found.push((
                    name,
                    Shell::PowerShell,
                    vec!["-NoProfile", "-NonInteractive", "-Command"],
                ));
            }
        }
        found
    }

    fn run(program: &str, flags: &[&str], command: &str, dir: &Path) -> Option<i32> {
        std::process::Command::new(program)
            .args(flags)
            .arg(command)
            .current_dir(dir)
            .output()
            .expect("shell starts")
            .status
            .code()
    }

    /// Run `command` wrapped, with a fresh side file: its exit code, and what
    /// the shell left in the file when it left one.
    fn wrapped(
        program: &str,
        flags: &[&str],
        shell: Shell,
        command: &str,
        dir: &Path,
    ) -> (Option<i32>, Option<End>) {
        let side = side_file_in(dir);
        let code = run(program, flags, &wrap(shell, command, &side), dir);
        let end = std::fs::read_to_string(&side)
            .ok()
            .and_then(|text| parse_end(&text));
        std::fs::remove_file(&side).ok();
        (code, end)
    }

    /// Whether the directory a shell wrote is `expected`. A Git for Windows `sh`
    /// writes its own kind of path (`/tmp/..`): the host only runs `sh` where it
    /// is the native shell, so there only the tail is compared.
    fn is_dir(end: &End, shell: Shell, root: &Path, expected: &Path) -> bool {
        if cfg!(windows) && shell == Shell::Sh {
            let tail = expected
                .file_name()
                .map_or(String::new(), |name| name.to_string_lossy().into_owned());
            end.dir.ends_with(&format!("/{tail}"))
        } else {
            confine(root, &end.dir) == std::fs::canonicalize(expected).ok()
        }
    }

    /// For `sh` the recording leaves the status as it was: each command exits
    /// as it does without it, a heredoc with no end, a trailing backslash and a
    /// syntax error included.
    #[test]
    fn the_recording_keeps_the_exit_status_of_sh() {
        let temp = temp();
        for (program, shell, flags) in shells() {
            if shell != Shell::Sh {
                continue;
            }
            for command in [
                "true",
                "false",
                "exit 3",
                "(exit 4)",
                "f() { return 5; }; f",
                "echo hi",
                "echo hi # a comment",
                "set -e; false; echo no",
                "echo hi \\",
                "cat <<EOF\nunterminated",
                "if",
            ] {
                let plain = run(program, &flags, command, temp.path());
                let (code, _) = wrapped(program, &flags, shell, command, temp.path());
                assert_eq!(code, plain, "{program}: {command}");
            }
        }
    }

    /// Measured: `pwsh -Command` and `powershell.exe -Command` exit 0 when the
    /// last statement succeeded, 1 when it failed (a native program's own code
    /// is lost), and N for `exit N`. The recording exits with the same, except
    /// that a native program's code survives where it is the one that failed:
    /// better, never worse (a success stays 0, a failure stays non-zero).
    #[test]
    fn the_recording_keeps_the_exit_status_of_powershell_and_keeps_a_native_code() {
        let temp = temp();
        for (program, shell, flags) in shells() {
            if shell != Shell::PowerShell {
                continue;
            }
            let native = format!("& {program} -NoProfile -NonInteractive -Command 'exit 5'");
            // (command, plain, wrapped)
            let table: Vec<(String, Option<i32>, Option<i32>)> = vec![
                ("$null".into(), Some(0), Some(0)),
                ("Get-Item zz-missing-zz".into(), Some(1), Some(1)),
                ("exit 3".into(), Some(3), Some(3)),
                ("Write-Output hi".into(), Some(0), Some(0)),
                ("Write-Output hi # a comment".into(), Some(0), Some(0)),
                (
                    "Get-Item zz-missing-zz; Write-Output hi".into(),
                    Some(0),
                    Some(0),
                ),
                // Better: the native program's code, where plain says 1.
                (native.clone(), Some(1), Some(5)),
                (format!("{native}; exit 6"), Some(6), Some(6)),
                // A native failure and then a success is a success.
                (format!("{native}; Write-Output hi"), Some(0), Some(0)),
                // The last statement failed and the last native program's code is
                // 5: both fail, and the code says which program did.
                (
                    format!("{native}; Get-Item zz-missing-zz"),
                    Some(1),
                    Some(5),
                ),
                ("if (".into(), Some(1), Some(1)),
                (
                    "$ErrorActionPreference = 'Stop'; Get-Item zz-missing-zz; Write-Output after"
                        .into(),
                    Some(1),
                    Some(1),
                ),
            ];
            for (command, plain, expected) in table {
                assert_eq!(
                    run(program, &flags, &command, temp.path()),
                    plain,
                    "{program}: plain {command}"
                );
                let (code, _) = wrapped(program, &flags, shell, &command, temp.path());
                assert_eq!(code, expected, "{program}: wrapped {command}");
                // Never worse.
                assert_eq!(code == Some(0), plain == Some(0), "{program}: {command}");
            }
        }
    }

    /// (command, where it ended: `Some(true)` in sub, `Some(false)` where it
    /// started, `None` no file; the exit code, unchecked for `None`).
    type Case<'a> = (&'a str, Option<bool>, Option<Option<i32>>);

    /// Where each shell says it ended, for each way a command can end.
    /// - A command that exits itself (`exit 3`) now records, with its status.
    /// - `sh`: a heredoc with no end and a trailing backslash record; a syntax
    ///   error records where the shell started (nothing ran), status 2.
    /// - PowerShell: a trailing backtick and a thrown error record; a command
    ///   that does not parse never runs the recording: no file, the place stays.
    #[test]
    fn the_recording_writes_where_the_shell_ended_however_it_ended() {
        let temp = temp();
        let sub = temp.path().join("sub");
        std::fs::create_dir(&sub).expect("sub");
        for (program, shell, flags) in shells() {
            let cases: Vec<Case> = match shell {
                Shell::Sh => vec![
                    ("cd sub", Some(true), Some(Some(0))),
                    ("cd sub; exit 3", Some(true), Some(Some(3))),
                    ("cd sub; (exit 4)", Some(true), Some(Some(4))),
                    ("cd sub; false", Some(true), Some(Some(1))),
                    ("cd sub; set -e; false; echo no", Some(true), Some(Some(1))),
                    ("cd sub; cat <<EOF\nunterminated", Some(true), Some(Some(0))),
                    ("cd sub; echo hi \\", Some(true), Some(Some(0))),
                    ("cd sub; if", Some(false), Some(Some(2))),
                    // The command's own EXIT trap replaces ours: today's behavior.
                    ("cd sub; trap 'echo mine' EXIT", None, Some(Some(0))),
                    // Killed: nothing runs.
                    ("cd sub; kill -9 $$", None, None),
                ],
                Shell::PowerShell => vec![
                    ("Set-Location sub", Some(true), Some(Some(0))),
                    ("Set-Location sub; exit 3", Some(true), Some(Some(3))),
                    (
                        "Set-Location sub; Get-Item zz-missing-zz",
                        Some(true),
                        Some(Some(1)),
                    ),
                    ("Set-Location sub; throw 'x'", Some(true), Some(Some(1))),
                    (
                        "Set-Location sub; Write-Output hi `",
                        Some(true),
                        Some(Some(0)),
                    ),
                    ("Set-Location sub; if (", None, Some(Some(1))),
                    // Killed: nothing runs.
                    ("Set-Location sub; Stop-Process -Id $PID -Force", None, None),
                ],
            };
            for (command, ended, status) in cases {
                let (code, end) = wrapped(program, &flags, shell, command, temp.path());
                if let Some(status) = status {
                    assert_eq!(code, status, "{program}: status of {command}");
                }
                match (ended, end) {
                    (None, None) => {}
                    (Some(in_sub), Some(end)) => {
                        let expected = if in_sub { &sub } else { temp.path() };
                        assert!(
                            is_dir(&end, shell, temp.path(), expected),
                            "{program}: {command}: ended in {}",
                            end.dir
                        );
                        assert!(!end.path.is_empty(), "{program}: {command}");
                    }
                    (ended, end) => panic!("{program}: {command}: {ended:?} but {end:?}"),
                }
            }
        }
    }

    #[test]
    fn a_side_file_name_with_a_quote_is_quoted() {
        let temp = temp();
        for name in ["it's here.txt", "it\u{2019}s here.txt"] {
            let side = temp.path().join(name);
            for (program, shell, flags) in shells() {
                if cfg!(windows) && shell == Shell::Sh {
                    // Git for Windows' `sh` reads a single quote in its command
                    // line by its own rules; the host never runs `sh` there.
                    continue;
                }
                run(program, &flags, &wrap(shell, "echo hi", &side), temp.path());
                assert!(side.exists(), "{program}: {name}");
                std::fs::remove_file(&side).ok();
            }
        }
    }

    /// A program is found by its bare name in the directories of a `PATH`, and
    /// only there; a name with a directory is not asked about.
    #[test]
    fn a_program_is_looked_up_in_the_path_it_is_asked_about() {
        let temp = temp();
        let bin = temp.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        let file = bin.join(if cfg!(windows) {
            "zz-prog.exe"
        } else {
            "zz-prog"
        });
        std::fs::write(&file, "x").expect("program");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))
                .expect("executable");
        }
        let joined = |dirs: &[&Path]| {
            std::env::join_paths(dirs.iter().copied())
                .expect("a path")
                .to_string_lossy()
                .into_owned()
        };
        let with = joined(&[temp.path(), &bin]);
        let without = joined(&[temp.path()]);
        assert!(program_on_path("zz-prog", &with, temp.path(), None));
        assert!(!program_on_path("zz-prog", &without, temp.path(), None));
        assert!(!program_on_path("zz-prog", "", temp.path(), None));
        assert!(program_on_path("sub/zz-prog", &without, temp.path(), None));
        assert!(
            program_on_path("zz-prog", "bin", temp.path(), None),
            "relative entry"
        );
        assert_eq!(absolute_program("zz-prog", Some(&with)), Some(file.clone()));
        assert_eq!(absolute_program("zz-prog", Some(&without)), None);
        assert_eq!(absolute_program("zz-prog", None), None);
    }

    #[test]
    fn a_text_is_quoted_for_powershell_with_its_typographic_quotes_doubled() {
        assert_eq!(quoted("a'b"), "'a''b'");
        assert_eq!(quoted("a\u{2019}b"), "'a\u{2019}\u{2019}b'");
    }

    /// Two calls of one conversation that run together take turns: the one that
    /// has the lock reads and stores its place before the other reads.
    #[tokio::test]
    async fn the_calls_of_one_conversation_take_turns_and_others_do_not_wait() {
        let places = std::sync::Arc::new(Places::default());
        let first = places.serialize("t").await.expect("a turn");
        let waiting = {
            let places = std::sync::Arc::clone(&places);
            tokio::spawn(async move { places.serialize("t").await.is_some() })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !waiting.is_finished(),
            "the second call waits for the first"
        );
        // Another conversation does not.
        assert!(places.serialize("other").await.is_some());
        drop(first);
        assert!(waiting.await.expect("joined"));
        assert!(Places::new(false).serialize("t").await.is_none());
    }
}
