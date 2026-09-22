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
pub mod catalog;
pub mod config;
pub mod contracts;
pub mod host;
pub mod mcp;
pub mod skills;
pub mod transport;

pub use bridges::{ConfigInspection, ExtensionProvider, ExtensionToolDispatcher};
pub use catalog::{
    CatalogEntry, CatalogNameView, CatalogSource, MAX_CATALOG_ENTRIES, MAX_PROMOTED_SCHEMA_BYTES,
    PromotedTool, ToolCatalog, ToolContributor, catalog_from_descriptors, shared_catalog,
};
pub use config::{
    ConfigExplain, ConfigLayer, ConfigLayerKind, EffectiveEntry, InactivePluginView,
    RESTART_REQUIRED_PREFIXES, ReloadBoundary, explain_config,
};
pub use contracts::{
    CANCEL_GRACE_MS, CapabilityOffer, DEFAULT_CALL_TIMEOUT_MS, ENVIRONMENT_ALLOWLIST,
    EXTENSION_PROTOCOL_VERSION, ExtensionCapability, ExtensionDenial, ExtensionError,
    ExtensionFrame, ExtensionHandshake, ExtensionInventoryEntry, ExtensionManifest,
    ExtensionRegistration, FrameKind, HANDSHAKE_TIMEOUT_MS, HOST_METHOD_ALLOWLIST,
    HostHandshakeOffer, InactiveReason, MAX_FRAME_BYTES, MAX_INFLIGHT_CALLS, MAX_SKILL_BYTES,
    MAX_STDERR_BYTES, NegotiatedSession, RestartPolicy, TrustGrant, is_allowlisted_host_method,
    supported_host_methods, supported_protocol_versions,
};
pub use host::{ExtensionLease, ExtensionRuntime, LoadOutcome, UnloadReport};
pub use mcp::{
    MCP_CALL_TIMEOUT_MS, MCP_DISCOVERY_TIMEOUT_MS, MCP_MAX_PAGES, MCP_MAX_RESOURCES, MCP_MAX_TOOLS,
    MCP_READ_TIMEOUT_MS, MCP_SDK_VERSION, MCP_SPEC_REVISION, McpClient, McpFeature,
    McpMetadataCache, McpResourceContent, McpResourceDescriptor, McpResourceProvenance, McpRuntime,
    McpSupportMatrix, McpToolDescriptor, McpToolDispatcher, validate_arguments,
    validate_tool_schema,
};
pub use skills::{
    MAX_SKILL_CATALOG_ENTRIES, MAX_SKILL_HEAD_BYTES, SkillActivation, SkillCatalog,
    SkillCatalogEntry, SkillConflict, SkillContributor, SkillDescriptor, SkillSource, SkillUpdate,
    SkillVersionPin, TrustedSkillRoot, compose_skills, discover_skills,
};
pub use transport::{
    CallOutcome, CancelOutcome, EchoOnlyHost, EnvironmentOverrides, ExtensionTransport,
    HostMethodHandler, executable_digest,
};
