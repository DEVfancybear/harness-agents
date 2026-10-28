//! Guards on the commands the model runs (`docs/architecture/GUARDS.en.md`),
//! after Agnostic-AI's `rm-guard`, `process-kill-guard`, `slow-command-guard`,
//! `secret-path-guard` and `gate-freeze` (read at `aab09ff`).
//!
//! A command is read as a shell reads it - quotes, separators, pipes,
//! redirections, comments, leading `sudo`/`env`/assignments - and each
//! command in it is judged by what it runs and on what:
//! - a recursive delete passes only when everything it removes can be had
//!   back: a build or cache directory inside the workspace, or files git
//!   tracks with nothing uncommitted or ignored among them;
//! - a process is killed by its id, never by its name, which cannot tell the
//!   agent's process from one the user is working in;
//! - a recursive search does not start at a drive's root or the home folder;
//! - a secret file (`.env`, keys, credentials) is not read or copied through
//!   the shell, as the file tools already refuse it;
//! - what judges the agent - ha's configuration, hooks and state, and the
//!   project's `.harness` - is not changed through the shell.
//!
//! Each refusal names the variable that lifts it. It is read from ha's own
//! environment, so an assignment inside the command cannot.

use std::path::{Component, Path, PathBuf};

/// Lifts the recursive-delete guard.
pub(crate) const ALLOW_RECURSIVE_DELETE: &str = "HA_ALLOW_RECURSIVE_DELETE";
/// Lifts the kill-by-name guard.
pub(crate) const ALLOW_KILL_BY_NAME: &str = "HA_ALLOW_KILL_BY_NAME";
/// Lifts the root-search guard.
pub(crate) const ALLOW_ROOT_SEARCH: &str = "HA_ALLOW_ROOT_SEARCH";
/// Lifts the secret-path guard.
pub(crate) const ALLOW_SECRET_PATHS: &str = "HA_ALLOW_SECRET_PATHS";
/// Lifts the guard on ha's own configuration and state.
pub(crate) const ALLOW_HARNESS_WRITES: &str = "HA_ALLOW_HARNESS_WRITES";

/// Directories a recursive delete may always remove: what a build or a tool
/// regenerates.
const DISPOSABLE_DIRS: [&str; 17] = [
    "node_modules",
    "dist",
    "build",
    "target",
    "out",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".parcel-cache",
    "coverage",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".cache",
    "tmp",
    ".tmp",
];

pub(crate) fn lifted(variable: &str) -> bool {
    std::env::var(variable).is_ok_and(|value| !value.is_empty() && value != "0")
}

/// The host's own directories - its configuration and its state - that no
/// command may change. One host per process: every tool service, a child's
/// included, guards the same ones.
static HOST_DIRS: std::sync::RwLock<Vec<PathBuf>> = std::sync::RwLock::new(Vec::new());

/// Name the host's configuration and state directories for the guards.
pub fn protect_host_dirs(dirs: Vec<PathBuf>) {
    if let Ok(mut kept) = HOST_DIRS.write() {
        *kept = dirs;
    }
}

fn host_dirs() -> Vec<PathBuf> {
    HOST_DIRS
        .read()
        .map(|dirs| dirs.clone())
        .unwrap_or_default()
}

// --- reading a command ---

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Segment {
    /// The command and its arguments, quotes removed, wrappers dropped.
    pub words: Vec<String>,
    /// Targets of `>`, `>>`, `2>` and the like.
    pub redirects: Vec<String>,
    /// The output of the command before flows into this one.
    pub piped: bool,
}

impl Segment {
    fn command(&self) -> String {
        self.words.first().map_or_else(String::new, |word| {
            let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
            let name = name.to_ascii_lowercase();
            name.strip_suffix(".exe")
                .map_or(name.clone(), str::to_owned)
        })
    }

    fn arguments(&self) -> &[String] {
        self.words.get(1..).unwrap_or_default()
    }

    fn lower_arguments(&self) -> Vec<String> {
        self.arguments()
            .iter()
            .map(|word| word.to_ascii_lowercase())
            .collect()
    }
}

/// Words a command may be wrapped in before the command itself.
fn is_wrapper(word: &str) -> bool {
    matches!(
        word,
        "sudo" | "env" | "nice" | "time" | "command" | "exec" | "nohup" | "&" | "."
    ) || is_assignment(word)
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        && !name.starts_with(|character: char| character.is_ascii_digit())
}

/// The commands of a shell command line, bash's or `PowerShell`'s, with
/// those a wrapping shell runs (`bash -c "..."`) read too.
pub(crate) fn parse(command: &str) -> Vec<Segment> {
    expand(parse_flat(command), 0)
}

