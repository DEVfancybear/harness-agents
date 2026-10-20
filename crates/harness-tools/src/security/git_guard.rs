//! Destructive-git discard guard, ported from prime-agent's
//! `pa-core/src/tools/bash_guard.rs` (its bash tool's dirty-tree refusal).
//!
//! Detects git commands that discard uncommitted working-tree changes
//! (`git checkout -- .`, `git clean -f...`, `git reset --hard`,
//! `git restore .`), resolves which repository each discard targets
//! (cd chains, `git -C`, inline env assignments), and formats the
//! dirty-tree refusal. [`refusal`] runs the probes for a shell command.
#![allow(
    clippy::cast_possible_wrap,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::collapsible_if,
    reason = "ported from prime-agent as it is"
)]

/// Bypass env var for the destructive-git dirty-tree guard.
pub(crate) const BASH_DESTRUCTIVE_GIT_BYPASS_ENV: &str = "HA_ALLOW_DESTRUCTIVE_GIT";

/// prime-agent's probe for uncommitted work.
const GIT_STATUS_PORCELAIN_COMMAND: &str = "git status --porcelain --untracked-files=all";

/// prime-agent's refusal for a discard whose repository cannot be checked.
const RELOCATION_REFUSAL: &str = "Refusing to run this destructive git command: it changes directory (or repository) first, and the uncommitted changes of the repository it targets cannot be checked safely.\n\nRun the discard as its own command from the target directory, or set HA_ALLOW_DESTRUCTIVE_GIT=1.";

/// How many dirty paths the refusal lists before eliding the rest.
pub(crate) const MAX_DIRTY_PATHS_LISTED: usize = 10;

