//! `String.prototype` and `RegExp`.
//!
//! Positions are counted in characters (Unicode scalar values), not UTF-16
//! code units: the same for every character outside the astral planes, and
//! never splits one. Regular expressions run on `fancy-regex`, which has
//! the look-around and back-references JavaScript patterns use.

use std::cell::Cell;
use std::fmt::Write as _;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::builtins::{arg, relative_index, to_integer};
use crate::interp::{Interp, R};
use crate::value::{RegexObj, Value, to_number, to_string};

pub const METHODS: &[&str] = &[
    "at",
    "charAt",
    "charCodeAt",
    "codePointAt",
    "concat",
    "endsWith",
    "includes",
    "indexOf",
    "lastIndexOf",
    "localeCompare",
    "match",
    "matchAll",
    "normalize",
    "padEnd",
    "padStart",
    "repeat",
    "replace",
    "replaceAll",
    "search",
    "slice",
    "split",
    "startsWith",
    "substring",
    "substr",
    "toLowerCase",
    "toUpperCase",
    "toLocaleLowerCase",
    "toLocaleUpperCase",
    "trim",
    "trimStart",
    "trimEnd",
    "trimLeft",
    "trimRight",
    "toString",
    "valueOf",
];

/// The longest string a script may build.
const MAX_STRING_CHARS: usize = 256 * 1024 * 1024;

pub fn length(text: &str) -> usize {
    if text.is_ascii() {
        text.len()
    } else {
        text.chars().count()
    }
}

/// The byte offset of character `index` (the length at the end).
fn byte_of(text: &str, index: usize) -> usize {
    if text.is_ascii() {
        return index.min(text.len());
    }
    text.char_indices()
        .nth(index)
        .map_or(text.len(), |(byte, _)| byte)
}

/// The character index of byte offset `byte`.
fn char_of(text: &str, byte: usize) -> usize {
    if text.is_ascii() {
        byte
    } else {
        text[..byte.min(text.len())].chars().count()
    }
}

fn slice(text: &str, start: usize, end: usize) -> &str {
    if start >= end {
        return "";
    }
    &text[byte_of(text, start)..byte_of(text, end)]
}

pub fn char_at(text: &str, index: usize) -> Option<String> {
    if text.is_ascii() {
        return text.get(index..=index).map(str::to_owned);
    }
    text.chars().nth(index).map(String::from)
}

fn index_of(text: &str, needle: &str, from: usize) -> Option<usize> {
    let start = byte_of(text, from);
    text[start..]
        .find(needle)
        .map(|byte| char_of(text, start + byte))
}

