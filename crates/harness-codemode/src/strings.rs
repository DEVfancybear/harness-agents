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
                match_global(interp, regex, &text)?
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
            match_all(interp, regex, &text)?
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
    let multiline = flags.contains('m');
    let dot_all = flags.contains('s');
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
            // JavaScript ends a line at `\r`, U+2028 and U+2029 as well as at
            // `\n`: with `m`, `^` and `$` match there, and `.` never matches
            // one. A text with CRLF line ends is the common case.
            '^' if !in_class && multiline => out.push_str("(?:^|(?<=[\\r\\x{2028}\\x{2029}]))"),
            '$' if !in_class && multiline => out.push_str("(?:$|(?=[\\r\\x{2028}\\x{2029}]))"),
            '.' if !in_class && !dot_all => out.push_str("[^\\n\\r\\x{2028}\\x{2029}]"),
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

/// One match, in byte offsets of the text it was found in: the whole match
/// first, then each group (`None` for a group that took no part).
struct Found {
    groups: Vec<Option<(usize, usize)>>,
}

impl Found {
    fn start(&self) -> usize {
        self.groups
            .first()
            .copied()
            .flatten()
            .map_or(0, |group| group.0)
    }

    fn end(&self) -> usize {
        self.groups
            .first()
            .copied()
            .flatten()
            .map_or(0, |group| group.1)
    }

    fn text<'a>(&self, text: &'a str, group: usize) -> Option<&'a str> {
        self.groups
            .get(group)
            .copied()
            .flatten()
            .map(|(start, end)| &text[start..end])
    }
}

/// The next match at or after byte `from`; a sticky expression's only at
/// `from`.
fn find_at(interp: &Interp<'_>, regex: &RegexObj, text: &str, from: usize) -> R<Option<Found>> {
    if from > text.len() {
        return Ok(None);
    }
    let captures = regex
        .regex
        .captures_from_pos(text, from)
        .map_err(|error| regex_failure(interp, &error))?;
    let Some(captures) = captures else {
        return Ok(None);
    };
    let found = Found {
        groups: (0..captures.len())
            .map(|group| captures.get(group).map(|m| (m.start(), m.end())))
            .collect(),
    };
    if regex.flags.contains('y') && found.start() != from {
        return Ok(None);
    }
    Ok(Some(found))
}

/// Where the next search goes after a match ending at `end`: past it, or
/// past the next character when the match was empty.
fn next_from(text: &str, found: &Found) -> usize {
    let end = found.end();
    if end > found.start() {
        return end;
    }
    text[end..]
        .chars()
        .next()
        .map_or(end + 1, |c| end + c.len_utf8())
}

/// Character positions of byte offsets asked in increasing order, counted
/// on from the last one, so a scan of a whole text is linear.
struct Chars<'a> {
    text: &'a str,
    byte: usize,
    count: usize,
}

impl<'a> Chars<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            byte: 0,
            count: 0,
        }
    }

    fn at(&mut self, byte: usize) -> usize {
        if self.text.is_ascii() {
            return byte;
        }
        if byte < self.byte {
            self.byte = 0;
            self.count = 0;
        }
        self.count += self.text[self.byte..byte].chars().count();
        self.byte = byte;
        self.count
    }
}

/// The names of an expression's groups, by group number.
fn group_names(regex: &RegexObj) -> Vec<Option<String>> {
    regex
        .regex
        .capture_names()
        .map(|name| name.map(str::to_owned))
        .collect()
}

/// The `groups` object of a match: `undefined` without named groups.
fn groups_object(names: &[Option<String>], found: &Found, text: &str) -> Value {
    let mut groups = IndexMap::new();
    for (group, name) in names.iter().enumerate() {
        if let Some(name) = name {
            groups.insert(
                Rc::from(name.as_str()),
                found.text(text, group).map_or(Value::Undefined, Value::str),
            );
        }
    }
    if groups.is_empty() {
        Value::Undefined
    } else {
        Value::object(groups)
    }
}

