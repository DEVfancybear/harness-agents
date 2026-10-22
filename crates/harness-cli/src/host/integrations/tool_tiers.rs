//! Tiered tool disclosure, after the design notes on Jev engineering for coding
//! agents: a snippet per tool first, the full schema on demand.
//!
//! The tools of MCP servers and extensions can be many and their schemas long,
//! and every request sends the whole tool list ahead of the conversation. When
//! they are deferred, the model's tool list holds the core tools, the host's
//! own and two more - `tool_schema` and `use_tool` - and the system prompt
//! lists each deferred tool on one line. The model asks for the arguments of
//! the few it needs and calls them through `use_tool`; the arguments are
//! checked against the tool's schema and the call is gated like any other.
//!
//! The roster is sorted, and the tool list does not change with what the model
//! looks at, so the cached prefix stays whole. `tools.tiered` in settings.json
//! (or `HA_TOOL_TIERS`) is `auto` - deferred when their schemas are long -
//! `on` or `off`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

use super::paths::LaunchEnvironment;

/// The settings.json key that holds `tiered`.
pub const SETTING: &str = "tools";
/// Overrides `tools.tiered`: `auto`, `on` or `off`.
pub const MODE_VARIABLE: &str = "HA_TOOL_TIERS";
/// In `auto`, the deferrable tools are deferred once their schemas take this
/// much (about 3k tokens): a few small tools cost less in the list than a
/// round trip to read their arguments.
const AUTO_DEFER_AT_BYTES: usize = 12_000;
/// The longest one-line description in the roster.
const ROSTER_LINE_CHARS: usize = 140;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Auto,
    On,
    Off,
}

/// The mode: `HA_TOOL_TIERS`, else settings.json `tools.tiered`, else `auto`.
#[must_use]
pub fn mode(config_file: &Path, environment: &LaunchEnvironment) -> Mode {
    let parse = |text: &str| match text.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" => Some(Mode::On),
        "off" | "false" | "0" => Some(Mode::Off),
        "auto" => Some(Mode::Auto),
        _ => None,
    };
    if let Some(mode) = environment
        .value(MODE_VARIABLE)
        .and_then(|value| value.to_str())
        .and_then(parse)
    {
        return mode;
    }
    match super::config::load_setting(config_file, SETTING)
        .unwrap_or(Value::Null)
        .get("tiered")
    {
        Some(Value::Bool(true)) => Mode::On,
        Some(Value::Bool(false)) => Mode::Off,
        Some(Value::String(text)) => parse(text).unwrap_or(Mode::Auto),
        _ => Mode::Auto,
    }
}

fn name_of(schema: &Value) -> Option<&str> {
    schema
        .get("function")
        .unwrap_or(schema)
        .get("name")
        .and_then(Value::as_str)
}

/// The names of the tools that may be deferred: those of MCP servers and
/// extensions. The core tools and the host's own stay in the list.
#[must_use]
pub fn deferrable(groups: &[Vec<Value>]) -> BTreeSet<String> {
    groups
        .iter()
        .flatten()
        .filter_map(name_of)
        .map(str::to_owned)
        .collect()
}

/// The tool list split into what is sent and what is deferred, in the order
/// given (already sorted for the cache).
#[must_use]
pub fn split(
    schemas: Vec<Value>,
    deferrable: &BTreeSet<String>,
    mode: Mode,
) -> (Vec<Value>, Vec<Value>) {
    if mode == Mode::Off || deferrable.is_empty() {
        return (schemas, Vec::new());
    }
    let (deferred, sent): (Vec<Value>, Vec<Value>) = schemas
        .into_iter()
        .partition(|schema| name_of(schema).is_some_and(|name| deferrable.contains(name)));
    let size = deferred
        .iter()
        .map(|schema| schema.to_string().len())
        .sum::<usize>();
    if mode == Mode::Auto && size < AUTO_DEFER_AT_BYTES {
        let mut all = sent;
        all.extend(deferred);
        return (all, Vec::new());
    }
    (sent, deferred)
}

/// A deferred tool's one line: its name and the start of its description.
#[must_use]
pub fn snippet(schema: &Value) -> String {
    let function = schema.get("function").unwrap_or(schema);
    let description = function
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let first = description
        .split_inclusive(". ")
        .next()
        .unwrap_or(description)
        .trim();
    let mut line = first.chars().take(ROSTER_LINE_CHARS).collect::<String>();
    if first.chars().count() > ROSTER_LINE_CHARS {
        line.push('…');
    }
    format!("{}: {line}", name_of(schema).unwrap_or("?"))
}

/// The system prompt's roster of deferred tools.
#[must_use]
pub fn roster(deferred: &[Value]) -> String {
    let mut text = format!(
        "## More tools\n\nThese {} tools are not in your tool list, to keep it short. To use one, call `{}` with its name to see its arguments, then call `{}` with `{{\"name\": ..., \"arguments\": {{...}}}}`.\n",
        deferred.len(),
        harness_tools::TOOL_SCHEMA_TOOL,
        harness_tools::USE_TOOL_TOOL
    );
    for schema in deferred {
        let _ = write!(text, "\n- {}", snippet(schema));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{Mode, deferrable, roster, snippet, split};
    use serde_json::json;

    fn tool(name: &str, description: &str) -> serde_json::Value {
        json!({"type": "function", "function": {"name": name, "description": description, "parameters": {"type": "object", "properties": {"q": {"type": "string", "description": "x".repeat(400)}}}}})
    }

    /// In `auto` a few small tools stay in the list; many long ones are
    /// deferred, and the core tools never are.
    #[test]
    fn long_deferrable_tools_are_deferred_and_the_rest_stay() {
        let schemas = |count: usize| {
            let mut all = vec![tool("read_file", "Read a file.")];
            all.extend(
                (0..count)
                    .map(|index| tool(&format!("mcp_{index:02}"), "Query the thing. More words.")),
            );
            all
        };
        let names = deferrable(&[schemas(40)[1..].to_vec()]);
        let (sent, deferred) = split(schemas(2), &names, Mode::Auto);
        assert_eq!((sent.len(), deferred.len()), (3, 0), "small: all sent");
        let (sent, deferred) = split(schemas(40), &names, Mode::Auto);
        assert_eq!((sent.len(), deferred.len()), (1, 40));
        assert_eq!(sent[0]["function"]["name"], "read_file");
        let (sent, deferred) = split(schemas(2), &names, Mode::On);
        assert_eq!(
            (sent.len(), deferred.len()),
            (1, 2),
            "on: deferred however small"
        );
        let (sent, deferred) = split(schemas(40), &names, Mode::Off);
        assert_eq!((sent.len(), deferred.len()), (41, 0));
    }

    /// The roster has one line per tool: its name and its first sentence.
    #[test]
    fn the_roster_lists_each_tool_on_one_line() {
        assert_eq!(
            snippet(&tool(
                "mcp_issue",
                "Create an issue. Needs a title and a body."
            )),
            "mcp_issue: Create an issue."
        );
        let text = roster(&[tool("a", "First."), tool("b", "Second.")]);
        assert!(
            text.contains("These 2 tools are not in your tool list"),
            "{text}"
        );
        assert!(text.contains("call `tool_schema`") && text.contains("call `use_tool`"));
        assert!(text.ends_with("\n- a: First.\n- b: Second."), "{text}");
    }
}
