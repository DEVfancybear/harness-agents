//! Trust, identity, capability and framing contracts for external extensions.
//!
//! These contracts are deliberately explicit about authority: a plugin declares
//! what it wants, the host decides what it gets, and nothing a plugin sends can
//! widen its own grant. The architecture documents leave frame and timeout
//! numbers open, so P6 freezes them here and the acceptance tests exercise them.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::PathBuf,
};

use harness_types::{ContentHash, ErrorCode, HarnessError, PluginInstanceId, ScopeId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Independently versioned external plugin protocol revision.
pub const EXTENSION_PROTOCOL_VERSION: u16 = 1;

/// Highest frame size accepted on either direction of the stdio transport.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Bounded stderr capture per extension instance.
pub const MAX_STDERR_BYTES: usize = 64 * 1024;

/// Concurrent calls allowed to one extension instance.
pub const MAX_INFLIGHT_CALLS: usize = 4;

/// A silent or slow handshake fails closed.
pub const HANDSHAKE_TIMEOUT_MS: u64 = 5_000;

/// Default per-call deadline before process-tree termination.
pub const DEFAULT_CALL_TIMEOUT_MS: u64 = 30_000;

/// Bounds one skill document read at admission.
pub const MAX_SKILL_BYTES: usize = 256 * 1024;

/// The only host methods an extension may request during a handshake.
pub const HOST_METHOD_ALLOWLIST: &[&str] = &[
    "host.echo",
    "host.workspace.observe",
    "host.task.read",
    "host.memory.search",
];

/// The environment variables a plugin process may receive, if the host has them.
pub const ENVIRONMENT_ALLOWLIST: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "SystemRoot",
    "TEMP",
    "TMP",
    "HOME",
    "LANG",
];

/// A typed extension failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ExtensionError {
    code: ErrorCode,
    message: String,
}

impl ExtensionError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<HarnessError> for ExtensionError {
    fn from(error: HarnessError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

impl From<std::io::Error> for ExtensionError {
    fn from(error: std::io::Error) -> Self {
        Self::new(ErrorCode::ExtensionProtocolError, error.to_string())
    }
}

/// One capability an extension can provide or request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionCapability {
    Tools,
    ModelProvider,
    MemoryExtractor,
    Skills,
}

impl ExtensionCapability {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::ModelProvider => "model_provider",
            Self::MemoryExtractor => "memory_extractor",
            Self::Skills => "skills",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tools" => Some(Self::Tools),
            "model_provider" => Some(Self::ModelProvider),
            "memory_extractor" => Some(Self::MemoryExtractor),
            "skills" => Some(Self::Skills),
            _ => None,
        }
    }
}

/// A capability with the contract version that implements it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityOffer {
    pub capability: ExtensionCapability,
    pub api_version: u16,
}

/// Everything a plugin states about itself before any work is admitted.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    pub schema_version: u16,
    pub plugin_id: String,
    pub implementation_version: String,
    /// Digest of the executable the host will start.
    pub executable_digest: ContentHash,
    pub host_api_version: u16,
    pub config_schema_version: u16,
    pub provides: Vec<CapabilityOffer>,
    pub requires: Vec<CapabilityOffer>,
    /// Host methods the plugin wants. A request, never authority.
    pub requested_host_methods: Vec<String>,
    /// Secret references the plugin wants resolved. Values never travel.
    pub requested_secrets: Vec<String>,
    /// Extra environment variable names the plugin wants passed through.
    pub requested_environment: Vec<String>,
    pub restart_policy: RestartPolicy,
    pub blocks_recovery_when_absent: bool,
}

impl ExtensionManifest {
    /// Validate structure only. This never executes anything and never resolves
    /// a secret, so reading a manifest from a repository is inert.
    pub fn validate(&self) -> Result<(), ExtensionError> {
        if self.schema_version != EXTENSION_PROTOCOL_VERSION {
            return Err(ExtensionError::new(
                ErrorCode::UnsupportedSchemaVersion,
                format!(
                    "extension manifest schema {} is not supported",
                    self.schema_version
                ),
            ));
        }
        if self.plugin_id.trim().is_empty() || self.implementation_version.trim().is_empty() {
            return Err(ExtensionError::new(
                ErrorCode::InvalidPayload,
                "an extension manifest requires a plugin id and implementation version",
            ));
        }
        if self.host_api_version == 0 || self.config_schema_version == 0 {
            return Err(ExtensionError::new(
                ErrorCode::InvalidPayload,
                "extension host api and config schema versions must be positive",
            ));
        }
        for offer in self.provides.iter().chain(self.requires.iter()) {
            if offer.api_version == 0 {
                return Err(ExtensionError::new(
                    ErrorCode::InvalidPayload,
                    "capability versions must be positive",
                ));
            }
        }
        for method in &self.requested_host_methods {
            if !is_allowlisted_host_method(method) {
                return Err(ExtensionError::new(
                    ErrorCode::HostMethodDenied,
                    format!("host method {method} is not allowlisted"),
                ));
            }
        }
        for name in &self.requested_environment {
            if !ENVIRONMENT_ALLOWLIST.contains(&name.as_str()) {
                return Err(ExtensionError::new(
                    ErrorCode::EnvironmentDenied,
                    format!("environment variable {name} is not allowlisted"),
                ));
            }
        }
        Ok(())
    }

