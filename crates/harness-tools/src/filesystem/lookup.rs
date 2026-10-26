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
//!   so - before, "0 matches" left the model to guess the spelling itself;
//! - several words no line holds together: every file holding one is
//!   counted and ranked with BM25's weights (agentic-file-search's keyword
//!   ranking), so a rare word outranks one on every line (`by_words`).

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
    let words = query_words(request.query);
    if output.matches.is_empty()
        && !request.regex
        && texts.len() == 1
        && words.len() > 1
        && let Some((found, said)) = by_words(root, request, &words)?
    {
        (output, note) = (found, Some(said));
    }
    definitions_first(&mut output, &names);
    Ok((output, note))
}

/// The words of a literal query, lowercased, each once.
fn query_words(query: &str) -> Vec<String> {
    let mut words = Vec::<String>::new();
    for word in query.split_whitespace().map(str::to_lowercase) {
        if !words.contains(&word) {
            words.push(word);
        }
    }
    words
}

/// Most lines one file shows in a search for several words, so the result
/// reaches past its first files.
const WORD_LINES_PER_FILE: usize = 20;

/// BM25's IDF as an integer score: three decimals kept.
const IDF_SCALE: f64 = 1000.0;

/// BM25's term-frequency saturation: the tenth occurrence of a word adds
/// little to a file that holds it nine times.
const TF_SATURATION: f64 = 1.2;

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a positive score far below u64::MAX"
)]
fn scaled(score: f64) -> u64 {
    (score * IDF_SCALE).round().max(1.0) as u64
}

/// One word's score in a file that holds it `count` times: its IDF `weight`,
/// saturated by how often it occurs (BM25 without a length norm).
fn word_score(weight: u64, count: u32) -> u64 {
    let count = f64::from(count);
    #[allow(clippy::cast_precision_loss, reason = "a scaled IDF below 2^52")]
    let weight = weight as f64;
    scaled(weight / IDF_SCALE * count * (TF_SATURATION + 1.0) / (count + TF_SATURATION))
}

/// A search for several words no line holds together (agentic-file-search's
/// keyword ranking, with BM25's weights): every searched file holding one of
/// them is counted - not only those before the 512-match cut - and the files
/// holding the most words come first (the shared rule of
/// `harness_types::search`), then those whose words are rarer in the
/// workspace and occur more often, so a word on every line no longer outranks
/// the one that names the thing. A file's lines come with the rarest words
/// first, at most [`WORD_LINES_PER_FILE`] of them. `None` when no file holds
/// any word.
fn by_words(
    root: &Path,
    request: &Lookup<'_>,
    words: &[String],
) -> Result<Option<(SearchOutput, String)>, HarnessError> {
    let walk = crate::workspace::search_walk(root, request.path)?;
    let (files, searched) = crate::search::word_counts(root, &walk, words, request.glob)?;
    if files.is_empty() {
        return Ok(None);
    }
    let documents = u64::try_from(searched.max(files.len())).unwrap_or(u64::MAX);
    let weights = (0..words.len())
        .map(|index| {
            let holding = files.iter().filter(|file| file.counts[index] > 0).count();
            scaled(harness_types::search::bm25_idf(
                documents,
                u64::try_from(holding).unwrap_or(u64::MAX),
            ))
        })
        .collect::<Vec<_>>();
    let mut found = harness_types::search::match_words(files, words, |file, word| {
        let index = words.iter().position(|each| each == word)?;
        let count = file.counts[index];
        (count > 0).then(|| word_score(weights[index], count))
    });
    let mut note = found.note("files", words).unwrap_or_else(|| {
        format!(
            "0 matches for '{}'; lines of the files holding all its words:",
            request.query
        )
    });
    found.most_first();
    let order = found
        .hits
        .iter()
        .map(|(_, file)| file.file)
        .collect::<Vec<_>>();
    let pattern = any_of_pattern(&words.iter().map(String::as_str).collect::<Vec<_>>());
    let (lines, cut) = crate::search::ranked_matches(
        root,
        &walk,
        &order,
        &crate::search::SearchQuery {
            query: &pattern,
            use_regex: true,
            case_insensitive: true,
            glob: None,
            context_lines: request.context_lines,
            anchors: request.anchors,
        },
        WORD_LINES_PER_FILE,
        |line| {
            let line = line.to_lowercase();
            words
                .iter()
                .zip(&weights)
                .filter(|(word, _)| line.contains(word.as_str()))
                .map(|(_, weight)| *weight)
                .sum()
        },
    )?;
    if cut {
        let at = note.len() - usize::from(note.ends_with(':'));
        note.insert_str(
            at,
            &format!(" (at most {WORD_LINES_PER_FILE} lines of each file, its rarest words first)"),
        );
    }
    Ok(Some((
        SearchOutput {
            matches: lines.matches,
            truncated: lines.truncated || !walk.complete,
        },
        note,
    )))
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
    let Some((_, after)) = declaration(line) else {
        return false;
    };
    let Some(tail) = strip_name(after, name) else {
        return false;
    };
    !tail.starts_with(|character: char| character.is_alphanumeric() || character == '_')
}

