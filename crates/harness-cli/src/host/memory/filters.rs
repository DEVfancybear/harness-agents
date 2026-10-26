//! memory-engine's search filters, on memory entries
//! (`docs/architecture/MEMORY_STORE.en.md` §6.4).
//!
//! memory-engine filters in Postgres: `ltree` lquery and ltxtquery for the
//! tree, `@>` for meta, a SQL/JSON path predicate (`@@`) for `metaPredicate`,
//! and range operators for temporal. ha's stores are small, so the same
//! semantics are evaluated here, over the entries a search considers.

use std::collections::BTreeMap;

use serde_json::{Value, json};

// ---------------------------------------------------------------- tree

/// A tree filter, its syntax detected as memory-engine detects it: a pattern
/// holding `&` is an ltxtquery (Boolean search over labels); one holding any
/// of `* ! { } | @ %` is an lquery; anything else is an exact path, matching
/// that node and every one below it. Labels are joined by `/`, or by `.` as
/// memory-engine's ltree joins them. A pattern without a leading `/` also
/// matches a tree read below its scope's root (`team/decisions` matches
/// `/global/team/decisions`).
#[derive(Clone, Debug)]
pub struct TreeFilter {
    pattern: Pattern,
    relative: bool,
}

#[derive(Clone, Debug)]
enum Pattern {
    Exact(Vec<String>),
    Lquery(Vec<Item>),
    Ltxtquery(Text),
}

/// One label pattern: alternatives, any of which matches a label, with the
/// modifiers ltree allows (`@` any case, `*` prefix, `%` underscore words).
#[derive(Clone, Debug)]
pub struct Label {
    text: String,
    any_case: bool,
    prefix: bool,
    words: bool,
}

/// One item of an lquery: `*` with a count of labels, or label patterns.
#[derive(Clone, Debug)]
pub enum Item {
    Star {
        least: usize,
        most: usize,
    },
    Labels {
        alternatives: Vec<Label>,
        negated: bool,
        least: usize,
        most: usize,
    },
}

/// An ltxtquery.
#[derive(Clone, Debug)]
pub enum Text {
    Word(Label),
    Not(Box<Text>),
    And(Box<Text>, Box<Text>),
    Or(Box<Text>, Box<Text>),
}

const TREE_SYNTAX: &str = "a tree filter is a path (/global/team), an lquery (/global/*/decisions, */!draft/*, /global/*{1}) or a label search (api & !draft); labels are letters, digits, _ and -, joined by / or .";

impl TreeFilter {
    /// # Errors
    /// The pattern is not a path, an lquery or an ltxtquery.
    pub fn parse(pattern: &str) -> Result<Self, String> {
        let pattern = pattern.trim();
        let relative = !pattern.starts_with('/');
        if pattern.contains('&') {
            return Ok(Self {
                pattern: Pattern::Ltxtquery(TextParser::new(pattern).parse()?),
                relative,
            });
        }
        // memory-engine's `work.projects.*` is ha's `work/projects/*`.
        let pattern = if pattern.contains('/') {
            pattern.to_owned()
        } else {
            pattern.replace('.', "/")
        };
        let body = pattern.trim_start_matches('/').trim_end_matches('/');
        if pattern
            .chars()
            .any(|character| "*!{}|@%".contains(character))
        {
            return Ok(Self {
                pattern: Pattern::Lquery(
                    body.split('/')
                        .map(parse_item)
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                relative,
            });
        }
        let labels = body
            .split('/')
            .filter(|label| !label.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if labels.iter().any(|label| !is_label(label)) {
            return Err(TREE_SYNTAX.to_owned());
        }
        Ok(Self {
            pattern: Pattern::Exact(labels),
            relative,
        })
    }

    /// Whether a tree (`/global/team/decisions`) matches.
    #[must_use]
    pub fn matches(&self, tree: &str) -> bool {
        let labels = tree
            .trim_matches('/')
            .split('/')
            .filter(|label| !label.is_empty())
            .collect::<Vec<_>>();
        if self.matches_labels(&labels) {
            return true;
        }
        // A scope's root is `/global`, `/local` or `/projects/<key>`.
        let (["global" | "local", below @ ..] | ["projects", _, below @ ..]) = labels.as_slice()
        else {
            return false;
        };
        self.relative && !below.is_empty() && self.matches_labels(below)
    }

    fn matches_labels(&self, labels: &[&str]) -> bool {
        match &self.pattern {
            Pattern::Exact(path) => {
                labels.len() >= path.len() && path.iter().zip(labels).all(|(a, b)| a == b)
            }
            Pattern::Lquery(items) => {
                let mut memo = BTreeMap::new();
                lquery(items, labels, 0, 0, &mut memo)
            }
            Pattern::Ltxtquery(text) => text.matches(labels),
        }
    }
}

fn is_label(label: &str) -> bool {
    !label.is_empty()
        && label
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

/// An lquery item: `*`, `*{n}`, `*{n,}`, `*{n,m}`, `*{,m}`, or `!a|b@*%{n,m}`.
fn parse_item(item: &str) -> Result<Item, String> {
    let (body, quantifier) = match item.find('{') {
        Some(at) => {
            let close = item
                .strip_suffix('}')
                .ok_or_else(|| TREE_SYNTAX.to_owned())?;
            (&item[..at], Some(&close[at + 1..]))
        }
        None => (item, None),
    };
    let (least, most) = match quantifier {
        None => (usize::MAX, usize::MAX),
        Some(range) => {
            let number = |text: &str| -> Result<Option<usize>, String> {
                let text = text.trim();
                if text.is_empty() {
                    Ok(None)
                } else {
                    text.parse().map(Some).map_err(|_| TREE_SYNTAX.to_owned())
                }
            };
            if let Some((low, high)) = range.split_once(',') {
                (
                    number(low)?.unwrap_or(0),
                    number(high)?.unwrap_or(usize::MAX - 1),
                )
            } else {
                let exact = number(range)?.ok_or_else(|| TREE_SYNTAX.to_owned())?;
                (exact, exact)
            }
        }
    };
    if body == "*" {
        let (least, most) = if least == usize::MAX {
            (0, usize::MAX - 1)
        } else {
            (least, most)
        };
        return Ok(Item::Star { least, most });
    }
    let (negated, body) = match body.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, body),
    };
    let alternatives = body
        .split('|')
        .map(parse_label)
        .collect::<Result<Vec<_>, _>>()?;
    let (least, most) = if least == usize::MAX {
        (1, 1)
    } else {
        (least, most)
    };
    Ok(Item::Labels {
        alternatives,
        negated,
        least,
        most,
    })
}

/// A label pattern with its trailing modifiers, in any order.
fn parse_label(pattern: &str) -> Result<Label, String> {
    let text = pattern.trim_end_matches(['@', '*', '%']);
    let modifiers = &pattern[text.len()..];
    if !is_label(text) {
        return Err(TREE_SYNTAX.to_owned());
    }
    Ok(Label {
        text: text.to_owned(),
        any_case: modifiers.contains('@'),
        prefix: modifiers.contains('*'),
        words: modifiers.contains('%'),
    })
}

impl Label {
    fn matches(&self, label: &str) -> bool {
        let fold = |text: &str| {
            if self.any_case {
                text.to_lowercase()
            } else {
                text.to_owned()
            }
        };
        let (pattern, label) = (fold(&self.text), fold(label));
        let same = |pattern: &str, word: &str| {
            if self.prefix {
                word.starts_with(pattern)
            } else {
                word == pattern
            }
        };
        if self.words {
            // As Postgres' `compare_subnode`: each of the pattern's
            // underscore-separated words matches some word of the label, in
            // any order (`bar%` matches `foo_bar`).
            let words = label
                .split('_')
                .filter(|word| !word.is_empty())
                .collect::<Vec<_>>();
            pattern
                .split('_')
                .filter(|wanted| !wanted.is_empty())
                .all(|wanted| words.iter().any(|word| same(wanted, word)))
        } else {
            same(&pattern, &label)
        }
    }
}

/// Whether `items[item..]` matches `labels[label..]`.
fn lquery(
    items: &[Item],
    labels: &[&str],
    item: usize,
    label: usize,
    memo: &mut BTreeMap<(usize, usize), bool>,
) -> bool {
    if let Some(known) = memo.get(&(item, label)) {
        return *known;
    }
    let result = match items.get(item) {
        None => label == labels.len(),
        Some(Item::Star { least, most }) => (*least..=(*most).min(labels.len() - label))
            .any(|taken| lquery(items, labels, item + 1, label + taken, memo)),
        Some(Item::Labels {
            alternatives,
            negated,
            least,
            most,
        }) => {
            let fits = |index: usize| {
                labels.get(index).is_some_and(|label| {
                    alternatives.iter().any(|pattern| pattern.matches(label)) != *negated
                })
            };
            let mut taken = 0;
            let mut found = false;
            loop {
                if taken >= *least && lquery(items, labels, item + 1, label + taken, memo) {
                    found = true;
                    break;
                }
                if taken >= *most || !fits(label + taken) {
                    break;
                }
                taken += 1;
            }
            found
        }
    };
    memo.insert((item, label), result);
    result
}

impl Text {
    fn matches(&self, labels: &[&str]) -> bool {
        match self {
            Self::Word(pattern) => labels.iter().any(|label| pattern.matches(label)),
            Self::Not(inner) => !inner.matches(labels),
            Self::And(left, right) => left.matches(labels) && right.matches(labels),
            Self::Or(left, right) => left.matches(labels) || right.matches(labels),
        }
    }
}

struct TextParser<'a> {
    tokens: Vec<&'a str>,
    at: usize,
}

