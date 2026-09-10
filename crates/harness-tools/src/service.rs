use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use harness_providers::CancellationToken;
use harness_store_sqlite::{
    PublishedArtifact, SourceWorkMarker, SqliteStore, StoreError, ToolApprovalBinding,
    ToolApprovalRecord, ToolApprovalState, ToolIntentCommit, ToolIntentRecord, ToolIntentStatus,
    ToolSettlementCommit, ToolTaskUpdateCommit,
};
use harness_types::{
    ContentHash, ErrorCode, EventEnvelope, EventId, HarnessError, NextActionProposal,
    P0_SCHEMA_VERSION, PendingToolCall, PendingToolState, ProducerIdentity, SourceAuthority,
    TaskId, ToolExecutionId, ToolExecutionReceipt, ToolIntentState, ToolOutcomeState, WorkingState,
};
use serde_json::{Map, Value, json};

use crate::{
    ApprovalGrant, CodingToolAction, IsolationMode, PreparedToolRequest, TOOL_CONTRACT_VERSION,
    ToolCapabilities, ToolExecutionView, ToolOutput, ToolPolicy, ToolRequest,
    process::{self, ProcessResult},
    workspace::{
        apply_text_patch, inspect_workspace, list_files, read_text, read_text_output, redact_text,
        resolve_relative, search_text,
    },
};

/// An optional presentation observer. It is deliberately invoked only after a
/// receipt commit; an error or panic can alter neither receipt nor outcome.
pub trait ToolObserver: Send + Sync {
    fn observe(&self, view: &ToolExecutionView) -> Result<(), String>;
}

/// The one P3 authority permitted to move a coding proposal across a side
/// effect. Direct helpers in sibling modules are crate-private.
#[derive(Clone)]
pub struct ToolExecutionService {
    store: Arc<SqliteStore>,
    policy: ToolPolicy,
    observer: Option<Arc<dyn ToolObserver>>,
}

impl ToolExecutionService {
    #[must_use]
    pub fn new(store: Arc<SqliteStore>) -> Self {
        Self {
            store,
            policy: ToolPolicy::default(),
            observer: None,
        }
    }

    #[must_use]
    pub fn with_policy(mut self, policy: ToolPolicy) -> Self {
        self.policy = policy;
        self
    }

    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn ToolObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    #[must_use]
    pub const fn contract_version() -> u16 {
        TOOL_CONTRACT_VERSION
    }

    #[must_use]
    pub fn capabilities() -> ToolCapabilities {
        ToolCapabilities {
            schema_version: TOOL_CONTRACT_VERSION,
            #[cfg(windows)]
            process_tree_cleanup: "windows_job_object".to_owned(),
            #[cfg(unix)]
            process_tree_cleanup: "posix_process_session".to_owned(),
            #[cfg(not(any(windows, unix)))]
            process_tree_cleanup: "unsupported".to_owned(),
            strict_isolation: false,
            shell_requires_explicit_action: true,
            filesystem_network_sandbox: false,
        }
    }

