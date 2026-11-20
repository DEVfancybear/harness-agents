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

use harness_providers::CancellationToken;
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

/// `command` with the recording in front of it, on the command's own first line
/// so the line numbers of every message about the command stay the model's.
///
/// - `sh`: an `EXIT` trap, set before the command, saves `$?`, writes
///   `pwd` and `$PATH` to `side_file`, and exits with the saved status. The trap
///   starts with `set +eu`: a command that enabled `set -e` or `set -u` must not
///   have the write (or an unset `PATH`) change its status. A status above 128
///   that names a signal is a program the signal killed (the shell cannot tell
///   it from `exit 143`): after the write the shell re-raises the signal on itself,
///   so the call ends as it did without the trap, by the signal, not by an exit
///   code. A command that sets its own `EXIT` trap replaces ours: then no
///   location is kept (the old behavior), and its status is its own. A syntax
///   error in the first line stops the shell before the trap is set (status 2,
///   nothing recorded, the place unchanged); a heredoc with no end or a trailing
///   backslash are read as before, and the trap still runs.
///   runs.
/// - `pwsh` and `powershell.exe`: a `PowerShell.Exiting` engine event writes the
///   same. `-SupportEvent` keeps its job out of `Get-Job`, so `Get-Job | Wait-Job`
///   waits for the command's jobs only and `Get-Job | Remove-Job` finds nothing
///   of ours. The event runs on `exit N` too (checked on PowerShell 7 and 5.1: a
///   `try`/`finally` around the command would also run there, but needs text
///   after it). It reads the location from `$ExecutionContext` and the `PATH`
///   from the process environment, because a cmdlet cannot be loaded once the
///   runspace is closing. The registration goes after what PowerShell needs at
///   the top of a script (`using` statements and a `param` block, found by their
///   grammar by [`registration_at`]), on the same line, so the command parses
///   as it did.
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
            let file = file.replace('\'', "'\\''");
            format!(
                "__ha_exit() {{ __ha_rc=$?; set +eu; printf '%s\\n%s\\n' \"$(pwd)\" \"$PATH\" > '{file}' 2>/dev/null; if [ \"$__ha_rc\" -gt 128 ]; then __ha_sig=$(kill -l \"$((__ha_rc - 128))\" 2>/dev/null); if [ -n \"$__ha_sig\" ]; then trap - EXIT; kill -s \"$__ha_sig\" $$; fi; fi; exit $__ha_rc; }}; trap __ha_exit EXIT; {command}"
            )
        }
        Shell::PowerShell => {
            let file = quoted(&file);
            let register = format!(
                "Register-EngineEvent -SourceIdentifier PowerShell.Exiting -SupportEvent -Action {{ try {{ [System.IO.File]::WriteAllText({file}, $ExecutionContext.SessionState.Path.CurrentFileSystemLocation.ProviderPath + \"`n\" + [System.Environment]::GetEnvironmentVariable('PATH')) }} catch {{ }} }} | Out-Null"
            );
            let body = match registration_at(command) {
                Some((at, true)) => {
                    format!("{}; {register}{}", &command[..at], &command[at..])
                }
                Some((at, false)) => {
                    format!("{}{register}; {}", &command[..at], &command[at..])
                }
                // It does not parse: nothing runs, so nothing is recorded.
                None => command.to_owned(),
            };
            format!(
                "{body}\n\n$__ha_ok = $?; $__ha_rc = $LASTEXITCODE\nif ($__ha_ok) {{ exit 0 }} elseif ($__ha_rc -is [int] -and $__ha_rc -ne 0) {{ exit $__ha_rc }} else {{ exit 1 }}\n"
            )
        }
    }
}

/// Where the registration goes in a PowerShell command: the byte offset, and
/// whether it follows a leading form (so it is joined to it with `;`) or goes
/// before the first statement. A script may start with `using` statements and a
/// `param` block (with attributes before it), and PowerShell refuses them
/// anywhere else, so the registration goes after the last of them; comments and
/// blank lines (`#requires` among them) stay where they are. `None` when what
/// the grammar puts first does not close (an unclosed block comment or
/// parenthesis): the command does not parse, whatever is in front of it.
fn registration_at(command: &str) -> Option<(usize, bool)> {
    let bytes = command.as_bytes();
    let mut at = 0;
    let mut last_form = None;
    loop {
        at = skip_trivia(bytes, at)?;
        if let Some(end) = using_statement(bytes, at) {
            last_form = Some(end);
            at = end;
        } else if let Some(open) = param_open(bytes, at) {
            let end = closing(bytes, open, b'(', b')')?;
            last_form = Some(end);
            at = end;
        } else {
            break;
        }
        // The `;` that ends a `using` statement, if there is one, stays after the
        // registration.
        if bytes.get(at) == Some(&b';') {
            at += 1;
        }
    }
    Some(match last_form {
        Some(end) => (end, true),
        None => (at, false),
    })
}

