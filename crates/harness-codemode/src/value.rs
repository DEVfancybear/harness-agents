//! Values of a codemode script and the conversions ECMAScript defines for
//! them: `ToString`, `ToNumber`, `ToBoolean`, equality, and JSON text both
//! ways. Objects, arrays, `Map` and `Set` keep insertion order.

use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::ast;
use crate::interp::Env;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Object,
    /// An `Error` (or `TypeError`, ...): its `name` says which.
    Error,
}

pub struct Object {
    pub props: RefCell<IndexMap<Rc<str>, Value>>,
    pub class: Class,
    pub frozen: Cell<bool>,
}

pub struct Array {
    pub items: RefCell<Vec<Value>>,
    /// Named properties of an array: `index`, `input` and `groups` of a
    /// regular expression match, or what a script set on one.
    pub extra: RefCell<IndexMap<Rc<str>, Value>>,
    pub frozen: Cell<bool>,
}

pub enum Function {
    /// An arrow has no `this` of its own: it reads the one of the scope it
    /// was made in.
    Closure { def: Rc<ast::Function>, env: Env },
    /// A built-in, called with `this` bound: `"Array.prototype.map"` with
    /// the array, `"Math.max"`, `"tools"` with the tool's name.
    Native { name: Rc<str>, this: Value },
}

#[derive(Clone)]
pub enum Settled {
    Fulfilled(Value),
    Rejected(Value),
}

pub enum PromiseState {
    /// A tool call, run when something awaits it.
    Call {
        name: Rc<str>,
        args: Option<String>,
    },
    /// `base.then(on_fulfilled, on_rejected)`, or `finally` when `finally`.
    Then {
        base: Rc<Promise>,
        on_fulfilled: Option<Value>,
        on_rejected: Option<Value>,
        finally: bool,
    },
    /// `Promise.all` and its siblings over these items.
    Combine {
        items: Vec<Value>,
        how: Combine,
    },
    /// Made by `new Promise(executor)` and not settled by it: nothing can
    /// settle it later, since scripts have no timers.
    Waiting,
    /// Being forced: awaiting it from inside its own handler.
    Running,
    Settled(Settled),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    All,
    AllSettled,
    Race,
    Any,
}

pub struct Promise {
    pub state: RefCell<PromiseState>,
    /// The call record this promise's tool call has, once it ran.
    pub call: Cell<Option<usize>>,
}

impl Promise {
    pub fn new(state: PromiseState) -> Rc<Self> {
        Rc::new(Self {
            state: RefCell::new(state),
            call: Cell::new(None),
        })
    }

    pub fn settled(&self) -> Option<Settled> {
        match &*self.state.borrow() {
            PromiseState::Settled(settled) => Some(settled.clone()),
            _ => None,
        }
    }
}

pub struct RegexObj {
    pub source: Rc<str>,
    pub flags: Rc<str>,
    pub regex: fancy_regex::Regex,
    pub last_index: Cell<usize>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum MapKey {
    Undefined,
    Null,
    Bool(bool),
    Number(u64),
    Str(Rc<str>),
    Ref(usize),
}

#[derive(Clone)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    Str(Rc<str>),
    Array(Rc<Array>),
    Object(Rc<Object>),
    Function(Rc<Function>),
    Promise(Rc<Promise>),
    Map(Rc<RefCell<IndexMap<MapKey, (Value, Value)>>>),
    Set(Rc<RefCell<IndexMap<MapKey, Value>>>),
    Regex(Rc<RegexObj>),
    Date(f64),
    /// `Math`, `JSON`, `console`, `globalThis`: names whose members are
    /// built-ins.
    Namespace(&'static str),
    /// The script's `tools` object.
    Tools,
}

impl Value {
    pub fn str(text: impl Into<Rc<str>>) -> Self {
        Self::Str(text.into())
    }

    pub fn array(items: Vec<Value>) -> Self {
        let array = Rc::new(Array {
            items: RefCell::new(items),
            extra: RefCell::new(IndexMap::new()),
            frozen: Cell::new(false),
        });
        crate::heap::array(&array);
        Self::Array(array)
    }

    pub fn object(props: IndexMap<Rc<str>, Value>) -> Self {
        let object = Rc::new(Object {
            props: RefCell::new(props),
            class: Class::Object,
            frozen: Cell::new(false),
        });
        crate::heap::object(&object);
        Self::Object(object)
    }

