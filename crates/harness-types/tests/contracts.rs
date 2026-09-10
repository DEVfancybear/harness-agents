use std::{fmt::Debug, path::PathBuf};

use harness_types::{
    AgentRunId, ArtifactId, CheckEvidence, CheckOutcome, ChildRunStatus, ChildState,
    CliOutputFormat, CliPresentationConfig, ContentHash, ContextPacket, ContextPacketId, ErrorCode,
    EventEnvelope, EventId, HarnessConfig, InstructionId, InstructionLedgerEntry,
    InstructionStatus, MemoryAsset, MemoryAssetId, MemoryAssetStatus, MemoryScope, MemoryVersion,
    MemoryVersionRef, NextActionProposal, P0_SCHEMA_VERSION, PendingToolCall, PendingToolState,
    PlanItem, PlanItemId, PlanItemStatus, PluginInstanceId, PluginManifest, ProducerIdentity,
    ProjectId, ScopeId, ServiceContract, SessionId, SourceAuthority, SourceRef, TaskId,
    ToolExecutionId, ToolExecutionReceipt, ToolIntentState, ToolOutcomeState, Validity,
    WorkingState, WorkspaceChange, WorkspaceObservation, canonical_json_bytes,
    generated_schema_documents, validate_schema_version,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

const UUID: &str = "018f8b64-5c8d-7a0a-8f21-123456789abc";

fn project_id() -> ProjectId {
    ProjectId::parse(format!("project_{UUID}")).unwrap()
}

fn task_id() -> TaskId {
    TaskId::parse(format!("task_{UUID}")).unwrap()
}

fn session_id() -> SessionId {
    SessionId::parse(format!("session_{UUID}")).unwrap()
}

fn event_id() -> EventId {
    EventId::parse(format!("event_{UUID}")).unwrap()
}

fn instruction_id() -> InstructionId {
    InstructionId::parse(format!("instruction_{UUID}")).unwrap()
}

fn plan_item_id() -> PlanItemId {
    PlanItemId::parse(format!("plan_item_{UUID}")).unwrap()
}

fn agent_run_id() -> AgentRunId {
    AgentRunId::parse(format!("agent_run_{UUID}")).unwrap()
}

fn artifact_id() -> ArtifactId {
    ArtifactId::parse(format!("artifact_{UUID}")).unwrap()
}

fn tool_execution_id() -> ToolExecutionId {
    ToolExecutionId::parse(format!("tool_execution_{UUID}")).unwrap()
}

fn memory_asset_id() -> MemoryAssetId {
    MemoryAssetId::parse(format!("memory_asset_{UUID}")).unwrap()
}

fn plugin_instance_id() -> PluginInstanceId {
    PluginInstanceId::parse(format!("plugin_instance_{UUID}")).unwrap()
}

fn scope_id() -> ScopeId {
    ScopeId::parse(format!("scope_{UUID}")).unwrap()
}

fn context_packet_id() -> ContextPacketId {
    ContextPacketId::parse(format!("context_packet_{UUID}")).unwrap()
}

fn hash(value: &str) -> ContentHash {
    ContentHash::from_bytes(value.as_bytes())
}

fn source_ref(sequence: u64) -> SourceRef {
    SourceRef {
        event_id: event_id(),
        sequence,
        content_hash: hash("source-ref"),
    }
}

fn event_envelope() -> EventEnvelope {
    let payload = json!({
        "instruction": "fix parser",
        "priority": 1,
    })
    .as_object()
    .unwrap()
    .clone();
    EventEnvelope {
        schema_version: P0_SCHEMA_VERSION,
        event_id: event_id(),
        session_id: session_id(),
        seq: 1,
        event_type: "input.admitted".to_owned(),
        producer: ProducerIdentity {
            plugin_id: "host.foundation".to_owned(),
            implementation_version: "0.1.0".to_owned(),
        },
        authority: SourceAuthority::User,
        correlation_id: None,
        causation_id: None,
        continuity_critical: true,
        payload_hash: ContentHash::from_canonical_json(&Value::Object(payload.clone())).unwrap(),
        payload,
    }
}

fn working_state() -> WorkingState {
    WorkingState {
        schema_version: P0_SCHEMA_VERSION,
        session_id: session_id(),
        task_id: task_id(),
        revision: 1,
        through_event_seq: 4,
        objective_ref: source_ref(1),
        acceptance_criteria_refs: vec![source_ref(2)],
        active_instruction_refs: vec![instruction_id()],
        decision_refs: vec![source_ref(3)],
        superseded_decision_refs: vec![source_ref(4)],
        plan_items: vec![PlanItem {
            id: plan_item_id(),
            status: PlanItemStatus::Pending,
            owner: Some(agent_run_id()),
            dependencies: vec![],
            evidence_refs: vec![source_ref(2)],
        }],
        workspace: WorkspaceObservation {
            project_id: project_id(),
            worktree_id: "main".to_owned(),
            base_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            observed_fingerprint: hash("workspace"),
        },
        changes: vec![WorkspaceChange {
            path: "src/parser.rs".to_owned(),
            before_hash: Some(hash("before")),
            after_hash: Some(hash("after")),
            tool_execution_id: Some(tool_execution_id()),
        }],
        checks: vec![CheckEvidence {
            command: "cargo test".to_owned(),
            outcome: CheckOutcome::Failed,
            tested_revision: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            artifact_id: Some(artifact_id()),
        }],
        pending_tool_calls: vec![PendingToolCall {
            execution_id: tool_execution_id(),
            state: PendingToolState::OutcomeUnknown,
            reconciliation_hint: "compare workspace fingerprint".to_owned(),
        }],
        children: vec![ChildState {
            agent_run_id: agent_run_id(),
            task_id: task_id(),
            status: ChildRunStatus::Completed,
            result_ref: Some(source_ref(4)),
        }],
        blockers: vec!["test B is failing".to_owned()],
        pending_questions: vec!["confirm parser edge case".to_owned()],
        next_action_proposals: vec![NextActionProposal {
            authority: SourceAuthority::ModelProposed,
            description: "investigate test B".to_owned(),
        }],
    }
}

fn assert_round_trip<T>(value: &T)
where
    T: Debug + DeserializeOwned + PartialEq + Serialize,
{
    let rendered = serde_json::to_string(value).unwrap();
    let restored: T = serde_json::from_str(&rendered).unwrap();
    assert_eq!(&restored, value);
}

#[test]
fn versioned_contracts_round_trip() {
    assert_event_and_working_state_round_trip();
    assert_instruction_and_receipt_round_trip();
    assert_memory_round_trip();
    assert_plugin_packet_and_config_round_trip();
}

fn assert_event_and_working_state_round_trip() {
    let event = event_envelope();
    event.validate().unwrap();
    assert_round_trip(&event);

    let state = working_state();
    state.validate().unwrap();
    assert_round_trip(&state);
}

fn assert_instruction_and_receipt_round_trip() {
    let instruction = InstructionLedgerEntry {
        schema_version: P0_SCHEMA_VERSION,
        instruction_id: instruction_id(),
        source: source_ref(1),
        scope: "task".to_owned(),
        authority: SourceAuthority::User,
        mandatory: true,
        status: InstructionStatus::Effective,
        source_hash: hash("instruction"),
        superseded_by: None,
    };
    instruction.validate().unwrap();
    assert_round_trip(&instruction);

    let receipt = ToolExecutionReceipt {
        schema_version: P0_SCHEMA_VERSION,
        tool_execution_id: tool_execution_id(),
        task_id: task_id(),
        invocation_id: "invoke-1".to_owned(),
        input_hash: hash("input"),
        policy_revision: 1,
        approval_id: None,
        intent_state: ToolIntentState::IntentRecorded,
        outcome_state: ToolOutcomeState::OutcomeUnknown,
        before_fingerprint: Some(hash("before")),
        after_fingerprint: None,
        artifact_id: None,
        observed_at_seq: 3,
    };
    receipt.validate().unwrap();
    assert_round_trip(&receipt);
}

fn assert_memory_round_trip() {
    let asset = MemoryAsset {
        schema_version: P0_SCHEMA_VERSION,
        memory_asset_id: memory_asset_id(),
        kind: "project_fact".to_owned(),
        owner_id: "user-local".to_owned(),
        project_id: Some(project_id()),
        scope: MemoryScope::Project,
        visibility: "project_members".to_owned(),
        status: MemoryAssetStatus::Active,
        current_version: 1,
        created_by: SourceAuthority::RuntimeObserved,
    };
    asset.validate().unwrap();
    assert_round_trip(&asset);

    let memory_version = MemoryVersion {
        schema_version: P0_SCHEMA_VERSION,
        memory_asset_id: memory_asset_id(),
        version: 1,
        content_or_artifact_hash: hash("memory-content"),
        content_hash: hash("memory-content"),
        source_event_refs: vec![event_id()],
        source_file_hashes: vec![hash("parser.rs")],
        source_commit: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
        provenance_kind: "runtime_observed".to_owned(),
        evidence_state: "verified".to_owned(),
        confidence_annotation: None,
        validity: Validity::Valid,
        supersedes: None,
        extractor_version: None,
    };
    memory_version.validate().unwrap();
    assert_round_trip(&memory_version);
}

fn assert_plugin_packet_and_config_round_trip() {
    let manifest = PluginManifest {
        schema_version: P0_SCHEMA_VERSION,
        plugin_id: "memory.local".to_owned(),
        implementation_version: "0.1.0".to_owned(),
        instance_id: plugin_instance_id(),
        scope_id: scope_id(),
        host_api_version: 1,
        config_schema_version: 1,
        provides: vec![ServiceContract {
            service_id: "memory_store".to_owned(),
            api_version: 1,
        }],
        requires: vec![],
        capabilities: vec!["read_memory".to_owned()],
        blocks_recovery_when_absent: false,
    };
    manifest.validate().unwrap();
    assert_round_trip(&manifest);

    let packet = ContextPacket {
        schema_version: P0_SCHEMA_VERSION,
        packet_id: context_packet_id(),
        session_id: session_id(),
        task_id: task_id(),
        checkpoint_id: "checkpoint-1".to_owned(),
        through_event_seq: 4,
        memory_versions: vec![MemoryVersionRef {
            memory_asset_id: memory_asset_id(),
            version: 1,
        }],
        rendering_version: 1,
        token_estimate: 12,
        content: "current objective".to_owned(),
        content_hash: hash("current objective"),
        source_manifest: vec![source_ref(1)],
    };
    packet.validate().unwrap();
    assert_round_trip(&packet);

    let config = HarnessConfig {
        schema_version: P0_SCHEMA_VERSION,
        cli: CliPresentationConfig {
            output: CliOutputFormat::Json,
        },
    };
    config.validate().unwrap();
    assert_round_trip(&config);
}

#[test]
fn committed_schemas_match_real_types() {
    let schema_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas");
    let documents = generated_schema_documents();
    let event_schema = documents
        .iter()
        .find(|document| document.file_name == "event-envelope.v1.schema.json")
        .expect("event schema is generated");
    assert_eq!(
        event_schema.value["properties"]["schema_version"]["const"],
        1
    );
    assert_eq!(event_schema.value["properties"]["seq"]["minimum"], 1);
    assert_eq!(
        event_schema.value["properties"]["payload"]["type"],
        "object"
    );
    assert_eq!(
        event_schema.value["properties"]["event_id"]["pattern"],
        "^event_[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
    );
    for document in documents {
        let mut expected = serde_json::to_string_pretty(&document.value).unwrap();
        expected.push('\n');
        let committed = std::fs::read_to_string(schema_root.join(document.file_name)).unwrap();
        assert_eq!(committed, expected, "schema drift: {}", document.file_name);
    }
}

#[test]
fn invalid_ids_versions_sequences_authorities_payloads_and_hashes_are_rejected() {
    let uppercase = format!("project_{}", UUID.to_uppercase());
    assert_eq!(
        ProjectId::parse(uppercase).unwrap_err().code(),
        ErrorCode::InvalidId
    );
    assert_eq!(
        ProjectId::parse(format!("task_{UUID}")).unwrap_err().code(),
        ErrorCode::InvalidId
    );
    assert_eq!(
        ProjectId::parse("project_00000000-0000-0000-0000-000000000000")
            .unwrap_err()
            .code(),
        ErrorCode::InvalidId
    );
    assert_eq!(
        validate_schema_version(0).unwrap_err().code(),
        ErrorCode::UnsupportedSchemaVersion
    );
    assert_eq!(
        validate_schema_version(2).unwrap_err().code(),
        ErrorCode::UnsupportedSchemaVersion
    );

    let mut event = event_envelope();
    event.seq = 0;
    assert_eq!(
        event.validate().unwrap_err().code(),
        ErrorCode::InvalidSequence
    );
    let mut event = event_envelope();
    event.schema_version = 2;
    assert_eq!(
        event.validate().unwrap_err().code(),
        ErrorCode::UnsupportedSchemaVersion
    );
    let mut event = event_envelope();
    event.payload_hash = hash("not the payload");
    assert_eq!(event.validate().unwrap_err().code(), ErrorCode::InvalidHash);
    assert_eq!(
        EventEnvelope::parse_json(r#"{"schema_version":1}"#)
            .unwrap_err()
            .code(),
        ErrorCode::MissingAuthority
    );
    assert_eq!(
        EventEnvelope::parse_json(
            r#"{
            "schema_version":1,
            "event_id":"event_018f8b64-5c8d-7a0a-8f21-123456789abc",
            "session_id":"session_018f8b64-5c8d-7a0a-8f21-123456789abc",
            "seq":1,
            "event_type":"input.admitted",
            "producer":{"plugin_id":"host","implementation_version":"1"},
            "authority":"user",
            "correlation_id":null,
            "causation_id":null,
            "continuity_critical":true,
            "payload":[],
            "payload_hash":"sha256:0000000000000000000000000000000000000000000000000000000000000000"
        }"#
        )
        .unwrap_err()
        .code(),
        ErrorCode::InvalidPayload
    );
    assert_eq!(
        ContentHash::parse("sha256:ABCDEF").unwrap_err().code(),
        ErrorCode::InvalidHash
    );
}

#[test]
fn canonical_hash_is_deterministic_and_rejects_floating_point() {
    let first: Value = serde_json::from_str(r#"{"z":2,"a":{"y":1,"x":0}}"#).unwrap();
    let reordered: Value = serde_json::from_str(r#"{"a":{"x":0,"y":1},"z":2}"#).unwrap();
    let changed: Value = serde_json::from_str(r#"{"a":{"x":0,"y":1},"z":3}"#).unwrap();

    assert_eq!(
        canonical_json_bytes(&first).unwrap(),
        canonical_json_bytes(&reordered).unwrap()
    );
    assert_eq!(
        ContentHash::from_canonical_json(&first).unwrap(),
        ContentHash::from_canonical_json(&reordered).unwrap()
    );
    assert_ne!(
        ContentHash::from_canonical_json(&first).unwrap(),
        ContentHash::from_canonical_json(&changed).unwrap()
    );
    let decimal: Value = serde_json::from_str(r#"{"value":1.5}"#).unwrap();
    assert_eq!(
        canonical_json_bytes(&decimal).unwrap_err().code(),
        ErrorCode::InvalidPayload
    );
}

#[test]
fn typed_errors_render_a_stable_code_prefix() {
    let error = validate_schema_version(42).unwrap_err();
    assert_eq!(
        error.to_string(),
        "unsupported_schema_version: schema version 42 is not supported by P0"
    );
}
