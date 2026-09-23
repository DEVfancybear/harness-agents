//! What a single execution is actually granted, and what the backend cannot
//! grant it.
//!
//! A [`CapabilityMatrix`] says what the *host* can enforce. An [`ExecutionPlan`]
//! says what *this call* asked for, which of those requests a backend control
//! maps onto, and which ones have no control at all. The second list is the
//! point: a plan that cannot name its unmapped requests is a plan that hides
//! them.
//!
//! Scope checking here is **policy**, not enforcement (ADR-N12 D5). Resolving a
//! path and refusing one that leaves the workspace is what the gate has always
//! done; it does not stop a child process from opening that path itself, and
//! nothing in this module may be quoted as if it did.

use std::path::{Path, PathBuf};

use harness_types::{ErrorCode, HarnessError};
use serde::{Deserialize, Serialize};

use crate::{CodingToolAction, PROCESS_ENVIRONMENT_ALLOWLIST};

use super::capability::{Capability, CapabilityMatrix, StrictProfile};

/// Version of the plan shape.
pub const EXECUTION_PLAN_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeAccess {
    Read,
    Write,
}

/// One path this execution is granted, as the plan records it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScopeGrant {
    pub path: String,
    pub access: ScopeAccess,
}

/// A request the plan could not map onto a backend control.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnmappedControl {
    /// The control a confinement backend would use, or that this call wished
    /// for: `filesystem_write`, `network_egress`, `credential_socket`, …
    pub control: String,
    /// The capability that would have to be enforced for it to exist.
    pub capability: Capability,
    pub reason: String,
}

/// The environment a call's child will see, named rather than valued.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnvironmentPlan {
    /// Names copied from the host allowlist.
    pub allowlisted: Vec<String>,
    /// `secret://` references this call resolved from grants.
    pub granted: Vec<String>,
}

/// What the plan records about a single execution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionPlan {
    pub schema_version: u16,
    pub backend: String,
    pub backend_version: String,
    pub profile: StrictProfile,
    pub run_root: String,
    pub grants: Vec<ScopeGrant>,
    pub environment: EnvironmentPlan,
    pub deadline_ms: u64,
    pub output_quota_bytes: u64,
    /// Capabilities this host measured as enforced.
    pub enforced: Vec<Capability>,
    /// Capabilities this host does not have, and therefore does not claim.
    pub not_claimed: Vec<Capability>,
    /// Requests with no backend control behind them.
    pub unmapped: Vec<UnmappedControl>,
}

impl ExecutionPlan {
    /// Map a process action onto the backend's controls.
    ///
    /// `deadline_ms` and `output_quota_bytes` are the bounds the runner will
    /// apply; `env_bindings` are the `secret://` references the policy already
    /// granted, named and never valued.
    #[must_use]
    pub fn for_process_action(
        action: &CodingToolAction,
        run_root: &Path,
        matrix: &CapabilityMatrix,
        profile: StrictProfile,
        deadline_ms: u64,
        output_quota_bytes: u64,
    ) -> Self {
        let environment = EnvironmentPlan {
            allowlisted: PROCESS_ENVIRONMENT_ALLOWLIST
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            granted: env_references(action),
        };
        let mut unmapped = Vec::new();
        for capability in [
            Capability::FilesystemWriteConfinement,
            Capability::FilesystemReadConfinement,
        ] {
            if !matrix.is_enforced(capability) {
                unmapped.push(UnmappedControl {
                    control: if capability == Capability::FilesystemWriteConfinement {
                        "filesystem_write_scope".to_owned()
                    } else {
                        "filesystem_read_scope".to_owned()
                    },
                    capability,
                    reason: format!(
                        "the granted workspace is a policy scope, not a backend control: this host does not enforce {} (see {} )",
                        capability.as_str(),
                        matrix
                            .evidence(capability)
                            .map_or("no probe", |evidence| evidence.probe_id.as_str())
                    ),
                });
            }
        }
        if !matrix.is_enforced(Capability::NetworkEgressDenial) {
            unmapped.push(UnmappedControl {
                control: "network_egress".to_owned(),
                capability: Capability::NetworkEgressDenial,
                reason:
                    "the backend applies no egress rule; a child process keeps this user's network access"
                        .to_owned(),
            });
        }
        if !matrix.is_enforced(Capability::CredentialSocketDenial) {
            unmapped.push(UnmappedControl {
                control: "credential_socket".to_owned(),
                capability: Capability::CredentialSocketDenial,
                reason:
                    "the backend applies no socket rule; a child process can open the host's pipes"
                        .to_owned(),
            });
        }
        Self {
            schema_version: EXECUTION_PLAN_SCHEMA_VERSION,
            backend: matrix.host.backend.clone(),
            backend_version: matrix.host.backend_version.clone(),
            profile,
            run_root: run_root.to_string_lossy().replace('\\', "/"),
            grants: vec![ScopeGrant {
                path: run_root.to_string_lossy().replace('\\', "/"),
                access: ScopeAccess::Write,
            }],
            environment,
            deadline_ms,
            output_quota_bytes,
            enforced: matrix.enforced(),
            not_claimed: matrix.unsupported(),
            unmapped,
        }
    }