/// Keywords that declare a variable: in a file's outline only at its top
/// level, as a local `let` is not part of a file's shape.
const VARIABLE: &[&str] = &["let", "var", "val", "const", "static"];

/// Whether `line` belongs in a code file's outline: it declares a named
/// function, type, class, module or the like at any depth, or a variable or
/// constant at the top level (unindented).
///
/// The declared name must be followed by code: nothing, punctuation (`(`,
/// `{`, `:`, `=`, `;`...), or a line ending in `{`, `(`, `:` or ` do` - so a
/// sentence of prose that starts with `let` or `type` is not a declaration.
pub(crate) fn declares(line: &str) -> bool {
    let Some((keyword, after)) = declaration(line) else {
        return false;
    };
    if !after.starts_with(|character: char| {
        character.is_alphabetic() || character == '_' || character == '$'
    }) || (VARIABLE.contains(&keyword) && line.starts_with([' ', '\t']))
    {
        return false;
    }
    let rest = after
        .trim_start_matches(|character: char| {
            character.is_alphanumeric() || character == '_' || character == '$'
        })
        .trim();
    rest.is_empty()
        || rest.starts_with(['(', '{', ':', '=', ';', '<', '[', ',', '.', '!'])
        || rest.ends_with(['{', '(', ':', ';', '='])
        || rest.ends_with(" do")
}

