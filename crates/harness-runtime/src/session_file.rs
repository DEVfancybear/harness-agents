//! ha's session file, in prime-agent's JSONL shape (`session-manager.ts`): a
//! header line, then one line per message. `/export <file>.jsonl` writes it and
//! `/import <file>.jsonl` reads it back as the start of a new conversation.
//!
//! ```text
//! {"type":"session","version":1,"app":"ha","id":"...","timestamp":1727000000000,"cwd":"..."}
//! {"type":"message","message":{"role":"user","content":"..."}}
//! {"type":"message","message":{"role":"assistant","content":"..."}}
//! ```

use harness_providers::{MessageRole, ProviderMessage};
use serde_json::{Value, json};

/// The version this build writes and reads.
pub const SESSION_FILE_VERSION: u64 = 1;

/// The task setting that names a conversation's imported file: its turns
/// continue the messages that file holds.
pub const IMPORTED_HISTORY_SETTING: &str = "imported_history";

/// What a session file's header says.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionFileHeader {
    pub id: String,
    pub timestamp_ms: i64,
    pub cwd: String,
    pub title: Option<String>,
}

/// Write `messages` as a session file.
///
/// # Errors
/// A message cannot be serialized.
pub fn write_session_file(
    header: &SessionFileHeader,
    messages: &[ProviderMessage],
) -> Result<String, String> {
    let mut first = json!({
        "type": "session",
        "version": SESSION_FILE_VERSION,
        "app": "ha",
        "id": header.id,
        "timestamp": header.timestamp_ms,
        "cwd": header.cwd,
    });
    if let Some(title) = &header.title {
        first["title"] = json!(title);
    }
    let mut out = serde_json::to_string(&first).map_err(|error| error.to_string())?;
    out.push('\n');
    for message in messages {
        // Reasoning is never stored, and images stay out of a text file.
        let mut message = message.clone();
        message.attachments.clear();
        message.reasoning = None;
        let line = json!({"type": "message", "message": message});
        out.push_str(&serde_json::to_string(&line).map_err(|error| error.to_string())?);
        out.push('\n');
    }
    Ok(out)
}

/// Read a session file: its header and its messages, in order.
///
/// # Errors
/// The first line is not an ha session header of a version this build reads,
/// or a line is not JSON; the error names the line.
pub fn read_session_file(text: &str) -> Result<(SessionFileHeader, Vec<ProviderMessage>), String> {
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty());
    let (_, first) = lines.next().ok_or("the file is empty")?;
    let header: Value =
        serde_json::from_str(first).map_err(|_| "line 1 is not a session header".to_owned())?;
    // A prime-agent session file (no `app`, format version 2 or 3).
    if header["type"] == "session" && header["app"].is_null() {
        return read_prime_session(&header, lines);
    }
    if header["type"] != "session" || header["app"] != "ha" {
        return Err(
            "line 1 is not an ha session header (export one with /export <file>.jsonl)".to_owned(),
        );
    }
    let version = header["version"].as_u64().unwrap_or_default();
    if version != SESSION_FILE_VERSION {
        return Err(format!(
            "session file version {version} is not supported (this build reads version {SESSION_FILE_VERSION})"
        ));
    }
    let parsed = SessionFileHeader {
        id: header["id"].as_str().unwrap_or_default().to_owned(),
        timestamp_ms: header["timestamp"].as_i64().unwrap_or_default(),
        cwd: header["cwd"].as_str().unwrap_or_default().to_owned(),
        title: header["title"].as_str().map(str::to_owned),
    };
    let mut messages = Vec::new();
    for (index, line) in lines {
        let value: Value =
            serde_json::from_str(line).map_err(|_| format!("line {} is not JSON", index + 1))?;
        // Entries other than messages (a later version's) are skipped, as
        // prime-agent skips entry types it does not know.
        if value["type"] != "message" {
            continue;
        }
        let message: ProviderMessage = serde_json::from_value(value["message"].clone())
            .map_err(|_| format!("line {} is not a message", index + 1))?;
        messages.push(message);
    }
    Ok((parsed, messages))
}

