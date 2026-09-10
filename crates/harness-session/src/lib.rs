#![forbid(unsafe_code)]

//! P1 session commands, deterministic projection, and recovery views.

use std::sync::Arc;

use harness_store_sqlite::{
    AdmissionAck, AdmissionCommit, PersistedPluginManifest, PublishedArtifact, ReceiptAck,
    ReceiptCommit, SnapshotRecord, SourceWorkMarker, SqliteStore, StoreError,
};
use harness_types::{
    CheckEvidence, ContentHash, EventEnvelope, EventId, InputId, InstructionId,
    InstructionLedgerEntry, InstructionStatus, NextActionProposal, P0_SCHEMA_VERSION, PlanItem,
    ProducerIdentity, SessionId, SnapshotId, SourceAuthority, SourceRef, TaskId,
    ToolExecutionReceipt, WorkingState, WorkspaceObservation,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

mod context;

pub use context::{
    ContextBlock, ContextBlockKind, ContextBuildRequest, ContextBuildResult, ContextBuilder,
    ContextError,
};

/// Input accepted by the durable admission command.
#[derive(Clone, Debug)]
pub struct AdmitInputRequest {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub input_id: InputId,
    pub expected_sequence: u64,
    pub authority: SourceAuthority,
    pub raw_text: String,
    pub workspace: WorkspaceObservation,
    pub initial_plan_items: Vec<PlanItem>,
}

/// Synthetic runtime-observed evidence used only by P1 fixtures and recovery
/// tests. It is deliberately not a tool executor.
#[derive(Clone, Debug)]
pub struct RecordSyntheticReceiptRequest {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub expected_sequence: u64,
    pub receipt: ToolExecutionReceipt,
    pub artifact: Option<PublishedArtifact>,
}

/// A snapshot's complete deterministic recovery payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverySnapshot {
    pub schema_version: u16,
    pub working_state: WorkingState,
    pub receipts: Vec<ToolExecutionReceipt>,
    pub instruction_texts: Vec<String>,
}

/// State rebuilt exclusively from a snapshot plus committed journal tail.
#[derive(Clone, Debug, PartialEq)]
pub struct RecoveryView {
    pub working_state: WorkingState,
    pub receipts: Vec<ToolExecutionReceipt>,
    pub instruction_texts: Vec<String>,
    pub snapshot_sequence: Option<u64>,
    pub replayed_through_sequence: u64,
    pub snapshot_diagnostic: Option<String>,
    pub pending_execution_count: u64,
}

/// P1 transaction commands and recovery operations.
#[derive(Clone)]
pub struct SessionService {
    store: Arc<SqliteStore>,
}

impl SessionService {
    #[must_use]
    pub fn new(store: Arc<SqliteStore>) -> Self {
        Self { store }
    }

    #[must_use]
    pub fn store(&self) -> &Arc<SqliteStore> {
        &self.store
    }