    /// Parse a manifest read from disk. Reading is inert: no secret is resolved
    /// and no executable is started.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, ExtensionError> {
        let manifest: Self = serde_json::from_slice(bytes).map_err(|_| {
            ExtensionError::new(
                ErrorCode::InvalidPayload,
                "extension manifest is not valid JSON",
            )
        })?;
        manifest.validate()?;
        Ok(manifest)
    }
}

/// Whether a host method is inside the pinned allowlist.
#[must_use]
pub fn is_allowlisted_host_method(method: &str) -> bool {
    HOST_METHOD_ALLOWLIST.contains(&method)
}

/// What the host does when an extension exits.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    Never,
    OnFailure,
    Manual,
}

/// The host's explicit trust decision. Without one of these a repository or
/// profile can never cause an executable to run or a secret to be resolved.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrustGrant {
    pub plugin_id: String,
    /// The digest the user actually trusted.
    pub executable_digest: ContentHash,
    /// Capabilities the user allowed. Anything else is refused.
    pub allowed_capabilities: Vec<ExtensionCapability>,
    /// Secret references the user allowed the host to resolve.
    pub allowed_secrets: Vec<String>,
    pub granted_by: String,
}

impl TrustGrant {
    /// Structural validation of a grant itself.
    pub fn validate(&self) -> Result<(), ExtensionError> {
        if self.plugin_id.trim().is_empty() || self.granted_by.trim().is_empty() {
            return Err(ExtensionError::new(
                ErrorCode::InvalidPayload,
                "a trust grant requires a plugin id and a granting principal",
            ));
        }
        Ok(())
    }

    pub fn allows(&self, capability: ExtensionCapability) -> bool {
        self.allowed_capabilities.contains(&capability)
    }

    pub fn allows_secret(&self, reference: &str) -> bool {
        self.allowed_secrets
            .iter()
            .any(|allowed| allowed == reference)
    }

    /// Intersect a child grant with a parent grant. The result never widens.
    pub fn intersect(&self, requested: &TrustGrant) -> Result<TrustGrant, ExtensionError> {
        if requested.plugin_id != self.plugin_id {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionUntrusted,
                "a delegated trust grant cannot move to another plugin",
            ));
        }
        let capabilities = requested
            .allowed_capabilities
            .iter()
            .copied()
            .filter(|capability| self.allows(*capability))
            .collect::<Vec<_>>();
        if capabilities.len() != requested.allowed_capabilities.len() {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionCapabilityMismatch,
                "a delegated trust grant cannot add a capability the parent lacks",
            ));
        }
        let secrets = requested
            .allowed_secrets
            .iter()
            .filter(|reference| self.allows_secret(reference))
            .cloned()
            .collect::<Vec<_>>();
        if secrets.len() != requested.allowed_secrets.len() {
            return Err(ExtensionError::new(
                ErrorCode::SecretNotGranted,
                "a delegated trust grant cannot add a secret the parent lacks",
            ));
        }
        Ok(TrustGrant {
            plugin_id: self.plugin_id.clone(),
            executable_digest: self.executable_digest.clone(),
            allowed_capabilities: capabilities,
            allowed_secrets: secrets,
            granted_by: self.granted_by.clone(),
        })
    }

    /// Refuse to load when the manifest digest is not the trusted digest.
    pub fn verify_manifest(&self, manifest: &ExtensionManifest) -> Result<(), ExtensionError> {
        manifest.validate()?;
        if manifest.plugin_id != self.plugin_id {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionUntrusted,
                format!(
                    "plugin {} is not the trusted plugin {}",
                    manifest.plugin_id, self.plugin_id
                ),
            ));
        }
        if manifest.executable_digest != self.executable_digest {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionDigestMismatch,
                "the plugin digest does not match the trusted digest",
            ));
        }
        for offer in &manifest.provides {
            if !self.allows(offer.capability) {
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionCapabilityMismatch,
                    format!(
                        "capability {} is not in the trust grant",
                        offer.capability.as_str()
                    ),
                ));
            }
        }
        for reference in &manifest.requested_secrets {
            if !self.allows_secret(reference) {
                return Err(ExtensionError::new(
                    ErrorCode::SecretNotGranted,
                    format!("secret reference {reference} is not in the trust grant"),
                ));
            }
        }
        Ok(())
    }
}