/// Read a prime-agent session file (`pa-types` `FileEntry`): the branch the
/// newest entry ends, walked back through `parentId`, with the latest
/// compaction on it applied as prime builds a context - its summary, then the
/// entries it kept, then the ones after it.
fn read_prime_session<'a>(
    header: &Value,
    lines: impl Iterator<Item = (usize, &'a str)>,
) -> Result<(SessionFileHeader, Vec<ProviderMessage>), String> {
    let mut entries = Vec::new();
    for (index, line) in lines {
        let value: Value =
            serde_json::from_str(line).map_err(|_| format!("line {} is not JSON", index + 1))?;
        entries.push(value);
    }
    // The branch: from the newest entry back to the root. A file without
    // entry ids (version 1) is one line of entries already.
    let by_id = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| Some((entry["id"].as_str()?.to_owned(), index)))
        .collect::<std::collections::HashMap<_, _>>();
    let path = if by_id.is_empty() {
        (0..entries.len()).collect::<Vec<_>>()
    } else {
        let mut path = Vec::new();
        let mut at = entries.iter().rposition(|entry| entry["id"].is_string());
        let mut seen = std::collections::HashSet::new();
        while let Some(index) = at {
            if !seen.insert(index) {
                break;
            }
            path.push(index);
            at = entries[index]["parentId"]
                .as_str()
                .and_then(|parent| by_id.get(parent).copied());
        }
        path.reverse();
        path
    };
    let mut messages = Vec::new();
    let compaction = path
        .iter()
        .rposition(|index| entries[*index]["type"] == "compaction");
    let kept = match compaction {
        Some(position) => {
            let entry = &entries[path[position]];
            messages.push(ProviderMessage::new(
                MessageRole::User,
                format!(
                    "[The conversation before this point was compacted. Summary of it:]\n\n{}",
                    entry["summary"].as_str().unwrap_or_default()
                ),
            ));
            let first_kept = entry["firstKeptEntryId"].as_str().unwrap_or_default();
            let start = path[..position]
                .iter()
                .position(|index| entries[*index]["id"] == first_kept)
                .unwrap_or(position);
            path[start..position]
                .iter()
                .chain(&path[position + 1..])
                .copied()
                .collect::<Vec<_>>()
        }
        None => path,
    };
    for index in kept {
        let entry = &entries[index];
        match entry["type"].as_str() {
            Some("message") => messages.extend(prime_message(&entry["message"])),
            Some("branch_summary") => {
                if let Some(summary) = entry["summary"].as_str() {
                    messages.push(ProviderMessage::new(
                        MessageRole::User,
                        format!("[Summary of a branch of this conversation:]\n\n{summary}"),
                    ));
                }
            }
            _ => {}
        }
    }
    let parsed = SessionFileHeader {
        id: header["id"].as_str().unwrap_or_default().to_owned(),
        timestamp_ms: 0,
        cwd: header["cwd"].as_str().unwrap_or_default().to_owned(),
        title: None,
    };
    Ok((parsed, messages))
}

/// The text of prime-agent message content: a string, or the text blocks of
/// a block list.
fn prime_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// One prime-agent `AgentMessage` as the messages ha sends a model.
fn prime_message(message: &Value) -> Option<ProviderMessage> {
    match message["role"].as_str()? {
        "user" | "custom" => {
            let text = prime_text(&message["content"]);
            (!text.trim().is_empty()).then(|| ProviderMessage::new(MessageRole::User, text))
        }
        "assistant" => {
            let blocks = message["content"].as_array().cloned().unwrap_or_default();
            let text = prime_text(&message["content"]);
            let calls = blocks
                .iter()
                .filter(|block| block["type"] == "toolCall")
                .map(|block| {
                    harness_providers::ProviderToolCall::new(
                        block["id"].as_str().unwrap_or_default(),
                        block["name"].as_str().unwrap_or_default(),
                        block["arguments"].to_string(),
                    )
                })
                .collect::<Vec<_>>();
            if text.trim().is_empty() && calls.is_empty() {
                return None;
            }
            Some(ProviderMessage::assistant_with_calls(text, calls))
        }
        "toolResult" => Some(ProviderMessage::tool_result(
            message["toolCallId"].as_str().unwrap_or_default(),
            prime_text(&message["content"]),
        )),
        "bashExecution" => Some(ProviderMessage::new(
            MessageRole::User,
            format!(
                "[The user ran `{}`]\n{}",
                message["command"].as_str().unwrap_or_default(),
                message["output"].as_str().unwrap_or_default()
            ),
        )),
        "branchSummary" | "compactionSummary" => message["summary"]
            .as_str()
            .map(|summary| ProviderMessage::new(MessageRole::User, summary.to_owned())),
        _ => None,
    }
}

