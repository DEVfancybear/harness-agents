//! Syntax highlighting for code in the TUI, as prime-agent highlights it.
//!
//! prime-agent colours code through highlight.js (`theme.ts` `highlightCode`): the
//! colours are the theme's own `syntax*` tokens, not a highlighter theme, and a
//! block is highlighted only when a language is named - by a fence label or a
//! file path - never guessed, because guessing colours prose as code. This module
//! does the same with syntect and bat's grammar set: a grammar gives each piece of
//! text its `TextMate` scopes, the scopes are reduced to one [`Kind`], and the theme
//! decides what a kind looks like ([`style`]). `NO_COLOR` therefore needs nothing
//! here: the plain theme styles every kind the same.
//!
//! Parsing is cached line by line, keyed by the language and every line before it.
//! A streaming answer is re-rendered on every frame; with the cache only the lines
//! that are new since the last frame are parsed.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};

use ratatui::style::Style;
use syntect::easy::ScopeRangeIterator;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

use super::theme::Theme;

/// What a piece of code is, for colouring. prime-agent's `syntax*` theme tokens,
/// plus the diff and heading markup a `diff` or `markdown` block carries.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Kind {
    Plain,
    Comment,
    Keyword,
    Function,
    Variable,
    String,
    Number,
    Type,
    Operator,
    Punctuation,
    Inserted,
    Deleted,
    Heading,
}

/// A grammar the highlighter knows.
#[derive(Clone, Copy)]
pub struct Language(&'static SyntaxReference);

impl Language {
    /// The grammar's name, as bat lists it.
    #[must_use]
    pub fn name(self) -> &'static str {
        &self.0.name
    }
}

impl std::fmt::Debug for Language {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// bat's grammars, loaded once. Lines are fed without their newline.
fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_no_newlines)
}

/// Load the grammars on a background thread, so the first code block does not
/// wait for them.
pub fn warm_up() {
    let _ = std::thread::Builder::new()
        .name("ha-highlight".to_owned())
        .spawn(|| {
            syntaxes();
        });
}

/// The language a fence label names: its first word, as a grammar name or a file
/// extension (```` ```rust ````, ```` ```rs ````, ```` ```py title="x" ````).
#[must_use]
pub fn language_for_label(label: &str) -> Option<Language> {
    let token = label
        .split(|character: char| character.is_whitespace() || character == ',')
        .next()?
        .trim_start_matches(['{', '.'])
        .trim_end_matches('}');
    if token.is_empty() {
        return None;
    }
    let set = syntaxes();
    set.find_syntax_by_token(token)
        .or_else(|| set.find_syntax_by_extension(&token.to_ascii_lowercase()))
        .filter(|syntax| syntax.name != "Plain Text")
        .map(Language)
}

/// The language a file path is written in: the grammar that claims its whole name
/// (`Makefile`, `Dockerfile`) or its extension. The file is never opened.
#[must_use]
pub fn language_for_path(path: &str) -> Option<Language> {
    let name = path
        .trim()
        .trim_matches(['"', '\''])
        .rsplit(['/', '\\'])
        .next()?;
    if name.is_empty() {
        return None;
    }
    let set = syntaxes();
    set.find_syntax_by_extension(name)
        .or_else(|| {
            let (_, extension) = name.rsplit_once('.')?;
            set.find_syntax_by_extension(extension)
                .or_else(|| set.find_syntax_by_extension(&extension.to_ascii_lowercase()))
        })
        .filter(|syntax| syntax.name != "Plain Text")
        .map(Language)
}

/// One run of a line: its text and what it is.
pub type Run = (Kind, String);

/// A line longer than this is left plain: a grammar's regexes on a minified file
/// can take long enough to stall a frame, and such a line is unreadable anyway.
const MAX_LINE_BYTES: usize = 4096;

/// Cached lines past this are dropped all at once; the cache refills from what is
/// on screen.
const MAX_CACHED_LINES: usize = 50_000;

struct Cached {
    state: ParseState,
    stack: ScopeStack,
    runs: Vec<Run>,
}

