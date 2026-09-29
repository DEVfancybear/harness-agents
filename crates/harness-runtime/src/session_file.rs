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
        let prime = "{\"type\":\"session\",\"version\":3,\"id\":\"x\"}\n";
        assert!(
            read_session_file(prime)
                .expect_err("not ha")
                .contains("not an ha session header")
        );
        let broken = "{\"type\":\"session\",\"version\":1,\"app\":\"ha\"}\nnot json\n";
        assert_eq!(
            read_session_file(broken).expect_err("broken"),
            "line 2 is not JSON"
        );
    }
}