    /// Validate identity and original arguments, apply deterministic policy
    /// transforms, validate again, then snapshot the exact final action and
    /// workspace state an approval will bind to.
    pub async fn prepare(&self, request: ToolRequest) -> Result<PreparedToolRequest, HarnessError> {
        if request.actor_id.trim().is_empty() {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "tool actor must not be empty",
            ));
        }
        let state = self
            .current_state(&request.session_id, &request.task_id)
            .await?;
        let workspace =
            inspect_workspace(&request.workspace_root, state.workspace.project_id.clone())?;
        self.store
            .register_project(workspace.registration())
            .await
            .map_err(store_error)?;
        Self::validate_workspace_action(&workspace.root, &request.action)?;
        let final_action = self.policy.transform_and_validate(request.action.clone())?;
        Self::validate_workspace_action(&workspace.root, &final_action)?;
        let action_hash = final_action.canonical_hash()?;
        Ok(PreparedToolRequest {
            request,
            final_action: final_action.clone(),
            action_hash,
            workspace_root: workspace.root,
            workspace_root_text: workspace.root_text,
            workspace_fingerprint: workspace.fingerprint,
            workspace_identity_hash: workspace.identity_hash,
            project_id: workspace.project_id,
            policy_revision: self.policy.revision(),
            policy_denial: self.policy.denial_for(&final_action),
        })
    }

    /// Issue and persist a one-use approval for the exact prepared action.
    pub async fn approve(
        &self,
        prepared: &PreparedToolRequest,
    ) -> Result<ApprovalGrant, HarnessError> {
        self.approve_with_expiry(prepared, None).await
    }

    /// Same as [`Self::approve`], with an optional explicit expiry for UI/host
    /// integrations. The store independently enforces the expiry at consume.
    pub async fn approve_with_expiry(
        &self,
        prepared: &PreparedToolRequest,
        expires_at_unix_ms: Option<u64>,
    ) -> Result<ApprovalGrant, HarnessError> {
        if let Some(reason) = &prepared.policy_denial {
            return Err(HarnessError::new(ErrorCode::PolicyDenied, reason.clone()));
        }
        if prepared.policy_revision != self.policy.revision() {
            return Err(HarnessError::new(
                ErrorCode::ApprovalStale,
                "policy changed before approval could be issued",
            ));
        }
        if expires_at_unix_ms.is_some_and(|expiry| current_unix_ms().is_ok_and(|now| now > expiry))
        {
            return Err(HarnessError::new(
                ErrorCode::ApprovalStale,
                "cannot issue an already expired approval",
            ));
        }
        let approval_id = harness_types::ToolApprovalId::generate();
        let binding_json = json!({
            "schema_version": TOOL_CONTRACT_VERSION,
            "approval_id": approval_id,
            "actor_id": prepared.request.actor_id,
            "action_hash": prepared.action_hash,
            "workspace_root": prepared.workspace_root_text,
            "workspace_fingerprint": prepared.workspace_fingerprint,
            "policy_revision": prepared.policy_revision,
            "tool_revision": TOOL_CONTRACT_VERSION,
            "expires_at_unix_ms": expires_at_unix_ms,
        });
        let binding_hash = ContentHash::from_canonical_json(&binding_json)?;
        let grant = ApprovalGrant {
            approval_id: approval_id.clone(),
            actor_id: prepared.request.actor_id.clone(),
            action_hash: prepared.action_hash.clone(),
            workspace_root: prepared.workspace_root_text.clone(),
            workspace_fingerprint: prepared.workspace_fingerprint.clone(),
            policy_revision: prepared.policy_revision,
            tool_revision: u64::from(TOOL_CONTRACT_VERSION),
            expires_at_unix_ms,
        };
        self.store
            .issue_tool_approval(ToolApprovalRecord {
                approval_id,
                actor_id: grant.actor_id.clone(),
                binding_hash,
                action_hash: grant.action_hash.clone(),
                workspace_root: grant.workspace_root.clone(),
                workspace_fingerprint: grant.workspace_fingerprint.clone(),
                policy_revision: grant.policy_revision,
                tool_revision: grant.tool_revision,
                expires_at_unix_ms,
                state: ToolApprovalState::Active,
                approval_json: binding_json,
            })
            .await
            .map_err(store_error)?;
        Ok(grant)
    }

    pub async fn revoke_approval(
        &self,
        grant: &ApprovalGrant,
        reason: &str,
    ) -> Result<(), HarnessError> {
        self.store
            .revoke_tool_approval(&grant.approval_id, reason)
            .await
            .map_err(store_error)
    }

    /// Execute an approved P3 action without provider cancellation plumbing.
    pub async fn execute(
        &self,
        prepared: PreparedToolRequest,
        approval: Option<ApprovalGrant>,
    ) -> Result<ToolExecutionView, HarnessError> {
        self.execute_with_cancellation(prepared, approval, CancellationToken::new())
            .await
    }

    /// The only public P3 side-effect entrypoint. The ordered gate is:
    /// validate → transform → revalidate → approval → monotonic guards →
    /// durable intent/approval consumption → dispatch → durable receipt →
    /// non-authoritative observer.
    #[allow(clippy::too_many_lines)]
    pub async fn execute_with_cancellation(
        &self,
        prepared: PreparedToolRequest,
        approval: Option<ApprovalGrant>,
        cancellation: CancellationToken,
    ) -> Result<ToolExecutionView, HarnessError> {
        let execution_id = ToolExecutionId::generate();
        let transformed = self
            .policy
            .transform_and_validate(prepared.request.action.clone())?;
        if prepared.policy_revision != self.policy.revision()
            || transformed != prepared.final_action
        {
            return self
                .record_denied(
                    &prepared,
                    execution_id,
                    approval.as_ref(),
                    ErrorCode::ApprovalStale,
                    "policy transform or revision changed after preparation",
                )
                .await;
        }
        if let Some(reason) = self.policy.denial_for(&transformed) {
            return self
                .record_denied(
                    &prepared,
                    execution_id,
                    approval.as_ref(),
                    ErrorCode::PolicyDenied,
                    &reason,
                )
                .await;
        }
        if action_requests_strict_isolation(&transformed) {
            return self
                .record_denied(
                    &prepared,
                    execution_id,
                    approval.as_ref(),
                    ErrorCode::StrictIsolationUnavailable,
                    "this host has lifecycle cleanup but no verified strict isolation sandbox",
                )
                .await;
        }
        if cancellation.is_cancelled() {
            return self
                .record_denied(
                    &prepared,
                    execution_id,
                    approval.as_ref(),
                    ErrorCode::ProcessCanceled,
                    "tool execution was canceled before the durable intent",
                )
                .await;
        }
        let Some(approval) = approval else {
            return self
                .record_denied(
                    &prepared,
                    execution_id,
                    None,
                    ErrorCode::ApprovalRequired,
                    "tool action requires an explicit approval",
                )
                .await;
        };
        if let Some((code, reason)) = approval_mismatch(&prepared, &approval) {
            return self
                .record_denied(&prepared, execution_id, Some(&approval), code, &reason)
                .await;
        }
        Self::validate_workspace_action(&prepared.workspace_root, &transformed)?;
        let reobserved = inspect_workspace(&prepared.workspace_root, prepared.project_id.clone())?;
        if reobserved.identity_hash != prepared.workspace_identity_hash
            || reobserved.fingerprint != prepared.workspace_fingerprint
        {
            return self
                .record_denied(
                    &prepared,
                    execution_id,
                    Some(&approval),
                    ErrorCode::StaleWorkspace,
                    "workspace root, Git identity, or fingerprint changed after approval",
                )
                .await;
        }
        self.store
            .register_project(reobserved.registration())
            .await
            .map_err(store_error)?;
        if let Err(error) =
            Self::validate_dispatch_preconditions(&prepared.workspace_root, &transformed)
        {
            let code = error.code();
            return self
                .record_denied(
                    &prepared,
                    execution_id,
                    Some(&approval),
                    code,
                    &error.to_string(),
                )
                .await;
        }
        if matches!(transformed, CodingToolAction::TaskUpdate { .. }) {
            return self
                .execute_task_update(prepared, approval, execution_id)
                .await;
        }
        let mut state = self
            .current_state(&prepared.request.session_id, &prepared.request.task_id)
            .await?;
        let intent_sequence = self
            .store
            .next_sequence(&prepared.request.session_id)
            .await
            .map_err(store_error)?;
        state.pending_tool_calls.push(PendingToolCall {
            execution_id: execution_id.clone(),
            state: PendingToolState::Pending,
            reconciliation_hint: "inspect the durable tool intent and workspace before any retry"
                .to_owned(),
        });
        state.revision = intent_sequence;
        state.through_event_seq = intent_sequence;
        let intent = ToolIntentRecord {
            tool_execution_id: execution_id.clone(),
            session_id: prepared.request.session_id.clone(),
            task_id: prepared.request.task_id.clone(),
            invocation_id: prepared.request.invocation_id.as_str().to_owned(),
            actor_id: prepared.request.actor_id.clone(),
            tool_name: transformed.kind().as_str().to_owned(),
            action_json: transformed.canonical_value()?,
            action_hash: prepared.action_hash.clone(),
            workspace_root: prepared.workspace_root_text.clone(),
            workspace_fingerprint: prepared.workspace_fingerprint.clone(),
            before_fingerprint: Some(prepared.workspace_fingerprint.clone()),
            policy_revision: prepared.policy_revision,
            tool_revision: u64::from(TOOL_CONTRACT_VERSION),
            approval: binding_from_grant(&approval),
            status: ToolIntentStatus::Recorded,
            intent_sequence,
        };
        let intent_event = event_for_intent(&intent)?;
        self.store
            .commit_tool_intent(ToolIntentCommit {
                expected_sequence: intent_sequence,
                event: intent_event.clone(),
                intent,
                working_state: state,
                marker: SourceWorkMarker {
                    marker_id: format!("tool-intent:{}", execution_id.as_str()),
                    event_id: intent_event.event_id,
                    sequence: intent_sequence,
                    kind: "tool_intent".to_owned(),
                    status: "committed".to_owned(),
                },
            })
            .await
            .map_err(store_error)?;

        match self
            .dispatch(&prepared.workspace_root, &transformed, cancellation)
            .await
        {
            Ok(output) => {
                self.settle(
                    &prepared,
                    execution_id,
                    Some(approval.approval_id.as_str()),
                    output,
                    ToolOutcomeState::Settled,
                    ToolIntentStatus::Settled,
                )
                .await
            }
            Err(error) => {
                let reason =
                    format!("side effect may be uncertain; do not rerun automatically: {error}");
                self.settle(
                    &prepared,
                    execution_id,
                    Some(approval.approval_id.as_str()),
                    ToolOutput::OutcomeUnknown { reason },
                    ToolOutcomeState::OutcomeUnknown,
                    ToolIntentStatus::OutcomeUnknown,
                )
                .await
            }
        }
    }

    /// Inspect a pending intent after restart. It deliberately never dispatches
    /// the action again. Only an exact observed patch post-state is settled as
    /// applied; all other cases remain an explicit unknown outcome.
    pub async fn reconcile_pending(
        &self,
        session_id: &harness_types::SessionId,
        execution_id: &ToolExecutionId,
    ) -> Result<ToolExecutionView, HarnessError> {
        let intent = self
            .store
            .tool_intent(execution_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| {
                HarnessError::new(ErrorCode::InvalidPayload, "tool intent was not found")
            })?;
        if intent.session_id != *session_id || intent.status != ToolIntentStatus::Recorded {
            return Err(HarnessError::new(
                ErrorCode::ToolIntentConflict,
                "tool intent is not pending for this session",
            ));
        }
        let action: CodingToolAction =
            serde_json::from_value(intent.action_json.clone()).map_err(|_| {
                HarnessError::new(ErrorCode::InvalidPayload, "stored tool action is invalid")
            })?;
        let state = self
            .current_state(&intent.session_id, &intent.task_id)
            .await?;
        let root = Path::new(&intent.workspace_root);
        let output = match &action {
            CodingToolAction::ApplyPatch {
                path,
                expected_hash,
                replacement,
                ..
            } => {
                let path = resolve_relative(root, path, false)?;
                match read_text(&path) {
                    Ok(current) if current == *replacement => ToolOutput::ApplyPatch {
                        path: path.to_string_lossy().replace('\\', "/"),
                        before_hash: expected_hash.clone(),
                        after_hash: ContentHash::from_bytes(current.as_bytes()),
                    },
                    _ => ToolOutput::OutcomeUnknown {
                        reason: "pending patch has no exact observed post-state; no rerun was attempted"
                            .to_owned(),
                    },
                }
            }
            _ => ToolOutput::OutcomeUnknown {
                reason: "pending non-idempotent tool requires user-directed reconciliation; no rerun was attempted"
                    .to_owned(),
            },
        };
        let outcome = if matches!(output, ToolOutput::ApplyPatch { .. }) {
            ToolOutcomeState::Settled
        } else {
            ToolOutcomeState::OutcomeUnknown
        };
        let final_status = if outcome == ToolOutcomeState::Settled {
            ToolIntentStatus::Settled
        } else {
            ToolIntentStatus::OutcomeUnknown
        };
        let prepared = PreparedToolRequest {
            request: ToolRequest {
                session_id: intent.session_id.clone(),
                task_id: intent.task_id.clone(),
                actor_id: intent.actor_id.clone(),
                invocation_id: harness_types::ToolInvocationId::parse(
                    intent.invocation_id.clone(),
                )?,
                workspace_root: root.to_owned(),
                action: action.clone(),
            },
            final_action: action,
            action_hash: intent.action_hash.clone(),
            workspace_root: root.to_owned(),
            workspace_root_text: intent.workspace_root.clone(),
            workspace_fingerprint: intent.workspace_fingerprint.clone(),
            workspace_identity_hash: ContentHash::from_bytes(intent.workspace_root.as_bytes()),
            project_id: state.workspace.project_id,
            policy_revision: intent.policy_revision,
            policy_denial: None,
        };
        self.settle(
            &prepared,
            execution_id.clone(),
            Some(intent.approval.approval_id.as_str()),
            output,
            outcome,
            final_status,
        )
        .await
    }

    pub async fn reassociate_workspace(
        &self,
        session_id: &harness_types::SessionId,
        task_id: &TaskId,
        old_canonical_root: &str,
        replacement_root: impl AsRef<Path>,
    ) -> Result<(), HarnessError> {
        let state = self.current_state(session_id, task_id).await?;
        let replacement = inspect_workspace(replacement_root.as_ref(), state.workspace.project_id)?;
        self.store
            .reassociate_project(
                &replacement.project_id,
                old_canonical_root,
                replacement.registration(),
            )
            .await
            .map_err(store_error)
    }

    fn validate_workspace_action(
        root: &Path,
        action: &CodingToolAction,
    ) -> Result<(), HarnessError> {
        match action {
            CodingToolAction::ReadFile { path } | CodingToolAction::ApplyPatch { path, .. } => {
                let _ = resolve_relative(root, path, false)?;
            }
            CodingToolAction::ListFiles { path }
            | CodingToolAction::SearchText { path, .. }
            | CodingToolAction::GitDiff { path } => {
                if let Some(path) = path {
                    let _ = resolve_relative(root, path, true)?;
                }
            }
            CodingToolAction::RunProcess { .. }
            | CodingToolAction::RunShell { .. }
            | CodingToolAction::GitStatus
            | CodingToolAction::TaskUpdate { .. } => {}
        }
        Ok(())
    }

    /// Validate operation-specific state before a durable intent claims a
    /// side effect may occur. The patch hash is checked here and again inside
    /// the atomic write helper to close the ordinary stale-edit case while
    /// still treating a race after intent conservatively.
    fn validate_dispatch_preconditions(
        root: &Path,
        action: &CodingToolAction,
    ) -> Result<(), HarnessError> {
        match action {
            CodingToolAction::ReadFile { path } => {
                // Deterministic content-policy failures (binary or unsupported
                // encoding) are denied before an intent is created. A later
                // disappearance/race remains an ordinary settled/unknown
                // dispatch result, never an unsafe success.
                let target = resolve_relative(root, path, false)?;
                let _ = read_text(&target)?;
            }
            CodingToolAction::ApplyPatch {
                path,
                expected_hash,
                ..
            } => {
                let target = resolve_relative(root, path, false)?;
                let current = read_text(&target)?;
                if ContentHash::from_bytes(current.as_bytes()) != *expected_hash {
                    return Err(HarnessError::new(
                        ErrorCode::StaleWorkspace,
                        "patch expected hash does not match current file content",
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }

    async fn execute_task_update(
        &self,
        prepared: PreparedToolRequest,
        approval: ApprovalGrant,
        _execution_id: ToolExecutionId,
    ) -> Result<ToolExecutionView, HarnessError> {
        let CodingToolAction::TaskUpdate { note } = prepared.final_action else {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "non-task action reached task update path",
            ));
        };
        let mut state = self
            .current_state(&prepared.request.session_id, &prepared.request.task_id)
            .await?;
        let sequence = self
            .store
            .next_sequence(&prepared.request.session_id)
            .await
            .map_err(store_error)?;
        state.next_action_proposals.push(NextActionProposal {
            authority: SourceAuthority::RuntimeObserved,
            description: note.clone(),
        });
        state.revision = sequence;
        state.through_event_seq = sequence;
        let event = event_for_task_update(&prepared.request.session_id, sequence, &note)?;
        self.store
            .commit_tool_task_update(ToolTaskUpdateCommit {
                session_id: prepared.request.session_id,
                task_id: prepared.request.task_id,
                expected_sequence: sequence,
                event: event.clone(),
                working_state: state,
                marker: SourceWorkMarker {
                    marker_id: format!("tool-task-update:{sequence}"),
                    event_id: event.event_id,
                    sequence,
                    kind: "tool_task_update".to_owned(),
                    status: "committed".to_owned(),
                },
                approval: binding_from_grant(&approval),
            })
            .await
            .map_err(store_error)?;
        let mut view = ToolExecutionView {
            schema_version: TOOL_CONTRACT_VERSION,
            execution_id: None,
            receipt: None,
            output: ToolOutput::TaskUpdate { note },
            observer_failure: None,
        };
        self.notify(&mut view);
        Ok(view)
    }

    async fn dispatch(
        &self,
        root: &Path,
        action: &CodingToolAction,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput, HarnessError> {
        match action {
            CodingToolAction::ReadFile { path } => {
                let target = resolve_relative(root, path, false)?;
                let output = read_text_output(&target)?;
                Ok(ToolOutput::ReadFile {
                    path: path.replace('\\', "/"),
                    content: output.text,
                    truncated: output.truncated,
                })
            }
            CodingToolAction::ListFiles { path } => {
                let (paths, truncated) = list_files(root, path.as_deref())?;
                Ok(ToolOutput::ListFiles { paths, truncated })
            }
            CodingToolAction::SearchText { query, path } => {
                let output = search_text(root, query, path.as_deref())?;
                Ok(ToolOutput::SearchText {
                    matches: output.matches,
                    truncated: output.truncated,
                })
            }
            CodingToolAction::ApplyPatch {
                path,
                expected_hash,
                replacement,
            } => {
                let target = resolve_relative(root, path, false)?;
                let (before_hash, after_hash) =
                    apply_text_patch(&target, expected_hash, replacement)?;
                Ok(ToolOutput::ApplyPatch {
                    path: path.replace('\\', "/"),
                    before_hash,
                    after_hash,
                })
            }
            CodingToolAction::RunProcess {
                executable,
                args,
                timeout_ms,
                ..
            } => {
                let output =
                    process::run_structured(root, executable, args, *timeout_ms, cancellation)
                        .await?;
                Ok(process_output(output))
            }
            CodingToolAction::RunShell {
                command,
                timeout_ms,
                ..
            } => {
                let output = process::run_shell(root, command, *timeout_ms, cancellation).await?;
                Ok(process_output(output))
            }
            CodingToolAction::GitStatus => {
                let output = process::run_structured(
                    root,
                    "git",
                    &[
                        "status".to_owned(),
                        "--porcelain=v1".to_owned(),
                        "--branch".to_owned(),
                        "--untracked-files=all".to_owned(),
                    ],
                    15_000,
                    cancellation,
                )
                .await?;
                Ok(git_output("status", output))
            }
            CodingToolAction::GitDiff { path } => {
                let mut args = vec!["diff".to_owned(), "--no-ext-diff".to_owned()];
                if let Some(path) = path {
                    args.push("--".to_owned());
                    args.push(path.clone());
                }
                let output =
                    process::run_structured(root, "git", &args, 15_000, cancellation).await?;
                Ok(git_output("diff", output))
            }
            CodingToolAction::TaskUpdate { .. } => Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "task update must use its atomic task projection path",
            )),
        }
    }

    async fn settle(
        &self,
        prepared: &PreparedToolRequest,
        execution_id: ToolExecutionId,
        approval_id: Option<&str>,
        output: ToolOutput,
        outcome_state: ToolOutcomeState,
        final_status: ToolIntentStatus,
    ) -> Result<ToolExecutionView, HarnessError> {
        let mut state = self
            .current_state(&prepared.request.session_id, &prepared.request.task_id)
            .await?;
        let sequence = self
            .store
            .next_sequence(&prepared.request.session_id)
            .await
            .map_err(store_error)?;
        state
            .pending_tool_calls
            .retain(|pending| pending.execution_id != execution_id);
        state.revision = sequence;
        state.through_event_seq = sequence;
        let after_fingerprint =
            inspect_workspace(&prepared.workspace_root, prepared.project_id.clone()).map_or_else(
                |_| prepared.workspace_fingerprint.clone(),
                |workspace| workspace.fingerprint,
            );
        let artifact = self.publish_output_artifact(&output)?;
        let receipt = ToolExecutionReceipt {
            schema_version: P0_SCHEMA_VERSION,
            tool_execution_id: execution_id.clone(),
            task_id: prepared.request.task_id.clone(),
            invocation_id: prepared.request.invocation_id.as_str().to_owned(),
            input_hash: prepared.action_hash.clone(),
            policy_revision: prepared.policy_revision,
            approval_id: approval_id.map(ToOwned::to_owned),
            intent_state: ToolIntentState::IntentRecorded,
            outcome_state,
            before_fingerprint: Some(prepared.workspace_fingerprint.clone()),
            after_fingerprint: Some(after_fingerprint),
            artifact_id: artifact
                .as_ref()
                .map(|artifact| artifact.artifact_id.clone()),
            observed_at_seq: sequence,
        };
        let event = event_for_receipt(&prepared.request.session_id, sequence, &receipt, "p3")?;
        self.store
            .commit_tool_settlement(ToolSettlementCommit {
                expected_sequence: sequence,
                event: event.clone(),
                receipt: receipt.clone(),
                working_state: state,
                marker: SourceWorkMarker {
                    marker_id: format!("tool-receipt:{}", execution_id.as_str()),
                    event_id: event.event_id,
                    sequence,
                    kind: "tool_receipt".to_owned(),
                    status: match outcome_state {
                        ToolOutcomeState::Settled => "settled".to_owned(),
                        ToolOutcomeState::OutcomeUnknown => "outcome_unknown".to_owned(),
                        ToolOutcomeState::NotStarted | ToolOutcomeState::Denied => {
                            "invalid".to_owned()
                        }
                    },
                },
                artifact,
                final_status,
            })
            .await
            .map_err(store_error)?;
        let mut view = ToolExecutionView {
            schema_version: TOOL_CONTRACT_VERSION,
            execution_id: Some(execution_id),
            receipt: Some(receipt),
            output,
            observer_failure: None,
        };
        self.notify(&mut view);
        Ok(view)
    }

    async fn record_denied(
        &self,
        prepared: &PreparedToolRequest,
        execution_id: ToolExecutionId,
        approval: Option<&ApprovalGrant>,
        code: ErrorCode,
        reason: &str,
    ) -> Result<ToolExecutionView, HarnessError> {
        let mut state = self
            .current_state(&prepared.request.session_id, &prepared.request.task_id)
            .await?;
        let sequence = self
            .store
            .next_sequence(&prepared.request.session_id)
            .await
            .map_err(store_error)?;
        state.revision = sequence;
        state.through_event_seq = sequence;
        let receipt = ToolExecutionReceipt {
            schema_version: P0_SCHEMA_VERSION,
            tool_execution_id: execution_id.clone(),
            task_id: prepared.request.task_id.clone(),
            invocation_id: prepared.request.invocation_id.as_str().to_owned(),
            input_hash: prepared.action_hash.clone(),
            policy_revision: prepared.policy_revision,
            approval_id: approval.map(|grant| grant.approval_id.as_str().to_owned()),
            intent_state: ToolIntentState::Denied,
            outcome_state: ToolOutcomeState::Denied,
            before_fingerprint: Some(prepared.workspace_fingerprint.clone()),
            after_fingerprint: None,
            artifact_id: None,
            observed_at_seq: sequence,
        };
        let event = event_for_receipt(
            &prepared.request.session_id,
            sequence,
            &receipt,
            "p3_denied",
        )?;
        self.store
            .commit_receipt(harness_store_sqlite::ReceiptCommit {
                session_id: prepared.request.session_id.clone(),
                task_id: prepared.request.task_id.clone(),
                expected_sequence: sequence,
                event: event.clone(),
                receipt: receipt.clone(),
                working_state: state,
                marker: SourceWorkMarker {
                    marker_id: format!("tool-denied:{}", execution_id.as_str()),
                    event_id: event.event_id,
                    sequence,
                    kind: "tool_denied".to_owned(),
                    status: code.as_str().to_owned(),
                },
                artifact: None,
            })
            .await
            .map_err(store_error)?;
        let mut view = ToolExecutionView {
            schema_version: TOOL_CONTRACT_VERSION,
            execution_id: Some(execution_id),
            receipt: Some(receipt),
            output: ToolOutput::Denied {
                code: code.as_str().to_owned(),
                reason: reason.to_owned(),
            },
            observer_failure: None,
        };
        self.notify(&mut view);
        Ok(view)
    }

    fn publish_output_artifact(
        &self,
        output: &ToolOutput,
    ) -> Result<Option<PublishedArtifact>, HarnessError> {
        let bytes = serde_json::to_vec(output).map_err(|_| {
            HarnessError::new(
                ErrorCode::ArtifactWriteFailed,
                "tool output cannot be serialized",
            )
        })?;
        self.store
            .publish_artifact(&bytes)
            .map(Some)
            .map_err(store_error)
    }

    fn notify(&self, view: &mut ToolExecutionView) {
        let Some(observer) = &self.observer else {
            return;
        };
        let result = catch_unwind(AssertUnwindSafe(|| observer.observe(view)));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => view.observer_failure = Some(error),
            Err(_) => {
                view.observer_failure =
                    Some("tool observer panicked after receipt commit".to_owned());
            }
        }
    }

    async fn current_state(
        &self,
        session_id: &harness_types::SessionId,
        task_id: &TaskId,
    ) -> Result<WorkingState, HarnessError> {
        let state = self
            .store
            .current_projection(task_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| {
                HarnessError::new(ErrorCode::InvalidPayload, "task has no admitted state")
            })?;
        if state.session_id != *session_id {
            return Err(HarnessError::new(
                ErrorCode::TaskLeaseConflict,
                "requested session does not own the task projection",
            ));
        }
        Ok(state)
    }
}