/// The commands of one command line, as written.
#[allow(
    clippy::too_many_lines,
    reason = "one pass over the characters, every shell construct in one place"
)]
fn parse_flat(command: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut current = Segment::default();
    let mut word = String::new();
    let mut in_word = false;
    let mut redirect_next = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    let finish_word =
        |word: &mut String, in_word: &mut bool, current: &mut Segment, redirect: &mut bool| {
            if !*in_word {
                return;
            }
            if *redirect {
                current.redirects.push(std::mem::take(word));
                *redirect = false;
            } else if current.words.is_empty() && is_wrapper(word) {
                word.clear();
            } else {
                current.words.push(std::mem::take(word));
            }
            *in_word = false;
        };
    let finish_segment = |current: &mut Segment, segments: &mut Vec<Segment>, piped: bool| {
        if !current.words.is_empty() || !current.redirects.is_empty() {
            segments.push(std::mem::take(current));
        }
        current.piped = piped;
    };
    while let Some(character) = chars.next() {
        if let Some(open) = quote {
            if character == open {
                quote = None;
            } else if open == '"' && character == '\\' && chars.peek() == Some(&'"') {
                word.push('"');
                chars.next();
            } else {
                word.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => {
                quote = Some(character);
                in_word = true;
            }
            '#' if !in_word => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        break;
                    }
                }
                finish_segment(&mut current, &mut segments, false);
            }
            '>' => {
                // `2>`, `*>`: the stream number is not a word.
                if in_word && (word == "2" || word == "1" || word == "*") {
                    word.clear();
                    in_word = false;
                }
                finish_word(&mut word, &mut in_word, &mut current, &mut redirect_next);
                if chars.peek() == Some(&'>') {
                    chars.next();
                }
                if chars.peek() == Some(&'&') {
                    // `2>&1`: a stream, not a file.
                    chars.next();
                    while chars.peek().is_some_and(char::is_ascii_digit) {
                        chars.next();
                    }
                } else {
                    redirect_next = true;
                }
            }
            '{' if in_word && word.ends_with('$') => {
                // `${HOME}`: a variable, not a block.
                word.push('{');
                for next in chars.by_ref() {
                    word.push(next);
                    if next == '}' {
                        break;
                    }
                }
            }
            '|' | ';' | '\n' | '(' | ')' | '{' | '}' | '&' => {
                if character == '&' && !in_word && current.words.is_empty() {
                    // PowerShell's call operator: `& $tool -Name x`.
                    let mut ahead = chars.clone();
                    if ahead.next().is_some_and(char::is_whitespace) {
                        current.words.push("&".to_owned());
                        continue;
                    }
                }
                finish_word(&mut word, &mut in_word, &mut current, &mut redirect_next);
                let doubled = chars.peek() == Some(&character);
                if doubled && matches!(character, '|' | '&') {
                    chars.next();
                }
                let piped = character == '|' && !doubled;
                finish_segment(&mut current, &mut segments, piped);
            }
            '$' if chars.peek() == Some(&'(') => {
                // `$(...)`: its commands are commands too.
                finish_word(&mut word, &mut in_word, &mut current, &mut redirect_next);
                finish_segment(&mut current, &mut segments, false);
            }
            whitespace if whitespace.is_whitespace() => {
                finish_word(&mut word, &mut in_word, &mut current, &mut redirect_next);
            }
            other => {
                word.push(other);
                in_word = true;
            }
        }
    }
    finish_word(&mut word, &mut in_word, &mut current, &mut redirect_next);
    finish_segment(&mut current, &mut segments, false);
    // `&` kept as the call operator stays out of the command's name.
    for segment in &mut segments {
        if segment.words.first().is_some_and(|word| word == "&") && segment.words.len() > 1 {
            // `& $var`: the command is the variable; keep `&` so it reads as one.
            if !segment.words[1].starts_with('$') {
                segment.words.remove(0);
            }
        }
    }
    segments
}

/// Shells whose command comes as an argument: `bash -c "..."`,
/// `powershell -Command "..."`, `cmd /c ...`. That command is read too, or a
/// wrapper would carry anything past the guards.
fn expand(segments: Vec<Segment>, depth: usize) -> Vec<Segment> {
    let mut out = Vec::new();
    for segment in segments {
        let command = segment.command();
        let lower = segment.lower_arguments();
        let inner = match command.as_str() {
            "bash" | "sh" | "zsh" | "dash" | "ksh" => lower
                .iter()
                .position(|word| is_short_flag_cluster(word, "celixs") && word.contains('c'))
                .and_then(|at| segment.arguments().get(at + 1).cloned()),
            "powershell" | "pwsh" => lower
                .iter()
                .position(|word| is_parameter(word, "command", 0) && word.len() >= 2)
                .map(|at| segment.arguments()[at + 1..].join(" ")),
            "cmd" => lower
                .iter()
                .position(|word| word == "/c" || word == "/k")
                .map(|at| segment.arguments()[at + 1..].join(" ")),
            _ => None,
        };
        out.push(segment);
        if let Some(inner) = inner.filter(|inner| !inner.trim().is_empty())
            && depth < 4
        {
            // Read with the same rules, one level further in.
            out.extend(expand(parse_flat(&inner), depth + 1));
        }
    }
    out
}

