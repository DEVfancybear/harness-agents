//! Tool-argument validation, ported from prime-agent's `pa-agent/src/validation.rs`:
//! the TS `validateToolArguments` contract (`packages/ai/src/utils/validation.ts`).
//!
//! The TS reference validates against `TypeBox` schemas with `Value.Convert`
//! (primitive coercion) plus a compiled validator. This port implements the
//! JSON Schema subset that the product's tool schemas use: `type` (including
//! type arrays), `properties`, `required`, `items`, `enum`, `const`,
//! `additionalProperties: false`, and numeric bounds, with the same primitive
//! coercion behavior (`"42"` -> `42` for `type: "number"`, etc.). The error
//! message format matches the TS reference exactly so surfaced text is
//! identical.

// Kept as prime-agent wrote it, so the port reads line for line against the source.
#![allow(clippy::collapsible_if)]

use serde_json::Value;

/// Validates tool call arguments against the tool's JSON Schema, returning
/// the validated (and potentially coerced) arguments.
///
/// Mirrors TS `validateToolArguments(tool, toolCall)`: on failure it returns
/// the preformatted error message (TS throws `Error(message)`); the caller
/// wraps it into an error tool result.
///
/// # Errors
///
/// Returns the preformatted validation error message when the arguments fail
/// the tool's schema checks (after coercion).
#[cfg(test)]
pub fn validate_tool_arguments(
    tool_name: &str,
    schema: &Value,
    arguments: &Value,
) -> Result<Value, String> {
    let mut args = arguments.clone();
    let mut changed = false;
    coerce(schema, &mut args, &mut changed);
    let mut errors = Vec::new();
    check(schema, &args, "", &mut errors);
    if errors.is_empty() {
        return Ok(args);
    }
    Err(validation_message(tool_name, &errors, arguments))
}

/// `validate_tool_arguments` for arguments still in their JSON text, as a
/// provider sends them: `Ok(None)` when they pass unchanged, `Ok(Some(_))`
/// with the coerced arguments when coercion changed something.
///
/// The arguments are parsed once and coerced in place. The by-reference form
/// copies the whole value first to keep the original for the error message,
/// and its caller then compared the two trees - for a 1 MB `write_file` that
/// was two extra passes over the content on every call. Here the original is
/// re-read from the text only when validation fails.
///
/// # Errors
///
/// The same preformatted message as `validate_tool_arguments`. Text that is
/// not JSON is validated as `null`, as that caller always did.
pub fn validate_tool_arguments_json(
    tool_name: &str,
    schema: &Value,
    arguments: &str,
) -> Result<Option<Value>, String> {
    let mut args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    let mut changed = false;
    coerce(schema, &mut args, &mut changed);
    let mut errors = Vec::new();
    check(schema, &args, "", &mut errors);
    if errors.is_empty() {
        return Ok(changed.then_some(args));
    }
    let original: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    Err(validation_message(tool_name, &errors, &original))
}

fn validation_message(tool_name: &str, errors: &[(String, String)], arguments: &Value) -> String {
    let error_lines = errors
        .iter()
        .map(|(path, message)| format!("  - {path}: {message}"))
        .collect::<Vec<_>>()
        .join("\n");
    let error_lines = if error_lines.is_empty() {
        "Unknown validation error".to_string()
    } else {
        error_lines
    };
    let received =
        serde_json::to_string_pretty(arguments).unwrap_or_else(|_| arguments.to_string());
    format!(
        "Validation failed for tool \"{tool_name}\":\n{error_lines}\n\nReceived arguments:\n{received}"
    )
}