fn is_js_whitespace(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

fn text_of(interp: &Interp<'_>, this: &Value, method: &str) -> R<Rc<str>> {
    match this {
        Value::Str(text) => Ok(text.clone()),
        Value::Undefined | Value::Null => Err(interp.type_error(format!(
            "String.prototype.{method} called on null or undefined"
        ))),
        other => Ok(Rc::from(to_string(other))),
    }
}

fn pad(interp: &mut Interp<'_>, text: &str, args: &[Value], start: bool) -> R<Value> {
    let target = to_integer(&arg(args, 0)).max(0.0) as usize;
    let filler = match arg(args, 1) {
        Value::Undefined => " ".to_owned(),
        value => to_string(&value),
    };
    let current = length(text);
    if target <= current || filler.is_empty() {
        return Ok(Value::str(text));
    }
    if target > MAX_STRING_CHARS {
        return Err(interp.throw("RangeError", "Invalid string length"));
    }
    interp.alloc(target)?;
    let padding: String = filler.chars().cycle().take(target - current).collect();
    Ok(Value::str(if start {
        format!("{padding}{text}")
    } else {
        format!("{text}{padding}")
    }))
}

#[allow(clippy::too_many_lines, reason = "one arm per String.prototype method")]
pub fn string_method(
    interp: &mut Interp<'_>,
    method: &str,
    this: &Value,
    args: &[Value],
) -> R<Value> {
    let text = text_of(interp, this, method)?;
    let len = length(&text);
    Ok(match method {
        "at" => {
            let index = to_integer(&arg(args, 0));
            let index = if index < 0.0 {
                len as f64 + index
            } else {
                index
            };
            if index < 0.0 || index >= len as f64 {
                Value::Undefined
            } else {
                char_at(&text, index as usize).map_or(Value::Undefined, Value::str)
            }
        }
        "charAt" => {
            let index = to_integer(&arg(args, 0));
            if index < 0.0 {
                Value::str("")
            } else {
                Value::str(char_at(&text, index as usize).unwrap_or_default())
            }
        }
        "charCodeAt" | "codePointAt" => {
            let index = to_integer(&arg(args, 0));
            let found = (index >= 0.0)
                .then(|| text.chars().nth(index as usize))
                .flatten();
            match found {
                Some(c) => Value::Number(f64::from(u32::from(c))),
                None if method == "charCodeAt" => Value::Number(f64::NAN),
                None => Value::Undefined,
            }
        }
        "concat" => {
            let mut joined = text.to_string();
            for value in args {
                joined.push_str(&to_string(value));
            }
            interp.alloc(joined.len())?;
            Value::str(joined)
        }
        "endsWith" => {
            let needle = to_string(&arg(args, 0));
            let end = relative_index(&arg(args, 1), len, len);
            Value::Bool(slice(&text, 0, end).ends_with(&needle))
        }
        "startsWith" => {
            let needle = to_string(&arg(args, 0));
            let start = relative_index(&arg(args, 1), len, 0);
            Value::Bool(slice(&text, start, len).starts_with(&needle))
        }
        "includes" => {
            if matches!(arg(args, 0), Value::Regex(_)) {
                return Err(interp.type_error(
                    "First argument to String.prototype.includes must not be a regular expression",
                ));
            }
            let needle = to_string(&arg(args, 0));
            let start = relative_index(&arg(args, 1), len, 0);
            Value::Bool(index_of(&text, &needle, start).is_some())
        }
        "indexOf" => {
            let needle = to_string(&arg(args, 0));
            let start = relative_index(&arg(args, 1), len, 0);
            Value::Number(index_of(&text, &needle, start).map_or(-1.0, |index| index as f64))
        }
        "lastIndexOf" => {
            let needle = to_string(&arg(args, 0));
            let limit = match arg(args, 1) {
                Value::Undefined => len,
                value => relative_index(&value, len, len),
            };
            let end = byte_of(&text, (limit + length(&needle)).min(len));
            Value::Number(
                text[..end]
                    .rfind(&needle)
                    .map_or(-1.0, |byte| char_of(&text, byte) as f64),
            )
        }
        "localeCompare" => {
            let other = to_string(&arg(args, 0));
            let ordering = text
                .to_lowercase()
                .cmp(&other.to_lowercase())
                .then_with(|| text.as_ref().cmp(other.as_str()));
            Value::Number(match ordering {
                std::cmp::Ordering::Less => -1.0,
                std::cmp::Ordering::Equal => 0.0,
                std::cmp::Ordering::Greater => 1.0,
            })
        }
        "normalize" | "toString" | "valueOf" => Value::Str(text),
        "padStart" => pad(interp, &text, args, true)?,
        "padEnd" => pad(interp, &text, args, false)?,
        "repeat" => {
            let count = to_integer(&arg(args, 0));
            if count < 0.0 || count.is_infinite() {
                return Err(interp.throw(
                    "RangeError",
                    format!("Invalid count value: {}", to_string(&arg(args, 0))),
                ));
            }
            let total = (count as usize).saturating_mul(len);
            if total > MAX_STRING_CHARS {
                return Err(interp.throw("RangeError", "Invalid string length"));
            }
            interp.alloc(total)?;
            Value::str(text.repeat(count as usize))
        }
        "slice" => {
            let start = relative_index(&arg(args, 0), len, 0);
            let end = relative_index(&arg(args, 1), len, len);
            Value::str(slice(&text, start, end))
        }
        "substring" => {
            let clamp = |value: &Value, default: usize| match value {
                Value::Undefined => default,
                value => to_integer(value).clamp(0.0, len as f64) as usize,
            };
            let start = clamp(&arg(args, 0), 0);
            let end = clamp(&arg(args, 1), len);
            Value::str(slice(&text, start.min(end), start.max(end)))
        }
        "substr" => {
            let start = relative_index(&arg(args, 0), len, 0);
            let count = match arg(args, 1) {
                Value::Undefined => len,
                value => to_integer(&value).max(0.0) as usize,
            };
            Value::str(slice(&text, start, start.saturating_add(count).min(len)))
        }
        "toLowerCase" | "toLocaleLowerCase" => Value::str(text.to_lowercase()),
        "toUpperCase" | "toLocaleUpperCase" => Value::str(text.to_uppercase()),
        "trim" => Value::str(text.trim_matches(is_js_whitespace)),
        "trimStart" | "trimLeft" => Value::str(text.trim_start_matches(is_js_whitespace)),
        "trimEnd" | "trimRight" => Value::str(text.trim_end_matches(is_js_whitespace)),
        "split" => split(interp, &text, args)?,
        "replace" | "replaceAll" => replace(interp, &text, args, method == "replaceAll")?,
        "match" => {
            let regex = regex_argument(interp, &arg(args, 0), "")?;
            let Value::Regex(regex) = &regex else {
                return Ok(Value::Null);
            };
            if regex.flags.contains('g') {
                let mut found = Vec::new();
                for found_match in regex.regex.find_iter(&text) {
                    let found_match = found_match.map_err(|error| regex_failure(interp, &error))?;
                    found.push(Value::str(found_match.as_str()));
                }
                regex.last_index.set(0);
                if found.is_empty() {
                    Value::Null
                } else {
                    Value::array(found)
                }
            } else {
                exec(interp, regex, &text, 0)?.map_or(Value::Null, |found| found.0)
            }
        }
        "matchAll" => {
            let regex = regex_argument(interp, &arg(args, 0), "g")?;
            let Value::Regex(regex) = &regex else {
                return Ok(Value::array(Vec::new()));
            };
            if !regex.flags.contains('g') {
                return Err(interp.type_error(
                    "String.prototype.matchAll called with a non-global RegExp argument",
                ));
            }
            let mut found = Vec::new();
            let mut position = 0;
            while position <= len {
                let Some((found_match, start, end)) = exec(interp, regex, &text, position)? else {
                    break;
                };
                found.push(found_match);
                position = if end == start { end + 1 } else { end };
                interp.tick()?;
            }
            Value::array(found)
        }
        "search" => {
            let regex = regex_argument(interp, &arg(args, 0), "")?;
            let Value::Regex(regex) = &regex else {
                return Ok(Value::Number(-1.0));
            };
            match regex
                .regex
                .find(&text)
                .map_err(|error| regex_failure(interp, &error))?
            {
                Some(found) => Value::Number(char_of(&text, found.start()) as f64),
                None => Value::Number(-1.0),
            }
        }
        _ => Value::Undefined,
    })
}

fn regex_failure(interp: &Interp<'_>, error: &fancy_regex::Error) -> crate::interp::Flow {
    interp.throw("RangeError", format!("regular expression failed: {error}"))
}

/// A pattern argument as a regular expression; a string is compiled as a
/// pattern, as `match` and `search` read one.
fn regex_argument(interp: &mut Interp<'_>, value: &Value, flags: &str) -> R<Value> {
    match value {
        Value::Regex(_) => Ok(value.clone()),
        Value::Undefined => make_regex(interp, "(?:)", flags),
        other => make_regex(interp, &to_string(other), flags),
    }
}

/// A JavaScript pattern as `fancy-regex` reads it.
fn translate(pattern: &str, flags: &str) -> String {
    let mut out = String::new();
    let mut inline = String::new();
    for (flag, inline_flag) in [('i', 'i'), ('m', 'm'), ('s', 's')] {
        if flags.contains(flag) {
            inline.push(inline_flag);
        }
    }
    if !inline.is_empty() {
        let _ = write!(out, "(?{inline})");
    }
    let chars: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    let mut in_class = false;
    while index < chars.len() {
        let c = chars[index];
        match c {
            '\\' if index + 1 < chars.len() => {
                let next = chars[index + 1];
                match next {
                    // JavaScript's `\d` and `\w` are ASCII.
                    'd' if !in_class => out.push_str("[0-9]"),
                    'D' if !in_class => out.push_str("[^0-9]"),
                    'd' => out.push_str("0-9"),
                    'w' if !in_class => out.push_str("[A-Za-z0-9_]"),
                    'W' if !in_class => out.push_str("[^A-Za-z0-9_]"),
                    'w' => out.push_str("A-Za-z0-9_"),
                    // `\/` and other escaped punctuation: the character.
                    c if !c.is_ascii_alphanumeric() && !"\\.^$|?*+()[]{}-#&~".contains(c) => {
                        out.push(c);
                    }
                    c if c.is_ascii_punctuation() => {
                        out.push('\\');
                        out.push(c);
                    }
                    'u' if index + 2 < chars.len() && chars[index + 2] == '{' => {
                        out.push_str("\\x");
                    }
                    c => {
                        out.push('\\');
                        out.push(c);
                    }
                }
                index += 2;
                continue;
            }
            '[' if !in_class => {
                // `[^]` matches anything, `[]` nothing.
                if chars.get(index + 1) == Some(&'^') && chars.get(index + 2) == Some(&']') {
                    out.push_str("[\\s\\S]");
                    index += 3;
                    continue;
                }
                if chars.get(index + 1) == Some(&']') {
                    out.push_str("[^\\s\\S]");
                    index += 2;
                    continue;
                }
                in_class = true;
                out.push('[');
                if chars.get(index + 1) == Some(&'^') {
                    out.push('^');
                    index += 1;
                }
                // A `]` or `[` right after the opening is a literal in
                // JavaScript only when escaped; nested `[` is literal.
            }
            '[' => out.push_str("\\["),
            ']' if in_class => {
                in_class = false;
                out.push(']');
            }
            '&' | '~' if in_class => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
        index += 1;
    }
    out
}

pub fn make_regex(interp: &mut Interp<'_>, pattern: &str, flags: &str) -> R<Value> {
    for flag in flags.chars() {
        if !"dgimsuvy".contains(flag) || flags.matches(flag).count() > 1 {
            return Err(interp.throw(
                "SyntaxError",
                format!("Invalid flags supplied to RegExp constructor '{flags}'"),
            ));
        }
    }
    let translated = translate(pattern, flags);
    let regex = fancy_regex::RegexBuilder::new(&translated)
        .backtrack_limit(1_000_000)
        .build()
        .map_err(|error| {
            interp.throw(
                "SyntaxError",
                format!("Invalid regular expression: /{pattern}/{flags}: {error}"),
            )
        })?;
    Ok(Value::Regex(Rc::new(RegexObj {
        source: Rc::from(if pattern.is_empty() { "(?:)" } else { pattern }),
        flags: Rc::from(flags),
        regex,
        last_index: Cell::new(0),
    })))
}

/// `regex.exec(text)` from character `from` (or `lastIndex` for a global or
/// sticky one): the match array with `index`, `input` and `groups`, and
/// where the match starts and ends, in characters.
fn exec(
    interp: &mut Interp<'_>,
    regex: &RegexObj,
    text: &str,
    from: usize,
) -> R<Option<(Value, usize, usize)>> {
    let start = byte_of(text, from);
    if start > text.len() {
        return Ok(None);
    }
    let captures = regex
        .regex
        .captures_from_pos(text, start)
        .map_err(|error| regex_failure(interp, &error))?;
    let Some(captures) = captures else {
        return Ok(None);
    };
    let whole = captures
        .get(0)
        .map_or((start, start), |m| (m.start(), m.end()));
    if regex.flags.contains('y') && whole.0 != start {
        return Ok(None);
    }
    let mut items = Vec::with_capacity(captures.len());
    for group in 0..captures.len() {
        items.push(
            captures
                .get(group)
                .map_or(Value::Undefined, |m| Value::str(m.as_str())),
        );
    }
    let mut groups = IndexMap::new();
    for (index, name) in regex.regex.capture_names().enumerate() {
        if let Some(name) = name {
            groups.insert(
                Rc::from(name),
                captures
                    .get(index)
                    .map_or(Value::Undefined, |m| Value::str(m.as_str())),
            );
        }
    }
    let (start, end) = (char_of(text, whole.0), char_of(text, whole.1));
    let array = Value::array(items);
    if let Value::Array(inner) = &array {
        let mut extra = inner.extra.borrow_mut();
        extra.insert(Rc::from("index"), Value::Number(start as f64));
        extra.insert(Rc::from("input"), Value::str(text));
        extra.insert(
            Rc::from("groups"),
            if groups.is_empty() {
                Value::Undefined
            } else {
                Value::object(groups)
            },
        );
    }
    interp.alloc(text.len().min(4096))?;
    Ok(Some((array, start, end)))
}

pub fn regex_method(
    interp: &mut Interp<'_>,
    method: &str,
    this: &Value,
    args: &[Value],
) -> R<Value> {
    let Value::Regex(regex) = this else {
        return Err(interp.type_error(format!(
            "RegExp.prototype.{method} called on a value that is not a RegExp"
        )));
    };
    match method {
        "toString" => Ok(Value::str(to_string(this))),
        "test" | "exec" => {
            let text = to_string(&arg(args, 0));
            let advancing = regex.flags.contains('g') || regex.flags.contains('y');
            let from = if advancing { regex.last_index.get() } else { 0 };
            if from > length(&text) {
                regex.last_index.set(0);
                return Ok(if method == "test" {
                    Value::Bool(false)
                } else {
                    Value::Null
                });
            }
            let found = exec(interp, regex, &text, from)?;
            if let Some((found, _, end)) = found {
                if advancing {
                    regex.last_index.set(end);
                }
                Ok(if method == "test" {
                    Value::Bool(true)
                } else {
                    found
                })
            } else {
                if advancing {
                    regex.last_index.set(0);
                }
                Ok(if method == "test" {
                    Value::Bool(false)
                } else {
                    Value::Null
                })
            }
        }
        _ => Ok(Value::Undefined),
    }
}

/// Every match of `regex` in `text`, as (start, end) character positions
/// and the match arrays.
fn all_matches(
    interp: &mut Interp<'_>,
    regex: &RegexObj,
    text: &str,
    global: bool,
) -> R<Vec<(Value, usize, usize)>> {
    let mut found = Vec::new();
    let mut position = 0;
    let len = length(text);
    while position <= len {
        let Some((item, start, end)) = exec(interp, regex, text, position)? else {
            break;
        };
        found.push((item, start, end));
        if !global {
            break;
        }
        position = if end == start { end + 1 } else { end };
        interp.tick()?;
    }
    Ok(found)
}

/// `$&`, `$1`, `$<name>`, `` $` ``, `$'` and `$$` in a replacement.
fn expand(
    template: &str,
    text: &str,
    found: &[Value],
    start: usize,
    end: usize,
    groups: &Value,
) -> String {
    let mut out = String::new();
    let chars: Vec<char> = template.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '$' || index + 1 >= chars.len() {
            out.push(chars[index]);
            index += 1;
            continue;
        }
        let next = chars[index + 1];
        match next {
            '$' => {
                out.push('$');
                index += 2;
            }
            '&' => {
                out.push_str(&found.first().map(to_string).unwrap_or_default());
                index += 2;
            }
            '`' => {
                out.push_str(slice(text, 0, start));
                index += 2;
            }
            '\'' => {
                out.push_str(slice(text, end, length(text)));
                index += 2;
            }
            '<' => {
                let close = chars[index..].iter().position(|c| *c == '>');
                if let (Some(close), Value::Object(_)) = (close, groups) {
                    let name: String = chars[index + 2..index + close].iter().collect();
                    if let Some(value) = groups.own(&name)
                        && !matches!(value, Value::Undefined)
                    {
                        out.push_str(&to_string(&value));
                    }
                    index += close + 1;
                } else {
                    out.push('$');
                    index += 1;
                }
            }
            c if c.is_ascii_digit() => {
                let two: Option<usize> = chars
                    .get(index + 2)
                    .filter(|d| d.is_ascii_digit())
                    .and_then(|d| format!("{c}{d}").parse().ok())
                    .filter(|n: &usize| *n > 0 && *n < found.len());
                let (group, width) = match two {
                    Some(group) => (group, 3),
                    None => (c.to_digit(10).unwrap_or(0) as usize, 2),
                };
                if group > 0 && group < found.len() {
                    if !matches!(found[group], Value::Undefined) {
                        out.push_str(&to_string(&found[group]));
                    }
                    index += width;
                } else {
                    out.push('$');
                    index += 1;
                }
            }
            _ => {
                out.push('$');
                index += 1;
            }
        }
    }
    out
}

fn replace(interp: &mut Interp<'_>, text: &str, args: &[Value], all: bool) -> R<Value> {
    let pattern = arg(args, 0);
    let replacement = arg(args, 1);
    let len = length(text);
    // (start, end, match array) of each match to replace.
    let mut matches: Vec<(usize, usize, Vec<Value>, Value)> = Vec::new();
    match &pattern {
        Value::Regex(regex) => {
            let global = regex.flags.contains('g');
            if all && !global {
                return Err(interp.type_error("replaceAll must be called with a global RegExp"));
            }
            for (item, start, end) in all_matches(interp, regex, text, global)? {
                let (values, groups) = match &item {
                    Value::Array(array) => (
                        array.items.borrow().clone(),
                        array
                            .extra
                            .borrow()
                            .get("groups")
                            .cloned()
                            .unwrap_or(Value::Undefined),
                    ),
                    _ => (Vec::new(), Value::Undefined),
                };
                matches.push((start, end, values, groups));
            }
            if global {
                regex.last_index.set(0);
            }
        }
        other => {
            let needle = to_string(other);
            let needle_len = length(&needle);
            let mut position = 0;
            while position <= len {
                let Some(start) = index_of(text, &needle, position) else {
                    break;
                };
                matches.push((
                    start,
                    start + needle_len,
                    vec![Value::str(needle.as_str())],
                    Value::Undefined,
                ));
                if !all {
                    break;
                }
                position = if needle_len == 0 {
                    start + 1
                } else {
                    start + needle_len
                };
                interp.tick()?;
            }
        }
    }
    let mut out = String::new();
    let mut last = 0;
    for (start, end, values, groups) in matches {
        out.push_str(slice(text, last, start));
        if matches!(replacement, Value::Function(_)) {
            let mut call_args = values.clone();
            call_args.push(Value::Number(start as f64));
            call_args.push(Value::str(text));
            if !matches!(groups, Value::Undefined) {
                call_args.push(groups.clone());
            }
            let result = interp.call(&replacement, Value::Undefined, call_args)?;
            out.push_str(&to_string(&result));
        } else {
            out.push_str(&expand(
                &to_string(&replacement),
                text,
                &values,
                start,
                end,
                &groups,
            ));
        }
        last = end;
        if out.len() > MAX_STRING_CHARS {
            return Err(interp.throw("RangeError", "Invalid string length"));
        }
    }
    out.push_str(slice(text, last.min(len), len));
    interp.alloc(out.len())?;
    Ok(Value::str(out))
}

fn split(interp: &mut Interp<'_>, text: &str, args: &[Value]) -> R<Value> {
    let limit = match arg(args, 1) {
        Value::Undefined => usize::MAX,
        value => crate::interp::to_uint32(to_number(&value)) as usize,
    };
    let separator = arg(args, 0);
    let mut parts: Vec<Value> = Vec::new();
    match &separator {
        Value::Undefined => parts.push(Value::str(text)),
        Value::Regex(regex) => {
            let len = length(text);
            if len == 0 {
                let empty_match = exec(interp, regex, text, 0)?.is_some();
                return Ok(Value::array(if empty_match {
                    Vec::new()
                } else {
                    vec![Value::str("")]
                }));
            }
            let mut last = 0;
            let mut position = 0;
            while position < len {
                let Some((item, start, end)) = exec(interp, regex, text, position)? else {
                    break;
                };
                if start >= len {
                    break;
                }
                // An empty match splits between characters, never before
                // the first one or where the last piece ended.
                if end == start && start <= last {
                    position = start + 1;
                    continue;
                }
                parts.push(Value::str(slice(text, last, start)));
                if let Value::Array(array) = &item {
                    parts.extend(array.items.borrow().iter().skip(1).cloned());
                }
                last = end;
                position = if end == start { end + 1 } else { end };
                interp.tick()?;
            }
            parts.push(Value::str(slice(text, last, len)));
        }
        other => {
            let separator = to_string(other);
            if separator.is_empty() {
                parts.extend(text.chars().map(|c| Value::str(c.to_string())));
            } else {
                parts.extend(text.split(separator.as_str()).map(Value::str));
            }
        }
    }
    parts.truncate(limit);
    interp.alloc(parts.len() * 16 + text.len())?;
    Ok(Value::array(parts))
}