/// The match array of `exec`: its groups, and `index` (in characters),
/// `input` (the searched string itself, not a copy) and `groups`.
fn match_array(
    interp: &mut Interp<'_>,
    names: &[Option<String>],
    text: &str,
    input: &Value,
    found: &Found,
    index: usize,
) -> R<Value> {
    let items: Vec<Value> = (0..found.groups.len())
        .map(|group| found.text(text, group).map_or(Value::Undefined, Value::str))
        .collect();
    let matched: usize = found
        .groups
        .iter()
        .flatten()
        .map(|(start, end)| end - start)
        .sum();
    interp.alloc(matched + items.len() * 16 + 64)?;
    let array = Value::array(items);
    if let Value::Array(inner) = &array {
        let mut extra = inner.extra.borrow_mut();
        extra.insert(Rc::from("index"), Value::Number(index as f64));
        extra.insert(Rc::from("input"), input.clone());
        extra.insert(Rc::from("groups"), groups_object(names, found, text));
    }
    Ok(array)
}

/// `regex.exec(input)` from character `from`: the match array and where the
/// match starts and ends, in characters.
fn exec(
    interp: &mut Interp<'_>,
    regex: &RegexObj,
    input: &Rc<str>,
    from: usize,
) -> R<Option<(Value, usize, usize)>> {
    let text: &str = input;
    let Some(found) = find_at(interp, regex, text, byte_of(text, from))? else {
        return Ok(None);
    };
    let mut chars = Chars::new(text);
    let start = chars.at(found.start());
    let end = chars.at(found.end());
    let array = match_array(
        interp,
        &group_names(regex),
        text,
        &Value::Str(input.clone()),
        &found,
        start,
    )?;
    Ok(Some((array, start, end)))
}

/// `match` with a global expression: every matched string.
fn match_global(interp: &mut Interp<'_>, regex: &RegexObj, text: &str) -> R<Value> {
    let mut found = Vec::new();
    for item in all_matches(interp, regex, text, true)? {
        found.push(Value::str(item.text(text, 0).unwrap_or("")));
    }
    regex.last_index.set(0);
    interp.alloc(found.len() * 16)?;
    Ok(if found.is_empty() {
        Value::Null
    } else {
        Value::array(found)
    })
}