impl<'a> TextParser<'a> {
    fn new(pattern: &'a str) -> Self {
        let mut tokens = Vec::new();
        let mut start = None;
        for (index, character) in pattern.char_indices() {
            if "&|!()".contains(character) || character.is_whitespace() {
                if let Some(begin) = start.take() {
                    tokens.push(&pattern[begin..index]);
                }
                if !character.is_whitespace() {
                    tokens.push(&pattern[index..index + character.len_utf8()]);
                }
            } else if start.is_none() {
                start = Some(index);
            }
        }
        if let Some(begin) = start {
            tokens.push(&pattern[begin..]);
        }
        Self { tokens, at: 0 }
    }

    fn parse(mut self) -> Result<Text, String> {
        let text = self.or()?;
        if self.at != self.tokens.len() {
            return Err(TREE_SYNTAX.to_owned());
        }
        Ok(text)
    }

    fn peek(&self) -> Option<&'a str> {
        self.tokens.get(self.at).copied()
    }

    fn or(&mut self) -> Result<Text, String> {
        let mut left = self.and()?;
        while self.peek() == Some("|") {
            self.at += 1;
            left = Text::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Text, String> {
        let mut left = self.not()?;
        while self.peek() == Some("&") {
            self.at += 1;
            left = Text::And(Box::new(left), Box::new(self.not()?));
        }
        Ok(left)
    }

    fn not(&mut self) -> Result<Text, String> {
        match self.peek() {
            Some("!") => {
                self.at += 1;
                Ok(Text::Not(Box::new(self.not()?)))
            }
            Some("(") => {
                self.at += 1;
                let inner = self.or()?;
                if self.peek() != Some(")") {
                    return Err(TREE_SYNTAX.to_owned());
                }
                self.at += 1;
                Ok(inner)
            }
            Some(word) if !"&|)".contains(word) => {
                self.at += 1;
                parse_label(word).map(Text::Word)
            }
            _ => Err(TREE_SYNTAX.to_owned()),
        }
    }
}

// ---------------------------------------------------------------- meta

/// JSON containment as Postgres' `jsonb @>` has it: an object holds every key
/// of the pattern with a value holding the pattern's; an array holds each of
/// the pattern's elements in some element of its own; scalars are equal.
#[must_use]
pub fn contains(value: &Value, pattern: &Value) -> bool {
    match (value, pattern) {
        (Value::Object(value), Value::Object(pattern)) => pattern
            .iter()
            .all(|(key, wanted)| value.get(key).is_some_and(|held| contains(held, wanted))),
        (Value::Array(value), Value::Array(pattern)) => pattern
            .iter()
            .all(|wanted| value.iter().any(|held| contains(held, wanted))),
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (value, pattern) => value == pattern,
    }
}

// ---------------------------------------------------------------- temporal

/// memory-engine's temporal filter: every mode given must hold. Bounds are
/// half-open `[start, end)`; a point in time is `[t, t]`.
#[derive(Clone, Debug, Default)]
pub struct Temporal {
    before: Option<String>,
    after: Option<String>,
    contains: Option<String>,
    overlaps: Option<(String, String)>,
    within: Option<(String, String)>,
}

impl Temporal {
    /// # Errors
    /// Not an object of the five modes, or a time that does not parse.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let fields = value
            .as_object()
            .ok_or("temporal filter is {before, after, contains, overlaps, within}")?;
        let time = |key: &str| -> Result<Option<String>, String> {
            match fields.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(text)) => super::memory_store::instant(text).map(Some),
                Some(_) => Err(format!("temporal {key} is a time")),
            }
        };
        let range = |key: &str| -> Result<Option<(String, String)>, String> {
            match fields.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(value) => super::memory_store::temporal_bounds(value)?
                    .map(Some)
                    .ok_or_else(|| format!("temporal {key} is {{start, end}}")),
            }
        };
        for key in fields.keys() {
            if !["before", "after", "contains", "overlaps", "within"].contains(&key.as_str()) {
                return Err(format!(
                    "temporal filter has no {key:?}; use before, after, contains, overlaps or within"
                ));
            }
        }
        Ok(Self {
            before: time("before")?,
            after: time("after")?,
            contains: time("contains")?,
            overlaps: range("overlaps")?,
            within: range("within")?,
        })
    }

    /// Whether an entry with these bounds matches; one without matches no
    /// mode.
    #[must_use]
    pub fn matches(&self, bounds: Option<&(String, String)>) -> bool {
        let Some((start, end)) = bounds else {
            return false;
        };
        let point = start == end;
        // [start, end) for a period, [t, t] for a point.
        let holds = |time: &str| {
            if point {
                time == start
            } else {
                start.as_str() <= time && time < end.as_str()
            }
        };
        let overlaps = |(from, to): &(String, String)| {
            if point {
                from <= start && (start < to || (from == to && start == from))
            } else if from == to {
                holds(from)
            } else {
                start < to && from < end
            }
        };
        self.before
            .as_ref()
            .is_none_or(|time| if point { start < time } else { end <= time })
            && self.after.as_ref().is_none_or(|time| start > time)
            && self.contains.as_ref().is_none_or(|time| holds(time))
            && self.overlaps.as_ref().is_none_or(overlaps)
            && self.within.as_ref().is_none_or(|(from, to)| {
                from <= start
                    && if point {
                        start < to || from == to
                    } else {
                        end <= to
                    }
            })
    }
}

