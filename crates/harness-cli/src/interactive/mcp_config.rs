//! `/mcp add | list | get | remove`: prime-agent's MCP management command
//! (`core/mcp/mcp-command.ts`) for the servers a user adds from the app.
//!
//! The servers live in `mcp-servers.json` beside the user config, the way the
//! model `/model` chose lives in `selection.json`: the app never rewrites the
//! user's hand-written `config.toml` (and its comments), and adding a server does
//! not depend on which config schema that file is written in. The file is read at
//! the user layer, so a trusted project's `[mcp_servers]` still overrides it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harness_types::McpServerConfigV2;

/// The file the app-managed servers live in, beside the user config.
#[must_use]
pub fn managed_path(user_config: &Path) -> PathBuf {
    user_config.with_file_name("mcp-servers.json")
}

/// The servers `/mcp add` saved; a missing or unreadable file is none.
#[must_use]
pub fn load(user_config: &Path) -> BTreeMap<String, McpServerConfigV2> {
    std::fs::read_to_string(managed_path(user_config))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save(user_config: &Path, servers: &BTreeMap<String, McpServerConfigV2>) -> Result<(), String> {
    let path = managed_path(user_config);
    let failed =
        |error: &dyn std::fmt::Display| format!("{} could not be saved: {error}", path.display());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| failed(&error))?;
    }
    let text = serde_json::to_string_pretty(servers).map_err(|error| failed(&error))?;
    let staged = path.with_extension("json.staged");
    std::fs::write(&staged, text).map_err(|error| failed(&error))?;
    std::fs::rename(&staged, &path).map_err(|error| failed(&error))
}

/// The usage `/mcp` prints, prime-agent's syntax.
pub const USAGE: &[&str] = &[
    "/mcp add <name> [--env KEY=VALUE] [--cwd DIR] [--force] -- <command> [args...]",
    "/mcp add <name> --url <https-url> [--bearer-token-env-var VAR] [--force]",
    "/mcp list · /mcp get <name> · /mcp remove <name>",
    "Values that are secrets use --env KEY=secret://ENV_NAME, read from your environment when the server starts.",
];

/// Split a command line into words: whitespace separates, double or single quotes
/// group. Backslashes are kept as they are, so a Windows path needs no escaping.
#[must_use]
pub fn split_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for character in line.chars() {
        match quote {
            Some(open) if character == open => quote = None,
            Some(_) => current.push(character),
            None if character == '"' || character == '\'' => {
                quote = Some(character);
                started = true;
            }
            None if character.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(character);
                started = true;
            }
        }
    }
    if started {
        words.push(current);
    }
    words
}

/// What one `/mcp` call did, for the reply and for whether the next turn sees a
/// change.
#[derive(Debug, Eq, PartialEq)]
pub struct Outcome {
    pub lines: Vec<String>,
    pub changed: bool,
}

/// Run `/mcp <args>` against the managed file beside `user_config`.
/// `configured` is every server the resolved configuration has, so `list` and
/// `get` show where each comes from and `remove` can say when a server is in a
/// config file rather than the managed one.
pub fn run(
    user_config: &Path,
    args: &[String],
    configured: &BTreeMap<String, McpServerConfigV2>,
) -> Result<Outcome, String> {
    let managed = load(user_config);
    match args.first().map(String::as_str) {
        Some("list") => {
            require_count(args, 1, "/mcp list")?;
            Ok(Outcome {
                lines: list_lines(configured, &managed),
                changed: false,
            })
        }
        Some("get") => {
            require_count(args, 2, "/mcp get <name>")?;
            let name = validate_name(&args[1])?;
            let config = configured
                .get(name)
                .or_else(|| managed.get(name))
                .ok_or_else(|| format!("MCP server \"{name}\" was not found."))?;
            Ok(Outcome {
                lines: describe(name, config, managed.contains_key(name)),
                changed: false,
            })
        }
        Some("remove") => {
            require_count(args, 2, "/mcp remove <name>")?;
            let name = validate_name(&args[1])?;
            let mut managed = managed;
            if managed.remove(name).is_none() {
                return Err(if configured.contains_key(name) {
                    format!(
                        "MCP server \"{name}\" comes from a config file ([mcp_servers.{name}] in config.toml); remove it there."
                    )
                } else {
                    format!("MCP server \"{name}\" was not found.")
                });
            }
            save(user_config, &managed)?;
            Ok(Outcome {
                lines: vec![format!(
                    "Removed MCP server \"{name}\". The next turn no longer has it."
                )],
                changed: true,
            })
        }
        Some("add") => {
            let (name, config, force) = parse_add(&args[1..])?;
            let mut managed = managed;
            let replaced = managed.contains_key(&name);
            if replaced && !force {
                return Err(format!(
                    "MCP server \"{name}\" already exists. Use --force to replace it."
                ));
            }
            if !replaced && configured.contains_key(&name) && !force {
                return Err(format!(
                    "MCP server \"{name}\" is already configured in config.toml. Use another name, or --force to override it from the app."
                ));
            }
            config.validate(&name).map_err(|error| error.to_string())?;
            let transport = config
                .transport
                .clone()
                .unwrap_or_else(|| "stdio".to_owned());
            managed.insert(name.clone(), config);
            save(user_config, &managed)?;
            Ok(Outcome {
                lines: vec![format!(
                    "{} MCP server \"{name}\" ({transport}). Available from the next turn; /mcp shows its status.",
                    if replaced { "Replaced" } else { "Added" }
                )],
                changed: true,
            })
        }
        Some(other) => Err(format!(
            "unknown /mcp action {other:?}. {}",
            USAGE.join(" | ")
        )),
        None => Err(USAGE.join("\n")),
    }
}