// --- paths ---

/// `path` without `.` and with `..` applied, as text: nothing is read from
/// the disk, so a path that does not exist yet is judged the same way.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn same_or_inside(path: &Path, base: &Path) -> bool {
    let lower = |path: &Path| {
        let text = path.to_string_lossy().replace('\\', "/");
        let text = text.trim_end_matches('/');
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text.to_owned()
        }
    };
    let (path, base) = (lower(path), lower(base));
    path == base || path.starts_with(&format!("{base}/"))
}

/// A word that names a path the guard can judge: no variable, glob, home
/// shorthand or substitution in it.
fn literal(word: &str) -> bool {
    !word.is_empty()
        && !word.contains(['$', '*', '?', '~', '%', '`', '[', '{'])
        && !word.starts_with('-')
}

fn absolute(root: &Path, word: &str) -> PathBuf {
    let path = Path::new(word);
    lexical(
        &if path.is_absolute() || word.starts_with('/') || word.starts_with('\\') {
            path.to_path_buf()
        } else {
            root.join(path)
        },
    )
}

fn names(word: &str) -> Vec<String> {
    word.split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Whether `word` names a secret file: an env file (not its example), a
/// private key, a credentials file, a token file.
pub(crate) fn names_secret(word: &str) -> bool {
    let parts = names(word);
    parts.iter().enumerate().any(|(index, name)| {
        let env = name == ".env"
            || name == ".envrc"
            || name.strip_prefix(".env.").is_some_and(|suffix| {
                !matches!(suffix, "example" | "sample" | "template" | "dist")
            });
        let key = matches!(
            name.as_str(),
            "id_rsa" | "id_ed25519" | "id_ecdsa" | "id_dsa" | ".secrets" | ".secrets.env"
        ) || name.contains("private_key");
        let tokens = matches!(
            name.as_str(),
            "credentials.json" | ".npmrc" | ".netrc" | ".pgpass" | ".git-credentials"
        ) || (name == "credentials" && index > 0 && parts[index - 1] == ".aws");
        let extension = index + 1 == parts.len()
            && name.rsplit_once('.').is_some_and(|(stem, extension)| {
                !stem.is_empty() && matches!(extension, "pem" | "key" | "p12" | "pfx" | "clixml")
            });
        env || key || tokens || extension
    })
}

// --- the guards ---

/// One refusal: the guard, and what to tell the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub guard: &'static str,
    pub reason: String,
}

fn refusal(guard: &'static str, variable: &str, reason: &str) -> Option<Refusal> {
    (!lifted(variable)).then(|| Refusal {
        guard,
        reason: format!(
            "{reason}\n\nThe user can lift this guard by starting ha with {variable}=1."
        ),
    })
}

fn is_short_flag_cluster(word: &str, letters: &str) -> bool {
    word.len() > 1
        && word.len() <= 5
        && word.starts_with('-')
        && !word.starts_with("--")
        && word[1..]
            .chars()
            .all(|character| letters.contains(character))
}

/// `PowerShell`'s parameter `-name` as the user may shorten it, down to `least`
/// characters after the dash.
fn is_parameter(word: &str, name: &str, least: usize) -> bool {
    let word = word.to_ascii_lowercase();
    word.len() > least && word.starts_with('-') && name.starts_with(&word[1..])
}

