//! Per-model system-prompt additions, after prime-agent's `model_prompts.rs`
//! (1e894a62, e41c9c4c): a TOML rule map from model selectors to markdown files.
//!
//! The shipped rules run first, then the user's `model-prompts.toml` beside the
//! user config. A rule's `match` globs are tried against every final segment
//! run of the model selector (`provider/model`), case-insensitive; every
//! matching rule's files join, in first-seen order, into one segment the system
//! prompt carries before its session tail, where it stays cached. A file is
//! looked up in the config directory first and among the shipped files after.
//! Any problem in either layer turns the additions off for the turn and is
//! reported; it never blocks the model.
//!
//! ```toml
//! [[rule]]
//! match = ["deepseek-*"]
//! files = ["deepseek.md"]
//! ```

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path};

use serde::Deserialize;

const SHIPPED_TOML: &str = include_str!("prompts/model_prompts.toml");
const SHIPPED_FILES: &[(&str, &str)] = &[(
    "bash-over-subprocess.md",
    include_str!("prompts/bash-over-subprocess.md"),
)];
const USER_FILE: &str = "model-prompts.toml";
const SHIPPED_SOURCE: &str = "shipped model_prompts.toml";

/// What the rules give one model: the segment, or why there is none.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Resolution {
    /// `None` whenever `errors` is not empty.
    pub extras: Option<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    r#match: Vec<String>,
    files: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(default)]
    rule: Vec<Rule>,
}

struct Parsed {
    patterns: Vec<globset::GlobMatcher>,
    files: Vec<String>,
}

fn parse(text: &str, source: &str) -> (Vec<Parsed>, Vec<String>) {
    let document = match toml::from_str::<Document>(text) {
        Ok(document) => document,
        Err(error) => return (Vec::new(), vec![format!("{source}: {error}")]),
    };
    let mut rules = Vec::new();
    let mut errors = Vec::new();
    for rule in document.rule {
        let mut patterns = Vec::new();
        for pattern in rule.r#match {
            match globset::GlobBuilder::new(&pattern)
                .literal_separator(true)
                .case_insensitive(true)
                .build()
            {
                Ok(glob) => patterns.push(glob.compile_matcher()),
                Err(error) => errors.push(format!("{source}: pattern {pattern:?}: {error}")),
            }
        }
        let mut files = Vec::new();
        for name in rule.files {
            let inside = Path::new(&name).components().all(|component| {
                !matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            });
            if inside {
                files.push(
                    Path::new(&name)
                        .components()
                        .filter(|component| !matches!(component, Component::CurDir))
                        .map(|component| component.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            } else {
                errors.push(format!(
                    "{source}: file {name:?} must be a relative path inside the config directory"
                ));
            }
        }
        rules.push(Parsed { patterns, files });
    }
    (rules, errors)
}

/// The selector's segments, without a trailing `:<thinking level>`.
fn segments(selector: &str) -> Vec<&str> {
    let selector = match selector.rsplit_once(':') {
        Some((model, "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max")) => model,
        _ => selector,
    };
    selector.split('/').collect()
}

fn applies(rule: &Parsed, segments: &[&str]) -> bool {
    rule.patterns.iter().any(|matcher| {
        (1..=segments.len())
            .any(|count| matcher.is_match(segments[segments.len() - count..].join("/")))
    })
}

/// Read a regular file only: opening a FIFO can wait forever for a writer.
fn read_regular(path: &Path) -> io::Result<String> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    std::fs::read_to_string(path)
}

fn resolve_files(
    rules: &[Parsed],
    source: &str,
    config_dir: &Path,
    shipped: &[(&str, &str)],
    contents: &mut BTreeMap<String, String>,
) -> Vec<String> {
    let mut errors = Vec::new();
    for rule in rules {
        for name in &rule.files {
            if contents.contains_key(name) {
                continue;
            }
            let path = config_dir.join(name);
            let content = path.canonicalize().and_then(|resolved| {
                if !resolved.starts_with(config_dir.canonicalize()?) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "file resolves outside the config directory",
                    ));
                }
                read_regular(&resolved)
            });
            match content {
                Ok(content) => {
                    contents.insert(name.clone(), content);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match shipped
                        .iter()
                        .find(|(shipped, _)| *shipped == name.as_str())
                    {
                        Some((_, content)) => {
                            contents.insert(name.clone(), (*content).to_owned());
                        }
                        None => errors.push(format!("{source}: missing file {name:?}")),
                    }
                }
                Err(error) => errors.push(format!("{source}: {}: {error}", path.display())),
            }
        }
    }
    errors
}

