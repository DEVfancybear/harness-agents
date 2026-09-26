//! The slash commands and the menu that offers them, after prime-agent's
//! `slash-commands.ts` and its autocomplete provider.
//!
//! One table names every built-in command, its aliases, the argument it takes and
//! the line the menu shows. Skills (`/skill:<name>`) and prompt commands join the
//! menu at run time. Matching is prime-agent's fuzzy match: the characters typed
//! must appear in order, and a match at a word boundary or in a run ranks higher,
//! so `/skil` offers the skills and `/mdl` finds `/model`.

use std::collections::BTreeMap;

/// One built-in slash command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlashCommand {
    pub name: &'static str,
    /// Other names that run the same command; searched, never listed.
    pub aliases: &'static [&'static str],
    /// The argument placeholder, empty for a command that takes none.
    pub arguments: &'static str,
    /// One line, short enough for one menu row.
    pub summary: &'static str,
    /// Values the argument menu offers, when they are fixed. A value may carry
    /// its own placeholder after its first word (`add <name> -- <command>`): the
    /// menu shows it whole, and choosing it types the first word and a space so
    /// the rest can be entered, instead of running the command without it.
    pub options: &'static [&'static str],
}

impl SlashCommand {
    /// The name as it is typed, with its placeholder when it takes an argument.
    #[must_use]
    pub fn usage(&self) -> String {
        if self.arguments.is_empty() {
            self.name.to_owned()
        } else {
            format!("{} {}", self.name, self.arguments)
        }
    }

    const fn takes_argument(&self) -> bool {
        !self.arguments.is_empty()
    }
}

const fn command(
    name: &'static str,
    arguments: &'static str,
    summary: &'static str,
) -> SlashCommand {
    SlashCommand {
        name,
        aliases: &[],
        arguments,
        summary,
        options: &[],
    }
}

/// Built-in commands in the order the menu and `/help` list them: prime-agent's
/// commands first, in prime-agent's order and words, then the ones only this app
/// has.
pub const SLASH_COMMANDS: [SlashCommand; 34] = [
    command("/model", "[search]", "Select model (opens selector UI)"),
    SlashCommand {
        aliases: &["/thinking"],
        options: &["off", "minimal", "low", "medium", "high", "xhigh", "max"],
        ..command(
            "/effort",
            "[level]",
            "Select reasoning/thinking level (opens selector UI)",
        )
    },
    command(
        "/export",
        "[path]",
        "Export session (Markdown default, or specify path: .md/.jsonl)",
    ),
    command("/copy", "", "Copy last agent message to clipboard"),
    SlashCommand {
        aliases: &["/rename"],
        ..command("/name", "[name]", "Set or show the session display name")
    },
    SlashCommand {
        aliases: &["/status"],
        ..command("/session", "", "Show session info")
    },
    SlashCommand {
        aliases: &["/usage"],
        ..command(
            "/context",
            "",
            "Show token, cost, and context usage for agent and sub-agents",
        )
    },
    command("/hotkeys", "", "Show all keyboard shortcuts"),
    command("/login", "[provider]", "Configure provider authentication"),
    command("/logout", "[provider]", "Remove provider authentication"),
    SlashCommand {
        options: &[
            "add <name> -- <command> [args...] | --url <url>",
            "list",
            "get <name>",
            "remove <name>",
        ],
        ..command(
            "/mcp",
            "[add|list|get|remove]",
            "Show MCP servers, or add, inspect and remove them",
        )
    },
    SlashCommand {
        aliases: &["/clear"],
        ..command("/new", "", "Start a new session")
    },
    command(
        "/compact",
        "[instructions]",
        "Compact the session context; optional instructions focus the summary",
    ),
    command(
        "/refine",
        "[--global] [--rollback <id>] [instructions]",
        "Refine continual harness prompt notes, skills, subagents, and memory",
    ),
    SlashCommand {
        options: &["status", "pause", "resume", "clear"],
        ..command(
            "/goal",
            "[objective]",
            "Set or view a persistent goal; supports pause, resume, and clear",
        )
    },
    command(
        "/resume",
        "[id]",
        "Open the session picker, or resume a session by id",
    ),
    command(
        "/reload",
        "",
        "Reload skills, prompt commands and instruction files",
    ),
    command("/help", "", "List every command"),
    command(
        "/more",
        "",
        "Reopen the recent transcript in a scrollable panel",
    ),
    command("/skills", "", "List discovered and active skills"),
    command(
        "/agents",
        "",
        "Show delegated workers, steps and current status",
    ),
    command(
        "/steer",
        "<text>",
        "Send a correction to the active run at its next safe step",
    ),
    SlashCommand {
        aliases: &["/permission", "/mode"],
        options: &["ask", "auto-edit", "full-auto"],
        ..command(
            "/permissions",
            "[mode]",
            "Choose the permission mode, or show it with the rules and auto-allowed count",
        )
    },
    command("/cost", "", "Show the session cost from model prices"),
    command(
        "/diff",
        "",
        "Show tracked changes since this session started",
    ),
    command(
        "/undo",
        "",
        "Restore the latest safe file change, after approval",
    ),
    command(
        "/config",
        "",
        "Show the resolved configuration and data files",
    ),
    command(
        "/hooks",
        "",
        "List trusted hook commands and where they come from",
    ),
    SlashCommand {
        options: &["yes"],
        ..command(
            "/trust",
            "[yes]",
            "Trust this project's config after explicit confirmation",
        )
    },
    command("/init", "", "Print a starter AGENTS.md sample"),
    command(
        "/image",
        "",
        "Paste a screenshot or a file path from the clipboard",
    ),
    command(
        "/attach",
        "<path>",
        "Attach a file: an image is shown to the model, text goes in the message",
    ),
    command(
        "/system-prompt",
        "",
        "Show the exact system prompt sent to the model",
    ),
    SlashCommand {
        aliases: &["/exit"],
        ..command("/quit", "", "Quit ha")
    },
];

