//! What this host can enforce, as measured data rather than as a claim.
//!
//! M12's whole contract is in this file: a [`CapabilityMatrix`] carries one
//! [`CapabilityFinding`] per capability, each with the probe that produced it
//! and the observation it produced. A capability is only ever `enforced`
//! because a probe saw the enforcement hold, and only ever `unsupported`
//! because a probe saw the boundary fail or because the API that would provide
//! it does not exist in the pinned backend. "Not tested" is not a verdict.

use std::time::{SystemTime, UNIX_EPOCH};

use harness_types::{ErrorCode, HarnessError};
use serde::{Deserialize, Serialize};

/// Version of the matrix shape itself (not of the host it describes).
pub const CAPABILITY_MATRIX_SCHEMA_VERSION: u16 = 1;

/// The containment backend this workspace pins, named with its version.
///
/// A library version cannot be read at run time, so it is written here and
/// *checked* by [`tests::the_pinned_backend_version_matches_the_lockfile`]: the
/// constant cannot drift away from `Cargo.lock` without a red test.
pub const CONTAINMENT_BACKEND: &str = "process-wrap";
pub const CONTAINMENT_BACKEND_VERSION: &str = "10.0.0";

/// One thing a strict execution backend may or may not be able to enforce.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// A process cannot leave the container it was started in.
    ProcessContainment,
    /// Terminating the container terminates every descendant.
    ProcessTreeKill,
    /// The child's environment is the allowlist plus granted bindings, nothing else.
    EnvironmentAllowlist,
    /// A deadline is enforced by killing the container, not by asking nicely.
    DeadlineEnforced,
    /// Captured output is bounded by a quota.
    OutputBounds,
    /// The child cannot read files outside the granted scope.
    FilesystemReadConfinement,
    /// The child cannot write files outside the granted scope.
    FilesystemWriteConfinement,
    /// The child cannot open a network connection.
    NetworkEgressDenial,
    /// The child cannot open the host's credential sockets/pipes.
    CredentialSocketDenial,
    /// The child cannot exceed a memory cap.
    ResourceLimitMemory,
    /// The child cannot exceed a process-count cap.
    ResourceLimitProcessCount,
}

impl Capability {
    pub const ALL: [Self; 11] = [
        Self::ProcessContainment,
        Self::ProcessTreeKill,
        Self::EnvironmentAllowlist,
        Self::DeadlineEnforced,
        Self::OutputBounds,
        Self::FilesystemReadConfinement,
        Self::FilesystemWriteConfinement,
        Self::NetworkEgressDenial,
        Self::CredentialSocketDenial,
        Self::ResourceLimitMemory,
        Self::ResourceLimitProcessCount,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProcessContainment => "process_containment",
            Self::ProcessTreeKill => "process_tree_kill",
            Self::EnvironmentAllowlist => "environment_allowlist",
            Self::DeadlineEnforced => "deadline_enforced",
            Self::OutputBounds => "output_bounds",
            Self::FilesystemReadConfinement => "filesystem_read_confinement",
            Self::FilesystemWriteConfinement => "filesystem_write_confinement",
            Self::NetworkEgressDenial => "network_egress_denial",
            Self::CredentialSocketDenial => "credential_socket_denial",
            Self::ResourceLimitMemory => "resource_limit_memory",
            Self::ResourceLimitProcessCount => "resource_limit_process_count",
        }
    }

    /// Whether this capability confines *what the child can reach*, as opposed
    /// to containing its lifecycle or bounding its output.
    ///
    /// The distinction is the one ADR-N12 D4 fixes in vocabulary: a host runner
    /// that contains a process tree has not confined the filesystem, the
    /// network or a credential socket.
    #[must_use]
    pub const fn is_confinement(self) -> bool {
        matches!(
            self,
            Self::FilesystemReadConfinement
                | Self::FilesystemWriteConfinement
                | Self::NetworkEgressDenial
                | Self::CredentialSocketDenial
        )
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_str() == text)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityVerdict {
    /// A probe observed the enforcement hold on this host.
    Enforced,
    /// A probe observed the boundary fail, or the API is absent from the
    /// pinned backend. Never "not measured".
    Unsupported,
}

/// The probe that produced a verdict, and what it saw.
///
/// `observation` is the load-bearing field: it is what a reader checks instead
/// of trusting the verdict. It is never empty, and for [`CapabilityVerdict::Unsupported`]
/// it names the escape that was observed or the structural reason there is
/// nothing to enforce with.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityEvidence {
    pub probe_id: String,
    pub method: String,
    pub observation: String,
    pub observed_at_unix_ms: u64,
}