    /// Admit raw user text atomically. The returned value is the only durable
    /// ACK; no model classification or execution occurs here.
    pub async fn admit_input(
        &self,
        request: AdmitInputRequest,
    ) -> Result<AdmissionAck, StoreError> {
        if request.expected_sequence == 0 {
            return Err(StoreError::new(
                harness_types::ErrorCode::SequenceConflict,
                "input sequence must start at 1",
            ));
        }
        let event_id = EventId::generate();
        let instruction_id = InstructionId::generate();
        let payload = input_payload(&request, &instruction_id)?;
        let payload_hash = ContentHash::from_canonical_json(&Value::Object(payload.clone()))
            .map_err(|error| {
                StoreError::new(
                    error.code(),
                    format!("input payload is not canonical: {error}"),
                )
            })?;
        let event = EventEnvelope {
            schema_version: P0_SCHEMA_VERSION,
            event_id: event_id.clone(),
            session_id: request.session_id.clone(),
            seq: request.expected_sequence,
            event_type: "input.admitted".to_owned(),
            producer: ProducerIdentity {
                plugin_id: "p1.session".to_owned(),
                implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            authority: request.authority,
            correlation_id: None,
            causation_id: None,
            continuity_critical: true,
            payload,
            payload_hash: payload_hash.clone(),
        };
        let source = SourceRef {
            event_id: event_id.clone(),
            sequence: request.expected_sequence,
            content_hash: payload_hash,
        };
        let instruction = InstructionLedgerEntry {
            schema_version: P0_SCHEMA_VERSION,
            instruction_id: instruction_id.clone(),
            source: source.clone(),
            scope: "task".to_owned(),
            authority: request.authority,
            mandatory: true,
            status: InstructionStatus::Effective,
            source_hash: ContentHash::from_bytes(request.raw_text.as_bytes()),
            superseded_by: None,
        };
        let state =
            initial_working_state(&request, source, instruction_id, request.expected_sequence);
        let marker = SourceWorkMarker {
            marker_id: format!("input:{}", request.input_id.as_str()),
            event_id,
            sequence: request.expected_sequence,
            kind: "input_admission".to_owned(),
            status: "committed".to_owned(),
        };
        self.store
            .commit_admission(AdmissionCommit {
                session_id: request.session_id,
                task_id: request.task_id,
                input_id: request.input_id,
                input_hash: ContentHash::from_bytes(request.raw_text.as_bytes()),
                raw_text: request.raw_text,
                expected_sequence: request.expected_sequence,
                event,
                instruction,
                working_state: state,
                marker,
            })
            .await
    }

    /// Record a synthetic, immutable receipt without performing a side effect.
    pub async fn record_synthetic_receipt(
        &self,
        request: RecordSyntheticReceiptRequest,
    ) -> Result<ReceiptAck, StoreError> {
        let mut state = self
            .store
            .current_projection(&request.task_id)
            .await?
            .ok_or_else(|| {
                StoreError::new(
                    harness_types::ErrorCode::InvalidPayload,
                    "receipt requires an admitted task projection",
                )
            })?;
        if state.session_id != request.session_id {
            return Err(StoreError::new(
                harness_types::ErrorCode::IdempotencyConflict,
                "receipt session does not own this task projection",
            ));
        }
        state.revision = request.expected_sequence;
        state.through_event_seq = request.expected_sequence;

        let receipt_value = serde_json::to_value(&request.receipt).map_err(|_| {
            StoreError::new(
                harness_types::ErrorCode::InvalidPayload,
                "synthetic receipt cannot be serialized",
            )
        })?;
        let mut payload = Map::new();
        payload.insert("receipt".to_owned(), receipt_value);
        payload.insert("kind".to_owned(), Value::String("synthetic".to_owned()));
        let payload_hash = ContentHash::from_canonical_json(&Value::Object(payload.clone()))
            .map_err(|error| {
                StoreError::new(
                    error.code(),
                    format!("receipt payload is not canonical: {error}"),
                )
            })?;
        let event_id = EventId::generate();
        let event = EventEnvelope {
            schema_version: P0_SCHEMA_VERSION,
            event_id: event_id.clone(),
            session_id: request.session_id.clone(),
            seq: request.expected_sequence,
            event_type: "receipt.recorded".to_owned(),
            producer: ProducerIdentity {
                plugin_id: "p1.session".to_owned(),
                implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            authority: SourceAuthority::RuntimeObserved,
            correlation_id: None,
            causation_id: None,
            continuity_critical: true,
            payload,
            payload_hash,
        };
        let marker = SourceWorkMarker {
            marker_id: format!("receipt:{}", request.receipt.tool_execution_id.as_str()),
            event_id,
            sequence: request.expected_sequence,
            kind: "synthetic_receipt".to_owned(),
            status: "committed".to_owned(),
        };
        self.store
            .commit_receipt(ReceiptCommit {
                session_id: request.session_id,
                task_id: request.task_id,
                expected_sequence: request.expected_sequence,
                event,
                receipt: request.receipt,
                working_state: state,
                marker,
                artifact: request.artifact,
            })
            .await
    }

    /// Persist the complete recovery view at the latest committed sequence.
    pub async fn write_snapshot(
        &self,
        session_id: &SessionId,
    ) -> Result<SnapshotRecord, StoreError> {
        let recovered = self.recover(session_id).await?;
        let task_id = recovered.working_state.task_id.clone();
        let content = serde_json::to_value(RecoverySnapshot {
            schema_version: P0_SCHEMA_VERSION,
            working_state: recovered.working_state.clone(),
            receipts: recovered.receipts,
            instruction_texts: recovered.instruction_texts,
        })
        .map_err(|_| {
            StoreError::new(
                harness_types::ErrorCode::InvalidPayload,
                "snapshot cannot serialize",
            )
        })?;
        let snapshot = SnapshotRecord {
            snapshot_id: SnapshotId::generate(),
            session_id: session_id.clone(),
            task_id,
            through_sequence: recovered.replayed_through_sequence,
            schema_version: P0_SCHEMA_VERSION,
            content_hash: ContentHash::from_canonical_json(&content).map_err(|error| {
                StoreError::new(
                    error.code(),
                    format!("snapshot content is not canonical: {error}"),
                )
            })?,
            content,
        };
        self.store.write_snapshot(snapshot.clone()).await?;
        Ok(snapshot)
    }

    /// Restore a session from a valid snapshot plus every committed tail event.
    #[allow(clippy::too_many_lines)]
    pub async fn recover(&self, session_id: &SessionId) -> Result<RecoveryView, StoreError> {
        let summary = self
            .store
            .session_summary(session_id)
            .await?
            .ok_or_else(|| {
                StoreError::new(
                    harness_types::ErrorCode::InvalidPayload,
                    "session does not exist",
                )
            })?;
        let task_id = summary.task_id.clone();
        let mut snapshot_sequence = None;
        let mut snapshot_diagnostic = None;
        let mut state = None;
        let mut receipts = Vec::new();
        let mut instruction_texts = Vec::new();

        match self.store.latest_snapshot(session_id).await {
            Ok(Some(snapshot)) => match restore_snapshot(&snapshot, session_id, &task_id) {
                Ok(restored) => {
                    snapshot_sequence = Some(snapshot.through_sequence);
                    state = Some(restored.working_state);
                    receipts = restored.receipts;
                    instruction_texts = restored.instruction_texts;
                }
                Err(error) => {
                    snapshot_diagnostic = Some(error.to_string());
                }
            },
            Ok(None) => {}
            Err(error) if error.code() == harness_types::ErrorCode::SnapshotCorrupt => {
                snapshot_diagnostic = Some(error.to_string());
            }
            Err(error) => return Err(error),
        }

        let through = snapshot_sequence.unwrap_or(0);
        let expected_last = summary.next_sequence.checked_sub(1).ok_or_else(|| {
            StoreError::new(
                harness_types::ErrorCode::StorageWriteFailed,
                "session next sequence is invalid",
            )
        })?;
        if through > expected_last {
            return Err(StoreError::new(
                harness_types::ErrorCode::SnapshotCorrupt,
                "snapshot claims coverage beyond the committed journal",
            ));
        }
        let mut expected_sequence = through;
        let events = self.store.load_events_after(session_id, through).await?;
        for event in events {
            let next_sequence = expected_sequence.checked_add(1).ok_or_else(|| {
                StoreError::new(
                    harness_types::ErrorCode::SequenceConflict,
                    "journal sequence overflow during recovery",
                )
            })?;
            if event.session_id != *session_id {
                return Err(StoreError::new(
                    harness_types::ErrorCode::InvalidPayload,
                    "journal event belongs to another session",
                ));
            }
            if event.seq != next_sequence {
                return Err(StoreError::new(
                    harness_types::ErrorCode::SequenceConflict,
                    format!(
                        "journal sequence gap: expected {next_sequence}, found {}",
                        event.seq
                    ),
                ));
            }
            fold_event(
                &mut state,
                &mut receipts,
                &mut instruction_texts,
                &task_id,
                event,
            )?;
            expected_sequence = next_sequence;
        }
        if expected_sequence != expected_last {
            return Err(StoreError::new(
                harness_types::ErrorCode::SequenceConflict,
                format!("journal coverage ended at {expected_sequence}, expected {expected_last}"),
            ));
        }
        let working_state = state.ok_or_else(|| {
            StoreError::new(
                harness_types::ErrorCode::InvalidPayload,
                "journal contains no recoverable admitted input",
            )
        })?;
        let replayed_through_sequence = working_state.through_event_seq;
        Ok(RecoveryView {
            working_state,
            receipts,
            instruction_texts,
            snapshot_sequence,
            replayed_through_sequence,
            snapshot_diagnostic,
            pending_execution_count: 0,
        })
    }

    /// Record a continuity-critical decision and optionally supersede an
    /// earlier decision. The event and projection are committed atomically.
    pub async fn record_decision(
        &self,
        session_id: &SessionId,
        task_id: &TaskId,
        text: &str,
        supersedes: Option<SourceRef>,
    ) -> Result<SourceRef, StoreError> {
        if text.trim().is_empty() {
            return Err(StoreError::new(
                harness_types::ErrorCode::InvalidPayload,
                "decision text must not be empty",
            ));
        }
        let mut state = self
            .store
            .current_projection(task_id)
            .await?
            .ok_or_else(|| {
                StoreError::new(
                    harness_types::ErrorCode::InvalidPayload,
                    "decision requires an admitted task",
                )
            })?;
        if state.session_id != *session_id {
            return Err(StoreError::new(
                harness_types::ErrorCode::IdempotencyConflict,
                "decision session does not own task",
            ));
        }
        let sequence = self.store.next_sequence(session_id).await?;
        let event_id = EventId::generate();
        let mut payload = Map::new();
        payload.insert(
            "task_id".to_owned(),
            Value::String(task_id.as_str().to_owned()),
        );
        payload.insert("text".to_owned(), Value::String(text.to_owned()));
        if let Some(ref source) = supersedes {
            payload.insert(
                "supersedes".to_owned(),
                serde_json::to_value(source).map_err(|_| {
                    StoreError::new(
                        harness_types::ErrorCode::InvalidPayload,
                        "decision supersession is invalid",
                    )
                })?,
            );
        }
        let payload_hash = ContentHash::from_canonical_json(&Value::Object(payload.clone()))
            .map_err(|error| {
                StoreError::new(
                    error.code(),
                    format!("decision payload is not canonical: {error}"),
                )
            })?;
        let source = SourceRef {
            event_id: event_id.clone(),
            sequence,
            content_hash: payload_hash.clone(),
        };
        if let Some(old) = supersedes.clone() {
            state.decision_refs.retain(|current| current != &old);
            state.superseded_decision_refs.push(old);
        }
        state.decision_refs.push(source.clone());
        state.revision = sequence;
        state.through_event_seq = sequence;
        let event = EventEnvelope {
            schema_version: P0_SCHEMA_VERSION,
            event_id: event_id.clone(),
            session_id: session_id.clone(),
            seq: sequence,
            event_type: "decision.updated".to_owned(),
            producer: ProducerIdentity {
                plugin_id: "p2.session".to_owned(),
                implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            authority: SourceAuthority::User,
            correlation_id: None,
            causation_id: None,
            continuity_critical: true,
            payload,
            payload_hash,
        };
        self.store
            .commit_projected_event(
                event,
                task_id,
                state,
                SourceWorkMarker {
                    marker_id: format!("decision:{sequence}"),
                    event_id,
                    sequence,
                    kind: "decision_update".to_owned(),
                    status: "committed".to_owned(),
                },
            )
            .await?;
        Ok(source)
    }

    /// Append a runtime event while preserving the current projection.
    pub async fn append_runtime_event(
        &self,
        session_id: &SessionId,
        task_id: &TaskId,
        event_type: &str,
        payload: Map<String, Value>,
        continuity_critical: bool,
    ) -> Result<SourceRef, StoreError> {
        let mut state = self
            .store
            .current_projection(task_id)
            .await?
            .ok_or_else(|| {
                StoreError::new(
                    harness_types::ErrorCode::InvalidPayload,
                    "runtime event requires an admitted task",
                )
            })?;
        if state.session_id != *session_id {
            return Err(StoreError::new(
                harness_types::ErrorCode::IdempotencyConflict,
                "runtime event session does not own task",
            ));
        }
        let sequence = self.store.next_sequence(session_id).await?;
        let event_id = EventId::generate();
        let payload_hash = ContentHash::from_canonical_json(&Value::Object(payload.clone()))
            .map_err(|error| {
                StoreError::new(
                    error.code(),
                    format!("runtime payload is not canonical: {error}"),
                )
            })?;
        state.revision = sequence;
        state.through_event_seq = sequence;
        let source = SourceRef {
            event_id: event_id.clone(),
            sequence,
            content_hash: payload_hash.clone(),
        };
        let event = EventEnvelope {
            schema_version: P0_SCHEMA_VERSION,
            event_id: event_id.clone(),
            session_id: session_id.clone(),
            seq: sequence,
            event_type: event_type.to_owned(),
            producer: ProducerIdentity {
                plugin_id: "p2.session".to_owned(),
                implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            authority: SourceAuthority::RuntimeObserved,
            correlation_id: None,
            causation_id: None,
            continuity_critical,
            payload,
            payload_hash,
        };
        self.store
            .commit_projected_event(
                event,
                task_id,
                state,
                SourceWorkMarker {
                    marker_id: format!("runtime:{event_id}"),
                    event_id,
                    sequence,
                    kind: "runtime_event".to_owned(),
                    status: "committed".to_owned(),
                },
            )
            .await?;
        Ok(source)
    }

    pub async fn list_plugins(&self) -> Result<Vec<PersistedPluginManifest>, StoreError> {
        self.store.list_plugin_manifests().await
    }
}

fn input_payload(
    request: &AdmitInputRequest,
    instruction_id: &InstructionId,
) -> Result<Map<String, Value>, StoreError> {
    let workspace = serde_json::to_value(&request.workspace).map_err(|_| {
        StoreError::new(
            harness_types::ErrorCode::InvalidPayload,
            "workspace observation cannot be serialized",
        )
    })?;
    let mut payload = Map::new();
    payload.insert(
        "input_id".to_owned(),
        Value::String(request.input_id.as_str().to_owned()),
    );
    payload.insert("text".to_owned(), Value::String(request.raw_text.clone()));
    payload.insert(
        "instruction_id".to_owned(),
        Value::String(instruction_id.as_str().to_owned()),
    );
    payload.insert(
        "classification".to_owned(),
        Value::String("unclassified".to_owned()),
    );
    payload.insert("scope".to_owned(), Value::String("task".to_owned()));
    payload.insert("mandatory".to_owned(), Value::Bool(true));
    payload.insert(
        "task_id".to_owned(),
        Value::String(request.task_id.as_str().to_owned()),
    );
    payload.insert("workspace".to_owned(), workspace);
    payload.insert(
        "initial_plan_items".to_owned(),
        serde_json::to_value(&request.initial_plan_items).map_err(|_| {
            StoreError::new(
                harness_types::ErrorCode::InvalidPayload,
                "initial plan items cannot be serialized",
            )
        })?,
    );
    Ok(payload)
}

fn initial_working_state(
    request: &AdmitInputRequest,
    source: SourceRef,
    instruction_id: InstructionId,
    sequence: u64,
) -> WorkingState {
    WorkingState {
        schema_version: P0_SCHEMA_VERSION,
        session_id: request.session_id.clone(),
        task_id: request.task_id.clone(),
        revision: sequence,
        through_event_seq: sequence,
        objective_ref: source,
        acceptance_criteria_refs: Vec::new(),
        active_instruction_refs: vec![instruction_id],
        decision_refs: Vec::new(),
        superseded_decision_refs: Vec::new(),
        plan_items: request.initial_plan_items.clone(),
        workspace: request.workspace.clone(),
        changes: Vec::new(),
        checks: Vec::<CheckEvidence>::new(),
        pending_tool_calls: Vec::new(),
        children: Vec::new(),
        blockers: Vec::new(),
        pending_questions: Vec::new(),
        next_action_proposals: Vec::<NextActionProposal>::new(),
    }
}

fn restore_snapshot(
    snapshot: &SnapshotRecord,
    session_id: &SessionId,
    task_id: &TaskId,
) -> Result<RecoverySnapshot, StoreError> {
    if snapshot.schema_version != P0_SCHEMA_VERSION {
        return Err(StoreError::new(
            harness_types::ErrorCode::SnapshotCorrupt,
            "snapshot schema version is unsupported",
        ));
    }
    let actual_hash = ContentHash::from_canonical_json(&snapshot.content).map_err(|error| {
        StoreError::new(error.code(), format!("snapshot is not canonical: {error}"))
    })?;
    if actual_hash != snapshot.content_hash {
        return Err(StoreError::new(
            harness_types::ErrorCode::SnapshotCorrupt,
            "snapshot content hash does not match",
        ));
    }
    let restored: RecoverySnapshot =
        serde_json::from_value(snapshot.content.clone()).map_err(|_| {
            StoreError::new(
                harness_types::ErrorCode::SnapshotCorrupt,
                "snapshot recovery payload is invalid",
            )
        })?;
    if restored.schema_version != P0_SCHEMA_VERSION
        || restored.working_state.session_id != *session_id
        || restored.working_state.task_id != *task_id
        || restored.working_state.through_event_seq != snapshot.through_sequence
    {
        return Err(StoreError::new(
            harness_types::ErrorCode::SnapshotCorrupt,
            "snapshot identity or coverage does not match",
        ));
    }
    restored.working_state.validate().map_err(|error| {
        StoreError::new(
            error.code(),
            format!("snapshot working state is invalid: {error}"),
        )
    })?;
    for receipt in &restored.receipts {
        receipt.validate().map_err(|error| {
            StoreError::new(
                error.code(),
                format!("snapshot receipt is invalid: {error}"),
            )
        })?;
    }
    Ok(restored)
}

#[allow(clippy::too_many_lines)]
fn fold_event(
    state: &mut Option<WorkingState>,
    receipts: &mut Vec<ToolExecutionReceipt>,
    instruction_texts: &mut Vec<String>,
    expected_task_id: &TaskId,
    event: EventEnvelope,
) -> Result<(), StoreError> {
    match event.event_type.as_str() {
        "input.admitted" => {
            if state.is_some() {
                return Err(StoreError::new(
                    harness_types::ErrorCode::IdempotencyConflict,
                    "journal contains more than one admitted input for this minimal P1 session",
                ));
            }
            let task_id = payload_string(&event.payload, "task_id")?;
            if task_id != expected_task_id.as_str() {
                return Err(StoreError::new(
                    harness_types::ErrorCode::InvalidPayload,
                    "admitted input task does not match session task",
                ));
            }
            let input_id =
                InputId::parse(payload_string(&event.payload, "input_id")?).map_err(|_| {
                    StoreError::new(
                        harness_types::ErrorCode::InvalidPayload,
                        "journal input ID is invalid",
                    )
                })?;
            let instruction_id =
                InstructionId::parse(payload_string(&event.payload, "instruction_id")?).map_err(
                    |_| {
                        StoreError::new(
                            harness_types::ErrorCode::InvalidPayload,
                            "journal instruction ID is invalid",
                        )
                    },
                )?;
            let text = payload_string(&event.payload, "text")?;
            let workspace: WorkspaceObservation = serde_json::from_value(
                event.payload.get("workspace").cloned().ok_or_else(|| {
                    StoreError::new(
                        harness_types::ErrorCode::InvalidPayload,
                        "journal input has no workspace",
                    )
                })?,
            )
            .map_err(|_| {
                StoreError::new(
                    harness_types::ErrorCode::InvalidPayload,
                    "journal workspace is invalid",
                )
            })?;
            let source = SourceRef {
                event_id: event.event_id.clone(),
                sequence: event.seq,
                content_hash: event.payload_hash,
            };
            let initial_plan_items: Vec<PlanItem> = serde_json::from_value(
                event
                    .payload
                    .get("initial_plan_items")
                    .cloned()
                    .ok_or_else(|| {
                        StoreError::new(
                            harness_types::ErrorCode::InvalidPayload,
                            "journal input has no initial plan items",
                        )
                    })?,
            )
            .map_err(|_| {
                StoreError::new(
                    harness_types::ErrorCode::InvalidPayload,
                    "journal initial plan items are invalid",
                )
            })?;
            let request = AdmitInputRequest {
                session_id: event.session_id.clone(),
                task_id: expected_task_id.clone(),
                input_id,
                expected_sequence: event.seq,
                authority: event.authority,
                raw_text: text.clone(),
                workspace,
                initial_plan_items,
            };
            *state = Some(initial_working_state(
                &request,
                source,
                instruction_id,
                event.seq,
            ));
            instruction_texts.push(text);
        }
        "receipt.recorded" => {
            let current = state.as_mut().ok_or_else(|| {
                StoreError::new(
                    harness_types::ErrorCode::InvalidPayload,
                    "receipt precedes admitted input",
                )
            })?;
            let receipt: ToolExecutionReceipt =
                serde_json::from_value(event.payload.get("receipt").cloned().ok_or_else(|| {
                    StoreError::new(
                        harness_types::ErrorCode::InvalidPayload,
                        "receipt event has no receipt payload",
                    )
                })?)
                .map_err(|_| {
                    StoreError::new(
                        harness_types::ErrorCode::InvalidPayload,
                        "receipt payload is invalid",
                    )
                })?;
            receipt.validate().map_err(|error| {
                StoreError::new(error.code(), format!("receipt payload is invalid: {error}"))
            })?;
            if receipt.task_id != current.task_id || receipt.observed_at_seq != event.seq {
                return Err(StoreError::new(
                    harness_types::ErrorCode::InvalidPayload,
                    "receipt does not match its journal event",
                ));
            }
            if receipts
                .iter()
                .any(|existing| existing.tool_execution_id == receipt.tool_execution_id)
            {
                return Err(StoreError::new(
                    harness_types::ErrorCode::IdempotencyConflict,
                    "journal contains a duplicate receipt ID",
                ));
            }
            current.revision = event.seq;
            current.through_event_seq = event.seq;
            receipts.push(receipt);
        }
        "decision.updated" => {
            let current = state.as_mut().ok_or_else(|| {
                StoreError::new(
                    harness_types::ErrorCode::InvalidPayload,
                    "decision precedes admitted input",
                )
            })?;
            let text = payload_string(&event.payload, "text")?;
            let source = SourceRef {
                event_id: event.event_id.clone(),
                sequence: event.seq,
                content_hash: event.payload_hash.clone(),
            };
            if let Some(raw) = event.payload.get("supersedes") {
                let old: SourceRef = serde_json::from_value(raw.clone()).map_err(|_| {
                    StoreError::new(
                        harness_types::ErrorCode::InvalidPayload,
                        "decision supersession is invalid",
                    )
                })?;
                current.decision_refs.retain(|existing| existing != &old);
                if !current.superseded_decision_refs.contains(&old) {
                    current.superseded_decision_refs.push(old);
                }
            }
            current.decision_refs.push(source);
            current.revision = event.seq;
            current.through_event_seq = event.seq;
            instruction_texts.push(text);
        }
        _ if event.continuity_critical => {
            return Err(StoreError::new(
                harness_types::ErrorCode::UnknownCriticalEvent,
                format!(
                    "unknown continuity-critical event at sequence {}",
                    event.seq
                ),
            ));
        }
        _ => {
            if let Some(current) = state.as_mut() {
                current.revision = event.seq;
                current.through_event_seq = event.seq;
            }
        }
    }
    Ok(())
}

fn payload_string(payload: &Map<String, Value>, key: &str) -> Result<String, StoreError> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            StoreError::new(
                harness_types::ErrorCode::InvalidPayload,
                format!("journal event field {key} is missing or invalid"),
            )
        })
}