/// The built-in command a typed name runs, resolving aliases.
#[must_use]
pub fn builtin(name: &str) -> Option<&'static SlashCommand> {
    SLASH_COMMANDS
        .iter()
        .find(|command| command.name == name || command.aliases.contains(&name))
}

/// The canonical name for a typed command: an alias becomes the command it runs,
/// anything else is returned unchanged.
#[must_use]
pub fn canonical(name: &str) -> &str {
    builtin(name).map_or(name, |command| command.name)
}

/// A command that joins the menu at run time: a skill or a prompt command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MenuCommand {
    /// As typed, with the slash: `/skill:review`, `/fix-issue`.
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    /// Where it comes from, shown dim after the description: `skill`, `prompt`.
    pub tag: &'static str,
}

/// One row of the menu.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MenuItem {
    /// What the row reads as: the command with its placeholder, or an argument.
    pub label: String,
    pub description: String,
    pub tag: Option<String>,
    /// The buffer once the row is accepted.
    pub completion: String,
}

impl MenuItem {
    /// The command or value the row is for, without its placeholder.
    #[cfg(test)]
    #[must_use]
    pub fn name(&self) -> &str {
        self.label.split(' ').next().unwrap_or_default()
    }
}

/// The built-in rows for a buffer, with no skills or run-time options: what the
/// tests compare against.
#[cfg(test)]
#[must_use]
pub fn matching(buffer: &str) -> Vec<MenuItem> {
    suggest(buffer, &[], &ArgumentOptions::new())
}

/// Values the argument menu offers for commands whose options change at run time
/// (`/model`, `/login`), keyed by the command's canonical name.
pub type ArgumentOptions = BTreeMap<&'static str, Vec<(String, String)>>;

/// The rows the menu offers for a buffer, best match first; empty when the buffer
/// is not a slash command being typed, or is already complete.
#[must_use]
pub fn suggest(buffer: &str, extra: &[MenuCommand], dynamic: &ArgumentOptions) -> Vec<MenuItem> {
    if !buffer.starts_with('/') || buffer.contains('\n') {
        return Vec::new();
    }
    match buffer.split_once(' ') {
        None => suggest_names(buffer, extra),
        Some((name, argument)) => suggest_arguments(name, argument, dynamic),
    }
}

fn suggest_names(buffer: &str, extra: &[MenuCommand]) -> Vec<MenuItem> {
    // A command typed in full, with nothing more to add, needs no menu: Enter runs it.
    let exact_builtin = builtin(buffer).filter(|command| command.name == buffer);
    if exact_builtin.is_some_and(|command| !command.takes_argument())
        || extra.iter().any(|command| command.name == buffer)
    {
        return Vec::new();
    }
    let query = &buffer[1..];
    let mut scored: Vec<(f64, usize, MenuItem)> = Vec::new();
    for (order, command) in SLASH_COMMANDS.iter().enumerate() {
        let names = std::iter::once(command.name).chain(command.aliases.iter().copied());
        let Some(score) = names
            .filter_map(|name| fuzzy_score(query, &name[1..]))
            .min_by(f64::total_cmp)
        else {
            continue;
        };
        let completion = if command.takes_argument() {
            format!("{} ", command.name)
        } else {
            command.name.to_owned()
        };
        scored.push((
            score,
            order,
            MenuItem {
                label: command.usage(),
                description: command.summary.to_owned(),
                tag: None,
                completion,
            },
        ));
    }
    for (index, command) in extra.iter().enumerate() {
        let Some(score) = fuzzy_score(query, &command.name[1..]) else {
            continue;
        };
        let label = if command.argument_hint.is_empty() {
            command.name.clone()
        } else {
            format!("{} {}", command.name, command.argument_hint)
        };
        scored.push((
            score,
            SLASH_COMMANDS.len() + index,
            MenuItem {
                label,
                description: command.description.clone(),
                tag: Some(command.tag.to_owned()),
                completion: format!("{} ", command.name),
            },
        ));
    }
    rank(scored, query.is_empty())
}