impl CapabilityEvidence {
    #[must_use]
    pub fn new(probe_id: &str, method: &str, observation: impl Into<String>) -> Self {
        Self {
            probe_id: probe_id.to_owned(),
            method: method.to_owned(),
            observation: observation.into(),
            observed_at_unix_ms: now_unix_ms(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityFinding {
    pub capability: Capability,
    pub verdict: CapabilityVerdict,
    pub evidence: CapabilityEvidence,
}

impl CapabilityFinding {
    #[must_use]
    pub fn enforced(capability: Capability, evidence: CapabilityEvidence) -> Self {
        Self {
            capability,
            verdict: CapabilityVerdict::Enforced,
            evidence,
        }
    }

    #[must_use]
    pub fn unsupported(capability: Capability, evidence: CapabilityEvidence) -> Self {
        Self {
            capability,
            verdict: CapabilityVerdict::Unsupported,
            evidence,
        }
    }
}

/// The environment a matrix describes.
///
/// A verdict measured on one host says nothing about another, so the identity
/// travels with the matrix and a caller may not reuse a matrix whose identity
/// it did not come from.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HostIdentity {
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub backend: String,
    pub backend_version: String,
}

impl HostIdentity {
    /// The identity of the host as this process can observe it.
    ///
    /// The OS version is *run*, not remembered: the probe asks the platform for
    /// its own version string, because a version pinned from memory is exactly
    /// the failure M12 is written to avoid.
    #[must_use]
    pub fn observed(os_version: impl Into<String>) -> Self {
        Self {
            os: std::env::consts::OS.to_owned(),
            os_version: os_version.into(),
            arch: std::env::consts::ARCH.to_owned(),
            backend: CONTAINMENT_BACKEND.to_owned(),
            backend_version: CONTAINMENT_BACKEND_VERSION.to_owned(),
        }
    }
}

/// The measured capability set of one host.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityMatrix {
    pub schema_version: u16,
    pub host: HostIdentity,
    pub findings: Vec<CapabilityFinding>,
    pub measured_at_unix_ms: u64,
}

impl CapabilityMatrix {
    #[must_use]
    pub fn new(host: HostIdentity, findings: Vec<CapabilityFinding>) -> Self {
        Self {
            schema_version: CAPABILITY_MATRIX_SCHEMA_VERSION,
            host,
            findings,
            measured_at_unix_ms: now_unix_ms(),
        }
    }

    #[must_use]
    pub fn verdict(&self, capability: Capability) -> Option<CapabilityVerdict> {
        self.findings
            .iter()
            .find(|finding| finding.capability == capability)
            .map(|finding| finding.verdict)
    }

    #[must_use]
    pub fn evidence(&self, capability: Capability) -> Option<&CapabilityEvidence> {
        self.findings
            .iter()
            .find(|finding| finding.capability == capability)
            .map(|finding| &finding.evidence)
    }

    #[must_use]
    pub fn is_enforced(&self, capability: Capability) -> bool {
        self.verdict(capability) == Some(CapabilityVerdict::Enforced)
    }

    #[must_use]
    pub fn enforced(&self) -> Vec<Capability> {
        self.with_verdict(CapabilityVerdict::Enforced)
    }

    #[must_use]
    pub fn unsupported(&self) -> Vec<Capability> {
        self.with_verdict(CapabilityVerdict::Unsupported)
    }

    fn with_verdict(&self, verdict: CapabilityVerdict) -> Vec<Capability> {
        self.findings
            .iter()
            .filter(|finding| finding.verdict == verdict)
            .map(|finding| finding.capability)
            .collect()
    }

    /// Every capability the profile requires that this matrix does not enforce.
    #[must_use]
    pub fn missing_for(&self, profile: StrictProfile) -> Vec<Capability> {
        profile
            .required()
            .into_iter()
            .filter(|capability| !self.is_enforced(*capability))
            .collect()
    }

    /// A matrix is only usable if it is complete: one finding per capability,
    /// each with a probe and a non-empty observation.
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.schema_version != CAPABILITY_MATRIX_SCHEMA_VERSION {
            return Err(HarnessError::new(
                ErrorCode::UnsupportedSchemaVersion,
                format!(
                    "capability matrix schema {} is not {}",
                    self.schema_version, CAPABILITY_MATRIX_SCHEMA_VERSION
                ),
            ));
        }
        for capability in Capability::ALL {
            let mut matches = self
                .findings
                .iter()
                .filter(|finding| finding.capability == capability);
            let Some(finding) = matches.next() else {
                return Err(HarnessError::new(
                    ErrorCode::GateConfigurationError,
                    format!("capability {} has no measured verdict", capability.as_str()),
                ));
            };
            if matches.next().is_some() {
                return Err(HarnessError::new(
                    ErrorCode::GateConfigurationError,
                    format!("capability {} was measured twice", capability.as_str()),
                ));
            }
            if finding.evidence.probe_id.trim().is_empty()
                || finding.evidence.method.trim().is_empty()
                || finding.evidence.observation.trim().is_empty()
            {
                return Err(HarnessError::new(
                    ErrorCode::GateConfigurationError,
                    format!(
                        "capability {} carries a verdict without evidence",
                        capability.as_str()
                    ),
                ));
            }
        }
        Ok(())
    }

    /// A stable text rendering of the enforced and unsupported sets, for a
    /// refusal message a human reads.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "enforced=[{}] unsupported=[{}]",
            join_names(&self.enforced()),
            join_names(&self.unsupported())
        )
    }
}

