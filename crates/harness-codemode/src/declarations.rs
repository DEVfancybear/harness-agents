//! TypeScript declarations of the tools a script can call, from their JSON
//! Schemas (pi's `declarations.ts`): what the model reads to write the
//! arguments right.
//!
//! Property descriptions become `//` comments; constraints the type cannot
//! express follow as tags (`// @integer @minimum 1 @default 20`), and are
//! the first thing dropped when a type is over its size budget. Local
//! references (`#/$defs/...`) are expanded; recursive and remote ones render
//! as `unknown`. Schemas only shape the declarations: values are not checked
//! against them here (the host checks a call's arguments).

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::identifier::to_identifier;

const INDENT: &str = "  ";
/// The largest rendered input type, in characters, before it is `unknown`.
pub const DEFAULT_INPUT_MAX_CHARS: usize = 16_000;
/// Local `$ref` expansions per schema, so shared definitions cannot blow up.
const MAX_REF_EXPANSIONS: usize = 32;
const TAG_KEYWORDS: [&str; 11] = [
    "minimum",
    "exclusiveMinimum",
    "maximum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minItems",
    "maxItems",
];
const LOWER_BOUNDS: [&str; 4] = ["minimum", "exclusiveMinimum", "minLength", "minItems"];
const UPPER_BOUNDS: [&str; 4] = ["maximum", "exclusiveMaximum", "maxLength", "maxItems"];
const MAX_TAG_CHARS: usize = 80;

/// A tool as the declarations show it.
#[derive(Clone, Debug)]
pub struct DeclaredTool {
    pub name: String,
    pub description: String,
    pub input_schema: Option<Value>,
    pub output_schema: Option<Value>,
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\r' | '\n' | '\u{2028}' | '\u{2029}')
}

/// `declare const tools: { ... };` for these tools, each with its
/// description as a doc comment when `descriptions`.
#[must_use]
pub fn render_declarations(
    tools: &[DeclaredTool],
    descriptions: bool,
    input_max_chars: usize,
) -> String {
    if tools.is_empty() {
        return String::new();
    }
    let members: Vec<String> = tools
        .iter()
        .map(|tool| {
            let doc = if descriptions {
                doc_comment(&tool.description, INDENT)
            } else {
                String::new()
            };
            format!(
                "{doc}{INDENT}{}",
                render_tool_signature(tool, input_max_chars)
            )
        })
        .collect();
    format!("declare const tools: {{\n{}\n}};", members.join("\n"))
}

/// `name(args: T): Promise<R>;` with the name the script uses.
#[must_use]
pub fn render_tool_signature(tool: &DeclaredTool, input_max_chars: usize) -> String {
    let input = tool.input_schema.as_ref().map_or_else(
        || "unknown".to_owned(),
        |schema| schema_to_type(schema, Some(input_max_chars)),
    );
    let output = tool.output_schema.as_ref().map_or_else(
        || "unknown".to_owned(),
        |schema| schema_to_type(schema, None),
    );
    format!(
        "{}(args: {input}): Promise<{output}>;",
        to_identifier(&tool.name)
    )
}

fn doc_comment(description: &str, indent: &str) -> String {
    let text = description.trim();
    if text.is_empty() {
        return String::new();
    }
    let text = text.replace("*/", "*\\/");
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() == 1 {
        return format!("{indent}/** {} */\n", lines[0]);
    }
    let body: Vec<String> = lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                format!("{indent} *")
            } else {
                format!("{indent} * {line}")
            }
        })
        .collect();
    format!("{indent}/**\n{}\n{indent} */\n", body.join("\n"))
}

fn property_key(name: &str) -> String {
    if is_identifier(name) {
        name.to_owned()
    } else {
        Value::String(name.to_owned()).to_string()
    }
}

fn union(types: Vec<String>) -> String {
    let mut unique: Vec<String> = Vec::new();
    for item in types {
        if !unique.contains(&item) {
            unique.push(item);
        }
    }
    if unique.iter().any(|item| item == "unknown") {
        return "unknown".to_owned();
    }
    if unique.is_empty() {
        "never".to_owned()
    } else {
        unique.join(" | ")
    }
}

struct Context<'a> {
    root: &'a Value,
    resolving: BTreeSet<String>,
    expansions: usize,
    tags: bool,
}