// ---------------------------------------------------------------- JSON path

/// A Postgres SQL/JSON path predicate, evaluated against an entry's meta as
/// `meta @@ predicate` is, in lax mode: comparisons are existential over the
/// items each side yields, arrays unwrap where a value is wanted, and an error
/// is "unknown", which does not match.
#[derive(Clone, Debug)]
pub struct Predicate(Expr);

#[derive(Clone, Debug, PartialEq)]
enum Expr {
    Root,
    Current,
    Last,
    Literal(Value),
    Path(Box<Expr>, Vec<Step>),
    Neg(Box<Expr>),
    Arith(Box<Expr>, char, Box<Expr>),
    Compare(Box<Expr>, Cmp, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Exists(Box<Expr>),
    IsUnknown(Box<Expr>),
    LikeRegex(Box<Expr>, String, String),
    StartsWith(Box<Expr>, Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, PartialEq)]
enum Step {
    Key(String),
    AnyKey,
    AnyIndex,
    /// `.**`, or `.**{n}`, `.**{n to m}` with `last` for the deepest level.
    Descendants(Option<(Level, Level)>),
    Index(Vec<(Expr, Option<Expr>)>),
    Filter(Expr),
    Method(String),
}

/// A level of `.**{...}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Level {
    At(usize),
    Last,
}

/// The item methods this evaluator knows, as Postgres 17 has them without
/// arguments. Any other is refused when the predicate is parsed: one that
/// passed and matched nothing would read as no entry having the facet.
const METHODS: [&str; 17] = [
    "type",
    "size",
    "double",
    "ceiling",
    "floor",
    "abs",
    "keyvalue",
    "datetime",
    "date",
    "timestamp",
    "timestamp_tz",
    "string",
    "number",
    "decimal",
    "integer",
    "bigint",
    "boolean",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Truth {
    True,
    False,
    Unknown,
}

impl Truth {
    fn of(value: bool) -> Self {
        if value { Self::True } else { Self::False }
    }
}

impl Predicate {
    /// # Errors
    /// The text is not a SQL/JSON path this evaluator knows.
    pub fn parse(text: &str) -> Result<Self, String> {
        let tokens = lex(text)?;
        let mut parser = PathParser { tokens, at: 0 };
        let expr = parser.or()?;
        if parser.at != parser.tokens.len() {
            return Err(format!(
                "meta_predicate: unexpected {:?}",
                parser.tokens[parser.at]
            ));
        }
        Ok(Self(expr))
    }

    /// Whether `meta` matches: the predicate is true, or the path yields the
    /// single value `true`.
    #[must_use]
    pub fn matches(&self, meta: &Value) -> bool {
        if is_predicate(&self.0) {
            truth(&self.0, meta, meta, None) == Truth::True
        } else {
            matches!(
                values(&self.0, meta, meta, None).as_deref(),
                Ok([Value::Bool(true)])
            )
        }
    }
}

fn is_predicate(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Compare(..)
            | Expr::And(..)
            | Expr::Or(..)
            | Expr::Not(..)
            | Expr::Exists(..)
            | Expr::IsUnknown(..)
            | Expr::LikeRegex(..)
            | Expr::StartsWith(..)
    )
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Dollar,
    At,
    Dot,
    DotDot,
    Star,
    Open,
    Close,
    OpenBracket,
    CloseBracket,
    OpenBrace,
    CloseBrace,
    Comma,
    Question,
    Op(&'static str),
    Word(String),
    Str(String),
    Num(f64),
}

#[allow(
    clippy::too_many_lines,
    reason = "one lexer: a character, then the token it starts"
)]
fn lex(text: &str) -> Result<Vec<Token>, String> {
    let characters = text.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < characters.len() {
        let character = characters[at];
        let next = characters.get(at + 1).copied();
        match character {
            c if c.is_whitespace() => at += 1,
            '$' => {
                if next.is_some_and(|next| next.is_alphanumeric() || next == '_') {
                    return Err("meta_predicate: variables ($name) are not supported".to_owned());
                }
                tokens.push(Token::Dollar);
                at += 1;
            }
            '@' => {
                tokens.push(Token::At);
                at += 1;
            }
            '.' if next == Some('*') && characters.get(at + 2) == Some(&'*') => {
                tokens.push(Token::DotDot);
                at += 3;
            }
            '.' if next.is_some_and(|next| next.is_ascii_digit()) => {
                let (number, used) = number(&characters[at..])?;
                tokens.push(Token::Num(number));
                at += used;
            }
            '.' => {
                tokens.push(Token::Dot);
                at += 1;
            }
            '*' => {
                tokens.push(Token::Star);
                at += 1;
            }
            '(' => {
                tokens.push(Token::Open);
                at += 1;
            }
            ')' => {
                tokens.push(Token::Close);
                at += 1;
            }
            '[' => {
                tokens.push(Token::OpenBracket);
                at += 1;
            }
            ']' => {
                tokens.push(Token::CloseBracket);
                at += 1;
            }
            '{' => {
                tokens.push(Token::OpenBrace);
                at += 1;
            }
            '}' => {
                tokens.push(Token::CloseBrace);
                at += 1;
            }
            ',' => {
                tokens.push(Token::Comma);
                at += 1;
            }
            '?' => {
                tokens.push(Token::Question);
                at += 1;
            }
            '"' => {
                let mut text = String::new();
                at += 1;
                loop {
                    match characters.get(at) {
                        None => return Err("meta_predicate: unterminated string".to_owned()),
                        Some('"') => {
                            at += 1;
                            break;
                        }
                        Some('\\') => {
                            let escaped = characters
                                .get(at + 1)
                                .ok_or("meta_predicate: unterminated string")?;
                            if *escaped == 'u' {
                                let code = characters
                                    .get(at + 2..at + 6)
                                    .map(|digits| digits.iter().collect::<String>())
                                    .and_then(|digits| u32::from_str_radix(&digits, 16).ok())
                                    .and_then(char::from_u32)
                                    .ok_or("meta_predicate: \\u takes four hex digits")?;
                                text.push(code);
                                at += 6;
                                continue;
                            }
                            text.push(match escaped {
                                'n' => '\n',
                                't' => '\t',
                                'r' => '\r',
                                'b' => '\u{8}',
                                'f' => '\u{c}',
                                other => *other,
                            });
                            at += 2;
                        }
                        Some(other) => {
                            text.push(*other);
                            at += 1;
                        }
                    }
                }
                tokens.push(Token::Str(text));
            }
            c if c.is_ascii_digit() => {
                let (number, used) = number(&characters[at..])?;
                tokens.push(Token::Num(number));
                at += used;
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = at;
                while characters
                    .get(at)
                    .is_some_and(|c| c.is_alphanumeric() || *c == '_')
                {
                    at += 1;
                }
                tokens.push(Token::Word(characters[start..at].iter().collect()));
            }
            _ => {
                let two = next.map(|next| format!("{character}{next}"));
                let op = ["==", "!=", "<>", "<=", ">=", "&&", "||"]
                    .into_iter()
                    .find(|op| two.as_deref() == Some(*op));
                if let Some(op) = op {
                    tokens.push(Token::Op(op));
                    at += 2;
                } else if let Some(op) = ["<", ">", "!", "+", "-", "/", "%", "="]
                    .into_iter()
                    .find(|op| op.starts_with(character))
                {
                    tokens.push(Token::Op(op));
                    at += 1;
                } else {
                    return Err(format!("meta_predicate: unexpected {character:?}"));
                }
            }
        }
    }
    Ok(tokens)
}