/// The offset of the first thing that is not white space or a comment.
fn skip_trivia(bytes: &[u8], mut at: usize) -> Option<usize> {
    loop {
        match bytes.get(at) {
            Some(byte) if byte.is_ascii_whitespace() => at += 1,
            Some(b'<') if bytes.get(at + 1) == Some(&b'#') => {
                let close = bytes[at + 2..].windows(2).position(|pair| pair == b"#>")?;
                at += 2 + close + 2;
            }
            Some(b'#') => {
                while bytes.get(at).is_some_and(|byte| *byte != b'\n') {
                    at += 1;
                }
            }
            _ => return Some(at),
        }
    }
}

/// Whether `word` is at `at`, whole (not the start of a longer name).
fn keyword_at(bytes: &[u8], at: usize, word: &str) -> bool {
    let end = at + word.len();
    bytes
        .get(at..end)
        .is_some_and(|found| found.eq_ignore_ascii_case(word.as_bytes()))
        && bytes.get(end).is_none_or(|next| {
            !(next.is_ascii_alphanumeric() || matches!(next, b'_' | b'-' | b':'))
        })
}

/// The end of a `using ...` statement at `at`, which ends at `;` or the line's.
fn using_statement(bytes: &[u8], at: usize) -> Option<usize> {
    if !keyword_at(bytes, at, "using") {
        return None;
    }
    let mut end = at;
    while bytes
        .get(end)
        .is_some_and(|byte| !matches!(byte, b';' | b'\n' | b'\r'))
    {
        end += 1;
    }
    Some(end)
}

/// The offset of the `(` of a `param(...)` block at `at`, with the attributes
/// before it (`[CmdletBinding()]`); `None` when there is no such block (an
/// attribute that leads an expression is a statement).
fn param_open(bytes: &[u8], at: usize) -> Option<usize> {
    let mut cursor = at;
    while bytes.get(cursor) == Some(&b'[') {
        cursor = closing(bytes, cursor, b'[', b']')?;
        cursor = skip_trivia(bytes, cursor)?;
    }
    if !keyword_at(bytes, cursor, "param") {
        return None;
    }
    let open = skip_trivia(bytes, cursor + "param".len())?;
    (bytes.get(open) == Some(&b'(')).then_some(open)
}

/// The offset just after the bracket that closes the one at `at`, skipping
/// quoted strings and comments. `None` when it never closes.
fn closing(bytes: &[u8], at: usize, open: u8, close: u8) -> Option<usize> {
    let mut depth = 0usize;
    let mut cursor = at;
    while let Some(&byte) = bytes.get(cursor) {
        match byte {
            b'\'' | b'"' => {
                let quote = byte;
                cursor += 1;
                loop {
                    match bytes.get(cursor)? {
                        b'`' if quote == b'"' => cursor += 2,
                        found if *found == quote => {
                            // A doubled quote is one inside the string.
                            if bytes.get(cursor + 1) == Some(&quote) {
                                cursor += 2;
                            } else {
                                cursor += 1;
                                break;
                            }
                        }
                        _ => cursor += 1,
                    }
                }
                continue;
            }
            b'#' => {
                while bytes.get(cursor).is_some_and(|byte| *byte != b'\n') {
                    cursor += 1;
                }
                continue;
            }
            found if found == open => depth += 1,
            found if found == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(cursor + 1);
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    None
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

/// A name nobody can guess: two hashes seeded by the operating system.
fn unguessable() -> String {
    use std::fmt::Write as _;
    use std::hash::{BuildHasher, Hasher};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut name = String::with_capacity(32);
    for _ in 0..2 {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(NEXT.fetch_add(1, Ordering::Relaxed));
        hasher.write_u32(std::process::id());
        let _ = write!(name, "{:016x}", hasher.finish());
    }
    name
}

/// A new path for a side file in `directory`: a name nobody can guess.
pub(crate) fn side_file_in(directory: &Path) -> PathBuf {
    directory.join(format!("shell-end-{}.txt", unguessable()))
}

/// The directory the side files of this process go in: made by this process
/// under a random name, for its user alone (mode 0700 on Unix), and checked to
/// be the directory just made, not a link, owned by the user. The spool it used
/// to share (`/tmp/ha-tool-spool` on Linux) is open to every local user, and a
/// side file there with a name another user could guess was written through a
/// link (`>` follows one) or read after being replaced. `None` when it cannot
/// be made: then no location is tracked.
struct SideDir {
    path: PathBuf,
    /// The owner of the directory just made, which is this user.
    owner: Option<u64>,
}

#[cfg(unix)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "one signature on every platform: elsewhere there is no owner to read"
)]
fn owner_of(meta: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(u64::from(meta.uid()))
}

#[cfg(not(unix))]
fn owner_of(_meta: &std::fs::Metadata) -> Option<u64> {
    None
}

fn side_dir() -> Option<&'static SideDir> {
    static DIR: OnceLock<Option<SideDir>> = OnceLock::new();
    DIR.get_or_init(|| make_side_dir(&std::env::temp_dir()))
        .as_ref()
}

