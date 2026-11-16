//! What a script finds besides its own code: property access on every kind
//! of value, the global functions and constructors, `Math`, `JSON`,
//! `Object`, `Number`, `Promise`, `console`, and codemode's own `tools`,
//! `ALL_TOOLS`, `text`, `exit`, `store` and `load`.
//!
//! A built-in is a native function value named after what it is
//! (`"Array#map"` for a method, bound to the array; `"Math.max"` for a
//! static); [`call_native`] dispatches on that name.

use std::fmt::Write as _;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::interp::{Env, Flow, Interp, R, quote};
use crate::value::{
    Class, Combine, Function, Promise, PromiseState, Settled, Value, array_index, json_stringify,
    number_to_string, to_key, to_number, to_string,
};
use crate::{arrays, dates, strings};

const OBJECT_METHODS: [&str; 5] = [
    "hasOwnProperty",
    "toString",
    "valueOf",
    "toLocaleString",
    "propertyIsEnumerable",
];
const FUNCTION_METHODS: [&str; 4] = ["call", "apply", "bind", "toString"];
const PROMISE_METHODS: [&str; 3] = ["then", "catch", "finally"];
const NUMBER_METHODS: [&str; 5] = [
    "toFixed",
    "toPrecision",
    "toString",
    "toLocaleString",
    "valueOf",
];
const CONSOLE_METHODS: [&str; 5] = ["log", "info", "warn", "error", "debug"];
const ERRORS: [&str; 8] = [
    "Error",
    "TypeError",
    "RangeError",
    "SyntaxError",
    "ReferenceError",
    "EvalError",
    "URIError",
    "AggregateError",
];
const CONSTRUCTORS: [&str; 11] = [
    "Array", "Object", "Map", "Set", "Date", "RegExp", "Promise", "Number", "String", "Boolean",
    "Function",
];
const GLOBAL_FUNCTIONS: [&str; 12] = [
    "parseInt",
    "parseFloat",
    "isNaN",
    "isFinite",
    "encodeURIComponent",
    "decodeURIComponent",
    "encodeURI",
    "decodeURI",
    "structuredClone",
    "text",
    "exit",
    "store",
];
/// What a script reaches by name without declaring it, besides the
/// constructors.
const NAMESPACES: [&str; 4] = ["Math", "JSON", "console", "globalThis"];

pub fn install_globals(interp: &Interp<'_>, global: &Env) {
    let listed: Vec<Value> = interp
        .tools
        .iter()
        .map(|tool| {
            let entry = Value::object_from([
                ("name", Value::str(tool.js_name.as_str())),
                ("description", Value::str(tool.description.as_str())),
            ]);
            freeze(&entry);
            entry
        })
        .collect();
    let all = Value::array(listed);
    freeze(&all);
    global.declare(Rc::from("ALL_TOOLS"), all, false);
}

pub fn freeze(value: &Value) {
    match value {
        Value::Object(object) => object.frozen.set(true),
        Value::Array(array) => array.frozen.set(true),
        _ => {}
    }
}

/// A name the script did not declare: a built-in, or nothing.
pub fn global(name: &str) -> Option<Value> {
    Some(match name {
        "undefined" => Value::Undefined,
        "NaN" => Value::Number(f64::NAN),
        "Infinity" => Value::Number(f64::INFINITY),
        "tools" => Value::Tools,
        "load" => Value::native("load", Value::Undefined),
        name if NAMESPACES.contains(&name) => Value::Namespace(
            NAMESPACES
                .iter()
                .find(|n| **n == name)
                .copied()
                .unwrap_or("Math"),
        ),
        name if CONSTRUCTORS.contains(&name)
            || ERRORS.contains(&name)
            || GLOBAL_FUNCTIONS.contains(&name) =>
        {
            Value::native(name, Value::Undefined)
        }
        _ => return None,
    })
}

fn static_names(owner: &str) -> &'static [&'static str] {
    match owner {
        "Math" => &[
            "abs", "acos", "asin", "atan", "atan2", "cbrt", "ceil", "cos", "exp", "floor", "hypot",
            "log", "log10", "log1p", "log2", "max", "min", "pow", "random", "round", "sign", "sin",
            "sqrt", "tan", "trunc", "E", "LN2", "LN10", "LOG2E", "LOG10E", "PI", "SQRT1_2",
            "SQRT2",
        ],
        "JSON" => &["parse", "stringify"],
        "console" => &CONSOLE_METHODS,
        "Object" => &[
            "keys",
            "values",
            "entries",
            "assign",
            "fromEntries",
            "freeze",
            "isFrozen",
            "seal",
            "create",
            "getOwnPropertyNames",
            "hasOwn",
            "defineProperty",
            "getPrototypeOf",
            "groupBy",
        ],
        "Array" => &["isArray", "from", "of"],
        "Number" => &[
            "isInteger",
            "isFinite",
            "isNaN",
            "isSafeInteger",
            "parseFloat",
            "parseInt",
            "MAX_SAFE_INTEGER",
            "MIN_SAFE_INTEGER",
            "EPSILON",
            "MAX_VALUE",
            "MIN_VALUE",
            "POSITIVE_INFINITY",
            "NEGATIVE_INFINITY",
            "NaN",
        ],
        "String" => &["fromCharCode", "fromCodePoint"],
        "Promise" => &["all", "allSettled", "race", "any", "resolve", "reject"],
        "Date" => &["now", "parse", "UTC"],
        "Map" => &["groupBy"],
        _ => &[],
    }
}

fn static_member(owner: &str, key: &str) -> Option<Value> {
    if owner == "globalThis" {
        return global(key);
    }
    if !static_names(owner).contains(&key) {
        return None;
    }
    let constant = match (owner, key) {
        ("Math", "E") => std::f64::consts::E,
        ("Math", "LN2") => std::f64::consts::LN_2,
        ("Math", "LN10") => std::f64::consts::LN_10,
        ("Math", "LOG2E") => std::f64::consts::LOG2_E,
        ("Math", "LOG10E") => std::f64::consts::LOG10_E,
        ("Math", "PI") => std::f64::consts::PI,
        ("Math", "SQRT1_2") => std::f64::consts::FRAC_1_SQRT_2,
        ("Math", "SQRT2") => std::f64::consts::SQRT_2,
        ("Number", "MAX_SAFE_INTEGER") => 9_007_199_254_740_991.0,
        ("Number", "MIN_SAFE_INTEGER") => -9_007_199_254_740_991.0,
        ("Number", "EPSILON") => f64::EPSILON,
        ("Number", "MAX_VALUE") => f64::MAX,
        ("Number", "MIN_VALUE") => 5e-324,
        ("Number", "POSITIVE_INFINITY") => f64::INFINITY,
        ("Number", "NEGATIVE_INFINITY") => f64::NEG_INFINITY,
        ("Number", "NaN") => f64::NAN,
        _ => return Some(Value::native(format!("{owner}.{key}"), Value::Undefined)),
    };
    Some(Value::Number(constant))
}

fn native_name(function: &Function) -> Option<&str> {
    match function {
        Function::Native { name, .. } => Some(name),
        Function::Closure { .. } => None,
    }
}