fn number(characters: &[char]) -> Result<(f64, usize), String> {
    let mut used = 0;
    while characters
        .get(used)
        .is_some_and(|c| c.is_ascii_digit() || *c == '.')
    {
        used += 1;
    }
    if matches!(characters.get(used), Some('e' | 'E')) {
        used += 1;
        if matches!(characters.get(used), Some('+' | '-')) {
            used += 1;
        }
        while characters.get(used).is_some_and(char::is_ascii_digit) {
            used += 1;
        }
    }
    let text = characters[..used].iter().collect::<String>();
    text.parse()
        .map(|number| (number, used))
        .map_err(|_| format!("meta_predicate: bad number {text:?}"))
}

struct PathParser {
    tokens: Vec<Token>,
    at: usize,
}

impl PathParser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn eat(&mut self, token: &Token) -> bool {
        if self.peek() == Some(token) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn word(&mut self, word: &str) -> bool {
        if matches!(self.peek(), Some(Token::Word(w)) if w == word) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, token: &Token) -> Result<(), String> {
        if self.eat(token) {
            Ok(())
        } else {
            Err(format!(
                "meta_predicate: expected {token:?}, found {:?}",
                self.peek()
            ))
        }
    }

    fn or(&mut self) -> Result<Expr, String> {
        let mut left = self.and()?;
        while self.eat(&Token::Op("||")) {
            left = Expr::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, String> {
        let mut left = self.not()?;
        while self.eat(&Token::Op("&&")) {
            left = Expr::And(Box::new(left), Box::new(self.not()?));
        }
        Ok(left)
    }

    fn not(&mut self) -> Result<Expr, String> {
        if self.eat(&Token::Op("!")) {
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        let expr = self.predicate()?;
        if self.word("is") {
            if !self.word("unknown") {
                return Err("meta_predicate: expected unknown after is".to_owned());
            }
            return Ok(Expr::IsUnknown(Box::new(expr)));
        }
        Ok(expr)
    }

    fn predicate(&mut self) -> Result<Expr, String> {
        if self.word("exists") {
            self.expect(&Token::Open)?;
            let path = self.additive()?;
            self.expect(&Token::Close)?;
            return Ok(Expr::Exists(Box::new(path)));
        }
        let left = self.additive()?;
        let cmp = match self.peek() {
            Some(Token::Op("==" | "=")) => Some(Cmp::Eq),
            Some(Token::Op("!=" | "<>")) => Some(Cmp::Ne),
            Some(Token::Op("<")) => Some(Cmp::Lt),
            Some(Token::Op("<=")) => Some(Cmp::Le),
            Some(Token::Op(">")) => Some(Cmp::Gt),
            Some(Token::Op(">=")) => Some(Cmp::Ge),
            _ => None,
        };
        if let Some(cmp) = cmp {
            self.at += 1;
            let right = self.additive()?;
            return Ok(Expr::Compare(Box::new(left), cmp, Box::new(right)));
        }
        if self.word("like_regex") {
            let Some(Token::Str(pattern)) = self.peek().cloned() else {
                return Err("meta_predicate: like_regex takes a string".to_owned());
            };
            self.at += 1;
            let mut flags = String::new();
            if self.word("flag") {
                let Some(Token::Str(given)) = self.peek().cloned() else {
                    return Err("meta_predicate: flag takes a string".to_owned());
                };
                self.at += 1;
                flags = given;
            }
            return Ok(Expr::LikeRegex(Box::new(left), pattern, flags));
        }
        if self.word("starts") {
            if !self.word("with") {
                return Err("meta_predicate: expected with after starts".to_owned());
            }
            let right = self.additive()?;
            return Ok(Expr::StartsWith(Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<Expr, String> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Some(Token::Op("+")) => '+',
                Some(Token::Op("-")) => '-',
                _ => return Ok(left),
            };
            self.at += 1;
            left = Expr::Arith(Box::new(left), op, Box::new(self.multiplicative()?));
        }
    }

    fn multiplicative(&mut self) -> Result<Expr, String> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Token::Star) => '*',
                Some(Token::Op("/")) => '/',
                Some(Token::Op("%")) => '%',
                _ => return Ok(left),
            };
            self.at += 1;
            left = Expr::Arith(Box::new(left), op, Box::new(self.unary()?));
        }
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat(&Token::Op("-")) {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.eat(&Token::Op("+")) {
            return self.unary();
        }
        let primary = self.primary()?;
        let steps = self.steps()?;
        Ok(if steps.is_empty() {
            primary
        } else {
            Expr::Path(Box::new(primary), steps)
        })
    }

    fn primary(&mut self) -> Result<Expr, String> {
        let token = self
            .peek()
            .cloned()
            .ok_or("meta_predicate: unexpected end")?;
        self.at += 1;
        Ok(match token {
            Token::Dollar => Expr::Root,
            Token::At => Expr::Current,
            Token::Num(number) => Expr::Literal(json!(number)),
            Token::Str(text) => Expr::Literal(json!(text)),
            Token::Word(word) => match word.as_str() {
                "true" => Expr::Literal(json!(true)),
                "false" => Expr::Literal(json!(false)),
                "null" => Expr::Literal(Value::Null),
                "last" => Expr::Last,
                // A leading lax/strict mode word; lax is what is evaluated.
                "lax" | "strict" => return self.primary(),
                other => return Err(format!("meta_predicate: unexpected {other:?}")),
            },
            Token::Open => {
                let inner = self.or()?;
                self.expect(&Token::Close)?;
                inner
            }
            other => return Err(format!("meta_predicate: unexpected {other:?}")),
        })
    }

    /// A level of `.**{...}`: a number or `last`.
    fn level(&mut self) -> Result<Level, String> {
        if self.word("last") {
            return Ok(Level::Last);
        }
        match self.peek().cloned() {
            Some(Token::Num(number)) if number >= 0.0 && number.fract() == 0.0 => {
                self.at += 1;
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a whole, non-negative level"
                )]
                Ok(Level::At(number as usize))
            }
            other => Err(format!(
                "meta_predicate: a .** level is a number or last, not {other:?}"
            )),
        }
    }

    fn steps(&mut self) -> Result<Vec<Step>, String> {
        let mut steps = Vec::new();
        loop {
            match self.peek().cloned() {
                Some(Token::DotDot) => {
                    self.at += 1;
                    let levels = if self.eat(&Token::OpenBrace) {
                        let from = self.level()?;
                        let to = if self.word("to") { self.level()? } else { from };
                        self.expect(&Token::CloseBrace)?;
                        Some((from, to))
                    } else {
                        None
                    };
                    steps.push(Step::Descendants(levels));
                }
                Some(Token::Dot) => {
                    self.at += 1;
                    match self.peek().cloned() {
                        Some(Token::Star) => {
                            self.at += 1;
                            steps.push(Step::AnyKey);
                        }
                        Some(Token::Str(key)) => {
                            self.at += 1;
                            steps.push(Step::Key(key));
                        }
                        Some(Token::Word(word)) => {
                            self.at += 1;
                            if self.eat(&Token::Open) {
                                if !METHODS.contains(&word.as_str()) {
                                    return Err(format!(
                                        "meta_predicate: .{word}() is not a method this search knows; it knows .{}()",
                                        METHODS.join("(), .")
                                    ));
                                }
                                if !self.eat(&Token::Close) {
                                    return Err(format!(
                                        "meta_predicate: .{word}() takes no arguments here"
                                    ));
                                }
                                steps.push(Step::Method(word));
                            } else {
                                steps.push(Step::Key(word));
                            }
                        }
                        other => {
                            return Err(format!("meta_predicate: unexpected {other:?} after ."));
                        }
                    }
                }
                Some(Token::OpenBracket) => {
                    self.at += 1;
                    if self.eat(&Token::Star) {
                        self.expect(&Token::CloseBracket)?;
                        steps.push(Step::AnyIndex);
                        continue;
                    }
                    let mut subscripts = Vec::new();
                    loop {
                        let from = self.additive()?;
                        let to = if self.word("to") {
                            Some(self.additive()?)
                        } else {
                            None
                        };
                        subscripts.push((from, to));
                        if !self.eat(&Token::Comma) {
                            break;
                        }
                    }
                    self.expect(&Token::CloseBracket)?;
                    steps.push(Step::Index(subscripts));
                }
                Some(Token::Question) => {
                    self.at += 1;
                    self.expect(&Token::Open)?;
                    let filter = self.or()?;
                    self.expect(&Token::Close)?;
                    steps.push(Step::Filter(filter));
                }
                _ => return Ok(steps),
            }
        }
    }
}