/// The note that leads an imported conversation, so the model knows where the
/// earlier turns came from.
#[must_use]
pub fn imported_note(header: &SessionFileHeader) -> ProviderMessage {
    ProviderMessage::new(
        MessageRole::System,
        format!(
            "The conversation below was imported from a session file{}; it continues here.",
            header
                .title
                .as_deref()
                .map(|title| format!(" ({title})"))
                .unwrap_or_default()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{SessionFileHeader, read_session_file, write_session_file};
    use harness_providers::{MessageRole, ProviderMessage, ProviderToolCall};

    #[test]
    fn a_written_file_reads_back_the_same_messages() {
        let header = SessionFileHeader {
            id: "task-1".to_owned(),
            timestamp_ms: 7,
            cwd: "C:/work".to_owned(),
            title: Some("agents test".to_owned()),
        };
        let messages = vec![
            ProviderMessage::new(MessageRole::User, "list files"),
            ProviderMessage::assistant_with_calls(
                "",
                vec![ProviderToolCall::new("call_1", "list_files", "{}")],
            ),
            ProviderMessage::tool_result("call_1", "a.rs"),
            ProviderMessage::new(MessageRole::Assistant, "one file: a.rs"),
        ];
        let text = write_session_file(&header, &messages).expect("written");
        assert_eq!(
            text.lines().count(),
            5,
            "a header and one line per message: {text}"
        );
        let (read, back) = read_session_file(&text).expect("read");
        assert_eq!(read, header);
        assert_eq!(back, messages);
    }

    #[test]
    fn a_file_that_is_not_an_ha_session_is_refused_with_its_line() {
        assert!(read_session_file("").is_err());
        let other = "{\"type\":\"transcript\",\"id\":\"x\"}\n";
        assert!(
            read_session_file(other)
                .expect_err("not a session")
                .contains("not an ha session header")
        );
        let broken = "{\"type\":\"session\",\"version\":1,\"app\":\"ha\"}\nnot json\n";
        assert_eq!(
            read_session_file(broken).expect_err("broken"),
            "line 2 is not JSON"
        );
    }

    /// A prime-agent v3 file is read along the branch its newest entry ends,
    /// with the latest compaction applied: its summary, the entries it kept,
    /// then the ones after it. An abandoned branch is left out.
    #[test]
    fn a_prime_agent_session_is_imported_along_its_branch() {
        let lines = [
            r#"{"type":"session","version":3,"id":"s1","timestamp":"2026-10-01T00:00:00Z","cwd":"/work"}"#,
            r#"{"type":"message","id":"a","parentId":null,"message":{"role":"user","content":"old question"}}"#,
            r#"{"type":"message","id":"b","parentId":"a","message":{"role":"assistant","content":[{"type":"text","text":"old answer"}]}}"#,
            r#"{"type":"message","id":"c","parentId":"b","message":{"role":"user","content":[{"type":"text","text":"read it"}]}}"#,
            r#"{"type":"message","id":"d","parentId":"c","message":{"role":"assistant","content":[{"type":"toolCall","id":"t1","name":"read","arguments":{"path":"a.rs"}}]}}"#,
            r#"{"type":"message","id":"e","parentId":"d","message":{"role":"toolResult","toolCallId":"t1","toolName":"read","content":[{"type":"text","text":"fn main() {}"}]}}"#,
            r#"{"type":"compaction","id":"f","parentId":"e","summary":"the user asked about a.rs","firstKeptEntryId":"c","tokensBefore":9000}"#,
            r#"{"type":"message","id":"g","parentId":"f","message":{"role":"user","content":"abandoned"}}"#,
            r#"{"type":"message","id":"h","parentId":"f","message":{"role":"assistant","content":[{"type":"text","text":"it is empty"}]}}"#,
        ];
        let (header, messages) = read_session_file(&lines.join("\n")).expect("a prime session");
        assert_eq!(header.id, "s1");
        assert_eq!(header.cwd, "/work");
        let texts = messages
            .iter()
            .map(|message| (message.role, message.content.as_str()))
            .collect::<Vec<_>>();
        assert!(texts[0].1.contains("the user asked about a.rs"));
        assert_eq!(texts[1], (MessageRole::User, "read it"));
        assert_eq!(messages[2].tool_calls[0].name, "read");
        assert_eq!(texts[3], (MessageRole::Tool, "fn main() {}"));
        assert_eq!(texts[4], (MessageRole::Assistant, "it is empty"));
        assert_eq!(
            messages.len(),
            5,
            "the old turns and the abandoned branch are left out"
        );
    }
}