/// A recursive delete in `segment`, with what it would remove.
fn recursive_delete(segment: &Segment) -> Option<Vec<String>> {
    let command = segment.command();
    let arguments = segment.arguments();
    let lower = segment.lower_arguments();
    let targets = |value_of: &[&str], skip_value_of: &[&str]| {
        let mut targets = Vec::new();
        let mut index = 0;
        while index < arguments.len() {
            let word = &lower[index];
            if value_of.iter().any(|name| is_parameter(word, name, 1)) {
                if let Some(value) = arguments.get(index + 1) {
                    targets.push(value.clone());
                }
                index += 2;
                continue;
            }
            if skip_value_of.iter().any(|name| is_parameter(word, name, 1)) {
                index += 2;
                continue;
            }
            if !word.starts_with('-') && (!word.starts_with('/') || word.len() != 2) {
                targets.push(arguments[index].clone());
            }
            index += 1;
        }
        // PowerShell takes a list as `a,b` or `a, b`: each one is a target.
        targets
            .iter()
            .flat_map(|target| target.split(','))
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    match command.as_str() {
        "rm" => {
            let recursive = lower.iter().any(|word| {
                word == "--recursive"
                    || is_parameter(word, "recurse", 1)
                    || (is_short_flag_cluster(word, "rRfivdI") && word.contains(['r', 'R']))
            }) || arguments
                .iter()
                .any(|word| is_short_flag_cluster(word, "rRfivdI") && word.contains('R'));
            recursive.then(|| targets(&["path", "literalpath"], &["include", "exclude", "filter"]))
        }
        "remove-item" | "ri" | "del" | "erase" | "rd" | "rmdir" => {
            let recursive = lower
                .iter()
                .any(|word| is_parameter(word, "recurse", 1) || word == "/s");
            recursive.then(|| targets(&["path", "literalpath"], &["include", "exclude", "filter"]))
        }
        "find" if lower.iter().any(|word| word == "-delete") => {
            // `find <paths> ... -delete`: the paths before the first test.
            Some(
                arguments
                    .iter()
                    .take_while(|word| !word.starts_with('-'))
                    .cloned()
                    .collect(),
            )
        }
        _ => None,
    }
}

/// A name-based kill in `segment`, given the command before it in a pipe.
fn kill_by_name(
    segment: &Segment,
    before: Option<&Segment>,
) -> Option<(&'static str, &'static str)> {
    let command = segment.command();
    let lower = segment.lower_arguments();
    // `-n` is Stop-Process's `-Name`, but bash's `kill -n` names a signal:
    // `kill` needs the parameter spelled past its first letter.
    let least = if command == "kill" { 3 } else { 2 };
    let by_name = lower
        .iter()
        .any(|word| is_parameter(word, "name", 0) && word.len() >= least);
    let by_id = lower
        .iter()
        .any(|word| is_parameter(word, "id", 0) || is_parameter(word, "inputobject", 1));
    match command.as_str() {
        "stop-process" | "spps" | "kill" if by_name => {
            Some(("Stop-Process -Name", "Stop-Process -Id <pid>"))
        }
        "&" | "iex" | "invoke-expression" if by_name => Some((
            "a kill through a variable or an expression, with -Name",
            "Stop-Process -Id <pid>, invoked directly",
        )),
        "stop-process" | "spps" | "kill"
            if segment.piped
                && !by_id
                && before.is_some_and(|before| {
                    matches!(before.command().as_str(), "get-process" | "gps" | "ps")
                        && !before
                            .lower_arguments()
                            .iter()
                            .any(|word| is_parameter(word, "id", 0))
                }) =>
        {
            Some((
                "Get-Process <name> piped into Stop-Process",
                "Get-Process -Id <pid> | Stop-Process",
            ))
        }
        "taskkill" if lower.iter().any(|word| word == "/im" || word == "-im") => {
            Some(("taskkill /IM (by image name)", "taskkill /F /PID <pid>"))
        }
        "pkill" => Some(("pkill (by name or pattern)", "kill <pid>")),
        "killall" => Some(("killall (every process of a name)", "kill <pid>")),
        _ => None,
    }
}

fn home() -> Option<String> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .filter(|home| !home.is_empty())
}

/// Whether `word` is a drive's root or the home folder.
fn is_root(word: &str) -> bool {
    let trimmed = word.trim_end_matches(['/', '\\']);
    let lower = trimmed.to_ascii_lowercase();
    if lower.is_empty() && !word.is_empty() {
        return true; // `/`
    }
    let drive = |text: &str| {
        let bytes = text.as_bytes();
        bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
    };
    let git_bash_drive = |text: &str| {
        let text = text.strip_prefix("/mnt").unwrap_or(text);
        text.len() == 2 && text.starts_with('/') && text.as_bytes()[1].is_ascii_alphabetic()
    };
    drive(&lower)
        || git_bash_drive(&lower)
        || matches!(
            lower.as_str(),
            "~" | "$home" | "${home}" | "$env:userprofile" | "%userprofile%" | "$env:home"
        )
        || lower.ends_with(":\\users")
        || lower.ends_with(":/users")
        || home().is_some_and(|home| {
            let home = home
                .replace('\\', "/")
                .trim_end_matches('/')
                .to_ascii_lowercase();
            lower.replace('\\', "/") == home
        })
}

/// A recursive search in `segment` rooted at a drive or the home folder.
fn root_search(segment: &Segment) -> Option<String> {
    let command = segment.command();
    let arguments = segment.arguments();
    let lower = segment.lower_arguments();
    let recursive = match command.as_str() {
        "grep" | "egrep" | "fgrep" => {
            lower.iter().any(|word| {
                word == "--recursive"
                    || word == "--dereference-recursive"
                    || (word.starts_with('-') && !word.starts_with("--") && word.contains(['r']))
            }) || arguments
                .iter()
                .any(|word| word.starts_with('-') && !word.starts_with("--") && word.contains('R'))
        }
        "rg" | "ag" | "find" | "fd" => true,
        "get-childitem" | "gci" => lower.iter().any(|word| is_parameter(word, "recurse", 0)),
        "ls" | "dir" => {
            lower
                .iter()
                .any(|word| is_parameter(word, "recurse", 2) || word == "/s")
                || arguments.iter().any(|word| word == "-R")
        }
        _ => false,
    };
    if !recursive {
        return None;
    }
    let mut index = 0;
    while index < arguments.len() {
        let word = &arguments[index];
        if is_parameter(word, "path", 1) || is_parameter(word, "literalpath", 1) {
            if let Some(value) = arguments.get(index + 1).filter(|value| is_root(value)) {
                return Some(value.clone());
            }
            index += 2;
            continue;
        }
        if !word.starts_with('-') && is_root(word) {
            return Some(word.clone());
        }
        index += 1;
    }
    None
}