fn make_side_dir(base: &Path) -> Option<SideDir> {
    let path = base.join(format!("ha-shell-{}", unguessable()));
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    // Not recursive: it fails when the name is taken, a link included.
    builder.create(&path).ok()?;
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return None;
        }
    }
    let owner = owner_of(&meta);
    sweep(base, owner);
    Some(SideDir { path, owner })
}

/// Remove the empty side directories of earlier runs of this user, a day old
/// or more: each run makes one.
fn sweep(base: &Path, owner: Option<u64>) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("ha-shell-") {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > std::time::Duration::from_hours(24));
        if meta.is_dir() && !meta.file_type().is_symlink() && old && owner_of(&meta) == owner {
            // Only an empty directory goes.
            let _ = std::fs::remove_dir(entry.path());
        }
    }
}

/// A side file this process may have a shell write: its path, and the owner it
/// must have when read.
pub(crate) struct SideFile {
    path: PathBuf,
    owner: Option<u64>,
}

impl SideFile {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// What the shell wrote, once, and the file is removed. It is read only
    /// when it is a plain file of this user, not a link: anything else is a
    /// place nobody here wrote.
    pub(crate) fn take(&self) -> Option<End> {
        let text = read_private(&self.path, self.owner);
        let _ = std::fs::remove_file(&self.path);
        text.as_deref().and_then(parse_end)
    }
}

/// A new side file in this process's private directory; `None` when there is no
/// such directory.
pub(crate) fn side_file() -> Option<SideFile> {
    let dir = side_dir()?;
    Some(SideFile {
        path: side_file_in(&dir.path),
        owner: dir.owner,
    })
}

/// The text of `path`, when it is a plain file (not a link) of `owner`, and small.
fn read_private(path: &Path, owner: Option<u64>) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 256 * 1024 {
        return None;
    }
    if owner.is_some() && owner_of(&meta) != owner {
        return None;
    }
    std::fs::read_to_string(path).ok()
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

    /// What an approval binds, so one given for a call that starts in one place
    /// does not run it in another: empty at the root with the host's `PATH`.
    pub(crate) fn binding(&self) -> String {
        if self.is_default() {
            return String::new();
        }
        serde_json::json!({
            "dir": self.dir.as_ref().map(|dir| dir.to_string_lossy().into_owned()),
            "path": self.path,
        })
        .to_string()
    }

    /// What the person approving the call is shown when the call does not start
    /// at the root with the host's `PATH`.
    pub(crate) fn summary(&self, root: &Path) -> Option<String> {
        if self.is_default() {
            return None;
        }
        let mut lines = Vec::new();
        if let Some(dir) = &self.dir {
            lines.push(format!(
                "runs in {} (where an earlier shell call left the shell)",
                shown(root, dir)
            ));
        }
        if let Some(path) = &self.path {
            let shown: String = path.chars().take(300).collect();
            let cut = if path.chars().count() > 300 {
                "..."
            } else {
                ""
            };
            lines.push(format!(
                "runs with a PATH that differs from the host's (an earlier shell call set it): {shown}{cut}"
            ));
        }
        Some(lines.join("\n"))
    }
}

/// A map that keeps the most recently used `limit` entries: past it, the least
/// recently used goes, but never one `protected` names.
#[derive(Debug)]
struct Recent<V> {
    entries: HashMap<String, (u64, V)>,
    tick: u64,
    limit: usize,
}

