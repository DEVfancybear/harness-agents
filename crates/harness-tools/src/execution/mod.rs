//! M12 — the strict execution backend, and the boundary it does not cross.
//!
//! The module has one job: make the difference between *containment* (a process
//! tree, its environment, its bounds) and *confinement* (what a process can
//! reach) into measured data, so that a strict request is answered with either a
//! backend that genuinely enforces it or a typed refusal. There is deliberately
//! no third answer: nothing here can downgrade a strict request to the host
//! runner.
//!
//! * [`capability`] — the vocabulary: capabilities, verdicts, evidence, the
//!   host identity, and the refusal a profile produces.
//! * [`probe`] — the measurements themselves, each driving a real process and
//!   reading the effect from outside it.
//! * `probe_control` — the negative controls: the same fixtures with the one
//!   wrapper that provides a boundary left out, so a probe that cannot lose is
//!   itself detectable.

mod capability;
mod probe;
mod probe_control;

pub use capability::{
    CAPABILITY_MATRIX_SCHEMA_VERSION, CONTAINMENT_BACKEND, CONTAINMENT_BACKEND_VERSION, Capability,
    CapabilityEvidence, CapabilityFinding, CapabilityMatrix, CapabilityVerdict, HostIdentity,
    StrictProfile,
};
pub use probe::{BoundaryBreakObservation, CapabilityProbe, PROBE_CANARY_NAME, ProbeChild};