/// Commands that only say whether a file is there, never what is in it.
fn only_looks(segment: &Segment) -> bool {
    let lower = segment.lower_arguments();
    match segment.command().as_str() {
        "echo" | "printf" | "write-output" | "write-host" | "test-path" | "stat" | "ls" | "dir"
        | "get-childitem" | "gci" | "file" => true,
        "git" => match lower.first().map(String::as_str) {
            Some("check-ignore" | "status" | "ls-files" | "log") => true,
            Some("rm") => lower.iter().any(|word| word == "--cached"),
            _ => false,
        },
        _ => false,
    }
}

/// A secret file a segment reads, copies or writes.
fn secret_path(segment: &Segment) -> Option<String> {
    let arguments = segment.arguments();
    // `--env-file .env` hands the file to the program, which reads it; the
    // values never reach the output.
    let handed =
        |index: usize| index > 0 && arguments[index - 1].eq_ignore_ascii_case("--env-file");
    if !only_looks(segment)
        && let Some(word) = arguments
            .iter()
            .enumerate()
            .find(|(index, word)| names_secret(word) && !handed(*index))
            .map(|(_, word)| word)
    {
        return Some(word.clone());
    }
    segment
        .redirects
        .iter()
        .find(|word| names_secret(word))
        .cloned()
}

/// Commands that change what they name.
fn modifies(segment: &Segment) -> bool {
    let lower = segment.lower_arguments();
    match segment.command().as_str() {
        "rm" | "del" | "erase" | "remove-item" | "ri" | "rd" | "rmdir" | "mv" | "move"
        | "move-item" | "mi" | "ren" | "rename" | "rename-item" | "rni" | "cp" | "copy"
        | "copy-item" | "cpi" | "set-content" | "sc" | "add-content" | "ac" | "out-file"
        | "new-item" | "ni" | "tee" | "tee-object" | "truncate" | "chmod" | "icacls" | "attrib"
        | "touch" | "mkdir" | "md" => true,
        "sed" | "perl" => lower.iter().any(|word| word.starts_with("-i")),
        _ => false,
    }
}

/// A path under one of the `protected` directories a segment changes.
///
/// The workspace is the agent's to change even when it sits inside a
/// protected directory - a coder's worktree lives in ha's state - so inside
/// it only a protected directory that is itself inside it (`.harness`)
/// counts.
fn protected_write(segment: &Segment, root: &Path, protected: &[PathBuf]) -> Option<String> {
    let inside = |word: &String| {
        literal(word) && {
            let path = absolute(root, word);
            let in_workspace = same_or_inside(&path, root);
            protected.iter().any(|base| {
                same_or_inside(&path, base) && (!in_workspace || same_or_inside(base, root))
            })
        }
    };
    if modifies(segment)
        && let Some(word) = segment.arguments().iter().find(|word| inside(word))
    {
        return Some(word.clone());
    }
    segment.redirects.iter().find(|word| inside(word)).cloned()
}

/// What a recursive delete's targets come to: every one can be had back, or
/// the reason one cannot.
async fn delete_refusal(
    root: &Path,
    targets: &[String],
    relocated: bool,
    cancellation: &harness_providers::CancellationToken,
    spool: &crate::capture::ProcessSpoolConfig,
) -> Option<String> {
    if targets.is_empty() {
        return Some(
            "it names no path the guard can read (its input comes from a pipe or a variable)"
                .to_owned(),
        );
    }
    for target in targets {
        // A build or cache folder, reached without stepping back out of it:
        // `build/../src` is `src`.
        let parts = names(target);
        let disposable = !parts.iter().any(|name| name == "..")
            && parts
                .iter()
                .any(|name| DISPOSABLE_DIRS.contains(&name.as_str()));
        if !literal(target) {
            if disposable {
                continue;
            }
            return Some(format!(
                "`{target}` is not a literal path the guard can check"
            ));
        }
        if relocated && !Path::new(target).is_absolute() {
            if disposable {
                continue;
            }
            return Some(format!(
                "the command changes directory first, so where `{target}` points cannot be checked"
            ));
        }
        let path = absolute(root, target);
        // Outside the workspace nothing is the agent's to remove: the system's
        // temporary folder holds other programs' files too.
        if !same_or_inside(&path, root) {
            return Some(format!("`{target}` is outside the workspace"));
        }
        if disposable {
            continue;
        }
        if lexical(&path) == lexical(root) {
            return Some("it removes the whole workspace".to_owned());
        }
        let relative = path.strip_prefix(root).map_or_else(
            |_| target.clone(),
            |relative| relative.display().to_string(),
        );
        let args = [
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored=matching",
            "--",
            relative.as_str(),
        ]
        .map(str::to_owned);
        let status =
            crate::process::run_internal_git(root, &args, 15_000, cancellation.clone(), spool)
                .await;
        match status {
            Ok(result) if result.exit_code == Some(0) => {
                let left = result
                    .stdout
                    .lines()
                    .map(str::trim_end)
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>();
                if !left.is_empty() {
                    let shown = left
                        .iter()
                        .take(5)
                        .copied()
                        .collect::<Vec<_>>()
                        .join("\n  ");
                    return Some(format!(
                        "`{target}` holds {} file(s) git could not give back (uncommitted, untracked or ignored):\n  {shown}",
                        left.len()
                    ));
                }
            }
            _ => {
                return Some(format!(
                    "`{target}` is not in a git repository, so nothing could give it back"
                ));
            }
        }
    }
    None
}