/// prime-agent's `parseMcpAddArgs`, onto ha's server configuration: options,
/// then `--` and the stdio command, or `--url` for Streamable HTTP.
fn parse_add(args: &[String]) -> Result<(String, McpServerConfigV2, bool), String> {
    let name = validate_name(args.first().map_or("", String::as_str))?.to_owned();
    let separator = args.iter().position(|arg| arg == "--");
    let options = &args[1..separator.unwrap_or(args.len())];
    let command = separator.map_or(&[][..], |index| &args[index + 1..]);
    let mut url = None;
    let mut bearer = None;
    let mut cwd = None;
    let mut force = false;
    let mut env = BTreeMap::new();
    let mut seen = Vec::new();
    let mut index = 0;
    while index < options.len() {
        let option = options[index].as_str();
        if option != "--env" && seen.contains(&option) {
            return Err(format!("Duplicate MCP add option: {option}"));
        }
        seen.push(option);
        if option == "--force" {
            force = true;
            index += 1;
            continue;
        }
        if !matches!(
            option,
            "--url" | "--bearer-token-env-var" | "--cwd" | "--env"
        ) {
            return Err(format!("Unknown MCP add option: {option}. {}", USAGE[0]));
        }
        let value = options
            .get(index + 1)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{option} requires a value."))?
            .clone();
        match option {
            "--url" => url = Some(value),
            "--bearer-token-env-var" => {
                bearer = Some(validate_env_name(&value, option)?.to_owned());
            }
            "--cwd" => cwd = Some(value),
            _ => {
                let (key, value) = value
                    .split_once('=')
                    .filter(|(key, value)| !key.is_empty() && !value.is_empty())
                    .ok_or("--env must use KEY=VALUE (a secret as KEY=secret://ENV_NAME).")?;
                validate_env_name(key, "--env")?;
                if env.insert(key.to_owned(), value.to_owned()).is_some() {
                    return Err(format!("Duplicate environment variable: {key}"));
                }
            }
        }
        index += 2;
    }
    if separator.is_some() {
        if url.is_some() || bearer.is_some() {
            return Err("Stdio MCP servers cannot use HTTP options.".to_owned());
        }
        let (program, rest) = command
            .split_first()
            .filter(|(program, _)| !program.trim().is_empty())
            .ok_or("A command is required after --.")?;
        return Ok((
            name,
            McpServerConfigV2 {
                transport: Some("stdio".to_owned()),
                command: Some(program.clone()),
                args: rest.to_vec(),
                env,
                cwd,
                ..McpServerConfigV2::default()
            },
            force,
        ));
    }
    if cwd.is_some() || !env.is_empty() {
        return Err("--cwd and --env require a stdio command after --.".to_owned());
    }
    let url = url.ok_or("Use --url <url> for HTTP or -- <command> [args...] for stdio.")?;
    Ok((
        name,
        McpServerConfigV2 {
            transport: Some("streamable_http".to_owned()),
            url: Some(url),
            bearer_token_env: bearer,
            ..McpServerConfigV2::default()
        },
        force,
    ))
}

fn list_lines(
    configured: &BTreeMap<String, McpServerConfigV2>,
    managed: &BTreeMap<String, McpServerConfigV2>,
) -> Vec<String> {
    let mut names: Vec<&String> = configured.keys().chain(managed.keys()).collect();
    names.sort();
    names.dedup();
    if names.is_empty() {
        let mut lines = vec!["No MCP servers configured. Add one:".to_owned()];
        lines.extend(USAGE.iter().map(|line| format!("  {line}")));
        return lines;
    }
    names
        .into_iter()
        .filter_map(|name| {
            let config = configured.get(name).or_else(|| managed.get(name))?;
            Some(format!(
                "{name}: {} · {} · {}",
                config.transport.as_deref().unwrap_or("stdio"),
                target(config),
                if managed.contains_key(name) {
                    "added with /mcp"
                } else {
                    "config.toml"
                }
            ))
        })
        .collect()
}