/// JavaScript's `\s` character class.
fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\t' | '\n' | '\x0B' | '\x0C' | '\r' | ' ' | '\u{00A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// JS `str.split(/\s+/)` (leading/trailing empty tokens removed).
///
/// A plain character split: it runs on every shell command the model sends,
/// and compiling a regex for it each time cost more than the split.
fn split_ws(text: &str) -> Vec<&str> {
    text.split(is_js_whitespace)
        .filter(|part| !part.is_empty())
        .collect()
}

/// git global options between `git` and the subcommand.
const GIT_GLOBAL_OPTIONS: &str = r#"(?:-{1,2}[^\s;&|]+(?:\s+(?:"[^"]*"|'[^']*'|[^\s;&|]+))?\s+)*"#;

/// A regex compiled once per process. Every `run_shell` call is checked
/// against these, and a backtracking regex this size costs far more to
/// compile than to run.
type Pattern = std::sync::LazyLock<fancy_regex::Regex>;

static DISCARD_CHECKOUT: Pattern = Pattern::new(|| {
    fancy_regex::Regex::new(&format!(
        r"\bgit\s+{GIT_GLOBAL_OPTIONS}checkout\s+(?:(?:(?:-[fm]|--ours|--theirs|--conflict=\S+)\s+)*(?:--\s+)?(?:\.\/?|:\/)|[^\s;&|()]+\s+(?:--\s+)?(?:\.\/?|:\/)|(?:-f|--force)\s+[^\s;&|()]+)(?=\s|$|[;&|)])"
    ))
    .expect("checkout discard regex")
});

static DISCARD_RESTORE: Pattern = Pattern::new(|| {
    fancy_regex::Regex::new(&format!(
        r"\bgit\s+{GIT_GLOBAL_OPTIONS}restore\s+(?:(?:--source|--worktree)(?:=\S+)?\s+|-s(?:\s+\S+|[^\s]+)\s+|-W\s+|--\s+)?(?:\.\/?|:\/)(?=\s|$|[;&|)])"
    ))
    .expect("restore discard regex")
});

static DISCARD_RESET: Pattern = Pattern::new(|| {
    fancy_regex::Regex::new(&format!(
        r"\bgit\s+{GIT_GLOBAL_OPTIONS}reset\s+(?:(?:-[^\s;&|]+)\s+)*--hard\b"
    ))
    .expect("reset discard regex")
});

static DISCARD_CLEAN: Pattern = Pattern::new(|| {
    fancy_regex::Regex::new(&format!(r"\bgit\s+{GIT_GLOBAL_OPTIONS}clean\s+([^;&|]*)"))
        .expect("clean discard regex")
});

static ASSIGNMENT: Pattern = Pattern::new(|| {
    fancy_regex::Regex::new(r#"^[A-Za-z_][A-Za-z0-9_]*=[^\s$`;&|()<>"!]+$"#)
        .expect("assignment regex")
});

static CD: Pattern = Pattern::new(|| fancy_regex::Regex::new(r"\b(cd|pushd)\b").expect("cd regex"));

fn is_forced_clean_segment(args: &str) -> bool {
    let tokens: Vec<&str> = split_ws(args);
    let option_end = tokens.iter().position(|t| *t == "--");
    let option_tokens: &[&str] = match option_end {
        Some(end) => &tokens[..end],
        None => &tokens,
    };
    let forces: Vec<&&str> = option_tokens
        .iter()
        .filter(|arg| {
            if arg.starts_with("--") {
                arg.starts_with("--force")
            } else {
                arg.starts_with('-') && arg.contains('f')
            }
        })
        .collect();
    if forces.is_empty() {
        return false;
    }
    !option_tokens.iter().any(|arg| {
        *arg == "--dry-run" || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('n'))
    })
}

/// Replace characters inside single- or double-quoted spans with spaces so the
/// discard matcher cannot match quoted data. Character positions stay
/// identical to the original string. Command substitution (`$(...)`,
/// backticks) is left live because it executes. Unquoted `#` at a word
/// boundary starts a comment, masked to end of line.
fn mask_quoted_spans(command: &str) -> Vec<char> {
    let mut chars: Vec<char> = command.chars().collect();
    let mut quote: Option<char> = None;
    let len = chars.len();
    let mut i = 0usize;
    while i < len {
        let ch = chars[i];
        if quote.is_none() {
            let prev = if i > 0 { Some(chars[i - 1]) } else { None };
            if ch == '#'
                && (i == 0
                    || prev.is_none_or(|p| {
                        p.is_whitespace() || matches!(p, ';' | '&' | '|' | '(' | ')' | '{' | '}')
                    }))
            {
                let mut j = i;
                while j < len && chars[j] != '\n' {
                    chars[j] = ' ';
                    j += 1;
                }
                i = j;
                continue;
            }
            if ch == '"' || ch == '\'' {
                quote = Some(ch);
            }
        } else if quote == Some('\'') {
            if ch == '\'' {
                quote = None;
            } else {
                chars[i] = ' ';
            }
        } else if ch == '"' {
            quote = None;
        } else if ch == '\\' && i + 1 < len {
            chars[i] = ' ';
            chars[i + 1] = ' ';
            i += 1;
        } else if ch == '$' && i + 1 < len && chars[i + 1] == '(' {
            let mut depth = 0i32;
            let mut j = i;
            while j < len {
                if chars[j] == '(' {
                    depth += 1;
                } else if chars[j] == ')' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                j += 1;
            }
            i = j.saturating_sub(1);
        } else if ch == '`' {
            let mut j = i + 1;
            while j < len && chars[j] != '`' {
                j += 1;
            }
            i = j.saturating_sub(1);
        } else {
            chars[i] = ' ';
        }
        i += 1;
    }
    chars
}

/// Find every destructive git discard command in `command`, returning the
/// byte index where each `git` token starts (empty when none match).
pub fn find_destructive_git_discard_commands(command: &str) -> Vec<usize> {
    let masked: String = mask_quoted_spans(command).into_iter().collect();
    let mut indices: Vec<usize> = Vec::new();
    // A command that never names git cannot hold a discard; most shell
    // commands are builds and tests, so this skips the regexes for them.
    if !masked.contains("git") {
        return indices;
    }
    for pattern in [&DISCARD_CHECKOUT, &DISCARD_RESTORE, &DISCARD_RESET] {
        for m in pattern.find_iter(&masked).flatten() {
            indices.push(m.start());
        }
    }
    for caps in DISCARD_CLEAN.captures_iter(&masked).flatten() {
        if let Some(args) = caps.get(1) {
            if is_forced_clean_segment(args.as_str()) {
                indices.push(caps.get(0).map(|m| m.start()).unwrap_or_default());
            }
        }
    }
    indices.sort_unstable();
    indices
}

/// Where a discard command's probe must run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscardProbeTarget {
    /// Shell prefix relocating the probe, for example `cd sub && `.
    pub relocation_prefix: Option<String>,
    /// git status command for this discard.
    pub git_status_command: String,
}

