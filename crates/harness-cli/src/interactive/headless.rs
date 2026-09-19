//! Headless one-turn launch.
//!
//! H01 fixes the parser contract for `ha chat --headless --prompt <text>`
//! (stdout carries the result, logs stay on stderr, no raw terminal is enabled)
//! but the application service that executes the turn belongs to `HA_LAUNCH` H04.
//! Until it is wired this entrypoint fails closed with a typed error instead of
//! printing a fabricated response or falling back to the mock provider.

use std::path::PathBuf;
use std::process::ExitCode;

use harness_types::{ErrorCode, HarnessError};

/// A validated single-turn headless request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadlessRequest {
    pub prompt: String,
    pub json: bool,
    pub cwd: Option<PathBuf>,
    pub resume: Option<String>,
}

/// Run one headless turn.
///
/// H04 turns this into an await of the application service; the signature stays
/// synchronous until there is something real to await, so the compiler cannot
/// hide a turn that never ran.
pub fn run(request: HeadlessRequest) -> Result<ExitCode, HarnessError> {
    let HeadlessRequest {
        prompt,
        json,
        cwd,
        resume,
    } = request;
    // The prompt itself is never echoed into diagnostics or logs.
    let _ = (prompt, json, cwd, resume);
    Err(HarnessError::new(
        ErrorCode::ServiceUnavailable,
        "headless chat is not wired to the application service yet (HA_LAUNCH H04); no turn was executed and no mock response was produced",
    ))
}