/// What a value is, for messages: `undefined`, `"text"`, `object`.
pub fn describe(value: &Value) -> String {
    match value {
        Value::Undefined | Value::Null | Value::Bool(_) | Value::Number(_) => to_string(value),
        Value::Str(text) => {
            let short: String = text.chars().take(40).collect();
            quote(&short)
        }
        Value::Function(_) => "function".to_owned(),
        Value::Array(_) => "array".to_owned(),
        _ => "object".to_owned(),
    }
}

/// `String(value)` where a template literal or `+` needs it.
pub fn to_display_string(value: &Value) -> String {
    to_string(value)
}

/// `ToPrimitive`: objects become their string form; a date its ISO text,
/// so two dates compare in time order.
pub fn to_primitive(value: &Value) -> Value {
    match value {
        Value::Undefined | Value::Null | Value::Bool(_) | Value::Number(_) | Value::Str(_) => {
            value.clone()
        }
        other => Value::str(to_string(other)),
    }
}

pub fn iterate(value: &Value) -> Option<Vec<Value>> {
    Some(match value {
        Value::Array(array) => array.items.borrow().clone(),
        Value::Str(text) => text.chars().map(|c| Value::str(c.to_string())).collect(),
        Value::Set(set) => set.borrow().values().cloned().collect(),
        Value::Map(map) => map
            .borrow()
            .values()
            .map(|(key, value)| Value::array(vec![key.clone(), value.clone()]))
            .collect(),
        _ => return None,
    })
}

pub fn for_in_keys(value: &Value) -> Vec<Value> {
    own_entries(value)
        .into_iter()
        .map(|(key, _)| Value::Str(key))
        .collect()
}

/// Own enumerable properties, in order: what `Object.keys`, spread and
/// `for...in` see.
pub fn own_entries(value: &Value) -> Vec<(Rc<str>, Value)> {
    match value {
        Value::Object(object) => object
            .props
            .borrow()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        Value::Array(array) => {
            let mut entries: Vec<(Rc<str>, Value)> = array
                .items
                .borrow()
                .iter()
                .enumerate()
                .map(|(index, item)| (Rc::from(index.to_string()), item.clone()))
                .collect();
            entries.extend(
                array
                    .extra
                    .borrow()
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
            entries
        }
        Value::Str(text) => text
            .chars()
            .enumerate()
            .map(|(index, c)| (Rc::from(index.to_string()), Value::str(c.to_string())))
            .collect(),
        _ => Vec::new(),
    }
}

/// The names a script could have meant, for an error that names them.
fn close_matches(wanted: &str, names: &[String]) -> Vec<String> {
    let comparable = |name: &str| {
        name.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect::<String>()
    };
    let wanted = comparable(wanted);
    let exact: Vec<String> = names
        .iter()
        .filter(|name| comparable(name) == wanted)
        .cloned()
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    if wanted.is_empty() {
        return Vec::new();
    }
    names
        .iter()
        .filter(|name| {
            let name = comparable(name);
            !name.is_empty() && (name.contains(&wanted) || wanted.contains(&name))
        })
        .cloned()
        .collect()
}

fn tool_named<'a>(interp: &'a Interp<'_>, key: &str) -> Option<&'a crate::ToolInfo> {
    interp
        .tools
        .iter()
        .find(|tool| tool.js_name == key)
        .or_else(|| interp.tools.iter().find(|tool| tool.name == key))
}

fn missing_tool(interp: &Interp<'_>, key: &str) -> Flow {
    let names: Vec<String> = interp
        .tools
        .iter()
        .map(|tool| tool.js_name.clone())
        .collect();
    let mut message = format!("tools.{key} does not exist.");
    let close = close_matches(key, &names);
    if !close.is_empty() {
        let shown: Vec<String> = close
            .iter()
            .take(5)
            .map(|name| format!("tools.{name}"))
            .collect();
        let _ = write!(message, " Did you mean {}?", shown.join(", "));
    } else if names.len() <= 20 {
        let _ = write!(message, " Available: {}.", names.join(", "));
    }
    let _ = write!(
        message,
        " ALL_TOOLS lists every tool. Check for a member with \"{key}\" in tools."
    );
    interp.type_error(message)
}

#[allow(
    clippy::too_many_lines,
    reason = "one arm per kind of value, each with its own properties"
)]
pub fn get_property(interp: &mut Interp<'_>, object: &Value, key: &Value) -> R<Value> {
    let key = to_key(key);
    let method = |prefix: &str, names: &[&str]| {
        names
            .contains(&&*key)
            .then(|| Value::native(format!("{prefix}#{key}"), object.clone()))
    };
    let object_method = || method("Object", &OBJECT_METHODS);
    Ok(match object {
        Value::Undefined | Value::Null => {
            return Err(interp.type_error(format!(
                "Cannot read properties of {} (reading '{key}')",
                to_string(object)
            )));
        }
        Value::Str(text) => {
            if &*key == "length" {
                Value::Number(strings::length(text) as f64)
            } else if let Some(index) = array_index(&key) {
                strings::char_at(text, index).map_or(Value::Undefined, Value::str)
            } else {
                method("String", strings::METHODS)
                    .or_else(object_method)
                    .unwrap_or(Value::Undefined)
            }
        }
        Value::Number(_) => method("Number", &NUMBER_METHODS)
            .or_else(object_method)
            .unwrap_or(Value::Undefined),
        Value::Bool(_) => method("Boolean", &["toString", "valueOf"]).unwrap_or(Value::Undefined),
        Value::Array(array) => {
            if &*key == "length" {
                Value::Number(array.items.borrow().len() as f64)
            } else if let Some(index) = array_index(&key) {
                array
                    .items
                    .borrow()
                    .get(index)
                    .cloned()
                    .unwrap_or(Value::Undefined)
            } else if let Some(value) = array.extra.borrow().get(&*key) {
                value.clone()
            } else {
                method("Array", arrays::ARRAY_METHODS)
                    .or_else(object_method)
                    .unwrap_or(Value::Undefined)
            }
        }
        Value::Object(inner) => {
            if let Some(value) = inner.props.borrow().get(&*key) {
                return Ok(value.clone());
            }
            object_method().unwrap_or(Value::Undefined)
        }
        Value::Function(function) => match &*key {
            "name" => Value::str(match &**function {
                Function::Closure { def, .. } => def.name.as_deref().unwrap_or("").to_owned(),
                Function::Native { name, .. } => {
                    name.rsplit(['.', '#']).next().unwrap_or(name).to_owned()
                }
            }),
            "length" => Value::Number(match &**function {
                Function::Closure { def, .. } => def.params.len() as f64,
                Function::Native { .. } => 1.0,
            }),
            _ => {
                if let Some(value) =
                    native_name(function).and_then(|name| static_member(name, &key))
                {
                    value
                } else {
                    method("Function", &FUNCTION_METHODS).unwrap_or(Value::Undefined)
                }
            }
        },
        Value::Promise(_) => method("Promise", &PROMISE_METHODS)
            .or_else(object_method)
            .unwrap_or(Value::Undefined),
        Value::Map(map) => {
            if &*key == "size" {
                Value::Number(map.borrow().len() as f64)
            } else {
                method("Map", arrays::MAP_METHODS)
                    .or_else(object_method)
                    .unwrap_or(Value::Undefined)
            }
        }
        Value::Set(set) => {
            if &*key == "size" {
                Value::Number(set.borrow().len() as f64)
            } else {
                method("Set", arrays::SET_METHODS)
                    .or_else(object_method)
                    .unwrap_or(Value::Undefined)
            }
        }
        Value::Regex(regex) => match &*key {
            "source" => Value::Str(regex.source.clone()),
            "flags" => Value::Str(regex.flags.clone()),
            "global" => Value::Bool(regex.flags.contains('g')),
            "ignoreCase" => Value::Bool(regex.flags.contains('i')),
            "multiline" => Value::Bool(regex.flags.contains('m')),
            "sticky" => Value::Bool(regex.flags.contains('y')),
            "unicode" => Value::Bool(regex.flags.contains('u')),
            "lastIndex" => Value::Number(regex.last_index.get() as f64),
            _ => method("RegExp", &["test", "exec", "toString"]).unwrap_or(Value::Undefined),
        },
        Value::Date(_) => method("Date", dates::METHODS)
            .or_else(object_method)
            .unwrap_or(Value::Undefined),
        Value::Namespace(name) => static_member(name, &key).unwrap_or(Value::Undefined),
        Value::Tools => {
            if let Some(tool) = tool_named(interp, &key) {
                Value::native("tool", Value::str(tool.name.as_str()))
            } else if matches!(&*key, "then" | "toJSON" | "constructor") {
                Value::Undefined
            } else if let Some(method) = object_method() {
                method
            } else {
                return Err(missing_tool(interp, &key));
            }
        }
    })
}