impl<V> Recent<V> {
    fn new(limit: usize) -> Self {
        Self {
            entries: HashMap::new(),
            tick: 0,
            limit,
        }
    }

    fn get(&mut self, key: &str) -> Option<&V> {
        self.tick += 1;
        let tick = self.tick;
        self.entries.get_mut(key).map(|entry| {
            entry.0 = tick;
            &entry.1
        })
    }

    fn remove(&mut self, key: &str) -> Option<V> {
        self.entries.remove(key).map(|(_, value)| value)
    }

    fn insert(&mut self, key: &str, value: V, protected: impl Fn(&str) -> bool) {
        self.tick += 1;
        if !self.entries.contains_key(key) {
            while self.entries.len() >= self.limit {
                let oldest = self
                    .entries
                    .iter()
                    .filter(|(name, _)| !protected(name))
                    .min_by_key(|(_, (stamp, _))| *stamp)
                    .map(|(name, _)| name.clone());
                match oldest {
                    Some(name) => {
                        self.entries.remove(&name);
                    }
                    // Everything left is in use: the map grows past its limit.
                    None => break,
                }
            }
        }
        self.entries.insert(key.to_owned(), (self.tick, value));
    }
}

/// A call's turn among the calls of its conversation ([`Places::take_turn`]).
pub(crate) enum Turn {
    /// It holds the turn (none while the switch is off), and whether it
    /// waited behind another call for it.
    Held(Option<tokio::sync::OwnedMutexGuard<()>>, bool),
    /// It was withdrawn while it waited.
    Canceled,
}

/// The places of the conversations of one tool service, and the one-line notes
/// their calls leave for the result.
#[derive(Debug)]
pub(crate) struct Places {
    enabled: bool,
    kept: Mutex<Recent<(PathBuf, Place)>>,
    notes: Mutex<Recent<Vec<String>>>,
    /// One lock per conversation, held from reading a start place to storing
    /// the end place ([`Places::serialize`]).
    locks: Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    /// The directory each call was judged from by the command guard.
    judged: Mutex<Recent<String>>,
}

impl Default for Places {
    /// Kept unless `HA_SHELL_KEEP_LOCATION` says otherwise.
    fn default() -> Self {
        Self::new(enabled())
    }
}

impl Places {
    pub(crate) fn new(enabled: bool) -> Self {
        Self::with_limit(enabled, KEPT_LIMIT)
    }

    fn with_limit(enabled: bool, limit: usize) -> Self {
        Self {
            enabled,
            kept: Mutex::new(Recent::new(limit)),
            notes: Mutex::new(Recent::new(limit)),
            locks: Mutex::default(),
            judged: Mutex::new(Recent::new(limit)),
        }
    }

    /// Whether the conversation `task` has a call in its turn or waiting for it:
    /// its place is in use and is not evicted.
    fn in_use(&self, task: &str) -> bool {
        self.locks
            .lock()
            .ok()
            .and_then(|locks| locks.get(task).map(std::sync::Arc::strong_count))
            .is_some_and(|holders| holders > 1)
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
    #[cfg(test)]
    pub(crate) async fn serialize(&self, task: &str) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        Some(self.turn_lock(task)?.lock_owned().await)
    }

    /// The lock that orders the calls of `task`; `None` while the switch is off.
    fn turn_lock(&self, task: &str) -> Option<std::sync::Arc<tokio::sync::Mutex<()>>> {
        if !self.enabled {
            return None;
        }
        let mut locks = self.locks.lock().ok()?;
        if locks.len() >= KEPT_LIMIT && !locks.contains_key(task) {
            // Only the locks nobody holds or waits for can go.
            locks.retain(|_, lock| std::sync::Arc::strong_count(lock) > 1);
        }
        Some(std::sync::Arc::clone(
            locks.entry(task.to_owned()).or_default(),
        ))
    }

