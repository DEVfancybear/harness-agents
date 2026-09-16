#![forbid(unsafe_code)]

//! P6 external extension support: trust and capability contracts, a bounded
//! NDJSON stdio transport, external tool and model-provider bridges, the MCP
//! client adapter, and versioned skill/profile composition.
//!
//! External tools are reachable **only** through the existing P3 policy gate.
//! This crate adds a transport and a dispatch hook, never a second
//! authorization path, and it never advertises transport isolation as OS
//! sandboxing.

pub mod bridges;
pub mod contracts;
pub mod host;
pub mod mcp;
pub mod skills;
pub mod transport;

pub use bridges::{ConfigInspection, ExtensionProvider, ExtensionToolDispatcher};
pub use contracts::{
    CapabilityOffer, DEFAULT_CALL_TIMEOUT_MS, ENVIRONMENT_ALLOWLIST, EXTENSION_PROTOCOL_VERSION,
    ExtensionCapability, ExtensionDenial, ExtensionError, ExtensionFrame, ExtensionHandshake,
    ExtensionInventoryEntry, ExtensionManifest, ExtensionRegistration, FrameKind,
    HANDSHAKE_TIMEOUT_MS, HOST_METHOD_ALLOWLIST, HostHandshakeOffer, InactiveReason,
    MAX_FRAME_BYTES, MAX_INFLIGHT_CALLS, MAX_SKILL_BYTES, MAX_STDERR_BYTES, NegotiatedSession,
    RestartPolicy, TrustGrant, is_allowlisted_host_method, supported_host_methods,
    supported_protocol_versions,
};
pub use host::{ExtensionRuntime, LoadOutcome};
pub use mcp::{McpClient, McpToolDescriptor};
pub use skills::{
    SkillDescriptor, SkillSource, SkillUpdate, SkillVersionPin, compose_skills, discover_skills,
};
pub use transport::{
    CallOutcome, EchoOnlyHost, EnvironmentOverrides, ExtensionTransport, HostMethodHandler,
    executable_digest,
};
