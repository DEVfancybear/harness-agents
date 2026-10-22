//! What `search_text` adds around the search itself, after fsearch's `grep`
//! and `multi_grep` for coding agents:
//!
//! - `any_of`: more literal spellings searched together with the query (OR),
//!   so `prepareUpload` and `prepare_upload` take one call, not three;
//! - definitions first: the lines that declare the searched name lead the
//!   result, so the model reads where a thing is defined before its uses;
//! - another spelling when nothing matched: an identifier that matches
//!   nowhere is searched again in its other spellings (`snake_case`,
//!   `camelCase`, `kebab-case`, `SCREAMING_CASE`) and case, and the result says
//!   so - before, "0 matches" left the model to guess the spelling itself.

use std::path::Path;

use harness_types::{ErrorCode, HarnessError};

use crate::workspace::{SearchOutput, search_text};

/// One `search_text` call's arguments.
pub(crate) struct Lookup<'a> {
    pub query: &'a str,
    pub any_of: &'a [String],
    pub path: Option<&'a str>,
    pub regex: bool,
    pub case_insensitive: bool,
    pub glob: Option<&'a str>,
    pub context_lines: u32,
    pub anchors: bool,
}

/// The pattern `query` and `any_of` make: one regex OR-ing their literal text.
fn any_of_pattern(texts: &[&str]) -> String {
    texts
        .iter()
        .map(|text| regex::escape(text))
        .collect::<Vec<_>>()
        .join("|")
}

/// `any_of` is literal text; it does not combine with a regex query.
pub(crate) fn validate_any_of(regex: bool, any_of: &[String]) -> Result<(), HarnessError> {
    if regex && !any_of.is_empty() {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "search_text any_of takes literal strings; it does not combine with regex: true (put the alternation in the regex instead)",
        ));
    }
    Ok(())
}

/// Search, then put definitions first; an identifier that matched nothing is
/// tried in its other spellings, with a note saying so.
pub(crate) fn lookup(
    root: &Path,
    request: &Lookup<'_>,
) -> Result<(SearchOutput, Option<String>), HarnessError> {
    validate_any_of(request.regex, request.any_of)?;
    let mut texts = vec![request.query];
    texts.extend(
        request
            .any_of
            .iter()
            .map(String::as_str)
            .filter(|text| *text != request.query),
    );
    let (pattern, regex) = if texts.len() > 1 {
        (any_of_pattern(&texts), true)
    } else {
        (request.query.to_owned(), request.regex)
    };
    let search = |pattern: &str, regex: bool, case_insensitive: bool| {
        search_text(
            root,
            pattern,
            request.path,
            regex,
            case_insensitive,
            request.glob,
            request.context_lines,
            request.anchors,
        )
    };
    let mut output = search(&pattern, regex, request.case_insensitive)?;
    let mut names = texts
        .iter()
        .filter(|text| is_identifier(text))
        .map(|text| (*text).to_owned())
        .collect::<Vec<_>>();
    let mut note = None;
    if output.matches.is_empty()
        && !request.regex
        && texts.len() == 1
        && is_identifier(request.query)
    {
        let spellings = spellings(request.query);
        let (pattern, case_insensitive) = if spellings.len() > 1 {
            (
                any_of_pattern(&spellings.iter().map(String::as_str).collect::<Vec<_>>()),
                true,
            )
        } else {
            (regex::escape(request.query), true)
        };
        // Already case-insensitive with one spelling: nothing else to try.
        if spellings.len() > 1 || !request.case_insensitive {
            let other = search(&pattern, true, case_insensitive)?;
            if !other.matches.is_empty() {
                let mut found = Vec::<String>::new();
                let matcher = regex::RegexBuilder::new(&pattern)
                    .case_insensitive(true)
                    .build()
                    .ok();
                for hit in &other.matches {
                    if let Some(word) = matcher
                        .as_ref()
                        .and_then(|matcher| matcher.find(&hit.preview))
                    {
                        let word = word.as_str().to_owned();
                        if !found.contains(&word) && found.len() < 4 {
                            found.push(word);
                        }
                    }
                }
                note = Some(format!(
                    "0 matches for '{}'; found as {}:",
                    request.query,
                    if found.is_empty() {
                        "another spelling".to_owned()
                    } else {
                        found
                            .iter()
                            .map(|word| format!("'{word}'"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ));
                names.extend(found);
                output = other;
            }
        }
    }
    definitions_first(&mut output, &names);
    Ok((output, note))
}

/// Move the lines that declare one of `names` to the front, keeping the
/// order within each group.
fn definitions_first(output: &mut SearchOutput, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let (mut definitions, uses): (Vec<_>, Vec<_>) = std::mem::take(&mut output.matches)
        .into_iter()
        .partition(|hit| names.iter().any(|name| is_definition(&hit.preview, name)));
    definitions.extend(uses);
    output.matches = definitions;
}

/// A name a search can look up: letters, digits, `_` and `-`, with a letter.
fn is_identifier(text: &str) -> bool {
    text.chars().count() >= 3
        && text
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_' || character == '-')
        && text.chars().any(char::is_alphabetic)
}

/// The words of an identifier: split at `_`, `-` and `camelCase` humps.
fn identifier_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    for part in text.split(['_', '-']) {
        let characters = part.chars().collect::<Vec<_>>();
        let mut current = String::new();
        for (index, character) in characters.iter().enumerate() {
            let previous = index.checked_sub(1).map(|at| characters[at]);
            let next = characters.get(index + 1);
            // A hump: `parseConfig`, and the end of an acronym: `HTTPServer`.
            let hump = character.is_uppercase()
                && previous.is_some_and(|previous| {
                    previous.is_lowercase()
                        || previous.is_ascii_digit()
                        || (previous.is_uppercase() && next.is_some_and(|next| next.is_lowercase()))
                });
            if hump && !current.is_empty() {
                words.push(std::mem::take(&mut current).to_lowercase());
            }
            current.push(*character);
        }
        if !current.is_empty() {
            words.push(current.to_lowercase());
        }
    }
    words
}

fn capitalized(word: &str) -> String {
    let mut characters = word.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(characters).collect()
    })
}

