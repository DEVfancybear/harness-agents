//! The worker's local protocol: one JSON object per line over a loopback
//! socket, as prime-agent's daemon frames its public protocol.
//!
//! A connection presents the worker's token first. After that it asks one
//! question at a time (`create`, `list`, `send`, `stop`, ...), or it `attach`es
//! to an agent and becomes that agent's terminal: keys go in, and the effects
//! and view state the agent's controller produces come back.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::super::controller::Effect;
use super::super::events::{HistoryItem, Key, UiState};

/// The longest line either side reads: a whole conversation travels in one
/// `attached` reply.
pub const MAX_LINE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Hello {
        token: String,
    },
    /// Start an agent in this worker.
    Create(Box<CreateAgent>),
    List,
    /// Become the terminal of one agent.
    Attach {
        agent: String,
        columns: u16,
    },
    /// A key typed in the attached terminal; the reply is an `ack` with the
    /// same `seq`.
    Key {
        seq: u64,
        key: Key,
    },
    Columns {
        columns: u16,
    },
    Detach,
    /// prime-agent's `send`: a message from outside the terminal.
    Send {
        agent: String,
        text: String,
        #[serde(default)]
        from: Option<String>,
        #[serde(default)]
        mode: SendMode,
    },
    Stop {
        agent: String,
    },
    /// prime-agent's `abort`: end the agent's running turn; the agent stays.
    Abort {
        agent: String,
    },
    /// prime-agent's `get_last_assistant_text`.
    LastAnswer {
        agent: String,
    },
    Rename {
        agent: String,
        name: String,
    },
    /// Stop every agent and the worker.
    Shutdown,
    /// Stop the worker but keep its journal: the next worker - of a newer
    /// build - starts its agents again.
    Restart,
    /// prime-agent's headless session through the daemon: run one `ha exec`
    /// turn here, on the store the agents share. The replies are its output
    /// lines, then `exited` or `failed`; closing the connection cancels it.
    Exec(Box<ExecSpec>),
}

/// An `ha exec` run: the client's launch and its request.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExecSpec {
    pub caller_dir: PathBuf,
    /// The client's environment, held in memory for the run and never written.
    pub environment: Vec<(String, String)>,
    pub request: super::super::headless::HeadlessRequest,
}

/// prime-agent's delivery modes for `send`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SendMode {
    /// Steer a busy agent, start a turn on an idle one.
    #[default]
    Auto,
    Steer,
    /// Wait until the agent's current work finishes.
    FollowUp,
}

/// Everything an agent needs that the client knows: where it was opened, the
/// client's environment (credentials are read from it, as a terminal launch
/// reads them; it is held in memory and never written), and the launch
/// options.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CreateAgent {
    pub caller_dir: PathBuf,
    pub cwd: Option<PathBuf>,
    pub environment: Vec<(String, String)>,
    pub resume: Option<String>,
    pub fixture: bool,
    pub model: Option<String>,
    pub profile: Option<String>,
    pub approval: Option<String>,
    /// prime-agent's session name, unique among the agents.
    pub name: Option<String>,
    /// The first message, sent as soon as the agent starts (`rlm.create_session`).
    pub prompt: Option<String>,
    pub thinking: Option<String>,
    /// The id a recovered agent keeps; a new agent gets one.
    #[serde(default)]
    pub id: Option<String>,
}

/// One agent, as `ha agents` lists it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: Option<String>,
    /// The phase of its app: `ready`, `running`, `waiting_approval`, ...
    pub status: String,
    /// A child or a schedule is still working.
    pub busy: bool,
    pub scheduled: bool,
    pub attached: bool,
    pub project_root: PathBuf,
    pub model: String,
    pub conversation: Option<String>,
    pub last_request: Option<String>,
    pub idle_seconds: u64,
    pub worker_pid: u32,
}

impl AgentInfo {
    /// The name when it has one, else its id.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Ok {
        #[serde(default)]
        value: serde_json::Value,
    },
    Error {
        message: String,
    },
    /// The agent's conversation so far and its view, when a terminal attaches.
    Attached {
        agent: Box<AgentInfo>,
        banner: Vec<String>,
        history: Vec<HistoryItem>,
        state: Box<UiState>,
    },
    /// What the agent produced on its own.
    Frame {
        effects: Vec<Effect>,
        state: Option<Box<UiState>>,
    },
    /// What one key produced.
    Ack {
        seq: u64,
        effects: Vec<Effect>,
        state: Option<Box<UiState>>,
    },
    /// This terminal is no longer the agent's.
    Detached {
        reason: String,
    },
    /// One line an `exec` run wrote, to stdout or to stderr.
    Output {
        stderr: bool,
        text: String,
    },
    /// The `exec` run ended with this exit code.
    Exited {
        code: u8,
    },
    /// The `exec` run failed with this error.
    Failed {
        code: harness_types::ErrorCode,
        message: String,
    },
}

/// Write one message as a line.
///
/// # Errors
/// The peer is gone.
pub fn write_line(writer: &mut impl Write, message: &impl Serialize) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

/// Read one message; `None` at the end of the stream.
///
/// # Errors
/// The line is too long, is not the expected message, or the read failed.
pub fn read_line<T: for<'de> Deserialize<'de>>(
    reader: &mut impl BufRead,
) -> std::io::Result<Option<T>> {
    let mut line = Vec::new();
    let read = std::io::Read::take(&mut *reader, MAX_LINE_BYTES).read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') && read as u64 == MAX_LINE_BYTES {
        return Err(std::io::Error::other("the message is too long"));
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::{Reply, Request, SendMode, read_line, write_line};
    use crate::interactive::controller::Effect;
    use crate::interactive::events::{HistoryItem, Key};

    #[test]
    fn requests_and_replies_travel_as_lines() {
        let mut wire = Vec::new();
        write_line(
            &mut wire,
            &Request::Key {
                seq: 3,
                key: Key::Paste("a\nb".to_owned()),
            },
        )
        .expect("written");
        write_line(
            &mut wire,
            &Reply::Frame {
                effects: vec![Effect::History(HistoryItem::Notice {
                    message: "hi".to_owned(),
                })],
                state: None,
            },
        )
        .expect("written");
        let mut reader = std::io::BufReader::new(wire.as_slice());
        let Some(Request::Key { seq, key }) = read_line::<Request>(&mut reader).expect("read")
        else {
            panic!("a key request");
        };
        assert_eq!((seq, key), (3, Key::Paste("a\nb".to_owned())));
        assert!(matches!(
            read_line::<Reply>(&mut reader).expect("read"),
            Some(Reply::Frame { .. })
        ));
        assert!(read_line::<Reply>(&mut reader).expect("read").is_none());
        let send: Request =
            serde_json::from_str(r#"{"op":"send","agent":"a","text":"t"}"#).expect("parsed");
        assert!(matches!(
            send,
            Request::Send {
                mode: SendMode::Auto,
                from: None,
                ..
            }
        ));
    }
}
