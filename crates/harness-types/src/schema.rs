use schemars::schema_for;
use serde_json::Value;

use crate::{
    AgentProfileId, AgentRunId, ArtifactId, ContextPacket, ContextPacketId, EventEnvelope, EventId,
    HarnessConfig, InstructionId, InstructionLedgerEntry, MemoryAsset, MemoryAssetId,
    MemoryVersion, P0_SCHEMA_VERSION, PlanItemId, PluginInstanceId, PluginManifest, ProjectId,
    ScopeId, SessionId, TaskId, ToolExecutionId, ToolExecutionReceipt, WorkingState,
};

/// A committed JSON Schema document generated from a contract type.
#[derive(Clone, Debug, PartialEq)]
pub struct SchemaDocument {
    pub file_name: &'static str,
    pub value: Value,
}

/// Generate every P0 JSON Schema. The checked-in files are compared to these
/// values by tests, so the documents cannot silently drift from real types.
#[must_use]
pub fn generated_schema_documents() -> Vec<SchemaDocument> {
    vec![
        schema_document("event-envelope.v1.schema.json", schema_for!(EventEnvelope)),
        schema_document("working-state.v1.schema.json", schema_for!(WorkingState)),
        schema_document(
            "instruction-ledger.v1.schema.json",
            schema_for!(InstructionLedgerEntry),
        ),
        schema_document(
            "tool-execution-receipt.v1.schema.json",
            schema_for!(ToolExecutionReceipt),
        ),
        schema_document("memory-asset.v1.schema.json", schema_for!(MemoryAsset)),
        schema_document("memory-version.v1.schema.json", schema_for!(MemoryVersion)),
        schema_document(
            "plugin-manifest.v1.schema.json",
            schema_for!(PluginManifest),
        ),
        schema_document("context-packet.v1.schema.json", schema_for!(ContextPacket)),
        schema_document("harness-config.v1.schema.json", schema_for!(HarnessConfig)),
    ]
}

fn schema_document(file_name: &'static str, schema: schemars::Schema) -> SchemaDocument {
    let mut value = serde_json::to_value(schema).expect("schemars values serialize");
    let root = value
        .as_object_mut()
        .expect("schemars root schema is always an object");
    root.insert(
        "$id".to_owned(),
        Value::String(format!("https://harness-agents.local/schemas/{file_name}")),
    );
    root.insert(
        "x-harness-schema-version".to_owned(),
        Value::from(P0_SCHEMA_VERSION),
    );
    apply_contract_constraints(&mut value);
    SchemaDocument { file_name, value }
}

fn apply_contract_constraints(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(Value::Object(properties)) = object.get_mut("properties") {
                for (name, property) in properties.iter_mut() {
                    if let Value::Object(property_object) = property {
                        if let Some(pattern) = pattern_for_property(name) {
                            property_object
                                .insert("pattern".to_owned(), Value::String(pattern.to_owned()));
                        }
                        apply_numeric_constraints(name, property_object);
                    }
                    apply_contract_constraints(property);
                }
            }
            if let Some(Value::Object(definitions)) = object.get_mut("$defs") {
                if let Some(Value::Object(hash)) = definitions.get_mut("ContentHash") {
                    hash.insert(
                        "pattern".to_owned(),
                        Value::String("^sha256:[0-9a-f]{64}$".to_owned()),
                    );
                }
                for definition in definitions.values_mut() {
                    apply_contract_constraints(definition);
                }
            }
            for (key, nested) in object.iter_mut() {
                if key != "properties" && key != "$defs" {
                    apply_contract_constraints(nested);
                }
            }
        }
        Value::Array(values) => {
            for nested in values {
                apply_contract_constraints(nested);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn apply_numeric_constraints(name: &str, property: &mut serde_json::Map<String, Value>) {
    if name == "schema_version" {
        property.insert("const".to_owned(), Value::from(P0_SCHEMA_VERSION));
    }
    if matches!(
        name,
        "seq"
            | "sequence"
            | "through_event_seq"
            | "observed_at_seq"
            | "current_version"
            | "host_api_version"
            | "config_schema_version"
            | "api_version"
            | "version"
            | "rendering_version"
    ) {
        property.insert("minimum".to_owned(), Value::from(1_u8));
    }
}

fn pattern_for_property(name: &str) -> Option<&'static str> {
    match name {
        "agent_profile_id" => Some(AgentProfileId::SCHEMA_PATTERN),
        "agent_run_id" => Some(AgentRunId::SCHEMA_PATTERN),
        "artifact_id" => Some(ArtifactId::SCHEMA_PATTERN),
        "causation_id" | "correlation_id" | "event_id" => Some(EventId::SCHEMA_PATTERN),
        "instruction_id" => Some(InstructionId::SCHEMA_PATTERN),
        "instance_id" => Some(PluginInstanceId::SCHEMA_PATTERN),
        "memory_asset_id" => Some(MemoryAssetId::SCHEMA_PATTERN),
        "packet_id" => Some(ContextPacketId::SCHEMA_PATTERN),
        "project_id" => Some(ProjectId::SCHEMA_PATTERN),
        "scope_id" => Some(ScopeId::SCHEMA_PATTERN),
        "session_id" => Some(SessionId::SCHEMA_PATTERN),
        "task_id" => Some(TaskId::SCHEMA_PATTERN),
        "tool_execution_id" | "execution_id" => Some(ToolExecutionId::SCHEMA_PATTERN),
        "id" => Some(PlanItemId::SCHEMA_PATTERN),
        _ => None,
    }
}