fn binding_from_grant(grant: &ApprovalGrant) -> ToolApprovalBinding {
    ToolApprovalBinding {
        approval_id: grant.approval_id.clone(),
        actor_id: grant.actor_id.clone(),
        action_hash: grant.action_hash.clone(),
        workspace_root: grant.workspace_root.clone(),
        workspace_fingerprint: grant.workspace_fingerprint.clone(),
        policy_revision: grant.policy_revision,
        tool_revision: grant.tool_revision,
    }
}

fn approval_mismatch(
    prepared: &PreparedToolRequest,
    approval: &ApprovalGrant,
) -> Option<(ErrorCode, String)> {
    if approval.actor_id != prepared.request.actor_id
        || approval.action_hash != prepared.action_hash
        || approval.workspace_root != prepared.workspace_root_text
        || approval.workspace_fingerprint != prepared.workspace_fingerprint
        || approval.policy_revision != prepared.policy_revision
        || approval.tool_revision != u64::from(TOOL_CONTRACT_VERSION)
    {
        return Some((
            ErrorCode::ApprovalStale,
            "approval does not bind the final actor/action/workspace/policy revision".to_owned(),
        ));
    }
    if approval
        .expires_at_unix_ms
        .is_some_and(|expiry| current_unix_ms().is_ok_and(|now| now > expiry))
    {
        return Some((ErrorCode::ApprovalStale, "approval has expired".to_owned()));
    }
    None
}