/// What a caller asks a strict backend to guarantee.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StrictProfile {
    /// Containment: the process tree, its environment and its bounds.
    ///
    /// This is what a host runner can honestly provide without an OS
    /// confinement mechanism. It deliberately does **not** include anything
    /// that confines what the child can reach.
    Containment,
    /// Full: containment **and** confinement of filesystem, network and
    /// credential sockets.
    ///
    /// This is the profile `isolation: strict` means, and its name is not
    /// negotiable: a host that cannot confine must refuse, not substitute.
    Full,
}

impl StrictProfile {
    pub const CONTAINMENT_REQUIRED: [Capability; 5] = [
        Capability::ProcessContainment,
        Capability::ProcessTreeKill,
        Capability::EnvironmentAllowlist,
        Capability::DeadlineEnforced,
        Capability::OutputBounds,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Containment => "containment",
            Self::Full => "full",
        }
    }

    /// Everything the profile promises, which is what a matrix must enforce.
    #[must_use]
    pub fn required(self) -> Vec<Capability> {
        match self {
            Self::Containment => Self::CONTAINMENT_REQUIRED.to_vec(),
            Self::Full => Capability::ALL.to_vec(),
        }
    }

    /// The typed refusal when this host cannot serve the profile.
    ///
    /// It names the missing capabilities, and it is the *only* outcome for a
    /// profile that cannot be enforced: there is no path from here to the host
    /// runner.
    #[must_use]
    pub fn refusal(self, matrix: &CapabilityMatrix) -> Option<HarnessError> {
        let missing = matrix.missing_for(self);
        if missing.is_empty() {
            return None;
        }
        Some(HarnessError::new(
            ErrorCode::StrictIsolationUnavailable,
            format!(
                "strict profile {} cannot be enforced on this host: missing [{}]; backend {} {}; ran {}",
                self.as_str(),
                join_names(&missing),
                matrix.host.backend,
                matrix.host.backend_version,
                matrix.summary()
            ),
        ))
    }
}

fn join_names(capabilities: &[Capability]) -> String {
    capabilities
        .iter()
        .map(|capability| capability.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

#[must_use]
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(capability: Capability, verdict: CapabilityVerdict) -> CapabilityFinding {
        let evidence = CapabilityEvidence::new("P-TEST", "unit test", "observed");
        CapabilityFinding {
            capability,
            verdict,
            evidence,
        }
    }

    fn matrix(verdict: CapabilityVerdict) -> CapabilityMatrix {
        CapabilityMatrix::new(
            HostIdentity::observed("test"),
            Capability::ALL
                .into_iter()
                .map(|capability| finding(capability, verdict))
                .collect(),
        )
    }

    /// The constant that names the containment backend may not drift from the
    /// lockfile: a reader of an evidence file has to be able to trust it.
    #[test]
    fn the_pinned_backend_version_matches_the_lockfile() {
        let lock = include_str!("../../../../Cargo.lock");
        let mut lines = lock.lines();
        let mut found = None;
        while let Some(line) = lines.next() {
            if line == format!("name = \"{CONTAINMENT_BACKEND}\"") {
                let version = lines
                    .next()
                    .and_then(|line| line.strip_prefix("version = \""))
                    .and_then(|line| line.strip_suffix('"'))
                    .expect("a locked package states its version");
                found = Some(version.to_owned());
                break;
            }
        }
        assert_eq!(
            found.as_deref(),
            Some(CONTAINMENT_BACKEND_VERSION),
            "the backend version constant and Cargo.lock disagree"
        );
    }

    #[test]
    fn a_full_profile_is_refused_when_confinement_is_unsupported() {
        let matrix = CapabilityMatrix::new(
            HostIdentity::observed("test"),
            Capability::ALL
                .into_iter()
                .map(|capability| {
                    finding(
                        capability,
                        if capability.is_confinement() {
                            CapabilityVerdict::Unsupported
                        } else {
                            CapabilityVerdict::Enforced
                        },
                    )
                })
                .collect(),
        );
        assert!(matrix.validate().is_ok());
        assert!(StrictProfile::Containment.refusal(&matrix).is_none());
        let refusal = StrictProfile::Full
            .refusal(&matrix)
            .expect("full confinement cannot be served");
        assert_eq!(refusal.code(), ErrorCode::StrictIsolationUnavailable);
        for capability in [
            Capability::FilesystemReadConfinement,
            Capability::NetworkEgressDenial,
            Capability::CredentialSocketDenial,
        ] {
            assert!(
                refusal.to_string().contains(capability.as_str()),
                "the refusal must name {}",
                capability.as_str()
            );
        }
    }

    #[test]
    fn a_verdict_without_evidence_is_rejected() {
        let mut incomplete = matrix(CapabilityVerdict::Enforced);
        incomplete.findings.pop();
        assert_eq!(
            incomplete.validate().expect_err("missing finding").code(),
            ErrorCode::GateConfigurationError
        );
        let mut blank = matrix(CapabilityVerdict::Unsupported);
        blank.findings[3].evidence.observation = "  ".to_owned();
        assert_eq!(
            blank.validate().expect_err("blank observation").code(),
            ErrorCode::GateConfigurationError
        );
    }
}