fn target(config: &McpServerConfigV2) -> String {
    match (&config.command, &config.url) {
        (Some(command), _) => std::iter::once(command.as_str())
            .chain(config.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        (None, Some(url)) => url.clone(),
        (None, None) => "(no command or URL)".to_owned(),
    }
}

/// One server's non-secret configuration: an environment value is shown only as
/// its `secret://` reference or as set.
fn describe(name: &str, config: &McpServerConfigV2, managed: bool) -> Vec<String> {
    let mut lines = vec![
        format!(
            "{name} ({})",
            if managed {
                "added with /mcp"
            } else {
                "config.toml"
            }
        ),
        format!(
            "  transport: {}",
            config.transport.as_deref().unwrap_or("stdio")
        ),
        format!("  target: {}", target(config)),
    ];
    if let Some(cwd) = &config.cwd {
        lines.push(format!("  cwd: {cwd}"));
    }
    for (key, value) in &config.env {
        let shown = if value.starts_with("secret://") {
            value.as_str()
        } else {
            "(set)"
        };
        lines.push(format!("  env {key}: {shown}"));
    }
    if let Some(bearer) = &config.bearer_token_env {
        lines.push(format!("  bearer token from ${bearer}"));
    }
    lines
}

fn validate_name(name: &str) -> Result<&str, String> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
    if valid {
        Ok(name)
    } else {
        Err("MCP server names must be 1-64 letters, numbers, underscores, or hyphens and start with a letter or number.".to_owned())
    }
}

fn validate_env_name<'a>(value: &'a str, option: &str) -> Result<&'a str, String> {
    let valid = value
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    if valid {
        Ok(value)
    } else {
        Err(format!("{option} requires an environment variable name."))
    }
}

fn require_count(args: &[String], count: usize, usage: &str) -> Result<(), String> {
    if args.len() == count {
        Ok(())
    } else {
        Err(format!("Usage: {usage}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{load, run, split_words};
    use std::collections::BTreeMap;

    fn words(line: &str) -> Vec<String> {
        split_words(line)
    }

    /// A Windows path keeps its backslashes and a quoted argument keeps its spaces.
    #[test]
    fn words_split_like_a_shell_without_escapes() {
        assert_eq!(
            words(r#"add memory -- C:\Tools\mem.exe --root "C:\My Projects" 'a b'"#),
            [
                "add",
                "memory",
                "--",
                r"C:\Tools\mem.exe",
                "--root",
                r"C:\My Projects",
                "a b"
            ]
        );
    }

    /// prime-agent's add, list, get and remove, kept beside the user config and
    /// never written into config.toml.
    #[test]
    fn servers_are_added_listed_and_removed_in_the_managed_file() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let user = temporary.path().join("config.toml");
        std::fs::write(&user, "# my hand-written config\nschema_version = 1\n").expect("config");
        let none = BTreeMap::new();

        let added = run(
            &user,
            &words(r"add codebase-memory --env TOKEN=secret://MEM_TOKEN -- C:\mem\mem.exe --stdio"),
            &none,
        )
        .expect("add");
        assert!(added.changed);
        let saved = load(&user);
        let server = &saved["codebase-memory"];
        assert_eq!(server.command.as_deref(), Some(r"C:\mem\mem.exe"));
        assert_eq!(server.args, ["--stdio"]);
        assert_eq!(server.env["TOKEN"], "secret://MEM_TOKEN");
        assert_eq!(
            std::fs::read_to_string(&user).expect("config"),
            "# my hand-written config\nschema_version = 1\n",
            "config.toml is untouched"
        );

        assert!(
            run(&user, &words("add codebase-memory -- other"), &none)
                .expect_err("a second add needs --force")
                .contains("--force")
        );
        let listed = run(&user, &words("list"), &saved).expect("list");
        assert!(
            listed.lines[0].starts_with("codebase-memory: stdio"),
            "{:?}",
            listed.lines
        );
        let shown = run(&user, &words("get codebase-memory"), &saved).expect("get");
        assert!(
            shown
                .lines
                .iter()
                .any(|line| line.contains("secret://MEM_TOKEN"))
        );

        let http = run(
            &user,
            &words("add docs --url https://mcp.example.com/mcp --bearer-token-env-var DOCS_TOKEN"),
            &none,
        )
        .expect("add http");
        assert!(http.lines[0].contains("streamable_http"));

        run(&user, &words("remove codebase-memory"), &load(&user)).expect("remove");
        assert!(!load(&user).contains_key("codebase-memory"));
        assert!(run(&user, &words("remove nope"), &none).is_err());
    }

    #[test]
    fn bad_input_is_refused_with_the_reason() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let user = temporary.path().join("config.toml");
        let none = BTreeMap::new();
        for (line, reason) in [
            ("add", "names must"),
            ("add x", "--url"),
            ("add x --url https://a.example -- cmd", "cannot use HTTP"),
            ("add x --env NOEQUALS -- cmd", "KEY=VALUE"),
            ("add x --wat -- cmd", "Unknown MCP add option"),
            ("add x --", "command is required"),
            ("frob", "unknown /mcp action"),
        ] {
            let error = run(&user, &words(line), &none).expect_err(line);
            assert!(error.contains(reason), "{line}: {error}");
        }
    }
}