type Items = Result<Vec<Value>, ()>;

/// The items an expression yields; `Err` is an error, which a predicate
/// reads as unknown.
fn values(expr: &Expr, root: &Value, current: &Value, last: Option<usize>) -> Items {
    match expr {
        Expr::Root => Ok(vec![root.clone()]),
        Expr::Current => Ok(vec![current.clone()]),
        Expr::Last => last.map(|last| vec![json!(last)]).ok_or(()),
        Expr::Literal(value) => Ok(vec![value.clone()]),
        Expr::Path(start, steps) => {
            let mut items = values(start, root, current, last)?;
            for step in steps {
                items = apply(step, items, root)?;
            }
            Ok(items)
        }
        Expr::Neg(inner) => values(inner, root, current, last)?
            .into_iter()
            .map(|item| item.as_f64().map(|number| json!(-number)).ok_or(()))
            .collect(),
        Expr::Arith(left, op, right) => {
            let single = |expr: &Expr| -> Result<f64, ()> {
                match unwrap(values(expr, root, current, last)?).as_slice() {
                    [Value::Number(number)] => number.as_f64().ok_or(()),
                    _ => Err(()),
                }
            };
            let (a, b) = (single(left)?, single(right)?);
            let result = match op {
                '+' => a + b,
                '-' => a - b,
                '*' => a * b,
                '/' if b != 0.0 => a / b,
                '%' if b != 0.0 => a % b,
                _ => return Err(()),
            };
            Ok(vec![json!(result)])
        }
        predicate => Ok(match truth(predicate, root, current, last) {
            Truth::True => vec![json!(true)],
            Truth::False => vec![json!(false)],
            Truth::Unknown => vec![Value::Null],
        }),
    }
}

