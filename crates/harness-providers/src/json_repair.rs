//! Tool-call arguments a model wrote as almost-JSON, ported from prime-agent's
//! `repairJson` (`pa-agent/src/proxy.rs`, the TS `packages/ai/src/utils/json-parse.ts`):
//! raw control characters inside strings are escaped, and a backslash before a
//! character JSON does not escape is doubled.

use std::fmt::Write as _;

const VALID_JSON_ESCAPES: [char; 8] = ['"', '\\', '/', 'b', 'f', 'n', 'r', 't'];

fn is_control_character(ch: char) -> bool {
    (ch as u32) <= 0x1f
}

fn escape_control_character(ch: char) -> String {
    match ch {
        '\u{08}' => "\\b".to_string(),
        '\u{0c}' => "\\f".to_string(),
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        _ => format!("\\u{:04x}", ch as u32),
    }
}

/// Port of `repairJson`: escape raw control characters inside strings and
/// double backslashes before invalid escape characters.
#[must_use]
pub fn repair_json(json: &str) -> String {
    let mut repaired = String::new();
    let mut in_string = false;
    let chars: Vec<char> = json.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if !in_string {
            repaired.push(ch);
            if ch == '"' {
                in_string = true;
            }
            index += 1;
            continue;
        }
        if ch == '"' {
            repaired.push(ch);
            in_string = false;
            index += 1;
            continue;
        }
        if ch == '\\' {
            let next_char = chars.get(index + 1);
            match next_char {
                Some('u') => {
                    let digits: String = chars[index + 2..(index + 6).min(chars.len())]
                        .iter()
                        .collect();
                    if digits.len() == 4 && digits.chars().all(|c| c.is_ascii_hexdigit()) {
                        let _ = write!(repaired, "\\u{digits}");
                        index += 6;
                    } else {
                        repaired.push_str("\\\\");
                        index += 1;
                    }
                }
                Some(&next) if VALID_JSON_ESCAPES.contains(&next) => {
                    repaired.push('\\');
                    repaired.push(next);
                    index += 2;
                }
                None | Some(_) => {
                    repaired.push_str("\\\\");
                    index += 1;
                }
            }
            continue;
        }
        if is_control_character(ch) {
            repaired.push_str(&escape_control_character(ch));
        } else {
            repaired.push(ch);
        }
        index += 1;
    }
    repaired
}

/// The arguments as JSON that parses: as written, else repaired (prime-agent's
/// `parseJsonWithRepair`); `None` when neither parses.
#[must_use]
pub fn repaired_arguments(json: &str) -> Option<String> {
    if serde_json::from_str::<serde_json::Value>(json).is_ok() {
        return Some(json.to_owned());
    }
    let repaired = repair_json(json);
    (repaired != json && serde_json::from_str::<serde_json::Value>(&repaired).is_ok())
        .then_some(repaired)
}

#[cfg(test)]
mod tests {
    use super::{repair_json, repaired_arguments};

    /// A file's content written with raw newlines and tabs, and a Windows
    /// path with single backslashes, both parse after repair.
    #[test]
    fn raw_control_characters_and_stray_backslashes_are_repaired() {
        let raw = "{\"path\": \"C:\\dir\\sub.txt\", \"content\": \"line 1\n\tline 2\"}";
        assert!(serde_json::from_str::<serde_json::Value>(raw).is_err());
        let repaired = repaired_arguments(raw).expect("repaired");
        let value: serde_json::Value = serde_json::from_str(&repaired).expect("json");
        assert_eq!(value["content"], "line 1\n\tline 2");
        assert_eq!(value["path"], "C:\\dir\\sub.txt");
    }

    #[test]
    fn valid_json_is_kept_and_hopeless_json_is_refused() {
        assert_eq!(
            repaired_arguments("{\"a\":1}").as_deref(),
            Some("{\"a\":1}")
        );
        assert_eq!(repaired_arguments("{\"a\":"), None);
        assert_eq!(repair_json("{\"a\":\"\\u0041\"}"), "{\"a\":\"\\u0041\"}");
    }
}
