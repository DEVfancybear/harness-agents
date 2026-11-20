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
    }
}

/// Built-in commands in the order the menu and `/help` list them: prime-agent's
/// commands first, in prime-agent's order and words, then the ones only this app
/// has.
pub const SLASH_COMMANDS: [SlashCommand; 75] = [
    command("/model", "[search]", "Select model (opens selector UI)"),
    SlashCommand {
        aliases: &["/thinking"],
        ..command(
            "/effort",
            "[level]",
            "Select reasoning/thinking level (opens selector UI)",
        )
    },
    command(
        "/export",
        "[path]",
        "Export session (HTML default, or specify path: .html/.md/.jsonl)",
    ),
    command(
        "/share",
        "",
        "Share session as a secret GitHub gist (needs the gh CLI)",
    ),
    command(
        "/heartbeat",
        "[every <dur>] [--steer|--follow-up] <instr>",
        "Add a recurring heartbeat to this session",
    ),
    command(
        "/heartbeats",
        "[pause|resume|delete <id>]",
        "View and manage this session's heartbeats",
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
    command(
        "/mcp",
        "[add|list|get|remove|login|logout]",
        "Show MCP servers, or add, inspect and remove them",
    ),
    SlashCommand {
        aliases: &["/packages"],
        ..command(
            "/package",
            "[install <source>|remove <source>|update [source]|list] [--local]",
            "Install, remove, update and list packages of skills, prompts and themes",
        )
    },
    command(
        "/plugins",
        "[search]",
        "Browse and connect external services",
    ),
    SlashCommand {
        aliases: &["/clear"],
        ..command(
            "/new",
            "[--name <name>] [[--] prompt]",
            "Start a new session, optionally named and with a first prompt",
        )
    },
    command(
        "/compact",
        "[instructions]",
        "Compact the session context; optional instructions focus the summary",
    ),
    command(
        "/refine",
        "[--global] [--rollback <id>] [--curate] [instructions]",
        "Refine continual harness prompt notes, skills, subagents, and memory",
    ),
    command(
        "/learn",
        "[focus]",
        "Distill what this conversation taught into learned skills now",
    ),
    command(
        "/refinements",
        "[on|off]",
        "Show or set harness refinements: the memory digest, the memory tool, refine and learn (global)",
    ),
    command(
        "/factory",
        "[on|off|status]",
        "Show or set prime-agent's opt-in factory: state machines of child agents in the Python kernel",
    ),
    command(
        "/jev",
        "[on|off|status] | <decision> on|off",
        "Show or set the jev plugin (off by default) and its ten harness decisions (/jev status lists them)",
    ),
    command(
        "/bank",
        "[on|off|status]",
        "Show or set the action bank (on by default): calls the model fully decided run without a model step",
    ),
    command(
        "/codemode",
        "[on|off|status]",
        "Show or set codemode (on by default): one JavaScript program that calls the tools, and only what it prints comes back",
    ),
    command(
        "/knowledge",
        "[status|list|search <query>|read <path>|graph|refresh|bootstrap [focus]|on|off] [--global]",
        "Manage topic knowledge, sources and links shared across conversations; bootstrap has the agent map the project into it",
    ),
    command(
        "/checks",
        "[list|accept <name>|reject <name>]",
        "Review the checks learning proposed; accepted ones join [verify]",
    ),
    command(
        "/memory",
        "[queue] [list|show <id>|accept <id>...|all|reject <id>...|all]",
        "Review the memory changes that wait for you; accepted ones join memory",
    ),
    command(
        "/strategy",
        "",
        "Show the task's strategy, its evaluations and the decisions to keep or switch it",
    ),
    command(
        "/doctor",
        "",
        "Audit how ready this repository is for an agent, subsystem by subsystem",
    ),
    command(
        "/handoff",
        "[--reset] [focus]",
        "Write the handoff for the next session to .harness/progress.md; --reset continues in a fresh conversation",
    ),
    command(
        "/recall",
        "[n] [topic]",
        "Report what the last n session logs (3 by default) and the logs about a topic left",
    ),
    command(
        "/preserve",
        "[focus]",
        "Fold what this session settled into AGENTS.md (or CLAUDE.md), within 200 lines",
    ),
    command(
        "/verify",
        "",
        "Run the project's [verify] checks now and show what passed",
    ),
    command(
        "/review",
        "[focus]",
        "Have an independent verifier judge the work in this workspace",
    ),
    command(
        "/self-eval",
        "[focus]",
        "Score the last finished turn on five quality axes and show the report",
    ),
    command(
        "/features",
        "",
        "Show the project's feature list and what is verified",
    ),
    command(
        "/goal",
        "[objective]",
        "Set or view a persistent goal; supports pause, resume, and clear",
    ),
    command(
        "/autonomous",
        "[status|off|on [--max-continuations <n>] [--max-turns <n>] [--max-tokens <n>] [--timeout-ms <n>] [--gate <command>]]",
        "Set or view autonomous mode with an optional budget",
    ),
    command(
        "/schedule",
        "[list|add <when> -- <prompt>|pause|resume|cancel <id>]",
        "Schedule prompts for this conversation (in 10m, every 1h, at <date>, cron)",
    ),
    command(
        "/resume",
        "[selector]",
        "Open the session picker, or resume by id/name/latest; --compact summarizes first",
    ),
    command(
        "/reload",
        "",
        "Reload skills, prompt commands and instruction files",
    ),
    command(
        "/fullscreen",
        "[on|off]",
        "Toggle fullscreen (alternate screen) rendering with scrollable transcript",
    ),
    command("/help", "", "List every command"),
    command(
        "/more",
        "",
        "Reopen the recent transcript in a scrollable panel",
    ),
    command("/skills", "", "List discovered and active skills"),
    command(
        "/bug-report",
        "",
        "Write this conversation's diagnostics, logs and store records to one zip",
    ),
    command(
        "/import",
        "<path.jsonl>",
        "Import and resume a session from a JSONL file",
    ),
    command(
        "/agents",
        "",
        "Open the agents view (sessions and subagents); /agents stop [name] stops children",
    ),
    command("/fast", "", "Toggle OpenAI Fast mode"),
    command(
        "/tier",
        "[default|flex|priority|auto]",
        "Show or set the service tier",
    ),
    command(
        "/subagent-effort",
        "[level|inherit]",
        "Show or set the thinking level delegated children run at",
    ),
    command(
        "/subagent-login",
        "[provider]",
        "Log the delegated children in to a provider with their own account",
    ),
    command(
        "/subagent-logout",
        "[provider]",
        "Remove the delegated children's own login for a provider",
    ),
    command(
        "/subagent-model",
        "[<provider/model>|inherit]",
        "Show or set the model delegated children run on (subagentDefaultModel)",
    ),
    command(
        "/rlm-max-depth",
        "[<int> [--global]]",
        "Set/view the per-chat persistent RLM max depth immediately; never interrupts or queues the running turn",
    ),
    command(
        "/steer",
        "<text>",
        "Send a correction to the active run at its next safe step",
    ),
    SlashCommand {
        aliases: &["/followup"],
        ..command(
            "/queue",
            "[text|list|edit|drop|up|down]",
            "Queue a follow-up for after the active run, or list and edit the queue",
        )
    },
    command(
        "/scoped-models",
        "[pattern...|clear]",
        "Choose the models /model next and Alt+M cycle through",
    ),
    command(
        "/stash",
        "",
        "Put the draft aside, or bring it back (Ctrl-S)",
    ),
    command(
        "/fork",
        "[number]",
        "Start a new conversation before one of your messages",
    ),
    command(
        "/clone",
        "",
        "Start a new conversation with this one's whole history",
    ),
    command(
        "/tree",
        "[number [--summarize [focus]]|label <number> [text]]",
        "Show this conversation's turns, continue after one (summarizing the rest), or label one",
    ),
    SlashCommand {
        aliases: &["/side"],
        ..command(
            "/btw",
            "<question>",
            "Ask a side question about the conversation without adding it to the session",
        )
    },
    SlashCommand {
        aliases: &["/permission", "/mode"],
        ..command(
            "/permissions",
            "[mode]",
            "Choose the permission mode, or show it with the rules and auto-allowed count",
        )
    },
    command("/cost", "", "Show the session cost from model prices"),
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
    command(
        "/trust",
        "[yes]",
        "Trust this project's config after explicit confirmation",
    ),
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
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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

/// Values the argument menu offers that change at run time: a command's
/// canonical name keys its first word's (`/model`, `/login`), a key of
/// [`values`] the records a later word takes (`/memory queue accept <id>`).
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

/// The rows for the word being typed after a command: the values its grammar
/// allows there ([`slot`]), the best match first.
fn suggest_arguments(name: &str, argument: &str, dynamic: &ArgumentOptions) -> Vec<MenuItem> {
    let Some(command) = builtin(name) else {
        return Vec::new();
    };
    // The word being typed is what follows the last blank; the words before it
    // say which values it may take.
    let start = argument
        .char_indices()
        .rfind(|(_, character)| character.is_whitespace())
        .map_or(0, |(at, character)| at + character.len_utf8());
    let (before, typed) = argument.split_at(start);
    let words: Vec<&str> = before.split_whitespace().collect();
    let slot = slot(command.name, &words, dynamic);
    if slot.typed_only && typed.is_empty() {
        return Vec::new();
    }
    let head = |value: &str| {
        value
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    let mut options = slot.values;
    if typed.starts_with('-') {
        options.extend(slot.flags);
    }
    // A value already on the line is not offered again (a second id, a flag).
    options.retain(|(value, _)| !words.contains(&head(value).as_str()));
    // A value typed in full needs no menu: Enter runs it.
    if options.iter().any(|(value, _)| head(value) == typed) {
        return Vec::new();
    }
    let query = typed.to_lowercase();
    let scored = options
        .into_iter()
        .enumerate()
        .filter_map(|(order, (value, description))| {
            let first = head(&value);
            let score = if slot.free {
                first.to_lowercase().starts_with(&query).then_some(0.0)
            } else {
                fuzzy_score(typed, &first)
            }
            .or_else(|| {
                // A record is found by the words of its description too, after
                // every value its own name matches.
                if !slot.by_description || query.chars().count() < 2 {
                    return None;
                }
                let at = description.to_lowercase().find(&query)?;
                Some(1000.0 + f64::from(u32::try_from(at).unwrap_or(u32::MAX)))
            })?;
            // A value that names more input after its first word completes to
            // that word and a space: the argument is not finished, so accepting
            // it must not submit (`/mcp add` alone has no server to add).
            let takes_more = value.trim() != first;
            Some((
                score,
                order,
                MenuItem {
                    completion: format!(
                        "{name} {before}{first}{}",
                        if takes_more { " " } else { "" }
                    ),
                    label: value,
                    description,
                    tag: None,
                },
            ))
        })
        .collect();
    rank(scored, typed.is_empty())
}

/// Keys of [`ArgumentOptions`] for the run-time values a word after a
/// command's first one takes. The host fills them from state it already
/// holds, when the menu is read, never on a key press.
pub mod values {
    /// Memory changes waiting for review, by id, titled (`/memory queue accept`).
    pub const MEMORY_QUEUE: &str = "memory-queue";
    /// Recorded refinements, newest first (`/refine --rollback`).
    pub const REFINEMENTS: &str = "refinements";
    /// Checks learning proposed, by name (`/checks accept`).
    pub const CHECKS: &str = "checks";
    /// This conversation's scheduled jobs, by id (`/schedule cancel`).
    pub const SCHEDULES: &str = "schedules";
    /// This session's heartbeats, by id (`/heartbeats pause`).
    pub const HEARTBEATS: &str = "heartbeats";
    /// Configured MCP servers, by name (`/mcp get`).
    pub const MCP_SERVERS: &str = "mcp-servers";
    /// Installed packages, by source (`/package remove`).
    pub const PACKAGES: &str = "packages";
    /// Sessions of the last `/resume` listing, by id.
    pub const SESSIONS: &str = "sessions";
    /// Turns of the last `/fork` or `/tree` listing, by number.
    pub const TURNS: &str = "turns";
    /// Follow-ups waiting in the queue, by number (`/queue drop`).
    pub const QUEUED: &str = "queued";
    /// Running delegated children, by name (`/agents stop`).
    pub const AGENTS: &str = "agents";
}

/// What the word being typed may be, given the words before it.
#[derive(Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "three independent ways a word is matched"
)]
struct Slot {
    /// Values to choose from, each with its description. A value may carry its
    /// own placeholder after its first word (`accept <id>...`): the menu shows
    /// it whole, and choosing it types the first word and a space.
    values: Vec<(String, String)>,
    /// Flags, offered once the word starts with `-`, as a shell offers them:
    /// a blank word with Enter keeps running the command as it is.
    flags: Vec<(String, String)>,
    /// The word may be free text (`/goal <objective>`): only a value it begins
    /// is offered, so a word of a sentence never turns into a subcommand.
    free: bool,
    /// Offered only once something is typed: the word is optional here, and
    /// Enter on the blank runs what is on the line (a second id after `accept`).
    typed_only: bool,
    /// The values are records (ids, numbers) named by their descriptions: a
    /// word of the description finds one (`flutter` finds the memory change
    /// about Flutter).
    by_description: bool,
}

fn fixed(values: &[(&str, &str)]) -> Vec<(String, String)> {
    values
        .iter()
        .map(|(value, description)| ((*value).to_owned(), (*description).to_owned()))
        .collect()
}

fn choose(values: &[(&str, &str)]) -> Slot {
    Slot {
        values: fixed(values),
        ..Slot::default()
    }
}

fn free(values: &[(&str, &str)], flags: &[(&str, &str)]) -> Slot {
    Slot {
        values: fixed(values),
        flags: fixed(flags),
        free: true,
        ..Slot::default()
    }
}

fn flags(flags: &[(&str, &str)]) -> Slot {
    free(&[], flags)
}

/// The run-time records under `key` of [`values`].
fn records(dynamic: &ArgumentOptions, key: &str) -> Slot {
    Slot {
        values: dynamic.get(key).cloned().unwrap_or_default(),
        by_description: true,
        ..Slot::default()
    }
}

/// Whether `word` is one of `flags` that takes a value next (`--gate <command>`).
fn takes_value(flags: &[(&str, &str)], word: &str) -> bool {
    flags.iter().any(|(flag, _)| {
        let mut parts = flag.split(' ');
        parts.next() == Some(word)
            && parts
                .next()
                .is_some_and(|value| value.starts_with('<') || value.contains('='))
    })
}

const SWITCH: &[(&str, &str)] = &[
    ("status", "show whether it is on"),
    ("on", "turn it on"),
    ("off", "turn it off"),
];

const ON_OFF: &[(&str, &str)] = &[("on", "turn it on"), ("off", "turn it off")];

const INTERVALS: &[(&str, &str)] = &[
    ("5m", "five minutes"),
    ("10m", "ten minutes"),
    ("30m", "half an hour"),
    ("1h", "an hour"),
    ("6h", "six hours"),
    ("1d", "a day"),
];

const DELIVERY: &[(&str, &str)] = &[
    (
        "--steer",
        "deliver as a correction at the run's next safe step",
    ),
    (
        "--follow-up",
        "deliver after the active run finishes (the default)",
    ),
];

const MEMORY_ACTIONS: &[(&str, &str)] = &[
    (
        "queue <action>",
        "the memory changes that wait for your review",
    ),
    ("list", "list the changes that wait for your review"),
    ("show <id>...", "one change with its evidence and diff"),
    ("accept <id>...|all", "apply the changes to memory"),
    ("reject <id>...|all", "keep the changes out of memory"),
];

const KNOWLEDGE_ACTIONS: &[(&str, &str)] = &[
    (
        "status",
        "whether knowledge is on, with its pages and links",
    ),
    ("list [offset]", "the pages"),
    ("search <query>", "find pages by their words"),
    ("read <path>", "one page"),
    ("graph", "the pages and their links"),
    ("timeline [folder]", "pages by when they changed"),
    ("recent [count]", "the latest pages"),
    ("refresh", "re-read the library from disk"),
    (
        "bootstrap [focus]",
        "have the agent map the project into it",
    ),
    ("on", "turn knowledge on"),
    ("off", "turn knowledge off"),
];

const GLOBAL: &[(&str, &str)] = &[("--global", "the global library, not the project's")];

const REFINE_FLAGS: &[(&str, &str)] = &[
    (
        "--global",
        "refine the global store, not this conversation's",
    ),
    ("--rollback <id>", "undo one recorded refinement"),
    ("--curate", "curate memory: merge, retire and fix entries"),
    ("--history [n]", "show the latest refinements and their ids"),
];

const AUTONOMOUS_FLAGS: &[(&str, &str)] = &[
    (
        "--max-continuations <n>",
        "stop after this many continuations",
    ),
    ("--max-turns <n>", "stop after this many turns"),
    ("--max-tokens <n>", "stop after this many tokens"),
    ("--timeout-ms <n>", "stop after this long"),
    ("--gate <command>", "end when this command passes"),
    ("--gate-retries <n>", "how often a failed gate is retried"),
];

const PACKAGE_LOCAL: &[(&str, &str)] = &[(
    "--local",
    "the project's packages (.harness/settings.json), not the user's",
)];

const MCP_ADD_FLAGS: &[(&str, &str)] = &[
    ("--env KEY=VALUE", "an environment variable for the server"),
    ("--cwd <dir>", "the folder the server starts in"),
    ("--url <https-url>", "a remote server instead of a command"),
    (
        "--bearer-token-env-var <var>",
        "the variable holding the remote server's token",
    ),
    ("--force", "replace a server of the same name"),
    ("--", "the command and its arguments follow"),
];

/// The grammar of every built-in command's arguments: what the word after
/// `words` may be. A command whose first word's values change at run time
/// (`/model`, `/login`, `/effort`) takes them from `dynamic` under its own
/// name; a later word's records come under a key of [`values`].
#[allow(
    clippy::too_many_lines,
    reason = "one table of every command's grammar reads best in one place"
)]
fn slot(name: &str, words: &[&str], dynamic: &ArgumentOptions) -> Slot {
    if words.is_empty()
        && let Some(values) = dynamic.get(name).filter(|values| !values.is_empty())
    {
        return Slot {
            values: values.clone(),
            ..Slot::default()
        };
    }
    let last = words.last().copied().unwrap_or_default();
    match (name, words) {
        ("/effort", []) => choose(&[
            ("off", ""),
            ("minimal", ""),
            ("low", ""),
            ("medium", ""),
            ("high", ""),
            ("xhigh", ""),
            ("max", ""),
        ]),
        ("/tier", []) => choose(&[
            ("default", "the provider's standard tier"),
            ("flex", "slower and cheaper"),
            ("priority", "faster, at a higher price"),
            ("auto", "let the provider choose"),
        ]),
        ("/permissions", []) => choose(&[
            ("ask", "ask before every change and command"),
            ("auto-edit", "edit files without asking; ask for commands"),
            ("full-auto", "run without asking"),
        ]),
        ("/subagent-effort" | "/subagent-model", []) => {
            choose(&[("inherit", "children run as this agent does")])
        }
        ("/fullscreen", []) => choose(ON_OFF),
        ("/trust", []) => choose(&[("yes", "trust this project's config")]),
        ("/refinements" | "/factory" | "/bank" | "/codemode", []) => choose(SWITCH),
        ("/jev", []) => {
            let mut slot = choose(SWITCH);
            slot.values.extend(
                super::jev::DECISIONS.iter().map(|(decision, meaning)| {
                    (format!("{decision} on|off"), (*meaning).to_owned())
                }),
            );
            slot
        }
        ("/jev", [decision])
            if super::jev::DECISIONS
                .iter()
                .any(|(known, _)| known == decision) =>
        {
            choose(ON_OFF)
        }
        ("/memory", _) => {
            let rest = words.strip_prefix(&["queue"][..]).unwrap_or(words);
            match rest {
                [] if words.is_empty() => choose(MEMORY_ACTIONS),
                [] => choose(&MEMORY_ACTIONS[1..]),
                // `all` decides every waiting change: it stands alone.
                [action, ids @ ..] if ids.contains(&"all") && *action != "show" => Slot::default(),
                [action, ids @ ..] if matches!(*action, "show" | "accept" | "reject") => {
                    // Last, so an Enter on the first row decides one change.
                    let mut slot = records(dynamic, values::MEMORY_QUEUE);
                    if ids.is_empty() && *action != "show" && !slot.values.is_empty() {
                        let every = format!("every change that waits ({})", slot.values.len());
                        slot.values.push(("all".to_owned(), every));
                    }
                    Slot {
                        typed_only: !ids.is_empty(),
                        ..slot
                    }
                }
                _ => Slot::default(),
            }
        }
        ("/refine", _) if last == "--rollback" => records(dynamic, values::REFINEMENTS),
        ("/refine", _) if last == "--history" => Slot::default(),
        ("/refine", _) => flags(REFINE_FLAGS),
        ("/new", _) if last == "--name" || words.contains(&"--") => Slot::default(),
        ("/new", _) => flags(&[
            ("--name <name>", "name the new session"),
            ("--", "the first prompt follows"),
        ]),
        ("/handoff", _) => flags(&[(
            "--reset",
            "continue in a fresh conversation after writing it",
        )]),
        ("/knowledge", []) => Slot {
            flags: fixed(GLOBAL),
            ..choose(KNOWLEDGE_ACTIONS)
        },
        ("/knowledge", _) => flags(GLOBAL),
        ("/checks", []) => choose(&[
            ("list", "the checks that wait for you"),
            ("accept <name>", "add one to [verify]"),
            ("reject <name>", "drop one"),
        ]),
        ("/checks", ["accept" | "reject"]) => records(dynamic, values::CHECKS),
        ("/goal", []) => free(
            &[
                ("status", "show the goal"),
                ("pause", "pause it"),
                ("resume", "resume it"),
                ("clear", "drop it"),
            ],
            &[],
        ),
        ("/autonomous", []) => choose(&[
            ("status", "show autonomous mode and its budget"),
            ("on [budget]", "carry the conversation on by itself"),
            ("off", "stop"),
        ]),
        ("/autonomous", ["on" | "enable", ..]) if !takes_value(AUTONOMOUS_FLAGS, last) => {
            flags(AUTONOMOUS_FLAGS)
        }
        ("/schedule", []) => choose(&[
            ("list", "this conversation's jobs"),
            ("add <when> -- <prompt>", "schedule a prompt"),
            ("pause <id>", "pause a job"),
            ("resume <id>", "resume a job"),
            ("cancel <id>", "delete a job"),
        ]),
        ("/schedule", ["pause" | "resume" | "cancel" | "delete"]) => {
            records(dynamic, values::SCHEDULES)
        }
        ("/schedule", ["add", rest @ ..]) => {
            let when: Vec<&str> = rest
                .iter()
                .copied()
                .filter(|word| !word.starts_with("--"))
                .collect();
            match when.as_slice() {
                [] => free(
                    &[
                        ("in <duration>", "once, after a delay: in 10m"),
                        ("every <duration>", "again and again: every 1h"),
                        ("at <date>", "once, at an ISO date"),
                        ("@hourly", "every hour"),
                        ("@daily", "every day at midnight"),
                        ("@weekly", "every Sunday"),
                        ("@monthly", "the first of every month"),
                    ],
                    DELIVERY,
                ),
                ["in" | "every" | "each"] if !last.starts_with("--") => free(INTERVALS, &[]),
                _ => Slot::default(),
            }
        }
        ("/heartbeat", rest) if rest.iter().all(|word| word.starts_with("--")) => free(
            &[("every <interval>", "how often it runs (5m without it)")],
            DELIVERY,
        ),
        ("/heartbeat", ["every" | "each"]) => free(INTERVALS, &[]),
        ("/heartbeat", ["every" | "each", _, rest @ ..])
            if rest.iter().all(|word| word.starts_with("--")) =>
        {
            flags(DELIVERY)
        }
        ("/heartbeats", []) => choose(&[
            ("list", "this session's heartbeats"),
            ("pause <id>", "pause one"),
            ("resume <id>", "resume one"),
            ("delete <id>", "delete one"),
        ]),
        ("/heartbeats", ["pause" | "resume" | "delete" | "stop" | "remove"]) => {
            records(dynamic, values::HEARTBEATS)
        }
        ("/mcp", []) => choose(&[
            (
                "add <name> -- <command> [args...] | --url <url>",
                "add a server",
            ),
            ("list", "the servers"),
            ("get <name>", "one server's configuration"),
            ("remove <name>", "remove a server"),
            ("login <name>", "sign in to a server"),
            ("logout <name>", "sign out of a server"),
        ]),
        ("/mcp", ["get" | "remove" | "login" | "logout"]) => records(dynamic, values::MCP_SERVERS),
        ("/mcp", ["add", _, ..]) if !words.contains(&"--") && !takes_value(MCP_ADD_FLAGS, last) => {
            flags(MCP_ADD_FLAGS)
        }
        ("/package", []) => choose(&[
            (
                "install <source>",
                "npm:@scope/name, git:github.com/user/repo, https://..., ./path",
            ),
            ("remove <source>", "remove an installed package"),
            ("update [source]", "update every installed package, or one"),
            ("list", "the installed packages"),
        ]),
        ("/package", ["remove" | "uninstall" | "rm"]) => Slot {
            flags: fixed(PACKAGE_LOCAL),
            ..records(dynamic, values::PACKAGES)
        },
        ("/package", ["update"]) => records(dynamic, values::PACKAGES),
        ("/package", ["install" | "add" | "remove" | "uninstall" | "rm", ..])
            if !words.contains(&"--local") =>
        {
            flags(PACKAGE_LOCAL)
        }
        ("/queue", []) => free(
            &[
                ("list", "the queued follow-ups"),
                ("edit <n> <text>", "rewrite one"),
                ("drop <n>", "remove one"),
                ("up <n>", "move one earlier"),
                ("down <n>", "move one later"),
            ],
            &[],
        ),
        ("/queue", ["edit" | "drop" | "up" | "down"]) => records(dynamic, values::QUEUED),
        ("/agents", []) => choose(&[
            ("stop [name]", "stop a delegated child, or all of them"),
            ("messages [action]", "the messages agents send each other"),
        ]),
        ("/agents", ["stop"]) => {
            let mut slot = records(dynamic, values::AGENTS);
            slot.values
                .push(("all".to_owned(), "stop every child".to_owned()));
            slot
        }
        ("/agents", ["messages"]) => choose(&[
            ("status", "whether messages are taken in"),
            ("pause", "stop taking them in and drop the queued ones"),
            ("resume", "take them in again"),
            ("clear", "drop the queued ones"),
        ]),
        ("/resume", [] | ["--compact"]) => {
            let mut slot = records(dynamic, values::SESSIONS);
            slot.values
                .insert(0, ("latest".to_owned(), "the latest session".to_owned()));
            slot.flags = fixed(&[("--compact", "summarize it first")]);
            slot.free = true;
            slot
        }
        ("/fork", []) | ("/tree", ["label"]) => records(dynamic, values::TURNS),
        ("/tree", []) => {
            let mut slot = records(dynamic, values::TURNS);
            slot.values
                .push(("label <number> [text]".to_owned(), "name a turn".to_owned()));
            slot
        }
        ("/tree", [number]) if number.parse::<usize>().is_ok() => flags(&[(
            "--summarize [focus]",
            "continue there, summarizing the turns left behind",
        )]),
        ("/rlm-max-depth", []) => free(&[("unlimited", "no depth limit")], &[]),
        ("/rlm-max-depth", [_]) => flags(&[("--global", "for every chat, not only this one")]),
        ("/scoped-models", _) => {
            let mut values = if words.is_empty() {
                fixed(&[("clear", "cycle through every model again")])
            } else {
                Vec::new()
            };
            values.extend(dynamic.get("/model").cloned().unwrap_or_default());
            Slot {
                values,
                ..Slot::default()
            }
        }
        ("/plugins", []) => free(&[("connect <id>", "connect one service by its id")], &[]),
        _ => Slot::default(),
    }
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

    // Argument completion, command by command.

    use super::{MenuItem, SLASH_COMMANDS, matching, values};

    fn records(entries: &[(&'static str, &[(&str, &str)])]) -> ArgumentOptions {
        let mut dynamic = ArgumentOptions::new();
        for (key, rows) in entries {
            dynamic.insert(
                key,
                rows.iter()
                    .map(|(value, description)| ((*value).to_owned(), (*description).to_owned()))
                    .collect(),
            );
        }
        dynamic
    }

    fn rows(buffer: &str, dynamic: &ArgumentOptions) -> Vec<MenuItem> {
        suggest(buffer, &[], dynamic)
    }

    fn heads(buffer: &str, dynamic: &ArgumentOptions) -> Vec<String> {
        rows(buffer, dynamic)
            .iter()
            .map(|item| item.name().to_owned())
            .collect()
    }

    fn bare(buffer: &str) -> Vec<String> {
        heads(buffer, &ArgumentOptions::new())
    }

    fn queue() -> ArgumentOptions {
        records(&[(
            values::MEMORY_QUEUE,
            &[
                (
                    "mq-8768207a",
                    "NexaClip Windows listener state test with MSVC · create global memory",
                ),
                (
                    "mq-b8db4def",
                    "NexaClip Flutter SDK on Windows · create global memory",
                ),
                (
                    "mq-1ee5ca95",
                    "NexaClip Flutter test environment on Windows · create global memory",
                ),
            ],
        )])
    }

    #[test]
    fn memory_offers_the_queue_actions_then_the_waiting_ids_with_their_titles() {
        let dynamic = queue();
        assert_eq!(
            bare("/memory "),
            ["queue", "list", "show", "accept", "reject"]
        );
        assert_eq!(
            rows("/memory ", &dynamic)[0].completion,
            "/memory queue ",
            "queue takes an action: choosing it keeps the line open"
        );
        assert_eq!(bare("/memory queue "), ["list", "show", "accept", "reject"]);
        assert_eq!(
            rows("/memory queue a", &dynamic)[0].completion,
            "/memory queue accept "
        );
        let ids = rows("/memory queue accept ", &dynamic);
        assert_eq!(
            ids.iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["mq-8768207a", "mq-b8db4def", "mq-1ee5ca95", "all"],
            "every waiting id, in the order /memory lists them, then all"
        );
        assert_eq!(ids[3].description, "every change that waits (3)");
        assert!(ids[0].description.contains("Windows listener"), "{ids:?}");
        assert_eq!(
            heads("/memory reject ", &dynamic)
                .last()
                .map(String::as_str),
            Some("all")
        );
        assert!(
            heads("/memory reject all ", &dynamic).is_empty(),
            "all stands alone"
        );
        assert!(
            !heads("/memory show ", &dynamic).contains(&"all".to_owned()),
            "show takes ids"
        );
        // Nothing waits: no all to offer.
        assert!(heads("/memory accept ", &ArgumentOptions::new()).is_empty());
        assert_eq!(
            rows("/memory queue accept mq-b8", &dynamic)[0].completion,
            "/memory queue accept mq-b8db4def"
        );
        assert!(
            heads("/memory queue accept mq-b8db4def", &dynamic).is_empty(),
            "an id typed in full needs no menu: Enter accepts it"
        );
        // A word of the title finds the change.
        assert_eq!(
            heads("/memory queue reject flutter", &dynamic),
            ["mq-b8db4def", "mq-1ee5ca95"]
        );
        // `show` takes ids too, and the shorthand without `queue` completes alike.
        assert_eq!(heads("/memory show ", &dynamic).len(), 3);
        assert_eq!(
            rows("/memory accept mq-87", &dynamic)[0].completion,
            "/memory accept mq-8768207a"
        );
    }

    #[test]
    fn memory_offers_a_second_id_only_once_one_is_typed_and_never_the_first_again() {
        let dynamic = queue();
        assert!(
            heads("/memory queue accept mq-8768207a ", &dynamic).is_empty(),
            "a blank after the first id is Enter's to run, not a second row's"
        );
        assert_eq!(
            heads("/memory queue accept mq-8768207a mq", &dynamic),
            ["mq-b8db4def", "mq-1ee5ca95"]
        );
        assert!(heads("/memory queue list ", &dynamic).is_empty());
        assert!(heads("/memory queue accept ", &ArgumentOptions::new()).is_empty());
    }

    #[test]
    fn refine_offers_its_flags_on_a_dash_and_the_refinements_after_rollback() {
        let dynamic = records(&[(
            values::REFINEMENTS,
            &[("refine_1700_a", "Tightened the memory digest · global")],
        )]);
        assert!(
            heads("/refine ", &dynamic).is_empty(),
            "instructions are free text"
        );
        assert_eq!(
            heads("/refine -", &dynamic),
            ["--global", "--rollback", "--curate", "--history"]
        );
        assert_eq!(
            rows("/refine --rol", &dynamic)[0].completion,
            "/refine --rollback "
        );
        assert_eq!(heads("/refine --rollback ", &dynamic), ["refine_1700_a"]);
        assert_eq!(
            rows("/refine --rollback dig", &dynamic)[0].completion,
            "/refine --rollback refine_1700_a"
        );
        assert!(
            !heads("/refine --global -", &dynamic).contains(&"--global".to_owned()),
            "a flag on the line is not offered again"
        );
        assert!(heads("/refine --history ", &dynamic).is_empty());
        assert!(heads("/refine tighten the", &dynamic).is_empty());
    }

    #[test]
    fn jev_offers_the_switch_and_its_decisions_then_on_or_off() {
        let first = bare("/jev ");
        assert_eq!(&first[..3], ["status", "on", "off"]);
        assert_eq!(first.len(), 3 + super::super::jev::DECISIONS.len());
        assert!(first.contains(&"commands".to_owned()), "{first:?}");
        assert_eq!(
            matching("/jev comm")[0].completion,
            "/jev commands ",
            "a decision takes on or off next"
        );
        assert_eq!(bare("/jev commands "), ["on", "off"]);
        assert!(bare("/jev status ").is_empty());
    }

    #[test]
    fn switches_offer_status_on_and_off() {
        for name in ["/bank", "/codemode", "/factory", "/refinements"] {
            assert_eq!(bare(&format!("{name} ")), ["status", "on", "off"], "{name}");
            assert_eq!(
                matching(&format!("{name} of"))[0].completion,
                format!("{name} off")
            );
        }
        assert_eq!(bare("/fullscreen "), ["on", "off"]);
        assert_eq!(bare("/trust "), ["yes"]);
    }

    #[test]
    fn knowledge_offers_its_actions_and_global_anywhere() {
        let actions = bare("/knowledge ");
        assert_eq!(actions[0], "status");
        for action in [
            "search",
            "read",
            "bootstrap",
            "timeline",
            "recent",
            "on",
            "off",
        ] {
            assert!(
                actions.contains(&action.to_owned()),
                "{action}: {actions:?}"
            );
        }
        assert_eq!(
            matching("/knowledge sea")[0].completion,
            "/knowledge search "
        );
        assert_eq!(bare("/knowledge -"), ["--global"]);
        assert_eq!(bare("/knowledge list -"), ["--global"]);
        assert!(bare("/knowledge search rust ").is_empty());
    }

    #[test]
    fn checks_offer_the_proposed_checks_by_name() {
        let dynamic = records(&[(values::CHECKS, &[("no-fs-in-renderer", "rg fs src/ui")])]);
        assert_eq!(bare("/checks "), ["list", "accept", "reject"]);
        assert_eq!(heads("/checks accept ", &dynamic), ["no-fs-in-renderer"]);
        assert_eq!(heads("/checks reject no", &dynamic), ["no-fs-in-renderer"]);
        assert!(heads("/checks list ", &dynamic).is_empty());
    }

    #[test]
    fn schedule_offers_its_actions_the_ways_to_say_when_and_the_jobs() {
        let dynamic = records(&[(values::SCHEDULES, &[("job-1", "active · every 1h · lint")])]);
        assert_eq!(
            bare("/schedule "),
            ["list", "add", "pause", "resume", "cancel"]
        );
        let when = bare("/schedule add ");
        assert_eq!(&when[..3], ["in", "every", "at"]);
        assert!(when.contains(&"@daily".to_owned()), "{when:?}");
        assert_eq!(bare("/schedule add -"), ["--steer", "--follow-up"]);
        assert_eq!(bare("/schedule add --steer e"), ["every"]);
        assert_eq!(bare("/schedule add in ")[0], "5m");
        assert!(bare("/schedule add in 10m ").is_empty());
        assert_eq!(heads("/schedule cancel ", &dynamic), ["job-1"]);
    }

    #[test]
    fn heartbeats_offer_the_interval_flags_and_the_heartbeats() {
        let dynamic = records(&[(values::HEARTBEATS, &[("hb-1", "active · 10m · check CI")])]);
        assert_eq!(bare("/heartbeat "), ["every"]);
        assert_eq!(bare("/heartbeat -"), ["--steer", "--follow-up"]);
        assert_eq!(bare("/heartbeat every ")[0], "5m");
        assert_eq!(bare("/heartbeat every 10m -"), ["--steer", "--follow-up"]);
        assert!(
            bare("/heartbeat check").is_empty(),
            "an instruction is free text"
        );
        assert!(bare("/heartbeat every 10m check ").is_empty());
        assert_eq!(bare("/heartbeats "), ["list", "pause", "resume", "delete"]);
        assert_eq!(heads("/heartbeats pause ", &dynamic), ["hb-1"]);
    }

    #[test]
    fn mcp_offers_its_actions_the_servers_and_the_add_flags() {
        let dynamic = records(&[(
            values::MCP_SERVERS,
            &[("codebase-memory", "configured MCP server")],
        )]);
        assert_eq!(
            bare("/mcp "),
            ["add", "list", "get", "remove", "login", "logout"]
        );
        for action in ["get", "remove", "login", "logout"] {
            assert_eq!(
                heads(&format!("/mcp {action} "), &dynamic),
                ["codebase-memory"]
            );
        }
        assert!(bare("/mcp add ").is_empty(), "the server's name is free");
        assert!(bare("/mcp add docs -").contains(&"--url".to_owned()));
        assert!(
            bare("/mcp add docs --cwd ").is_empty(),
            "--cwd takes a folder"
        );
        assert!(
            bare("/mcp add docs -- npx -").is_empty(),
            "after -- the command is the server's"
        );
    }

    #[test]
    fn package_offers_its_actions_the_installed_sources_and_local() {
        let dynamic = records(&[(
            values::PACKAGES,
            &[
                ("npm:@acme/skills", "user package"),
                ("./tools/prompts", "project package (--local)"),
            ],
        )]);
        assert_eq!(bare("/package "), ["install", "remove", "update", "list"]);
        assert_eq!(
            bare("/packages "),
            bare("/package "),
            "the alias completes too"
        );
        let install = rows("/package i", &dynamic);
        assert_eq!(install[0].completion, "/package install ");
        for action in ["remove", "update"] {
            assert_eq!(
                heads(&format!("/package {action} "), &dynamic),
                ["npm:@acme/skills", "./tools/prompts"]
            );
        }
        // A project package is found by what the menu says about it.
        assert_eq!(heads("/package remove proj", &dynamic), ["./tools/prompts"]);
        assert_eq!(heads("/package remove -", &dynamic), ["--local"]);
        assert!(bare("/package install ").is_empty(), "the source is free");
        assert_eq!(bare("/package install npm:x -"), ["--local"]);
        assert!(bare("/package install npm:x --local -").is_empty());
        assert!(
            bare("/package update -").is_empty(),
            "update has no --local"
        );
    }

    #[test]
    fn free_text_commands_offer_only_a_value_the_word_begins() {
        assert_eq!(bare("/goal "), ["status", "pause", "resume", "clear"]);
        assert_eq!(matching("/goal st")[0].completion, "/goal status");
        assert!(
            bare("/goal use").is_empty(),
            "a fuzzy match would turn the objective `use` into pause"
        );
        assert!(bare("/goal ship the release").is_empty());
        assert_eq!(bare("/queue "), ["list", "edit", "drop", "up", "down"]);
        assert!(bare("/queue please").is_empty());
        assert_eq!(bare("/plugins "), ["connect"]);
        assert_eq!(bare("/rlm-max-depth "), ["unlimited"]);
        assert_eq!(bare("/rlm-max-depth 3 -"), ["--global"]);
    }

    #[test]
    fn autonomous_offers_on_off_and_its_budget_flags() {
        assert_eq!(bare("/autonomous "), ["status", "on", "off"]);
        assert_eq!(matching("/autonomous o")[0].label, "on [budget]");
        assert_eq!(matching("/autonomous o")[0].completion, "/autonomous on ");
        assert!(bare("/autonomous on ").is_empty());
        let budget = bare("/autonomous on -");
        assert!(budget.contains(&"--max-turns".to_owned()), "{budget:?}");
        assert!(
            bare("/autonomous on --gate ").is_empty(),
            "the gate is a command"
        );
        assert!(!bare("/autonomous on --max-turns 5 -").contains(&"--max-turns".to_owned()));
        assert!(bare("/autonomous off ").is_empty());
    }

    #[test]
    fn queue_agents_resume_fork_and_tree_offer_what_this_session_holds() {
        let dynamic = records(&[
            (values::QUEUED, &[("1", "Follow-up: run the tests")]),
            (
                values::AGENTS,
                &[("verifier-1c78", "gpt-6 · check the diff")],
            ),
            (values::SESSIONS, &[("s-42", "task-9 renamed the parser")]),
            (
                values::TURNS,
                &[("1", "fix the parser"), ("2", "add tests")],
            ),
        ]);
        assert_eq!(heads("/queue drop ", &dynamic), ["1"]);
        assert_eq!(bare("/agents "), ["stop", "messages"]);
        assert_eq!(heads("/agents stop ", &dynamic), ["verifier-1c78", "all"]);
        assert_eq!(
            bare("/agents messages "),
            ["status", "pause", "resume", "clear"]
        );
        assert_eq!(heads("/resume ", &dynamic), ["latest", "s-42"]);
        assert_eq!(heads("/resume pars", &dynamic), ["s-42"]);
        assert_eq!(heads("/resume -", &dynamic), ["--compact"]);
        assert_eq!(heads("/resume --compact ", &dynamic), ["latest", "s-42"]);
        assert_eq!(heads("/fork ", &dynamic), ["1", "2"]);
        assert_eq!(heads("/tree ", &dynamic), ["1", "2", "label"]);
        assert_eq!(heads("/tree label ", &dynamic), ["1", "2"]);
        assert_eq!(heads("/tree 1 -", &dynamic), ["--summarize"]);
        assert_eq!(heads("/tree tests", &dynamic), ["2"]);
    }

    #[test]
    fn new_handoff_and_scoped_models_offer_their_flags_and_models() {
        assert_eq!(bare("/new -"), ["--name", "--"]);
        assert!(bare("/new --name ").is_empty());
        assert!(bare("/new -- fix it -").is_empty());
        assert_eq!(bare("/handoff -"), ["--reset"]);
        let dynamic = records(&[("/model", &[("openai/gpt-6", "GPT-6"), ("deepseek/v4", "")])]);
        assert_eq!(
            heads("/scoped-models ", &dynamic),
            ["clear", "openai/gpt-6", "deepseek/v4"]
        );
        assert_eq!(
            heads("/scoped-models openai/gpt-6 ", &dynamic),
            ["deepseek/v4"]
        );
    }

    #[test]
    fn settings_offer_their_fixed_values_until_the_app_names_its_own() {
        assert_eq!(bare("/tier "), ["default", "flex", "priority", "auto"]);
        assert_eq!(bare("/permissions "), ["ask", "auto-edit", "full-auto"]);
        assert_eq!(bare("/subagent-effort "), ["inherit"]);
        assert_eq!(bare("/subagent-model "), ["inherit"]);
        let dynamic = records(&[("/tier", &[("priority", "faster (current)")])]);
        assert_eq!(heads("/tier ", &dynamic), ["priority"]);
    }

    /// The audit: every built-in command that takes an argument offers
    /// something to complete it, on a blank or on a dash, unless its argument
    /// is free text or a path.
    #[test]
    fn every_command_with_an_argument_offers_its_values() {
        const FREE: &[&str] = &[
            "/export",
            "/name",
            "/compact",
            "/learn",
            "/recall",
            "/preserve",
            "/review",
            "/self-eval",
            "/import",
            "/steer",
            "/btw",
            "/attach",
        ];
        // What the app names at run time, as the controller fills it.
        let dynamic = records(&[
            ("/model", &[("openai/gpt-6", "GPT-6")]),
            ("/login", &[("openai", "OpenAI")]),
            ("/logout", &[("openai", "OpenAI · api key")]),
            ("/subagent-login", &[("openai", "OpenAI")]),
            ("/subagent-logout", &[("openai", "OpenAI · api key")]),
            (values::TURNS, &[("1", "fix the parser")]),
        ]);
        for command in SLASH_COMMANDS
            .iter()
            .filter(|command| !command.arguments.is_empty())
        {
            let blank = rows(&format!("{} ", command.name), &dynamic);
            let dash = rows(&format!("{} -", command.name), &dynamic);
            assert!(
                !blank.is_empty() || !dash.is_empty() || FREE.contains(&command.name),
                "{} offers nothing to complete `{}`",
                command.name,
                command.arguments
            );
        }
    }
}