/// What the host offers during a handshake.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostHandshakeOffer {
    pub protocol_min: u16,
    pub protocol_max: u16,
    pub host_api_version: u16,
    pub event_schema_version: u16,
    pub allowed_host_methods: Vec<String>,
    pub max_frame_bytes: u64,
    pub max_inflight_calls: u32,
}

impl Default for HostHandshakeOffer {
    fn default() -> Self {
        Self {
            protocol_min: EXTENSION_PROTOCOL_VERSION,
            protocol_max: EXTENSION_PROTOCOL_VERSION,
            host_api_version: 1,
            event_schema_version: 1,
            allowed_host_methods: HOST_METHOD_ALLOWLIST
                .iter()
                .map(|method| (*method).to_owned())
                .collect(),
            max_frame_bytes: MAX_FRAME_BYTES as u64,
            max_inflight_calls: u32::try_from(MAX_INFLIGHT_CALLS).unwrap_or(u32::MAX),
        }
    }
}

/// The plugin's handshake reply.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionHandshake {
    pub protocol_version: u16,
    pub plugin_id: String,
    pub implementation_version: String,
    pub executable_digest: ContentHash,
    pub capabilities: Vec<CapabilityOffer>,
    pub host_methods: Vec<String>,
    pub config_schema_version: u16,
}

/// The negotiated session. Only what both sides agreed to appears here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NegotiatedSession {
    pub protocol_version: u16,
    pub plugin_id: String,
    pub implementation_version: String,
    pub executable_digest: ContentHash,
    pub capabilities: Vec<CapabilityOffer>,
    pub host_methods: Vec<String>,
    pub config_schema_version: u16,
}

impl NegotiatedSession {
    /// Negotiate a handshake against a manifest and the host offer. Every
    /// mismatch is refused before any work is admitted.
    pub fn negotiate(
        offer: &HostHandshakeOffer,
        manifest: &ExtensionManifest,
        handshake: &ExtensionHandshake,
    ) -> Result<Self, ExtensionError> {
        if handshake.protocol_version < offer.protocol_min
            || handshake.protocol_version > offer.protocol_max
        {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionProtocolUnsupported,
                format!(
                    "plugin protocol {} is outside the supported range {}..={}",
                    handshake.protocol_version, offer.protocol_min, offer.protocol_max
                ),
            ));
        }
        if handshake.plugin_id != manifest.plugin_id
            || handshake.executable_digest != manifest.executable_digest
        {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionDigestMismatch,
                "the handshake does not describe the manifest that was trusted",
            ));
        }
        if handshake.config_schema_version != manifest.config_schema_version {
            return Err(ExtensionError::new(
                ErrorCode::SchemaVersionMismatch,
                "the handshake config schema version differs from the manifest",
            ));
        }
        let promised: BTreeMap<ExtensionCapability, u16> = manifest
            .provides
            .iter()
            .map(|offer| (offer.capability, offer.api_version))
            .collect();
        for advertised in &handshake.capabilities {
            match promised.get(&advertised.capability) {
                Some(version) if *version == advertised.api_version => {}
                Some(version) => {
                    return Err(ExtensionError::new(
                        ErrorCode::SchemaVersionMismatch,
                        format!(
                            "capability {} advertised version {} but promised {}",
                            advertised.capability.as_str(),
                            advertised.api_version,
                            version
                        ),
                    ));
                }
                None => {
                    return Err(ExtensionError::new(
                        ErrorCode::ExtensionCapabilityMismatch,
                        format!(
                            "capability {} was not promised by the manifest",
                            advertised.capability.as_str()
                        ),
                    ));
                }
            }
        }
        for method in &handshake.host_methods {
            if !is_allowlisted_host_method(method) {
                return Err(ExtensionError::new(
                    ErrorCode::HostMethodDenied,
                    format!("host method {method} is not allowlisted"),
                ));
            }
        }
        Ok(Self {
            protocol_version: handshake.protocol_version,
            plugin_id: handshake.plugin_id.clone(),
            implementation_version: handshake.implementation_version.clone(),
            executable_digest: handshake.executable_digest.clone(),
            capabilities: handshake.capabilities.clone(),
            host_methods: handshake.host_methods.clone(),
            config_schema_version: handshake.config_schema_version,
        })
    }

    #[must_use]
    pub fn provides(&self, capability: ExtensionCapability) -> bool {
        self.capabilities
            .iter()
            .any(|offer| offer.capability == capability)
    }
}

/// One NDJSON frame. A single struct keeps encode/decode and limit checking in
/// one place; `kind` distinguishes the three legal shapes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionFrame {
    pub protocol_version: u16,
    pub id: String,
    pub kind: FrameKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub payload: Value,
}

/// The three legal frame shapes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
    Message,
    Response,
    Error,
}