fn resolve_with(
    selector: &str,
    shipped_toml: &str,
    shipped_files: &[(&str, &str)],
    config_dir: &Path,
) -> Resolution {
    let mut errors = Vec::new();
    let (shipped, mut layer) = parse(shipped_toml, SHIPPED_SOURCE);
    errors.append(&mut layer);
    let user_path = config_dir.join(USER_FILE);
    let user_source = user_path.display().to_string();
    let user = match read_regular(&user_path) {
        Ok(text) => {
            let (rules, mut layer) = parse(&text, &user_source);
            errors.append(&mut layer);
            rules
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            errors.push(format!("{user_source}: {error}"));
            Vec::new()
        }
    };
    let mut contents = BTreeMap::new();
    errors.extend(resolve_files(
        &shipped,
        SHIPPED_SOURCE,
        config_dir,
        shipped_files,
        &mut contents,
    ));
    errors.extend(resolve_files(
        &user,
        &user_source,
        config_dir,
        shipped_files,
        &mut contents,
    ));
    if !errors.is_empty() {
        return Resolution {
            extras: None,
            errors,
        };
    }
    let segments = segments(selector);
    let mut included: Vec<&str> = Vec::new();
    for rule in shipped.iter().chain(&user) {
        if applies(rule, &segments) {
            for name in &rule.files {
                if !included.contains(&name.as_str()) {
                    included.push(name);
                }
            }
        }
    }
    let parts = included
        .iter()
        .map(|name| contents[*name].trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    Resolution {
        extras: (!parts.is_empty()).then(|| parts.join("\n\n")),
        errors,
    }
}

/// The additions for `selector` (`provider/model`), from the shipped rules and
/// `model-prompts.toml` in `config_dir`.
#[must_use]
pub fn resolve(selector: &str, config_dir: &Path) -> Resolution {
    resolve_with(selector, SHIPPED_TOML, SHIPPED_FILES, config_dir)
}

/// The notice a broken rule map is reported with, once per distinct problem in
/// a process: every turn would repeat it otherwise.
#[must_use]
pub fn error_notice(errors: &[String]) -> Option<String> {
    static REPORTED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let key = errors.join("\n");
    let mut reported = REPORTED.lock().ok()?;
    if errors.is_empty() || reported.contains(&key) {
        return None;
    }
    reported.push(key);
    Some(format!(
        "per-model prompt additions are off for now: {}",
        errors.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIPPED: &str = "[[rule]]\nmatch = [\"glm-5.3*\"]\nfiles = [\"bash.md\"]\n";
    const FILES: &[(&str, &str)] = &[("bash.md", "Use bash().")];

    #[test]
    fn rules_match_the_selector_tail_and_join_their_files() {
        let directory = tempfile::tempdir().expect("dir");
        std::fs::write(
            directory.path().join(USER_FILE),
            "[[rule]]\nmatch = [\"deepseek-*\", \"opencode-go/*\"]\nfiles = [\"deepseek.md\", \"bash.md\"]\n",
        )
        .expect("rules");
        std::fs::write(
            directory.path().join("deepseek.md"),
            "Keep answers short.\n",
        )
        .expect("file");
        let deepseek = resolve_with(
            "opencode-go/deepseek-v4.1-flash",
            SHIPPED,
            FILES,
            directory.path(),
        );
        assert!(deepseek.errors.is_empty(), "{deepseek:?}");
        assert_eq!(
            deepseek.extras.as_deref(),
            Some("Keep answers short.\n\nUse bash().")
        );
        let glm = resolve_with("zai/GLM-5.3-air:high", SHIPPED, FILES, directory.path());
        assert_eq!(glm.extras.as_deref(), Some("Use bash()."));
        let other = resolve_with(
            "anthropic/claude-sonnet-4",
            SHIPPED,
            FILES,
            directory.path(),
        );
        assert_eq!(other, Resolution::default());
    }

    #[test]
    fn a_broken_map_turns_the_additions_off_and_says_why() {
        let directory = tempfile::tempdir().expect("dir");
        for (text, expected) in [
            (
                "[[rule]]\nmatch = [\"x\"]\nfiles = [\"../outside.md\"]\n",
                "inside the config directory",
            ),
            (
                "[[rule]]\nmatch = [\"x\"]\nfiles = [\"absent.md\"]\n",
                "missing file",
            ),
            (
                "[[rule]]\nmatch = [\"x\"]\nfiles = []\nextra = 1\n",
                "unknown field",
            ),
            ("not toml [", "model-prompts.toml"),
        ] {
            std::fs::write(directory.path().join(USER_FILE), text).expect("rules");
            let resolved = resolve_with("glm-5.3", SHIPPED, FILES, directory.path());
            assert!(resolved.extras.is_none(), "{text}");
            assert!(
                resolved.errors.iter().any(|error| error.contains(expected)),
                "{text}: {:?}",
                resolved.errors
            );
        }
        assert!(error_notice(&["one".to_owned()]).is_some());
        assert!(error_notice(&["one".to_owned()]).is_none(), "reported once");
    }

    #[test]
    fn the_shipped_rules_are_valid() {
        let directory = tempfile::tempdir().expect("dir");
        let resolved = resolve("zai/glm-5.3", directory.path());
        assert!(resolved.errors.is_empty(), "{resolved:?}");
        assert!(resolved.extras.is_some_and(|text| text.contains("bash(")));
    }
}