/// Result of resolving a discard command's probe location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiscardProbeResolution {
    /// No relocation is needed; probe in the tool cwd.
    NotRelocated,
    /// The target repository cannot be resolved safely; refuse.
    Unresolvable,
    /// Probe must run with the given relocation.
    Target(DiscardProbeTarget),
}

/// Shell separators treated by segment splitting.
const SEPARATORS: [&str; 5] = ["&&", "||", ";", "|", "\n"];

/// Split keeping separators (TS: `split(/(&&|\|\||;|\||\n)/)`).
///
/// One pass over the bytes: every separator is ASCII, so a match can only
/// start on a character boundary and the slices between them stay valid UTF-8.
fn split_with_separators(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut segment_start = 0usize;
    let mut index = 0usize;
    while index < text.len() {
        if let Some(separator) = SEPARATORS
            .iter()
            .find(|separator| text.as_bytes()[index..].starts_with(separator.as_bytes()))
        {
            if segment_start < index {
                parts.push(text[segment_start..index].to_owned());
            }
            parts.push((*separator).to_owned());
            index += separator.len();
            segment_start = index;
        } else {
            index += 1;
        }
    }
    if segment_start < text.len() {
        parts.push(text[segment_start..].to_owned());
    }
    parts
}

/// Split on separators, dropping them (TS: `split(/&&|\|\||;|\||\n/)`).
fn split_on_separators(text: &str) -> Vec<String> {
    split_with_separators(text)
        .into_iter()
        .filter(|part| !SEPARATORS.contains(&part.as_str()))
        .collect()
}