fn schema_type(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// Primitive coercion mirroring `TypeBox` `Value.Convert`: string values are
/// parsed into number/boolean when the schema requests it, and number/boolean
/// values are stringified when the schema requests a string.
fn coerce(schema: &Value, value: &mut Value, changed: &mut bool) {
    let types = schema_type(schema);
    if types.is_empty() {
        coerce_children(schema, value, changed);
        return;
    }
    for ty in types {
        match (ty, &*value) {
            ("number", Value::String(s)) => {
                if let Ok(n) = s.trim().parse::<f64>() {
                    *value = number_value(n);
                    *changed = true;
                    return coerce_children(schema, value, changed);
                }
            }
            ("integer", Value::String(s)) => {
                if let Ok(n) = s.trim().parse::<i64>() {
                    *value = Value::from(n);
                    *changed = true;
                    return coerce_children(schema, value, changed);
                }
            }
            ("boolean", Value::String(s)) => {
                let lower = s.trim().to_ascii_lowercase();
                if lower == "true" {
                    *value = Value::Bool(true);
                    *changed = true;
                    return coerce_children(schema, value, changed);
                }
                if lower == "false" {
                    *value = Value::Bool(false);
                    *changed = true;
                    return coerce_children(schema, value, changed);
                }
            }
            ("string", Value::Number(n)) => {
                *value = Value::String(n.to_string());
                *changed = true;
                return coerce_children(schema, value, changed);
            }
            ("string", Value::Bool(b)) => {
                *value = Value::String(b.to_string());
                *changed = true;
                return coerce_children(schema, value, changed);
            }
            _ => {}
        }
    }
    coerce_children(schema, value, changed);
}

fn coerce_children(schema: &Value, value: &mut Value, changed: &mut bool) {
    // Borrowed, not cloned: the schema and the value are separate trees, and a
    // copy of the properties map per object level was most of the cost.
    let Some(Value::Object(properties)) = schema.get("properties") else {
        return;
    };
    match value {
        Value::Object(map) => {
            for (key, sub_schema) in properties {
                if let Some(v) = map.get_mut(key) {
                    coerce(sub_schema, v, changed);
                }
            }
            // Coerce entries under `additionalProperties: { ... }` schemas too.
            if let Some(additional_schema) = schema.get("additionalProperties") {
                if additional_schema.is_object() {
                    for (key, v) in map.iter_mut() {
                        if !properties.contains_key(key) {
                            coerce(additional_schema, v, changed);
                        }
                    }
                }
            }
        }
        Value::Array(items) => {
            if let Some(Value::Array(item_schemas)) = schema.get("items") {
                // Positional tuple validation; coerce each pair.
                for (i, item) in items.iter_mut().enumerate() {
                    if let Some(s) = item_schemas.get(i) {
                        coerce(s, item, changed);
                    }
                }
            } else if let Some(item_schema) = schema.get("items") {
                for item in items.iter_mut() {
                    coerce(item_schema, item, changed);
                }
            }
        }
        _ => {}
    }
}

fn number_value(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 {
        // The guard proves the conversion exact: whole value, |n| < 2^53.
        #[allow(clippy::cast_possible_truncation)]
        let whole = n as i64;
        Value::from(whole)
    } else {
        serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)
    }
}

/// Instance path formatting mirroring TS `formatValidationPath`:
/// JSON pointer paths (`/a/b`) become dotted paths (`a.b`), and the empty
/// path reads as `root`.
fn format_path(path: &str) -> String {
    if path.is_empty() {
        "root".to_string()
    } else {
        path.to_string()
    }
}