/// The declaring keyword of `line` and the text after it, where the declared
/// name starts; `None` for a line that declares nothing.
fn declaration(line: &str) -> Option<(&'static str, &str)> {
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
    let keyword = DECLARING.iter().copied().find(|keyword| {
        rest.strip_prefix(*keyword)
            .is_some_and(|after| after.starts_with([' ', '\t', '<', '(']))
    })?;
    let mut after = rest[keyword.len()..].trim_start();
    // Go's receiver: `func (s *Server) Parse(`.
    if keyword == "func" && after.starts_with('(') {
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
    Some((keyword, after.strip_prefix("mut ").unwrap_or(after)))
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
    use super::{
        Lookup, declares, identifier_words, is_definition, is_identifier, lookup, spellings,
    };
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

        // Several words no line holds together: the lines of their words, the
        // files holding the most words first, and the word nothing holds.
        let (found, note) =
            lookup(&root, &request("upload nowhere_word file", &[])).expect("search");
        let mut files = found
            .matches
            .iter()
            .map(|hit| hit.path.as_str())
            .collect::<Vec<_>>();
        files.dedup();
        assert_eq!(files.len(), 3, "{files:?}");
        assert_eq!(files[2], "c.ts", "one word of three comes last: {files:?}");
        let note = note.expect("a note");
        assert!(note.contains("no files match 'nowhere_word'"), "{note}");
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

    /// A file's outline: what it declares, not its local variables nor a
    /// sentence of prose that starts with a keyword.
    #[test]
    fn an_outline_holds_declarations() {
        for line in [
            "pub(crate) fn parse_config(path: &Path) {",
            "    pub async fn method(&self) -> u8 {",
            "impl<T> Display for Wrapper<T> {",
            "mod loader;",
            "class Parser(Base):",
            "    def parse(self):",
            "export default function App() {",
            "export const ROUTES = [",
            "type Config struct {",
            "func (s *Server) Serve() error {",
            "interface Props {",
            "defmodule Shop.Cart do",
            "def run",
        ] {
            assert!(declares(line), "{line}");
        }
        for line in [
            "    let total = items.len();",
            "        const x = 1;",
            "let me explain how the loader works",
            "type the name into the box",
            "class of problems we see",
            "// fn commented_out() {}",
            "parse_config(path)?;",
        ] {
            assert!(!declares(line), "{line}");
        }
    }

    /// Several words no line holds together: every file holding one is
    /// counted, past the 512-match cut, and a file holding the rare word
    /// outranks files holding a word that is everywhere.
    #[test]
    fn several_words_are_ranked_by_how_rare_they_are() {
        let directory = tempfile::tempdir().expect("scratch directory");
        let root = fs::canonicalize(directory.path()).expect("root");
        // Six files fill the search with "value" long before z_last.rs, which
        // alone holds "checksum": path order and the old cut lost it.
        for name in ["a", "b", "c", "d", "e", "f"] {
            fs::write(
                root.join(format!("{name}.rs")),
                "let value = value + 1;\n".repeat(200),
            )
            .expect("common");
        }
        fs::write(root.join("z_last.rs"), "fn verify(checksum: u32) {}\n").expect("rare");
        // One file holds both words, on different lines: it leads, and its
        // line with the rare word comes before the one with the common word.
        fs::write(
            root.join("m_both.rs"),
            "let value = 1;\nlet other = 2;\nassert_eq!(checksum, 3);\n",
        )
        .expect("both");

        let (found, note) =
            lookup(&root, &request("checksum value nowhere_word", &[])).expect("search");
        let note = note.expect("a note");
        assert!(note.contains("no files match 'nowhere_word'"), "{note}");
        assert!(note.contains("at most 20 lines of each file"), "{note}");
        let mut files = found
            .matches
            .iter()
            .map(|hit| hit.path.as_str())
            .collect::<Vec<_>>();
        files.dedup();
        assert_eq!(files[0], "m_both.rs", "both words lead: {files:?}");
        assert_eq!(files[1], "z_last.rs", "the rare word next: {files:?}");
        assert_eq!(files.len(), 8, "every file shows: {files:?}");
        assert_eq!(found.matches[0].line, 3, "the rare word's line first");
        assert_eq!(found.matches[1].line, 1);
        let common = found
            .matches
            .iter()
            .filter(|hit| hit.path == "a.rs")
            .count();
        assert_eq!(common, 20, "a file shows at most 20 lines");

        // Every word in one file but no line holding all: that file's lines,
        // the line holding more of the words first.
        let (found, note) = lookup(&root, &request("value checksum", &[])).expect("search");
        assert!(
            note.expect("note")
                .starts_with("0 matches for 'value checksum'")
        );
        assert!(found.matches.iter().all(|hit| hit.path == "m_both.rs"));
        assert_eq!(found.matches[0].line, 3);
    }

    #[test]
    fn a_word_score_saturates_and_weighs_its_rarity() {
        let rare = super::scaled(harness_types::search::bm25_idf(100, 1));
        let common = super::scaled(harness_types::search::bm25_idf(100, 90));
        assert!(super::word_score(rare, 1) > super::word_score(common, 50));
        assert!(super::word_score(rare, 2) > super::word_score(rare, 1));
        assert!(super::word_score(rare, 100) < 2 * super::word_score(rare, 2));
    }
}
