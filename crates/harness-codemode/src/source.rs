//! The codemode source format (pi's `source.ts`): JavaScript, optionally
//! preceded by one options line.
//!
//! ```js
//! // @options: {"max_output_tokens": 2000, "timeout_ms": 30000}
//! const text = await tools.read_file({ path: "package.json" });
//! ```
//!
//! The options line is replaced by an empty line, so line numbers in errors
//! still match the input.

use serde_json::Value as Json;

pub const OPTIONS_PREFIX: &str = "// @options:";
const SUPPORTED: &str = "`max_output_tokens` and `timeout_ms`";
/// The largest delay a JavaScript timer takes, which bounds `timeout_ms`.
const MAX_TIMEOUT_MS: u64 = 2_147_483_647;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceOptions {
    /// A token budget for the script's output.
    pub max_output_tokens: Option<u64>,
    /// A hard deadline for the whole script, tool calls included.
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSource {
    pub code: String,
    pub options: SourceOptions,
}

fn parse_options(directive: &str) -> Result<SourceOptions, String> {
    let shape = || format!("@options must be a JSON object with supported fields {SUPPORTED}");
    if directive.is_empty() {
        return Err(shape());
    }
    let value: Json = serde_json::from_str(directive).map_err(|error| {
        format!("@options must be valid JSON with supported fields {SUPPORTED}: {error}")
    })?;
    let Json::Object(fields) = value else {
        return Err(shape());
    };
    let mut options = SourceOptions::default();
    for (key, value) in fields {
        match key.as_str() {
            "max_output_tokens" => {
                options.max_output_tokens = Some(
                    value
                        .as_u64()
                        .filter(|n| *n <= 9_007_199_254_740_991)
                        .ok_or_else(|| {
                            "@options field `max_output_tokens` must be a non-negative safe integer"
                                .to_owned()
                        })?,
                );
            }
            "timeout_ms" => {
                options.timeout_ms = Some(
                    value
                        .as_u64()
                        .filter(|n| *n > 0 && *n <= MAX_TIMEOUT_MS)
                        .ok_or_else(|| {
                            format!("@options field `timeout_ms` must be a positive integer up to {MAX_TIMEOUT_MS}")
                        })?,
                );
            }
            other => return Err(format!("@options only supports {SUPPORTED}; got `{other}`")),
        }
    }
    Ok(options)
}

/// Split an optional first-line `// @options: {...}` from the script.
///
/// # Errors
/// Empty input, invalid options, an options line without code after it, and
/// an options line that is not the first line, which would otherwise be
/// ignored without notice.
pub fn parse_source(input: &str) -> Result<ParsedSource, String> {
    if input.trim().is_empty() {
        return Err(
            "Expected JavaScript source text (non-empty). Provide JS only, optionally with a first line `// @options: {\"max_output_tokens\": 1000}`."
                .to_owned(),
        );
    }
    if let Some(misplaced) = input
        .split('\n')
        .enumerate()
        .skip(1)
        .find(|(_, line)| line.trim_start().starts_with(OPTIONS_PREFIX))
    {
        return Err(format!(
            "The {OPTIONS_PREFIX} line must be the first line of the input; found one on line {}",
            misplaced.0 + 1
        ));
    }
    let first = input
        .split('\n')
        .next()
        .unwrap_or("")
        .trim_end_matches('\r');
    let trimmed = first.trim_start();
    let Some(directive) = trimmed.strip_prefix(OPTIONS_PREFIX) else {
        return Ok(ParsedSource {
            code: input.to_owned(),
            options: SourceOptions::default(),
        });
    };
    let code = input.find('\n').map_or("", |newline| &input[newline..]);
    if code.trim().is_empty() {
        return Err(
            "The @options line must be followed by JavaScript source on subsequent lines"
                .to_owned(),
        );
    }
    Ok(ParsedSource {
        code: code.to_owned(),
        options: parse_options(directive.trim())?,
    })
}

#[cfg(test)]
mod tests {
    use super::{SourceOptions, parse_source};

    #[test]
    fn returns_plain_code_unchanged() {
        let parsed = parse_source("const a = 1;\nreturn a;").unwrap();
        assert_eq!(parsed.code, "const a = 1;\nreturn a;");
        assert_eq!(parsed.options, SourceOptions::default());
    }

    #[test]
    fn parses_the_options_line_and_keeps_line_numbers() {
        let parsed = parse_source(
            "// @options: {\"max_output_tokens\": 2000, \"timeout_ms\": 30000}\ntext(1);",
        )
        .unwrap();
        assert_eq!(parsed.code, "\ntext(1);");
        assert_eq!(parsed.options.max_output_tokens, Some(2000));
        assert_eq!(parsed.options.timeout_ms, Some(30000));
    }

    #[test]
    fn rejects_misplaced_empty_and_invalid_options() {
        for (input, needle) in [
            ("", "non-empty"),
            ("text(1)\n// @options: {}", "first line"),
            ("// @options: {}", "followed by JavaScript"),
            ("// @options: nope\nx", "valid JSON"),
            ("// @options: []\nx", "JSON object"),
            ("// @options: {\"other\": 1}\nx", "only supports"),
            ("// @options: {\"timeout_ms\": 0}\nx", "positive integer"),
            (
                "// @options: {\"max_output_tokens\": -1}\nx",
                "non-negative",
            ),
        ] {
            let error = parse_source(input).unwrap_err();
            assert!(error.contains(needle), "{input:?}: {error}");
        }
    }
}