    /// [`Self::serialize`], withdrawn when `cancellation` fires while the call
    /// waits for its turn. A call that waited behind another of its
    /// conversation was queued, as one waiting for the process permit is: it
    /// waited for the turn and then found the permit free, and was reported
    /// as never queued, and one canceled while it waited ran nothing and
    /// said nothing of it.
    pub(crate) async fn take_turn(&self, task: &str, cancellation: &CancellationToken) -> Turn {
        let Some(lock) = self.turn_lock(task) else {
            return Turn::Held(None, false);
        };
        if let Ok(guard) = std::sync::Arc::clone(&lock).try_lock_owned() {
            return Turn::Held(Some(guard), false);
        }
        tokio::select! {
            guard = lock.lock_owned() => Turn::Held(Some(guard), true),
            () = cancellation.cancelled() => Turn::Canceled,
        }
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
                kept.insert(task, (root.to_owned(), place.clone()), |other| {
                    self.in_use(other)
                });
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
        if self.enabled {
            // Always said, the root included (`.`): the last such line of a
            // result is then the host's, and a line the command printed itself
            // (`ran in 0.4s`) is not taken for the folder.
            notes.push(format!(
                "{}{}",
                harness_runtime::SHELL_DIR_PREFIX,
                started
                    .dir
                    .as_ref()
                    .map_or_else(|| ".".to_owned(), |dir| shown(root, dir))
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
                    kept.insert(task, (root.to_owned(), next), |other| self.in_use(other));
                }
            }
        }
        if !notes.is_empty()
            && let Ok(mut all) = self.notes.lock()
        {
            all.insert(call, notes, |_| false);
        }
    }

