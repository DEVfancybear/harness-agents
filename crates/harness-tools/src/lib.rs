#![forbid(unsafe_code)]

//! P3 coding-tool boundary.
//!
//! This crate owns the only executable coding-tool gate. Model and CLI input
//! are proposals; they become a side effect only after canonicalization,
//! policy, final-action approval, a durable intent, and a durable receipt.

mod contracts;
mod loop_service;
mod policy;
mod process;
mod service;
mod workspace;

pub use contracts::{
    ApprovalGrant, CodingToolAction, IsolationMode, PreparedToolRequest, TOOL_CONTRACT_VERSION,
    ToolCapabilities, ToolExecutionView, ToolKind, ToolOutput, ToolRequest, coding_tool_schemas,
};
pub use loop_service::{CodingLoopResult, CodingLoopService};
pub use policy::{PolicyEffect, PolicyRule, ToolPolicy};
pub use service::{ToolExecutionService, ToolObserver};
pub use workspace::{observe_workspace, observed_file_hash};