/// Why `command`, a shell command line, must not run; `None` when every guard
/// lets it through.
pub(crate) async fn shell_refusal(
    root: &Path,
    command: &str,
    protected: &[PathBuf],
    cancellation: harness_providers::CancellationToken,
    spool: &crate::capture::ProcessSpoolConfig,
) -> Option<Refusal> {
    let segments = parse(command);
    judge(root, &segments, protected, &cancellation, spool).await
}

/// Why a program and its arguments, run without a shell, must not run.
pub(crate) async fn process_refusal(
    root: &Path,
    executable: &str,
    args: &[String],
    protected: &[PathBuf],
    cancellation: harness_providers::CancellationToken,
    spool: &crate::capture::ProcessSpoolConfig,
) -> Option<Refusal> {
    let mut words = vec![executable.to_owned()];
    words.extend(args.iter().cloned());
    let segment = Segment {
        words,
        ..Segment::default()
    };
    // `powershell -Command ...` run without a shell is still a shell.
    let segments = expand(vec![segment], 0);
    judge(root, &segments, protected, &cancellation, spool).await
}

async fn judge(
    root: &Path,
    segments: &[Segment],
    protected: &[PathBuf],
    cancellation: &harness_providers::CancellationToken,
    spool: &crate::capture::ProcessSpoolConfig,
) -> Option<Refusal> {
    let mut protected = protected.to_vec();
    protected.extend(host_dirs());
    protected.push(root.join(".harness"));
    let mut relocated = false;
    for (index, segment) in segments.iter().enumerate() {
        let before = index.checked_sub(1).map(|before| &segments[before]);
        if let Some((what, instead)) = kill_by_name(segment, before)
            && let Some(refused) = refusal(
                "kill-by-name",
                ALLOW_KILL_BY_NAME,
                &format!(
                    "Refusing {what}: a name cannot tell a process the agent started from one the user is working in. Use {instead}, with the id captured when the process was started."
                ),
            )
        {
            return Some(refused);
        }
        if let Some(path) = root_search(segment)
            && let Some(refused) = refusal(
                "root-search",
                ALLOW_ROOT_SEARCH,
                &format!(
                    "Refusing a recursive search from `{path}`: a whole drive or home folder takes minutes and floods the output. Search the workspace or a named folder in it, or use search_text or find_files."
                ),
            )
        {
            return Some(refused);
        }
        if let Some(path) = secret_path(segment)
            && let Some(refused) = refusal(
                "secret-path",
                ALLOW_SECRET_PATHS,
                &format!(
                    "Refusing a command on `{path}`: it is a secret file (an env file, a key or credentials), which the file tools refuse too. Refer to the variable or setting by name instead of reading its value."
                ),
            )
        {
            return Some(refused);
        }
        if let Some(path) = protected_write(segment, root, &protected)
            && let Some(refused) = refusal(
                "harness-integrity",
                ALLOW_HARNESS_WRITES,
                &format!(
                    "Refusing a change to `{path}`: it is ha's configuration, hooks or state, which decide what the agent may do and what counts as verified. Tell the user what to change, and they will change it."
                ),
            )
        {
            return Some(refused);
        }
        if let Some(targets) = recursive_delete(segment)
            && !lifted(ALLOW_RECURSIVE_DELETE)
            && let Some(why) = delete_refusal(root, &targets, relocated, cancellation, spool).await
        {
            return refusal(
                "recursive-delete",
                ALLOW_RECURSIVE_DELETE,
                &format!(
                    "Refusing this recursive delete: {why}. A recursive delete runs only when everything it removes can be had back: a build or cache folder, or files in the workspace git tracks with nothing uncommitted, untracked or ignored among them. Commit what should be kept first, delete the files one by one, or ask the user to delete it."
                ),
            );
        }
        if matches!(
            segment.command().as_str(),
            "cd" | "pushd" | "set-location" | "sl" | "chdir" | "push-location"
        ) {
            relocated = true;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(command: &str) -> Vec<Vec<String>> {
        parse(command)
            .into_iter()
            .map(|segment| segment.words)
            .collect()
    }

    #[test]
    fn a_command_line_is_read_as_a_shell_reads_it() {
        assert_eq!(
            words("cd sub && sudo rm -rf 'a b' ; echo \"x # y\" # gone\nls | wc -l"),
            [
                vec!["cd", "sub"],
                vec!["rm", "-rf", "a b"],
                vec!["echo", "x # y"],
                vec!["ls"],
                vec!["wc", "-l"]
            ]
        );
        let segments = parse("FOO=1 cat .env > out.txt 2>&1 | grep x");
        assert_eq!(segments[0].words, ["cat", ".env"]);
        assert_eq!(segments[0].redirects, ["out.txt"]);
        assert!(segments[1].piped);
        assert_eq!(
            words("echo $(rm -rf /)"),
            [vec!["echo"], vec!["rm", "-rf", "/"]]
        );
        // A wrapping shell's command is read too.
        assert_eq!(words("bash -c 'rm -rf ~'")[1], ["rm", "-rf", "~"]);
        assert_eq!(
            words("powershell -NoProfile -Command Stop-Process -Name x")[1],
            ["Stop-Process", "-Name", "x"]
        );
        assert_eq!(
            words(r"cmd /c rd /s /q C:\x")[1],
            ["rd", "/s", "/q", r"C:\x"]
        );
        assert_eq!(
            words("& $tool -Name notepad")[0],
            ["&", "$tool", "-Name", "notepad"]
        );
        assert_eq!(words("& 'C:/x/y.exe' a")[0], ["C:/x/y.exe", "a"]);
    }

    #[test]
    fn kills_by_name_are_named_and_by_id_pass() {
        let first = |command: &str| {
            let segments = parse(command);
            segments.iter().enumerate().find_map(|(index, segment)| {
                kill_by_name(
                    segment,
                    index.checked_sub(1).map(|before| &segments[before]),
                )
            })
        };
        for refused in [
            "Stop-Process -Name notepad",
            "Stop-Process -n notepad -Force",
            "Get-Process notepad | Stop-Process",
            "taskkill /F /IM notepad.exe",
            "pkill -f node",
            "killall node",
            "& $stop -Name notepad",
        ] {
            assert!(first(refused).is_some(), "{refused}");
        }
        for passed in [
            "Stop-Process -Id 1234",
            "Get-Process -Id 1234 | Stop-Process",
            "taskkill /F /PID 1234",
            "kill 1234",
            "kill -n 9 1234",
            "Get-Process notepad",
        ] {
            assert!(first(passed).is_none(), "{passed}");
        }
    }

    #[test]
    fn a_search_from_a_root_is_named() {
        let search = |command: &str| parse(command).iter().find_map(root_search);
        for refused in [
            "grep -rn TODO /",
            "grep -R x C:\\",
            "rg needle ~",
            "find / -name x",
            "Get-ChildItem -Path C:\\ -Recurse -Filter *.log",
            "gci $env:USERPROFILE -r",
            "rg x /c",
            "rg x ${HOME}",
        ] {
            assert!(search(refused).is_some(), "{refused}");
        }
        for passed in [
            "grep -rn TODO src",
            "rg needle",
            "find . -name x",
            "ls -r /",
            "Get-ChildItem C:\\ ",
            "grep -n x /etc/hosts",
        ] {
            assert!(search(passed).is_none(), "{passed}");
        }
    }

    #[test]
    fn secret_files_are_named_and_their_examples_are_not() {
        for secret in [
            ".env",
            "app/.env.local",
            "~/.ssh/id_rsa",
            "C:\\k\\server.pem",
            ".aws/credentials",
            ".npmrc",
            "my_private_key.txt",
        ] {
            assert!(names_secret(secret), "{secret}");
        }
        for plain in [
            ".env.example",
            "src/env.rs",
            "id_rsa.pub",
            "keys.md",
            "credentials.rs",
            "x.key.md",
            ".pem",
        ] {
            assert!(!names_secret(plain), "{plain}");
        }
        let segment = |command: &str| {
            parse(command)
                .into_iter()
                .find_map(|segment| secret_path(&segment))
        };
        assert_eq!(segment("cat .env").as_deref(), Some(".env"));
        assert_eq!(
            segment("Get-Content -Path app/.env.production").as_deref(),
            Some("app/.env.production")
        );
        assert_eq!(segment("printf x > .env").as_deref(), Some(".env"));
        assert_eq!(segment("git add .env").as_deref(), Some(".env"));
        assert!(segment("echo .env >> .gitignore").is_none());
        assert!(segment("git rm --cached .env").is_none());
        assert!(segment("Test-Path .env").is_none());
        assert!(segment("docker compose --env-file .env up").is_none());
    }

    #[test]
    fn changes_to_what_judges_the_agent_are_named() {
        let root = Path::new("/w");
        let protected = [PathBuf::from("/home/u/.config/ha"), root.join(".harness")];
        let write = |command: &str| {
            parse(command)
                .into_iter()
                .find_map(|segment| protected_write(&segment, root, &protected))
        };
        assert!(write("echo x > .harness/features.json").is_some());
        assert!(write("Set-Content /home/u/.config/ha/config.toml x").is_some());
        assert!(write("sed -i s/a/b/ /home/u/.config/ha/settings.json").is_some());
        assert!(write("rm .harness/x").is_some());
        assert!(
            write("cat .harness/features.json").is_none(),
            "reading is fine"
        );
        assert!(write("echo x > notes.md").is_none());
        // A coder's worktree inside ha's state: the worktree is its own to
        // change, the state beside it is not.
        let data = PathBuf::from("/data/ha");
        let worktree = data.join("worktrees/w1");
        let protected = [data.clone(), worktree.join(".harness")];
        let write = |command: &str| {
            parse(command)
                .into_iter()
                .find_map(|segment| protected_write(&segment, &worktree, &protected))
        };
        assert!(write("echo x > src/main.rs").is_none());
        assert!(write("rm notes.md").is_none());
        assert!(write("rm ../../harness/lessons.json").is_some());
        assert!(write("echo x > .harness/features.json").is_some());
    }

    #[test]
    fn recursive_deletes_and_their_targets_are_read() {
        let delete = |command: &str| parse(command).iter().find_map(recursive_delete);
        assert_eq!(
            delete("rm -rf build dist"),
            Some(vec!["build".to_owned(), "dist".to_owned()])
        );
        assert_eq!(delete("rm -fr src"), Some(vec!["src".to_owned()]));
        assert_eq!(
            delete("Remove-Item -Recurse -Force -Path old -Filter *.x"),
            Some(vec!["old".to_owned()])
        );
        assert_eq!(delete("rd /s /q old"), Some(vec!["old".to_owned()]));
        assert_eq!(
            delete("Remove-Item -Recurse -Path src,docs, notes"),
            Some(vec![
                "src".to_owned(),
                "docs".to_owned(),
                "notes".to_owned()
            ])
        );
        assert_eq!(
            delete("find tmp -name '*.o' -delete"),
            Some(vec!["tmp".to_owned()])
        );
        assert_eq!(delete("gci x | Remove-Item -Recurse"), Some(Vec::new()));
        assert!(delete("rm file.txt").is_none());
        assert!(delete("rm -Force file.txt").is_none());
        assert!(delete("Remove-Item file.txt").is_none());
    }

    fn spool(dir: &Path) -> crate::capture::ProcessSpoolConfig {
        crate::capture::ProcessSpoolConfig::new(
            dir.join("spool"),
            crate::capture::SpoolLimits::default(),
        )
    }

    fn git(root: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(root)
            .output()
            .expect("git");
        assert!(status.status.success());
    }

    #[tokio::test]
    async fn a_delete_passes_only_when_what_it_removes_can_be_had_back() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path().join("w");
        std::fs::create_dir_all(root.join("src/old")).expect("dirs");
        std::fs::write(root.join("src/old/a.rs"), "a").expect("file");
        git(&root, &["init", "-q"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "a"]);
        let spool = spool(dir.path());
        let refused = |command: &'static str| {
            let root = root.clone();
            let spool = spool.clone();
            async move {
                shell_refusal(
                    &root,
                    command,
                    &[],
                    harness_providers::CancellationToken::new(),
                    &spool,
                )
                .await
                .map(|refusal| refusal.reason)
            }
        };
        assert_eq!(refused("rm -rf src/old").await, None, "tracked and clean");
        assert_eq!(
            refused("rm -rf node_modules target/debug").await,
            None,
            "disposable"
        );
        assert!(
            refused("rm -rf build/../src/old/../..").await.is_some(),
            "not a way out of the build folder"
        );
        std::fs::write(root.join("src/old/b.rs"), "new").expect("untracked");
        let reason = refused("rm -rf src/old").await.expect("refused");
        assert!(
            reason.contains("could not give back") && reason.contains("b.rs"),
            "{reason}"
        );
        let reason = refused("rm -rf ../elsewhere").await.expect("refused");
        assert!(reason.contains("outside the workspace"), "{reason}");
        let reason = refused("cd src && rm -rf old").await.expect("refused");
        assert!(reason.contains("changes directory first"), "{reason}");
        let reason = refused("rm -rf $DIR").await.expect("refused");
        assert!(reason.contains("not a literal path"), "{reason}");
        assert!(
            refused("rm -rf .")
                .await
                .expect("refused")
                .contains("whole workspace")
        );
        assert!(reason.contains(ALLOW_RECURSIVE_DELETE));
    }
}
