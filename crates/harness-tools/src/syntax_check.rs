//! The syntax gate (after `CowAgent`'s) on writes: a JSON or TOML file a write would leave
//! unparseable is refused and kept as it was. Only a write that breaks the
//! file is refused - one already broken, or not parsed here, is written - so
//! a configuration is never turned into a build failure found later.

use std::path::Path;

/// Why `after` cannot be written to `path`, or `None` when it can: it parses,
/// its kind is not checked here, or `before` did not parse either.
pub(crate) fn broken_by_write(path: &Path, before: Option<&str>, after: &str) -> Option<String> {
    let kind = Kind::of(path)?;
    let error = kind.parse(after).err()?;
    if before.is_some_and(|before| kind.parse(before).is_err()) {
        return None;
    }
    Some(format!(
        "the write would leave {} unparseable as {} ({error}); the file was left unchanged. Fix the text and write again.",
        path.display(),
        kind.name()
    ))
}

#[derive(Clone, Copy)]
enum Kind {
    Json,
    Toml,
}

impl Kind {
    fn of(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "json" => Some(Self::Json),
            "toml" => Some(Self::Toml),
            _ => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Json => "JSON",
            Self::Toml => "TOML",
        }
    }

    fn parse(self, text: &str) -> Result<(), String> {
        match self {
            // tsconfig.json and editor settings are JSON with comments and
            // trailing commas: read that way before calling one broken.
            Self::Json => serde_json::from_str::<serde_json::Value>(text)
                .or_else(|_| serde_json::from_str::<serde_json::Value>(&without_jsonc(text)))
                .map(|_| ())
                .map_err(|error| error.to_string()),
            Self::Toml => text
                .parse::<toml::Table>()
                .map(|_| ())
                .map_err(|error| error.message().to_owned()),
        }
    }
}

/// `text` without `//` and `/* */` comments outside strings and without a
/// comma before a closing bracket or brace.
fn without_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    let mut in_string = false;
    while let Some(character) = characters.next() {
        if in_string {
            out.push(character);
            if character == '\\' {
                if let Some(escaped) = characters.next() {
                    out.push(escaped);
                }
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match (character, characters.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(character);
            }
            ('/', Some('/')) => {
                for skipped in characters.by_ref() {
                    if skipped == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                characters.next();
                let mut previous = ' ';
                for skipped in characters.by_ref() {
                    if previous == '*' && skipped == '/' {
                        break;
                    }
                    previous = skipped;
                }
            }
            _ => out.push(character),
        }
    }
    // A trailing comma: drop a comma whose next non-space character closes.
    let mut cleaned = String::with_capacity(out.len());
    let characters = out.chars().collect::<Vec<_>>();
    for (index, character) in characters.iter().enumerate() {
        if *character == ',' {
            let next = characters[index + 1..]
                .iter()
                .find(|next| !next.is_whitespace());
            if matches!(next, Some(']' | '}')) {
                continue;
            }
        }
        cleaned.push(*character);
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::broken_by_write;
    use std::path::Path;

    #[test]
    fn a_write_that_breaks_json_or_toml_is_refused() {
        let json = Path::new("package.json");
        assert!(broken_by_write(json, Some("{\"a\": 1}"), "{\"a\": 1,,}").is_some());
        assert!(broken_by_write(json, None, "{\"a\": }").is_some());
        assert!(broken_by_write(json, Some("{\"a\": 1}"), "{\"a\": 2}").is_none());
        let toml = Path::new("Cargo.toml");
        let refused = broken_by_write(toml, Some("[a]\nb = 1\n"), "[a]\nb = \n")
            .expect("broken TOML is refused");
        assert!(refused.contains("TOML"), "{refused}");
        assert!(broken_by_write(toml, Some("[a]\nb = 1\n"), "[a]\nb = 2\n").is_none());
    }

    #[test]
    fn json_with_comments_a_broken_file_and_other_kinds_are_written() {
        let tsconfig = Path::new("tsconfig.json");
        let jsonc =
            "{\n  // strict\n  \"strict\": true, /* yes */\n  \"url\": \"http://x//y\",\n}\n";
        assert!(broken_by_write(tsconfig, None, jsonc).is_none());
        // Already broken before: the write is not what broke it.
        assert!(broken_by_write(Path::new("a.json"), Some("{"), "{ still").is_none());
        assert!(broken_by_write(Path::new("a.rs"), None, "fn (").is_none());
        assert!(broken_by_write(Path::new("README"), None, "{").is_none());
    }
}
