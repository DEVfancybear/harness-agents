use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::{ErrorCode, HarnessError};

/// Where a newly created contract ID gets its `UUIDv7` from.
///
/// Production code uses [`SystemIdSource`]. Tests and crash fixtures inject a
/// source with a fixed sequence so an ID asserted in one process is the same ID
/// observed in the next one.
pub trait IdSource: Send + Sync {
    fn uuid_v7(&self) -> Uuid;
}

/// The production `UUIDv7` source.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemIdSource;

impl IdSource for SystemIdSource {
    fn uuid_v7(&self) -> Uuid {
        Uuid::now_v7()
    }
}

/// A deterministic [`IdSource`] for fixtures.
///
/// It hands out the queued UUIDs in order and then returns the nil UUID, which
/// every typed ID constructor rejects. Exhausting a fixture source therefore
/// fails loudly instead of quietly reverting to real time.
#[derive(Debug, Default)]
pub struct FixedIdSource {
    remaining: std::sync::Mutex<std::collections::VecDeque<Uuid>>,
}

impl FixedIdSource {
    #[must_use]
    pub fn new(ids: impl IntoIterator<Item = Uuid>) -> Self {
        Self {
            remaining: std::sync::Mutex::new(ids.into_iter().collect()),
        }
    }

    /// Build a fixture source from hyphenated UUID strings, so a caller that
    /// does not depend on the `uuid` crate can still inject fixed IDs.
    pub fn parse_uuids(
        ids: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<Self, HarnessError> {
        let parsed = ids
            .into_iter()
            .map(|value| {
                Uuid::parse_str(value.as_ref()).map_err(|_| {
                    HarnessError::new(ErrorCode::InvalidId, "fixture UUID is not parseable")
                })
            })
            .collect::<Result<std::collections::VecDeque<_>, _>>()?;
        Ok(Self {
            remaining: std::sync::Mutex::new(parsed),
        })
    }
}

impl IdSource for FixedIdSource {
    fn uuid_v7(&self) -> Uuid {
        self.remaining
            .lock()
            .ok()
            .and_then(|mut remaining| remaining.pop_front())
            .unwrap_or(Uuid::nil())
    }
}

macro_rules! contract_id {
    ($name:ident, $prefix:literal, $pattern:literal) => {
        #[derive(Clone, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub const PREFIX: &'static str = $prefix;
            pub const SCHEMA_PATTERN: &'static str = $pattern;

            /// Generate a canonical `UUIDv7`-backed ID for a newly created record.
            #[must_use]
            pub fn generate() -> Self {
                Self(format!("{}_{}", Self::PREFIX, Uuid::now_v7().hyphenated()))
            }

            /// Generate an ID from an injected source, so fixtures can be
            /// deterministic and crash tests can predict the ID they reopen.
            pub fn generate_with(source: &dyn IdSource) -> Result<Self, HarnessError> {
                Self::from_uuid(source.uuid_v7())
            }

            /// Build an ID from an already chosen `UUIDv7`.
            pub fn from_uuid(uuid: Uuid) -> Result<Self, HarnessError> {
                if uuid.is_nil()
                    || uuid.get_variant() != uuid::Variant::RFC4122
                    || uuid.get_version_num() != 7
                {
                    return Err(HarnessError::new(
                        ErrorCode::InvalidId,
                        concat!(stringify!($name), " must use a UUIDv7"),
                    ));
                }
                Ok(Self(format!("{}_{}", Self::PREFIX, uuid.hyphenated())))
            }

            pub fn parse(value: impl Into<String>) -> Result<Self, HarnessError> {
                let value = value.into();
                let expected_prefix = concat!($prefix, "_");
                let suffix = value.strip_prefix(expected_prefix).ok_or_else(|| {
                    HarnessError::new(
                        ErrorCode::InvalidId,
                        concat!(stringify!($name), " has an invalid prefix"),
                    )
                })?;
                let uuid = Uuid::parse_str(suffix).map_err(|_| {
                    HarnessError::new(
                        ErrorCode::InvalidId,
                        concat!(stringify!($name), " does not contain a UUID"),
                    )
                })?;
                if uuid.is_nil()
                    || uuid.get_variant() != uuid::Variant::RFC4122
                    || uuid.get_version_num() != 7
                    || uuid.hyphenated().to_string() != suffix
                {
                    return Err(HarnessError::new(
                        ErrorCode::InvalidId,
                        concat!(stringify!($name), " must use canonical lowercase UUIDv7"),
                    ));
                }
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

contract_id!(
    AgentProfileId,
    "agent_profile",
    "^agent_profile_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    AgentRunId,
    "agent_run",
    "^agent_run_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    ArtifactId,
    "artifact",
    "^artifact_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    ContextPacketId,
    "context_packet",
    "^context_packet_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    CompositionSnapshotId,
    "composition_snapshot",
    "^composition_snapshot_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    EventId,
    "event",
    "^event_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    InstructionId,
    "instruction",
    "^instruction_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    HostId,
    "host",
    "^host_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    InputId,
    "input",
    "^input_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    MemoryAssetId,
    "memory_asset",
    "^memory_asset_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    PlanItemId,
    "plan_item",
    "^plan_item_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    PluginInstanceId,
    "plugin_instance",
    "^plugin_instance_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    ProjectId,
    "project",
    "^project_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    ProviderAttemptId,
    "provider_attempt",
    "^provider_attempt_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    RequestId,
    "request",
    "^request_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    ScopeId,
    "scope",
    "^scope_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    SessionId,
    "session",
    "^session_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    SnapshotId,
    "snapshot",
    "^snapshot_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    TaskId,
    "task",
    "^task_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    ToolExecutionId,
    "tool_execution",
    "^tool_execution_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    ToolApprovalId,
    "tool_approval",
    "^tool_approval_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    ToolInvocationId,
    "tool_invocation",
    "^tool_invocation_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    RuntimeCommandId,
    "runtime_command",
    "^runtime_command_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    ExtensionInstanceId,
    "extension_instance",
    "^extension_instance_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    SkillId,
    "skill",
    "^skill_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    StepId,
    "step",
    "^step_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
