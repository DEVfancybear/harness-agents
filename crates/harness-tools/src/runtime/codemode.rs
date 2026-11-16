//! The `codemode` host tool: the model writes one JavaScript program that
//! calls its tools as `await tools.<name>(args)`, and only what the program
//! prints and returns comes back (pi's codemode, run by `harness_codemode`'s
//! interpreter; `docs/architecture/CODEMODE.en.md`).
//!
//! The driver answers the program's calls itself: each one is checked
//! against its tool's schema and goes through the same gate as a call the
//! model makes (policy, guard, hooks, approval, intent and receipt), so a
//! program can do nothing its calls could not do one by one. The tools it
//! reaches are the turn's tools and the deferred ones, without the host's
//! control tools.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use harness_codemode::{DeclaredTool, ToolInfo, render_declarations};
use serde_json::{Value, json};

use crate::ToolOutput;

/// The host tool's name.
pub const CODEMODE_TOOL: &str = "codemode";

/// Tools a program cannot call: the host's own control tools, which act on
/// the turn rather than the workspace, a question to the user, and codemode
/// itself.
pub const NOT_FROM_CODEMODE: [&str; 6] = [
    CODEMODE_TOOL,
    crate::bank::ACTION_BANK_TOOL,
    crate::turn_driver::DROP_CONTEXT_TOOL,
    crate::turn_driver::TOOL_SCHEMA_TOOL,
    crate::turn_driver::USE_TOOL_TOOL,
    "ask_user",
];

/// A program's whole run, its calls included, unless its options line asks
/// for less or more.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_mins(5);
/// The longest run an options line may ask for.
pub const MAX_TIMEOUT: Duration = Duration::from_mins(30);
/// The most characters of a program's output the model reads by default.
pub const DEFAULT_OUTPUT_CHARS: usize = 60_000;
/// Characters per token when an options line gives `max_output_tokens`.
const CHARS_PER_TOKEN: usize = 4;
/// The declarations in the tool's description, before they are shortened.
const DECLARATIONS_BUDGET: usize = 12_000;

/// Codemode in a driver: off unless the host turns it on.
#[derive(Clone, Debug, Default)]
pub struct Codemode {
    /// Where each conversation's `store()` is kept between runs of `ha`;
    /// `None` keeps it in memory only.
    pub store_dir: Option<PathBuf>,
}

fn function_of(schema: &Value) -> &Value {
    schema.get("function").unwrap_or(schema)
}

fn name_of(schema: &Value) -> Option<&str> {
    function_of(schema).get("name").and_then(Value::as_str)
}

/// The first line of a description, for `ALL_TOOLS` and the roster.
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    let mut short: String = line.chars().take(160).collect();
    if line.chars().count() > 160 {
        short.push('…');
    }
    short
}

/// The tools a program reaches, in the order the model was shown them:
/// those it was sent, then the deferred ones.
pub fn reachable(tool_schemas: &[Value], deferred: &[Value]) -> Vec<(DeclaredTool, bool)> {
    let mut tools: Vec<(DeclaredTool, bool)> = Vec::new();
    for (schemas, is_deferred) in [(tool_schemas, false), (deferred, true)] {
        for schema in schemas {
            let Some(name) = name_of(schema) else {
                continue;
            };
            if NOT_FROM_CODEMODE.contains(&name) || tools.iter().any(|(tool, _)| tool.name == name)
            {
                continue;
            }
            let function = function_of(schema);
            tools.push((
                DeclaredTool {
                    name: name.to_owned(),
                    description: function
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    input_schema: function.get("parameters").cloned(),
                    output_schema: Some(json!({"type": "string"})),
                },
                is_deferred,
            ));
        }
    }
    tools
}

/// The tools as the program's `tools` and `ALL_TOOLS` hold them.
pub fn tool_infos(tools: &[(DeclaredTool, bool)]) -> Vec<ToolInfo> {
    tools
        .iter()
        .map(|(tool, _)| ToolInfo::new(tool.name.clone(), first_line(&tool.description)))
        .collect()
}

/// `declare const tools: {...}` for the tools the model was sent, short
/// enough for the budget: full argument types, then shorter ones, then
/// names. Their descriptions are in the tool list already.
fn declarations(tools: &[(DeclaredTool, bool)]) -> String {
    let sent: Vec<DeclaredTool> = tools
        .iter()
        .filter(|(_, is_deferred)| !is_deferred)
        .map(|(tool, _)| tool.clone())
        .collect();
    for input_max in [2_000, 600, 200] {
        let text = render_declarations(&sent, false, input_max);
        if text.chars().count() <= DECLARATIONS_BUDGET {
            return text;
        }
    }
    let without_types: Vec<DeclaredTool> = sent
        .into_iter()
        .map(|tool| DeclaredTool {
            input_schema: None,
            ..tool
        })
        .collect();
    render_declarations(&without_types, false, 0)
}