pub fn has_property(interp: &mut Interp<'_>, object: &Value, key: &str) -> bool {
    match object {
        Value::Object(inner) => {
            inner.props.borrow().contains_key(key) || OBJECT_METHODS.contains(&key)
        }
        Value::Array(array) => {
            key == "length"
                || array_index(key).is_some_and(|index| index < array.items.borrow().len())
                || array.extra.borrow().contains_key(key)
                || arrays::ARRAY_METHODS.contains(&key)
        }
        Value::Tools => tool_named(interp, key).is_some(),
        Value::Map(_) => key == "size" || arrays::MAP_METHODS.contains(&key),
        Value::Set(_) => key == "size" || arrays::SET_METHODS.contains(&key),
        Value::Namespace(name) => static_member(name, key).is_some(),
        Value::Function(function) => {
            matches!(key, "name" | "length")
                || FUNCTION_METHODS.contains(&key)
                || native_name(function).is_some_and(|name| static_member(name, key).is_some())
        }
        _ => false,
    }
}

pub fn set_property(interp: &mut Interp<'_>, object: &Value, key: &Value, value: Value) -> R<()> {
    let key = to_key(key);
    match object {
        Value::Undefined | Value::Null => Err(interp.type_error(format!(
            "Cannot set properties of {} (setting '{key}')",
            to_string(object)
        ))),
        Value::Object(inner) => {
            if inner.frozen.get() {
                return Err(interp.type_error(format!(
                    "Cannot assign to read only property '{key}' of object"
                )));
            }
            interp.alloc(32)?;
            inner.props.borrow_mut().insert(key, value);
            Ok(())
        }
        Value::Array(array) => {
            if array.frozen.get() {
                return Err(interp.type_error(format!(
                    "Cannot assign to read only property '{key}' of object"
                )));
            }
            if &*key == "length" {
                let length = to_number(&value);
                if !(length >= 0.0 && length.fract() == 0.0)
                    || length as usize > crate::interp::MAX_ARRAY_LENGTH
                {
                    return Err(interp.throw("RangeError", "Invalid array length"));
                }
                let length = length as usize;
                let grow = length.saturating_sub(array.items.borrow().len());
                interp.alloc(grow * 16)?;
                array.items.borrow_mut().resize(length, Value::Undefined);
                return Ok(());
            }
            if let Some(index) = array_index(&key) {
                if index >= crate::interp::MAX_ARRAY_LENGTH {
                    return Err(interp.throw("RangeError", "Invalid array length"));
                }
                let mut items = array.items.borrow_mut();
                if index >= items.len() {
                    let grow = index + 1 - items.len();
                    drop(items);
                    interp.alloc(grow * 16)?;
                    items = array.items.borrow_mut();
                    items.resize(index + 1, Value::Undefined);
                }
                items[index] = value;
                return Ok(());
            }
            array.extra.borrow_mut().insert(key, value);
            Ok(())
        }
        Value::Regex(regex) if &*key == "lastIndex" => {
            regex.last_index.set(to_number(&value).max(0.0) as usize);
            Ok(())
        }
        Value::Str(_) | Value::Number(_) | Value::Bool(_) => Err(interp.type_error(format!(
            "Cannot create property '{key}' on {} {}",
            object.type_of(),
            describe(object)
        ))),
        Value::Tools | Value::Namespace(_) => Err(interp.type_error(format!(
            "Cannot assign to read only property '{key}' of object"
        ))),
        // Functions, collections, promises and dates take no properties
        // here; assigning one is not an error in JavaScript either.
        _ => Ok(()),
    }
}

