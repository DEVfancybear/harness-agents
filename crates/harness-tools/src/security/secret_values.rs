//! Secret values recognized by their shape (`docs/architecture/GUARDS.en.md`).
//!
//! After Agnostic-AI's `secret-guard` and `tool-output-secret-watch`: a
//! provider's key has a known prefix and length, a PEM private key a known
//! header, and an assignment of a long value to a secret-named field is one
//! too. A match counts only when its value is random enough to be a real one:
//! a placeholder such as `sk-xxxxxxxxxxxxxxxxxxxxxxxx` or
//! `sk-your-key-goes-here-1234` has too few distinct characters, or too little
//! entropy, so no list of placeholder words is needed.
//!
//! What the host does with a match:
//! - a write of one into a file is refused ([`first`]);
//! - a tool's output has it replaced by `[REDACTED <kind>]` before the model
//!   or anything durable reads it ([`redact`], [`LineRedactor`]).

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

/// One shape: what it is called in a refusal, its pattern, and the capture
/// group holding the random part the entropy is judged on (0 = the match).
struct Shape {
    kind: &'static str,
    pattern: Regex,
    value_group: usize,
    /// The least entropy, in bits per character, of a real value.
    min_entropy: f64,
}

fn shape(kind: &'static str, pattern: &str, value_group: usize, min_entropy: f64) -> Shape {
    Shape {
        kind,
        pattern: Regex::new(pattern).expect("a secret shape"),
        value_group,
        min_entropy,
    }
}

/// Most specific first: an Anthropic key also reads as `sk-...`.
static SHAPES: LazyLock<Vec<Shape>> = LazyLock::new(|| {
    vec![
        shape("Anthropic API key", r"\bsk-ant-[A-Za-z0-9_-]{20,}", 0, 3.5),
        shape(
            "OpenAI project key",
            r"\bsk-proj-[A-Za-z0-9_-]{20,}",
            0,
            3.5,
        ),
        shape("Stripe live key", r"\b[sr]k_live_[0-9A-Za-z]{20,}", 0, 3.5),
        shape("API key (sk-)", r"\bsk-[A-Za-z0-9]{20,}", 0, 3.5),
        shape(
            "AWS access key id",
            r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
            0,
            3.0,
        ),
        shape(
            "GitHub token",
            r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})",
            0,
            3.5,
        ),
        shape("Slack token", r"\bxox[baprs]-[A-Za-z0-9-]{10,}", 0, 3.0),
        shape("Google API key", r"\bAIza[0-9A-Za-z_-]{35}", 0, 3.5),
        shape("npm token", r"\bnpm_[A-Za-z0-9]{36}\b", 0, 3.5),
        shape("Hugging Face token", r"\bhf_[A-Za-z0-9]{30,}\b", 0, 3.5),
        // The header is enough: a private key is never a placeholder.
        shape(
            "PEM private key",
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
            0,
            0.0,
        ),
        // A long quoted or bare value assigned to a field that names a secret.
        shape(
            "secret assignment",
            r#"(?i)(?:api[_-]?key|secret|token|password|passwd|auth)["']?\s*[:=]\s*["']?([A-Za-z0-9_\-+/=.]{20,})"#,
            1,
            4.0,
        ),
        shape(
            "bearer token",
            r"(?i)\bbearer\s+([A-Za-z0-9_\-.=]{20,})",
            1,
            4.0,
        ),
    ]
});

/// Shannon entropy of `text`, in bits per character.
fn entropy(text: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    let mut total = 0_u32;
    for character in text.chars() {
        *counts.entry(character).or_insert(0_u32) += 1;
        total += 1;
    }
    if total == 0 {
        return 0.0;
    }
    let total = f64::from(total);
    counts
        .values()
        .map(|count| {
            let share = f64::from(*count) / total;
            -share * share.log2()
        })
        .sum()
}

/// Whether `value` is random enough to be a real secret.
fn random_enough(value: &str, min_entropy: f64) -> bool {
    if min_entropy <= 0.0 {
        return true;
    }
    let distinct = value
        .chars()
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    distinct >= 10 && entropy(value) >= min_entropy
}

/// One recognized value: where it is in the text, and what it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Found {
    pub start: usize,
    pub end: usize,
    pub kind: &'static str,
}

/// Every secret value in `text`, in order, none overlapping.
#[must_use]
pub fn find(text: &str) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    for shape in SHAPES.iter() {
        for captures in shape.pattern.captures_iter(text) {
            let Some(value) = captures.get(shape.value_group) else {
                continue;
            };
            if !random_enough(value.as_str(), shape.min_entropy) {
                continue;
            }
            // For an assignment only the value is the secret; the field's name
            // stays readable.
            let (start, end) = (value.start(), value.end());
            if found
                .iter()
                .any(|other| start < other.end && other.start < end)
            {
                continue;
            }
            found.push(Found {
                start,
                end,
                kind: shape.kind,
            });
        }
    }
    found.sort_by_key(|found| found.start);
    found
}