/// A JSON Schema as a TypeScript type. A type longer than `max_chars` is
/// rendered again without constraint tags, and is `unknown` if it is still
/// too long.
#[must_use]
pub fn schema_to_type(schema: &Value, max_chars: Option<usize>) -> String {
    let render = |tags: bool| {
        to_type(
            schema,
            &mut Context {
                root: schema,
                resolving: BTreeSet::new(),
                expansions: 0,
                tags,
            },
        )
    };
    let typed = render(true);
    let Some(max) = max_chars else {
        return typed;
    };
    if typed.chars().count() <= max {
        return typed;
    }
    let untagged = render(false);
    if untagged.chars().count() <= max {
        untagged
    } else {
        "unknown".to_owned()
    }
}

fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = segment.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The subschema a local `$ref` (`#`, `#/$defs/Item`) names in `root`; `None`
/// for remote references, references that name nothing, and malformed ones.
#[must_use]
pub fn resolve_schema_ref<'a>(reference: &str, root: &'a Value) -> Option<&'a Value> {
    if reference != "#" && !reference.starts_with("#/") {
        return None;
    }
    let mut current = root;
    for segment in reference
        .get(2..)
        .unwrap_or("")
        .split('/')
        .filter(|s| !s.is_empty())
    {
        let key = percent_decode(segment)?
            .replace("~1", "/")
            .replace("~0", "~");
        current = current.as_object()?.get(&key)?;
    }
    match current {
        Value::Bool(_) | Value::Object(_) => Some(current),
        _ => None,
    }
}

fn json_text(value: &Value) -> String {
    value.to_string()
}

fn to_type(schema: &Value, context: &mut Context<'_>) -> String {
    let object = match schema {
        Value::Bool(false) => return "never".to_owned(),
        Value::Object(object) => object,
        _ => return "unknown".to_owned(),
    };
    if let Some(Value::String(reference)) = object.get("$ref") {
        if context.resolving.contains(reference) || context.expansions >= MAX_REF_EXPANSIONS {
            return "unknown".to_owned();
        }
        let Some(target) = resolve_schema_ref(reference, context.root) else {
            return "unknown".to_owned();
        };
        context.expansions += 1;
        context.resolving.insert(reference.clone());
        let typed = to_type(target, context);
        context.resolving.remove(reference);
        return typed;
    }
    if let Some(constant) = object.get("const") {
        return json_text(constant);
    }
    if let Some(Value::Array(values)) = object.get("enum") {
        return union(values.iter().map(json_text).collect());
    }
    let variants = match (object.get("anyOf"), object.get("oneOf")) {
        (Some(Value::Array(variants)), _) | (_, Some(Value::Array(variants))) => Some(variants),
        _ => None,
    };
    if let Some(variants) = variants {
        return union(
            variants
                .iter()
                .map(|variant| to_type(variant, context))
                .collect(),
        );
    }
    if let Some(Value::Array(parts)) = object.get("allOf") {
        let parts: Vec<String> = parts
            .iter()
            .map(|part| to_type(part, context))
            .filter(|part| part != "unknown")
            .collect();
        if parts.is_empty() {
            return "unknown".to_owned();
        }
        return parts
            .into_iter()
            .map(|part| {
                if part.contains(" | ") {
                    format!("({part})")
                } else {
                    part
                }
            })
            .collect::<Vec<_>>()
            .join(" & ");
    }
    match object.get("type") {
        Some(Value::Array(types)) => union(
            types
                .iter()
                .map(|entry| {
                    let mut single = object.clone();
                    single.insert("type".to_owned(), entry.clone());
                    to_type(&Value::Object(single), context)
                })
                .collect(),
        ),
        Some(Value::String(kind)) => match kind.as_str() {
            "string" => "string".to_owned(),
            "number" | "integer" => "number".to_owned(),
            "boolean" => "boolean".to_owned(),
            "null" => "null".to_owned(),
            "array" => array_type(object, context),
            "object" => object_type(object, context),
            _ => "unknown".to_owned(),
        },
        None => {
            if ["properties", "additionalProperties", "required"]
                .iter()
                .any(|key| object.contains_key(*key))
            {
                object_type(object, context)
            } else if object.contains_key("items") || object.contains_key("prefixItems") {
                array_type(object, context)
            } else {
                "unknown".to_owned()
            }
        }
        _ => "unknown".to_owned(),
    }
}

