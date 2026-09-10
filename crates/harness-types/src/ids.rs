use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use crate::{ErrorCode, HarnessError};

macro_rules! contract_id {
    ($name:ident, $prefix:literal, $pattern:literal) => {
        #[derive(Clone, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub const PREFIX: &'static str = $prefix;
            pub const SCHEMA_PATTERN: &'static str = $pattern;

            /// Generate a canonical UUIDv7-backed ID for a newly created record.
            #[must_use]
            pub fn generate() -> Self {
                Self(format!("{}_{}", Self::PREFIX, Uuid::now_v7().hyphenated()))
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
    TaskId,
    "task",
    "^task_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
contract_id!(
    ToolExecutionId,
    "tool_execution",
    "^tool_execution_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
);