pub fn delete_property(interp: &mut Interp<'_>, object: &Value, key: &Value) -> R<()> {
    let key = to_key(key);
    match object {
        Value::Object(inner) => {
            if inner.frozen.get() {
                return Err(interp.type_error(format!("Cannot delete property '{key}' of object")));
            }
            inner.props.borrow_mut().shift_remove(&*key);
        }
        Value::Array(array) => {
            if array.frozen.get() {
                return Err(interp.type_error(format!("Cannot delete property '{key}' of object")));
            }
            if let Some(index) = array_index(&key) {
                if let Some(item) = array.items.borrow_mut().get_mut(index) {
                    *item = Value::Undefined;
                }
            } else {
                array.extra.borrow_mut().shift_remove(&*key);
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn instance_of(value: &Value, constructor: &Value) -> bool {
    let Value::Function(function) = constructor else {
        return false;
    };
    let Some(name) = native_name(function) else {
        return false;
    };
    match name {
        "Error" => value.is_error(),
        name if ERRORS.contains(&name) => {
            value.is_error() && value.own("name").is_some_and(|own| to_string(&own) == name)
        }
        "Array" => matches!(value, Value::Array(_)),
        "Map" => matches!(value, Value::Map(_)),
        "Set" => matches!(value, Value::Set(_)),
        "Date" => matches!(value, Value::Date(_)),
        "RegExp" => matches!(value, Value::Regex(_)),
        "Promise" => matches!(value, Value::Promise(_)),
        "Function" => matches!(value, Value::Function(_)),
        "Object" => !matches!(
            value,
            Value::Undefined | Value::Null | Value::Bool(_) | Value::Number(_) | Value::Str(_)
        ),
        _ => false,
    }
}

pub fn make_regex(interp: &mut Interp<'_>, pattern: &str, flags: &str) -> R<Value> {
    strings::make_regex(interp, pattern, flags)
}

pub fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

/// `ToIntegerOrInfinity`.
pub fn to_integer(value: &Value) -> f64 {
    let number = to_number(value);
    if number.is_nan() { 0.0 } else { number.trunc() }
}

/// A relative index (`slice(-2)`) as a position in `0..=length`.
pub fn relative_index(value: &Value, length: usize, default: usize) -> usize {
    if matches!(value, Value::Undefined) {
        return default;
    }
    let index = to_integer(value);
    let length_f = length as f64;
    if index < 0.0 {
        (length_f + index).max(0.0) as usize
    } else {
        index.min(length_f) as usize
    }
}

pub fn require_function(interp: &Interp<'_>, value: &Value, what: &str) -> R<()> {
    if matches!(value, Value::Function(_)) {
        Ok(())
    } else {
        Err(interp.type_error(format!("{} is not a function ({what})", describe(value))))
    }
}

/// `console.*` and thrown values: strings as they are, errors with their
/// stack, numbers as `String()` prints them, the rest as JSON.
pub fn format_value(value: &Value) -> String {
    match value {
        Value::Str(text) => text.to_string(),
        Value::Number(number) => number_to_string(*number),
        value if value.is_error() => value
            .own("stack")
            .map_or_else(|| to_string(value), |stack| to_string(&stack)),
        value => match json_stringify(value, "") {
            Ok(Some(json)) => json,
            _ => to_string(value),
        },
    }
}

fn text_of(value: &Value) -> String {
    match value {
        Value::Undefined | Value::Null | Value::Bool(_) | Value::Number(_) | Value::Str(_) => {
            to_string(value)
        }
        value => match json_stringify(value, "") {
            Ok(Some(json)) => json,
            _ => to_string(value),
        },
    }
}

fn parse_int(text: &str, radix: &Value) -> f64 {
    let text = text.trim_start();
    let (sign, mut digits) = match text.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, text.strip_prefix('+').unwrap_or(text)),
    };
    let mut radix = to_integer(radix) as u32;
    if (radix == 0 || radix == 16)
        && let Some(rest) = digits
            .strip_prefix("0x")
            .or_else(|| digits.strip_prefix("0X"))
    {
        digits = rest;
        radix = 16;
    }
    if radix == 0 {
        radix = 10;
    }
    if !(2..=36).contains(&radix) {
        return f64::NAN;
    }
    let valid: Vec<u32> = digits.chars().map_while(|c| c.to_digit(radix)).collect();
    if valid.is_empty() {
        return f64::NAN;
    }
    sign * valid.into_iter().fold(0.0, |value, digit| {
        value * f64::from(radix) + f64::from(digit)
    })
}

fn parse_float(text: &str) -> f64 {
    let text = text.trim_start();
    for prefix in ["Infinity", "+Infinity"] {
        if text.starts_with(prefix) {
            return f64::INFINITY;
        }
    }
    if text.starts_with("-Infinity") {
        return f64::NEG_INFINITY;
    }
    let mut end = 0;
    let bytes = text.as_bytes();
    let mut seen_digit = false;
    let mut seen_dot = false;
    let mut seen_exp = false;
    while end < bytes.len() {
        let c = bytes[end];
        if c.is_ascii_digit() {
            seen_digit = true;
        } else if (c == b'+' || c == b'-') && (end == 0 || matches!(bytes[end - 1], b'e' | b'E')) {
        } else if c == b'.' && !seen_dot && !seen_exp {
            seen_dot = true;
        } else if (c == b'e' || c == b'E') && seen_digit && !seen_exp {
            seen_exp = true;
        } else {
            break;
        }
        end += 1;
    }
    let mut candidate = &text[..end];
    while !candidate.is_empty() {
        if let Ok(value) = candidate.parse::<f64>() {
            return value;
        }
        candidate = &candidate[..candidate.len() - 1];
    }
    f64::NAN
}

fn uri_encode(text: &str, keep: &str) -> Value {
    let mut out = String::new();
    for byte in text.bytes() {
        let c = char::from(byte);
        if byte.is_ascii_alphanumeric() || keep.contains(c) {
            out.push(c);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    Value::str(out)
}

fn uri_decode(interp: &Interp<'_>, text: &str, keep: &str) -> R<Value> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text
                .get(index + 1..index + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            let Some(byte) = hex else {
                return Err(interp.throw("URIError", "URI malformed"));
            };
            if byte < 0x80 && keep.contains(char::from(byte)) {
                out.extend_from_slice(&bytes[index..index + 3]);
            } else {
                out.push(byte);
            }
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out)
        .map(Value::str)
        .map_err(|_| interp.throw("URIError", "URI malformed"))
}

fn deep_clone(interp: &mut Interp<'_>, value: &Value, depth: usize) -> R<Value> {
    if depth > 1000 {
        return Err(interp.throw(
            "RangeError",
            "structuredClone: the value is nested too deeply",
        ));
    }
    Ok(match value {
        Value::Array(array) => {
            let items = array.items.borrow().clone();
            let mut copied = Vec::with_capacity(items.len());
            for item in &items {
                copied.push(deep_clone(interp, item, depth + 1)?);
            }
            interp.alloc(copied.len() * 16)?;
            Value::array(copied)
        }
        Value::Object(object) => {
            let props: Vec<(Rc<str>, Value)> = object
                .props
                .borrow()
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            let mut copied = IndexMap::new();
            for (key, item) in props {
                copied.insert(key, deep_clone(interp, &item, depth + 1)?);
            }
            interp.alloc(copied.len() * 32)?;
            if object.class == Class::Error {
                let error = interp.error_value("Error", "");
                if let Value::Object(inner) = &error {
                    *inner.props.borrow_mut() = copied;
                }
                error
            } else {
                Value::object(copied)
            }
        }
        Value::Map(map) => {
            let entries: Vec<_> = map
                .borrow()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let mut copied = IndexMap::new();
            for (key, (k, v)) in entries {
                copied.insert(
                    key,
                    (
                        deep_clone(interp, &k, depth + 1)?,
                        deep_clone(interp, &v, depth + 1)?,
                    ),
                );
            }
            Value::Map(Rc::new(std::cell::RefCell::new(copied)))
        }
        Value::Set(set) => {
            let entries: Vec<_> = set
                .borrow()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let mut copied = IndexMap::new();
            for (key, item) in entries {
                copied.insert(key, deep_clone(interp, &item, depth + 1)?);
            }
            Value::Set(Rc::new(std::cell::RefCell::new(copied)))
        }
        Value::Function(_) | Value::Promise(_) | Value::Tools | Value::Namespace(_) => {
            return Err(interp.throw(
                "DataCloneError",
                format!("{} could not be cloned", describe(value)),
            ));
        }
        other => other.clone(),
    })
}

fn object_from_entries(interp: &mut Interp<'_>, source: &Value) -> R<Value> {
    let mut props = IndexMap::new();
    for entry in interp.iterate(source)? {
        let key = get_property(interp, &entry, &Value::Number(0.0))?;
        let value = get_property(interp, &entry, &Value::Number(1.0))?;
        props.insert(to_key(&key), value);
    }
    interp.alloc(props.len() * 32)?;
    Ok(Value::object(props))
}

fn promise_value(settled: Settled) -> Value {
    Value::Promise(Promise::new(PromiseState::Settled(settled)))
}

/// `then`, `catch`, `finally`.
fn promise_method(method: &str, this: &Value, args: &[Value]) -> Value {
    let base = match this {
        Value::Promise(promise) => Rc::clone(promise),
        other => Promise::new(PromiseState::Settled(Settled::Fulfilled(other.clone()))),
    };
    let handler = |index: usize| {
        args.get(index)
            .filter(|value| matches!(value, Value::Function(_)))
            .cloned()
    };
    let state = match method {
        "then" => PromiseState::Then {
            base,
            on_fulfilled: handler(0),
            on_rejected: handler(1),
            finally: false,
        },
        "catch" => PromiseState::Then {
            base,
            on_fulfilled: None,
            on_rejected: handler(0),
            finally: false,
        },
        _ => PromiseState::Then {
            base,
            on_fulfilled: handler(0),
            on_rejected: None,
            finally: true,
        },
    };
    Value::Promise(Promise::new(state))
}

fn combine(interp: &mut Interp<'_>, how: Combine, source: &Value) -> R<Value> {
    let items = interp.iterate(source)?;
    Ok(Value::Promise(Promise::new(PromiseState::Combine {
        items,
        how,
    })))
}

fn number_method(interp: &mut Interp<'_>, method: &str, number: f64, args: &[Value]) -> R<Value> {
    Ok(match method {
        "toFixed" => {
            let digits = to_integer(&arg(args, 0));
            if !(0.0..=100.0).contains(&digits) {
                return Err(interp.throw(
                    "RangeError",
                    "toFixed() digits argument must be between 0 and 100",
                ));
            }
            if !number.is_finite() || number.abs() >= 1e21 {
                Value::str(number_to_string(number))
            } else {
                Value::str(format!("{number:.*}", digits as usize))
            }
        }
        "toPrecision" => {
            if matches!(arg(args, 0), Value::Undefined) || !number.is_finite() {
                return Ok(Value::str(number_to_string(number)));
            }
            let precision = to_integer(&arg(args, 0));
            if !(1.0..=100.0).contains(&precision) {
                return Err(interp.throw(
                    "RangeError",
                    "toPrecision() argument must be between 1 and 100",
                ));
            }
            let precision = precision as usize;
            let exponent = if number == 0.0 {
                0
            } else {
                number.abs().log10().floor() as i64
            };
            if exponent < -6 || exponent >= precision as i64 {
                let text = format!("{number:.*e}", precision - 1);
                let (mantissa, power) = text.split_once('e').unwrap_or((&text, "0"));
                let power: i64 = power.parse().unwrap_or(0);
                Value::str(format!(
                    "{mantissa}e{}{}",
                    if power < 0 { '-' } else { '+' },
                    power.abs()
                ))
            } else {
                let decimals = (precision as i64 - 1 - exponent).max(0) as usize;
                Value::str(format!("{number:.decimals$}"))
            }
        }
        "toString" => {
            let radix = match arg(args, 0) {
                Value::Undefined => 10,
                value => to_integer(&value) as u32,
            };
            if !(2..=36).contains(&radix) {
                return Err(interp.throw("RangeError", "toString() radix must be between 2 and 36"));
            }
            if radix == 10 || !number.is_finite() {
                Value::str(number_to_string(number))
            } else {
                Value::str(integer_in_radix(number, radix))
            }
        }
        "toLocaleString" => Value::str(locale_number(number)),
        _ => Value::Number(number),
    })
}

fn integer_in_radix(number: f64, radix: u32) -> String {
    let negative = number < 0.0;
    let mut whole = number.abs().trunc();
    let mut digits = Vec::new();
    if whole == 0.0 {
        digits.push('0');
    }
    while whole >= 1.0 {
        let digit = (whole % f64::from(radix)) as u32;
        digits.push(char::from_digit(digit, radix).unwrap_or('0'));
        whole = (whole / f64::from(radix)).trunc();
    }
    let mut text: String = digits.into_iter().rev().collect();
    let mut fraction = number.abs().fract();
    if fraction > 0.0 {
        text.push('.');
        for _ in 0..20 {
            fraction *= f64::from(radix);
            let digit = fraction.trunc() as u32;
            text.push(char::from_digit(digit, radix).unwrap_or('0'));
            fraction = fraction.fract();
            if fraction == 0.0 {
                break;
            }
        }
    }
    if negative { format!("-{text}") } else { text }
}

/// `toLocaleString()` as `en-US` prints it: grouped thousands, at most
/// three decimals.
pub fn locale_number(number: f64) -> String {
    if !number.is_finite() {
        return if number.is_nan() {
            "NaN".to_owned()
        } else if number > 0.0 {
            "∞".to_owned()
        } else {
            "-∞".to_owned()
        };
    }
    let fixed = format!("{:.3}", number.abs());
    let (whole, fraction) = fixed.split_once('.').unwrap_or((&fixed, ""));
    let fraction = fraction.trim_end_matches('0');
    let mut grouped = String::new();
    for (index, c) in whole.chars().enumerate() {
        if index > 0 && (whole.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let sign = if number < 0.0 && (whole != "0" || !fraction.is_empty()) {
        "-"
    } else {
        ""
    };
    if fraction.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{fraction}")
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the dispatch table of every built-in; each arm is short"
)]
pub fn call_native(interp: &mut Interp<'_>, name: &str, this: Value, args: Vec<Value>) -> R<Value> {
    if let Some((owner, method)) = name.split_once('#') {
        return match owner {
            "String" => strings::string_method(interp, method, &this, &args),
            "Array" => arrays::array_method(interp, method, &this, &args),
            "Map" => arrays::map_method(interp, method, &this, &args),
            "Set" => arrays::set_method(interp, method, &this, &args),
            "RegExp" => strings::regex_method(interp, method, &this, &args),
            "Date" => dates::date_method(interp, method, &this, &args),
            "Promise" => match method {
                "resolveFn" | "rejectFn" => {
                    if let Value::Promise(promise) = &this {
                        let waiting = matches!(&*promise.state.borrow(), PromiseState::Waiting);
                        if waiting {
                            let value = arg(&args, 0);
                            let state = match (method, value) {
                                ("resolveFn", Value::Promise(inner)) => PromiseState::Then {
                                    base: inner,
                                    on_fulfilled: None,
                                    on_rejected: None,
                                    finally: false,
                                },
                                ("resolveFn", value) => {
                                    PromiseState::Settled(Settled::Fulfilled(value))
                                }
                                (_, value) => PromiseState::Settled(Settled::Rejected(value)),
                            };
                            *promise.state.borrow_mut() = state;
                        }
                    }
                    Ok(Value::Undefined)
                }
                method => Ok(promise_method(method, &this, &args)),
            },
            "Number" => match this {
                Value::Number(number) => number_method(interp, method, number, &args),
                _ => Err(interp
                    .type_error("Number.prototype method called on a value that is not a number")),
            },
            "Boolean" => Ok(match method {
                "toString" => Value::str(to_string(&this)),
                _ => this,
            }),
            "Function" => match method {
                "call" => {
                    let mut args = args.into_iter();
                    let receiver = args.next().unwrap_or(Value::Undefined);
                    interp.call(&this, receiver, args.collect())
                }
                "apply" => {
                    let receiver = arg(&args, 0);
                    let list = match arg(&args, 1) {
                        Value::Undefined | Value::Null => Vec::new(),
                        list => interp.iterate(&list)?,
                    };
                    interp.call(&this, receiver, list)
                }
                "bind" => {
                    let mut bound = vec![this];
                    bound.extend(args);
                    Ok(Value::native("Function#bound", Value::array(bound)))
                }
                "bound" => {
                    let Value::Array(bound) = &this else {
                        return Ok(Value::Undefined);
                    };
                    let bound = bound.items.borrow().clone();
                    let mut bound = bound.into_iter();
                    let function = bound.next().unwrap_or(Value::Undefined);
                    let receiver = bound.next().unwrap_or(Value::Undefined);
                    let mut all: Vec<Value> = bound.collect();
                    all.extend(args);
                    interp.call(&function, receiver, all)
                }
                _ => Ok(Value::str(to_string(&this))),
            },
            "Object" => match method {
                "hasOwnProperty" => {
                    let key = to_key(&arg(&args, 0));
                    Ok(Value::Bool(
                        own_entries(&this).iter().any(|(own, _)| *own == key)
                            || matches!(&this, Value::Array(array) if &*key == "length" || array_index(&key).is_some_and(|i| i < array.items.borrow().len())),
                    ))
                }
                "propertyIsEnumerable" => {
                    let key = to_key(&arg(&args, 0));
                    Ok(Value::Bool(
                        own_entries(&this).iter().any(|(own, _)| *own == key),
                    ))
                }
                "toString" => Ok(Value::str(match &this {
                    Value::Object(object) if object.class == Class::Object => {
                        "[object Object]".to_owned()
                    }
                    Value::Array(_) => "[object Array]".to_owned(),
                    other => to_string(other),
                })),
                "toLocaleString" => Ok(Value::str(match &this {
                    Value::Number(number) => locale_number(*number),
                    other => to_string(other),
                })),
                _ => Ok(this),
            },
            _ => Err(interp.type_error(format!("{name} is not a function"))),
        };
    }
    if let Some((owner, method)) = name.split_once('.') {
        return static_call(interp, owner, method, &args);
    }
    match name {
        "tool" => {
            let tool = to_string(&this);
            let argument = arg(&args, 0);
            let Ok(json) = json_stringify(&argument, "") else {
                return Ok(promise_value(Settled::Rejected(interp.error_value(
                        "TypeError",
                        &format!("the argument of tools.{tool}() cannot be converted to JSON: it holds a cycle, a Promise or a function"),
                    ))));
            };
            Ok(interp.tool_call(Rc::from(tool), json))
        }
        "text" => {
            let text = text_of(&arg(&args, 0));
            interp.push_output(text, false)?;
            Ok(Value::Undefined)
        }
        "exit" => Err(Flow::Exit),
        "store" => {
            let key = arg(&args, 0);
            let Value::Str(key) = key else {
                return Err(interp.type_error("store() key must be a string"));
            };
            let value = arg(&args, 1);
            if matches!(value, Value::Undefined) {
                interp.store_set(&key, None)?;
                return Ok(Value::Undefined);
            }
            match json_stringify(&value, "") {
                Ok(Some(json)) => interp.store_set(&key, Some(json))?,
                _ => {
                    return Err(interp.type_error(format!(
                        "store({}) value is not JSON-serializable",
                        quote(&key)
                    )));
                }
            }
            Ok(Value::Undefined)
        }
        "load" => {
            let Value::Str(key) = arg(&args, 0) else {
                return Err(interp.type_error("load() key must be a string"));
            };
            match interp.store_json(&key).cloned() {
                Some(json) => crate::value::json_parse(&json)
                    .map_err(|error| interp.throw("SyntaxError", error)),
                None => Ok(Value::Undefined),
            }
        }
        "parseInt" => Ok(Value::Number(parse_int(
            &to_string(&arg(&args, 0)),
            &arg(&args, 1),
        ))),
        "parseFloat" => Ok(Value::Number(parse_float(&to_string(&arg(&args, 0))))),
        "isNaN" => Ok(Value::Bool(to_number(&arg(&args, 0)).is_nan())),
        "isFinite" => Ok(Value::Bool(to_number(&arg(&args, 0)).is_finite())),
        "encodeURIComponent" => Ok(uri_encode(&to_string(&arg(&args, 0)), "-_.!~*'()")),
        "encodeURI" => Ok(uri_encode(
            &to_string(&arg(&args, 0)),
            "-_.!~*'();/?:@&=+$,#",
        )),
        "decodeURIComponent" => uri_decode(interp, &to_string(&arg(&args, 0)), ""),
        "decodeURI" => uri_decode(interp, &to_string(&arg(&args, 0)), ";/?:@&=+$,#"),
        "structuredClone" => deep_clone(interp, &arg(&args, 0), 0),
        "Number" => Ok(Value::Number(if args.is_empty() {
            0.0
        } else {
            to_number(&args[0])
        })),
        "String" => Ok(Value::str(match args.first() {
            None => String::new(),
            Some(value) => to_string(value),
        })),
        "Boolean" => Ok(Value::Bool(arg(&args, 0).truthy())),
        "Date" => Ok(Value::str(dates::date_string(dates::now()))),
        "Symbol" | "Function" => {
            Err(interp.type_error(format!("{name} is not available in codemode scripts")))
        }
        name => construct_native(interp, name, args),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the static functions of every built-in namespace"
)]
fn static_call(interp: &mut Interp<'_>, owner: &str, method: &str, args: &[Value]) -> R<Value> {
    let number = |index: usize| to_number(&arg(args, index));
    let math = |f: fn(f64) -> f64| Ok(Value::Number(f(to_number(&arg(args, 0)))));
    match (owner, method) {
        ("Math", "abs") => math(f64::abs),
        ("Math", "acos") => math(f64::acos),
        ("Math", "asin") => math(f64::asin),
        ("Math", "atan") => math(f64::atan),
        ("Math", "cbrt") => math(f64::cbrt),
        ("Math", "ceil") => math(f64::ceil),
        ("Math", "cos") => math(f64::cos),
        ("Math", "exp") => math(f64::exp),
        ("Math", "floor") => math(f64::floor),
        ("Math", "log") => math(f64::ln),
        ("Math", "log10") => math(f64::log10),
        ("Math", "log1p") => math(f64::ln_1p),
        ("Math", "log2") => math(f64::log2),
        ("Math", "sin") => math(f64::sin),
        ("Math", "sqrt") => math(f64::sqrt),
        ("Math", "tan") => math(f64::tan),
        ("Math", "trunc") => math(f64::trunc),
        ("Math", "sign") => math(|x| {
            if x.is_nan() || x == 0.0 {
                x
            } else {
                x.signum()
            }
        }),
        // JavaScript rounds halves up: Math.round(-2.5) is -2.
        ("Math", "round") => math(|x| if x.is_finite() { (x + 0.5).floor() } else { x }),
        ("Math", "atan2") => Ok(Value::Number(number(0).atan2(number(1)))),
        ("Math", "pow") => interp.binary(crate::ast::BinaryOp::Pow, &arg(args, 0), &arg(args, 1)),
        ("Math", "hypot") => Ok(Value::Number(
            args.iter()
                .map(|value| to_number(value).powi(2))
                .sum::<f64>()
                .sqrt(),
        )),
        ("Math", "max" | "min") => {
            let mut result = if method == "max" {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            };
            for value in args {
                let value = to_number(value);
                if value.is_nan() {
                    return Ok(Value::Number(f64::NAN));
                }
                result = if method == "max" {
                    result.max(value)
                } else {
                    result.min(value)
                };
            }
            Ok(Value::Number(result))
        }
        ("Math", "random") => Ok(Value::Number(interp.next_random())),
        ("JSON", "parse") => {
            let text = to_string(&arg(args, 0));
            interp.alloc(text.len() * 2)?;
            let value = crate::value::json_parse(&text)
                .map_err(|error| interp.throw("SyntaxError", error))?;
            let reviver = arg(args, 1);
            if matches!(reviver, Value::Function(_)) {
                return revive(interp, &reviver, Value::str(""), value);
            }
            Ok(value)
        }
        ("JSON", "stringify") => {
            let indent = match arg(args, 2) {
                Value::Number(count) => " ".repeat(count.clamp(0.0, 10.0) as usize),
                Value::Str(text) => text.chars().take(10).collect(),
                _ => String::new(),
            };
            let mut value = arg(args, 0);
            match arg(args, 1) {
                replacer @ Value::Function(_) => {
                    value = replace_json(interp, &replacer, Value::str(""), value, 0)?;
                }
                Value::Array(keys) => {
                    let keys: Vec<Rc<str>> = keys.items.borrow().iter().map(to_key).collect();
                    value = pick_keys(&value, &keys);
                }
                _ => {}
            }
            match json_stringify(&value, &indent) {
                Ok(Some(json)) => {
                    interp.alloc(json.len())?;
                    Ok(Value::str(json))
                }
                Ok(None) => Ok(Value::Undefined),
                Err(crate::value::JsonError::Cycle) => {
                    Err(interp.type_error("Converting circular structure to JSON"))
                }
                Err(crate::value::JsonError::Unsupported(what)) => {
                    Err(interp.type_error(format!("JSON.stringify cannot convert {what}")))
                }
            }
        }
        ("console", _) => {
            let line = args.iter().map(format_value).collect::<Vec<_>>().join(" ");
            interp.push_output(line, true)?;
            Ok(Value::Undefined)
        }
        ("Object", "keys") => Ok(Value::array(
            own_entries(&arg(args, 0))
                .into_iter()
                .map(|(key, _)| Value::Str(key))
                .collect(),
        )),
        ("Object", "getOwnPropertyNames") => Ok(Value::array(
            own_entries(&arg(args, 0))
                .into_iter()
                .map(|(key, _)| Value::Str(key))
                .collect(),
        )),
        ("Object", "values") => Ok(Value::array(
            own_entries(&arg(args, 0))
                .into_iter()
                .map(|(_, value)| value)
                .collect(),
        )),
        ("Object", "entries") => Ok(Value::array(
            own_entries(&arg(args, 0))
                .into_iter()
                .map(|(key, value)| Value::array(vec![Value::Str(key), value]))
                .collect(),
        )),
        ("Object", "assign") => {
            let target = arg(args, 0);
            for source in args.iter().skip(1) {
                for (key, value) in own_entries(source) {
                    set_property(interp, &target, &Value::Str(key), value)?;
                }
            }
            Ok(target)
        }
        ("Object", "fromEntries") => object_from_entries(interp, &arg(args, 0)),
        ("Object", "freeze" | "seal") => {
            let target = arg(args, 0);
            freeze(&target);
            Ok(target)
        }
        ("Object", "isFrozen") => Ok(Value::Bool(match arg(args, 0) {
            Value::Object(object) => object.frozen.get(),
            Value::Array(array) => array.frozen.get(),
            _ => true,
        })),
        ("Object", "create") => {
            let mut props = IndexMap::new();
            if let Value::Object(proto) = arg(args, 0) {
                for (key, value) in proto.props.borrow().iter() {
                    props.insert(key.clone(), value.clone());
                }
            }
            Ok(Value::object(props))
        }
        ("Object", "hasOwn") => {
            let key = to_key(&arg(args, 1));
            Ok(Value::Bool(
                own_entries(&arg(args, 0))
                    .iter()
                    .any(|(own, _)| *own == key),
            ))
        }
        ("Object", "defineProperty") => {
            let target = arg(args, 0);
            let descriptor = arg(args, 2);
            let value = descriptor.own("value").unwrap_or(Value::Undefined);
            set_property(interp, &target, &arg(args, 1), value)?;
            Ok(target)
        }
        ("Object", "getPrototypeOf") => Ok(Value::Null),
        ("Object" | "Map", "groupBy") => {
            let items = interp.iterate(&arg(args, 0))?;
            let key_of = arg(args, 1);
            require_function(interp, &key_of, "groupBy key")?;
            if owner == "Map" {
                let mut groups: IndexMap<crate::value::MapKey, (Value, Value)> = IndexMap::new();
                for (index, item) in items.into_iter().enumerate() {
                    let key = interp.call(
                        &key_of,
                        Value::Undefined,
                        vec![item.clone(), Value::Number(index as f64)],
                    )?;
                    let entry = groups
                        .entry(key.map_key())
                        .or_insert_with(|| (key, Value::array(Vec::new())));
                    if let Value::Array(list) = &entry.1 {
                        list.items.borrow_mut().push(item);
                    }
                }
                return Ok(Value::Map(Rc::new(std::cell::RefCell::new(groups))));
            }
            let mut groups: IndexMap<Rc<str>, Value> = IndexMap::new();
            for (index, item) in items.into_iter().enumerate() {
                let key = interp.call(
                    &key_of,
                    Value::Undefined,
                    vec![item.clone(), Value::Number(index as f64)],
                )?;
                let entry = groups
                    .entry(to_key(&key))
                    .or_insert_with(|| Value::array(Vec::new()));
                if let Value::Array(list) = entry {
                    list.items.borrow_mut().push(item);
                }
            }
            Ok(Value::object(groups))
        }
        ("Array", "isArray") => Ok(Value::Bool(matches!(arg(args, 0), Value::Array(_)))),
        ("Array", "of") => Ok(Value::array(args.to_vec())),
        ("Array", "from") => {
            let source = arg(args, 0);
            let items = match &source {
                Value::Object(_) => {
                    let length = to_integer(&get_property(interp, &source, &Value::str("length"))?);
                    if !(0.0..=crate::interp::MAX_ARRAY_LENGTH as f64).contains(&length) {
                        return Err(interp.throw("RangeError", "Invalid array length"));
                    }
                    let mut items = Vec::with_capacity(length as usize);
                    for index in 0..length as usize {
                        items.push(get_property(interp, &source, &Value::Number(index as f64))?);
                    }
                    interp.alloc(items.len() * 16)?;
                    items
                }
                Value::Undefined | Value::Null => {
                    return Err(
                        interp.type_error(format!("{} is not iterable", to_string(&source)))
                    );
                }
                other => interp.iterate(other)?,
            };
            let map = arg(args, 1);
            if matches!(map, Value::Undefined) {
                return Ok(Value::array(items));
            }
            require_function(interp, &map, "Array.from map")?;
            let mut mapped = Vec::with_capacity(items.len());
            for (index, item) in items.into_iter().enumerate() {
                mapped.push(interp.call(
                    &map,
                    Value::Undefined,
                    vec![item, Value::Number(index as f64)],
                )?);
            }
            Ok(Value::array(mapped))
        }
        ("Number", "isInteger") => Ok(Value::Bool(
            matches!(arg(args, 0), Value::Number(n) if n.is_finite() && n.fract() == 0.0),
        )),
        ("Number", "isSafeInteger") => Ok(Value::Bool(
            matches!(arg(args, 0), Value::Number(n) if n.is_finite() && n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_991.0),
        )),
        ("Number", "isFinite") => Ok(Value::Bool(
            matches!(arg(args, 0), Value::Number(n) if n.is_finite()),
        )),
        ("Number", "isNaN") => Ok(Value::Bool(
            matches!(arg(args, 0), Value::Number(n) if n.is_nan()),
        )),
        ("Number", "parseFloat") => Ok(Value::Number(parse_float(&to_string(&arg(args, 0))))),
        ("Number", "parseInt") => Ok(Value::Number(parse_int(
            &to_string(&arg(args, 0)),
            &arg(args, 1),
        ))),
        ("String", "fromCharCode" | "fromCodePoint") => {
            let mut text = String::new();
            for value in args {
                let code = to_number(value);
                let code = if code.is_finite() {
                    code as u32
                } else {
                    0xfffd
                };
                text.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            }
            Ok(Value::str(text))
        }
        ("Promise", "all") => combine(interp, Combine::All, &arg(args, 0)),
        ("Promise", "allSettled") => combine(interp, Combine::AllSettled, &arg(args, 0)),
        ("Promise", "race") => combine(interp, Combine::Race, &arg(args, 0)),
        ("Promise", "any") => combine(interp, Combine::Any, &arg(args, 0)),
        ("Promise", "resolve") => Ok(match arg(args, 0) {
            promise @ Value::Promise(_) => promise,
            value => promise_value(Settled::Fulfilled(value)),
        }),
        ("Promise", "reject") => Ok(promise_value(Settled::Rejected(arg(args, 0)))),
        ("Date", "now") => Ok(Value::Number(dates::now())),
        ("Date", "parse") => Ok(Value::Number(dates::parse(&to_string(&arg(args, 0))))),
        ("Date", "UTC") => Ok(Value::Number(dates::from_parts(args))),
        _ => Err(interp.type_error(format!("{owner}.{method} is not a function"))),
    }
}

fn revive(interp: &mut Interp<'_>, reviver: &Value, key: Value, value: Value) -> R<Value> {
    match &value {
        Value::Array(array) => {
            let items = array.items.borrow().clone();
            for (index, item) in items.into_iter().enumerate() {
                let item_value = revive(interp, reviver, Value::str(index.to_string()), item)?;
                array.items.borrow_mut()[index] = item_value;
            }
        }
        Value::Object(object) => {
            let props: Vec<(Rc<str>, Value)> = object
                .props
                .borrow()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (prop, item) in props {
                let item_value = revive(interp, reviver, Value::Str(prop.clone()), item)?;
                if matches!(item_value, Value::Undefined) {
                    object.props.borrow_mut().shift_remove(&prop);
                } else {
                    object.props.borrow_mut().insert(prop, item_value);
                }
            }
        }
        _ => {}
    }
    interp.call(reviver, Value::Undefined, vec![key, value])
}

fn replace_json(
    interp: &mut Interp<'_>,
    replacer: &Value,
    key: Value,
    value: Value,
    depth: usize,
) -> R<Value> {
    if depth > 1000 {
        return Err(interp.type_error("Converting circular structure to JSON"));
    }
    let value = interp.call(replacer, Value::Undefined, vec![key, value])?;
    Ok(match &value {
        Value::Array(array) => {
            let items = array.items.borrow().clone();
            let mut copied = Vec::with_capacity(items.len());
            for (index, item) in items.into_iter().enumerate() {
                copied.push(replace_json(
                    interp,
                    replacer,
                    Value::str(index.to_string()),
                    item,
                    depth + 1,
                )?);
            }
            Value::array(copied)
        }
        Value::Object(object) => {
            let props: Vec<(Rc<str>, Value)> = object
                .props
                .borrow()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let mut copied = IndexMap::new();
            for (prop, item) in props {
                let item =
                    replace_json(interp, replacer, Value::Str(prop.clone()), item, depth + 1)?;
                copied.insert(prop, item);
            }
            Value::object(copied)
        }
        _ => value,
    })
}

fn pick_keys(value: &Value, keys: &[Rc<str>]) -> Value {
    match value {
        Value::Object(object) => {
            let props = object.props.borrow();
            let mut picked = IndexMap::new();
            for key in keys {
                if let Some(item) = props.get(&**key) {
                    picked.insert(key.clone(), pick_keys(item, keys));
                }
            }
            Value::object(picked)
        }
        Value::Array(array) => Value::array(
            array
                .items
                .borrow()
                .iter()
                .map(|item| pick_keys(item, keys))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// `new Name(...args)` of a built-in, and the constructors called without
/// `new` that act the same.
pub fn construct_native(interp: &mut Interp<'_>, name: &str, args: Vec<Value>) -> R<Value> {
    match name {
        name if ERRORS.contains(&name) => {
            let (message, options) = if name == "AggregateError" {
                (arg(&args, 1), arg(&args, 2))
            } else {
                (arg(&args, 0), arg(&args, 1))
            };
            let message = match message {
                Value::Undefined => String::new(),
                value => to_string(&value),
            };
            let error = interp.error_value(name, &message);
            if let Value::Object(object) = &error {
                if let Some(cause) = options.own("cause") {
                    object.props.borrow_mut().insert(Rc::from("cause"), cause);
                }
                if name == "AggregateError" {
                    let errors = interp.iterate(&arg(&args, 0))?;
                    object
                        .props
                        .borrow_mut()
                        .insert(Rc::from("errors"), Value::array(errors));
                }
            }
            Ok(error)
        }
        "Array" => {
            if let [Value::Number(length)] = args.as_slice() {
                if !(*length >= 0.0 && length.fract() == 0.0)
                    || *length as usize > crate::interp::MAX_ARRAY_LENGTH
                {
                    return Err(interp.throw("RangeError", "Invalid array length"));
                }
                interp.alloc(*length as usize * 16)?;
                return Ok(Value::array(vec![Value::Undefined; *length as usize]));
            }
            Ok(Value::array(args))
        }
        "Object" => Ok(match arg(&args, 0) {
            Value::Undefined | Value::Null => Value::object(IndexMap::new()),
            value => value,
        }),
        "Map" => arrays::new_map(interp, &arg(&args, 0)),
        "Set" => arrays::new_set(interp, &arg(&args, 0)),
        "Date" => Ok(Value::Date(dates::construct(&args))),
        "RegExp" => {
            let (pattern, flags) = match (arg(&args, 0), arg(&args, 1)) {
                (Value::Regex(regex), Value::Undefined) => {
                    (regex.source.to_string(), regex.flags.to_string())
                }
                (Value::Regex(regex), flags) => (regex.source.to_string(), to_string(&flags)),
                (pattern, Value::Undefined) => (to_string(&pattern), String::new()),
                (pattern, flags) => (to_string(&pattern), to_string(&flags)),
            };
            strings::make_regex(interp, &pattern, &flags)
        }
        "Promise" => {
            let executor = arg(&args, 0);
            require_function(interp, &executor, "Promise resolver")?;
            let promise = Promise::new(PromiseState::Waiting);
            let resolve = Value::native("Promise#resolveFn", Value::Promise(Rc::clone(&promise)));
            let reject = Value::native("Promise#rejectFn", Value::Promise(Rc::clone(&promise)));
            match interp.call(&executor, Value::Undefined, vec![resolve, reject.clone()]) {
                Ok(_) => {}
                Err(Flow::Throw(error)) => {
                    interp.call(&reject, Value::Undefined, vec![error])?;
                }
                Err(flow) => return Err(flow),
            }
            Ok(Value::Promise(promise))
        }
        "Number" | "String" | "Boolean" => call_native(interp, name, Value::Undefined, args),
        name => Err(interp.type_error(format!("{name} is not a constructor"))),
    }
}
