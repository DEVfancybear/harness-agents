//! Codemode for ha, after pi's `@earendil-works/pi-codemode`: the model
//! writes one JavaScript program that calls the host's tools as
//! `await tools.<name>(args)`, and only what the program prints with
//! `text()`/`console.*` and what it returns reach the model. The calls it
//! makes on the way, and their results, stay out of the context.
//!
//! pi runs the script in `QuickJS` compiled to WebAssembly, in a worker
//! thread. ha runs it in an interpreter written here, on a thread of its
//! own: there is no VM, no WebAssembly and no JavaScript engine. The script
//! still has no capability but the tools the host passes in: no timers, no
//! `fetch`, no `process`, no modules, no file system.
//!
//! What a script finds (as in pi): `tools.<name>(args)` (a name that is not
//! an identifier is also reachable as `tools["my-tool"]` and as
//! `tools.my_tool`), `ALL_TOOLS`, `text(value)`, `console.log/info/warn/
//! error/debug`, `exit()`, `store(key, value)` / `load(key)` for small JSON
//! state kept across runs, and the standard built-ins (`JSON`, `Math`,
//! `Object`, `Array`, `String`, `Number`, `Promise`, `Map`, `Set`, `Date`
//! in UTC, `RegExp`, errors). A script may start with a
//! `// @options: {"timeout_ms": ..., "max_output_tokens": ...}` line.
//!
//! A tool call runs when the script awaits it; `Promise.all` hands the host
//! all of its calls at once. A call nothing awaits never runs and is
//! recorded as cancelled.

// A JavaScript number is an `f64`: lengths, indices, bit operations and
// dates become one and come back, and `===` compares floats exactly. The
// conversions follow ECMAScript (`ToIntegerOrInfinity`, `ToUint32`, ...),
// each bounded where it is made.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::float_cmp
)]

mod arrays;
pub mod ast;
mod builtins;
mod dates;
pub mod declarations;
mod heap;
pub mod identifier;
mod interp;
mod lexer;
pub mod parser;
mod runner;
pub mod source;
mod strings;
mod value;

pub use declarations::{
    DeclaredTool, render_declarations, render_tool_signature, resolve_schema_ref, schema_to_type,
};
pub use identifier::to_identifier;
pub use interp::{Fatal, Host, HostCall, MAX_STORE_TOTAL_CHARS, MAX_STORE_VALUE_CHARS};
pub use runner::{RunOptions, execute, render_result, run};
pub use source::{OPTIONS_PREFIX, ParsedSource, SourceOptions, parse_source};

/// A tool as the script sees it: `tools[js_name]` and `tools[name]`, and an
/// `ALL_TOOLS` entry `{ name: js_name, description }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolInfo {
    pub name: String,
    pub js_name: String,
    pub description: String,
}

impl ToolInfo {
    #[must_use]
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            js_name: to_identifier(&name),
            name,
            description: description.into(),
        }
    }
}

/// Where the script's code is wrong, before anything ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub message: String,
    pub line: u32,
    pub col: u32,
}

/// One item of the script's output, in the order it made them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputItem {
    pub text: String,
    /// Made by `console.*`, not `text()`.
    pub console: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallStatus {
    Ok,
    Error,
    /// Never awaited, so never run; or still running when the script ended.
    Cancelled,
}

impl CallStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallRecord {
    pub name: String,
    pub status: CallStatus,
    pub duration_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The script threw, or did not parse.
    Script,
    Timeout,
    Aborted,
    /// The runner failed outside the script's control.
    Sandbox,
}

impl ErrorKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Script => "script",
            Self::Timeout => "timeout",
            Self::Aborted => "aborted",
            Self::Sandbox => "sandbox",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptError {
    pub kind: ErrorKind,
    pub name: Option<String>,
    pub message: String,
    /// `Name: message` and the line it was thrown at, `codemode.js:<line>`.
    pub stack: Option<String>,
}

/// What `store()` changed: keys set (to JSON text) and keys deleted. Only a
/// script that succeeded reports writes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreWrites {
    pub set: Vec<(String, String)>,
    pub delete: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodemodeResult {
    /// The JSON text of what the script returned (`None` for `undefined`),
    /// or why it failed.
    pub outcome: Result<Option<String>, ScriptError>,
    /// Kept for a failed script too, up to the failure.
    pub output: Vec<OutputItem>,
    pub calls: Vec<CallRecord>,
    pub store_writes: StoreWrites,
}

impl CodemodeResult {
    #[must_use]
    pub fn ok(&self) -> bool {
        self.outcome.is_ok()
    }
}