    pub fn map(entries: IndexMap<MapKey, (Value, Value)>) -> Self {
        let map = Rc::new(RefCell::new(entries));
        crate::heap::map(&map);
        Self::Map(map)
    }

    pub fn set(entries: IndexMap<MapKey, Value>) -> Self {
        let set = Rc::new(RefCell::new(entries));
        crate::heap::set(&set);
        Self::Set(set)
    }

    pub fn object_from<const N: usize>(entries: [(&str, Value); N]) -> Self {
        Self::object(
            entries
                .into_iter()
                .map(|(key, value)| (Rc::from(key), value))
                .collect(),
        )
    }

    pub fn native(name: impl Into<Rc<str>>, this: Value) -> Self {
        Self::Function(Rc::new(Function::Native {
            name: name.into(),
            this,
        }))
    }

    pub fn is_nullish(&self) -> bool {
        matches!(self, Self::Undefined | Self::Null)
    }

    pub fn truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Bool(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::Str(text) => !text.is_empty(),
            _ => true,
        }
    }

    pub fn type_of(&self) -> &'static str {
        match self {
            Self::Undefined => "undefined",
            Self::Null
            | Self::Array(_)
            | Self::Object(_)
            | Self::Promise(_)
            | Self::Map(_)
            | Self::Set(_)
            | Self::Regex(_)
            | Self::Date(_)
            | Self::Namespace(_)
            | Self::Tools => "object",
            Self::Bool(_) => "boolean",
            Self::Number(_) => "number",
            Self::Str(_) => "string",
            Self::Function(_) => "function",
        }
    }

    /// Identity of an object, for `===`, `Map` keys and cycle checks.
    pub fn address(&self) -> Option<usize> {
        Some(match self {
            Self::Array(rc) => Rc::as_ptr(rc).cast::<()>() as usize,
            Self::Object(rc) => Rc::as_ptr(rc).cast::<()>() as usize,
            Self::Function(rc) => Rc::as_ptr(rc).cast::<()>() as usize,
            Self::Promise(rc) => Rc::as_ptr(rc).cast::<()>() as usize,
            Self::Map(rc) => Rc::as_ptr(rc).cast::<()>() as usize,
            Self::Set(rc) => Rc::as_ptr(rc).cast::<()>() as usize,
            Self::Regex(rc) => Rc::as_ptr(rc).cast::<()>() as usize,
            _ => return None,
        })
    }

    pub fn map_key(&self) -> MapKey {
        match self {
            Self::Undefined => MapKey::Undefined,
            Self::Null => MapKey::Null,
            Self::Bool(value) => MapKey::Bool(*value),
            Self::Number(value) => {
                // SameValueZero: -0 is 0, and NaN is one key.
                let value = if *value == 0.0 { 0.0 } else { *value };
                MapKey::Number(if value.is_nan() {
                    f64::NAN.to_bits()
                } else {
                    value.to_bits()
                })
            }
            Self::Str(text) => MapKey::Str(text.clone()),
            Self::Date(time) => MapKey::Number(time.to_bits()),
            Self::Namespace(name) => MapKey::Str(Rc::from(format!("\u{0}{name}"))),
            Self::Tools => MapKey::Str(Rc::from("\u{0}tools")),
            other => MapKey::Ref(other.address().unwrap_or(0)),
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Self::Object(object) if object.class == Class::Error)
    }

    /// A property of a plain object or error, without built-ins.
    pub fn own(&self, key: &str) -> Option<Value> {
        match self {
            Self::Object(object) => object.props.borrow().get(key).cloned(),
            _ => None,
        }
    }
}

pub fn strict_equals(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undefined, Value::Undefined)
        | (Value::Null, Value::Null)
        | (Value::Tools, Value::Tools) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Namespace(x), Value::Namespace(y)) => x == y,
        // Dates are values here, not objects: the same time is the same date.
        (Value::Date(x), Value::Date(y)) => x.to_bits() == y.to_bits(),
        _ => match (a.address(), b.address()) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
    }
}