    /// The directory the host's command guard judged `call` from, before the
    /// call was approved and run. The place can change in between (a call of the
    /// conversation that ran meanwhile), so the dispatch compares it with the
    /// place it reads under the lock ([`Places::take_judged`]). The entry goes
    /// when the call does, however it ends ([`Places::forget_judged`]).
    pub(crate) fn judged(&self, call: &str, dir: &str) {
        if let Ok(mut judged) = self.judged.lock() {
            judged.insert(call, dir.to_owned(), |_| false);
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

    /// The call ended without reaching the dispatch (the user refused it, a hook
    /// blocked it, it failed first): what the guard judged is no longer wanted.
    pub(crate) fn forget_judged(&self, call: &str) {
        let _ = self.take_judged(call);
    }

    #[cfg(test)]
    pub(crate) fn judged_len(&self) -> usize {
        self.judged.lock().map_or(0, |judged| judged.entries.len())
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

/// `dir` as the model reads it: relative to the root, `/` separated, `.` for
/// the root itself.
fn shown(root: &Path, dir: &Path) -> String {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
    let text = dir
        .strip_prefix(&root)
        .map_or_else(
            |_| dir.to_string_lossy(),
            |relative| relative.to_string_lossy(),
        )
        .replace('\\', "/");
    if text.is_empty() {
        ".".to_owned()
    } else {
        text
    }
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
            [
                "ran in .".to_owned(),
                "the next call starts in sub".to_owned()
            ]
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
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert_eq!(notes[0], "ran in .");
        assert!(
            notes[1].contains("outside the workspace") && notes[1].contains("workspace root"),
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
            [
                "ran in .".to_owned(),
                "PATH was changed: the next calls keep it".to_owned()
            ]
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
            ["ran in .".to_owned(), "PATH is the host's again".to_owned()]
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
    ///   error in the first line stops the shell before the trap is set (status 2,
    ///   no file).
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
                    ("cd sub; if", None, Some(Some(2))),
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

    /// A command's exit code and what it printed.
    fn output_of(
        program: &str,
        flags: &[&str],
        command: &str,
        dir: &Path,
    ) -> (Option<i32>, String) {
        let output = std::process::Command::new(program)
            .args(flags)
            .arg(command)
            .current_dir(dir)
            .output()
            .expect("shell starts");
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout)
                .replace("\r\n", "\n")
                .trim()
                .to_owned(),
        )
    }

    /// `output_of`, wrapped, with the side file read.
    fn wrapped_output(
        program: &str,
        flags: &[&str],
        shell: Shell,
        command: &str,
        dir: &Path,
    ) -> (Option<i32>, String, Option<End>) {
        let side = side_file_in(dir);
        let (code, text) = output_of(program, flags, &wrap(shell, command, &side), dir);
        let end = std::fs::read_to_string(&side)
            .ok()
            .and_then(|text| parse_end(&text));
        std::fs::remove_file(&side).ok();
        (code, text, end)
    }

    /// The registration was an event job `Get-Job` listed: `Get-Job | Wait-Job`
    /// waited for it until the call timed out and `Get-Job | Remove-Job` failed,
    /// where the command alone exits 0. It is a support event now, out of every
    /// list, and still recorded where the shell ended.
    #[test]
    fn the_recording_is_not_a_job_the_command_can_see() {
        let temp = temp();
        let sub = temp.path().join("sub");
        std::fs::create_dir(&sub).expect("sub");
        for (program, shell, flags) in shells() {
            if shell != Shell::PowerShell {
                continue;
            }
            for (command, expected) in [
                ("Start-Job { 1 } | Wait-Job | Receive-Job", "1"),
                ("(Get-Job | Measure-Object).Count", "0"),
                (
                    "Start-Job { 1 } | Out-Null; (Get-Job | Measure-Object).Count",
                    "1",
                ),
                (
                    "Start-Job { 1 } | Out-Null; Get-Job | Wait-Job | Out-Null; Get-Job | Remove-Job; (Get-Job | Measure-Object).Count",
                    "0",
                ),
                ("(Get-EventSubscriber | Measure-Object).Count", "0"),
            ] {
                let started = std::time::Instant::now();
                let plain = output_of(program, &flags, command, temp.path());
                let command = format!("Set-Location sub; {command}");
                let (code, text, end) =
                    wrapped_output(program, &flags, shell, &command, temp.path());
                assert_eq!(
                    (code, text.as_str()),
                    (Some(0), expected),
                    "{program}: {command}"
                );
                assert_eq!(plain.1, expected, "{program}: plain {command}");
                assert!(
                    started.elapsed() < std::time::Duration::from_mins(1),
                    "{program}: {command} waited"
                );
                let end = end.unwrap_or_else(|| panic!("{program}: {command}: not recorded"));
                assert!(
                    is_dir(&end, shell, temp.path(), &sub),
                    "{program}: {}",
                    end.dir
                );
            }
        }
    }

    /// What PowerShell wants at the top of a script stays at the top: a command
    /// that starts with `using`, a `param` block or a `#requires` line parses
    /// and prints what it printed, the line of an error in it is the line the
    /// model wrote, and the place is recorded.
    #[test]
    fn a_command_with_a_leading_form_runs_as_it_did_and_keeps_its_line_numbers() {
        let temp = temp();
        let sub = temp.path().join("sub");
        std::fs::create_dir(&sub).expect("sub");
        let cases = [
            (
                "using namespace System.IO; Set-Location sub; [Path]::GetFileName('a/b.txt')",
                "b.txt",
            ),
            (
                "using namespace System.IO\nSet-Location sub\n[Path]::GetFileName('a/b.txt')",
                "b.txt",
            ),
            (
                "# a note\n\nusing namespace System.IO\nusing namespace System.Text\nSet-Location sub\n[Path]::GetFileName('a/b.txt')",
                "b.txt",
            ),
            (
                "#requires -Version 3\nSet-Location sub\nWrite-Output hi",
                "hi",
            ),
            ("param($a = 5)\nSet-Location sub\nWrite-Output $a", "5"),
            (
                "[CmdletBinding()]param([string]$a = 'x;y)')\nSet-Location sub\nWrite-Output $a",
                "x;y)",
            ),
            (
                "using namespace System.IO\nparam($a = 6) # c\nSet-Location sub\nWrite-Output $a",
                "6",
            ),
            // The line of an error is the model's: the second line here.
            (
                "Set-Location sub\ntry { Get-Item zz-missing-zz -ErrorAction Stop } catch { $_.InvocationInfo.ScriptLineNumber }",
                "2",
            ),
            (
                "using namespace System.IO\nSet-Location sub\ntry { Get-Item zz-missing-zz -ErrorAction Stop } catch { $_.InvocationInfo.ScriptLineNumber }",
                "3",
            ),
            (
                "param($a)\nSet-Location sub\ntry { Get-Item zz-missing-zz -ErrorAction Stop } catch { $_.InvocationInfo.ScriptLineNumber }",
                "3",
            ),
        ];
        for (program, shell, flags) in shells() {
            if shell != Shell::PowerShell {
                continue;
            }
            for (command, expected) in cases {
                let plain = output_of(program, &flags, command, temp.path());
                let (code, text, end) =
                    wrapped_output(program, &flags, shell, command, temp.path());
                assert_eq!(
                    plain,
                    (Some(0), expected.to_owned()),
                    "{program}: plain {command}"
                );
                assert_eq!(
                    (code, text.as_str()),
                    (Some(0), expected),
                    "{program}: wrapped {command}"
                );
                let end = end.unwrap_or_else(|| panic!("{program}: {command}: not recorded"));
                assert!(
                    is_dir(&end, shell, temp.path(), &sub),
                    "{program}: {}",
                    end.dir
                );
            }
        }
    }

    #[test]
    fn the_registration_goes_after_what_a_script_must_start_with() {
        let at = |command: &str| registration_at(command);
        // Nothing to start with: before the first statement, after comments.
        assert_eq!(at("Get-Date"), Some((0, false)));
        assert_eq!(at("# note\nGet-Date"), Some((7, false)));
        assert_eq!(at("<# a\nblock #>\nGet-Date"), Some((14, false)));
        // After a `using` statement, which ends at `;` or at the line.
        assert_eq!(at("using namespace A.B; Get-Date"), Some((19, true)));
        assert_eq!(at("using namespace A.B\nGet-Date"), Some((19, true)));
        assert_eq!(at("USING namespace A.B\r\nGet-Date"), Some((19, true)));
        // After a `param` block, attributes before it included.
        assert_eq!(at("param($a, $b = ')')\nGet-Date"), Some((19, true)));
        assert_eq!(
            at("[CmdletBinding()] param($a)\nGet-Date"),
            Some((27, true))
        );
        assert_eq!(
            at("using namespace A\nparam(\n $a # )\n)\nGet-Date"),
            Some((34, true))
        );
        // An expression that starts with a type is a statement, not an attribute.
        assert_eq!(at("[Path]::GetFileName('a')"), Some((0, false)));
        // A name that only starts like a keyword is not one.
        assert_eq!(at("usingX 1"), Some((0, false)));
        assert_eq!(at("params 1"), Some((0, false)));
        // What does not close does not parse: nothing is registered.
        assert_eq!(at("param($a\nGet-Date"), None);
        assert_eq!(at("<# never closed\nGet-Date"), None);
    }

    /// Side files are in a directory made for this process under a random name,
    /// each with a name nobody can guess; one that is a link is not read.
    #[test]
    fn side_files_are_in_a_private_directory_under_unguessable_names() {
        let first = side_file().expect("a side file");
        let second = side_file().expect("a side file");
        assert_ne!(first.path(), second.path());
        let name = |file: &SideFile| {
            file.path()
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .into_owned()
        };
        assert!(name(&first).starts_with("shell-end-") && name(&first).len() >= 10 + 32);
        assert!(
            !name(&first).starts_with(&format!("shell-end-{}-", std::process::id())),
            "the process id is not in the name"
        );
        let directory = first.path().parent().expect("a directory");
        assert_eq!(directory, second.path().parent().expect("a directory"));
        assert!(
            directory
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .starts_with("ha-shell-")
        );
        assert!(directory.is_dir());
        // A file the shell wrote is read once, and removed.
        std::fs::write(first.path(), "/w/sub\n/bin\n").expect("written");
        assert_eq!(
            first.take(),
            Some(End {
                dir: "/w/sub".to_owned(),
                path: "/bin".to_owned()
            })
        );
        assert!(!first.path().exists());
        assert_eq!(first.take(), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_side_directory_is_for_this_user_alone_and_a_link_is_not_read() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
        let temp = temp();
        let made = make_side_dir(temp.path()).expect("a directory");
        let meta = std::fs::symlink_metadata(&made.path).expect("made");
        assert!(meta.is_dir());
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        assert_eq!(Some(u64::from(meta.uid())), made.owner);
        let target = temp.path().join("target.txt");
        std::fs::write(&target, "/w/sub\n/bin\n").expect("target");
        let link = made.path.join("shell-end-link.txt");
        symlink(&target, &link).expect("link");
        let file = SideFile {
            path: link.clone(),
            owner: made.owner,
        };
        assert_eq!(file.take(), None, "a link is not read");
        // A file of another owner is not read.
        let plain = made.path.join("shell-end-plain.txt");
        std::fs::write(&plain, "/w/sub\n/bin\n").expect("plain");
        let stranger = SideFile {
            path: plain.clone(),
            owner: made.owner.map(|owner| owner + 1),
        };
        assert_eq!(stranger.take(), None, "a file of another user is not read");
    }

    #[test]
    fn the_places_of_the_least_recently_used_conversation_go_first_and_a_busy_one_stays() {
        let root = temp();
        let sub = root.path().join("sub");
        std::fs::create_dir(&sub).expect("sub");
        let places = Places::with_limit(true, 2);
        let put = |task: &str| {
            places.finish(
                task,
                "call",
                root.path(),
                &Place::default(),
                Some(&end(&sub, "/h")),
                Some("/h"),
            );
        };
        put("t1");
        put("t2");
        // Using t1 makes t2 the oldest.
        assert!(places.start("t1", root.path()).dir.is_some());
        put("t3");
        assert!(places.start("t1", root.path()).dir.is_some(), "t1 was used");
        assert_eq!(places.start("t2", root.path()), Place::default(), "t2 went");
        assert!(places.start("t3", root.path()).dir.is_some());
        // A conversation with a call in its turn is never the one that goes.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let _turn = places.serialize("t1").await.expect("a turn");
            put("t4");
            put("t5");
            assert!(places.start("t1", root.path()).dir.is_some(), "t1 is busy");
        });
    }

    #[test]
    fn what_the_guard_judged_goes_with_the_call_and_the_oldest_go_first() {
        let places = Places::with_limit(true, 2);
        places.judged("a", "/w");
        places.judged("b", "/w/sub");
        places.forget_judged("a");
        assert_eq!(places.take_judged("a"), None, "forgotten");
        places.judged("c", "/w");
        places.judged("d", "/w");
        // Past the limit the oldest went, not everything.
        assert_eq!(places.take_judged("b"), None);
        assert_eq!(places.take_judged("c").as_deref(), Some("/w"));
        assert_eq!(places.take_judged("d").as_deref(), Some("/w"));
    }

    /// A call that ran at the root has the host's line too, so the last `ran in`
    /// line of a result is always the host's.
    #[test]
    fn a_call_at_the_root_says_where_it_ran_too() {
        let root = temp();
        let places = Places::default();
        places.finish("t", "c1", root.path(), &Place::default(), None, Some("/h"));
        assert_eq!(places.take_notes("c1"), ["ran in .".to_owned()]);
        let off = Places::new(false);
        off.finish("t", "c1", root.path(), &Place::default(), None, Some("/h"));
        assert!(
            off.take_notes("c1").is_empty(),
            "nothing while the switch is off"
        );
    }

    #[test]
    fn what_an_approval_binds_and_shows_is_the_place_when_it_is_not_the_default() {
        let root = temp();
        assert_eq!(Place::default().binding(), "");
        assert_eq!(Place::default().summary(root.path()), None);
        let here = Place {
            dir: Some(root.path().join("sub")),
            path: Some("/x:/y".to_owned()),
        };
        assert_ne!(here.binding(), "");
        assert_ne!(here.binding(), Place::default().binding());
        let other = Place {
            dir: Some(root.path().join("other")),
            path: Some("/x:/y".to_owned()),
        };
        assert_ne!(here.binding(), other.binding());
        let summary = here.summary(root.path()).expect("a summary");
        assert!(summary.contains("runs in "), "{summary}");
        assert!(summary.contains("/x:/y"), "{summary}");
    }

    /// With an `EXIT` trap, a program a signal killed is reported by the shell
    /// as 128+N; the call then ends by the signal, as without the trap. Unix
    /// only: a Windows host has no such deaths to show.
    #[cfg(unix)]
    #[test]
    fn a_program_killed_by_a_signal_ends_the_call_by_that_signal() {
        use std::os::unix::process::ExitStatusExt;
        let temp = temp();
        let side = side_file_in(temp.path());
        let command = "sh -c 'kill -TERM $$'";
        let run = |text: &str| {
            std::process::Command::new("sh")
                .args(["-c", text])
                .current_dir(temp.path())
                .output()
                .expect("shell starts")
                .status
        };
        let wrapped = run(&wrap(Shell::Sh, command, &side));
        assert_eq!(wrapped.signal(), Some(15), "{wrapped:?}");
        assert!(side.exists(), "the place was written before it");
    }

    /// A command that turned on `set -e` or `set -u` has the same status with
    /// the recording: a write that fails, or an unset `PATH`, cannot change it.
    #[test]
    fn the_recording_cannot_change_the_status_of_a_command_with_set_e_or_set_u() {
        let temp = temp();
        for (program, shell, flags) in shells() {
            if shell != Shell::Sh {
                continue;
            }
            // `PATH` unset under `set -u` made the trap's own `$PATH` an error.
            let unset = "set -u; unset PATH; exit 5";
            let (code, _) = wrapped(program, &flags, shell, unset, temp.path());
            assert_eq!(code, Some(5), "{program}: {unset}");
            // A side file in a folder that is not there: the write fails.
            let missing = temp.path().join("missing").join("side.txt");
            for command in ["set -e; exit 5", "set -eu; true", "set -e; false"] {
                let plain = run(program, &flags, command, temp.path());
                let code = run(
                    program,
                    &flags,
                    &wrap(shell, command, &missing),
                    temp.path(),
                );
                assert_eq!(code, plain, "{program}: {command}");
            }
        }
    }
}