/// Port of `resolveDiscardProbeTarget` from bash.ts.
pub fn resolve_discard_probe_target(
    command: &str,
    discard_index: usize,
    user_command_start: usize,
) -> DiscardProbeResolution {
    let prefix = &command[..discard_index];
    let invocation = &command[discard_index..];
    // A discard inside the configured command prefix would be replayed by the
    // probe itself; refuse instead of executing it during probing.
    if user_command_start > 0 && discard_index < user_command_start {
        return DiscardProbeResolution::Unresolvable;
    }
    let tokens = split_ws(invocation);

    // git -C <dir> (or repository-relocating global options) on the discard
    // invocation itself.
    let mut dash_c_dir: Option<String> = None;
    let mut subcommand_index: Option<usize> = None;
    for (index, token) in tokens.iter().enumerate() {
        if index == 0 {
            continue; // "git"
        }
        if *token == "reset" || *token == "checkout" || *token == "clean" || *token == "restore" {
            subcommand_index = Some(index);
            break;
        }
        if *token == "-C" {
            let dir = tokens.get(index + 1).copied();
            // A quoted, escaped, or substituted path cannot be replayed as a
            // single token; refuse rather than probe a truncated directory.
            let Some(dir) = dir else {
                return DiscardProbeResolution::Unresolvable;
            };
            if dir.contains(['"', '\'', '\\', '$', '`']) {
                return DiscardProbeResolution::Unresolvable;
            }
            // Repeated -C paths are relative to the preceding one.
            dash_c_dir = Some(match dash_c_dir {
                Some(existing) => format!("{existing} -C {dir}"),
                None => dir.to_string(),
            });
        } else if token.starts_with("--git-dir")
            || token.starts_with("--work-tree")
            || token.starts_with("--prefix")
        {
            return DiscardProbeResolution::Unresolvable;
        } else if *token == "-c" {
            let config = tokens.get(index + 1).copied();
            // core.worktree/core.bare relocate the repository.
            if let Some(config) = config {
                if config.starts_with("core.worktree") || config.starts_with("core.bare") {
                    let relocates = config
                        .strip_prefix("core.worktree")
                        .or_else(|| config.strip_prefix("core.bare"))
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with('='));
                    if relocates {
                        return DiscardProbeResolution::Unresolvable;
                    }
                }
            }
        }
        // Other flags do not relocate.
    }

    // git clean -x/-X also deletes ignored files, so its probe includes them.
    let mut clean_removes_ignored = false;
    if let Some(sub_index) = subcommand_index {
        if tokens[sub_index] == "clean" {
            for token in &tokens[sub_index + 1..] {
                if *token == "--" {
                    break; // everything after -- is a pathspec
                }
                if token.starts_with("--") {
                    continue;
                }
                if token.starts_with('-') && token[1..].contains(['x', 'X']) {
                    clean_removes_ignored = true;
                    break;
                }
            }
        }
    }

    // Inline env assignments directly before the git invocation relocate the
    // target repository; replay them in the probe, or refuse when they cannot.
    let last_segment = split_on_separators(prefix).pop().unwrap_or_default();
    let leading_tokens: Vec<&str> = split_ws(last_segment.trim());
    for token in &leading_tokens {
        if ASSIGNMENT.is_match(token).unwrap_or(false) {
            continue; // replayable assignment
        }
        // Wrappers that cannot change directory or select another repository.
        if *token == "sudo"
            || *token == "env"
            || *token == "command"
            || *token == "builtin"
            || token.ends_with('/')
        {
            continue;
        }
        return DiscardProbeResolution::Unresolvable;
    }
    let assignments: Vec<&str> = leading_tokens
        .iter()
        .copied()
        .filter(|token| token.contains('='))
        .collect();
    let env_prefix = if assignments.is_empty() {
        String::new()
    } else {
        format!("{} ", assignments.join(" "))
    };

    // cd relocations earlier in the command.
    let has_cd = CD.is_match(prefix).unwrap_or(false) || prefix.contains('(');
    let mut persistent_cd_args: Vec<String> = Vec::new();
    let mut grouped_cd_args: Vec<String> = Vec::new();
    let mut saw_cd = false;
    let mut paren_depth: i64 = 0;
    let mut cd_pending_separator = false;
    if has_cd {
        let mut offset = 0usize;
        for part in split_with_separators(prefix) {
            let start = offset;
            offset += part.len();
            if start < user_command_start {
                continue; // command-prefix region: replayed as-is
            }
            let is_separator = SEPARATORS.contains(&part.as_str());
            if is_separator {
                if cd_pending_separator && (part == ";" || part == "\n") {
                    // The discard's directory depends on the cd succeeding.
                    return DiscardProbeResolution::Unresolvable;
                }
                if part == "||" || part == "|" {
                    if saw_cd {
                        return DiscardProbeResolution::Unresolvable;
                    }
                    continue;
                }
                cd_pending_separator = false;
                continue;
            }
            let trimmed = part.trim();
            let opens = part.matches('(').count() as i64;
            let closes = part.matches(')').count() as i64;
            let inside_group = paren_depth > 0 || opens > 0;
            paren_depth = 0.max(paren_depth + opens - closes);
            if inside_group {
                let stripped: String = trimmed
                    .trim_start_matches(|c: char| c == '(' || c.is_whitespace())
                    .trim_end_matches(|c: char| c == ')' || c.is_whitespace())
                    .to_string();
                if let Some(arg) = stripped.strip_prefix("cd") {
                    let arg = arg.trim();
                    if !arg.is_empty()
                        && arg.contains(['$', '`', ';', '&', '|', '(', ')', '<', '>', '#', '"'])
                    {
                        return DiscardProbeResolution::Unresolvable;
                    }
                    saw_cd = true;
                    cd_pending_separator = true;
                    grouped_cd_args.push(arg.to_string());
                } else if CD.is_match(trimmed).unwrap_or(false) {
                    return DiscardProbeResolution::Unresolvable;
                }
                if paren_depth == 0 {
                    grouped_cd_args.clear();
                }
                continue;
            }
            if trimmed == "pushd" || trimmed.starts_with("pushd ") {
                return DiscardProbeResolution::Unresolvable;
            }
            let Some(arg) = trimmed.strip_prefix("cd") else {
                cd_pending_separator = false;
                continue; // not a cd: cannot change cwd
            };
            let arg = arg.trim();
            // An arg we cannot replay safely leaves the target repository
            // unknown; refuse rather than probe blindly.
            let balanced = arg.matches('"').count() % 2 == 0 && arg.matches('\'').count() % 2 == 0;
            if !balanced
                || (!arg.is_empty()
                    && arg.contains(['$', '`', ';', '&', '|', '(', ')', '<', '>', '#']))
            {
                return DiscardProbeResolution::Unresolvable;
            }
            saw_cd = true;
            cd_pending_separator = true;
            persistent_cd_args.push(arg.to_string());
        }
    }
    let cd_args = if paren_depth > 0 {
        let mut args = persistent_cd_args.clone();
        args.extend(grouped_cd_args);
        args
    } else {
        persistent_cd_args
    };

    if cd_args.is_empty() && dash_c_dir.is_none() && !clean_removes_ignored && env_prefix.is_empty()
    {
        return DiscardProbeResolution::NotRelocated;
    }
    let ignored = if clean_removes_ignored {
        " --ignored=matching"
    } else {
        ""
    };
    let cd_prefix = if cd_args.is_empty() {
        String::new()
    } else {
        let joins = cd_args
            .iter()
            .map(|arg| {
                if arg.is_empty() {
                    "cd".to_string()
                } else {
                    format!("cd {arg}")
                }
            })
            .collect::<Vec<_>>()
            .join(" && ");
        format!("{joins} && ")
    };
    let relocation = format!("{cd_prefix}{env_prefix}");
    DiscardProbeResolution::Target(DiscardProbeTarget {
        relocation_prefix: if relocation.is_empty() {
            None
        } else {
            Some(relocation)
        },
        git_status_command: format!(
            "{}status --porcelain --untracked-files=all{ignored}",
            match dash_c_dir.as_deref() {
                Some(dir) => format!("git -C {dir} "),
                None => "git ".to_string(),
            }
        ),
    })
}