/// `==`: `null == undefined`, and numbers, strings and booleans compared as
/// numbers when their types differ.
pub fn loose_equals(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (x, y) if x.is_nullish() && y.is_nullish() => true,
        (x, y) if x.is_nullish() || y.is_nullish() => false,
        (
            Value::Number(_) | Value::Str(_) | Value::Bool(_),
            Value::Number(_) | Value::Str(_) | Value::Bool(_),
        ) if a.type_of() != b.type_of() => to_number(a) == to_number(b),
        (Value::Array(_) | Value::Object(_), Value::Number(_) | Value::Str(_) | Value::Bool(_)) => {
            loose_equals(&Value::str(to_string(a)), b)
        }
        (Value::Number(_) | Value::Str(_) | Value::Bool(_), Value::Array(_) | Value::Object(_)) => {
            loose_equals(a, &Value::str(to_string(b)))
        }
        _ => strict_equals(a, b),
    }
}

/// `SameValueZero`, which `includes`, `Map` and `Set` use.
pub fn same_value_zero(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) if x.is_nan() && y.is_nan() => true,
        _ => strict_equals(a, b),
    }
}

pub fn string_to_number(text: &str) -> f64 {
    let text = text.trim();
    if text.is_empty() {
        return 0.0;
    }
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits == "Infinity" {
        return sign * f64::INFINITY;
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            if rest.is_empty() || !rest.chars().all(|c| c.is_digit(radix)) {
                return f64::NAN;
            }
            return rest.chars().fold(0.0, |value, c| {
                value * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0))
            });
        }
    }
    let valid = digits
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
        && digits
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '.');
    if !valid {
        return f64::NAN;
    }
    digits.parse::<f64>().map_or(f64::NAN, |value| sign * value)
}

pub fn to_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(value) => f64::from(u8::from(*value)),
        Value::Number(value) => *value,
        Value::Str(text) => string_to_number(text),
        Value::Date(time) => *time,
        Value::Array(array) => {
            let items = array.items.borrow();
            match items.len() {
                0 => 0.0,
                1 => to_number(&Value::str(to_string(&items[0]))),
                _ => f64::NAN,
            }
        }
        _ => f64::NAN,
    }
}

/// ECMAScript `Number::toString`: the shortest digits that read back as the
/// same number, in plain notation from 1e-7 up to 1e21.
pub fn number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if value < 0.0 {
        return format!("-{}", number_to_string(-value));
    }
    // `{:e}` gives the shortest round-trip digits: "1.2345e-7".
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let k = i32::try_from(digits.len()).unwrap_or(i32::MAX);
    let n = exponent + 1;
    if k <= n && n <= 21 {
        let zeros = usize::try_from(n - k).unwrap_or(0);
        format!("{digits}{}", "0".repeat(zeros))
    } else if 0 < n && n <= 21 {
        let point = usize::try_from(n).unwrap_or(0);
        format!("{}.{}", &digits[..point], &digits[point..])
    } else if -6 < n && n <= 0 {
        let zeros = usize::try_from(-n).unwrap_or(0);
        format!("0.{}{digits}", "0".repeat(zeros))
    } else {
        let sign = if n - 1 < 0 { '-' } else { '+' };
        let power = (n - 1).abs();
        if k == 1 {
            format!("{digits}e{sign}{power}")
        } else {
            format!("{}.{}e{sign}{power}", &digits[..1], &digits[1..])
        }
    }
}