fn array_type(schema: &Map<String, Value>, context: &mut Context<'_>) -> String {
    if let Some(items) = schema.get("items")
        && !items.is_array()
    {
        return format!("Array<{}>", to_type(items, context));
    }
    let tuple = match (schema.get("prefixItems"), schema.get("items")) {
        (Some(Value::Array(items)), _) | (_, Some(Value::Array(items))) => items.clone(),
        _ => Vec::new(),
    };
    if tuple.is_empty() {
        return "unknown[]".to_owned();
    }
    format!(
        "[{}]",
        tuple
            .iter()
            .map(|item| to_type(item, context))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn description_of(property: &Value) -> String {
    property
        .get("description")
        .and_then(Value::as_str)
        .map_or("", str::trim)
        .to_owned()
}

fn number_text(value: &Value) -> Option<String> {
    value.as_f64().map(|number| {
        if number.fract() == 0.0 && number.abs() < 1e15 {
            format!("{}", number as i64)
        } else {
            number.to_string()
        }
    })
}

/// Constraints a TypeScript type cannot express, as tags:
/// `@integer @minimum 1 @maximum 100 @default 20`.
fn constraint_tags(schema: &Value) -> String {
    let Value::Object(property) = schema else {
        return String::new();
    };
    // `int | None` keeps its constraints in the non-null branch.
    let variants = match (property.get("anyOf"), property.get("oneOf")) {
        (Some(Value::Array(variants)), _) | (_, Some(Value::Array(variants))) => variants.clone(),
        _ => Vec::new(),
    };
    let non_null: Vec<&Value> = variants
        .iter()
        .filter(|variant| variant.get("type").and_then(Value::as_str) != Some("null"))
        .collect();
    let mut branches: Vec<Map<String, Value>> = vec![property.clone()];
    if let [Value::Object(branch)] = non_null.as_slice() {
        branches.push(branch.clone());
    }
    // Draft-04 makes `minimum` exclusive with a boolean `exclusiveMinimum`.
    for branch in &mut branches {
        for (exclusive, bound) in [
            ("exclusiveMinimum", "minimum"),
            ("exclusiveMaximum", "maximum"),
        ] {
            if branch.get(exclusive) == Some(&Value::Bool(true)) {
                let value = branch.remove(bound).unwrap_or(Value::Null);
                branch.insert(exclusive.to_owned(), value);
            }
        }
    }
    let mut tags: Vec<String> = Vec::new();
    let is_integer = |kind: Option<&Value>| match kind {
        Some(Value::String(kind)) => kind == "integer",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == "integer"),
        _ => false,
    };
    if branches.iter().any(|branch| is_integer(branch.get("type"))) {
        tags.push("@integer".to_owned());
    }
    for keyword in TAG_KEYWORDS {
        let values: Vec<&Value> = branches
            .iter()
            .filter_map(|branch| branch.get(keyword))
            .collect();
        if LOWER_BOUNDS.contains(&keyword) || UPPER_BOUNDS.contains(&keyword) {
            let numbers: Vec<f64> = values.iter().filter_map(|value| value.as_f64()).collect();
            if numbers.is_empty() {
                continue;
            }
            let bound = if LOWER_BOUNDS.contains(&keyword) {
                numbers.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            } else {
                numbers.iter().copied().fold(f64::INFINITY, f64::min)
            };
            tags.push(format!(
                "@{keyword} {}",
                number_text(&serde_json::json!(bound)).unwrap_or_default()
            ));
            continue;
        }
        let mut seen: Vec<String> = Vec::new();
        for value in values {
            let text = match value {
                Value::Number(_) => number_text(value),
                Value::String(text) => Some(text.clone()),
                _ => None,
            };
            if let Some(text) = text
                && !seen.contains(&text)
            {
                tags.push(format!("@{keyword} {text}"));
                seen.push(text);
            }
        }
    }
    if branches
        .iter()
        .any(|branch| branch.get("uniqueItems") == Some(&Value::Bool(true)))
    {
        tags.push("@uniqueItems".to_owned());
    }
    if let Some(default) = property.get("default")
        && !default.is_null()
    {
        tags.push(format!("@default {}", json_text(default)));
    }
    if property.get("deprecated") == Some(&Value::Bool(true)) {
        tags.push("@deprecated".to_owned());
    }
    tags.into_iter()
        .filter(|tag| tag.chars().count() <= MAX_TAG_CHARS)
        .map(|tag| {
            tag.chars()
                .map(|c| {
                    if is_line_terminator(c) {
                        format!("\\u{:04x}", c as u32)
                    } else {
                        c.to_string()
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn object_type(schema: &Map<String, Value>, context: &mut Context<'_>) -> String {
    let empty = Map::new();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let required: BTreeSet<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut names: Vec<&String> = properties.keys().collect();
    names.sort();
    let mut members: Vec<String> = names
        .iter()
        .map(|name| {
            let optional = if required.contains(name.as_str()) {
                ""
            } else {
                "?"
            };
            format!(
                "{}{optional}: {};",
                property_key(name),
                to_type(&properties[name.as_str()], context)
            )
        })
        .collect();
    match schema.get("additionalProperties") {
        Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => members.push("[key: string]: unknown;".to_owned()),
        Some(additional) => {
            members.push(format!("[key: string]: {};", to_type(additional, context)));
        }
        None if names.is_empty() => members.push("[key: string]: unknown;".to_owned()),
        None => {}
    }
    if members.is_empty() {
        return "{}".to_owned();
    }
    let comments: Vec<Vec<String>> = names
        .iter()
        .map(|name| {
            let property = &properties[name.as_str()];
            let mut lines: Vec<String> = description_of(property)
                .split(is_line_terminator)
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect();
            if context.tags {
                let tags = constraint_tags(property);
                if !tags.is_empty() {
                    lines.push(tags);
                }
            }
            lines
        })
        .collect();
    if comments.iter().all(Vec::is_empty) {
        return format!("{{ {} }}", members.join(" "));
    }
    let mut lines = vec!["{".to_owned()];
    for (index, comment) in comments.iter().enumerate() {
        for line in comment {
            lines.push(format!("{INDENT}// {line}"));
        }
        lines.push(format!(
            "{INDENT}{}",
            members[index].replace('\n', &format!("\n{INDENT}"))
        ));
    }
    for member in &members[names.len()..] {
        lines.push(format!("{INDENT}{member}"));
    }
    lines.push("}".to_owned());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{DeclaredTool, render_declarations, resolve_schema_ref, schema_to_type};

    #[test]
    fn renders_primitives_literals_and_unions() {
        assert_eq!(schema_to_type(&json!({"type": "string"}), None), "string");
        assert_eq!(
            schema_to_type(&json!({"type": ["string", "null"]}), None),
            "string | null"
        );
        assert_eq!(
            schema_to_type(&json!({"enum": ["a", 1]}), None),
            "\"a\" | 1"
        );
        assert_eq!(schema_to_type(&json!({"const": true}), None), "true");
        assert_eq!(schema_to_type(&json!(true), None), "unknown");
        assert_eq!(schema_to_type(&json!(false), None), "never");
    }

    #[test]
    fn renders_objects_with_sorted_properties_and_comments() {
        assert_eq!(
            schema_to_type(
                &json!({"type": "object", "properties": {"b": {"type": "number"}, "a": {"type": "string"}}, "required": ["a"]}),
                None
            ),
            "{ a: string; b?: number; }"
        );
        assert_eq!(
            schema_to_type(
                &json!({"type": "object", "properties": {"limit": {"type": "integer", "minimum": 1, "default": 20, "description": "How many"}}}),
                None
            ),
            "{\n  // How many\n  // @integer @minimum 1 @default 20\n  limit?: number;\n}"
        );
    }

    #[test]
    fn resolves_local_references_and_stops_at_recursive_ones() {
        let schema = json!({
            "$defs": {"node": {"type": "object", "properties": {"next": {"$ref": "#/$defs/node"}}, "required": ["next"]}},
            "$ref": "#/$defs/node"
        });
        assert_eq!(schema_to_type(&schema, None), "{ next: unknown; }");
        assert!(resolve_schema_ref("#/$defs/%E0", &schema).is_none());
        assert!(resolve_schema_ref("http://x/y", &schema).is_none());
        assert!(resolve_schema_ref("#/$defs/node", &schema).is_some());
    }

    #[test]
    fn drops_tags_then_gives_up_over_the_budget() {
        let schema =
            json!({"type": "object", "properties": {"a": {"type": "integer", "minimum": 1}}});
        let tagged = schema_to_type(&schema, None);
        assert!(tagged.contains("@integer"));
        assert_eq!(schema_to_type(&schema, Some(20)), "{ a?: number; }");
        assert_eq!(schema_to_type(&schema, Some(5)), "unknown");
    }

    #[test]
    fn renders_tools_with_normalized_identifiers() {
        let text = render_declarations(
            &[DeclaredTool {
                name: "read-file".to_owned(),
                description: "Read a file */ now".to_owned(),
                input_schema: Some(
                    json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
                ),
                output_schema: Some(json!({"type": "string"})),
            }],
            true,
            16_000,
        );
        assert_eq!(
            text,
            "declare const tools: {\n  /** Read a file *\\/ now */\n  read_file(args: { path: string; }): Promise<string>;\n};"
        );
    }
}