/// prime-agent's bash-tool check before a shell command runs: the reason to
/// refuse it when it would discard uncommitted work, else `None`. The
/// probe runs `git status` where each discard points (its cd chain or
/// `git -C`), and a probe that fails (no repository) refuses nothing.
pub(crate) async fn refusal(
    root: &std::path::Path,
    command: &str,
    cancellation: harness_providers::CancellationToken,
    spool: &crate::capture::ProcessSpoolConfig,
) -> Option<String> {
    let discards = find_destructive_git_discard_commands(command);
    if discards.is_empty()
        || is_truthy_env_value(std::env::var(BASH_DESTRUCTIVE_GIT_BYPASS_ENV).ok().as_ref())
    {
        return None;
    }
    let mut probes: Vec<(String, bool)> = Vec::new();
    for index in discards {
        let probe = match resolve_discard_probe_target(command, index, 0) {
            DiscardProbeResolution::Unresolvable => return Some(RELOCATION_REFUSAL.to_owned()),
            DiscardProbeResolution::NotRelocated => {
                (GIT_STATUS_PORCELAIN_COMMAND.to_owned(), false)
            }
            DiscardProbeResolution::Target(target) => (
                format!(
                    "{}{}",
                    target.relocation_prefix.as_deref().unwrap_or(""),
                    target.git_status_command
                ),
                target.git_status_command.contains("--ignored=matching"),
            ),
        };
        if !probes.contains(&probe) {
            probes.push(probe);
        }
    }
    for (probe, includes_ignored) in probes {
        // A probe without a cd chain or env assignments is a bare `git status`
        // whose tokens carry no quotes, escapes or substitutions (the resolver
        // refuses those), so git is started directly with the same tokens. On
        // Windows the shell is PowerShell, whose start-up costs several times
        // the status itself; only a relocation still needs it to replay `cd`.
        let tokens = split_ws(&probe);
        // A relocation is a `cd ... && ` chain or env assignments before `git`.
        let direct = !probe.contains("&&") && tokens.first() == Some(&"git");
        let run = if direct {
            let args = tokens[1..]
                .iter()
                .map(|token| (*token).to_owned())
                .collect::<Vec<_>>();
            crate::process::run_internal_git(root, &args, 15_000, cancellation.clone(), spool).await
        } else {
            crate::process::run_internal_shell(root, &probe, 15_000, cancellation.clone(), spool)
                .await
        };
        let Ok(result) = run else {
            continue;
        };
        if result.exit_code != Some(0) {
            continue;
        }
        let dirty = result
            .stdout
            .lines()
            .map(|line| line.trim_end_matches('\r'))
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !dirty.is_empty() {
            return Some(format_dirty_tree_refusal(&dirty, includes_ignored));
        }
    }
    None
}