/// An identifier's spellings: `snake_case`, `SCREAMING_CASE`, `kebab-case`,
/// `camelCase`, `PascalCase` and its words run together; just the identifier
/// for a one-word name (searched without case).
fn spellings(text: &str) -> Vec<String> {
    let words = identifier_words(text);
    if words.len() < 2 {
        return vec![text.to_owned()];
    }
    let mut spellings = vec![
        words.join("_"),
        words.join("-"),
        words.concat(),
        words
            .iter()
            .enumerate()
            .map(|(index, word)| {
                if index == 0 {
                    word.clone()
                } else {
                    capitalized(word)
                }
            })
            .collect(),
    ];
    spellings.dedup();
    spellings
}

/// Words that may come before a declaration's keyword.
const MODIFIERS: &[&str] = &[
    "pub",
    "export",
    "default",
    "async",
    "public",
    "private",
    "protected",
    "internal",
    "static",
    "final",
    "abstract",
    "override",
    "unsafe",
    "extern",
    "inline",
    "virtual",
    "sealed",
    "open",
    "data",
    "declare",
    "readonly",
    "local",
    "global",
];

/// Keywords that declare the name after them.
const DECLARING: &[&str] = &[
    "fn",
    "struct",
    "enum",
    "trait",
    "type",
    "union",
    "mod",
    "impl",
    "macro_rules!",
    "const",
    "static",
    "let",
    "class",
    "interface",
    "def",
    "func",
    "function",
    "var",
    "val",
    "module",
    "record",
    "namespace",
    "typedef",
    "object",
    "protocol",
    "alias",
    "defmodule",
    "defp",
];

/// Whether `line` declares `name`: modifiers, a declaring keyword, then the
/// name (`pub(crate) fn parse(`, `class Parser:`, `const PARSE = `), or a Go
/// method (`func (s *Server) Parse(`).
fn is_definition(line: &str, name: &str) -> bool {
    let mut rest = line.trim_start();
    loop {
        if let Some(after) = rest
            .strip_prefix("pub(")
            .and_then(|after| after.split_once(')'))
        {
            rest = after.1.trim_start();
            continue;
        }
        let Some(modifier) = MODIFIERS.iter().find(|modifier| {
            rest.strip_prefix(**modifier)
                .is_some_and(|after| after.starts_with([' ', '\t']))
        }) else {
            break;
        };
        rest = rest[modifier.len()..].trim_start();
    }
    let Some(keyword) = DECLARING.iter().find(|keyword| {
        rest.strip_prefix(**keyword)
            .is_some_and(|after| after.starts_with([' ', '\t', '<', '(']))
    }) else {
        return false;
    };
    let mut after = rest[keyword.len()..].trim_start();
    // Go's receiver: `func (s *Server) Parse(`.
    if *keyword == "func" && after.starts_with('(') {
        after = after
            .split_once(')')
            .map_or("", |(_, rest)| rest.trim_start());
    }
    // Rust's generics on impl, and pointers in C-like code.
    if after.starts_with('<') {
        after = after
            .split_once('>')
            .map_or("", |(_, rest)| rest.trim_start());
    }
    let after = after.trim_start_matches(['*', '&']);
    let after = after.strip_prefix("mut ").unwrap_or(after);
    let Some(tail) = strip_name(after, name) else {
        return false;
    };
    !tail.starts_with(|character: char| character.is_alphanumeric() || character == '_')
}