    /// Whether this plan promises anything the backend does not control.
    ///
    /// A profile is servable when every capability it requires is enforced. The
    /// unmapped list is what makes that answer readable instead of implicit.
    #[must_use]
    pub fn promises_uncontrolled(&self) -> bool {
        self.unmapped
            .iter()
            .any(|control| self.profile.required().contains(&control.capability))
    }

    /// The typed refusal for a plan whose profile cannot be served here.
    #[must_use]
    pub fn refusal(&self, matrix: &CapabilityMatrix) -> Option<HarnessError> {
        if !self.promises_uncontrolled() {
            return None;
        }
        let uncontrolled = self
            .unmapped
            .iter()
            .map(|control| control.control.clone())
            .collect::<Vec<_>>()
            .join(",");
        let missing = matrix
            .missing_for(self.profile)
            .iter()
            .map(|capability| capability.as_str())
            .collect::<Vec<_>>()
            .join(",");
        Some(HarnessError::new(
            ErrorCode::StrictIsolationUnavailable,
            format!(
                "profile {} has no backend control for [{uncontrolled}] on {} {}; missing capabilities [{missing}]",
                self.profile.as_str(),
                self.backend,
                self.backend_version
            ),
        ))
    }
}

/// The `secret://` references an action binds, in declaration order.
fn env_references(action: &CodingToolAction) -> Vec<String> {
    match action {
        CodingToolAction::RunProcess { env, .. } | CodingToolAction::RunShell { env, .. } => env
            .iter()
            .map(|binding| binding.reference.clone())
            .collect(),
        _ => Vec::new(),
    }
}

/// Resolve a path that came from a record, refusing to leave `root`.
///
/// This is the *anti-traversal* rule for anything the host writes or reads by a
/// stored name (an artifact's relative path, an export destination): an
/// absolute path, a `..` component, or a Windows drive/UNC prefix is refused
/// rather than normalized into something plausible.
pub fn resolve_within(root: &Path, relative: &str) -> Result<PathBuf, HarnessError> {
    let trimmed = relative.trim();
    if trimmed.is_empty() {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "a stored relative path must not be empty",
        ));
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return Err(HarnessError::new(
            ErrorCode::ArtifactWriteFailed,
            format!("path {trimmed} is absolute; only a path inside the artifact root is allowed"),
        ));
    }
    let mut resolved = root.to_owned();
    for component in candidate.components() {
        match component {
            std::path::Component::Normal(part) => resolved.push(part),
            std::path::Component::CurDir => {}
            other => {
                return Err(HarnessError::new(
                    ErrorCode::ArtifactWriteFailed,
                    format!(
                        "path {trimmed} contains {other:?}, which would leave the artifact root"
                    ),
                ));
            }
        }
    }
    Ok(resolved)
}