/// Check `value` against `schema`, appending `(path, message)` errors.
// One arm per JSON Schema keyword, mirroring the TS reference's shape.
#[allow(clippy::too_many_lines)]
fn check(schema: &Value, value: &Value, path: &str, errors: &mut Vec<(String, String)>) {
    let types = schema_type(schema);
    if !types.is_empty() && !types.iter().any(|ty| type_matches(ty, value)) {
        let expected = types.join("/");
        let found = type_name(value);
        let base = format_path(path);
        errors.push((base, format!("Expected {expected}, received {found}")));
        // Type mismatch: deeper checks would only add noise.
        return;
    }

    if let Some(Value::Array(enum_values)) = schema.get("enum") {
        if !enum_values.iter().any(|allowed| allowed == value) {
            let base = format_path(path);
            errors.push((
                base,
                "Value did not match any of the expected enum values".to_string(),
            ));
        }
    }
    if let Some(expected_const) = schema.get("const") {
        if expected_const != value {
            let base = format_path(path);
            errors.push((
                base,
                "Value did not match the expected const value".to_string(),
            ));
        }
    }

    match value {
        Value::Object(map) if types.contains(&"object") || schema.get("properties").is_some() => {
            if let Some(Value::Object(properties)) = schema.get("properties") {
                for (key, sub_schema) in properties {
                    let sub_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    if let Some(v) = map.get(key) {
                        check(sub_schema, v, &sub_path, errors);
                    } else {
                        // Optional properties are skipped, mirroring
                        // standard JSON Schema.
                    }
                }
            }
            if let Some(Value::Array(required)) = schema.get("required") {
                for req in required.iter().filter_map(Value::as_str) {
                    if !map.contains_key(req) {
                        let base = format_path(path);
                        errors.push((base, format!("Required property '{req}' is missing")));
                    }
                }
            }
            if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
                if let Some(Value::Object(properties)) = schema.get("properties") {
                    for key in map.keys() {
                        if !properties.contains_key(key) {
                            let sub_path = if path.is_empty() {
                                key.clone()
                            } else {
                                format!("{path}.{key}")
                            };
                            errors.push((
                                sub_path,
                                "Property is not allowed by additionalProperties".to_string(),
                            ));
                        }
                    }
                }
            }
        }
        Value::Array(items) => {
            if let Some(Value::Array(item_schemas)) = schema.get("items") {
                for (index, item) in items.iter().enumerate() {
                    let sub_path = if path.is_empty() {
                        index.to_string()
                    } else {
                        format!("{path}.{index}")
                    };
                    if let Some(sub_schema) = item_schemas.get(index) {
                        check(sub_schema, item, &sub_path, errors);
                    }
                }
            } else if let Some(item_schema) = schema.get("items") {
                for (index, item) in items.iter().enumerate() {
                    let sub_path = if path.is_empty() {
                        index.to_string()
                    } else {
                        format!("{path}.{index}")
                    };
                    check(item_schema, item, &sub_path, errors);
                }
            }
        }
        Value::Number(n) => {
            if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
                if n.as_f64().unwrap_or(f64::MIN) < min {
                    let base = format_path(path);
                    errors.push((
                        base,
                        format!("Expected value to be greater than or equal to {min}"),
                    ));
                }
            }
            if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
                if n.as_f64().unwrap_or(f64::MAX) > max {
                    let base = format_path(path);
                    errors.push((
                        base,
                        format!("Expected value to be less than or equal to {max}"),
                    ));
                }
            }
        }
        _ => {}
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_matches(ty: &str, value: &Value) -> bool {
    match ty {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::validate_tool_arguments;

    fn read_file_schema() -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer", "minimum": 0},
                "limit": {"type": "integer", "minimum": 1, "maximum": 2000},
                "regex": {"type": "boolean"}
            },
            "required": ["path"]
        })
    }

    /// `TypeBox`'s `Value.Convert`: a number or a boolean sent as a string is
    /// taken as one, and a number sent for a string becomes its text.
    #[test]
    fn primitives_sent_as_strings_are_coerced() {
        let coerced = validate_tool_arguments(
            "read_file",
            &read_file_schema(),
            &json!({"path": 7, "limit": "50", "regex": "TRUE"}),
        )
        .expect("coerced");
        assert_eq!(coerced, json!({"path": "7", "limit": 50, "regex": true}));
    }

    /// The text form says whether coercion changed anything, and fails with
    /// the same message, echoing the arguments as they were sent.
    #[test]
    fn the_text_form_reports_changes_and_fails_alike() {
        let schema = read_file_schema();
        assert_eq!(
            super::validate_tool_arguments_json("read_file", &schema, r#"{"path":"a"}"#),
            Ok(None)
        );
        assert_eq!(
            super::validate_tool_arguments_json(
                "read_file",
                &schema,
                r#"{"path":"a","limit":"5"}"#
            ),
            Ok(Some(json!({"path": "a", "limit": 5})))
        );
        let sent = json!({"path": "a", "limit": "0"});
        assert_eq!(
            super::validate_tool_arguments_json("read_file", &schema, &sent.to_string()),
            validate_tool_arguments("read_file", &schema, &sent).map(Some)
        );
    }

    /// Every problem is named, with its path, and the arguments are echoed,
    /// in the TS reference's words.
    #[test]
    fn a_failure_names_every_problem_and_echoes_the_arguments() {
        let error = validate_tool_arguments(
            "read_file",
            &read_file_schema(),
            &json!({"limit": 0, "extra": 1}),
        )
        .expect_err("invalid");
        assert!(
            error.starts_with("Validation failed for tool \"read_file\":\n"),
            "{error}"
        );
        assert!(
            error.contains("  - limit: Expected value to be greater than or equal to 1"),
            "{error}"
        );
        assert!(
            error.contains("  - root: Required property 'path' is missing"),
            "{error}"
        );
        assert!(
            error.contains("  - extra: Property is not allowed by additionalProperties"),
            "{error}"
        );
        assert!(
            error.contains("Received arguments:\n{\n  \"extra\": 1,\n  \"limit\": 0\n}"),
            "{error}"
        );
    }
}