/// The first secret value in `text`, if any: what a refused write names.
#[must_use]
pub fn first(text: &str) -> Option<&'static str> {
    find(text).first().map(|found| found.kind)
}

/// `text` with every secret value replaced by `[REDACTED <kind>]`.
#[must_use]
pub fn redact(text: &str) -> Cow<'_, str> {
    let found = find(text);
    if found.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for value in found {
        out.push_str(&text[at..value.start]);
        out.push_str("[REDACTED ");
        out.push_str(value.kind);
        out.push(']');
        at = value.end;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
}

/// The longest line held back waiting for its end; a longer one is redacted
/// as it is. No shape spans a line, so complete lines are all it needs.
const MAX_HELD_LINE: usize = 16 * 1024;

/// [`redact`] over a stream, a line at a time: a value split across two reads
/// is still found.
#[derive(Debug, Default)]
pub(crate) struct LineRedactor {
    held: Vec<u8>,
}

impl LineRedactor {
    fn redact_bytes(bytes: &[u8]) -> Vec<u8> {
        match std::str::from_utf8(bytes) {
            Ok(text) => redact(text).into_owned().into_bytes(),
            // Not text: nothing a shape reads.
            Err(_) => bytes.to_vec(),
        }
    }

    /// The complete lines of what has arrived, redacted.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.held.extend_from_slice(chunk);
        // A carriage return ends a line too: a progress bar redraws one line
        // with it, and holding it back would freeze the bar.
        let cut = match self
            .held
            .iter()
            .rposition(|byte| *byte == b'\n' || *byte == b'\r')
        {
            Some(newline) => newline + 1,
            None if self.held.len() > MAX_HELD_LINE => self.held.len(),
            None => return Vec::new(),
        };
        let rest = self.held.split_off(cut);
        let complete = std::mem::replace(&mut self.held, rest);
        Self::redact_bytes(&complete)
    }

    /// What is left at the end of the stream, redacted.
    pub(crate) fn finish(self) -> Vec<u8> {
        Self::redact_bytes(&self.held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Built at run time, so no literal key sits in the source.
    fn key(prefix: &str) -> String {
        format!("{prefix}{}", "aB3dE5gH7jK9mN1pQ2rS4tU6vW8xY0z")
    }

    #[test]
    fn real_shaped_values_are_found_and_placeholders_are_not() {
        assert_eq!(
            first(&format!("OPENAI_API_KEY={}", key("sk-"))),
            Some("API key (sk-)")
        );
        assert_eq!(
            first("client_secret = \"Zq8vN2pL5xR7tK3mW9bY4cF6\""),
            Some("secret assignment")
        );
        assert_eq!(
            first(&format!("key {}", key("sk-ant-"))),
            Some("Anthropic API key")
        );
        assert_eq!(
            first(&format!("token: {}", key("ghp_") + "12345")),
            Some("GitHub token")
        );
        assert_eq!(
            first(&format!("push with {}12345", key("ghp_"))),
            Some("GitHub token")
        );
        assert_eq!(
            first("-----BEGIN RSA PRIVATE KEY-----"),
            Some("PEM private key")
        );
        assert_eq!(
            first("AWS id AKIAQX7ZK2P4MB9LRT3W here"),
            Some("AWS access key id")
        );
        for placeholder in [
            "OPENAI_API_KEY=sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            "api_key = \"your-api-key-goes-here\"",
            "token_count = 123456789012345678901",
            "password: aaaaaaaaaaaaaaaaaaaaaaaa",
            "sk-1111111111111111111111",
        ] {
            assert_eq!(first(placeholder), None, "{placeholder}");
        }
    }

    #[test]
    fn redaction_keeps_the_field_and_names_the_kind() {
        let text = format!("A=1\nOPENAI_API_KEY={}\nB=2\n", key("sk-"));
        assert_eq!(
            redact(&text),
            "A=1\nOPENAI_API_KEY=[REDACTED API key (sk-)]\nB=2\n"
        );
        assert!(matches!(redact("nothing here"), Cow::Borrowed(_)));
    }

    #[test]
    fn a_value_split_across_reads_is_still_redacted() {
        let text = format!("x {} y\nz\n", key("sk-ant-"));
        let (head, tail) = text.split_at(12);
        let mut stream = LineRedactor::default();
        let mut out = stream.push(head.as_bytes());
        assert!(out.is_empty(), "the line is held until it ends");
        out.extend(stream.push(tail.as_bytes()));
        out.extend(stream.finish());
        assert_eq!(
            String::from_utf8(out).expect("text"),
            "x [REDACTED Anthropic API key] y\nz\n"
        );
    }
}