fn suggest_arguments(name: &str, argument: &str, dynamic: &ArgumentOptions) -> Vec<MenuItem> {
    // One word of argument is completed; a longer one is free text.
    if argument.contains(char::is_whitespace) {
        return Vec::new();
    }
    let Some(command) = builtin(name) else {
        return Vec::new();
    };
    let options: Vec<(String, String)> = match dynamic.get(command.name) {
        Some(options) => options.clone(),
        None => command
            .options
            .iter()
            .map(|option| ((*option).to_owned(), String::new()))
            .collect(),
    };
    // An option that names more input after its first word completes to that
    // word and a space: the argument is not finished, so accepting it must not
    // submit (`/mcp add` alone has no server to add).
    let word = |value: &str| {
        value
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    if options.iter().any(|(value, _)| word(value) == argument) {
        return Vec::new();
    }
    let scored = options
        .into_iter()
        .enumerate()
        .filter_map(|(order, (value, description))| {
            let head = word(&value);
            let score = fuzzy_score(argument, &head)?;
            let takes_more = value.trim() != head;
            Some((
                score,
                order,
                MenuItem {
                    completion: if takes_more {
                        format!("{name} {head} ")
                    } else {
                        format!("{name} {head}")
                    },
                    label: value,
                    description,
                    tag: None,
                },
            ))
        })
        .collect();
    rank(scored, argument.is_empty())
}

/// Best match first; with nothing typed yet, the table's own order.
fn rank(mut scored: Vec<(f64, usize, MenuItem)>, keep_order: bool) -> Vec<MenuItem> {
    if keep_order {
        scored.sort_by_key(|(_, order, _)| *order);
    } else {
        scored.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    }
    scored.into_iter().map(|(_, _, item)| item).collect()
}

/// prime-agent's `fuzzyMatch` (`packages/tui/src/fuzzy.ts`): every query character
/// must appear in order; lower is better. Space-separated tokens must all match.
#[must_use]
pub fn fuzzy_score(query: &str, text: &str) -> Option<f64> {
    let mut total = 0.0;
    for token in query.split_whitespace() {
        total += fuzzy_token(token, text)?;
    }
    Some(total)
}

fn fuzzy_token(query: &str, text: &str) -> Option<f64> {
    let query = query.to_lowercase();
    if let Some(score) = match_query(&query, text) {
        return Some(score);
    }
    // `gpt5` also finds `5gpt`-shaped names and the reverse, as prime-agent does.
    let split = query.find(|c: char| c.is_ascii_digit())?;
    let (head, tail) = query.split_at(split);
    let swapped = if !head.is_empty()
        && head.chars().all(|c| c.is_ascii_lowercase())
        && tail.chars().all(|c| c.is_ascii_digit())
    {
        format!("{tail}{head}")
    } else {
        let letters = query.find(|c: char| c.is_ascii_lowercase())?;
        let (digits, rest) = query.split_at(letters);
        if digits.is_empty()
            || !digits.chars().all(|c| c.is_ascii_digit())
            || !rest.chars().all(|c| c.is_ascii_lowercase())
        {
            return None;
        }
        format!("{rest}{digits}")
    };
    match_query(&swapped, text).map(|score| score + 5.0)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "positions are menu-sized; the score is a ranking, not a measurement"
)]
fn match_query(query: &str, text: &str) -> Option<f64> {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let query: Vec<char> = query.chars().collect();
    if query.is_empty() {
        return Some(0.0);
    }
    if query.len() > text.len() {
        return None;
    }
    let mut query_index = 0;
    let mut score = 0.0;
    let mut last_match: Option<usize> = None;
    let mut consecutive = 0.0;
    for (index, character) in text.iter().enumerate() {
        if query_index == query.len() {
            break;
        }
        if *character != query[query_index] {
            continue;
        }
        let boundary = index == 0 || " -_./:".contains(text[index - 1]);
        if last_match.is_some_and(|last| last + 1 == index) {
            consecutive += 1.0;
            score -= consecutive * 5.0;
        } else {
            consecutive = 0.0;
            if let Some(last) = last_match {
                score += (index - last - 1) as f64 * 2.0;
            }
        }
        if boundary {
            score -= 10.0;
        }
        score += index as f64 * 0.1;
        last_match = Some(index);
        query_index += 1;
    }
    if query_index < query.len() {
        return None;
    }
    if query == text {
        score -= 100.0;
    }
    Some(score)
}