/// `matchAll`: the match array of every match.
fn match_all(interp: &mut Interp<'_>, regex: &RegexObj, input: &Rc<str>) -> R<Value> {
    let text: &str = input;
    let names = group_names(regex);
    let shared = Value::Str(input.clone());
    let mut chars = Chars::new(text);
    let mut arrays = Vec::new();
    for found in all_matches(interp, regex, text, true)? {
        let index = chars.at(found.start());
        arrays.push(match_array(interp, &names, text, &shared, &found, index)?);
    }
    Ok(Value::array(arrays))
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
            let input: Rc<str> = match arg(args, 0) {
                Value::Str(text) => text,
                other => Rc::from(to_string(&other)),
            };
            let advancing = regex.flags.contains('g') || regex.flags.contains('y');
            let from = if advancing { regex.last_index.get() } else { 0 };
            if from > length(&input) {
                regex.last_index.set(0);
                return Ok(if method == "test" {
                    Value::Bool(false)
                } else {
                    Value::Null
                });
            }
            let found = exec(interp, regex, &input, from)?;
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

/// Every match of `regex` in `text` (only the first unless `global`), in
/// byte offsets.
fn all_matches(
    interp: &mut Interp<'_>,
    regex: &RegexObj,
    text: &str,
    global: bool,
) -> R<Vec<Found>> {
    let mut found = Vec::new();
    let mut position = 0;
    while position <= text.len() {
        let Some(item) = find_at(interp, regex, text, position)? else {
            break;
        };
        position = next_from(text, &item);
        found.push(item);
        if !global {
            break;
        }
        interp.tick()?;
        interp.alloc(64)?;
    }
    Ok(found)
}

/// `$&`, `$1`, `$<name>`, `` $` ``, `$'` and `$$` in a replacement.
fn expand(template: &str, text: &str, found: &Found, names: &[Option<String>]) -> String {
    let group_count = found.groups.len();
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
                out.push_str(found.text(text, 0).unwrap_or(""));
                index += 2;
            }
            '`' => {
                out.push_str(&text[..found.start()]);
                index += 2;
            }
            '\'' => {
                out.push_str(&text[found.end()..]);
                index += 2;
            }
            '<' if names.iter().any(Option::is_some) => {
                if let Some(close) = chars[index..].iter().position(|c| *c == '>') {
                    let name: String = chars[index + 2..index + close].iter().collect();
                    if let Some(group) = names.iter().position(|n| n.as_deref() == Some(&name)) {
                        out.push_str(found.text(text, group).unwrap_or(""));
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
                    .filter(|n: &usize| *n > 0 && *n < group_count);
                let (group, width) = match two {
                    Some(group) => (group, 3),
                    None => (c.to_digit(10).unwrap_or(0) as usize, 2),
                };
                if group > 0 && group < group_count {
                    out.push_str(found.text(text, group).unwrap_or(""));
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

fn replace(interp: &mut Interp<'_>, input: &Rc<str>, args: &[Value], all: bool) -> R<Value> {
    let text: &str = input;
    let pattern = arg(args, 0);
    let replacement = arg(args, 1);
    let mut names: Vec<Option<String>> = vec![None];
    let matches: Vec<Found> = match &pattern {
        Value::Regex(regex) => {
            let global = regex.flags.contains('g');
            if all && !global {
                return Err(interp.type_error("replaceAll must be called with a global RegExp"));
            }
            names = group_names(regex);
            let found = all_matches(interp, regex, text, global)?;
            if global {
                regex.last_index.set(0);
            }
            found
        }
        other => {
            let needle = to_string(other);
            let mut found = Vec::new();
            let mut position = 0;
            while position <= text.len() {
                let Some(start) = text[position..]
                    .find(needle.as_str())
                    .map(|at| at + position)
                else {
                    break;
                };
                let item = Found {
                    groups: vec![Some((start, start + needle.len()))],
                };
                position = next_from(text, &item);
                found.push(item);
                if !all {
                    break;
                }
                interp.tick()?;
            }
            found
        }
    };
    let template = (!matches!(replacement, Value::Function(_))).then(|| to_string(&replacement));
    let shared = Value::Str(input.clone());
    let mut chars = Chars::new(text);
    let mut out = String::new();
    let mut last = 0;
    for found in &matches {
        out.push_str(&text[last..found.start()]);
        if let Some(template) = &template {
            out.push_str(&expand(template, text, found, &names));
        } else {
            let mut call_args: Vec<Value> = (0..found.groups.len())
                .map(|group| found.text(text, group).map_or(Value::Undefined, Value::str))
                .collect();
            call_args.push(Value::Number(chars.at(found.start()) as f64));
            call_args.push(shared.clone());
            let groups = groups_object(&names, found, text);
            if !matches!(groups, Value::Undefined) {
                call_args.push(groups);
            }
            let result = interp.call(&replacement, Value::Undefined, call_args)?;
            out.push_str(&to_string(&result));
        }
        last = found.end();
        if out.len() > MAX_STRING_CHARS {
            return Err(interp.throw("RangeError", "Invalid string length"));
        }
    }
    out.push_str(&text[last.min(text.len())..]);
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
            if text.is_empty() {
                let empty_match = find_at(interp, regex, text, 0)?.is_some();
                return Ok(Value::array(if empty_match {
                    Vec::new()
                } else {
                    vec![Value::str("")]
                }));
            }
            let mut last = 0;
            let mut position = 0;
            while position < text.len() {
                let Some(found) = find_at(interp, regex, text, position)? else {
                    break;
                };
                let (start, end) = (found.start(), found.end());
                if start >= text.len() {
                    break;
                }
                // An empty match splits between characters, never before
                // the first one or where the last piece ended.
                if end == start && start <= last {
                    position = next_from(text, &found);
                    continue;
                }
                parts.push(Value::str(&text[last..start]));
                for group in 1..found.groups.len() {
                    parts.push(found.text(text, group).map_or(Value::Undefined, Value::str));
                }
                last = end;
                position = next_from(text, &found);
                interp.tick()?;
                if parts.len() >= limit {
                    break;
                }
            }
            parts.push(Value::str(&text[last..]));
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