#[derive(Default)]
struct Cache {
    lines: HashMap<u64, Cached>,
    kinds: HashMap<Scope, Option<Kind>>,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

fn key(previous: u64, line: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    previous.hash(&mut hasher);
    line.hash(&mut hasher);
    hasher.finish()
}

/// Highlight consecutive lines of one file or block: a string or comment that
/// opens on one line carries on to the next.
#[must_use]
pub fn highlight(language: Language, lines: &[&str]) -> Vec<Vec<Run>> {
    run_lines(language, lines, true)
}

/// Highlight lines that do not follow each other - the changed lines of a diff -
/// each from a fresh start, as prime-agent highlights a diff line by line.
#[must_use]
pub fn highlight_each(language: Language, lines: &[&str]) -> Vec<Vec<Run>> {
    run_lines(language, lines, false)
}

fn run_lines(language: Language, lines: &[&str], continuous: bool) -> Vec<Vec<Run>> {
    let set = syntaxes();
    let Ok(mut cache) = cache().lock() else {
        return lines.iter().map(|line| plain(line)).collect();
    };
    if cache.lines.len() > MAX_CACHED_LINES {
        cache.lines.clear();
    }
    let start = key(u64::from(continuous), language.name());
    let fresh = || (ParseState::new(language.0), ScopeStack::new());
    let mut previous = start;
    let (mut state, mut stack) = fresh();
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        if !continuous {
            previous = start;
            (state, stack) = fresh();
        }
        let id = key(previous, line);
        previous = id;
        if let Some(hit) = cache.lines.get(&id) {
            state = hit.state.clone();
            stack = hit.stack.clone();
            out.push(hit.runs.clone());
            continue;
        }
        let runs = if line.len() > MAX_LINE_BYTES {
            plain(line)
        } else {
            parse_line(&mut state, &mut stack, line, set, &mut cache.kinds)
                .unwrap_or_else(|| plain(line))
        };
        cache.lines.insert(
            id,
            Cached {
                state: state.clone(),
                stack: stack.clone(),
                runs: runs.clone(),
            },
        );
        out.push(runs);
    }
    out
}

fn plain(line: &str) -> Vec<Run> {
    vec![(Kind::Plain, (*line).to_owned())]
}

/// One line's runs, or `None` when the grammar fails on it; the state is then
/// left as it was, so the next line starts from where this one did.
fn parse_line(
    state: &mut ParseState,
    stack: &mut ScopeStack,
    line: &str,
    set: &SyntaxSet,
    kinds: &mut HashMap<Scope, Option<Kind>>,
) -> Option<Vec<Run>> {
    let mut next_state = state.clone();
    let mut next_stack = stack.clone();
    let operations = next_state.parse_line(line, set).ok()?;
    let mut runs: Vec<Run> = Vec::new();
    for (range, operation) in ScopeRangeIterator::new(&operations, line) {
        next_stack.apply(operation).ok()?;
        if range.is_empty() {
            continue;
        }
        let kind = classify(next_stack.as_slice(), kinds);
        let text = &line[range];
        match runs.last_mut() {
            Some((last, run)) if *last == kind => run.push_str(text),
            _ => runs.push((kind, text.to_owned())),
        }
    }
    *state = next_state;
    *stack = next_stack;
    Some(runs)
}

/// The kind of text under a scope stack: the innermost scope that says what it is.
/// Punctuation only counts when nothing around it does, so a string's quotes are
/// string and a comment's `//` is comment.
fn classify(stack: &[Scope], kinds: &mut HashMap<Scope, Option<Kind>>) -> Kind {
    let mut punctuation = false;
    for scope in stack.iter().rev() {
        let kind = *kinds
            .entry(*scope)
            .or_insert_with(|| kind_of(&scope.build_string()));
        match kind {
            Some(Kind::Punctuation) => punctuation = true,
            Some(kind) => return kind,
            None => {}
        }
    }
    if punctuation {
        Kind::Punctuation
    } else {
        Kind::Plain
    }
}

/// `TextMate`'s standard scope names, most specific first. These are the names
/// every grammar shares, so no language needs a rule of its own.
const RULES: &[(&str, Kind)] = &[
    ("comment", Kind::Comment),
    ("string", Kind::String),
    ("constant.character.escape", Kind::String),
    ("constant", Kind::Number),
    ("keyword.operator", Kind::Operator),
    ("keyword", Kind::Keyword),
    ("storage", Kind::Keyword),
    ("variable.language", Kind::Keyword),
    ("variable.function", Kind::Function),
    ("entity.name.function", Kind::Function),
    ("support.function", Kind::Function),
    ("entity.name.tag", Kind::Keyword),
    ("entity.other.attribute-name", Kind::Variable),
    ("entity.name.section", Kind::Heading),
    ("entity.name", Kind::Type),
    ("entity.other.inherited-class", Kind::Type),
    ("support.type", Kind::Type),
    ("support.class", Kind::Type),
    ("variable", Kind::Variable),
    ("markup.inserted", Kind::Inserted),
    ("markup.deleted", Kind::Deleted),
    ("markup.heading", Kind::Heading),
    ("meta.diff.range", Kind::Heading),
    ("meta.diff.header", Kind::Comment),
    ("punctuation", Kind::Punctuation),
];