fn action_requests_strict_isolation(action: &CodingToolAction) -> bool {
    matches!(
        action,
        CodingToolAction::RunProcess {
            isolation: IsolationMode::Strict,
            ..
        } | CodingToolAction::RunShell {
            isolation: IsolationMode::Strict,
            ..
        }
    )
}

fn process_output(output: ProcessResult) -> ToolOutput {
    ToolOutput::Process {
        executable: output.executable,
        exit_code: output.exit_code,
        timed_out: output.timed_out,
        canceled: output.canceled,
        tree_cleanup_confirmed: output.tree_cleanup_confirmed,
        stdout: redact_text(&output.stdout),
        stderr: redact_text(&output.stderr),
        stdout_truncated: output.stdout_truncated,
        stderr_truncated: output.stderr_truncated,
    }
}

fn git_output(operation: &str, output: ProcessResult) -> ToolOutput {
    let combined = if output.stderr.is_empty() {
        output.stdout
    } else if output.stdout.is_empty() {
        output.stderr
    } else {
        format!("{}\n{}", output.stdout, output.stderr)
    };
    ToolOutput::Git {
        operation: operation.to_owned(),
        output: redact_text(&combined),
        truncated: output.stdout_truncated || output.stderr_truncated,
    }
}

fn event_for_intent(intent: &ToolIntentRecord) -> Result<EventEnvelope, HarnessError> {
    let mut payload = Map::new();
    payload.insert(
        "tool_execution_id".to_owned(),
        Value::String(intent.tool_execution_id.as_str().to_owned()),
    );
    payload.insert(
        "tool_name".to_owned(),
        Value::String(intent.tool_name.clone()),
    );
    payload.insert(
        "action_hash".to_owned(),
        Value::String(intent.action_hash.as_str().to_owned()),
    );
    payload.insert(
        "reconciliation_hint".to_owned(),
        Value::String("inspect before retry; never blindly replay a durable intent".to_owned()),
    );
    let payload_hash = ContentHash::from_canonical_json(&Value::Object(payload.clone()))?;
    Ok(EventEnvelope {
        schema_version: P0_SCHEMA_VERSION,
        event_id: EventId::generate(),
        session_id: intent.session_id.clone(),
        seq: intent.intent_sequence,
        event_type: "tool.intent.recorded".to_owned(),
        producer: producer(),
        authority: SourceAuthority::RuntimeObserved,
        correlation_id: None,
        causation_id: None,
        continuity_critical: true,
        payload,
        payload_hash,
    })
}