pub(crate) fn is_truthy_env_value(value: Option<&String>) -> bool {
    match value {
        Some(v) => !v.is_empty() && v != "0",
        None => false,
    }
}

pub(crate) fn format_dirty_tree_refusal(
    dirty_paths: &[String],
    includes_ignored_files: bool,
) -> String {
    let listed_count = dirty_paths.len().min(MAX_DIRTY_PATHS_LISTED);
    let listed = &dirty_paths[..listed_count];
    let elided = dirty_paths.len() - listed_count;
    let noun = if includes_ignored_files {
        "uncommitted or ignored file(s)"
    } else {
        "uncommitted change(s)"
    };
    let mut lines = vec![format!(
        "Refusing to run this destructive git command: the working tree has {} {noun}.",
        dirty_paths.len()
    )];
    lines.extend(listed.iter().map(|line| format!("  {line}")));
    if elided > 0 {
        lines.push(format!("  ... and {elided} more"));
    }
    lines.push(String::new());
    lines.push("Commit, stash, or stage your work first.".to_string());
    lines.push(format!(
        "To discard these changes intentionally, set {BASH_DESTRUCTIVE_GIT_BYPASS_ENV}=1."
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_keep_separators_and_multibyte_text_in_one_pass() {
        assert_eq!(
            split_with_separators("cd é && git reset --hard; ls|wc\nx||y"),
            [
                "cd é ",
                "&&",
                " git reset --hard",
                ";",
                " ls",
                "|",
                "wc",
                "\n",
                "x",
                "||",
                "y"
            ]
        );
        assert_eq!(split_on_separators("a&&b"), ["a", "b"]);
        assert!(split_with_separators("").is_empty());
        assert_eq!(
            split_ws("  git\u{3000}status \t-s  "),
            ["git", "status", "-s"]
        );
    }

    #[test]
    fn discards_are_found_and_quoted_text_is_not() {
        assert_eq!(
            find_destructive_git_discard_commands("git reset --hard"),
            [0]
        );
        assert_eq!(
            find_destructive_git_discard_commands("cargo test && git checkout -- ."),
            [14]
        );
        assert_eq!(
            find_destructive_git_discard_commands("git clean -fd").len(),
            1
        );
        assert!(find_destructive_git_discard_commands("git clean -nfd").is_empty());
        assert!(find_destructive_git_discard_commands("echo 'git reset --hard'").is_empty());
        assert!(find_destructive_git_discard_commands("cargo test --workspace").is_empty());
    }

    #[test]
    fn probes_follow_cd_chains_and_dash_c() {
        assert_eq!(
            resolve_discard_probe_target("git reset --hard", 0, 0),
            DiscardProbeResolution::NotRelocated
        );
        let command = "cd sub && git -C inner clean -xfd";
        let index = command.find("git").unwrap();
        assert_eq!(
            resolve_discard_probe_target(command, index, 0),
            DiscardProbeResolution::Target(DiscardProbeTarget {
                relocation_prefix: Some("cd sub && ".to_owned()),
                git_status_command:
                    "git -C inner status --porcelain --untracked-files=all --ignored=matching"
                        .to_owned(),
            })
        );
        let command = "cd $HOME && git reset --hard";
        let index = command.find("git").unwrap();
        assert_eq!(
            resolve_discard_probe_target(command, index, 0),
            DiscardProbeResolution::Unresolvable
        );
    }
}