/// The `codemode` tool for this tool list.
pub fn codemode_schema(tool_schemas: &[Value], deferred: &[Value]) -> Value {
    let tools = reachable(tool_schemas, deferred);
    let mut description = String::from(
        "Run one JavaScript program that calls your tools as functions. Use it when a step needs several tool calls whose results you only need to filter, count, compare, combine or loop over: the calls run inside the program, and only what it prints with text() or console.log and what it returns come back to you, so their full results never enter the conversation. For one or two calls whose output you want to read, call the tools directly.\n\
         `code` is the body of an async function: `const text = await tools.read_file({ path: \"Cargo.toml\" })`. Each call takes the same arguments as the tool and resolves to the text the tool returns (for read_file, the file's text without line numbers, cut with a note when it is long); a call that fails or is refused throws an Error you can catch. A call runs when it is awaited; `await Promise.all(paths.map(path => tools.read_file({ path })))` runs several side by side. Approvals and checks apply to every call, as to your own.\n\
         Also available: ALL_TOOLS (each tool's name and description), text(value), console.log(...), exit(), store(key, value) and load(key) for small JSON values kept for the next codemode call of this session, and JSON, Math, Object, Array, String, Number, Promise, Map, Set, RegExp and Date (UTC). There are no timers, no fetch, no require or import, and no files except through tools. An optional first line sets limits: `// @options: {\"timeout_ms\": 60000, \"max_output_tokens\": 4000}`.\n",
    );
    let declared = declarations(&tools);
    if !declared.is_empty() {
        let _ = write!(description, "\n```ts\n{declared}\n```\n");
    }
    let deferred_names: Vec<&str> = tools
        .iter()
        .filter(|(_, is_deferred)| *is_deferred)
        .map(|(tool, _)| tool.name.as_str())
        .collect();
    if !deferred_names.is_empty() {
        let _ = write!(
            description,
            "\nThe 'More tools' are callable too, with the arguments tool_schema shows: {}.\n",
            deferred_names.join(", ")
        );
    }
    json!({
        "type": "function",
        "function": {
            "name": CODEMODE_TOOL,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "JavaScript: the body of an async function. Use `await tools.<name>(args)`, print with text() or console.log, and return a value."
                    }
                },
                "required": ["code"],
                "additionalProperties": false
            }
        }
    })
}

/// A finished call as the program sees it: the JSON text of a string, what
/// the model would read, except that `read_file` gives the file's text.
pub fn script_value(name: &str, output: &ToolOutput) -> String {
    let text = match output {
        ToolOutput::ReadFile { content, .. } => without_line_numbers(content),
        output => crate::turn_driver::render_tool_output(name, output),
    };
    Value::String(text).to_string()
}