pub fn date_iso(time: f64) -> Option<String> {
    if !time.is_finite() {
        return None;
    }
    let millis = time as i64;
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
        .map(|date| date.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// `String(value)`.
/// How deep `JSON.stringify` and `String()` go into nested values.
const MAX_JSON_DEPTH: usize = 10_000;
const MAX_STRING_DEPTH: usize = 1_000;

pub fn to_string(value: &Value) -> String {
    to_string_at(value, 0)
}

fn to_string_at(value: &Value, depth: usize) -> String {
    match value {
        Value::Undefined => "undefined".to_owned(),
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => number_to_string(*value),
        Value::Str(text) => text.to_string(),
        Value::Array(array) => {
            let items = array.items.borrow();
            let mut text = String::new();
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                if !item.is_nullish() {
                    // A cycle prints empty, as engines print it.
                    if let Value::Array(inner) = item
                        && Rc::ptr_eq(inner, array)
                    {
                        continue;
                    }
                    if depth < MAX_STRING_DEPTH {
                        text.push_str(&to_string_at(item, depth + 1));
                    }
                }
            }
            text
        }
        Value::Object(object) if object.class == Class::Error => {
            let props = object.props.borrow();
            let name = props
                .get("name")
                .map_or_else(|| "Error".to_owned(), to_string);
            let message = props.get("message").map(to_string).unwrap_or_default();
            if message.is_empty() {
                name
            } else if name.is_empty() {
                message
            } else {
                format!("{name}: {message}")
            }
        }
        Value::Object(_) | Value::Tools => "[object Object]".to_owned(),
        Value::Namespace(name) => format!("[object {name}]"),
        Value::Function(function) => match &**function {
            Function::Closure { def, .. } => format!(
                "function {}() {{ [code] }}",
                def.name.as_deref().unwrap_or("")
            ),
            Function::Native { name, .. } => {
                let short = name.rsplit('.').next().unwrap_or(name);
                format!("function {short}() {{ [native code] }}")
            }
        },
        Value::Promise(_) => "[object Promise]".to_owned(),
        Value::Map(_) => "[object Map]".to_owned(),
        Value::Set(_) => "[object Set]".to_owned(),
        Value::Regex(regex) => format!("/{}/{}", regex.source, regex.flags),
        Value::Date(time) => date_iso(*time).unwrap_or_else(|| "Invalid Date".to_owned()),
    }
}

/// `ToPropertyKey`.
pub fn to_key(value: &Value) -> Rc<str> {
    match value {
        Value::Str(text) => text.clone(),
        other => Rc::from(to_string(other)),
    }
}

/// An array index a key names, if it names one.
pub fn array_index(key: &str) -> Option<usize> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    if !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<usize>().ok()
}