/// The closest built-in command to a mistyped one, for "did you mean".
#[must_use]
pub fn closest(name: &str) -> Option<&'static str> {
    SLASH_COMMANDS
        .iter()
        .flat_map(|command| {
            std::iter::once(command.name)
                .chain(command.aliases.iter().copied())
                .map(move |candidate| (candidate, command.name))
        })
        .map(|(candidate, canonical)| (edit_distance(name, candidate), canonical))
        .filter(|(distance, _)| *distance <= 2)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, canonical)| canonical)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, left) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, right) in b.iter().enumerate() {
            let cost = usize::from(left != right);
            current.push(
                (previous[j] + cost)
                    .min(previous[j + 1] + 1)
                    .min(current[j] + 1),
            );
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::{ArgumentOptions, MenuCommand, canonical, closest, fuzzy_score, suggest};

    fn skill(name: &str) -> MenuCommand {
        MenuCommand {
            name: format!("/skill:{name}"),
            description: format!("the {name} skill"),
            argument_hint: String::new(),
            tag: "skill",
        }
    }

    #[test]
    fn a_partial_word_offers_every_skill() {
        let extra = [skill("review"), skill("debug")];
        let items = suggest("/skil", &extra, &ArgumentOptions::new());
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        assert!(labels.contains(&"/skill:review"), "{labels:?}");
        assert!(labels.contains(&"/skill:debug"), "{labels:?}");
        assert!(labels.contains(&"/skills"), "{labels:?}");
        let review = items
            .iter()
            .find(|item| item.label == "/skill:review")
            .unwrap();
        assert_eq!(review.completion, "/skill:review ");
        assert_eq!(review.tag.as_deref(), Some("skill"));
    }

    #[test]
    fn matching_is_fuzzy_and_ranks_the_closest_first() {
        let items = suggest("/mdl", &[], &ArgumentOptions::new());
        assert_eq!(
            items.first().map(|item| item.label.as_str()),
            Some("/model [search]")
        );
        assert!(fuzzy_score("xyz", "model").is_none());
    }

    #[test]
    fn an_alias_finds_its_command_and_runs_it() {
        let items = suggest("/thinki", &[], &ArgumentOptions::new());
        assert!(items.iter().any(|item| item.label.starts_with("/effort")));
        assert_eq!(canonical("/thinking"), "/effort");
        assert_eq!(canonical("/exit"), "/quit");
        assert_eq!(canonical("/unknown"), "/unknown");
    }

    #[test]
    fn a_command_that_takes_an_argument_completes_with_a_space_and_offers_values() {
        let items = suggest("/effo", &[], &ArgumentOptions::new());
        assert_eq!(items[0].completion, "/effort ");
        let levels = suggest("/effort h", &[], &ArgumentOptions::new());
        assert_eq!(levels[0].label, "high");
        assert_eq!(levels[0].completion, "/effort high");
        assert!(suggest("/effort high", &[], &ArgumentOptions::new()).is_empty());
    }

    #[test]
    fn run_time_options_replace_the_fixed_ones() {
        let mut dynamic = ArgumentOptions::new();
        dynamic.insert(
            "/model",
            vec![
                ("opencode/kimi-k2.6".to_owned(), "Kimi K2.6".to_owned()),
                ("deepseek/deepseek-v4-flash".to_owned(), String::new()),
            ],
        );
        let items = suggest("/model kimi", &[], &dynamic);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].completion, "/model opencode/kimi-k2.6");
    }

    #[test]
    fn a_complete_command_needs_no_menu() {
        assert!(suggest("/copy", &[], &ArgumentOptions::new()).is_empty());
        assert!(suggest("hello", &[], &ArgumentOptions::new()).is_empty());
    }

    #[test]
    fn a_typo_names_the_command_it_meant() {
        assert_eq!(closest("/modell"), Some("/model"));
        assert_eq!(closest("/exti"), Some("/quit"));
        assert_eq!(closest("/zzzzzzzz"), None);
    }
}