/// Lax mode's automatic unwrapping: an array where items are wanted is its
/// elements.
fn unwrap(items: Vec<Value>) -> Vec<Value> {
    items
        .into_iter()
        .flat_map(|item| match item {
            Value::Array(elements) => elements,
            other => vec![other],
        })
        .collect()
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "array subscripts and sizes are small"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one step of a path, each kind its arm"
)]
fn apply(step: &Step, items: Vec<Value>, root: &Value) -> Items {
    let mut out = Vec::new();
    match step {
        Step::Key(key) => {
            for item in unwrap(items) {
                if let Some(value) = item.get(key) {
                    out.push(value.clone());
                }
            }
        }
        Step::AnyKey => {
            for item in unwrap(items) {
                if let Value::Object(fields) = item {
                    out.extend(fields.into_values());
                }
            }
        }
        Step::AnyIndex => out = unwrap(items),
        Step::Descendants(levels) => {
            /// Each value under `value` with its depth below it (0: itself).
            fn walk(value: &Value, depth: usize, out: &mut Vec<(usize, Value)>) {
                out.push((depth, value.clone()));
                match value {
                    Value::Array(items) => {
                        for item in items {
                            walk(item, depth + 1, out);
                        }
                    }
                    Value::Object(fields) => {
                        for item in fields.values() {
                            walk(item, depth + 1, out);
                        }
                    }
                    _ => {}
                }
            }
            for item in &items {
                let mut found = Vec::new();
                walk(item, 0, &mut found);
                let deepest = found.iter().map(|(depth, _)| *depth).max().unwrap_or(0);
                let at = |level: Level| match level {
                    Level::At(depth) => depth,
                    Level::Last => deepest,
                };
                out.extend(
                    found
                        .into_iter()
                        .filter(|(depth, _)| {
                            levels.is_none_or(|(from, to)| (at(from)..=at(to)).contains(depth))
                        })
                        .map(|(_, value)| value),
                );
            }
        }
        Step::Index(subscripts) => {
            for item in items {
                let elements = match item {
                    Value::Array(elements) => elements,
                    other => vec![other],
                };
                let last = elements.len().checked_sub(1);
                for (from, to) in subscripts {
                    let index = |expr: &Expr| -> Result<i64, ()> {
                        match values(expr, root, root, last)?.as_slice() {
                            [Value::Number(number)] => number
                                .as_f64()
                                .map(|number| number.trunc() as i64)
                                .ok_or(()),
                            _ => Err(()),
                        }
                    };
                    let start = index(from)?;
                    let end = match to {
                        Some(to) => index(to)?,
                        None => start,
                    };
                    for position in start.max(0)..=end {
                        if let Some(element) = elements.get(position as usize) {
                            out.push(element.clone());
                        }
                    }
                }
            }
        }
        Step::Filter(predicate) => {
            for item in unwrap(items) {
                if truth(predicate, root, &item, None) == Truth::True {
                    out.push(item);
                }
            }
        }
        Step::Method(method) => {
            for item in items {
                let numbers = |item: &Value| -> Result<f64, ()> { item.as_f64().ok_or(()) };
                out.push(match method.as_str() {
                    "size" => json!(item.as_array().map_or(1, Vec::len)),
                    "type" => json!(match &item {
                        Value::Null => "null",
                        Value::Bool(_) => "boolean",
                        Value::Number(_) => "number",
                        Value::String(_) => "string",
                        Value::Array(_) => "array",
                        Value::Object(_) => "object",
                    }),
                    "double" => match &item {
                        Value::Number(number) => json!(number.as_f64().ok_or(())?),
                        Value::String(text) => json!(text.trim().parse::<f64>().map_err(|_| ())?),
                        _ => return Err(()),
                    },
                    "ceiling" => json!(numbers(&item)?.ceil()),
                    "floor" => json!(numbers(&item)?.floor()),
                    "abs" => json!(numbers(&item)?.abs()),
                    "datetime" | "date" | "timestamp" | "timestamp_tz" => {
                        json!(datetime(method, &item)?)
                    }
                    "string" => match &item {
                        Value::String(text) => json!(text),
                        Value::Number(number) => json!(number.to_string()),
                        Value::Bool(value) => json!(value.to_string()),
                        _ => return Err(()),
                    },
                    "number" | "decimal" => match &item {
                        Value::Number(_) => item.clone(),
                        Value::String(text) => json!(text.trim().parse::<f64>().map_err(|_| ())?),
                        _ => return Err(()),
                    },
                    "integer" | "bigint" => {
                        let number = match &item {
                            Value::Number(number) => number.as_f64().ok_or(())?,
                            Value::String(text) => text.trim().parse::<f64>().map_err(|_| ())?,
                            _ => return Err(()),
                        };
                        if !number.is_finite() {
                            return Err(());
                        }
                        json!(number.round() as i64)
                    }
                    "boolean" => match &item {
                        Value::Bool(_) => item.clone(),
                        Value::Number(number) => match number.as_i64() {
                            Some(0) => json!(false),
                            Some(_) => json!(true),
                            None => return Err(()),
                        },
                        // Postgres' boolean input, which this method reads.
                        Value::String(text) => match text.trim().to_lowercase().as_str() {
                            "t" | "true" | "y" | "yes" | "on" | "1" => json!(true),
                            "f" | "false" | "n" | "no" | "off" | "0" => json!(false),
                            _ => return Err(()),
                        },
                        _ => return Err(()),
                    },
                    "keyvalue" => {
                        let Value::Object(fields) = &item else {
                            return Err(());
                        };
                        for (key, value) in fields {
                            out.push(json!({"key": key, "value": value, "id": 0}));
                        }
                        continue;
                    }
                    _ => return Err(()),
                });
            }
        }
    }
    Ok(out)
}

/// `.datetime()` and its kin: an ISO 8601 date or time as the store keeps
/// times (UTC, RFC 3339 with milliseconds), so two compare as text in time
/// order; `.date()` keeps the day.
fn datetime(method: &str, item: &Value) -> Result<String, ()> {
    let Value::String(text) = item else {
        return Err(());
    };
    let text = text.trim();
    let instant = super::memory_store::instant(text)
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f"))
                .map(|time| {
                    time.and_utc()
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                })
        })
        .map_err(|_| ())?;
    Ok(if method == "date" {
        format!("{}T00:00:00.000Z", &instant[..10])
    } else {
        instant
    })
}

fn compare(left: &Value, cmp: Cmp, right: &Value) -> Result<bool, ()> {
    use std::cmp::Ordering;
    let order = match (left, right) {
        (Value::Number(a), Value::Number(b)) => a.as_f64().partial_cmp(&b.as_f64()).ok_or(())?,
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Null, Value::Null) => Ordering::Equal,
        // Null compares with anything as unequal, never as an error.
        (Value::Null, _) | (_, Value::Null) => {
            return Ok(matches!(cmp, Cmp::Ne));
        }
        _ => return Err(()),
    };
    Ok(match cmp {
        Cmp::Eq => order == Ordering::Equal,
        Cmp::Ne => order != Ordering::Equal,
        Cmp::Lt => order == Ordering::Less,
        Cmp::Le => order != Ordering::Greater,
        Cmp::Gt => order == Ordering::Greater,
        Cmp::Ge => order != Ordering::Less,
    })
}