fn event_for_receipt(
    session_id: &harness_types::SessionId,
    sequence: u64,
    receipt: &ToolExecutionReceipt,
    kind: &str,
) -> Result<EventEnvelope, HarnessError> {
    let mut payload = Map::new();
    payload.insert(
        "receipt".to_owned(),
        serde_json::to_value(receipt).map_err(|_| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                "tool receipt cannot be serialized",
            )
        })?,
    );
    payload.insert("kind".to_owned(), Value::String(kind.to_owned()));
    let payload_hash = ContentHash::from_canonical_json(&Value::Object(payload.clone()))?;
    Ok(EventEnvelope {
        schema_version: P0_SCHEMA_VERSION,
        event_id: EventId::generate(),
        session_id: session_id.clone(),
        seq: sequence,
        event_type: "receipt.recorded".to_owned(),
        producer: producer(),
        authority: SourceAuthority::RuntimeObserved,
        correlation_id: None,
        causation_id: None,
        continuity_critical: true,
        payload,
        payload_hash,
    })
}

fn event_for_task_update(
    session_id: &harness_types::SessionId,
    sequence: u64,
    note: &str,
) -> Result<EventEnvelope, HarnessError> {
    let mut payload = Map::new();
    payload.insert("note".to_owned(), Value::String(note.to_owned()));
    let payload_hash = ContentHash::from_canonical_json(&Value::Object(payload.clone()))?;
    Ok(EventEnvelope {
        schema_version: P0_SCHEMA_VERSION,
        event_id: EventId::generate(),
        session_id: session_id.clone(),
        seq: sequence,
        event_type: "task.updated".to_owned(),
        producer: producer(),
        authority: SourceAuthority::RuntimeObserved,
        correlation_id: None,
        causation_id: None,
        continuity_critical: true,
        payload,
        payload_hash,
    })
}

fn producer() -> ProducerIdentity {
    ProducerIdentity {
        plugin_id: "p3.tools".to_owned(),
        implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

fn current_unix_ms() -> Result<u64, HarnessError> {
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
        HarnessError::new(
            ErrorCode::ApprovalStale,
            "system clock is before Unix epoch",
        )
    })?;
    u64::try_from(duration.as_millis()).map_err(|_| {
        HarnessError::new(
            ErrorCode::ApprovalStale,
            "system clock milliseconds exceed range",
        )
    })
}

fn store_error(error: StoreError) -> HarnessError {
    error.into_harness_error()
}