impl ExtensionFrame {
    #[must_use]
    pub fn message(id: impl Into<String>, method: impl Into<String>, payload: Value) -> Self {
        Self {
            protocol_version: EXTENSION_PROTOCOL_VERSION,
            id: id.into(),
            kind: FrameKind::Message,
            method: Some(method.into()),
            payload,
        }
    }

    #[must_use]
    pub fn response(id: impl Into<String>, payload: Value) -> Self {
        Self {
            protocol_version: EXTENSION_PROTOCOL_VERSION,
            id: id.into(),
            kind: FrameKind::Response,
            method: None,
            payload,
        }
    }

    #[must_use]
    pub fn error(
        id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            protocol_version: EXTENSION_PROTOCOL_VERSION,
            id: id.into(),
            kind: FrameKind::Error,
            method: None,
            payload: serde_json::json!({"code": code.into(), "message": message.into()}),
        }
    }

    /// Encode to one bounded NDJSON line. An oversize payload is refused rather
    /// than buffered.
    pub fn encode_line(&self) -> Result<Vec<u8>, ExtensionError> {
        let mut bytes = serde_json::to_vec(self).map_err(|_| {
            ExtensionError::new(
                ErrorCode::InvalidPayload,
                "extension frame is not serializable",
            )
        })?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(ExtensionError::new(
                ErrorCode::FrameLimitExceeded,
                format!(
                    "frame of {} bytes exceeds the {MAX_FRAME_BYTES} byte limit",
                    bytes.len()
                ),
            ));
        }
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Decode one NDJSON line with the frame limit applied before parsing.
    pub fn decode_line(line: &[u8]) -> Result<Self, ExtensionError> {
        if line.len() > MAX_FRAME_BYTES {
            return Err(ExtensionError::new(
                ErrorCode::FrameLimitExceeded,
                format!(
                    "frame of {} bytes exceeds the {MAX_FRAME_BYTES} byte limit",
                    line.len()
                ),
            ));
        }
        let text = std::str::from_utf8(line).map_err(|_| {
            ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                "frame is not valid UTF-8",
            )
        })?;
        let frame: Self =
            serde_json::from_str(text.trim_end_matches(['\r', '\n'])).map_err(|_| {
                ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    "frame is not a valid extension envelope",
                )
            })?;
        if frame.protocol_version != EXTENSION_PROTOCOL_VERSION {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionProtocolUnsupported,
                format!("frame protocol {} is not supported", frame.protocol_version),
            ));
        }
        if frame.kind == FrameKind::Message && frame.method.is_none() {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                "a message frame requires a method",
            ));
        }
        Ok(frame)
    }
}

/// A loaded extension instance as the host sees it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionRegistration {
    pub instance_id: PluginInstanceId,
    pub scope_id: ScopeId,
    pub generation: u64,
    pub session: NegotiatedSession,
    pub executable: PathBuf,
}

/// Why an extension is not active. Reported honestly instead of guessing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InactiveReason {
    NoTrustGrant,
    DigestMismatch,
    CapabilityNotGranted,
    SecretNotGranted,
    HandshakeRejected,
    ExitBeforeHandshake,
    Unloaded,
    RestartRequired,
}

impl InactiveReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoTrustGrant => "no_trust_grant",
            Self::DigestMismatch => "digest_mismatch",
            Self::CapabilityNotGranted => "capability_not_granted",
            Self::SecretNotGranted => "secret_not_granted",
            Self::HandshakeRejected => "handshake_rejected",
            Self::ExitBeforeHandshake => "exit_before_handshake",
            Self::Unloaded => "unloaded",
            Self::RestartRequired => "restart_required",
        }
    }
}

/// One entry of the host inventory of known extensions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionInventoryEntry {
    pub plugin_id: String,
    pub implementation_version: Option<String>,
    pub state: &'static str,
    pub generation: u64,
    pub inactive_reason: Option<InactiveReason>,
    pub protocols: Vec<u16>,
    pub capabilities: Vec<ExtensionCapability>,
}

/// A typed denial recorded for a refused request. Denial is monotonic and must
/// survive into durable evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionDenial {
    pub code: ErrorCode,
    pub reason: String,
}

impl ExtensionDenial {
    #[must_use]
    pub fn new(code: ErrorCode, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
        }
    }
}

impl fmt::Display for ExtensionDenial {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.reason)
    }
}

/// The set of host methods actually reachable in this build. Used by
/// `config explain` and the compatibility inventory so a claim is never implied.
#[must_use]
pub fn supported_host_methods() -> BTreeSet<String> {
    HOST_METHOD_ALLOWLIST
        .iter()
        .map(|method| (*method).to_owned())
        .collect()
}

/// Protocol revisions this build understands.
#[must_use]
pub const fn supported_protocol_versions() -> [u16; 1] {
    [EXTENSION_PROTOCOL_VERSION]
}