pub fn json_quote(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[derive(Debug)]
pub enum JsonError {
    Cycle,
    /// `JSON.stringify` met a value it refuses, such as a `Promise`.
    Unsupported(&'static str),
}

/// `JSON.stringify(value, null, indent)`; `None` where JavaScript returns
/// `undefined` (a function, `undefined`).
pub fn json_stringify(value: &Value, indent: &str) -> Result<Option<String>, JsonError> {
    let mut out = String::new();
    let mut stack = std::collections::HashSet::new();
    if write_json(value, indent, 0, &mut stack, &mut out)? {
        Ok(Some(out))
    } else {
        Ok(None)
    }
}

/// Whether `value` wrote anything: `undefined` and functions do not.
fn write_json(
    value: &Value,
    indent: &str,
    level: usize,
    stack: &mut std::collections::HashSet<usize>,
    out: &mut String,
) -> Result<bool, JsonError> {
    let newline = |out: &mut String, level: usize| {
        if !indent.is_empty() {
            out.push('\n');
            for _ in 0..level {
                out.push_str(indent);
            }
        }
    };
    match value {
        Value::Undefined | Value::Function(_) => return Ok(false),
        Value::Null => out.push_str("null"),
        Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => {
            if value.is_finite() {
                out.push_str(&number_to_string(*value));
            } else {
                out.push_str("null");
            }
        }
        Value::Str(text) => json_quote(text, out),
        Value::Date(time) => match date_iso(*time) {
            Some(iso) => json_quote(&iso, out),
            None => out.push_str("null"),
        },
        Value::Promise(_) => return Err(JsonError::Unsupported("a Promise (await it first)")),
        Value::Array(array) => {
            let address = value.address().unwrap_or(0);
            if stack.len() >= MAX_JSON_DEPTH {
                return Err(JsonError::Unsupported(
                    "a value nested more than 10000 levels deep",
                ));
            }
            if stack.contains(&address) {
                return Err(JsonError::Cycle);
            }
            stack.insert(address);
            let items = array.items.borrow().clone();
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                newline(out, level + 1);
                if !write_json(item, indent, level + 1, stack, out)? {
                    out.push_str("null");
                }
            }
            if !items.is_empty() {
                newline(out, level);
            }
            out.push(']');
            stack.remove(&address);
        }
        Value::Object(object) => {
            let address = value.address().unwrap_or(0);
            if stack.len() >= MAX_JSON_DEPTH {
                return Err(JsonError::Unsupported(
                    "a value nested more than 10000 levels deep",
                ));
            }
            if stack.contains(&address) {
                return Err(JsonError::Cycle);
            }
            stack.insert(address);
            let props: Vec<(Rc<str>, Value)> = object
                .props
                .borrow()
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            out.push('{');
            let mut first = true;
            for (key, item) in &props {
                let mark = out.len();
                if !first {
                    out.push(',');
                }
                newline(out, level + 1);
                json_quote(key, out);
                out.push(':');
                if !indent.is_empty() {
                    out.push(' ');
                }
                if write_json(item, indent, level + 1, stack, out)? {
                    first = false;
                } else {
                    out.truncate(mark);
                }
            }
            if !first {
                newline(out, level);
            }
            out.push('}');
            stack.remove(&address);
        }
        // Like engines: Map, Set, RegExp and namespaces have no own
        // enumerable properties.
        Value::Map(_) | Value::Set(_) | Value::Regex(_) | Value::Namespace(_) | Value::Tools => {
            out.push_str("{}");
        }
    }
    Ok(true)
}

/// `JSON.parse`.
pub fn json_parse(text: &str) -> Result<Value, String> {
    let mut parser = JsonParser {
        chars: text.chars().collect(),
        pos: 0,
        depth: 0,
    };
    parser.skip();
    let value = parser.value()?;
    parser.skip();
    if parser.pos < parser.chars.len() {
        return Err(parser.unexpected());
    }
    Ok(value)
}

struct JsonParser {
    chars: Vec<char>,
    pos: usize,
    depth: usize,
}

impl JsonParser {
    fn skip(&mut self) {
        while self
            .chars
            .get(self.pos)
            .is_some_and(|c| matches!(c, ' ' | '\t' | '\n' | '\r'))
        {
            self.pos += 1;
        }
    }

    fn unexpected(&self) -> String {
        match self.chars.get(self.pos) {
            Some(c) => format!("Unexpected token '{c}' in JSON at position {}", self.pos),
            None => "Unexpected end of JSON input".to_owned(),
        }
    }

    fn eat(&mut self, c: char) -> bool {
        if self.chars.get(self.pos) == Some(&c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, String> {
        for expected in word.chars() {
            if !self.eat(expected) {
                return Err(self.unexpected());
            }
        }
        Ok(value)
    }

    fn value(&mut self) -> Result<Value, String> {
        self.depth += 1;
        if self.depth > 5000 {
            return Err("JSON is nested too deeply".to_owned());
        }
        let result = self.value_inner();
        self.depth -= 1;
        result
    }

    fn value_inner(&mut self) -> Result<Value, String> {
        match self.chars.get(self.pos) {
            Some('{') => {
                self.pos += 1;
                let mut props = IndexMap::new();
                self.skip();
                if self.eat('}') {
                    return Ok(Value::object(props));
                }
                loop {
                    self.skip();
                    if self.chars.get(self.pos) != Some(&'"') {
                        return Err(self.unexpected());
                    }
                    let key = self.string()?;
                    self.skip();
                    if !self.eat(':') {
                        return Err(self.unexpected());
                    }
                    self.skip();
                    let value = self.value()?;
                    props.insert(Rc::from(key), value);
                    self.skip();
                    if self.eat(',') {
                        continue;
                    }
                    if self.eat('}') {
                        return Ok(Value::object(props));
                    }
                    return Err(self.unexpected());
                }
            }
            Some('[') => {
                self.pos += 1;
                let mut items = Vec::new();
                self.skip();
                if self.eat(']') {
                    return Ok(Value::array(items));
                }
                loop {
                    self.skip();
                    items.push(self.value()?);
                    self.skip();
                    if self.eat(',') {
                        continue;
                    }
                    if self.eat(']') {
                        return Ok(Value::array(items));
                    }
                    return Err(self.unexpected());
                }
            }
            Some('"') => Ok(Value::str(self.string()?)),
            Some('t') => self.literal("true", Value::Bool(true)),
            Some('f') => self.literal("false", Value::Bool(false)),
            Some('n') => self.literal("null", Value::Null),
            Some(c) if *c == '-' || c.is_ascii_digit() => self.number(),
            _ => Err(self.unexpected()),
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        self.eat('-');
        let digits = |parser: &mut Self| {
            let begin = parser.pos;
            while parser
                .chars
                .get(parser.pos)
                .is_some_and(char::is_ascii_digit)
            {
                parser.pos += 1;
            }
            parser.pos > begin
        };
        if self.chars.get(self.pos) == Some(&'0') {
            self.pos += 1;
        } else if !digits(self) {
            return Err(self.unexpected());
        }
        if self.eat('.') && !digits(self) {
            return Err(self.unexpected());
        }
        if self.eat('e') || self.eat('E') {
            if !self.eat('+') {
                self.eat('-');
            }
            if !digits(self) {
                return Err(self.unexpected());
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<f64>()
            .map(Value::Number)
            .map_err(|_| self.unexpected())
    }

    fn string(&mut self) -> Result<String, String> {
        self.pos += 1;
        let mut text = String::new();
        loop {
            match self.chars.get(self.pos).copied() {
                None => return Err("Unterminated string in JSON".to_owned()),
                Some('"') => {
                    self.pos += 1;
                    return Ok(text);
                }
                Some('\\') => {
                    self.pos += 1;
                    let escaped = self.chars.get(self.pos).copied();
                    self.pos += 1;
                    match escaped {
                        Some('"') => text.push('"'),
                        Some('\\') => text.push('\\'),
                        Some('/') => text.push('/'),
                        Some('b') => text.push('\u{8}'),
                        Some('f') => text.push('\u{c}'),
                        Some('n') => text.push('\n'),
                        Some('r') => text.push('\r'),
                        Some('t') => text.push('\t'),
                        Some('u') => {
                            let high = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&high)
                                && self.chars.get(self.pos) == Some(&'\\')
                                && self.chars.get(self.pos + 1) == Some(&'u')
                            {
                                self.pos += 2;
                                let low = self.hex4()?;
                                0x10000
                                    + ((high - 0xD800) << 10)
                                    + (low.wrapping_sub(0xDC00) & 0x3ff)
                            } else {
                                high
                            };
                            text.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        _ => {
                            self.pos -= 1;
                            return Err(self.unexpected());
                        }
                    }
                }
                Some(c) if (c as u32) < 0x20 => return Err(self.unexpected()),
                Some(c) => {
                    text.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut value = 0;
        for _ in 0..4 {
            let digit = self
                .chars
                .get(self.pos)
                .and_then(|c| c.to_digit(16))
                .ok_or_else(|| self.unexpected())?;
            value = value * 16 + digit;
            self.pos += 1;
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::{Value, json_parse, json_stringify, number_to_string, string_to_number, to_string};

    #[test]
    fn prints_numbers_as_javascript_does() {
        for (value, text) in [
            (1.0, "1"),
            (-0.5, "-0.5"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1e21, "1e+21"),
            (1.5e-7, "1.5e-7"),
            (123_456_789_012.0, "123456789012"),
            (0.000_001, "0.000001"),
            (f64::NAN, "NaN"),
            (f64::NEG_INFINITY, "-Infinity"),
        ] {
            assert_eq!(number_to_string(value), text);
        }
    }

    #[test]
    fn reads_numbers_from_strings() {
        assert_eq!(string_to_number(" 42 "), 42.0);
        assert_eq!(string_to_number(""), 0.0);
        assert_eq!(string_to_number("0x1f"), 31.0);
        assert!(string_to_number("12px").is_nan());
        assert_eq!(string_to_number("-Infinity"), f64::NEG_INFINITY);
    }

    #[test]
    fn round_trips_json_in_insertion_order() {
        let value = json_parse(r#"{"b": [1, 2.5, "x\n"], "a": {"c": null, "d": true}}"#).unwrap();
        assert_eq!(
            json_stringify(&value, "").unwrap().unwrap(),
            r#"{"b":[1,2.5,"x\n"],"a":{"c":null,"d":true}}"#
        );
        assert_eq!(
            json_stringify(&value, "  ").unwrap().unwrap(),
            "{\n  \"b\": [\n    1,\n    2.5,\n    \"x\\n\"\n  ],\n  \"a\": {\n    \"c\": null,\n    \"d\": true\n  }\n}"
        );
        assert!(json_parse("{\"a\": 1,}").is_err());
        assert!(json_parse("[1] x").is_err());
    }

    #[test]
    fn leaves_out_undefined_and_refuses_cycles() {
        let object = Value::object_from([("a", Value::Undefined), ("b", Value::Number(f64::NAN))]);
        assert_eq!(
            json_stringify(&object, "").unwrap().unwrap(),
            r#"{"b":null}"#
        );
        assert!(json_stringify(&Value::Undefined, "").unwrap().is_none());
        let array = Value::array(vec![]);
        if let Value::Array(inner) = &array {
            inner.items.borrow_mut().push(array.clone());
        }
        assert!(json_stringify(&array, "").is_err());
        assert_eq!(
            to_string(&Value::array(vec![Value::Number(1.0), Value::Null])),
            "1,"
        );
    }
}