/// `text` without `name` at its start, matched without case only when the
/// name was (the identifier's other spellings differ in case).
fn strip_name<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.strip_prefix(name).or_else(|| {
        let head = text.get(..name.len())?;
        head.eq_ignore_ascii_case(name).then(|| &text[name.len()..])
    })
}

#[cfg(test)]
mod tests {
    use super::{Lookup, identifier_words, is_definition, is_identifier, lookup, spellings};
    use std::fs;

    fn request<'a>(query: &'a str, any_of: &'a [String]) -> Lookup<'a> {
        Lookup {
            query,
            any_of,
            path: None,
            regex: false,
            case_insensitive: false,
            glob: None,
            context_lines: 0,
            anchors: false,
        }
    }

    /// One call finds every spelling, definitions lead, and an identifier
    /// spelled another way is found with a note saying how.
    #[test]
    fn a_search_finds_spellings_and_puts_definitions_first() {
        let directory = tempfile::tempdir().expect("scratch directory");
        let root = fs::canonicalize(directory.path()).expect("root");
        fs::write(
            root.join("a_use.rs"),
            "let x = prepare_upload(file);
",
        )
        .expect("use");
        fs::write(
            root.join("b_def.rs"),
            "pub fn prepare_upload(file: File) {}
",
        )
        .expect("def");
        fs::write(
            root.join("c.ts"),
            "export const prepareUpload = () => 1;
",
        )
        .expect("ts");

        let any_of = ["prepareUpload".to_owned()];
        let (found, note) = lookup(&root, &request("prepare_upload", &any_of)).expect("search");
        assert!(note.is_none());
        let files = found
            .matches
            .iter()
            .map(|hit| hit.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 3, "{files:?}");
        assert_ne!(files[0], "a_use.rs", "a use is not first: {files:?}");
        assert!(
            files[..2].contains(&"b_def.rs") && files[..2].contains(&"c.ts"),
            "{files:?}"
        );

        let (found, note) = lookup(&root, &request("PrepareUpload", &[])).expect("search");
        assert_eq!(found.matches.len(), 3, "every spelling");
        let note = note.expect("a note");
        assert!(
            note.starts_with("0 matches for 'PrepareUpload'; found as"),
            "{note}"
        );
        assert!(note.contains("'prepare_upload'"), "{note}");
        assert_eq!(found.matches[0].path, "b_def.rs", "the definition leads");

        let regex = lookup(
            &root,
            &Lookup {
                regex: true,
                ..request("prepare.*", &any_of)
            },
        );
        assert!(regex.is_err(), "any_of is literal");
        let (found, note) = lookup(&root, &request("nowhere_at_all", &[])).expect("search");
        assert!(found.matches.is_empty() && note.is_none());
    }

    #[test]
    fn an_identifier_has_other_spellings() {
        assert_eq!(identifier_words("prepareUpload"), ["prepare", "upload"]);
        assert_eq!(identifier_words("HTTPServer"), ["http", "server"]);
        assert_eq!(
            identifier_words("prepare_upload-file"),
            ["prepare", "upload", "file"]
        );
        let all = spellings("prepareUpload");
        for spelling in [
            "prepare_upload",
            "prepare-upload",
            "prepareupload",
            "prepareUpload",
        ] {
            assert!(all.contains(&spelling.to_owned()), "{spelling}: {all:?}");
        }
        assert_eq!(spellings("parser"), ["parser"]);
        assert!(is_identifier("parse_config"));
        assert!(!is_identifier("a b"));
        assert!(!is_identifier("x"));
    }

    #[test]
    fn definitions_are_told_from_uses() {
        for line in [
            "pub(crate) fn parse_config(path: &Path) {",
            "    pub async fn parse_config<T>(",
            "def parse_config(path):",
            "export function parse_config() {",
            "func (s *Server) parse_config() error {",
            "class parse_config:",
            "const parse_config = () => {",
            "impl<T> parse_config for T {",
            "struct parse_config;",
        ] {
            assert!(is_definition(line, "parse_config"), "{line}");
        }
        for line in [
            "let config = parse_config(path);",
            "    parse_config(path)?;",
            "fn parse_config_file() {",
            "// parse_config reads the file",
            "use crate::parse_config;",
        ] {
            assert!(!is_definition(line, "parse_config"), "{line}");
        }
        assert!(!is_definition("class ParseConfig:", "parse_config"));
        assert!(is_definition("pub struct PrepareUpload {", "prepareupload"));
    }
}