/// A predicate's three-valued truth: true when any pair of items satisfies
/// it, unknown when none does and some pair was an error, else false.
fn truth(expr: &Expr, root: &Value, current: &Value, last: Option<usize>) -> Truth {
    let items = |expr: &Expr| values(expr, root, current, last).map(unwrap);
    match expr {
        Expr::And(left, right) => match (
            truth(left, root, current, last),
            truth(right, root, current, last),
        ) {
            (Truth::False, _) | (_, Truth::False) => Truth::False,
            (Truth::True, Truth::True) => Truth::True,
            _ => Truth::Unknown,
        },
        Expr::Or(left, right) => match (
            truth(left, root, current, last),
            truth(right, root, current, last),
        ) {
            (Truth::True, _) | (_, Truth::True) => Truth::True,
            (Truth::False, Truth::False) => Truth::False,
            _ => Truth::Unknown,
        },
        Expr::Not(inner) => match truth(inner, root, current, last) {
            Truth::True => Truth::False,
            Truth::False => Truth::True,
            Truth::Unknown => Truth::Unknown,
        },
        Expr::IsUnknown(inner) => Truth::of(truth(inner, root, current, last) == Truth::Unknown),
        Expr::Exists(path) => match values(path, root, current, last) {
            Ok(found) => Truth::of(!found.is_empty()),
            Err(()) => Truth::Unknown,
        },
        Expr::Compare(left, cmp, right) => {
            let (Ok(left), Ok(right)) = (items(left), items(right)) else {
                return Truth::Unknown;
            };
            let mut error = false;
            for a in &left {
                for b in &right {
                    match compare(a, *cmp, b) {
                        Ok(true) => return Truth::True,
                        Ok(false) => {}
                        Err(()) => error = true,
                    }
                }
            }
            if error { Truth::Unknown } else { Truth::False }
        }
        Expr::LikeRegex(subject, pattern, flags) => {
            let Ok(subject) = items(subject) else {
                return Truth::Unknown;
            };
            let mut builder = regex::RegexBuilder::new(&if flags.contains('q') {
                regex::escape(pattern)
            } else {
                pattern.clone()
            });
            builder
                .case_insensitive(flags.contains('i'))
                .dot_matches_new_line(flags.contains('s'))
                .multi_line(flags.contains('m'))
                .ignore_whitespace(flags.contains('x'));
            let Ok(regex) = builder.build() else {
                return Truth::Unknown;
            };
            let mut error = false;
            for item in &subject {
                match item {
                    Value::String(text) if regex.is_match(text) => return Truth::True,
                    Value::String(_) => {}
                    _ => error = true,
                }
            }
            if error { Truth::Unknown } else { Truth::False }
        }
        Expr::StartsWith(subject, prefix) => {
            let (Ok(subject), Ok(prefix)) = (items(subject), items(prefix)) else {
                return Truth::Unknown;
            };
            let mut error = false;
            for item in &subject {
                for wanted in &prefix {
                    match (item, wanted) {
                        (Value::String(text), Value::String(wanted))
                            if text.starts_with(wanted.as_str()) =>
                        {
                            return Truth::True;
                        }
                        (Value::String(_), Value::String(_)) => {}
                        _ => error = true,
                    }
                }
            }
            if error { Truth::Unknown } else { Truth::False }
        }
        // A path or a value inside a filter: true when it yields true.
        other => match values(other, root, current, last) {
            Ok(found) => Truth::of(found.contains(&Value::Bool(true))),
            Err(()) => Truth::Unknown,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(pattern: &str, tree: &str) -> bool {
        TreeFilter::parse(pattern).expect(pattern).matches(tree)
    }

    /// The examples of memory-engine's tree filter syntax.
    #[test]
    fn tree_filters_follow_ltree() {
        assert!(tree("/work/projects", "/work/projects/api/auth"));
        assert!(tree("/work/projects", "/work/projects"));
        assert!(!tree("/work/projects", "/work/projectsx"));
        assert!(tree("/work/projects/*", "/work/projects/api"));
        assert!(tree("/work/*{1}", "/work/projects"));
        assert!(!tree("/work/*{1}", "/work/projects/api"));
        assert!(tree("/work/*{2,4}", "/work/a/b"));
        assert!(!tree("/work/*{2,4}", "/work/a"));
        assert!(tree("*/api/*", "/work/projects/api/auth"));
        assert!(!tree("*/api/*", "/work/projects/apix"));
        assert!(tree("*/!draft/*", "/work/projects/api"));
        assert!(!tree("*/!draft/*", "/draft"));
        assert!(tree("/work|personal/*", "/personal/reading"));
        assert!(!tree("/work|personal/*", "/share/reading"));
        assert!(tree("/work/!archived/*{0,}", "/work/projects/api"));
        assert!(!tree("/work/!archived/*{0,}", "/work/archived/old"));
        assert!(tree("api & auth", "/work/api/auth"));
        assert!(!tree("api & auth", "/work/api"));
        assert!(tree("api & !draft", "/work/api"));
        assert!(!tree("api & !draft", "/work/api/draft"));
        assert!(tree("*/Api@/*", "/work/api"));
        assert!(tree("*/proj*/*", "/work/projects"));
        assert!(tree("*/foo_bar%/*", "/x/foo_bar_baz"));
        assert!(!tree("*/foo_bar%/*", "/x/foo_barbaz"));
        assert!(TreeFilter::parse("/work/a b").is_err());
    }

    #[test]
    fn meta_containment_is_jsonb_containment() {
        let meta = json!({"type": "decision", "tags": ["a", "b"], "owner": {"team": "api", "lead": "lan"}});
        assert!(contains(&meta, &json!({"type": "decision"})));
        assert!(contains(&meta, &json!({"tags": ["b"]})));
        assert!(contains(&meta, &json!({"owner": {"team": "api"}})));
        assert!(!contains(&meta, &json!({"tags": ["c"]})));
        assert!(!contains(&meta, &json!({"type": "note"})));
        assert!(contains(&json!({"n": 3}), &json!({"n": 3.0})));
    }

    #[test]
    fn temporal_modes_follow_half_open_ranges() {
        let period = (
            "2026-03-01T00:00:00.000Z".to_owned(),
            "2026-04-01T00:00:00.000Z".to_owned(),
        );
        let point = (
            "2026-03-15T00:00:00.000Z".to_owned(),
            "2026-03-15T00:00:00.000Z".to_owned(),
        );
        let filter = |value: Value| Temporal::parse(&value).expect("temporal");
        assert!(filter(json!({"before": "2026-04-01"})).matches(Some(&period)));
        assert!(!filter(json!({"before": "2026-03-31"})).matches(Some(&period)));
        assert!(filter(json!({"after": "2026-02-28"})).matches(Some(&period)));
        assert!(!filter(json!({"after": "2026-03-01"})).matches(Some(&period)));
        assert!(filter(json!({"contains": "2026-03-01"})).matches(Some(&period)));
        assert!(!filter(json!({"contains": "2026-04-01"})).matches(Some(&period)));
        assert!(
            filter(json!({"overlaps": {"start": "2026-03-31", "end": "2026-05-01"}}))
                .matches(Some(&period))
        );
        assert!(
            !filter(json!({"overlaps": {"start": "2026-04-01", "end": "2026-05-01"}}))
                .matches(Some(&period))
        );
        assert!(
            filter(json!({"within": {"start": "2026-01-01", "end": "2026-12-31"}}))
                .matches(Some(&period))
        );
        assert!(
            !filter(json!({"within": {"start": "2026-03-02", "end": "2026-12-31"}}))
                .matches(Some(&period))
        );
        assert!(filter(json!({"contains": "2026-03-15"})).matches(Some(&point)));
        assert!(
            filter(json!({"within": {"start": "2026-03-01", "end": "2026-04-01"}}))
                .matches(Some(&point))
        );
        // Both cutoffs: wholly between them.
        assert!(
            filter(json!({"after": "2026-02-01", "before": "2026-05-01"})).matches(Some(&period))
        );
        assert!(!filter(json!({"before": "2027-01-01"})).matches(None));
        assert!(Temporal::parse(&json!({"during": "2026"})).is_err());
    }

    fn predicate(text: &str, meta: &Value) -> bool {
        Predicate::parse(text).expect(text).matches(meta)
    }

    /// The predicates memory-engine's documentation shows, and the rest of
    /// the dialect it names: Boolean logic, missing keys, regular
    /// expressions, cardinality, arithmetic and cross-field conditions.
    #[test]
    fn meta_predicates_follow_sql_json_path() {
        let meta = json!({
            "priority": 4, "status": "pending", "tags": ["a", "b", "c"],
            "grants": [{"user": "tom", "level": 2}, {"user": "ann", "level": 1}],
            "budget": 10, "spent": 7, "name": "Queue choice", "active": true
        });
        assert!(predicate("$.priority >= 3", &meta));
        assert!(predicate("$.priority >= 3 && !exists($.archivedAt)", &meta));
        assert!(!predicate(
            "$.priority >= 3 && exists($.status)  && $.status == \"active\"",
            &meta
        ));
        assert!(predicate(
            "$.status == \"active\" || $.status == \"pending\"",
            &meta
        ));
        assert!(predicate("!exists($.archivedAt)", &meta));
        assert!(predicate(
            "exists($.grants[*] ? (@.user == \"tom\" && @.level >= 2))",
            &meta
        ));
        assert!(!predicate(
            "exists($.grants[*] ? (@.user == \"ann\" && @.level >= 2))",
            &meta
        ));
        assert!(predicate("$.tags.size() == 3", &meta));
        assert!(predicate("$.tags[last] == \"c\"", &meta));
        assert!(predicate("$.tags[0 to 1] == \"b\"", &meta));
        assert!(predicate("$.budget - $.spent < 5", &meta));
        assert!(predicate("$.spent * 2 > $.budget", &meta));
        assert!(predicate("$.name like_regex \"^queue\" flag \"i\"", &meta));
        assert!(predicate("$.name starts with \"Queue\"", &meta));
        assert!(predicate("$.priority.type() == \"number\"", &meta));
        assert!(
            predicate("$.tags == \"b\"", &meta),
            "existential over items"
        );
        assert!(predicate("$.active", &meta));
        assert!(predicate("($.status == 1) is unknown", &meta));
        assert!(
            !predicate("$.status == 1", &meta),
            "an error does not match"
        );
        assert!(predicate("$.** ? (@ == \"ann\") == \"ann\"", &meta));
        assert!(Predicate::parse("$.a ==").is_err());
        assert!(Predicate::parse("$x == 1").is_err());
    }

    /// Postgres 17's conversion methods, `.**` by level and `\u` escapes; a
    /// method this evaluator does not know is refused, never read as no
    /// match.
    #[test]
    fn methods_levels_and_escapes() {
        let meta = json!({
            "due": "2026-10-30T12:00:00Z",
            "day": "2026-10-30",
            "count": "42",
            "ratio": 2.5,
            "flag": "yes",
            "owner": {"team": {"lead": "lan"}},
            "name": "Café",
        });
        assert!(predicate(
            "$.due.datetime() < \"2026-11-01\".datetime()",
            &meta
        ));
        assert!(!predicate(
            "$.due.datetime() < \"2026-10-30\".datetime()",
            &meta
        ));
        assert!(predicate(
            "$.day.date() == \"2026-10-30T09:00:00Z\".date()",
            &meta
        ));
        assert!(predicate("$.count.number() > 40", &meta));
        assert!(predicate("$.count.integer() == 42", &meta));
        assert!(predicate("$.ratio.integer() == 3", &meta));
        assert!(predicate("$.ratio.string() == \"2.5\"", &meta));
        assert!(predicate("$.flag.boolean() == true", &meta));
        assert!(predicate("$.owner.**{2} == \"lan\"", &meta));
        assert!(!predicate("$.owner.**{1} == \"lan\"", &meta));
        assert!(predicate("$.owner.**{1 to last} == \"lan\"", &meta));
        assert!(predicate("$.name == \"Caf\\u00e9\"", &meta));
        let unknown = Predicate::parse("$.due.timestamp_tzz() > 1").expect_err("unknown method");
        assert!(unknown.contains("datetime()"), "{unknown}");
        assert!(Predicate::parse("$.due.datetime(\"YYYY\") > 1").is_err());
    }

    /// `%` as Postgres has it: each word of the pattern matches some word of
    /// the label, in any order. Labels may be joined by dots, and a pattern
    /// without a leading slash reads a tree below its scope's root.
    #[test]
    fn words_dots_and_relative_trees() {
        assert!(tree("/global/bar%", "/global/foo_bar"));
        assert!(tree("/global/baz_foo%", "/global/foo_bar_baz"));
        assert!(!tree("/global/qux%", "/global/foo_bar"));
        assert!(tree("/global/ba*%", "/global/foo_bar"));
        assert!(tree("work.projects", "/global/work/projects/api"));
        assert!(tree("work.projects.*", "/projects/p-1/work/projects/api"));
        assert!(tree("*.api.*", "/local/work/api/notes"));
        assert!(!tree("/work/projects", "/global/work/projects"));
        assert!(!tree("work.projects", "/global/team/work/projects"));
    }
}