fn kind_of(scope: &str) -> Option<Kind> {
    RULES.iter().find_map(|(prefix, kind)| {
        let matches = scope == *prefix
            || scope
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('.'));
        matches.then_some(*kind)
    })
}

/// How the theme draws a kind; `plain` is the style of text no scope names.
#[must_use]
pub fn style(kind: Kind, plain: Style, theme: &Theme) -> Style {
    let syntax = &theme.syntax;
    match kind {
        Kind::Plain => plain,
        Kind::Comment => syntax.comment,
        Kind::Keyword => syntax.keyword,
        Kind::Function => syntax.function,
        Kind::Variable => syntax.variable,
        Kind::String => syntax.string,
        Kind::Number => syntax.number,
        Kind::Type => syntax.kind,
        Kind::Operator => syntax.operator,
        Kind::Punctuation => syntax.punctuation,
        Kind::Inserted => theme.diff_added,
        Kind::Deleted => theme.diff_removed,
        Kind::Heading => theme.md_heading,
    }
}

#[cfg(test)]
mod tests {
    use super::{Kind, Language, highlight, highlight_each, language_for_label, language_for_path};

    fn kinds_of(runs: &[(Kind, String)], text: &str) -> Vec<Kind> {
        runs.iter()
            .filter(|(_, run)| run.contains(text))
            .map(|(kind, _)| *kind)
            .collect()
    }

    #[test]
    fn a_fence_label_or_a_path_names_the_language() {
        assert_eq!(language_for_label("rust").map(Language::name), Some("Rust"));
        assert_eq!(language_for_label("rs").map(Language::name), Some("Rust"));
        assert_eq!(
            language_for_label("py title=\"x.py\"").map(Language::name),
            Some("Python")
        );
        assert!(
            language_for_label("typescript").is_some(),
            "bat's extra set"
        );
        assert!(language_for_label("toml").is_some(), "bat's extra set");
        assert!(language_for_label("").is_none());
        assert!(language_for_label("not-a-language").is_none());
        assert_eq!(
            language_for_path("crates/app/src/main.rs").map(Language::name),
            Some("Rust")
        );
        assert_eq!(
            language_for_path(r"C:\work\Cargo.toml").map(Language::name),
            Some("TOML")
        );
        assert!(language_for_path("Dockerfile").is_some());
        assert!(language_for_path("notes.unknownext").is_none());
        assert!(language_for_path("README").is_none());
    }

    #[test]
    fn code_is_split_into_the_kinds_the_theme_colours() {
        let rust = language_for_label("rust").expect("rust");
        let lines = highlight(rust, &["fn main() { let s = \"hi\"; // note", "}"]);
        let first = &lines[0];
        assert_eq!(
            first
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<String>(),
            "fn main() { let s = \"hi\"; // note",
            "highlighting never changes the text"
        );
        assert!(kinds_of(first, "fn").contains(&Kind::Keyword));
        assert!(kinds_of(first, "main").contains(&Kind::Function));
        assert!(kinds_of(first, "\"hi\"").contains(&Kind::String));
        assert!(kinds_of(first, "// note").contains(&Kind::Comment));
    }

    #[test]
    fn a_comment_opened_on_one_line_carries_on_to_the_next() {
        let rust = language_for_label("rust").expect("rust");
        let lines = highlight(rust, &["/* start", "still comment */ let x = 1;"]);
        assert_eq!(lines[1][0], (Kind::Comment, "still comment */".to_owned()));
        // Line by line, the second line starts fresh.
        let each = highlight_each(rust, &["/* start", "still comment */"]);
        assert_ne!(each[1][0].0, Kind::Comment);
    }

    #[test]
    fn a_cached_block_highlights_the_same_as_a_fresh_one() {
        let python = language_for_label("python").expect("python");
        let text = ["def f(x):", "    return x + 1  # add"];
        assert_eq!(highlight(python, &text), highlight(python, &text));
        // A block that grows (a streaming answer) keeps its earlier lines.
        let longer = highlight(python, &[text[0], text[1], "print(f(2))"]);
        assert_eq!(&longer[..2], &highlight(python, &text)[..]);
    }
}