/// `read_file`'s lines without the number (`12: `) or hashline anchor
/// (`12#a3: `) each one starts with, so a program reads the file's text.
/// Secrets stay redacted, and a note that the file was cut stays.
fn without_line_numbers(content: &str) -> String {
    content
        .split('\n')
        .map(|line| {
            let digits = line.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return line;
            }
            let rest = &line[digits..];
            let rest = match rest.strip_prefix('#') {
                Some(anchor) => anchor.trim_start_matches(|c: char| c.is_ascii_alphanumeric()),
                None => rest,
            };
            rest.strip_prefix(": ")
                .or_else(|| (rest == ":").then_some(""))
                .unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What `store()` kept, by session, while the app runs: each session's keys
/// and their JSON text, in the order they were set. With a store directory
/// it is also written to `<dir>/<session>.json`, so a resumed conversation
/// (`ha exec --resume`, an app started again) loads it.
type Stores = HashMap<String, Vec<(String, String)>>;

static STORES: LazyLock<Mutex<Stores>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// A session id as a file name: what is not a letter, digit, `-` or `_`
/// becomes `_`.
fn store_file(dir: &Path, session: &str) -> PathBuf {
    let name: String = session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{name}.json"))
}

fn read_store_file(path: &Path) -> Option<Vec<(String, String)>> {
    let text = std::fs::read_to_string(path).ok()?;
    let entries: Vec<(String, String)> = serde_json::from_str(&text).ok()?;
    // Each value must still be JSON a program can load.
    Some(
        entries
            .into_iter()
            .filter(|(_, json)| serde_json::from_str::<Value>(json).is_ok())
            .collect(),
    )
}

pub fn load_store(codemode: &Codemode, session: &str) -> Vec<(String, String)> {
    let Ok(mut stores) = STORES.lock() else {
        return Vec::new();
    };
    if let Some(store) = stores.get(session) {
        return store.clone();
    }
    let loaded = codemode
        .store_dir
        .as_deref()
        .and_then(|dir| read_store_file(&store_file(dir, session)))
        .unwrap_or_default();
    stores.insert(session.to_owned(), loaded.clone());
    loaded
}

pub fn save_store(codemode: &Codemode, session: &str, writes: &harness_codemode::StoreWrites) {
    if writes.set.is_empty() && writes.delete.is_empty() {
        return;
    }
    let Ok(mut stores) = STORES.lock() else {
        return;
    };
    let store = stores.entry(session.to_owned()).or_default();
    for key in &writes.delete {
        store.retain(|(existing, _)| existing != key);
    }
    for (key, json) in &writes.set {
        match store.iter_mut().find(|(existing, _)| existing == key) {
            Some(entry) => entry.1.clone_from(json),
            None => store.push((key.clone(), json.clone())),
        }
    }
    let Some(dir) = codemode.store_dir.as_deref() else {
        return;
    };
    // Written whole and renamed into place, under the lock, so a reader
    // never sees half a file.
    let path = store_file(dir, session);
    let written = std::fs::create_dir_all(dir).and_then(|()| {
        let text = serde_json::to_string(store).unwrap_or_else(|_| "[]".to_owned());
        let partial = path.with_extension("json.partial");
        std::fs::write(&partial, text)?;
        std::fs::rename(&partial, &path)
    });
    if let Err(error) = written {
        harness_types::diag::warn("codemode", "store_not_saved")
            .field("error", error.to_string())
            .emit();
    }
}

/// The run's limits from its options line.
pub fn run_options(
    options: &harness_codemode::SourceOptions,
) -> (harness_codemode::RunOptions, usize) {
    let timeout = options
        .timeout_ms
        .map_or(DEFAULT_TIMEOUT, Duration::from_millis)
        .min(MAX_TIMEOUT);
    let max_chars = options
        .max_output_tokens
        .map_or(DEFAULT_OUTPUT_CHARS, |tokens| {
            usize::try_from(tokens)
                .unwrap_or(usize::MAX)
                .saturating_mul(CHARS_PER_TOKEN)
                .max(200)
        });
    (
        harness_codemode::RunOptions {
            timeout: Some(timeout),
            ..harness_codemode::RunOptions::default()
        },
        max_chars,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Codemode, codemode_schema, load_store, reachable, save_store};

    fn tool(name: &str) -> serde_json::Value {
        json!({"type": "function", "function": {"name": name, "description": format!("{name} does a thing\nmore"), "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}}})
    }

    #[test]
    fn reaches_the_turn_and_deferred_tools_but_not_the_control_tools() {
        let tools = reachable(
            &[
                tool("read_file"),
                tool("codemode"),
                tool("use_tool"),
                tool("ask_user"),
            ],
            &[tool("mcp__docs__search"), tool("read_file")],
        );
        let names: Vec<(&str, bool)> = tools
            .iter()
            .map(|(tool, deferred)| (tool.name.as_str(), *deferred))
            .collect();
        assert_eq!(
            names,
            vec![("read_file", false), ("mcp__docs__search", true)]
        );
    }

    #[test]
    fn describes_the_sent_tools_as_declarations_and_names_the_deferred_ones() {
        let schema = codemode_schema(&[tool("read_file")], &[tool("mcp__docs__search")]);
        let description = schema["function"]["description"].as_str().unwrap();
        assert!(
            description.contains("read_file(args: { path: string; }): Promise<string>;"),
            "{description}"
        );
        assert!(
            description
                .contains("callable too, with the arguments tool_schema shows: mcp__docs__search.")
        );
        assert!(
            !description.contains("does a thing"),
            "descriptions are in the tool list already"
        );
        assert_eq!(
            schema["function"]["parameters"]["required"],
            json!(["code"])
        );
    }

    #[test]
    fn gives_a_program_the_files_text_without_line_numbers() {
        assert_eq!(
            super::without_line_numbers(
                "1: fn a() {}\n2: \n3: // 4: not a number\n12#a3: anchored"
            ),
            "fn a() {}\n\n// 4: not a number\nanchored"
        );
        assert_eq!(
            super::without_line_numbers(
                "1: kept\n\n[Showing lines 1-1 of 9. Use offset=2 to continue.]"
            ),
            "kept\n\n[Showing lines 1-1 of 9. Use offset=2 to continue.]"
        );
    }

    #[test]
    fn keeps_store_writes_by_session_and_on_disk() {
        let directory = tempfile::tempdir().unwrap();
        let codemode = Codemode {
            store_dir: Some(directory.path().join("codemode")),
        };
        let session = "codemode-test-session";
        let writes = harness_codemode::StoreWrites {
            set: vec![
                ("a".to_owned(), "1".to_owned()),
                ("b".to_owned(), "2".to_owned()),
            ],
            delete: Vec::new(),
        };
        save_store(&codemode, session, &writes);
        save_store(
            &codemode,
            session,
            &harness_codemode::StoreWrites {
                set: vec![("a".to_owned(), "3".to_owned())],
                delete: vec!["b".to_owned()],
            },
        );
        assert_eq!(
            load_store(&codemode, session),
            vec![("a".to_owned(), "3".to_owned())]
        );
        assert!(load_store(&codemode, "another-session").is_empty());
        // Another process finds it on disk.
        let on_disk = super::read_store_file(&super::store_file(
            directory.path().join("codemode").as_path(),
            session,
        ));
        assert_eq!(on_disk, Some(vec![("a".to_owned(), "3".to_owned())]));
        assert!(super::store_file(directory.path(), "../x y").ends_with("___x_y.json"));
    }
}
